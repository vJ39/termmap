// termmap — mapscii 風の端末地図レンダラ
//   halfblock (既定): ▀ + truecolor / braille: 点字ドット(--mono でプレーン)
//   --classify : 地物カテゴリ(水域/緑地/幹線道路/線路?/建物)を色分け(ラスタ色からの推定)
//   --place    : 日本語住所などをジオコーディング(Nominatim)して中心に
//   --interactive(-i): カーソルキーでパン、+/- でズーム、q で終了
//   --png PATH : カテゴリ色PNGを書き出す(確認用)  --image PNG : 既存画像を描画

mod geo;
mod tiles;
mod render;
mod route;
mod poi;
mod spots;
mod share;
mod streetview;
mod elevation;
mod gpslive;
mod searchcache;
mod roadsearch;
#[allow(dead_code)]
mod roadtrace; // point_at/sample等は使用、cut_segment等は将来用
// 以下は今後の機能用(おすすめ相談)。まだ未wireのため dead_code 許容。
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod recommend;

use std::collections::HashMap;
use std::io::Write;
use image::{RgbImage, imageops::FilterType};

use geo::*;
use tiles::*;
use render::*;
use route::*;
use poi::*;
use spots::*;
use share::*;

#[derive(Clone)]
struct Args {
    lat: Option<f64>,
    lon: Option<f64>,
    place: Option<String>,
    zoom: u32,
    width: Option<u32>,
    win_px: u32,
    style: String,
    braille: bool,
    mono: bool,
    classify: bool,
    edge: bool,
    interactive: bool,
    resume: bool,
    here: bool,
    threshold: Option<u8>,
    range: Vec<f64>,
    home: Option<(f64, f64)>,
    route: Option<Vec<(f64, f64)>>,
    route_mode: String,
    gpx: Option<String>,
    load_route: Option<String>,
    save_route: Option<String>,
    list_routes: bool,
    share: bool,
    wander: bool,
    dist: Option<f64>,
    shape: String,
    image: Option<String>,
    png: Option<String>,
}

fn arg_err(msg: &str) -> ! { eprintln!("{msg}"); std::process::exit(2); }

fn parse_args() -> Args {
    let mut a = Args { lat: None, lon: None, place: None, zoom: 14, width: None, win_px: 640,
                       style: "osm".to_string(), braille: false, mono: false, classify: false,
                       edge: false, interactive: false, resume: false, here: false, threshold: None,
                       range: Vec::new(), home: None, route: None, route_mode: "surface".to_string(),
                       gpx: None, load_route: None, save_route: None, list_routes: false, share: false,
                       wander: false, dist: None, shape: "loop".to_string(), image: None, png: None };
    let mut it = std::env::args().skip(1);
    macro_rules! val { ($k:expr) => { it.next().unwrap_or_else(|| arg_err(&format!("{} は値が必要です", $k))) } }
    macro_rules! num { ($k:expr) => {{ let v = val!($k); v.parse().unwrap_or_else(|_| arg_err(&format!("{} の値が不正: {}", $k, v))) }} }
    while let Some(k) = it.next() {
        match k.as_str() {
            "--lat" => a.lat = Some(num!("--lat")),
            "--lon" => a.lon = Some(num!("--lon")),
            "--place" => a.place = Some(val!("--place")),
            "--zoom" => a.zoom = num!("--zoom"),
            "--width" => a.width = Some(num!("--width")),
            "--style" => a.style = val!("--style"),
            "--braille" => a.braille = true,
            "--mono" => a.mono = true,
            "--classify" => a.classify = true,
            "--edge" => a.edge = true,
            "-i" | "--interactive" => a.interactive = true,
            "--resume" | "--last" => a.resume = true,
            "--here" => a.here = true,
            "--range" => {
                let v = val!("--range");
                a.range = v.split(',').filter_map(|s| s.trim().parse::<f64>().ok()).filter(|&k| k > 0.0).collect();
                if a.range.is_empty() { arg_err("--range は正の数値CSV (例 10,20,30)"); }
            }
            "--home" => {
                let v = val!("--home");
                let p: Vec<f64> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                if p.len() != 2 { arg_err("--home は lat,lon 形式"); }
                a.home = Some((p[0], p[1]));
            }
            "--route" => {
                let v = val!("--route");
                let wps: Vec<(f64, f64)> = v.split(';').filter_map(|p| {
                    let mut it = p.split(',');
                    Some((it.next()?.trim().parse().ok()?, it.next()?.trim().parse().ok()?))
                }).collect();
                if wps.len() < 2 { arg_err("--route は lat,lon;lat,lon 形式(2点以上)"); }
                a.route = Some(wps);
            }
            "--route-mode" => a.route_mode = val!("--route-mode"),
            "--gpx" => a.gpx = Some(val!("--gpx")),
            "--load-route" => a.load_route = Some(val!("--load-route")),
            "--save-route" => a.save_route = Some(val!("--save-route")),
            "--routes" => a.list_routes = true,
            "--share" => a.share = true,
            "--wander" => a.wander = true,
            "--dist" => a.dist = Some(num!("--dist")),
            "--shape" => a.shape = val!("--shape"),
            "--threshold" => a.threshold = Some(num!("--threshold")),
            "--image" => a.image = Some(val!("--image")),
            "--png" => a.png = Some(val!("--png")),
            "-h" | "--help" => { eprintln!("usage: termmap (--place \"住所\" | --lat LAT --lon LON | --resume | --here) [--zoom Z] [--style osm|voyager|dark|light] [-i] [--braille] [--classify] [--edge] [--mono] [--range KM,..] [--home LAT,LON] [--route \"LAT,LON;LAT,LON\"] [--route-mode surface|highway|short] [--gpx OUT] [--load-route N] [--save-route N] [--routes] [--share] [--width N] [--png OUT] | --image PNG"); std::process::exit(0); }
            _ => arg_err(&format!("unknown arg: {k}")),
        }
    }
    if a.image.is_none() && a.zoom > 20 { arg_err("--zoom は 0..=20 で指定 (OSMタイル有効域)"); }
    if let Some(w) = a.width { if w == 0 || w > 1024 { arg_err("--width は 1..=1024 で指定"); } }
    a
}

// GPS/測位で現在地を取得 (--here)。macOS CoreLocationCLI に委譲。
// 要: brew install corelocationcli + System Settings > Privacy > Location Services で許可。
fn gps_here() -> Result<(f64, f64), String> {
    let bin = if std::path::Path::new("/opt/homebrew/bin/CoreLocationCLI").exists() {
        "/opt/homebrew/bin/CoreLocationCLI"
    } else { "CoreLocationCLI" };
    let out = std::process::Command::new(bin)
        .args(["--format", "%latitude %longitude"])
        .output()
        .map_err(|e| format!("CoreLocationCLI 実行失敗: {e}\n  brew install corelocationcli を入れてください"))?;
    let s = String::from_utf8_lossy(&out.stdout);
    let e = String::from_utf8_lossy(&out.stderr);
    let line = s.trim();
    let mut it = line.split_whitespace();
    let lat = it.next().and_then(|v| v.parse::<f64>().ok());
    let lon = it.next().and_then(|v| v.parse::<f64>().ok());
    match (lat, lon) {
        (Some(la), Some(lo)) => Ok((la, lo)),
        _ => Err(format!(
            "測位できません: {}{}\n  System Settings > Privacy & Security > Location Services で CoreLocationCLI を許可してください",
            line, e.trim())),
    }
}

// CLI フラグから OverlaySpec を組む。center_* はリング等の基準(--home 指定時はそちら優先)。
fn build_spec(a: &Args, center_lat: f64, center_lon: f64) -> OverlaySpec {
    let mut rings = Vec::new();
    if !a.range.is_empty() {
        let (rl, ro) = a.home.unwrap_or((center_lat, center_lon));
        rings.push(Ring { lat: rl, lon: ro, radii_km: a.range.clone(), color: [255, 90, 90], thickness: 2 });
    }
    OverlaySpec { pois: Vec::new(), routes: Vec::new(), rings, spots: Vec::new() }
}
// --route があれば BRouter で取得して spec に追加し、要約(距離/時間)を返す。--gpx 指定時は書き出し。
fn attach_route(spec: &mut OverlaySpec, a: &Args) -> Result<Option<String>, String> {
    let wps = match &a.route { Some(w) => w, None => return Ok(None) };
    let r = fetch_route(wps, &a.route_mode, 0)?;
    if let Some(g) = &a.gpx { write_gpx(g, &r.pts)?; }
    let summary = format!("ルート {} {}点", route_summary(&a.route_mode, &r), r.pts.len());
    spec.routes.push(Route { pts: r.pts, color: [0, 220, 255], thickness: 2 });
    Ok(Some(summary))
}

fn render(img: &RgbImage, a: &Args, ov: Option<&OverlayLayer>) -> String {
    let th = a.threshold.unwrap_or(if a.edge { 45 } else { 195 });
    if a.edge { render_braille(img, a.mono, false, th, true, ov) }
    else if a.braille { render_braille(img, a.mono, a.classify, th, false, ov) }
    else if a.classify {
        let mut rc = recolor(img);
        if let Some(o) = ov { composite(&mut rc, o); }
        render_halfblock(&rc)
    } else if let Some(o) = ov {
        let mut c = img.clone();
        composite(&mut c, o);
        render_halfblock(&c)
    } else {
        render_halfblock(img)
    }
}

// ---- 直近 location の保存/復元 (--resume) ----
fn state_file() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/termmap/last.txt"))
}
fn save_state(lat: f64, lon: f64, z: u32, style: &str, wps: &[(f64, f64)], mode: &str) {
    if let Some(p) = state_file() {
        if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
        let mut s = format!("{lat} {lon} {z} {style}\n");
        if wps.len() >= 2 { // ルート(始点..終点)も保持 → --resume で復元
            let j = wps.iter().map(|(la, lo)| format!("{la},{lo}")).collect::<Vec<_>>().join(";");
            s.push_str(&format!("route {mode} {j}\n"));
        }
        let _ = std::fs::write(&p, s);
    }
}
fn load_state() -> Option<(f64, f64, u32, String)> {
    let s = std::fs::read_to_string(state_file()?).ok()?;
    let line = s.lines().next()?;
    let mut it = line.split_whitespace();
    let lat = it.next()?.parse().ok()?;
    let lon = it.next()?.parse().ok()?;
    let z = it.next()?.parse().ok()?;
    let style = it.next().unwrap_or("osm").to_string();
    Some((lat, lon, z, style))
}
// 保存されたルート(mode, waypoints)を復元
fn load_route() -> Option<(Vec<(f64, f64)>, String)> {
    let s = std::fs::read_to_string(state_file()?).ok()?;
    let line = s.lines().find(|l| l.starts_with("route "))?;
    let mut it = line.splitn(3, ' ');
    it.next(); // "route"
    let mode = it.next()?.to_string();
    let wps: Vec<(f64, f64)> = it.next()?.split(';').filter_map(|p| {
        let mut c = p.split(',');
        Some((c.next()?.trim().parse().ok()?, c.next()?.trim().parse().ok()?))
    }).collect();
    if wps.len() >= 2 { Some((wps, mode)) } else { None }
}

// ---- お気に入りルート (名前付き保存) ----
fn routes_dir() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".config/termmap/routes"))
}
fn sanitize_name(n: &str) -> String {
    n.trim().chars().map(|c| if c == '/' || c == '\\' || c == ':' || c.is_control() { '_' } else { c }).collect()
}
fn save_named_route(name: &str, mode: &str, wps: &[(f64, f64)]) -> Result<(), String> {
    if wps.len() < 2 { return Err("ルートが未確定(2点以上必要)".into()); }
    let dir = routes_dir().ok_or("HOME不明")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut s = format!("{mode}\n");
    for (la, lo) in wps { s.push_str(&format!("{la},{lo}\n")); }
    std::fs::write(dir.join(format!("{}.txt", sanitize_name(name))), s).map_err(|e| e.to_string())
}
fn load_named_route(name: &str) -> Option<(Vec<(f64, f64)>, String)> {
    let dir = routes_dir()?;
    let s = std::fs::read_to_string(dir.join(format!("{}.txt", sanitize_name(name)))).ok()?;
    let mut lines = s.lines();
    let mode = lines.next()?.trim().to_string();
    let wps: Vec<(f64, f64)> = lines.filter_map(|l| {
        let mut c = l.split(',');
        Some((c.next()?.trim().parse().ok()?, c.next()?.trim().parse().ok()?))
    }).collect();
    if wps.len() >= 2 { Some((wps, mode)) } else { None }
}
fn list_named_routes() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(dir) = routes_dir() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map(|x| x == "txt").unwrap_or(false) {
                    if let Some(st) = p.file_stem().and_then(|s| s.to_str()) { v.push(st.to_string()); }
                }
            }
        }
    }
    v.sort();
    v
}

fn rng_seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1
}
fn lcg(s: &mut u64) -> u64 { *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); *s >> 16 }

// 走りまくりモード: 峠/展望を経由する周回(or片道)の waypoint 列を生成。
fn wander_route(origin: (f64, f64), dist_km: f64, shape: &str) -> Result<Vec<(f64, f64)>, String> {
    let r_km = if shape == "oneway" { (dist_km / 2.5).max(1.0) } else { (dist_km / 7.0).max(1.0) };
    let dlat = r_km / 111.0;
    let dlon = r_km / (111.0 * origin.0.to_radians().cos().abs().max(0.1));
    let (s, w, n, e) = (origin.0 - dlat, origin.1 - dlon, origin.0 + dlat, origin.1 + dlon);
    let mut cands: Vec<(f64, f64, f64, f64)> = Vec::new(); // lat,lon,距離km,方位角
    for kind in POI_KINDS.iter().filter(|k| k.label == "峠道" || k.label == "展望") {
        if let Ok(v) = fetch_pois(kind, s, w, n, e) {
            for (la, lo, _) in v { cands.push((la, lo, haversine_km(origin, (la, lo)), bearing(origin, (la, lo)))); }
        }
    }
    if cands.is_empty() { return Err("スポット(峠/展望)が見つからない。距離を上げるか山方面で試して".into()); }
    let band: Vec<(f64, f64, f64, f64)> = cands.iter().cloned().filter(|t| t.2 >= r_km * 0.4 && t.2 <= r_km * 1.3).collect();
    let pool = if band.len() >= 2 { band } else { cands };
    let mut rng = rng_seed();
    let k = ((dist_km / 20.0).round() as usize).clamp(2, 6);
    let mut wps = vec![origin];
    if shape == "oneway" {
        let dir = (lcg(&mut rng) % 360) as f64;
        let mut sel: Vec<(f64, f64, f64, f64)> = pool.iter().cloned().filter(|t| angdiff(t.3, dir) < 60.0).collect();
        sel.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        for t in sel.into_iter().take(k) { wps.push((t.0, t.1)); }
        if wps.len() < 2 { return Err("その方角にスポットが無い".into()); }
    } else {
        let offset = (lcg(&mut rng) % 360) as f64;
        let mut picked: Vec<(f64, f64, f64)> = Vec::new();
        for i in 0..k {
            let center = (offset + 360.0 * i as f64 / k as f64) % 360.0;
            if let Some(t) = pool.iter().min_by(|a, b| angdiff(a.3, center).partial_cmp(&angdiff(b.3, center)).unwrap_or(std::cmp::Ordering::Equal)) {
                if angdiff(t.3, center) < 360.0 / k as f64 && !picked.iter().any(|p| (p.0 - t.0).abs() < 1e-6 && (p.1 - t.1).abs() < 1e-6) {
                    picked.push((t.0, t.1, t.3));
                }
            }
        }
        if picked.is_empty() { return Err("スポットが足りない".into()); }
        picked.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
        for (la, lo, _) in picked { wps.push((la, lo)); }
        wps.push(origin); // 周回で戻る
    }
    Ok(wps)
}
// 文字列を cells 幅に収める(非ASCII=2セル概算)。不足は空白パディング。
fn fit_cells(s: &str, cells: usize) -> String {
    let (mut w, mut o) = (0usize, String::new());
    for ch in s.chars() {
        let cw = if ch.is_ascii() { 1 } else { 2 };
        if w + cw > cells { break; }
        o.push(ch); w += cw;
    }
    while w < cells { o.push(' '); w += 1; }
    o
}

// 対話モードの操作マニュアル(? で表示)
const HELP: &[&str] = &[
    " termmap 対話モード ─ 操作マニュアル",
    "",
    " [移動]",
    "   ←↑↓→        パン (Shift+矢印で大きく)",
    "   + / -          ズーム",
    "   /              住所・地名で検索して移動",
    "   a              中心の住所を表示",
    "   Enter          中心付近の最寄りお気に入りにスナップ＋名前表示",
    "",
    " [ルートを作る]  中心の十字(黄)が置く位置",
    "   s / e / v      中心を 始点 / 終点 / 経由点 にする",
    "   Tab / S-Tab    点を選択 (白丸で強調)",
    "   [ / ]          選択点を 前 / 後ろ へ並べ替え",
    "   x              選択点を削除     c  ルート全消去",
    "   m              モード切替  下道 → 高速 → 最短",
    "   n              代替ルート候補を巡回(BRouterの案 1〜4)",
    "   r              道路名/refで現在view内の道路を経路に追加(例: 国道16号 / E20)。複数のrで道を連結",
    "   W              走りまくり: 峠/展望を巡る周回を自動生成(連打で別案)",
    "",
    " [目的地・お気に入り]",
    "   f              カテゴリ検索 1ｶﾞｿ 2ｶﾌｪ 3ｺﾝﾋﾞﾆ 4道の駅 5展望 6公園 7峠道",
    "                   / でキーワード周辺検索(現在範囲) → リスト",
    "                   → リスト: ↑↓選択 Enter移動 / s始点 e終点 v経由 / f再検索 Esc閉",
    "   S / L          ルートを お気に入り保存 / 呼び出し",
    "   g              ルートを GPX 保存 (termmap-route.gpx)",
    "   E              標高プロファイル 表示/非表示 (ルート確定後・下部に折れ線)",
    "   A              ルート再生 開始/停止 (プレビュー走行・全体を約20秒で自動パン)",
    "   G              ライブ現在地 ON/OFF (CoreLocationCLIを5秒毎・自位置と軌跡を表示)",
    "",
    " [マイスポット] (ラーメン等をカテゴリ別に色分け保存)",
    "   P              カテゴリ一覧を開く",
    "                   カテゴリ: ↑↓ Enter=中へ n新規 r改名 c色 x削除(空のみ)",
    "                   スポット: ↑↓ Enter=移動 n新規(現在地) x削除 Esc戻る",
    "   V              マイスポットの表示 / 非表示",
    "   o              スマホ共有(GoogleマップのQRをポップアップ表示)",
    "",
    " [実写]",
    "   i              中心地点の実写(Street View)を全画面表示  ←→向き ↑↓前後移動 Esc/q戻る",
    "                   要 config.toml [streetview] api_key",
    "",
    " [起動オプション]  --range KM,.. 航続リング / --route / --load-route 名前",
    "",
    "",
    " [設定]  , で設定画面 (braille/classify/edge/mono/style を実行中に切替・sで保存)",
    "         config.toml で既定を指定可 ([display]/[streetview])",
    "",
    "   ?  ヘルプ   q  終了   Esc  サブモード取消   Ctrl+C  計算の中断(終了はq)",
    "",
    "   (任意のキーで閉じる)",
];

// ---- 対話モード (crossterm) ----
// 端末状態を RAII で復元する。パニック/早期return でも Drop で raw mode と代替スクリーンを必ず戻す。
struct TermGuard;
impl TermGuard {
    fn enter() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide, crossterm::event::EnableBracketedPaste)?;
        Ok(Self)
    }
}
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste,
            crossterm::cursor::Show, crossterm::terminal::LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn interactive(mut cx: f64, mut cy: f64, mut z: u32, a: &Args) -> std::io::Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    enum Focus { Map, Search(String), SaveName(String), NearSearch(String), PoiMenu, PoiList, RouteList, WaypointList,
                 NewCat(String), SpotName(String, String), SpotList, SpotCatList, SpotRename(String, usize), Settings, RoadSearch(String) }
    let _guard = TermGuard::enter()?; // Drop で必ず端末復元
    let mut cache: Cache = HashMap::new();
    let mut out = std::io::stdout();
    let mut addr = String::new();          // 'a' 住所 / 一時メッセージ
    let mut focus = Focus::Map;
    let mut cfg = config::load_config();   // 設定(streetview key / 描画既定 等・設定画面で書き換え)
    let mut opts = a.clone();              // 実行中に変えられる描画設定(Argsのコピー)
    // config を既定として適用(CLIフラグは ON 方向で優先。style は CLI が既定osmなら config 採用)
    opts.braille = opts.braille || cfg.braille;
    opts.classify = opts.classify || cfg.classify;
    opts.edge = opts.edge || cfg.edge;
    opts.mono = opts.mono || cfg.mono;
    if opts.style == "osm" { opts.style = cfg.style.clone(); }
    let mut set_sel: usize = 0;            // 設定画面の選択行
    let mut street: Option<(RgbImage, i32, f64, f64)> = None; // 実写(画像, heading, lat, lon)

    let (home_lat, home_lon) = pixel_to_deg(cx, cy, z);
    let mut spec = build_spec(a, home_lat, home_lon); // --range のリングは保持

    let mut wps: Vec<(f64, f64)> = a.route.clone().unwrap_or_default(); // 始点..終点
    let mut wp_sel: usize = 0;             // Tab で巡回する選択 waypoint
    let mut mode = a.route_mode.clone();
    let mut pois: Vec<(f64, f64, String, PoiCat)> = Vec::new(); // 目的地検索結果
    let mut poi_sel: usize = 0;
    let mut poi_label = String::new();
    let mut route_names: Vec<String> = Vec::new(); // お気に入り一覧(L)
    let mut rn_sel: usize = 0;
    let mut help = false; // ? でヘルプ表示
    let mut qr_view: Option<String> = None; // o でGoogleマップQRをポップアップ表示
    let mut route_alt: u32 = 0; // n で BRouter の代替ルート(0..=3)を巡回
    let mut route_ele: Vec<f64> = Vec::new(); // 直近ルートの標高列(pts と同数)
    let mut route_ascend: f64 = 0.0;          // 直近ルートの累積登り(m)
    let mut show_elev = false;                // E で標高プロファイル表示
    let mut gps_rx: Option<std::sync::mpsc::Receiver<(f64, f64)>> = None; // G ライブ現在地の受信
    let mut gps_pos: Option<(f64, f64)> = None; // 最新の自位置
    let mut gps_trail: Vec<(f64, f64)> = Vec::new(); // 通過ブレッドクラム
    let mut play: Option<f64> = None; // A ルート再生(先頭からの距離m。Noneで停止)
    let mut scache = searchcache::load(); // 検索結果キャッシュ(キーワード+位置→結果。API節約)
    let mut popup: Option<String> = None; // 中央に出す一時ポップアップ(スポット名等・任意キーで閉じる)
    // ルート計算のバックグラウンド受信(マーカーは即時、ルート線は別スレッド)
    let (mut route_note, mut route_job) = {
        let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0);
        (n_, j_)
    };
    let mut spots = load_spots();          // マイスポット
    let mut spot_cats = load_spot_cats();
    let mut show_spots = true;
    let mut sp_sel: usize = 0;
    let mut cat_sel: usize = 0;
    let mut cur_cat = String::new(); // スポット一覧で表示中のカテゴリ
    apply_spots(&mut spec, &spots, &spot_cats, show_spots);

    let _ = write!(out, "\x1b[2J");
    loop {
        let (tc, tr) = crossterm::terminal::size().unwrap_or((100, 40));
        let cols = tc.max(20) as u32;
        let map_rows = (tr.max(3) - 1) as u32;
        if help { // ヘルプ全画面。任意キーで閉じる
            let _ = write!(out, "\x1b[2J\x1b[H");
            for (i, l) in HELP.iter().enumerate().take(map_rows as usize) {
                let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + 1, l);
            }
            let _ = write!(out, "\x1b[{};1H\x1b[7m 任意のキーで閉じる \x1b[0m\x1b[K", tr);
            let _ = out.flush();
            if let Event::Key(_) = event::read()? { help = false; }
            continue;
        }
        if street.is_some() { // 実写(Street View)全画面。←→で向き、Esc/qで戻る
            { // 描画(不変借用のスコープ)
                let (img, heading, slat, slon) = street.as_ref().unwrap();
                let rs = image::imageops::resize(img, cols.max(10), map_rows * 2, FilterType::Triangle);
                let art = render_halfblock(&rs);
                let sv_lines: Vec<&str> = art.split("\r\n").collect();
                let _ = write!(out, "\x1b[H");
                for i in 0..map_rows as usize {
                    let ln = sv_lines.get(i).copied().unwrap_or("");
                    let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + 1, ln);
                }
                let hd = ((heading % 360) + 360) % 360;
                let st = fit_cells(&format!(" 実写 h{hd}°  ←→向き ↑↓移動  Esc/q戻る  {slat:.4},{slon:.4} "), cols as usize);
                let _ = write!(out, "\x1b[{};1H\x1b[7m{st}\x1b[0m\x1b[K", tr);
                let _ = out.flush();
            }
            let (hd_c, slat_c, slon_c) = { let (_, h, la, lo) = street.as_ref().unwrap(); (*h, *la, *lo) };
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                        // ←→=向き回転 / ↑↓=向き方向に前後移動(隣パノラマへスナップ)
                        let (nlat, nlon, nhd) = match k.code {
                            KeyCode::Left => (slat_c, slon_c, hd_c - 45),
                            KeyCode::Right => (slat_c, slon_c, hd_c + 45),
                            KeyCode::Up => { let (a, b) = streetview::step(slat_c, slon_c, hd_c as f64, 20.0); (a, b, hd_c) }
                            _ => { let (a, b) = streetview::step(slat_c, slon_c, hd_c as f64 + 180.0, 20.0); (a, b, hd_c) }
                        };
                        if let Ok(im) = streetview::fetch(nlat, nlon, nhd, 640, 480, &cfg.streetview_api_key) {
                            street = Some((im, nhd, nlat, nlon)); // Err時は現状維持(行き止まり等)
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => street = None,
                    _ => {}
                }
            }
            continue;
        }
        // 標高プロファイル帯を出すぶん地図の行数を減らす(E)
        let elev_on = show_elev && !spec.routes.is_empty() && route_ele.len() >= 2 && route_ele.iter().any(|&z| z != 0.0);
        let elev_h: u32 = if elev_on { (map_rows / 3).clamp(4, 12) } else { 0 };
        let map_rows = if elev_h > 0 { map_rows.saturating_sub(elev_h + 1).max(3) } else { map_rows };
        let show_routes = matches!(focus, Focus::RouteList);
        let show_wps = matches!(focus, Focus::WaypointList);
        let show_splist = matches!(focus, Focus::SpotList);
        let show_catlist = matches!(focus, Focus::SpotCatList);
        let show_settings = matches!(focus, Focus::Settings);
        let gut: u32 = if !pois.is_empty() || show_routes || show_wps || show_splist || show_catlist || show_settings { 26 } else { 0 };
        let map_cols = cols.saturating_sub(gut).max(10);
        let (ow, oh) = if opts.braille || opts.edge { (map_cols * 2, map_rows * 4) } else { (map_cols, map_rows * 2) };
        if let Some(rx) = &gps_rx { // ライブ現在地を取り込み、自位置に追従
            while let Ok((la, lo)) = rx.try_recv() {
                gps_pos = Some((la, lo));
                gps_trail.push((la, lo));
                if gps_trail.len() > 300 { gps_trail.remove(0); }
                let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny;
            }
        }
        if play.is_some() { // ルート再生: 位置を進めて自動パン(全体を約20秒で走破)
            if let Some(rt) = spec.routes.last().map(|r| r.pts.clone()) {
                if rt.len() >= 2 {
                    let total = roadtrace::polyline_len(&rt);
                    let d = play.unwrap() + (total / 250.0).max(1.0);
                    if d >= total { play = None; addr = "再生: 終了".into(); }
                    else {
                        play = Some(d);
                        let (pla, plo) = roadtrace::point_at(&rt, d);
                        let (nx, ny) = deg_to_pixel(pla, plo, z); cx = nx; cy = ny;
                    }
                } else { play = None; }
            } else { play = None; }
        }
        let (lat, lon) = pixel_to_deg(cx, cy, z);

        let body = match build_window(cx, cy, z, ow, oh, &opts.style, &mut cache) {
            Ok(img) => {
                let mut ov = build_overlay(&spec, cx, cy, z, ow, oh, 1.0, 1.0, ow, oh);
                let (mx, my) = (ow as i32 / 2, oh as i32 / 2); // 中心クロスヘア(黄)
                draw_line(&mut ov, mx - 6, my, mx + 6, my, [255, 255, 0], 1);
                draw_line(&mut ov, mx, my - 6, mx, my + 6, [255, 255, 0], 1);
                if gps_pos.is_some() { // ライブ現在地: トレイル(薄青)+自位置(赤)
                    for (tla, tlo) in &gps_trail {
                        let (gx, gy) = deg_to_pixel(*tla, *tlo, z);
                        let ix = (gx - (cx - ow as f64 / 2.0)).floor() as i32;
                        let iy = (gy - (cy - oh as f64 / 2.0)).floor() as i32;
                        draw_ring(&mut ov, ix, iy, 1, [80, 160, 255], 1);
                    }
                    if let Some((gla, glo)) = gps_pos {
                        let (gx, gy) = deg_to_pixel(gla, glo, z);
                        let ix = (gx - (cx - ow as f64 / 2.0)).floor() as i32;
                        let iy = (gy - (cy - oh as f64 / 2.0)).floor() as i32;
                        draw_ring(&mut ov, ix, iy, 4, [255, 60, 60], 2);
                    }
                }
                if !wps.is_empty() { // 選択中(Tab)の waypoint を白丸で強調
                    let s = wp_sel.min(wps.len() - 1);
                    let (gx, gy) = deg_to_pixel(wps[s].0, wps[s].1, z);
                    let ix = (gx - (cx - ow as f64 / 2.0)).floor() as i32;
                    let iy = (gy - (cy - oh as f64 / 2.0)).floor() as i32;
                    draw_ring(&mut ov, ix, iy, 3, [255, 255, 255], 1);
                }
                render(&img, &opts, Some(&ov))
            }
            Err(e) => format!("取得失敗: {e}\r\n"),
        };

        // 左袖リスト(POI か お気に入り)の各行を組む
        let glines: Vec<String> = if gut > 0 {
            let gw = gut as usize;
            let (header, items, sel): (String, Vec<String>, usize) = if show_wps {
                let n = wps.len();
                let its = wps.iter().enumerate().map(|(i, (la, lo))| {
                    let role = if i == 0 { "始点" } else if i + 1 == n { "終点" } else { "経由" };
                    format!("#{} {} {:.3},{:.3}", i + 1, role, la, lo)
                }).collect();
                ("並べ替え".to_string(), its, wp_sel)
            } else if show_splist {
                let its = spots.iter().filter(|s| s.cat == cur_cat).map(|s| if s.name.is_empty() { "(無名)".to_string() } else { s.name.clone() }).collect();
                (format!("{cur_cat}"), its, sp_sel)
            } else if show_catlist {
                let its = spot_cats.iter().map(|(n, i)| format!("{} 色{}", n, i)).collect();
                ("カテゴリ".to_string(), its, cat_sel)
            } else if show_settings {
                let onoff = |b: bool| if b { "ON" } else { "OFF" };
                let keyset = if cfg.streetview_api_key.trim().is_empty() { "未設定" } else { "設定済" };
                let mode_ja = match cfg.route_profile.as_str() { "car-fast" => "高速", "moped" => "下道", "shortest" => "最短", o => o };
                let model_ja = match cfg.llm_model.as_str() { "claude-sonnet-5" => "sonnet", "claude-haiku-4-5" => "haiku", "claude-opus-4-8" => "opus", o => o };
                let its = vec![
                    format!("点字ドット {}", onoff(opts.braille)),
                    format!("地物色分け {}", onoff(opts.classify)),
                    format!("輪郭抽出 {}", onoff(opts.edge)),
                    format!("単色 {}", onoff(opts.mono)),
                    format!("地図種別 {}", opts.style),
                    format!("既定ルート {}", mode_ja),
                    format!("道路の点間隔 {}m", cfg.sample_interval_m as i64),
                    format!("スポット既定表示 {}", onoff(cfg.show_spots)),
                    format!("おすすめ {}", onoff(cfg.llm_recommend_enabled)),
                    format!("提案AIモデル {}", model_ja),
                    format!("APIキー {}", keyset),
                ];
                ("設定".to_string(), its, set_sel)
            } else if show_routes {
                ("お気に入り".to_string(), route_names.clone(), rn_sel)
            } else {
                let its = pois.iter().map(|(la, lo, nm, _)| {
                    let d = haversine_km((lat, lon), (*la, *lo));
                    format!("{} {:.1}k", if nm.is_empty() { "(無名)" } else { nm }, d)
                }).collect();
                (poi_label.clone(), its, poi_sel)
            };
            let mut gl = Vec::with_capacity(map_rows as usize);
            gl.push(fit_cells(&format!("[{} {}]", header, items.len()), gw));
            for (idx, it) in items.iter().enumerate() {
                let cell = fit_cells(&format!("{}{}", if idx == sel { ">" } else { " " }, it), gw);
                gl.push(if idx == sel { format!("\x1b[7m{cell}\x1b[0m") } else { cell });
            }
            gl
        } else { Vec::new() };

        // 左袖 + 地図 を絶対座標で配置
        let _ = write!(out, "\x1b[H");
        let lines: Vec<&str> = body.split("\r\n").collect();
        let blank = fit_cells("", gut as usize);
        for i in 0..map_rows as usize {
            let ln = lines.get(i).copied().unwrap_or("");
            if gut > 0 {
                let g = glines.get(i).cloned().unwrap_or_else(|| blank.clone());
                write!(out, "\x1b[{};1H{}\x1b[{};{}H{}", i + 1, g, i + 1, gut + 1, ln)?;
            } else {
                write!(out, "\x1b[{};1H{}", i + 1, ln)?;
            }
        }
        if elev_h > 0 { // 標高プロファイル帯(地図の下・ステータスの上)
            let (mn, mx, _asc) = elevation::elevation_stats(&route_ele);
            let label = fit_cells(&format!(" 標高 ↑{route_ascend:.0}m  最高{mx:.0}m 最低{mn:.0}m  (Eで消す) "), cols as usize);
            let _ = write!(out, "\x1b[{};1H\x1b[7m{label}\x1b[0m\x1b[K", map_rows + 1);
            let chart = elevation::elevation_chart(&route_ele, cols as usize, elev_h as usize);
            for (i, line) in chart.iter().enumerate() {
                let _ = write!(out, "\x1b[{};1H{}\x1b[K", map_rows + 2 + i as u32, line);
            }
            // 地図中心が経路上のどこかを示す縦カーソル(パン/再生で動く)
            if let Some(rt) = spec.routes.last() {
                if rt.pts.len() >= 2 {
                    let (mut bi, mut bd) = (0usize, f64::MAX);
                    for (i, p) in rt.pts.iter().enumerate() {
                        let d = (p.0 - lat).powi(2) + (p.1 - lon).powi(2);
                        if d < bd { bd = d; bi = i; }
                    }
                    let col = elevation::profile_col(rt.pts.len(), bi, cols as usize);
                    for i in 0..elev_h as usize {
                        let _ = write!(out, "\x1b[{};{}H\x1b[1;31m|\x1b[0m", map_rows + 2 + i as u32, col + 1);
                    }
                }
            }
        }
        let status = match &focus {
            Focus::Search(buf) => format!(" 検索: {buf}\u{2588}   Enter=移動 Esc=取消 "),
            Focus::SaveName(buf) => format!(" ルート名: {buf}\u{2588}   Enter=保存 Esc=取消 "),
            Focus::NearSearch(buf) => format!(" 周辺検索: {buf}\u{2588}   Enter=検索 Esc=取消 "),
            Focus::NewCat(buf) => format!(" 新規カテゴリ: {buf}\u{2588}   Enter=作成 Esc=取消 "),
            Focus::SpotName(buf, cat) => format!(" [{cat}] 名前 or GoogleマップURL: {buf}\u{2588}   Enter=保存 Esc=取消 "),
            Focus::SpotList => format!(" [{cur_cat}] ↑↓選択 Enter=移動 n=新規(現在地) x=削除 Esc=戻る "),
            Focus::SpotCatList => " カテゴリ: ↑↓選択 Enter=中へ n新規 r改名 c色 x削除(空のみ) Esc=閉 ".to_string(),
            Focus::Settings => {
                let desc = match set_sel {
                    0 => "braille: 点字ドットで高精細描画(色は淡め)。OFFはハーフブロック",
                    1 => "classify: 地物を色分け(水域/緑地/道路/建物)。地形が見やすい",
                    2 => "edge: 輪郭抽出表示(線画風)",
                    3 => "mono: 単色描画(色を使わない)",
                    4 => "style: タイル種別を循環(osm=標準/voyager/dark=暗/light=淡)",
                    5 => "既定mode: 起動時のルート種別。car-fast=高速優先 / moped=下道(高速回避) / shortest=最短距離",
                    6 => "道路の点間隔: rの道路名ルートで、その道を何mおきの点でなぞるか(小=忠実で点多/大=粗い)。←→で調整",
                    7 => "spot既定: 起動時にお気に入りスポットを表示するか",
                    8 => "おすすめ: claude -p でツーリングスポットを提案する機能のON/OFF(未実装)",
                    9 => "LLM: おすすめに使うモデルを循環(claude-sonnet-5/haiku/opus)",
                    _ => "APIkey: Google(Street View/検索)のキー。この行でCmd+V貼付→設定、sで保存",
                };
                format!(" ▶ {desc}   [↑↓選択 Enter切替 s保存 Esc閉]")
            }
            Focus::RoadSearch(buf) => format!(" 道路名/ref: {buf}\u{2588}   Enter=view内を経路に追加(複数連結可・cで全消去) Esc=取消 "),
            Focus::SpotRename(buf, _) => format!(" カテゴリ改名: {buf}\u{2588}   Enter=確定 Esc=取消 "),
            Focus::PoiMenu => " 目的地: 1ガソスタ 2カフェ 3コンビニ 4道の駅 5展望 6公園 7峠道  / キーワード周辺検索  Esc=取消 ".to_string(),
            Focus::PoiList => format!(" [{}] ↑↓選択 Enter=移動 s始 e終 v経由 f再検索 Esc閉 ", poi_label),
            Focus::RouteList => " お気に入り: ↑↓選択 Enter=読込 Esc=閉 ".to_string(),
            Focus::WaypointList => " 並べ替え: ↑↓/Tab 選択  [ ]移動  x削除  +/-拡縮  Esc閉 ".to_string(),
            Focus::Map => {
                let live = if gps_rx.is_some() { "●LIVE(Gで解除) " } else { "" };
                let playing = if play.is_some() { "▶再生中(Aで停止) " } else { "" };
                let base = format!(" {live}{playing}?ヘルプ z{z} {lat:.4},{lon:.4} | s始 e終 v経由({}) m:{} n候補 r道路 W走 f目的地 P点 V表 i実写 E標高 A再生 G現在地 S保存 L呼出 gGPX o共有 ,設定 /検索 q",
                    wps.len(), mode_label(&mode));
                match &route_note { Some(rn) => format!("{base} | {rn} "), None => base }
            }
        };
        let status = fit_cells(&status, cols as usize);
        write!(out, "\x1b[{};1H\x1b[7m{status}\x1b[0m", tr)?;

        if let Some(msg) = &popup { // 中央に名前ポップアップ(任意キーで閉じる)
            let text = format!("  {}  ", msg);
            let w = text.chars().count();
            let c0 = ((cols as usize).saturating_sub(w) / 2).max(1);
            let r0 = (map_rows / 2).max(1);
            let pad = " ".repeat(w);
            let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0, c0, pad);
            let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0 + 1, c0, text);
            let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0 + 2, c0, pad);
        }

        // QR共有ポップアップ(地図の上に白地で重ねる。白地×黒でどのテーマでもスキャン可)
        if let Some(q) = &qr_view {
            let lines: Vec<&str> = q.lines().collect();
            let qw = lines.iter().map(|l| l.chars().count()).max().unwrap_or(21);
            let bw = qw + 2; // 左右1マスの白余白(quiet zoneは切ってあるので自前で細枠)
            let c0 = (cols as usize).saturating_sub(bw) as u32 / 2 + 1;
            let r0 = ((map_rows as usize).saturating_sub(lines.len() + 3) / 2).max(1) as u32;
            let blank = " ".repeat(bw);
            let _ = write!(out, "\x1b[{r0};{c0}Hスマホでスキャン→Googleマップ");
            let _ = write!(out, "\x1b[{};{c0}H\x1b[30;47m{blank}\x1b[0m", r0 + 1); // 上白余白
            for (i, l) in lines.iter().enumerate() {
                let _ = write!(out, "\x1b[{};{c0}H\x1b[30;47m {} \x1b[0m", r0 + 2 + i as u32, l);
            }
            let _ = write!(out, "\x1b[{};{c0}H\x1b[30;47m{blank}\x1b[0m", r0 + 2 + lines.len() as u32); // 下白余白
            let _ = write!(out, "\x1b[{};1H\x1b[7m 任意のキーで閉じる \x1b[0m\x1b[K", tr);
        }
        out.flush()?;

        // 入力待ち。ルート計算中(route_job)はポーリングして結果を取り込む
        let ev: Option<Event> = if route_job.is_some() {
            match route_job.as_ref().unwrap().try_recv() {
                Ok(Ok(r)) => {
                    spec.routes.clear();
                    route_note = Some(route_summary(&mode, &r));
                    route_ele = r.ele;
                    route_ascend = r.ascend_m;
                    spec.routes.push(Route { pts: r.pts, color: [0, 220, 255], thickness: 2 });
                    route_job = None;
                    None
                }
                Ok(Err(e)) => { route_note = Some(format!("({e})")); route_job = None; None }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    if event::poll(std::time::Duration::from_millis(80))? { Some(event::read()?) } else { None }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => { route_job = None; None }
            }
        } else if gps_rx.is_some() || play.is_some() {
            if event::poll(std::time::Duration::from_millis(80))? { Some(event::read()?) } else { None }
        } else {
            Some(event::read()?)
        };
        match ev {
            None => {} // 再描画のみ(計算待ち)
            Some(Event::Key(k)) if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) => {
                if route_job.is_some() { route_job = None; route_note = Some("中断".to_string()); } // 計算中断(アプリは終了しない)
            }
            Some(Event::Key(_)) if qr_view.is_some() => qr_view = None, // ポップアップを閉じる
            Some(Event::Key(_)) if popup.is_some() => popup = None, // 名前ポップアップを閉じる
            Some(Event::Key(k)) => {
                let cur = std::mem::replace(&mut focus, Focus::Map);
                match cur {
                    Focus::Search(mut buf) => match k.code {
                        KeyCode::Enter => { // 候補を一覧表示(左袖)。Enterで移動/s e vで経路点
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                let ckey = searchcache::make_key(&q, lat, lon);
                                let results: Vec<(f64, f64, String)> = match scache.get(&ckey) {
                                    Some(v) => v.clone(), // キャッシュヒット=API叩かない
                                    None => {
                                        let r = geocode_list(&q, Some((lat, lon)), &cfg.streetview_api_key);
                                        if !r.is_empty() { scache.insert(ckey, r.clone()); let _ = searchcache::save(&scache); }
                                        r
                                    }
                                };
                                if results.is_empty() { addr = format!("見つからない: {q}"); }
                                else {
                                    pois = results.into_iter().take(8).map(|(la, lo, nm)| (la, lo, nm, PoiCat::Waypoint)).collect();
                                    poi_sel = 0;
                                    poi_label = format!("検索:{q}");
                                    set_markers(&mut spec, &wps, &pois);
                                    focus = Focus::PoiList;
                                }
                            }
                        }
                        KeyCode::Esc => {}
                        KeyCode::Backspace => { buf.pop(); focus = Focus::Search(buf); }
                        KeyCode::Char(c) => { buf.push(c); focus = Focus::Search(buf); }
                        _ => focus = Focus::Search(buf),
                    },
                    Focus::SpotCatList => match k.code { // カテゴリ一覧(P)
                        KeyCode::Up => { cat_sel = cat_sel.saturating_sub(1); focus = Focus::SpotCatList; }
                        KeyCode::Down => { if cat_sel + 1 < spot_cats.len() { cat_sel += 1; } focus = Focus::SpotCatList; }
                        KeyCode::Char('n') => focus = Focus::NewCat(String::new()),
                        KeyCode::Char('r') => { if let Some((n, _)) = spot_cats.get(cat_sel) { focus = Focus::SpotRename(n.clone(), cat_sel); } else { focus = Focus::SpotCatList; } }
                        KeyCode::Char('c') => {
                            if let Some(e) = spot_cats.get_mut(cat_sel) { e.1 = (e.1 + 1) % SPOT_PALETTE.len() as u8; let _ = save_all_cats(&spot_cats); apply_spots(&mut spec, &spots, &spot_cats, show_spots); }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Char('x') => {
                            if let Some((name, _)) = spot_cats.get(cat_sel).cloned() {
                                if spots.iter().any(|s| s.cat == name) { addr = format!("使用中: {name}(先に空に)"); }
                                else { spot_cats.remove(cat_sel); if cat_sel >= spot_cats.len() && cat_sel > 0 { cat_sel -= 1; } let _ = save_all_cats(&spot_cats); }
                            }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Enter => { if let Some((name, _)) = spot_cats.get(cat_sel) { cur_cat = name.clone(); sp_sel = 0; focus = Focus::SpotList; } else { focus = Focus::SpotCatList; } }
                        KeyCode::Esc => {}
                        _ => focus = Focus::SpotCatList,
                    },
                    Focus::Settings => { let mut stay = true; match k.code { // 設定画面
                        KeyCode::Up => { set_sel = set_sel.saturating_sub(1); }
                        KeyCode::Down => { if set_sel + 1 < 11 { set_sel += 1; } }
                        KeyCode::Left | KeyCode::Right => {
                            if set_sel == 6 { let d = if k.code == KeyCode::Left { -100.0 } else { 100.0 }; cfg.sample_interval_m = (cfg.sample_interval_m + d).clamp(100.0, 5000.0); }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => match set_sel {
                            0 => opts.braille = !opts.braille,
                            1 => opts.classify = !opts.classify,
                            2 => opts.edge = !opts.edge,
                            3 => opts.mono = !opts.mono,
                            4 => { opts.style = match opts.style.as_str() { "osm" => "voyager", "voyager" => "dark", "dark" => "light", _ => "osm" }.to_string(); cache.clear(); }
                            5 => cfg.route_profile = match cfg.route_profile.as_str() { "car-fast" => "moped", "moped" => "shortest", _ => "car-fast" }.to_string(),
                            6 => {} // ←→で調整
                            7 => { cfg.show_spots = !cfg.show_spots; show_spots = cfg.show_spots; apply_spots(&mut spec, &spots, &spot_cats, show_spots); }
                            8 => cfg.llm_recommend_enabled = !cfg.llm_recommend_enabled,
                            9 => cfg.llm_model = match cfg.llm_model.as_str() { "claude-sonnet-5" => "claude-haiku-4-5", "claude-haiku-4-5" => "claude-opus-4-8", _ => "claude-sonnet-5" }.to_string(),
                            _ => addr = "APIkey: この行で貼り付け(Cmd+V)して設定".into(),
                        },
                        KeyCode::Char('s') => {
                            cfg.braille = opts.braille; cfg.classify = opts.classify; cfg.edge = opts.edge; cfg.mono = opts.mono; cfg.style = opts.style.clone();
                            addr = match config::save_config(&cfg) { Ok(_) => "設定を保存(config.toml)".into(), Err(e) => format!("保存失敗: {e}") };
                        }
                        KeyCode::Esc => { stay = false; }
                        _ => {}
                    } if stay { focus = Focus::Settings; } },
                    Focus::RoadSearch(mut buf) => match k.code { // 道路名/ref で現在view内をルート化
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            if !name.is_empty() {
                                let (n_lat, w_lon) = pixel_to_deg(cx - ow as f64 / 2.0, cy - oh as f64 / 2.0, z);
                                let (s_lat, e_lon) = pixel_to_deg(cx + ow as f64 / 2.0, cy + oh as f64 / 2.0, z);
                                match roadsearch::fetch(&name, s_lat, w_lon, n_lat, e_lon) {
                                    Ok(frags) if !frags.is_empty() => {
                                        let rf: Vec<roadtrace::RoadFrag> = frags.into_iter().map(|(pts, oneway)| roadtrace::RoadFrag { pts, oneway }).collect();
                                        let poly = roadtrace::assemble_polyline(&rf);
                                        let samp = roadtrace::sample_every(&poly, cfg.sample_interval_m.max(100.0));
                                        if samp.len() >= 2 {
                                            wps.extend(samp); // 複数の道路名を順に繋げる(cで全消去してから始めると綺麗)
                                            if wps.len() > 150 { wps.truncate(150); } // BRouter過負荷防止の上限
                                            wp_sel = 0;
                                            let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, 0);
                                            route_note = nn; route_job = jj;
                                            addr = format!("道路: {name} 追加 (計{}点/連結は次のrで続けて指定)", wps.len());
                                        } else { addr = "道路: 点が足りない(拡大/移動して再検索)".into(); }
                                    }
                                    Ok(_) => addr = format!("道路が見つからない: {name}(view内に無い)"),
                                    Err(e) => addr = format!("道路: {e}"),
                                }
                            }
                        }
                        KeyCode::Esc => {}
                        KeyCode::Backspace => { buf.pop(); focus = Focus::RoadSearch(buf); }
                        KeyCode::Char(c) => { buf.push(c); focus = Focus::RoadSearch(buf); }
                        _ => focus = Focus::RoadSearch(buf),
                    },
                    Focus::SpotList => match k.code { // cur_cat のスポット一覧
                        KeyCode::Up => { sp_sel = sp_sel.saturating_sub(1); focus = Focus::SpotList; }
                        KeyCode::Down => { let n = spots.iter().filter(|s| s.cat == cur_cat).count(); if sp_sel + 1 < n { sp_sel += 1; } focus = Focus::SpotList; }
                        KeyCode::Char('n') => focus = Focus::SpotName(String::new(), cur_cat.clone()), // 現在地を新規スポット
                        KeyCode::Enter => {
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(sp_sel) { let (nx, ny) = deg_to_pixel(spots[gi].lat, spots[gi].lon, z); cx = nx; cy = ny; }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Char('x') => {
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(sp_sel) {
                                spots.remove(gi);
                                if sp_sel > 0 && sp_sel >= idxs.len() - 1 { sp_sel -= 1; }
                                let _ = save_all_spots(&spots);
                                apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                            }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Esc => focus = Focus::SpotCatList,
                        _ => focus = Focus::SpotList,
                    },
                    Focus::NewCat(mut buf) => match k.code {
                        KeyCode::Enter => { let name = buf.trim().to_string(); if !name.is_empty() { let _ = ensure_spot_cat(&name, &mut spot_cats); } focus = Focus::SpotCatList; }
                        KeyCode::Esc => focus = Focus::SpotCatList,
                        KeyCode::Backspace => { buf.pop(); focus = Focus::NewCat(buf); }
                        KeyCode::Char(c) => { buf.push(c); focus = Focus::NewCat(buf); }
                        _ => focus = Focus::NewCat(buf),
                    },
                    Focus::SpotRename(mut buf, idx) => match k.code {
                        KeyCode::Enter => {
                            let new = spot_clean(buf.trim());
                            if !new.is_empty() {
                                if let Some(old) = spot_cats.get(idx).map(|(n, _)| n.clone()) {
                                    for s in spots.iter_mut() { if s.cat == old { s.cat = new.clone(); } }
                                    if let Some(e) = spot_cats.get_mut(idx) { e.0 = new; }
                                    let _ = save_all_spots(&spots);
                                    let _ = save_all_cats(&spot_cats);
                                    apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                                }
                            }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Esc => focus = Focus::SpotCatList,
                        KeyCode::Backspace => { buf.pop(); focus = Focus::SpotRename(buf, idx); }
                        KeyCode::Char(c) => { buf.push(c); focus = Focus::SpotRename(buf, idx); }
                        _ => focus = Focus::SpotRename(buf, idx),
                    },
                    Focus::SpotName(mut buf, cat) => match k.code {
                        KeyCode::Enter => {
                            let t = buf.trim().to_string();
                            // 決定: 保存(座標,名前) / エラー / 入力継続。空・不正URL・短縮URLを弾く
                            enum Act { Save(f64, f64, String), Err(String), Cont }
                            let act = if t.is_empty() { Act::Cont }
                                else if t.starts_with("http") {
                                    if t.contains("goo.gl") || t.contains("maps.app") { Act::Err("短縮URLは不可。Googleマップの通常URL(…/@…/!3d…!4d…)を貼って".into()) }
                                    else if let Some((la, lo, nm)) = parse_gmaps_place(&t) { Act::Save(la, lo, if nm.trim().is_empty() { "(無名)".into() } else { nm }) }
                                    else { Act::Err("URLから位置を取得できません(GoogleマップのURLか確認)".into()) }
                                } else { Act::Save(lat, lon, t.clone()) };
                            match act {
                                Act::Save(la, lo, name) => {
                                    let s = Spot { lat: la, lon: lo, cat: cat.clone(), name };
                                    let _ = ensure_spot_cat(&s.cat, &mut spot_cats);
                                    addr = match append_spot(&s) { Ok(_) => format!("スポット保存: {}", s.name), Err(e) => format!("({e})") };
                                    spots.push(s); show_spots = true; apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                                    focus = Focus::SpotList;
                                }
                                Act::Err(msg) => { addr = msg; focus = Focus::SpotName(t, cat); }
                                Act::Cont => focus = Focus::SpotName(String::new(), cat),
                            }
                        }
                        KeyCode::Esc => focus = Focus::SpotList,
                        KeyCode::Backspace => { buf.pop(); focus = Focus::SpotName(buf, cat); }
                        KeyCode::Char(c) => { buf.push(c); focus = Focus::SpotName(buf, cat); }
                        _ => focus = Focus::SpotName(buf, cat),
                    },
                    Focus::NearSearch(mut buf) => match k.code {
                        KeyCode::Enter => {
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                let (vt, vl) = pixel_to_deg(cx - ow as f64 * 1.25, cy - oh as f64 * 1.25, z);
                                let (vb, vr) = pixel_to_deg(cx + ow as f64 * 1.25, cy + oh as f64 * 1.25, z);
                                let rlat = 2.0 / 111.0;
                                let rlon = 2.0 / (111.0 * lat.to_radians().cos().abs().max(0.1));
                                // お気に入りスポット優先: 名前一致(大小無視)を★付きで先頭に(距離順)
                                let ql = q.to_lowercase();
                                let mut mine: Vec<(f64, f64, String, PoiCat)> = spots.iter()
                                    .filter(|s| s.name.to_lowercase().contains(&ql))
                                    .map(|s| (s.lat, s.lon, format!("★{}", s.name), PoiCat::Home)).collect();
                                mine.sort_by(|p, r| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (r.0, r.1))).unwrap_or(std::cmp::Ordering::Equal));
                                let v = search_nearby(&q, vb.min(lat - rlat), vl.min(lon - rlon), vt.max(lat + rlat), vr.max(lon + rlon));
                                let mut osm: Vec<(f64, f64, String, PoiCat)> = v.into_iter().map(|(a, b, nm)| (a, b, nm, PoiCat::Other)).collect();
                                osm.sort_by(|p, r| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (r.0, r.1))).unwrap_or(std::cmp::Ordering::Equal));
                                mine.extend(osm);
                                if mine.is_empty() { addr = format!("周辺に無し: {q}"); }
                                else {
                                    pois = mine; poi_sel = 0; poi_label = format!("周辺:{q}");
                                    set_markers(&mut spec, &wps, &pois);
                                    focus = Focus::PoiList;
                                }
                            }
                        }
                        KeyCode::Esc => {}
                        KeyCode::Backspace => { buf.pop(); focus = Focus::NearSearch(buf); }
                        KeyCode::Char(c) => { buf.push(c); focus = Focus::NearSearch(buf); }
                        _ => focus = Focus::NearSearch(buf),
                    },
                    Focus::PoiMenu => match k.code {
                        KeyCode::Esc => {}
                        KeyCode::Char('/') => focus = Focus::NearSearch(String::new()),
                        KeyCode::Char(c) => {
                            if let Some(kind) = POI_KINDS.iter().find(|kk| kk.key == c) {
                                // 表示範囲(2.5倍)と 半径2kmの箱 の広い方で検索(高ズームでも駅前を拾えるように)
                                let (vt, vl) = pixel_to_deg(cx - ow as f64 * 1.25, cy - oh as f64 * 1.25, z);
                                let (vb, vr) = pixel_to_deg(cx + ow as f64 * 1.25, cy + oh as f64 * 1.25, z);
                                let rlat = 2.0 / 111.0;
                                let rlon = 2.0 / (111.0 * lat.to_radians().cos().abs().max(0.1));
                                match fetch_pois(kind, vb.min(lat - rlat), vl.min(lon - rlon), vt.max(lat + rlat), vr.max(lon + rlon)) {
                                    Ok(v) => {
                                        let mut items: Vec<(f64, f64, String, PoiCat)> = v.into_iter().map(|(la, lo, nm)| (la, lo, nm, kind.cat)).collect();
                                        items.sort_by(|p, q| p.2.cmp(&q.2));
                                        items.dedup_by(|p, q| !p.2.is_empty() && p.2 == q.2);
                                        items.sort_by(|p, q| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (q.0, q.1))).unwrap_or(std::cmp::Ordering::Equal));
                                        items.truncate(50);
                                        if items.is_empty() { addr = format!("周辺2kmに{}無し", kind.label); }
                                        else { pois = items; poi_sel = 0; poi_label = kind.label.to_string(); set_markers(&mut spec, &wps, &pois); focus = Focus::PoiList; }
                                    }
                                    Err(e) => addr = format!("({e})"),
                                }
                            } else { focus = Focus::PoiMenu; }
                        }
                        _ => focus = Focus::PoiMenu,
                    },
                    Focus::PoiList => match k.code {
                        KeyCode::Up => { poi_sel = poi_sel.saturating_sub(1); focus = Focus::PoiList; }
                        KeyCode::Down => { if poi_sel + 1 < pois.len() { poi_sel += 1; } focus = Focus::PoiList; }
                        KeyCode::Enter => { // 選択地点へ移動するだけ(ルートには足さない)
                            if let Some(p) = pois.get(poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, z); cx = nx; cy = ny; }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('s') => {
                            if let Some(p) = pois.get(poi_sel) {
                                let pt = (p.0, p.1);
                                if wps.is_empty() { wps.push(pt); } else { wps[0] = pt; }
                                { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
                            }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('e') => {
                            if let Some(p) = pois.get(poi_sel) {
                                let pt = (p.0, p.1);
                                if wps.len() >= 2 { let l = wps.len() - 1; wps[l] = pt; } else { wps.push(pt); }
                                { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
                            }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('v') => {
                            if let Some(p) = pois.get(poi_sel) {
                                let pt = (p.0, p.1);
                                if wps.len() < 2 { wps.push(pt); } else { wps.insert(wps.len() - 1, pt); }
                                { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
                            }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('f') => focus = Focus::PoiMenu,
                        KeyCode::Esc => { pois.clear(); set_markers(&mut spec, &wps, &pois); }
                        _ => focus = Focus::PoiList,
                    },
                    Focus::SaveName(mut buf) => match k.code {
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            if !name.is_empty() {
                                addr = match save_named_route(&name, &mode, &wps) { Ok(_) => format!("保存: {name}"), Err(e) => format!("({e})") };
                            }
                        }
                        KeyCode::Esc => {}
                        KeyCode::Backspace => { buf.pop(); focus = Focus::SaveName(buf); }
                        KeyCode::Char(c) => { buf.push(c); focus = Focus::SaveName(buf); }
                        _ => focus = Focus::SaveName(buf),
                    },
                    Focus::RouteList => match k.code {
                        KeyCode::Up => { rn_sel = rn_sel.saturating_sub(1); focus = Focus::RouteList; }
                        KeyCode::Down => { if rn_sel + 1 < route_names.len() { rn_sel += 1; } focus = Focus::RouteList; }
                        KeyCode::Enter => {
                            if let Some(name) = route_names.get(rn_sel) {
                                if let Some((w, m)) = load_named_route(name) {
                                    let (nx, ny) = deg_to_pixel(w[0].0, w[0].1, z); cx = nx; cy = ny;
                                    wps = w; mode = m; wp_sel = 0;
                                    { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
                                }
                            }
                        }
                        KeyCode::Esc => {}
                        _ => focus = Focus::RouteList,
                    },
                    // 並べ替えパネル: 順序を見ながら選択/移動/削除
                    Focus::WaypointList => match k.code {
                        KeyCode::Up | KeyCode::BackTab => { if !wps.is_empty() { wp_sel = (wp_sel + wps.len() - 1) % wps.len(); } focus = Focus::WaypointList; }
                        KeyCode::Down | KeyCode::Tab => { if !wps.is_empty() { wp_sel = (wp_sel + 1) % wps.len(); } focus = Focus::WaypointList; }
                        KeyCode::Char('+') | KeyCode::Char('=') => { if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; } focus = Focus::WaypointList; }
                        KeyCode::Char('-') | KeyCode::Char('_') => { if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; } focus = Focus::WaypointList; }
                        KeyCode::Char('[') => { if wp_sel > 0 && wp_sel < wps.len() { wps.swap(wp_sel, wp_sel - 1); wp_sel -= 1; { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } } focus = Focus::WaypointList; }
                        KeyCode::Char(']') => { if wp_sel + 1 < wps.len() { wps.swap(wp_sel, wp_sel + 1); wp_sel += 1; { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } } focus = Focus::WaypointList; }
                        KeyCode::Char('x') => {
                            if !wps.is_empty() { let i = wp_sel.min(wps.len() - 1); wps.remove(i); if wp_sel >= wps.len() && wp_sel > 0 { wp_sel -= 1; } { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            if !wps.is_empty() { focus = Focus::WaypointList; } // 空になったら閉じる
                        }
                        KeyCode::Esc | KeyCode::Enter => {} // 閉じる → Map
                        _ => focus = Focus::WaypointList,
                    },
                    Focus::Map => {
                        let frac = if k.modifiers.contains(KeyModifiers::SHIFT) { 4.0 } else { 16.0 };
                        let step = (oh as f64 / frac).max(1.0);
                        let mut quit = false;
                        match k.code {
                            KeyCode::Left => { cx -= step; addr.clear(); }
                            KeyCode::Right => { cx += step; addr.clear(); }
                            KeyCode::Up => { cy -= step; addr.clear(); }
                            KeyCode::Down => { cy += step; addr.clear(); }
                            KeyCode::Char('+') | KeyCode::Char('=') => if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; addr.clear(); },
                            KeyCode::Char('-') | KeyCode::Char('_') => if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; addr.clear(); },
                            KeyCode::Enter => { // 中心付近の最寄りお気に入りにスナップ＋名前表示
                                let mut best: Option<(f64, usize)> = None;
                                for (i, s) in spots.iter().enumerate() {
                                    let (gx, gy) = deg_to_pixel(s.lat, s.lon, z);
                                    let dpx = ((gx - cx).powi(2) + (gy - cy).powi(2)).sqrt();
                                    if best.map_or(true, |(bd, _)| dpx < bd) { best = Some((dpx, i)); }
                                }
                                match best {
                                    Some((dpx, i)) if dpx <= (ow.min(oh) as f64) * 0.25 => {
                                        let s = &spots[i];
                                        let (nx, ny) = deg_to_pixel(s.lat, s.lon, z); cx = nx; cy = ny;
                                        popup = Some(if s.name.is_empty() { "★ (無名スポット)".into() } else { format!("★ {} [{}]", s.name, s.cat) });
                                    }
                                    Some(_) => addr = "近くにお気に入り無し".into(),
                                    None => addr = "お気に入り未登録".into(),
                                }
                            }
                            KeyCode::Char('a') => addr = reverse_geocode(lat, lon).unwrap_or_else(|e| format!("({e})")),
                            KeyCode::Char('/') => focus = Focus::Search(String::new()),
                            KeyCode::Char('f') => focus = Focus::PoiMenu,
                            KeyCode::Char('S') => focus = Focus::SaveName(String::new()),
                            KeyCode::Char('L') => { route_names = list_named_routes(); rn_sel = 0; if route_names.is_empty() { addr = "お気に入り無し".into(); } else { focus = Focus::RouteList; } }
                            KeyCode::Char('s') => { if wps.is_empty() { wps.push((lat, lon)); } else { wps[0] = (lat, lon); } { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char('e') => { if wps.len() >= 2 { let l = wps.len() - 1; wps[l] = (lat, lon); } else { wps.push((lat, lon)); } { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char('v') => { if wps.len() < 2 { wps.push((lat, lon)); } else { wps.insert(wps.len() - 1, (lat, lon)); } { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Tab | KeyCode::BackTab => { if !wps.is_empty() { focus = Focus::WaypointList; } } // 並べ替えパネル
                            KeyCode::Char('?') => help = true,
                            KeyCode::Char('P') => { cat_sel = 0; focus = Focus::SpotCatList; } // マイスポット(カテゴリ一覧)
                            KeyCode::Char(',') => { set_sel = 0; focus = Focus::Settings; } // 設定画面
                            KeyCode::Char('r') => focus = Focus::RoadSearch(String::new()), // 道路名でルート(現在view内)
                            KeyCode::Char('V') => { show_spots = !show_spots; apply_spots(&mut spec, &spots, &spot_cats, show_spots); addr = if show_spots { "マイスポット表示".into() } else { "マイスポット非表示".into() }; }
                            KeyCode::Char('E') => { // 標高プロファイルの表示/非表示
                                show_elev = !show_elev;
                                if show_elev && (spec.routes.is_empty() || !route_ele.iter().any(|&z| z != 0.0)) { addr = "標高: ルート確定後に表示".into(); }
                            }
                            KeyCode::Char('A') => { // ルート再生(プレビュー走行)の開始/停止
                                if spec.routes.last().map_or(false, |r| r.pts.len() >= 2) {
                                    if play.is_some() { play = None; addr = "再生: 停止".into(); }
                                    else { play = Some(0.0); addr = "再生: 開始(Aで停止)".into(); }
                                } else { addr = "再生: ルート未確定".into(); }
                            }
                            KeyCode::Char('G') => { // ライブ現在地(ブレッドクラム)の ON/OFF
                                if gps_rx.is_some() { gps_rx = None; addr = "ライブ現在地: OFF".into(); }
                                else {
                                    let bin = if std::path::Path::new("/opt/homebrew/bin/CoreLocationCLI").exists() { "/opt/homebrew/bin/CoreLocationCLI" } else { "CoreLocationCLI" };
                                    if gpslive::available(bin) { gps_rx = Some(gpslive::start_poller(bin.to_string(), 5)); gps_trail.clear(); gps_pos = None; addr = "ライブ現在地: ON(5秒ごと)".into(); }
                                    else { addr = "ライブ: CoreLocationCLI無し(brew install corelocationcli)".into(); }
                                }
                            }
                            KeyCode::Char('i') => { // 実写(Street View)を中心地点で開く
                                if !streetview::available(&cfg.streetview_api_key) { addr = "実写: APIキー未設定(config.toml [streetview])".into(); }
                                else { addr = "実写取得中…".into();
                                    match streetview::fetch(lat, lon, 0, 640, 480, &cfg.streetview_api_key) {
                                        Ok(img) => { street = Some((img, 0, lat, lon)); addr.clear(); }
                                        Err(e) => addr = format!("実写: {e}"),
                                    }
                                }
                            }
                            KeyCode::Char('n') => { // BRouter の代替ルート候補を巡回
                                if wps.len() >= 2 {
                                    route_alt = (route_alt + 1) % 4;
                                    let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, route_alt);
                                    route_note = nn; route_job = jj;
                                } else { addr = "ルート未確定".into(); }
                            }
                            KeyCode::Char('W') => { // 走りまくり(峠/展望の周回)を生成。連打で別案
                                let dist = a.dist.unwrap_or(40.0);
                                match wander_route((lat, lon), dist, &a.shape) {
                                    Ok(w) => { wps = w; wp_sel = 0; let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = nn; route_job = jj; }
                                    Err(e) => addr = format!("({e})"),
                                }
                            }
                            KeyCode::Char('o') => { // スマホ共有(GoogleマップQR)
                                if wps.len() >= 2 {
                                    let (url, _) = gmaps_url(&wps);
                                    match qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L) {
                                        Ok(c) => qr_view = Some(c.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(false).build()),
                                        Err(_) => addr = "QR生成失敗".into(),
                                    }
                                } else { addr = "ルート未確定".into(); }
                            }
                            KeyCode::Char('x') => { if !wps.is_empty() { let i = wp_sel.min(wps.len() - 1); wps.remove(i); if wp_sel >= wps.len() && wp_sel > 0 { wp_sel -= 1; } { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } } }
                            KeyCode::Char('[') => { if wp_sel > 0 && wp_sel < wps.len() { wps.swap(wp_sel, wp_sel - 1); wp_sel -= 1; { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } } }
                            KeyCode::Char(']') => { if wp_sel + 1 < wps.len() { wps.swap(wp_sel, wp_sel + 1); wp_sel += 1; { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } } }
                            KeyCode::Char('m') => { mode = match mode_label(&mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char('c') => { wps.clear(); wp_sel = 0; { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char('g') => match spec.routes.last() {
                                Some(rt) => addr = match write_gpx("termmap-route.gpx", &rt.pts) { Ok(_) => "GPX保存: termmap-route.gpx".into(), Err(e) => format!("({e})") },
                                None => addr = "ルート未確定".into(),
                            },
                            KeyCode::Char('q') => quit = true, // Esc はサブモードの取消専用(Mapでは終了しない)
                            _ => {}
                        }
                        if quit { break; }
                        let n = (TILE as f64) * 2f64.powi(z as i32);
                        if cx < 0.0 { cx += n; } else if cx >= n { cx -= n; }
                        cy = cy.clamp(0.0, n - 1.0);
                    }
                }
            }
            Some(Event::Paste(s)) => { match &mut focus {
                Focus::Search(buf) | Focus::SaveName(buf) | Focus::NearSearch(buf) | Focus::NewCat(buf) | Focus::RoadSearch(buf) => buf.push_str(&s),
                Focus::SpotName(buf, _) | Focus::SpotRename(buf, _) => buf.push_str(&s),
                Focus::Settings if set_sel == 10 => { cfg.streetview_api_key = s.trim().to_string(); addr = "APIkey設定(sで保存)".into(); }
                _ => {}
            } }
            _ => {}
        }
    }
    let (lat, lon) = pixel_to_deg(cx, cy, z);
    save_state(lat, lon, z, &opts.style, &wps, &mode); // 終了時の位置とルートを --resume 用に保存
    Ok(())
}

fn oneshot(src: RgbImage, a: &Args, ctx: Option<(f64, f64, u32, &OverlaySpec)>) {
    if let Some(path) = &a.png {
        let mut rc = recolor(&src);
        if let Some((cx, cy, z, spec)) = ctx {
            if !spec.is_empty() {
                let (w, h) = rc.dimensions();
                let ov = build_overlay(spec, cx, cy, z, w, h, 1.0, 1.0, w, h);
                composite(&mut rc, &ov);
            }
        }
        if let Err(e) = rc.save(path) { eprintln!("save png {path}: {e}"); std::process::exit(1); }
        eprintln!("wrote {path}");
        return;
    }
    let cols = a.width.unwrap_or_else(|| terminal_size::terminal_size().map(|(w, _)| w.0 as u32).unwrap_or(100));
    let (sw, sh) = src.dimensions();
    let aspect = sh as f64 / sw as f64;
    let rows = ((cols as f64) * aspect / 2.0).round().max(1.0) as u32;
    let (out_w, out_h) = if a.braille || a.edge { (cols * 2, rows * 4) } else { (cols, rows * 2) };
    let resized = image::imageops::resize(&src, out_w, out_h, FilterType::Triangle);
    let ov = ctx.and_then(|(cx, cy, z, spec)| {
        if spec.is_empty() { None }
        else { Some(build_overlay(spec, cx, cy, z, sw, sh, out_w as f64 / sw as f64, out_h as f64 / sh as f64, out_w, out_h)) }
    });
    print!("{}", render(&resized, a, ov.as_ref()).replace("\r\n", "\n"));
}

fn main() {
    let mut a = parse_args();

    // お気に入りルート一覧
    if a.list_routes {
        for n in list_named_routes() { println!("{n}"); }
        return;
    }

    // 画像モード
    if let Some(path) = &a.image {
        match image::open(path) {
            Ok(im) => { oneshot(im.to_rgb8(), &a, None); } // 画像モードは地理原点なし=overlay不可
            Err(e) => { eprintln!("image open {path}: {e}"); std::process::exit(1); }
        }
        return;
    }

    // 中心座標の決定 (--load-route > --here > --resume > --place > --lat/--lon)
    let (lat, lon) = if let Some(name) = a.load_route.clone() {
        match load_named_route(&name) {
            Some((wps, m)) => { let start = wps[0]; a.route = Some(wps); a.route_mode = m; start }
            None => { eprintln!("お気に入りルートが見つかりません: {name}"); std::process::exit(1); }
        }
    } else if a.here {
        match gps_here() { Ok(v) => v, Err(e) => { eprintln!("{e}"); std::process::exit(1); } }
    } else if a.resume && a.place.is_none() && a.lat.is_none() && a.lon.is_none() {
        match load_state() {
            Some((la, lo, z, st)) => {
                a.zoom = z; a.style = st;
                if a.route.is_none() { if let Some((wps, m)) = load_route() { a.route = Some(wps); a.route_mode = m; } }
                (la, lo)
            }
            None => { eprintln!("保存された location がありません (--resume)"); std::process::exit(1); }
        }
    } else if let Some(p) = &a.place {
        match geocode(p, None, "") { Ok(v) => v, Err(e) => { eprintln!("{e}"); std::process::exit(1); } }
    } else if let Some(wps) = a.route.as_ref().filter(|w| !w.is_empty()) {
        wps[0] // --route のみ指定時は始点を中心にする
    } else {
        match (a.lat, a.lon) { (Some(la), Some(lo)) => (la, lo), _ => { eprintln!("need --place \"住所\" or --lat/--lon or --image (or --resume)"); std::process::exit(2); } }
    };
    // 走りまくりモード: 峠/展望を経由する周回(or片道)を生成して a.route に載せる
    if a.wander {
        let origin = a.home.unwrap_or((lat, lon));
        let dist = a.dist.unwrap_or(40.0);
        match wander_route(origin, dist, &a.shape) {
            Ok(w) => a.route = Some(w),
            Err(e) => { eprintln!("wander: {e}"); std::process::exit(1); }
        }
    }
    let (cx, cy) = deg_to_pixel(lat, lon, a.zoom);

    // お気に入りルート保存(--save-route。--route か --load-route が前提)
    if let Some(name) = &a.save_route {
        match &a.route {
            Some(wps) => match save_named_route(name, &a.route_mode, wps) {
                Ok(_) => eprintln!("お気に入り保存: {name} ({}点)", wps.len()),
                Err(e) => eprintln!("保存失敗: {e}"),
            },
            None => eprintln!("--save-route には --route または --load-route が必要"),
        }
    }

    // スマホ共有: Googleマップ経路URL + 端末QR を出して終了
    if a.share {
        match &a.route {
            Some(wps) if wps.len() >= 2 => {
                let (url, dropped) = gmaps_url(wps);
                if dropped > 0 { eprintln!("経由点が多いため末尾寄り {dropped} 点を省略(GoogleマップURLの上限)"); }
                println!("{url}");
                match qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L) {
                    Ok(code) => println!("{}", code.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(true).build()),
                    Err(e) => eprintln!("QR生成失敗: {e}"),
                }
            }
            _ => { eprintln!("--share には --route または --load-route が必要"); std::process::exit(2); }
        }
        return;
    }

    if a.interactive {
        if let Err(e) = interactive(cx, cy, a.zoom, &a) { eprintln!("interactive: {e}"); std::process::exit(1); }
        return;
    }

    let mut spec = build_spec(&a, lat, lon);
    let route_note = match attach_route(&mut spec, &a) { Ok(s) => s, Err(e) => { eprintln!("{e}"); None } };
    let mut cache: Cache = HashMap::new();
    match build_window(cx, cy, a.zoom, a.win_px, a.win_px, &a.style, &mut cache) {
        Ok(src) => {
            save_state(lat, lon, a.zoom, &a.style, a.route.as_deref().unwrap_or(&[]), &a.route_mode);
            oneshot(src, &a, Some((cx, cy, a.zoom, &spec)));
            if a.png.is_none() { // 地図描画時のみ 中心座標+住所 をフッタ表示(stderr)
                let addr = reverse_geocode(lat, lon).unwrap_or_default();
                eprintln!("中心 {lat:.5},{lon:.5}  z{}  {}", a.zoom, addr);
                if let Some(note) = &route_note { eprintln!("{note}"); }
            }
            if let Some(g) = &a.gpx { if route_note.is_some() { eprintln!("GPX: {g}"); } }
        }
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_and_fit() {
        assert_eq!(sanitize_name("a/b:c"), "a_b_c");
        assert_eq!(fit_cells("ab", 5), "ab   ");
        assert!(fit_cells("あ", 4).starts_with("あ"));
    }
}
