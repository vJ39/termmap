// termmap — mapscii 風の端末地図レンダラ
//   halfblock (既定): ▀ + truecolor / braille: 点字ドット(--mono でプレーン)
//   --classify : 地物カテゴリ(水域/緑地/幹線道路/線路?/建物)を色分け(ラスタ色からの推定)
//   --place    : 日本語住所などをジオコーディング(Nominatim)して中心に
//   --interactive(-i): カーソルキーでパン、+/- でズーム、q で終了
//   --png PATH : カテゴリ色PNGを書き出す(確認用)  --image PNG : 既存画像を描画
use std::collections::HashMap;
use std::io::{Read, Write};
use image::{RgbImage, imageops::FilterType};

const TILE: u32 = 256;
type Cache = HashMap<(u32, i64, i64), RgbImage>;

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
    image: Option<String>,
    png: Option<String>,
}

fn arg_err(msg: &str) -> ! { eprintln!("{msg}"); std::process::exit(2); }

fn parse_args() -> Args {
    let mut a = Args { lat: None, lon: None, place: None, zoom: 14, width: None, win_px: 640,
                       style: "osm".to_string(), braille: false, mono: false, classify: false,
                       edge: false, interactive: false, resume: false, here: false, threshold: None,
                       range: Vec::new(), home: None, route: None, route_mode: "surface".to_string(),
                       gpx: None, load_route: None, save_route: None, list_routes: false, image: None, png: None };
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
            "--threshold" => a.threshold = Some(num!("--threshold")),
            "--image" => a.image = Some(val!("--image")),
            "--png" => a.png = Some(val!("--png")),
            "-h" | "--help" => { eprintln!("usage: termmap (--place \"住所\" | --lat LAT --lon LON | --resume | --here) [--zoom Z] [--style osm|voyager|dark|light] [-i] [--braille] [--classify] [--edge] [--mono] [--range KM,..] [--home LAT,LON] [--route \"LAT,LON;LAT,LON\"] [--route-mode surface|highway|short] [--gpx OUT] [--width N] [--png OUT] | --image PNG"); std::process::exit(0); }
            _ => arg_err(&format!("unknown arg: {k}")),
        }
    }
    if a.image.is_none() && a.zoom > 20 { arg_err("--zoom は 0..=20 で指定 (OSMタイル有効域)"); }
    if let Some(w) = a.width { if w == 0 || w > 1024 { arg_err("--width は 1..=1024 で指定"); } }
    a
}

// ---- 座標変換 (Web Mercator, グローバルピクセル) ----
fn deg_to_pixel(lat: f64, lon: f64, z: u32) -> (f64, f64) {
    let latr = lat.to_radians();
    let n = (TILE as f64) * 2f64.powi(z as i32);
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - (latr.tan() + 1.0 / latr.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}
fn pixel_to_deg(px: f64, py: f64, z: u32) -> (f64, f64) {
    let n = (TILE as f64) * 2f64.powi(z as i32);
    let lon = px / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * py / n)).sinh().atan().to_degrees();
    (lat, lon)
}

// ---- ジオコーディング (Nominatim) ----
fn urlencode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{:02X}", b)),
        }
    }
    o
}
fn json_first(body: &str, key: &str) -> Option<String> {
    let i = body.find(key)? + key.len();
    let rest = &body[i..];
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}
fn geocode(place: &str) -> Result<(f64, f64), String> {
    let url = format!("https://nominatim.openstreetmap.org/search?format=json&limit=1&accept-language=ja&q={}", urlencode(place));
    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .call().map_err(|e| format!("geocode: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    let lat = json_first(&body, "\"lat\":\"").ok_or_else(|| format!("住所が見つかりません: {place}"))?;
    let lon = json_first(&body, "\"lon\":\"").ok_or_else(|| format!("住所が見つかりません: {place}"))?;
    let lat: f64 = lat.parse().map_err(|_| "lat parse失敗".to_string())?;
    let lon: f64 = lon.parse().map_err(|_| "lon parse失敗".to_string())?;
    Ok((lat, lon))
}

// 逆ジオコーディング (Nominatim reverse) → 住所文字列(display_name)
fn reverse_geocode(lat: f64, lon: f64) -> Result<String, String> {
    let url = format!("https://nominatim.openstreetmap.org/reverse?format=json&accept-language=ja&zoom=18&lat={lat}&lon={lon}");
    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .call().map_err(|e| format!("revgeo: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    json_first(&body, "\"display_name\":\"").ok_or_else(|| "住所が取得できません".to_string())
}

// 語で広域検索(Nominatim, 複数ヒット)。(lat,lon,表示名) の列を返す。
fn geocode_list(q: &str, limit: u32) -> Vec<(f64, f64, String)> {
    let url = format!("https://nominatim.openstreetmap.org/search?format=json&limit={limit}&accept-language=ja&q={}", urlencode(q));
    let body = match ureq::get(&url).set("User-Agent", "termmap/0.1 (personal experiment)").call() {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = match body.find('[') { Some(p) => p, None => return out };
    let (mut depth, mut obj_start, mut in_obj, mut in_str, mut esc) = (0i32, 0usize, false, false, false);
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if esc { esc = false; } else if b == b'\\' { esc = true; } else if b == b'"' { in_str = false; }
        } else {
            match b {
                b'"' => in_str = true,
                b'{' => { if depth == 0 { obj_start = i; in_obj = true; } depth += 1; }
                b'}' => { depth -= 1; if depth == 0 && in_obj {
                    let obj = &body[obj_start..=i];
                    let la = json_str(obj, "lat").and_then(|s| s.parse::<f64>().ok());
                    let lo = json_str(obj, "lon").and_then(|s| s.parse::<f64>().ok());
                    if let (Some(la), Some(lo)) = (la, lo) {
                        out.push((la, lo, json_str(obj, "display_name").unwrap_or_default()));
                    }
                    in_obj = false;
                }}
                b']' => { if depth == 0 { break; } }
                _ => {}
            }
        }
        i += 1;
    }
    out
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

// ---- ルーティング (BRouter 公開API) ----
struct RouteResult { pts: Vec<(f64, f64)>, dist_m: f64, time_s: f64, hw_m: f64 }
// short=最短 / highway=高速OK(car-fast) / それ以外=下道(高速回避, moped). 既知名は透過。
fn route_profile(mode: &str) -> &str {
    match mode {
        "short" | "shortest" => "shortest",
        "highway" | "fast" | "高速" => "car-fast",
        "surface" | "下道" | "quiet" | "car" => "moped",
        other => other,
    }
}
fn mode_label(mode: &str) -> &'static str {
    match mode {
        "short" | "shortest" => "最短",
        "highway" | "fast" | "高速" => "高速",
        _ => "下道",
    }
}
// 距離/時間/(高速なら)料金概算の要約。料金=高速区間km×¥30(普通車概算, 割引なし)。
fn route_summary(mode: &str, r: &RouteResult) -> String {
    let mut s = format!("{} {:.1}km {}分", mode_label(mode), r.dist_m / 1000.0, (r.time_s / 60.0).round() as i64);
    if r.hw_m > 50.0 {
        let km = r.hw_m / 1000.0;
        s.push_str(&format!(" 高速{km:.1}km ¥{}概算", (km * 30.0).round() as i64));
    }
    s
}
// BRouter geojson の messages([[headers],[row..]]) を文字列行に分解(値は全て文字列)
fn parse_message_rows(body: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mi = match body.find("\"messages\"") { Some(i) => i, None => return rows };
    let after = &body[mi..];
    let start = match after.find('[') { Some(i) => i, None => return rows };
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    let (mut cur, mut field): (Vec<String>, String) = (Vec::new(), String::new());
    for b in after[start..].chars() {
        if in_str {
            if esc { field.push(b); esc = false; }
            else if b == '\\' { esc = true; }
            else if b == '"' { in_str = false; }
            else { field.push(b); }
        } else {
            match b {
                '"' => in_str = true,
                '[' => { depth += 1; if depth == 2 { cur = Vec::new(); field.clear(); } }
                ',' => { if depth == 2 { cur.push(std::mem::take(&mut field)); } }
                ']' => {
                    if depth == 2 { cur.push(std::mem::take(&mut field)); rows.push(std::mem::take(&mut cur)); }
                    depth -= 1;
                    if depth == 0 { break; }
                }
                _ => {}
            }
        }
    }
    rows
}
// 高速(motorway=有料道)区間の総メートル。料金概算に使う。
fn expressway_meters(body: &str) -> f64 {
    let rows = parse_message_rows(body);
    if rows.is_empty() { return 0.0; }
    let di = rows[0].iter().position(|h| h == "Distance");
    let wi = rows[0].iter().position(|h| h == "WayTags");
    let (di, wi) = match (di, wi) { (Some(d), Some(w)) => (d, w), _ => return 0.0 };
    let mut m = 0.0;
    for r in &rows[1..] {
        if let (Some(d), Some(w)) = (r.get(di), r.get(wi)) {
            if w.contains("highway=motorway") {
                if let Ok(v) = d.parse::<f64>() { m += v; }
            }
        }
    }
    m
}
// geojson の LineString coordinates([[lon,lat,elev],...]) を (lat,lon) 列へ。
// BRouter は整形済み(空白/改行あり)なのでブラケット深さで走査する。
fn parse_geojson_line(body: &str) -> Option<Vec<(f64, f64)>> {
    let ci = body.find("\"coordinates\"")?;
    let after = &body[ci..];
    let open = after.find('[')?; // 外側配列の開始
    let mut depth = 0i32;
    let mut close = None;
    for (i, &b) in after.as_bytes().iter().enumerate().skip(open) {
        match b {
            b'[' => depth += 1,
            b']' => { depth -= 1; if depth == 0 { close = Some(i); break; } }
            _ => {}
        }
    }
    let inner = &after[open + 1..close?]; // 各点 [lon, lat, elev], ...
    let mut pts = Vec::new();
    let mut rest = inner;
    while let Some(o) = rest.find('[') {
        let c = rest[o..].find(']')? + o;
        let mut it = rest[o + 1..c].split(',');
        let lon: f64 = it.next()?.trim().parse().ok()?;
        let lat: f64 = it.next()?.trim().parse().ok()?;
        pts.push((lat, lon));
        rest = &rest[c + 1..];
    }
    if pts.is_empty() { None } else { Some(pts) }
}
// mode: "short"=最短(shortest) / それ以外=裏道(safety)。wps は (lat,lon) 列。
fn fetch_route(wps: &[(f64, f64)], mode: &str) -> Result<RouteResult, String> {
    if wps.len() < 2 { return Err("--route は始点と終点(2点以上)が必要".into()); }
    let profile = route_profile(mode);
    let lonlats = wps.iter().map(|(la, lo)| format!("{lo},{la}")).collect::<Vec<_>>().join("|");
    let url = format!("https://brouter.de/brouter?lonlats={lonlats}&profile={profile}&alternativeidx=0&format=geojson");
    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .call().map_err(|e| format!("route: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    let pts = parse_geojson_line(&body).ok_or("route: geometry parse失敗")?;
    let num = |k: &str| json_first(body.as_str(), k).and_then(|s| s.trim().parse::<f64>().ok());
    let dist_m = num("\"track-length\": \"").or_else(|| num("\"track-length\":\"")).unwrap_or(0.0);
    let time_s = num("\"total-time\": \"").or_else(|| num("\"total-time\":\"")).unwrap_or(0.0);
    let hw_m = expressway_meters(&body);
    Ok(RouteResult { pts, dist_m, time_s, hw_m })
}
fn write_gpx(path: &str, pts: &[(f64, f64)]) -> Result<(), String> {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx version=\"1.1\" creator=\"termmap\" xmlns=\"http://www.topografix.com/GPX/1/1\">\n<trk><name>termmap route</name><trkseg>\n");
    for (la, lo) in pts { s.push_str(&format!("<trkpt lat=\"{la}\" lon=\"{lo}\"></trkpt>\n")); }
    s.push_str("</trkseg></trk>\n</gpx>\n");
    std::fs::write(path, s).map_err(|e| format!("gpx write {path}: {e}"))
}

// タイルスタイル → URL。voyager/dark/light は CartoDB の label-free 系(端末で見やすい)。
fn tile_url(style: &str, z: u32, x: i64, y: i64) -> String {
    match style {
        "voyager" => format!("https://basemaps.cartocdn.com/rastertiles/voyager_nolabels/{z}/{x}/{y}.png"),
        "dark"    => format!("https://basemaps.cartocdn.com/dark_nolabels/{z}/{x}/{y}.png"),
        "light"   => format!("https://basemaps.cartocdn.com/light_nolabels/{z}/{x}/{y}.png"),
        _         => format!("https://tile.openstreetmap.org/{z}/{x}/{y}.png"),
    }
}
fn fetch_tile(style: &str, z: u32, x: i64, y: i64) -> Result<RgbImage, String> {
    let url = tile_url(style, z, x, y);
    let resp = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .call().map_err(|e| format!("fetch tile {z}/{x}/{y}: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(image::load_from_memory(&buf).map_err(|e| format!("decode tile {z}/{x}/{y}: {e}"))?.to_rgb8())
}

// 中心(cx,cy グローバルpx)から win_w×win_h の矩形窓を組み立てる。タイルは cache 経由。
fn build_window(cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32, style: &str, cache: &mut Cache) -> Result<RgbImage, String> {
    let left = cx - win_w as f64 / 2.0;
    let top = cy - win_h as f64 / 2.0;
    let tf = TILE as f64;
    let tx_min = (left / tf).floor() as i64;
    let tx_max = ((left + win_w as f64 - 1.0) / tf).floor() as i64;
    let ty_min = (top / tf).floor() as i64;
    let ty_max = ((top + win_h as f64 - 1.0) / tf).floor() as i64;
    let max_t = 2i64.pow(z);

    // 未キャッシュのタイルを列挙
    let mut missing: Vec<(i64, i64)> = Vec::new();
    for ty in ty_min..=ty_max {
        if ty < 0 || ty >= max_t { continue; }
        for tx in tx_min..=tx_max {
            let wx = ((tx % max_t) + max_t) % max_t;
            if !cache.contains_key(&(z, wx, ty)) { missing.push((wx, ty)); }
        }
    }
    missing.sort_unstable();
    missing.dedup();
    const CONCURRENCY: usize = 8;
    for chunk in missing.chunks(CONCURRENCY) {
        let got: Vec<((i64, i64), Result<RgbImage, String>)> = std::thread::scope(|s| {
            let hs: Vec<_> = chunk.iter().map(|&(wx, ty)| s.spawn(move || ((wx, ty), fetch_tile(style, z, wx, ty)))).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for ((wx, ty), r) in got { cache.insert((z, wx, ty), r?); }
    }

    let cols = (tx_max - tx_min + 1) as u32;
    let rows = (ty_max - ty_min + 1) as u32;
    let bg = if style == "dark" { image::Rgb([26, 26, 26]) } else { image::Rgb([221, 221, 221]) };
    let mut canvas = RgbImage::from_pixel(cols * TILE, rows * TILE, bg);
    for ty in ty_min..=ty_max {
        if ty < 0 || ty >= max_t { continue; }
        for tx in tx_min..=tx_max {
            let wx = ((tx % max_t) + max_t) % max_t;
            if let Some(t) = cache.get(&(z, wx, ty)) {
                let ox = (tx - tx_min) as u32 * TILE;
                let oy = (ty - ty_min) as u32 * TILE;
                for (px, py, p) in t.enumerate_pixels() { canvas.put_pixel(ox + px, oy + py, *p); }
            }
        }
    }
    let crop_x = (left - tx_min as f64 * tf).max(0.0) as u32;
    let crop_y = (top - ty_min as f64 * tf).max(0.0) as u32;
    Ok(image::imageops::crop_imm(&canvas, crop_x, crop_y, win_w, win_h).to_image())
}

fn lum(p: &image::Rgb<u8>) -> f64 { 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64 }

#[derive(Clone, Copy, PartialEq)]
enum Cat { Water, Park, RoadMajor, Rail, Building, Other }
fn classify(p: &image::Rgb<u8>) -> Option<Cat> {
    let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
    let sat = r.max(g).max(b) - r.min(g).min(b);
    let l = lum(p);
    if b - r > 12 && b + 6 > g && b > 150 { return Some(Cat::Water); }
    if g - r > 8 && g - b > 6 { return Some(Cat::Park); }
    if r > 205 && g > 150 && (r - b) > 45 { return Some(Cat::RoadMajor); }
    if l < 115.0 && sat < 45 { return Some(Cat::Rail); }
    if sat > 6 && sat < 42 && r >= g && g >= b && l > 170.0 && l < 226.0 { return Some(Cat::Building); }
    if l > 233.0 { return None; }
    if sat < 14 { return Some(Cat::Other); }
    None
}
fn cat_color(c: Cat) -> (u8, u8, u8) {
    match c {
        Cat::Water => (86, 170, 222), Cat::Park => (110, 190, 110),
        Cat::RoadMajor => (240, 200, 70), Cat::Rail => (180, 95, 200),
        Cat::Building => (200, 172, 148), Cat::Other => (150, 150, 150),
    }
}
fn recolor(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    let mut out = RgbImage::from_pixel(w, h, image::Rgb([245, 245, 245]));
    for (x, y, p) in img.enumerate_pixels() {
        if let Some(c) = classify(p) { let (r, g, b) = cat_color(c); out.put_pixel(x, y, image::Rgb([r, g, b])); }
    }
    out
}

fn render_halfblock(img: &RgbImage) -> String {
    let (w, h) = img.dimensions();
    let mut out = String::with_capacity(w as usize * h as usize * 20);
    let mut y = 0;
    while y + 1 < h {
        for x in 0..w {
            let t = img.get_pixel(x, y);
            let b = img.get_pixel(x, y + 1);
            out.push_str(&format!("\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m\u{2580}", t[0], t[1], t[2], b[0], b[1], b[2]));
        }
        out.push_str("\x1b[0m\r\n");
        y += 2;
    }
    out
}
fn render_braille(img: &RgbImage, mono: bool, classify_on: bool, threshold: u8, edge: bool, ov: Option<&OverlayLayer>) -> String {
    const BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    let (w, h) = img.dimensions();
    let (cols, rows) = (w / 2, h / 4);
    let th = threshold as f64;
    // エッジ検出: 隣接画素の色差(RGB各chの絶対差の和)。明るさが近くても色が違う境界(水際/緑地/道路)を拾う。
    let grad = |x: u32, y: u32| -> f64 {
        if x == 0 || y == 0 || x + 1 >= w || y + 1 >= h { return 0.0; }
        let d = |p: &image::Rgb<u8>, q: &image::Rgb<u8>| {
            (p[0] as f64 - q[0] as f64).abs() + (p[1] as f64 - q[1] as f64).abs() + (p[2] as f64 - q[2] as f64).abs()
        };
        d(img.get_pixel(x + 1, y), img.get_pixel(x - 1, y)) + d(img.get_pixel(x, y + 1), img.get_pixel(x, y - 1))
    };
    let mut out = String::with_capacity(cols as usize * rows as usize * 6);
    for cy in 0..rows {
        for cx in 0..cols {
            let mut bits: u8 = 0;
            let (mut sr, mut sg, mut sb, mut n) = (0u32, 0u32, 0u32, 0u32);
            let mut cc = [0u32; 6];
            let (mut ovr, mut ovg, mut ovb, mut ovn) = (0u32, 0u32, 0u32, 0u32);
            for dx in 0..2u32 {
                for dy in 0..4u32 {
                    let (gx, gy) = (cx * 2 + dx, cy * 4 + dy);
                    let p = img.get_pixel(gx, gy);
                    let ovpix = ov.and_then(|o| o.get(gx, gy));
                    let on = ovpix.is_some()
                             || if edge { grad(gx, gy) > th }
                                else if classify_on { classify(p).is_some() }
                                else { lum(p) < th };
                    if on {
                        bits |= BITS[dx as usize][dy as usize];
                        if let Some(c) = ovpix { ovr += c[0] as u32; ovg += c[1] as u32; ovb += c[2] as u32; ovn += 1; }
                        else {
                            sr += p[0] as u32; sg += p[1] as u32; sb += p[2] as u32; n += 1;
                            if classify_on { if let Some(c) = classify(p) { cc[c as usize] += 1; } }
                        }
                    }
                }
            }
            let ch = char::from_u32(0x2800 + bits as u32).unwrap();
            if bits == 0 { out.push(' '); }
            else if mono { out.push(ch); }
            else if ovn > 0 { out.push_str(&format!("\x1b[38;2;{};{};{}m{ch}", ovr / ovn, ovg / ovn, ovb / ovn)); }
            else if classify_on {
                let bi = (0..6).max_by_key(|&i| cc[i]).unwrap();
                let (r, g, b) = cat_color([Cat::Water, Cat::Park, Cat::RoadMajor, Cat::Rail, Cat::Building, Cat::Other][bi]);
                out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{ch}"));
            } else {
                // braille はインク=暗い画素の平均色になりがちで沈むので輝度を持ち上げる
                let br = |s: u32| ((s as f64 / n as f64) * 1.6).min(255.0) as u8;
                out.push_str(&format!("\x1b[38;2;{};{};{}m{ch}", br(sr), br(sg), br(sb)));
            }
        }
        if !mono { out.push_str("\x1b[0m"); }
        out.push_str("\r\n");
    }
    out
}

// ---- オーバーレイ (POIマーカー / 経路 / 航続リング) ----
#[derive(Clone, Copy)]
#[allow(dead_code)] // POI 実装(次増分)で全variant使用
enum PoiCat { Home, Food, Fuel, Shop, Danger, Waypoint, Other }
fn poi_color(c: PoiCat) -> [u8; 3] {
    match c {
        PoiCat::Home => [255, 64, 64], PoiCat::Food => [255, 140, 0],
        PoiCat::Fuel => [255, 215, 0], PoiCat::Shop => [80, 200, 255],
        PoiCat::Danger => [255, 0, 200], PoiCat::Waypoint => [120, 255, 120],
        PoiCat::Other => [255, 255, 255],
    }
}
#[allow(dead_code)] // POI 実装(次増分)で使用
struct Poi { lat: f64, lon: f64, cat: PoiCat }
struct Route { pts: Vec<(f64, f64)>, color: [u8; 3], thickness: u32 }
struct Ring { lat: f64, lon: f64, radii_km: Vec<f64>, color: [u8; 3], thickness: u32 }
struct OverlaySpec { pois: Vec<Poi>, routes: Vec<Route>, rings: Vec<Ring> }
impl OverlaySpec {
    fn is_empty(&self) -> bool { self.pois.is_empty() && self.routes.is_empty() && self.rings.is_empty() }
}

// 緯度latズームzでの m/px (Web Mercator)
fn meters_per_pixel(lat: f64, z: u32) -> f64 {
    156543.033_92 * lat.to_radians().cos() / 2f64.powi(z as i32)
}

// インクマスク層。描画は最終出力寸法(resize後)で構築する。
struct OverlayLayer { w: u32, h: u32, ink: Vec<Option<[u8; 3]>> }
impl OverlayLayer {
    fn new(w: u32, h: u32) -> Self { Self { w, h, ink: vec![None; (w as usize) * (h as usize)] } }
    fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h { return; }
        self.ink[(y as usize) * (self.w as usize) + x as usize] = Some(c);
    }
    fn get(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.w || y >= self.h { return None; }
        self.ink[(y as usize) * (self.w as usize) + x as usize]
    }
}
fn draw_marker(ov: &mut OverlayLayer, ix: i32, iy: i32, color: [u8; 3], size: i32) {
    let half = size / 2;
    for dy in -half - 1..=half + 1 { for dx in -half - 1..=half + 1 {
        if dx.abs() > half || dy.abs() > half { ov.put(ix + dx, iy + dy, [20, 20, 20]); } // ハロー
    }}
    for dy in -half..=half { for dx in -half..=half { ov.put(ix + dx, iy + dy, color); }}
}
fn draw_line(ov: &mut OverlayLayer, mut x0: i32, mut y0: i32, x1: i32, y1: i32, color: [u8; 3], thickness: u32) {
    let dx = (x1 - x0).abs(); let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs(); let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let t = thickness.max(1) as i32 - 1;
    loop {
        for oy in 0..=t { for ox in 0..=t { ov.put(x0 + ox, y0 + oy, color); }}
        if x0 == x1 && y0 == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x0 += sx; }
        if e2 <= dx { err += dx; y0 += sy; }
    }
}
fn draw_polyline(ov: &mut OverlayLayer, pts: &[(i32, i32)], color: [u8; 3], thickness: u32) {
    for w in pts.windows(2) { draw_line(ov, w[0].0, w[0].1, w[1].0, w[1].1, color, thickness); }
}
fn draw_ring(ov: &mut OverlayLayer, cx: i32, cy: i32, radius: i32, color: [u8; 3], thickness: u32) {
    if radius <= 0 { return; }
    for rr in radius..radius + thickness.max(1) as i32 {
        let (mut x, mut y, mut err) = (rr, 0i32, 1 - rr);
        while x >= y {
            for (px, py) in [(x, y), (y, x), (-x, y), (-y, x), (x, -y), (y, -x), (-x, -y), (-y, -x)] {
                ov.put(cx + px, cy + py, color);
            }
            y += 1;
            if err < 0 { err += 2 * y + 1; } else { x -= 1; err += 2 * (y - x) + 1; }
        }
    }
}
// spec(緯度経度) を 表示画像座標へ射影して焼く。win_w/h=元画像寸法, scale=resize比, out_w/h=最終寸法。
fn build_overlay(spec: &OverlaySpec, cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32,
                 scale_x: f64, scale_y: f64, out_w: u32, out_h: u32) -> OverlayLayer {
    let mut ov = OverlayLayer::new(out_w, out_h);
    let left = cx - win_w as f64 / 2.0;
    let top = cy - win_h as f64 / 2.0;
    let to_img = |lat: f64, lon: f64| -> (i32, i32) {
        let (gx, gy) = deg_to_pixel(lat, lon, z);
        (((gx - left) * scale_x).floor() as i32, ((gy - top) * scale_y).floor() as i32)
    };
    for r in &spec.rings { // リング(最背面)
        let (rx, ry) = to_img(r.lat, r.lon);
        let mpp = meters_per_pixel(r.lat, z);
        for km in &r.radii_km {
            let rpx = ((km * 1000.0 / mpp) * scale_x).round() as i32;
            draw_ring(&mut ov, rx, ry, rpx, r.color, r.thickness);
        }
    }
    for rt in &spec.routes { // 経路
        let pts: Vec<(i32, i32)> = rt.pts.iter().map(|&(la, lo)| to_img(la, lo)).collect();
        draw_polyline(&mut ov, &pts, rt.color, rt.thickness);
    }
    for p in &spec.pois { // マーカー(最前面)
        let (ix, iy) = to_img(p.lat, p.lon);
        if ix < -4 || iy < -4 || ix > out_w as i32 + 4 || iy > out_h as i32 + 4 { continue; }
        draw_marker(&mut ov, ix, iy, poi_color(p.cat), 3);
    }
    ov
}
fn composite(img: &mut RgbImage, ov: &OverlayLayer) {
    let (w, h) = img.dimensions();
    for y in 0..h.min(ov.h) { for x in 0..w.min(ov.w) {
        if let Some(c) = ov.get(x, y) { img.put_pixel(x, y, image::Rgb(c)); }
    }}
}
// CLI フラグから OverlaySpec を組む。center_* はリング等の基準(--home 指定時はそちら優先)。
fn build_spec(a: &Args, center_lat: f64, center_lon: f64) -> OverlaySpec {
    let mut rings = Vec::new();
    if !a.range.is_empty() {
        let (rl, ro) = a.home.unwrap_or((center_lat, center_lon));
        rings.push(Ring { lat: rl, lon: ro, radii_km: a.range.clone(), color: [255, 90, 90], thickness: 2 });
    }
    OverlaySpec { pois: Vec::new(), routes: Vec::new(), rings }
}
// --route があれば BRouter で取得して spec に追加し、要約(距離/時間)を返す。--gpx 指定時は書き出し。
fn attach_route(spec: &mut OverlaySpec, a: &Args) -> Result<Option<String>, String> {
    let wps = match &a.route { Some(w) => w, None => return Ok(None) };
    let r = fetch_route(wps, &a.route_mode)?;
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

// ---- 目的地検索 (Overpass) ----
struct PoiKind { key: char, label: &'static str, filter: &'static str, cat: PoiCat }
const POI_KINDS: [PoiKind; 7] = [
    PoiKind { key: '1', label: "ガソスタ", filter: "nwr[\"amenity\"=\"fuel\"]", cat: PoiCat::Fuel },
    PoiKind { key: '2', label: "カフェ", filter: "nwr[\"amenity\"=\"cafe\"]", cat: PoiCat::Food },
    PoiKind { key: '3', label: "コンビニ", filter: "nwr[\"shop\"=\"convenience\"]", cat: PoiCat::Shop },
    PoiKind { key: '4', label: "道の駅", filter: "nwr[\"name\"~\"道の駅\"][\"highway\"!~\"traffic_signals|bus_stop\"]", cat: PoiCat::Waypoint },
    PoiKind { key: '5', label: "展望", filter: "nwr[\"tourism\"=\"viewpoint\"]", cat: PoiCat::Other },
    PoiKind { key: '6', label: "公園", filter: "nwr[\"leisure\"=\"park\"]", cat: PoiCat::Other },
    PoiKind { key: '7', label: "峠道", filter: "nwr[\"mountain_pass\"=\"yes\"]", cat: PoiCat::Danger },
];
// 文字列フィールド抽出。key は裸のキー名("name" 等)。コロン後の空白を許容。
fn json_str(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = s.find(&pat)? + pat.len();
    let rest = &s[i..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q1 = after.find('"')?;
    let q2 = after[q1 + 1..].find('"')?;
    Some(after[q1 + 1..q1 + 1 + q2].to_string())
}
// 数値フィールド抽出("lat": 35.7 のような)
fn json_num(s: &str, key: &str) -> Option<f64> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}
// Overpass out:json の elements から (lat,lon,name) を取り出す。文字列内の括弧を無視する走査。
fn parse_overpass(body: &str) -> Vec<(f64, f64, String)> {
    let mut out = Vec::new();
    let ei = match body.find("\"elements\"") { Some(i) => i, None => return out };
    let bytes = body.as_bytes();
    let mut i = ei;
    while i < bytes.len() && bytes[i] != b'[' { i += 1; }
    let (mut depth, mut obj_start, mut in_obj, mut in_str, mut esc) = (0i32, 0usize, false, false, false);
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if esc { esc = false; } else if b == b'\\' { esc = true; } else if b == b'"' { in_str = false; }
        } else {
            match b {
                b'"' => in_str = true,
                b'{' => { if depth == 0 { obj_start = i; in_obj = true; } depth += 1; }
                b'}' => { depth -= 1; if depth == 0 && in_obj {
                    let obj = &body[obj_start..=i];
                    if let (Some(la), Some(lo)) = (json_num(obj, "\"lat\":"), json_num(obj, "\"lon\":")) {
                        out.push((la, lo, json_str(obj, "name").unwrap_or_default()));
                    }
                    in_obj = false;
                }}
                b']' => { if depth == 0 { break; } }
                _ => {}
            }
        }
        i += 1;
    }
    out
}
// 表示bbox(south,west,north,east)で kind を検索
fn fetch_pois(kind: &PoiKind, s: f64, w: f64, n: f64, e: f64) -> Result<Vec<(f64, f64, String)>, String> {
    let q = format!("[out:json][timeout:25];({}({:.5},{:.5},{:.5},{:.5}););out center;", kind.filter, s, w, n, e);
    let url = format!("https://overpass-api.de/api/interpreter?data={}", urlencode(&q));
    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .set("Accept", "application/json")
        .call().map_err(|e| format!("overpass: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    Ok(parse_overpass(&body))
}
fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let r = 6371.0;
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let (dlat, dlon) = ((b.0 - a.0).to_radians(), (b.1 - a.1).to_radians());
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
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
// waypoints/pois/mode から spec の pois/routes を作り直し、ルート要約を返す(rings は保持)。
fn recompute(spec: &mut OverlaySpec, wps: &[(f64, f64)], pois: &[(f64, f64, String, PoiCat)], mode: &str) -> Option<String> {
    spec.pois.clear();
    for (la, lo, _, cat) in pois { spec.pois.push(Poi { lat: *la, lon: *lo, cat: *cat }); }
    let n = wps.len();
    for (idx, (la, lo)) in wps.iter().enumerate() {
        let cat = if idx == 0 { PoiCat::Waypoint } else if idx == n - 1 { PoiCat::Home } else { PoiCat::Food };
        spec.pois.push(Poi { lat: *la, lon: *lo, cat });
    }
    spec.routes.clear();
    if n >= 2 {
        match fetch_route(wps, mode) {
            Ok(r) => {
                let note = route_summary(mode, &r);
                spec.routes.push(Route { pts: r.pts, color: [0, 220, 255], thickness: 2 });
                Some(note)
            }
            Err(e) => Some(format!("({e})")),
        }
    } else { None }
}

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
    enum Focus { Map, Search(String), SaveName(String), PoiMenu, PoiList, RouteList }
    let _guard = TermGuard::enter()?; // Drop で必ず端末復元
    let mut cache: Cache = HashMap::new();
    let mut out = std::io::stdout();
    let mut addr = String::new();          // 'a' 住所 / 一時メッセージ
    let mut focus = Focus::Map;

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
    let mut route_note = recompute(&mut spec, &wps, &pois, &mode);

    let _ = write!(out, "\x1b[2J");
    loop {
        let (tc, tr) = crossterm::terminal::size().unwrap_or((100, 40));
        let cols = tc.max(20) as u32;
        let map_rows = (tr.max(3) - 1) as u32;
        let show_routes = matches!(focus, Focus::RouteList);
        let gut: u32 = if !pois.is_empty() || show_routes { 26 } else { 0 };
        let map_cols = cols.saturating_sub(gut).max(10);
        let (ow, oh) = if a.braille || a.edge { (map_cols * 2, map_rows * 4) } else { (map_cols, map_rows * 2) };
        let (lat, lon) = pixel_to_deg(cx, cy, z);

        let body = match build_window(cx, cy, z, ow, oh, &a.style, &mut cache) {
            Ok(img) => {
                let mut ov = build_overlay(&spec, cx, cy, z, ow, oh, 1.0, 1.0, ow, oh);
                let (mx, my) = (ow as i32 / 2, oh as i32 / 2); // 中心クロスヘア(黄)
                draw_line(&mut ov, mx - 6, my, mx + 6, my, [255, 255, 0], 1);
                draw_line(&mut ov, mx, my - 6, mx, my + 6, [255, 255, 0], 1);
                render(&img, a, Some(&ov))
            }
            Err(e) => format!("取得失敗: {e}\r\n"),
        };

        // 左袖リスト(POI か お気に入り)の各行を組む
        let glines: Vec<String> = if gut > 0 {
            let gw = gut as usize;
            let (header, items, sel): (String, Vec<String>, usize) = if show_routes {
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
        let status = match &focus {
            Focus::Search(buf) => format!(" 検索(語): {buf}\u{2588}   Enter=検索 Esc=取消 "),
            Focus::SaveName(buf) => format!(" ルート名: {buf}\u{2588}   Enter=保存 Esc=取消 "),
            Focus::PoiMenu => " 目的地: 1ガソスタ 2カフェ 3コンビニ 4道の駅 5展望 6公園 7峠道   Esc=取消 ".to_string(),
            Focus::PoiList => format!(" [{}] ↑↓選択 Enter=経由に追加 e=終点 f=再検索 Esc=閉 ", poi_label),
            Focus::RouteList => " お気に入り: ↑↓選択 Enter=読込 Esc=閉 ".to_string(),
            Focus::Map => {
                let base = format!(" z{z} {lat:.4},{lon:.4} | s始 e終 v経由({}) Tab#{} []並替 x消 m:{} f目的地 S保存 L呼出 gGPX /検索 a住所 q",
                    wps.len(), if wps.is_empty() { 0 } else { wp_sel + 1 }, mode_label(&mode));
                match &route_note { Some(rn) => format!("{base} | {rn} "), None => base }
            }
        };
        let status = fit_cells(&status, cols as usize);
        write!(out, "\x1b[{};1H\x1b[7m{status}\x1b[0m", tr)?;
        out.flush()?;

        match event::read()? {
            Event::Key(k) => {
                let cur = std::mem::replace(&mut focus, Focus::Map);
                match cur {
                    Focus::Search(mut buf) => match k.code {
                        KeyCode::Enter => {
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                let results = geocode_list(&q, 20);
                                if results.is_empty() { addr = format!("見つからない: {q}"); }
                                else {
                                    let (la, lo) = (results[0].0, results[0].1);
                                    let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; addr.clear();
                                    pois = results.into_iter().map(|(a, b, nm)| (a, b, nm, PoiCat::Other)).collect();
                                    poi_sel = 0; poi_label = format!("検索:{q}");
                                    route_note = recompute(&mut spec, &wps, &pois, &mode);
                                    focus = Focus::PoiList; // 候補一覧から経由/終点に追加できる
                                }
                            }
                        }
                        KeyCode::Esc => {}
                        KeyCode::Backspace => { buf.pop(); focus = Focus::Search(buf); }
                        KeyCode::Char(c) => { buf.push(c); focus = Focus::Search(buf); }
                        _ => focus = Focus::Search(buf),
                    },
                    Focus::PoiMenu => match k.code {
                        KeyCode::Esc => {}
                        KeyCode::Char(c) => {
                            if let Some(kind) = POI_KINDS.iter().find(|kk| kk.key == c) {
                                let (hw, hh) = (ow as f64 * 1.25, oh as f64 * 1.25); // 表示範囲の2.5倍を検索
                                let (lat_top, lon_left) = pixel_to_deg(cx - hw, cy - hh, z);
                                let (lat_bot, lon_right) = pixel_to_deg(cx + hw, cy + hh, z);
                                match fetch_pois(kind, lat_bot, lon_left, lat_top, lon_right) {
                                    Ok(v) => {
                                        let mut items: Vec<(f64, f64, String, PoiCat)> = v.into_iter().map(|(la, lo, nm)| (la, lo, nm, kind.cat)).collect();
                                        items.sort_by(|p, q| p.2.cmp(&q.2));
                                        items.dedup_by(|p, q| !p.2.is_empty() && p.2 == q.2);
                                        items.sort_by(|p, q| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (q.0, q.1))).unwrap_or(std::cmp::Ordering::Equal));
                                        pois = items; poi_sel = 0; poi_label = kind.label.to_string();
                                        route_note = recompute(&mut spec, &wps, &pois, &mode);
                                        focus = Focus::PoiList;
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
                        KeyCode::Enter => {
                            if let Some(p) = pois.get(poi_sel) {
                                if wps.len() < 2 { wps.push((p.0, p.1)); } else { wps.insert(wps.len() - 1, (p.0, p.1)); }
                                route_note = recompute(&mut spec, &wps, &pois, &mode);
                            }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('e') => {
                            if let Some(p) = pois.get(poi_sel) {
                                let pt = (p.0, p.1);
                                if wps.len() >= 2 { let l = wps.len() - 1; wps[l] = pt; } else { wps.push(pt); }
                                route_note = recompute(&mut spec, &wps, &pois, &mode);
                            }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('p') => focus = Focus::PoiMenu,
                        KeyCode::Esc => { pois.clear(); route_note = recompute(&mut spec, &wps, &pois, &mode); }
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
                                    route_note = recompute(&mut spec, &wps, &pois, &mode);
                                }
                            }
                        }
                        KeyCode::Esc => {}
                        _ => focus = Focus::RouteList,
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
                            KeyCode::Char('a') => addr = reverse_geocode(lat, lon).unwrap_or_else(|e| format!("({e})")),
                            KeyCode::Char('/') => focus = Focus::Search(String::new()),
                            KeyCode::Char('f') | KeyCode::Char('p') => focus = Focus::PoiMenu,
                            KeyCode::Char('S') => focus = Focus::SaveName(String::new()),
                            KeyCode::Char('L') => { route_names = list_named_routes(); rn_sel = 0; if route_names.is_empty() { addr = "お気に入り無し".into(); } else { focus = Focus::RouteList; } }
                            KeyCode::Char('s') => { if wps.is_empty() { wps.push((lat, lon)); } else { wps[0] = (lat, lon); } route_note = recompute(&mut spec, &wps, &pois, &mode); }
                            KeyCode::Char('e') => { if wps.len() >= 2 { let l = wps.len() - 1; wps[l] = (lat, lon); } else { wps.push((lat, lon)); } route_note = recompute(&mut spec, &wps, &pois, &mode); }
                            KeyCode::Char('v') => { if wps.len() < 2 { wps.push((lat, lon)); } else { wps.insert(wps.len() - 1, (lat, lon)); } route_note = recompute(&mut spec, &wps, &pois, &mode); }
                            KeyCode::Tab => { if !wps.is_empty() { wp_sel = (wp_sel + 1) % wps.len(); } }
                            KeyCode::Char('x') => { if !wps.is_empty() { let i = wp_sel.min(wps.len() - 1); wps.remove(i); if wp_sel >= wps.len() && wp_sel > 0 { wp_sel -= 1; } route_note = recompute(&mut spec, &wps, &pois, &mode); } }
                            KeyCode::Char('[') => { if wp_sel > 0 && wp_sel < wps.len() { wps.swap(wp_sel, wp_sel - 1); wp_sel -= 1; route_note = recompute(&mut spec, &wps, &pois, &mode); } }
                            KeyCode::Char(']') => { if wp_sel + 1 < wps.len() { wps.swap(wp_sel, wp_sel + 1); wp_sel += 1; route_note = recompute(&mut spec, &wps, &pois, &mode); } }
                            KeyCode::Char('m') => { mode = match mode_label(&mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); route_note = recompute(&mut spec, &wps, &pois, &mode); }
                            KeyCode::Char('c') => { wps.clear(); wp_sel = 0; route_note = recompute(&mut spec, &wps, &pois, &mode); }
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
            Event::Paste(s) => { match &mut focus { Focus::Search(buf) | Focus::SaveName(buf) => buf.push_str(&s), _ => {} } }
            _ => {}
        }
    }
    let (lat, lon) = pixel_to_deg(cx, cy, z);
    save_state(lat, lon, z, &a.style, &wps, &mode); // 終了時の位置とルートを --resume 用に保存
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
        match geocode(p) { Ok(v) => v, Err(e) => { eprintln!("{e}"); std::process::exit(1); } }
    } else if let Some(wps) = a.route.as_ref().filter(|w| !w.is_empty()) {
        wps[0] // --route のみ指定時は始点を中心にする
    } else {
        match (a.lat, a.lon) { (Some(la), Some(lo)) => (la, lo), _ => { eprintln!("need --place \"住所\" or --lat/--lon or --image (or --resume)"); std::process::exit(2); } }
    };
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
