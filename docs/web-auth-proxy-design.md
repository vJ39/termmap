## 背景

ttydの `-c` (Basic認証) は、iOS Safariが最初のページ読み込みで通した資格情報を、
裏で動く `/token` のfetchに再利用できないため、Cloudflare Tunnel経由だと
WebSocket認証が通らずリロード地獄になる(実機・再現環境の両方で確認済み)。
ttyd・Cloudflare Tunnelの経路自体は正常(curlで認証ヘッダーを毎回明示すれば100%成功)。

対策: Cookieベースの認証プロキシをttydの手前に置く。Cookieは一度セットされれば
ブラウザが背景fetch・WebSocketハンドシェイクにも自動で付けるため、この問題を回避できる。

## 構成

```
ブラウザ ⇄ (HTTPS) Cloudflare Tunnel ⇄ (HTTP) webauth-proxy:7681 ⇄ (HTTP/WS) ttyd:<内部ポート> ⇄ termmap
```

- ttydは `-c` を外し `-i 127.0.0.1` のみ(認証なし・外部から直接到達不可な内部ポート)
- webauth-proxyが公開ポート(既定7681)を持ち、Cookie検証を通った通信だけをttydへ中継する
- 新規Cargoバイナリ `src/bin/webauth-proxy.rs`。新規依存クレートは追加しない(標準ライブラリのみ)
- `cargo build --release` で `target/release/webauth-proxy` も一緒にビルドされる

## 認証フロー

1. `GET /` 等、Cookie無し/無効 → 簡易ログインフォーム(HTML)を返す
2. `POST /login`(`user=...&pass=...`) → `TERMMAP_WEB_USER`/`TERMMAP_WEB_PASS`(既存の環境変数をそのまま流用)と**定数時間比較**
   - 一致: `/dev/urandom` から32byte読み、hex化した文字列をセッショントークンにする。
     プロセス内メモリの `HashMap<token, 有効期限>` に登録(署名や暗号は使わない。
     トークン自体がサーバー生成の乱数=推測不可能なため、bearerトークン方式で十分)。
     `Set-Cookie: termmap_session=<token>; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=2592000`(30日)
     を付けて `/` へ302リダイレクト
   - 不一致: 1秒待ってからログインフォームを再表示(簡易ブルートフォース対策。
     本格的なレート制限やロックアウトは個人利用の脅威モデルではオーバースペックなので入れない)
3. Cookie有効な状態での以降のリクエスト:
   - 通常のHTTPリクエスト(GET等) → リクエストライン・ヘッダー・ボディをそのままttydへ転送し、
     レスポンスをそのまま返す(素朴なリバースプロキシ)
   - `Upgrade: websocket` を含むリクエスト → Cookie検証後、ttydへ接続してリクエストをそのまま
     転送・101レスポンスを中継したら、あとはWebSocketのフレームを一切解釈せず
     生バイトを双方向にコピーするだけ(スレッド2本、`io::copy`相当)。実装を小さく保つ狙い
4. Cookie無効(期限切れ含む)でWebSocketアップグレード等が来た場合は401を返す(ログインへは誘導しない。
   JSからの裏リクエストなので誘導しても意味が無いため)

## 明示的にやらないこと

- Cookie署名(HMAC等)は使わない。トークンはサーバー生成の乱数のみで、推測不可能性はここに依存する
- セッションの永続化はしない(プロセス再起動でログインし直しになる。個人用途で許容範囲)
- HTTP/1.1の厳密なパース(chunked encoding等)はしない。ttydページのJSが実際に送るGET(ボディ無し)と
  ログインフォームのPOST(Content-Length付き)だけを想定した最小実装にする
- レート制限・アカウントロックアウト・複数ユーザー管理はしない(単一の共有ユーザー/パスワードのまま)

## 既存スクリプトへの影響

- `scripts/serve-web.sh`: ttydの起動から `-c` を外し、内部専用ポート(例: 17681)で起動。
  続けて `webauth-proxy`(公開ポート7681、ttyd内部ポート、TERMMAP_WEB_USER/PASSを渡す)を起動する
- `scripts/build-web-index.sh`: 変更不要(ttydの素のページを生成する処理はそのまま)
- README.md: 手順自体(build→serve→cloudflared)は変わらないので大きな変更は無い想定。
  認証がプロキシ側に移った旨だけ注記する

## 検証したいこと

- 自分のCloudflare Quick Tunnel経由で、curlで `/login` にPOSTしてCookieを取得
  → そのCookieを使って `/token` 相当・静的ページ取得ができること
  → Cookie無しだとログインフォームが返ること
- 実際にブラウザ(可能ならiPhone実機 or Chrome touch emulation)で、ログイン→地図表示→
  タッチ操作(スワイプ/雨雲ボタン等、既存のtouch-overlay.jsの操作)が問題なく動くこと
- Cookie無し/期限切れ状態で通常ページアクセス→ログインフォームへ誘導されること
