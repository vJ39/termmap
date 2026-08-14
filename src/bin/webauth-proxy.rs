// termmap web auth proxy — ttyd の手前に置く Cookie 認証リバースプロキシ。
//
// ttyd の -c (Basic認証) は、iOS Safari が最初のページ読み込みで通した資格情報を
// 裏で動く /token の fetch や WebSocket ハンドシェイクへ再利用できないため、
// Cloudflare Tunnel 経由だと認証が通らずリロード地獄になる。
// Cookie なら背景 fetch にも WebSocket ハンドシェイクにも自動で付くので、
// 認証を Cookie 方式に移してこの問題を避ける。設計は docs/web-auth-proxy-design.md。
//
// 構成:
//   ブラウザ ⇄ (HTTPS) Cloudflare Tunnel ⇄ (HTTP) このプロキシ:7681 ⇄ ttyd:17681 ⇄ termmap
//
// 環境変数:
//   TERMMAP_WEB_USER / TERMMAP_WEB_PASS  ログイン資格情報(必須。既定値は持たない)
//   WEBAUTH_PROXY_PORT                   公開ポート(既定 7681)
//   WEBAUTH_PROXY_UPSTREAM_PORT          転送先 ttyd のポート(既定 17681)
//
// 割り切り(設計書「明示的にやらないこと」):
//   - Cookie 署名(HMAC等)なし。トークンはサーバー生成の32byte乱数のみで、
//     推測不可能性はここに依存する
//   - セッションの永続化なし(プロセス再起動でログインし直し)
//   - HTTP/1.1 の厳密なパース(chunked encoding 等)はしない。ttyd のページが実際に送る
//     GET(ボディ無し)と、ログインフォームの POST(Content-Length 付き)だけを想定
//   - レート制限・アカウントロックアウト・複数ユーザー管理はしない

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const COOKIE_NAME: &str = "termmap_session";
/// Cookie の Max-Age と、サーバー側セッションの有効期限(30日)。
const SESSION_TTL_SECS: u64 = 2_592_000;
const DEFAULT_PORT: u16 = 7681;
const DEFAULT_UPSTREAM_PORT: u16 = 17681;
/// リクエストヘッダーの上限。これを超えたら 400 で切る(無限にバッファしない)。
const MAX_HEAD_BYTES: usize = 32 * 1024;
/// ログインフォームの POST ボディの上限。
const MAX_BODY_BYTES: usize = 64 * 1024;
/// ヘッダー/ボディ読み込みの待ち時間。WebSocket へ移る前に解除する(WSは無通信で待つため)。
const HEAD_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// 認証失敗時に待つ時間(簡易ブルートフォース対策)。
const LOGIN_FAIL_DELAY: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------- 設定

struct Config {
    user: String,
    pass: String,
    port: u16,
    upstream_port: u16,
}

fn env_required(name: &str) -> String {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!(
                "error: {name} が未設定。TERMMAP_WEB_USER と TERMMAP_WEB_PASS を設定すること。\n\
                 \n  export TERMMAP_WEB_USER=your-name\n  export TERMMAP_WEB_PASS=your-strong-password\n\
                 \n既定のユーザー名・パスワードは用意していない(公開経路に出す前提のため)。"
            );
            std::process::exit(1);
        }
    }
}

fn env_port(name: &str, default: u16) -> u16 {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => match v.parse::<u16>() {
            Ok(p) if p != 0 => p,
            _ => {
                eprintln!("error: {name} がポート番号として不正: {v}");
                std::process::exit(1);
            }
        },
        _ => default,
    }
}

// ---------------------------------------------------------------- セッション

/// プロセス内メモリだけで持つセッション表(token -> 有効期限)。
struct Sessions {
    inner: Mutex<HashMap<String, Instant>>,
}

impl Sessions {
    fn new() -> Self {
        Sessions { inner: Mutex::new(HashMap::new()) }
    }

    /// 期限を直接指定して登録する(テスト用途と、下の insert の実体)。
    fn insert_at(&self, token: String, expiry: Instant) {
        let mut m = self.inner.lock().unwrap();
        m.insert(token, expiry);
    }

    fn insert(&self, token: String, ttl: Duration) {
        let now = Instant::now();
        self.prune_at(now);
        self.insert_at(token, now + ttl);
    }

    /// now 時点で有効か。期限切れは無効(掃除は prune_at 側で行う)。
    fn is_valid_at(&self, token: &str, now: Instant) -> bool {
        let m = self.inner.lock().unwrap();
        match m.get(token) {
            Some(expiry) => now < *expiry,
            None => false,
        }
    }

    fn is_valid(&self, token: &str) -> bool {
        self.is_valid_at(token, Instant::now())
    }

    fn prune_at(&self, now: Instant) {
        let mut m = self.inner.lock().unwrap();
        m.retain(|_, expiry| now < *expiry);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

/// /dev/urandom から 32byte 読んで hex 化したものをセッショントークンにする。
fn new_token() -> io::Result<String> {
    let mut f = std::fs::File::open("/dev/urandom")?;
    let mut buf = [0u8; 32];
    f.read_exact(&mut buf)?;
    Ok(hex_encode(&buf))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// 定数時間比較。途中で return せず全バイトを XOR で累積し、最後に 0 判定する。
/// 長さ自体はループ回数に出るが(=パスワード長は漏れうる)、内容の一致位置は漏らさない。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff: u32 = (a.len() ^ b.len()) as u32;
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        diff |= (x ^ y) as u32;
    }
    diff == 0
}

// ---------------------------------------------------------------- HTTP パース

#[derive(Debug, PartialEq)]
struct RequestHead {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl RequestHead {
    /// クエリを落としたパス。
    fn path(&self) -> &str {
        path_before_query(&self.target)
    }

    fn header(&self, name: &str) -> Option<&str> {
        header_get(&self.headers, name)
    }

    fn is_websocket_upgrade(&self) -> bool {
        let upgrade_ws = self
            .header("upgrade")
            .map(|v| v.trim().eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);
        // Connection は "keep-alive, Upgrade" のように複数トークンが来る
        let conn_upgrade = self
            .header("connection")
            .map(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")))
            .unwrap_or(false);
        upgrade_ws && conn_upgrade
    }

    fn content_length(&self) -> usize {
        self.header("content-length")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0)
    }
}

fn path_before_query(target: &str) -> &str {
    match target.find('?') {
        Some(i) => &target[..i],
        None => target,
    }
}

fn header_get<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// "\r\n\r\n" の直後の位置を返す(= ヘッダー部の終端)。
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// ヘッダー部のバイト列を解釈する。行折り返し等の凝ったケースは扱わない。
fn parse_request_head(head: &[u8]) -> Option<RequestHead> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let _version = parts.next()?; // HTTP/1.1 前提。値は使わない
    if method.is_empty() || target.is_empty() {
        return None;
    }
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':')?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    Some(RequestHead { method, target, headers })
}

/// Cookie ヘッダーの値から目的の Cookie を取り出す。
fn cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    for part in cookie_header.split(';') {
        let part = part.trim();
        let (k, v) = match part.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        if k.trim() == name {
            return Some(v.trim());
        }
    }
    None
}

/// application/x-www-form-urlencoded から名前で値を取り出す。
fn form_value(body: &str, name: &str) -> Option<String> {
    for part in body.split('&') {
        let (k, v) = match part.split_once('=') {
            Some(kv) => kv,
            None => (part, ""),
        };
        if percent_decode(k) == name {
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push(h << 4 | l);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// 通常の HTTP リクエストを ttyd へ渡す形に直す。
/// レスポンス長を自前で解釈しないで済むよう Connection: close を強制し、
/// ttyd が応答を返し終えたら接続を閉じてくれる状態にする(それを EOF まで素通しする)。
fn rewrite_head_for_upstream(head: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(head);
    let mut out = String::with_capacity(text.len() + 32);
    for line in text.split("\r\n") {
        if line.is_empty() {
            break;
        }
        let drop = match line.split_once(':') {
            Some((k, _)) => {
                let k = k.trim();
                k.eq_ignore_ascii_case("connection")
                    || k.eq_ignore_ascii_case("keep-alive")
                    || k.eq_ignore_ascii_case("proxy-connection")
                    // chunked は解釈しない(=ボディを送らない)ので、宣言も落としておく。
                    // 残すと ttyd がボディを待ち続けて応答が返らなくなる。
                    || k.eq_ignore_ascii_case("transfer-encoding")
            }
            None => false, // リクエストライン
        };
        if !drop {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out.push_str("Connection: close\r\n\r\n");
    out.into_bytes()
}

// ---------------------------------------------------------------- レスポンス

fn login_page(error: bool) -> String {
    let msg = if error {
        "<p class=\"err\">ユーザー名かパスワードが違います</p>"
    } else {
        ""
    };
    let body = format!(
        "<!doctype html>\n\
         <html lang=\"ja\"><head><meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
         <title>termmap</title>\n\
         <style>\n\
         html,body{{height:100%;margin:0;background:#111;color:#ddd;\
         font-family:-apple-system,system-ui,sans-serif}}\n\
         form{{max-width:20rem;margin:0 auto;padding:3rem 1.25rem;\
         display:flex;flex-direction:column;gap:.75rem}}\n\
         h1{{font-size:1.25rem;margin:0 0 .5rem;letter-spacing:.08em}}\n\
         input{{font-size:1rem;padding:.6rem .7rem;border:1px solid #444;border-radius:.4rem;\
         background:#1b1b1b;color:#eee}}\n\
         button{{font-size:1rem;padding:.6rem;border:0;border-radius:.4rem;\
         background:#2f6f4f;color:#fff}}\n\
         .err{{margin:0;color:#e88;font-size:.9rem}}\n\
         </style></head><body>\n\
         <form method=\"post\" action=\"/login\">\n\
         <h1>termmap</h1>\n\
         {msg}\n\
         <input name=\"user\" placeholder=\"user\" autocomplete=\"username\" \
         autocapitalize=\"off\" autocorrect=\"off\" spellcheck=\"false\">\n\
         <input name=\"pass\" type=\"password\" placeholder=\"password\" \
         autocomplete=\"current-password\">\n\
         <button type=\"submit\">ログイン</button>\n\
         </form></body></html>\n"
    );
    body
}

fn write_response(
    client: &mut TcpStream,
    status: &str,
    content_type: &str,
    extra_headers: &[String],
    body: &[u8],
) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n",
        body.len()
    );
    for h in extra_headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    client.write_all(head.as_bytes())?;
    client.write_all(body)?;
    client.flush()
}

fn respond_login_form(client: &mut TcpStream, error: bool) -> io::Result<()> {
    let body = login_page(error);
    write_response(client, "200 OK", "text/html; charset=utf-8", &[], body.as_bytes())
}

/// WebSocket など JS からの裏リクエストが未認証だった場合。
/// WWW-Authenticate は付けない(ブラウザの Basic 認証ダイアログを出さないため)。
fn respond_unauthorized(client: &mut TcpStream) -> io::Result<()> {
    write_response(client, "401 Unauthorized", "text/plain; charset=utf-8", &[], b"unauthorized\n")
}

fn respond_bad_request(client: &mut TcpStream) -> io::Result<()> {
    write_response(client, "400 Bad Request", "text/plain; charset=utf-8", &[], b"bad request\n")
}

fn respond_bad_gateway(client: &mut TcpStream) -> io::Result<()> {
    write_response(client, "502 Bad Gateway", "text/plain; charset=utf-8", &[], b"upstream unavailable\n")
}

fn set_cookie_header(token: &str) -> String {
    format!(
        "Set-Cookie: {COOKIE_NAME}={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={SESSION_TTL_SECS}"
    )
}

fn respond_login_ok(client: &mut TcpStream, token: &str) -> io::Result<()> {
    let extra = vec!["Location: /".to_string(), set_cookie_header(token)];
    write_response(client, "302 Found", "text/plain; charset=utf-8", &extra, b"")
}

// ---------------------------------------------------------------- 接続処理

fn read_head(client: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(i) = find_head_end(&buf) {
            let rest = buf.split_off(i);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
        let n = client.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "eof before request head"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Content-Length 分のボディを読む(先読み済みの rest を先に使う)。
fn read_body(client: &mut TcpStream, rest: &[u8], len: usize) -> io::Result<Vec<u8>> {
    let mut body = Vec::with_capacity(len.min(MAX_BODY_BYTES));
    let take = rest.len().min(len);
    body.extend_from_slice(&rest[..take]);
    let mut chunk = [0u8; 2048];
    while body.len() < len {
        let n = client.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n.min(len - body.len())]);
    }
    Ok(body)
}

fn handle_login(
    client: &mut TcpStream,
    head: &RequestHead,
    rest: &[u8],
    cfg: &Config,
    sessions: &Sessions,
) -> io::Result<()> {
    let len = head.content_length();
    if len > MAX_BODY_BYTES {
        return respond_bad_request(client);
    }
    let body = read_body(client, rest, len)?;
    let body = String::from_utf8_lossy(&body).into_owned();
    let user = form_value(&body, "user").unwrap_or_default();
    let pass = form_value(&body, "pass").unwrap_or_default();

    // & (短絡しないビット演算) にして、user が違っても pass の比較を必ず行う。
    let ok = ct_eq(user.as_bytes(), cfg.user.as_bytes()) & ct_eq(pass.as_bytes(), cfg.pass.as_bytes());
    if !ok {
        thread::sleep(LOGIN_FAIL_DELAY);
        return respond_login_form(client, true);
    }

    let token = match new_token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("webauth-proxy: トークン生成に失敗: {e}");
            return write_response(
                client,
                "500 Internal Server Error",
                "text/plain; charset=utf-8",
                &[],
                b"internal error\n",
            );
        }
    };
    sessions.insert(token.clone(), Duration::from_secs(SESSION_TTL_SECS));
    respond_login_ok(client, &token)
}

fn authenticated(head: &RequestHead, sessions: &Sessions) -> bool {
    let cookie = match head.header("cookie") {
        Some(c) => c,
        None => return false,
    };
    match cookie_value(cookie, COOKIE_NAME) {
        Some(token) => sessions.is_valid(token),
        None => false,
    }
}

/// 通常の HTTP リクエストを ttyd へ中継する。
fn proxy_http(
    client: &mut TcpStream,
    head_bytes: &[u8],
    head: &RequestHead,
    rest: &[u8],
    cfg: &Config,
) -> io::Result<()> {
    let mut up = match TcpStream::connect(("127.0.0.1", cfg.upstream_port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("webauth-proxy: ttyd へ接続できない: {e}");
            return respond_bad_gateway(client);
        }
    };
    up.set_nodelay(true).ok();
    up.write_all(&rewrite_head_for_upstream(head_bytes))?;

    // ボディがあれば Content-Length 分だけ流す(ttyd のページが送るのは GET のみだが、
    // POST が来ても壊れないようにしておく)。丸ごとメモリに載せるので上限は付ける。
    let len = head.content_length();
    if len > MAX_BODY_BYTES {
        return respond_bad_request(client);
    }
    if len > 0 {
        let body = read_body(client, rest, len)?;
        up.write_all(&body)?;
    }
    up.flush()?;

    // Connection: close を強制しているので、レスポンスは EOF まで読めば全部。
    io::copy(&mut up, client)?;
    client.flush()
}

/// WebSocket。101 の中身は解釈せず、以降は生バイトを双方向にコピーするだけ。
fn proxy_websocket(
    client: TcpStream,
    head_bytes: &[u8],
    rest: &[u8],
    cfg: &Config,
) -> io::Result<()> {
    let mut client = client;
    let mut up = match TcpStream::connect(("127.0.0.1", cfg.upstream_port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("webauth-proxy: ttyd へ接続できない(ws): {e}");
            return respond_bad_gateway(&mut client);
        }
    };
    up.set_nodelay(true).ok();
    // WS はヘッダーを書き換えない(Sec-WebSocket-* や Connection: Upgrade をそのまま渡す)。
    up.write_all(head_bytes)?;
    if !rest.is_empty() {
        up.write_all(rest)?;
    }
    up.flush()?;

    // 双方向とも無通信で待つので読み取りタイムアウトは外す。
    client.set_read_timeout(None).ok();
    up.set_read_timeout(None).ok();

    let client_r = client.try_clone()?;
    let up_w = up.try_clone()?;
    let t = thread::spawn(move || pump(client_r, up_w));
    pump(up, client);
    let _ = t.join();
    Ok(())
}

/// 片方向コピー。終わったら両側を落として、対になるスレッドも抜けさせる。
fn pump(mut from: TcpStream, mut to: TcpStream) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
                if to.flush().is_err() {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = to.shutdown(Shutdown::Both);
}

fn handle_conn(mut client: TcpStream, cfg: &Config, sessions: &Sessions) -> io::Result<()> {
    client.set_nodelay(true).ok();
    client.set_read_timeout(Some(HEAD_READ_TIMEOUT)).ok();

    let (head_bytes, rest) = read_head(&mut client)?;
    let head = match parse_request_head(&head_bytes) {
        Some(h) => h,
        None => return respond_bad_request(&mut client),
    };

    if head.method.eq_ignore_ascii_case("POST") && head.path() == "/login" {
        return handle_login(&mut client, &head, &rest, cfg, sessions);
    }

    if !authenticated(&head, sessions) {
        // JS からの裏リクエスト(WebSocket)はログインへ誘導しても意味が無いので 401。
        return if head.is_websocket_upgrade() {
            respond_unauthorized(&mut client)
        } else {
            respond_login_form(&mut client, false)
        };
    }

    if head.is_websocket_upgrade() {
        proxy_websocket(client, &head_bytes, &rest, cfg)
    } else {
        proxy_http(&mut client, &head_bytes, &head, &rest, cfg)
    }
}

fn main() {
    let cfg = Arc::new(Config {
        user: env_required("TERMMAP_WEB_USER"),
        pass: env_required("TERMMAP_WEB_PASS"),
        port: env_port("WEBAUTH_PROXY_PORT", DEFAULT_PORT),
        upstream_port: env_port("WEBAUTH_PROXY_UPSTREAM_PORT", DEFAULT_UPSTREAM_PORT),
    });
    let sessions = Arc::new(Sessions::new());

    // 直接インターネットへ晒さない。外へ出すのは cloudflared 側の役目。
    let listener = match TcpListener::bind(("127.0.0.1", cfg.port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: 127.0.0.1:{} を待ち受けできない: {e}", cfg.port);
            std::process::exit(1);
        }
    };
    eprintln!(
        "webauth-proxy: http://127.0.0.1:{} -> ttyd 127.0.0.1:{} (user: {})",
        cfg.port, cfg.upstream_port, cfg.user
    );

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("webauth-proxy: accept 失敗: {e}");
                continue;
            }
        };
        let cfg = Arc::clone(&cfg);
        let sessions = Arc::clone(&sessions);
        thread::spawn(move || {
            if let Err(e) = handle_conn(stream, &cfg, &sessions) {
                // 相手が切っただけのケースが大半なので、握り潰さず1行だけ出す。
                if e.kind() != io::ErrorKind::UnexpectedEof && e.kind() != io::ErrorKind::BrokenPipe {
                    eprintln!("webauth-proxy: 接続処理: {e}");
                }
            }
        });
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn head_of(raw: &str) -> RequestHead {
        parse_request_head(raw.as_bytes()).expect("parse")
    }

    #[test]
    fn ct_eq_matches_only_on_identical_bytes() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secreT"));
        assert!(!ct_eq(b"secret", b"secre"));
        assert!(!ct_eq(b"secret", b"secretx"));
        assert!(ct_eq(b"", b""));
        assert!(!ct_eq(b"", b"a"));
        // 先頭が一致していても不一致は不一致(早期returnしていないことの確認も兼ねる)
        assert!(!ct_eq(b"aaaaaaaa", b"aaaaaaab"));
        // 長さ違いで片方が 0 埋めと衝突しないこと
        assert!(!ct_eq(&[0u8], &[]));
    }

    #[test]
    fn hex_encode_is_lowercase_fixed_width() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn new_token_is_64_hex_chars_and_unique() {
        let a = new_token().expect("urandom");
        let b = new_token().expect("urandom");
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn parse_request_head_reads_line_and_headers() {
        let h = head_of("GET /token?x=1 HTTP/1.1\r\nHost: example\r\nCookie: a=b\r\n\r\n");
        assert_eq!(h.method, "GET");
        assert_eq!(h.target, "/token?x=1");
        assert_eq!(h.path(), "/token");
        assert_eq!(h.header("host"), Some("example"));
        assert_eq!(h.header("HOST"), Some("example"));
        assert_eq!(h.header("nope"), None);
    }

    #[test]
    fn parse_request_head_rejects_garbage() {
        assert!(parse_request_head(b"\r\n\r\n").is_none());
        assert!(parse_request_head(b"GET\r\n\r\n").is_none());
        assert!(parse_request_head(b"GET /\r\n\r\n").is_none());
        // ヘッダー行にコロンが無い
        assert!(parse_request_head(b"GET / HTTP/1.1\r\nbroken\r\n\r\n").is_none());
    }

    #[test]
    fn path_before_query_strips_query() {
        assert_eq!(path_before_query("/login"), "/login");
        assert_eq!(path_before_query("/login?next=/"), "/login");
        assert_eq!(path_before_query("/"), "/");
    }

    #[test]
    fn find_head_end_points_just_after_delimiter() {
        let raw = b"GET / HTTP/1.1\r\n\r\nBODY";
        let i = find_head_end(raw).unwrap();
        assert_eq!(&raw[i..], b"BODY");
        assert!(find_head_end(b"GET / HTTP/1.1\r\n").is_none());
    }

    #[test]
    fn websocket_upgrade_detection() {
        let ws = head_of(
            "GET /ws HTTP/1.1\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\n\r\n",
        );
        assert!(ws.is_websocket_upgrade());
        let ws_case = head_of("GET /ws HTTP/1.1\r\nupgrade: WebSocket\r\nconnection: UPGRADE\r\n\r\n");
        assert!(ws_case.is_websocket_upgrade());
        let plain = head_of("GET / HTTP/1.1\r\nConnection: keep-alive\r\n\r\n");
        assert!(!plain.is_websocket_upgrade());
        // Upgrade はあるが Connection トークンが無い場合は WS 扱いしない
        let half = head_of("GET / HTTP/1.1\r\nUpgrade: websocket\r\n\r\n");
        assert!(!half.is_websocket_upgrade());
    }

    #[test]
    fn content_length_defaults_to_zero() {
        let h = head_of("POST /login HTTP/1.1\r\nContent-Length: 25\r\n\r\n");
        assert_eq!(h.content_length(), 25);
        let g = head_of("GET / HTTP/1.1\r\n\r\n");
        assert_eq!(g.content_length(), 0);
        let bad = head_of("POST /login HTTP/1.1\r\nContent-Length: abc\r\n\r\n");
        assert_eq!(bad.content_length(), 0);
    }

    #[test]
    fn cookie_value_picks_exact_name() {
        assert_eq!(cookie_value("termmap_session=abc", COOKIE_NAME), Some("abc"));
        assert_eq!(cookie_value("a=1; termmap_session=abc; b=2", COOKIE_NAME), Some("abc"));
        assert_eq!(cookie_value(" termmap_session = abc ", COOKIE_NAME), Some("abc"));
        // 名前の部分一致で拾わない
        assert_eq!(cookie_value("xtermmap_session=abc", COOKIE_NAME), None);
        assert_eq!(cookie_value("termmap_session_x=abc", COOKIE_NAME), None);
        assert_eq!(cookie_value("", COOKIE_NAME), None);
        assert_eq!(cookie_value("nocookie", COOKIE_NAME), None);
    }

    #[test]
    fn form_value_decodes_percent_and_plus() {
        let body = "user=yo+tsu&pass=p%40ss+w%2Frd";
        assert_eq!(form_value(body, "user").as_deref(), Some("yo tsu"));
        assert_eq!(form_value(body, "pass").as_deref(), Some("p@ss w/rd"));
        assert_eq!(form_value(body, "none"), None);
        assert_eq!(form_value("user=", "user").as_deref(), Some(""));
        assert_eq!(form_value("user", "user").as_deref(), Some(""));
        // 日本語(UTF-8 の %エンコード)
        assert_eq!(form_value("pass=%E3%81%82", "pass").as_deref(), Some("あ"));
        // 壊れた % は素通し
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn rewrite_head_forces_connection_close() {
        let raw = b"GET /token HTTP/1.1\r\nHost: h\r\nConnection: keep-alive\r\nKeep-Alive: timeout=5\r\nAccept: */*\r\n\r\n";
        let out = String::from_utf8(rewrite_head_for_upstream(raw)).unwrap();
        assert!(out.starts_with("GET /token HTTP/1.1\r\n"));
        assert!(out.contains("Host: h\r\n"));
        assert!(out.contains("Accept: */*\r\n"));
        assert!(!out.contains("keep-alive"));
        assert!(!out.contains("Keep-Alive"));
        assert_eq!(out.matches("Connection:").count(), 1);
        assert!(out.ends_with("Connection: close\r\n\r\n"));
    }

    #[test]
    fn rewrite_head_drops_transfer_encoding() {
        // chunked ボディは扱わないので、宣言ごと落として ttyd を待たせない
        let raw = b"POST /x HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n";
        let out = String::from_utf8(rewrite_head_for_upstream(raw)).unwrap();
        assert!(!out.to_lowercase().contains("transfer-encoding"));
        assert!(out.contains("Host: h\r\n"));
    }

    #[test]
    fn sessions_validity_follows_expiry() {
        let s = Sessions::new();
        let now = Instant::now();
        s.insert_at("live".to_string(), now + Duration::from_secs(60));
        s.insert_at("dead".to_string(), now + Duration::from_secs(1));

        assert!(s.is_valid_at("live", now));
        assert!(!s.is_valid_at("unknown", now));
        // 期限ちょうどは無効(now < expiry で判定している)
        assert!(!s.is_valid_at("dead", now + Duration::from_secs(1)));
        assert!(!s.is_valid_at("dead", now + Duration::from_secs(2)));
        assert!(s.is_valid_at("dead", now + Duration::from_millis(500)));
    }

    #[test]
    fn sessions_prune_drops_only_expired() {
        let s = Sessions::new();
        let now = Instant::now();
        s.insert_at("live".to_string(), now + Duration::from_secs(600));
        s.insert_at("dead".to_string(), now + Duration::from_secs(1));
        assert_eq!(s.len(), 2);
        s.prune_at(now + Duration::from_secs(2));
        assert_eq!(s.len(), 1);
        assert!(s.is_valid_at("live", now + Duration::from_secs(2)));
    }

    #[test]
    fn sessions_insert_uses_ttl() {
        let s = Sessions::new();
        s.insert("t".to_string(), Duration::from_secs(SESSION_TTL_SECS));
        assert!(s.is_valid("t"));
        assert!(!s.is_valid("other"));
    }

    #[test]
    fn set_cookie_header_matches_design() {
        let h = set_cookie_header("deadbeef");
        assert_eq!(
            h,
            "Set-Cookie: termmap_session=deadbeef; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=2592000"
        );
    }

    #[test]
    fn login_page_shows_error_only_when_asked() {
        let ok = login_page(false);
        assert!(ok.contains("action=\"/login\""));
        assert!(ok.contains("name=\"user\""));
        assert!(ok.contains("name=\"pass\""));
        assert!(!ok.contains("違います"));
        assert!(login_page(true).contains("違います"));
    }
}
