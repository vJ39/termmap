// termmap — mapscii 風の端末地図レンダラ
//   引数なし   : 前回終了位置から対話起動(保存が無ければ東京中心)。対話が既定。
//   halfblock (既定): ▀ + truecolor / braille: 点字ドット(--mono でプレーン)
//   --classify : 地物カテゴリ(水域/緑地/幹線道路/線路?/建物)を色分け(ラスタ色からの推定)
//   --place    : 日本語住所などをジオコーディング(Nominatim)して中心に
//   --interactive(-i): 対話モードのエイリアス(対話は既定なので付けても付けなくても同じ)
//   --png PATH : カテゴリ色PNGを書き出す(確認用)  --image PNG : 既存画像を描画

mod fsutil;
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
mod sound;
mod ui;
#[allow(dead_code)]
mod roadtrace; // point_at/sample等は使用、cut_segment等は将来用
// 以下は今後の機能用(おすすめ相談)。まだ未wireのため dead_code 許容。
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod recommend;

use image::{RgbImage, imageops::FilterType};

use geo::*;
use tiles::*;
use render::*;
use route::*;
use poi::*;
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

// Web Mercator の緯度有効域(±)。これを超えると deg_to_pixel の tan/ln が非有限化する。
const WM_LAT: f64 = 85.051_128_78;

// "lat,lon" を厳密にパースする。要素の過不足・範囲外はエラーにする(filter_map で黙って捨てない)。
fn parse_point(raw: &str) -> Result<(f64, f64), String> {
    let mut it = raw.split(',');
    let lat: f64 = it.next().ok_or("緯度がありません")?.trim().parse().map_err(|_| format!("緯度が不正: {raw}"))?;
    let lon: f64 = it.next().ok_or("経度がありません")?.trim().parse().map_err(|_| format!("経度が不正: {raw}"))?;
    if it.next().is_some() { return Err(format!("座標の要素が多すぎます: {raw}")); }
    if !(-WM_LAT..=WM_LAT).contains(&lat) { return Err(format!("緯度がWeb Mercator範囲外(±85.05): {lat}")); }
    if !(-180.0..=180.0).contains(&lon) { return Err(format!("経度が範囲外(±180): {lon}")); }
    Ok((lat, lon))
}

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
                let mut rs = Vec::new();
                for s in v.split(',') {
                    let s = s.trim();
                    if s.is_empty() { continue; }
                    let km: f64 = s.parse().unwrap_or_else(|_| arg_err(&format!("--range の値が不正: {s}")));
                    if km <= 0.0 { arg_err("--range は正の数値CSV (例 10,20,30)"); }
                    rs.push(km);
                }
                if rs.is_empty() { arg_err("--range は正の数値CSV (例 10,20,30)"); }
                a.range = rs;
            }
            "--home" => {
                let v = val!("--home");
                a.home = Some(parse_point(&v).unwrap_or_else(|e| arg_err(&e)));
            }
            "--route" => {
                let v = val!("--route");
                let wps: Vec<(f64, f64)> = v.split(';')
                    .map(|p| parse_point(p).unwrap_or_else(|e| arg_err(&e)))
                    .collect();
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
            "-h" | "--help" => { eprintln!("usage: termmap                    引数なし=前回位置から対話起動(保存が無ければ東京中心)\n       termmap [位置] [options]\n  位置: --place \"住所\" | --lat LAT --lon LON | --here | --resume | --load-route N\n  対話が既定。-i/--interactive はその後方互換エイリアス(付けても付けなくても対話で起動)。\n  非対話(静止出力)になるのは --png OUT / --gpx OUT / --save-route N のみ。\n  options: [--zoom Z] [--style osm|voyager|dark|light] [--braille] [--classify] [--edge] [--mono] [--range KM,..] [--home LAT,LON] [--route \"LAT,LON;LAT,LON\"] [--route-mode surface|highway|short] [--routes] [--share] [--width N] | --image PNG"); std::process::exit(0); }
            _ => arg_err(&format!("unknown arg: {k}")),
        }
    }
    if a.image.is_none() && a.zoom > 20 { arg_err("--zoom は 0..=20 で指定 (OSMタイル有効域)"); }
    if let Some(w) = a.width { if w == 0 || w > 1024 { arg_err("--width は 1..=1024 で指定"); } }
    if let Some(la) = a.lat { if !(-WM_LAT..=WM_LAT).contains(&la) { arg_err("--lat は -85.05..=85.05 (Web Mercator有効域)"); } }
    if let Some(lo) = a.lon { if !(-180.0..=180.0).contains(&lo) { arg_err("--lon は -180..=180"); } }
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
        let _ = fsutil::write_atomic(&p, s.as_bytes(), None);
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
    fsutil::write_atomic(&dir.join(format!("{}.txt", sanitize_name(name))), s.as_bytes(), None).map_err(|e| e.to_string())
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
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0); // 絵文字/全角=2, 半角=1, 結合文字=0
        if w + cw > cells { break; }
        o.push(ch); w += cw;
    }
    while w < cells { o.push(' '); w += 1; }
    o
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
    } else if let Some(p) = &a.place {
        match geocode(p, None, "") { Ok(v) => v, Err(e) => { eprintln!("{e}"); std::process::exit(1); } }
    } else if let (Some(la), Some(lo)) = (a.lat, a.lon) {
        (la, lo)
    } else if let Some(wps) = a.route.as_ref().filter(|w| !w.is_empty()) {
        wps[0] // --route のみ指定時は始点を中心にする
    } else {
        // 位置指定なし: 既定で前回位置をresume。保存が無ければ東京中心で開く(エラーにしない)
        match load_state() {
            Some((la, lo, z, st)) => {
                a.zoom = z; a.style = st;
                if a.route.is_none() { if let Some((wps, m)) = load_route() { a.route = Some(wps); a.route_mode = m; } }
                (la, lo)
            }
            None => (35.681236, 139.767125), // 東京駅付近(初回・保存なし時のフォールバック)
        }
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

    // 既定は対話モード。非対話(静止出力)になるのは --png / --gpx / --save-route のときだけ。
    // (--share/--routes/--image は上流で処理済み。-i は既定なので実質no-op)
    let want_static = a.png.is_some() || a.gpx.is_some() || a.save_route.is_some();
    if !want_static {
        if let Err(e) = ui::interactive(cx, cy, a.zoom, &a) { eprintln!("interactive: {e}"); std::process::exit(1); }
        return;
    }

    let mut spec = build_spec(&a, lat, lon);
    let route_note = match attach_route(&mut spec, &a) { Ok(s) => s, Err(e) => { eprintln!("{e}"); None } };
    let mut cache: Cache = Cache::new();
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

    #[test]
    fn parse_point_strict() {
        assert_eq!(parse_point("35.68,139.76").unwrap(), (35.68, 139.76));
        assert_eq!(parse_point(" 35.0 , 139.0 ").unwrap(), (35.0, 139.0)); // 空白は許容
        assert!(parse_point("35.0,invalid").is_err());   // 経度不正
        assert!(parse_point("壊れた値").is_err());        // 経度欠落
        assert!(parse_point("35,139,extra").is_err());   // 要素過多
        assert!(parse_point("88.0,139.0").is_err());      // 緯度がWeb Mercator範囲外
        assert!(parse_point("35.0,200.0").is_err());      // 経度範囲外
    }
}
