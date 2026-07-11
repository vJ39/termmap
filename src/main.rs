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
    braille: bool,
    mono: bool,
    classify: bool,
    interactive: bool,
    threshold: u8,
    image: Option<String>,
    png: Option<String>,
}

fn arg_err(msg: &str) -> ! { eprintln!("{msg}"); std::process::exit(2); }

fn parse_args() -> Args {
    let mut a = Args { lat: None, lon: None, place: None, zoom: 14, width: None, win_px: 640,
                       braille: false, mono: false, classify: false, interactive: false,
                       threshold: 195, image: None, png: None };
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
            "--braille" => a.braille = true,
            "--mono" => a.mono = true,
            "--classify" => a.classify = true,
            "-i" | "--interactive" => a.interactive = true,
            "--threshold" => a.threshold = num!("--threshold"),
            "--image" => a.image = Some(val!("--image")),
            "--png" => a.png = Some(val!("--png")),
            "-h" | "--help" => { eprintln!("usage: termmap (--place \"住所\" | --lat LAT --lon LON) [--zoom Z] [-i] [--braille] [--classify] [--mono] [--width N] [--png OUT] | --image PNG"); std::process::exit(0); }
            _ => arg_err(&format!("unknown arg: {k}")),
        }
    }
    if a.image.is_none() && a.zoom > 20 { arg_err("--zoom は 0..=20 で指定 (OSMタイル有効域)"); }
    if a.win_px == 0 || a.win_px > 2048 { arg_err("--win は 1..=2048 で指定"); }
    if a.width == Some(0) { arg_err("--width は 1 以上で指定"); }
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

fn fetch_tile(z: u32, x: i64, y: i64) -> Result<RgbImage, String> {
    let url = format!("https://tile.openstreetmap.org/{z}/{x}/{y}.png");
    let resp = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .call().map_err(|e| format!("fetch tile {z}/{x}/{y}: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(image::load_from_memory(&buf).map_err(|e| format!("decode tile {z}/{x}/{y}: {e}"))?.to_rgb8())
}

// 中心(cx,cy グローバルpx)から win_w×win_h の矩形窓を組み立てる。タイルは cache 経由。
fn build_window(cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32, cache: &mut Cache) -> Result<RgbImage, String> {
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
            let hs: Vec<_> = chunk.iter().map(|&(wx, ty)| s.spawn(move || ((wx, ty), fetch_tile(z, wx, ty)))).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for ((wx, ty), r) in got { cache.insert((z, wx, ty), r?); }
    }

    let cols = (tx_max - tx_min + 1) as u32;
    let rows = (ty_max - ty_min + 1) as u32;
    let mut canvas = RgbImage::from_pixel(cols * TILE, rows * TILE, image::Rgb([221, 221, 221]));
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
    let crop_x = (left - tx_min as f64 * tf).round().max(0.0) as u32;
    let crop_y = (top - ty_min as f64 * tf).round().max(0.0) as u32;
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
    let mut out = String::with_capacity((w * h) as usize * 20);
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
fn render_braille(img: &RgbImage, mono: bool, classify_on: bool, threshold: u8) -> String {
    const BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    let (w, h) = img.dimensions();
    let (cols, rows) = (w / 2, h / 4);
    let th = threshold as f64;
    let mut out = String::with_capacity((cols * rows) as usize * 6);
    for cy in 0..rows {
        for cx in 0..cols {
            let mut bits: u8 = 0;
            let (mut sr, mut sg, mut sb, mut n) = (0u32, 0u32, 0u32, 0u32);
            let mut cc = [0u32; 6];
            for dx in 0..2u32 {
                for dy in 0..4u32 {
                    let p = img.get_pixel(cx * 2 + dx, cy * 4 + dy);
                    let on = if classify_on { classify(p).is_some() } else { lum(p) < th };
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
                out.push_str(&format!("\x1b[38;2;{};{};{}m{ch}", sr / n, sg / n, sb / n));
            }
        }
        if !mono { out.push_str("\x1b[0m"); }
        out.push_str("\r\n");
    }
    out
}

fn render(img: &RgbImage, a: &Args) -> String {
    if a.braille { render_braille(img, a.mono, a.classify, a.threshold) }
    else if a.classify { render_halfblock(&recolor(img)) }
    else { render_halfblock(img) }
}

// ---- 対話モード (crossterm) ----
fn interactive(mut cx: f64, mut cy: f64, mut z: u32, a: &Args) -> std::io::Result<()> {
    use crossterm::{terminal, event::{self, Event, KeyCode}, cursor, execute};
    let mut cache: Cache = HashMap::new();
    let mut out = std::io::stdout();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;
    let res = (|| -> std::io::Result<()> {
        loop {
            let (tc, tr) = terminal::size().unwrap_or((100, 40));
            let cols = tc.max(10) as u32;
            let map_rows = (tr.max(3) - 1) as u32;
            let (ow, oh) = if a.braille { (cols * 2, map_rows * 4) } else { (cols, map_rows * 2) };
            let body = match build_window(cx, cy, z, ow, oh, &mut cache) {
                Ok(img) => render(&img, a),
                Err(e) => format!("取得失敗: {e}\r\n"),
            };
            let (lat, lon) = pixel_to_deg(cx, cy, z);
            let status = format!(" z{z}  {lat:.5},{lon:.5}   ←↑↓→=pan  +/-=zoom  q=quit ");
            let status: String = status.chars().take(cols as usize).collect();
            write!(out, "\x1b[H{body}\x1b[{tr};1H\x1b[7m{status}\x1b[0m")?;
            out.flush()?;
            match event::read()? {
                Event::Key(k) => {
                    let (dx, dy) = (ow as f64 / 3.0, oh as f64 / 3.0);
                    match k.code {
                        KeyCode::Left => cx -= dx,
                        KeyCode::Right => cx += dx,
                        KeyCode::Up => cy -= dy,
                        KeyCode::Down => cy += dy,
                        KeyCode::Char('+') | KeyCode::Char('=') => if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; },
                        KeyCode::Char('-') | KeyCode::Char('_') => if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; },
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        _ => {}
                    }
                    // ピクセル範囲を [0, n) に収める(経度wrap相当)
                    let n = (TILE as f64) * 2f64.powi(z as i32);
                    if cx < 0.0 { cx += n; } else if cx >= n { cx -= n; }
                    cy = cy.clamp(0.0, n - 1.0);
                }
                Event::Resize(..) => {}
                _ => {}
            }
        }
        Ok(())
    })();
    execute!(out, cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    res
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
    let (out_w, out_h) = if a.braille { (cols * 2, rows * 4) } else { (cols, rows * 2) };
    let resized = image::imageops::resize(&src, out_w, out_h, FilterType::Triangle);
    print!("{}", render(&resized, a).replace("\r\n", "\n"));
}

fn main() {
    let a = parse_args();

    // 画像モード
    if let Some(path) = &a.image {
        match image::open(path) {
            Ok(im) => { oneshot(im.to_rgb8(), &a); }
            Err(e) => { eprintln!("image open {path}: {e}"); std::process::exit(1); }
        }
        return;
    }

    // 中心座標の決定 (--place 優先)
    let (lat, lon) = if let Some(p) = &a.place {
        match geocode(p) { Ok(v) => v, Err(e) => { eprintln!("{e}"); std::process::exit(1); } }
    } else {
        match (a.lat, a.lon) { (Some(la), Some(lo)) => (la, lo), _ => { eprintln!("need --place \"住所\" or --lat/--lon or --image"); std::process::exit(2); } }
    };
    let (cx, cy) = deg_to_pixel(lat, lon, a.zoom);

    if a.interactive {
        if let Err(e) = interactive(cx, cy, a.zoom, &a) { eprintln!("interactive: {e}"); std::process::exit(1); }
        return;
    }

    let mut cache: Cache = HashMap::new();
    match build_window(cx, cy, a.zoom, a.win_px, a.win_px, &mut cache) {
        Ok(src) => oneshot(src, &a),
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    }
}
