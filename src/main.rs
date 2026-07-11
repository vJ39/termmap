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
    threshold: Option<u8>,
    image: Option<String>,
    png: Option<String>,
}

fn arg_err(msg: &str) -> ! { eprintln!("{msg}"); std::process::exit(2); }

fn parse_args() -> Args {
    let mut a = Args { lat: None, lon: None, place: None, zoom: 14, width: None, win_px: 640,
                       style: "osm".to_string(), braille: false, mono: false, classify: false,
                       edge: false, interactive: false, resume: false, threshold: None, image: None, png: None };
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
            "--win" => a.win_px = num!("--win"),
            "--style" => a.style = val!("--style"),
            "--braille" => a.braille = true,
            "--mono" => a.mono = true,
            "--classify" => a.classify = true,
            "--edge" => a.edge = true,
            "-i" | "--interactive" => a.interactive = true,
            "--resume" | "--last" => a.resume = true,
            "--threshold" => a.threshold = Some(num!("--threshold")),
            "--image" => a.image = Some(val!("--image")),
            "--png" => a.png = Some(val!("--png")),
            "-h" | "--help" => { eprintln!("usage: termmap (--place \"住所\" | --lat LAT --lon LON | --resume) [--zoom Z] [--style osm|voyager|dark|light] [-i] [--braille] [--classify] [--edge] [--mono] [--width N] [--png OUT] | --image PNG"); std::process::exit(0); }
            _ => arg_err(&format!("unknown arg: {k}")),
        }
    }
    if a.image.is_none() && a.zoom > 20 { arg_err("--zoom は 0..=20 で指定 (OSMタイル有効域)"); }
    if a.win_px == 0 || a.win_px > 2048 { arg_err("--win は 1..=2048 で指定"); }
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
fn render_braille(img: &RgbImage, mono: bool, classify_on: bool, threshold: u8, edge: bool) -> String {
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
            for dx in 0..2u32 {
                for dy in 0..4u32 {
                    let p = img.get_pixel(cx * 2 + dx, cy * 4 + dy);
                    let on = if edge { grad(cx * 2 + dx, cy * 4 + dy) > th }
                             else if classify_on { classify(p).is_some() }
                             else { lum(p) < th };
                    if on {
                        bits |= BITS[dx as usize][dy as usize];
                        sr += p[0] as u32; sg += p[1] as u32; sb += p[2] as u32; n += 1;
                        if classify_on { if let Some(c) = classify(p) { cc[c as usize] += 1; } }
                    }
                }
            }
            let ch = char::from_u32(0x2800 + bits as u32).unwrap();
            if bits == 0 { out.push(' '); }
            else if mono { out.push(ch); }
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

fn render(img: &RgbImage, a: &Args) -> String {
    let th = a.threshold.unwrap_or(if a.edge { 45 } else { 195 });
    if a.edge { render_braille(img, a.mono, false, th, true) }
    else if a.braille { render_braille(img, a.mono, a.classify, th, false) }
    else if a.classify { render_halfblock(&recolor(img)) }
    else { render_halfblock(img) }
}

// ---- 直近 location の保存/復元 (--resume) ----
fn state_file() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home).join(".config/termmap/last.txt"))
}
fn save_state(lat: f64, lon: f64, z: u32, style: &str) {
    if let Some(p) = state_file() {
        if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
        let _ = std::fs::write(&p, format!("{lat} {lon} {z} {style}\n"));
    }
}
fn load_state() -> Option<(f64, f64, u32, String)> {
    let s = std::fs::read_to_string(state_file()?).ok()?;
    let mut it = s.split_whitespace();
    let lat = it.next()?.parse().ok()?;
    let lon = it.next()?.parse().ok()?;
    let z = it.next()?.parse().ok()?;
    let style = it.next().unwrap_or("osm").to_string();
    Some((lat, lon, z, style))
}

// ---- 対話モード (crossterm) ----
// 端末状態を RAII で復元する。パニック/早期return でも Drop で raw mode と代替スクリーンを必ず戻す。
struct TermGuard;
impl TermGuard {
    fn enter() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen, crossterm::cursor::Hide)?;
        Ok(Self)
    }
}
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show, crossterm::terminal::LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn interactive(mut cx: f64, mut cy: f64, mut z: u32, a: &Args) -> std::io::Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    let _guard = TermGuard::enter()?; // Drop で必ず端末復元
    let mut cache: Cache = HashMap::new();
    let mut out = std::io::stdout();
    let mut addr = String::new(); // 'a' で現在地の住所を取得(パン/ズームで無効化)
    loop {
        let (tc, tr) = crossterm::terminal::size().unwrap_or((100, 40));
        let cols = tc.max(10) as u32;
        let map_rows = (tr.max(3) - 1) as u32;
        let (ow, oh) = if a.braille || a.edge { (cols * 2, map_rows * 4) } else { (cols, map_rows * 2) };
        let body = match build_window(cx, cy, z, ow, oh, &a.style, &mut cache) {
            Ok(img) => render(&img, a),
            Err(e) => format!("取得失敗: {e}\r\n"),
        };
        let (lat, lon) = pixel_to_deg(cx, cy, z);
        let status = if addr.is_empty() {
            format!(" z{z}  {lat:.5},{lon:.5}   ←↑↓→=pan Shift=大 +/-=zoom a=住所 q=quit ")
        } else {
            format!(" z{z}  {lat:.5},{lon:.5}  {addr}   (a=更新 q=quit) ")
        };
        let status: String = status.chars().take(cols as usize).collect();
        write!(out, "\x1b[H{body}\x1b[{tr};1H\x1b[7m{status}\x1b[0m")?;
        out.flush()?;
        match event::read()? {
            Event::Key(k) => {
                // 通常=細かく(window/12), Shift併用=大きく(window/3)
                let frac = if k.modifiers.contains(KeyModifiers::SHIFT) { 3.0 } else { 12.0 };
                let (dx, dy) = ((ow as f64 / frac).max(1.0), (oh as f64 / frac).max(1.0));
                match k.code {
                    KeyCode::Left => { cx -= dx; addr.clear(); }
                    KeyCode::Right => { cx += dx; addr.clear(); }
                    KeyCode::Up => { cy -= dy; addr.clear(); }
                    KeyCode::Down => { cy += dy; addr.clear(); }
                    KeyCode::Char('+') | KeyCode::Char('=') => if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; addr.clear(); },
                    KeyCode::Char('-') | KeyCode::Char('_') => if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; addr.clear(); },
                    KeyCode::Char('a') => { addr = reverse_geocode(lat, lon).unwrap_or_else(|e| format!("({e})")); }
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    _ => {}
                }
                let n = (TILE as f64) * 2f64.powi(z as i32);
                if cx < 0.0 { cx += n; } else if cx >= n { cx -= n; }
                cy = cy.clamp(0.0, n - 1.0);
            }
            Event::Resize(..) => {}
            _ => {}
        }
    }
    let (lat, lon) = pixel_to_deg(cx, cy, z);
    save_state(lat, lon, z, &a.style); // 終了時の位置を --resume 用に保存
    Ok(())
}

fn oneshot(src: RgbImage, a: &Args) {
    if let Some(path) = &a.png {
        if let Err(e) = recolor(&src).save(path) { eprintln!("save png {path}: {e}"); std::process::exit(1); }
        eprintln!("wrote {path}");
        return;
    }
    let cols = a.width.unwrap_or_else(|| terminal_size::terminal_size().map(|(w, _)| w.0 as u32).unwrap_or(100));
    let (sw, sh) = src.dimensions();
    let aspect = sh as f64 / sw as f64;
    let rows = ((cols as f64) * aspect / 2.0).round().max(1.0) as u32;
    let (out_w, out_h) = if a.braille || a.edge { (cols * 2, rows * 4) } else { (cols, rows * 2) };
    let resized = image::imageops::resize(&src, out_w, out_h, FilterType::Triangle);
    print!("{}", render(&resized, a).replace("\r\n", "\n"));
}

fn main() {
    let mut a = parse_args();

    // 画像モード
    if let Some(path) = &a.image {
        match image::open(path) {
            Ok(im) => { oneshot(im.to_rgb8(), &a); }
            Err(e) => { eprintln!("image open {path}: {e}"); std::process::exit(1); }
        }
        return;
    }

    // 中心座標の決定 (--resume > --place > --lat/--lon)
    let (lat, lon) = if a.resume && a.place.is_none() && a.lat.is_none() && a.lon.is_none() {
        match load_state() {
            Some((la, lo, z, st)) => { a.zoom = z; a.style = st; (la, lo) }
            None => { eprintln!("保存された location がありません (--resume)"); std::process::exit(1); }
        }
    } else if let Some(p) = &a.place {
        match geocode(p) { Ok(v) => v, Err(e) => { eprintln!("{e}"); std::process::exit(1); } }
    } else {
        match (a.lat, a.lon) { (Some(la), Some(lo)) => (la, lo), _ => { eprintln!("need --place \"住所\" or --lat/--lon or --image (or --resume)"); std::process::exit(2); } }
    };
    let (cx, cy) = deg_to_pixel(lat, lon, a.zoom);

    if a.interactive {
        if let Err(e) = interactive(cx, cy, a.zoom, &a) { eprintln!("interactive: {e}"); std::process::exit(1); }
        return;
    }

    let mut cache: Cache = HashMap::new();
    match build_window(cx, cy, a.zoom, a.win_px, a.win_px, &a.style, &mut cache) {
        Ok(src) => {
            save_state(lat, lon, a.zoom, &a.style);
            oneshot(src, &a);
            if a.png.is_none() { // 地図描画時のみ 中心座標+住所 をフッタ表示(stderr)
                let addr = reverse_geocode(lat, lon).unwrap_or_default();
                eprintln!("中心 {lat:.5},{lon:.5}  z{}  {}", a.zoom, addr);
            }
        }
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    }
}
