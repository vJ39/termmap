// 端末描画 (halfblock/braille/edge/classify) と オーバーレイ(POI/経路/リング)の構築・合成
use image::RgbImage;
use crate::geo::{deg_to_pixel, meters_per_pixel};

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
pub fn recolor(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    let mut out = RgbImage::from_pixel(w, h, image::Rgb([245, 245, 245]));
    for (x, y, p) in img.enumerate_pixels() {
        if let Some(c) = classify(p) { let (r, g, b) = cat_color(c); out.put_pixel(x, y, image::Rgb([r, g, b])); }
    }
    out
}

pub fn render_halfblock(img: &RgbImage) -> String {
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
pub fn render_braille(img: &RgbImage, mono: bool, classify_on: bool, threshold: u8, edge: bool, ov: Option<&OverlayLayer>) -> String {
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
pub enum PoiCat { Home, Food, Fuel, Shop, Danger, Waypoint, Other }
fn poi_color(c: PoiCat) -> [u8; 3] {
    match c {
        PoiCat::Home => [255, 64, 64], PoiCat::Food => [255, 140, 0],
        PoiCat::Fuel => [255, 215, 0], PoiCat::Shop => [80, 200, 255],
        PoiCat::Danger => [255, 0, 200], PoiCat::Waypoint => [120, 255, 120],
        PoiCat::Other => [255, 255, 255],
    }
}
#[allow(dead_code)] // POI 実装(次増分)で使用
pub struct Poi { pub lat: f64, pub lon: f64, pub cat: PoiCat }
pub struct Route { pub pts: Vec<(f64, f64)>, pub color: [u8; 3], pub thickness: u32 }
pub struct Ring { pub lat: f64, pub lon: f64, pub radii_km: Vec<f64>, pub color: [u8; 3], pub thickness: u32 }
// roads は道路名検索(r)で追加した道路の「塊」を保持する別レイヤ。routes(BRouterルート)とは
// 独立で、trigger_route の routes.clear() では消えない。個別追加・個別削除できる。
pub struct OverlaySpec { pub pois: Vec<Poi>, pub routes: Vec<Route>, pub roads: Vec<Route>, pub rings: Vec<Ring>, pub spots: Vec<(f64, f64, [u8; 3], u8)> }
impl OverlaySpec {
    pub fn is_empty(&self) -> bool { self.pois.is_empty() && self.routes.is_empty() && self.roads.is_empty() && self.rings.is_empty() && self.spots.is_empty() }
}

// インクマスク層。描画は最終出力寸法(resize後)で構築する。
pub struct OverlayLayer { w: u32, h: u32, ink: Vec<Option<[u8; 3]>> }
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
// マーカー形状。0=四角 1=三角(上向) 2=丸 3=菱形 4=十字 5=星(8方向)。カテゴリ別の識別用。
pub const NUM_MARKER_SHAPES: u8 = 6;
fn marker_inside(dx: i32, dy: i32, half: i32, shape: u8) -> bool {
    match shape {
        1 => dx.abs() <= dy + half,                    // 三角(頂点上)
        2 => dx * dx + dy * dy <= half * half + 1,      // 丸
        3 => dx.abs() + dy.abs() <= half,               // 菱形
        4 => dx == 0 || dy == 0,                        // 十字
        5 => dx == 0 || dy == 0 || dx.abs() == dy.abs(), // 星(8方向)
        _ => true,                                      // 四角
    }
}
fn draw_marker(ov: &mut OverlayLayer, ix: i32, iy: i32, color: [u8; 3], size: i32, shape: u8) {
    let half = size / 2;
    // ハロー: 形状を1px膨張させた暗色
    for dy in -half - 1..=half + 1 { for dx in -half - 1..=half + 1 {
        if marker_inside(dx, dy, half + 1, shape) { ov.put(ix + dx, iy + dy, [20, 20, 20]); }
    }}
    for dy in -half..=half { for dx in -half..=half {
        if marker_inside(dx, dy, half, shape) { ov.put(ix + dx, iy + dy, color); }
    }}
}
pub fn draw_line(ov: &mut OverlayLayer, mut x0: i32, mut y0: i32, x1: i32, y1: i32, color: [u8; 3], thickness: u32) {
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
pub fn draw_ring(ov: &mut OverlayLayer, cx: i32, cy: i32, radius: i32, color: [u8; 3], thickness: u32) {
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
pub fn build_overlay(spec: &OverlaySpec, cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32,
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
    for rt in &spec.routes { // 経路(BRouterルート)
        let pts: Vec<(i32, i32)> = rt.pts.iter().map(|&(la, lo)| to_img(la, lo)).collect();
        draw_polyline(&mut ov, &pts, rt.color, rt.thickness);
    }
    for rd in &spec.roads { // 道路の塊(別色レイヤ・BRouterルートの上に乗る)
        let pts: Vec<(i32, i32)> = rd.pts.iter().map(|&(la, lo)| to_img(la, lo)).collect();
        draw_polyline(&mut ov, &pts, rd.color, rd.thickness);
    }
    for p in &spec.pois { // マーカー(最前面)
        let (ix, iy) = to_img(p.lat, p.lon);
        if ix < -4 || iy < -4 || ix > out_w as i32 + 4 || iy > out_h as i32 + 4 { continue; }
        draw_marker(&mut ov, ix, iy, poi_color(p.cat), 3, 0);
    }
    for (la, lo, col, shape) in &spec.spots { // マイスポット(カテゴリ色＋形状)
        let (ix, iy) = to_img(*la, *lo);
        if ix < -4 || iy < -4 || ix > out_w as i32 + 4 || iy > out_h as i32 + 4 { continue; }
        draw_marker(&mut ov, ix, iy, *col, 4, *shape); // size 4=5x5で形状を判別可能に
    }
    ov
}
pub fn composite(img: &mut RgbImage, ov: &OverlayLayer) {
    let (w, h) = img.dimensions();
    for y in 0..h.min(ov.h) { for x in 0..w.min(ov.w) {
        if let Some(c) = ov.get(x, y) { img.put_pixel(x, y, image::Rgb(c)); }
    }}
}

// ---- インライン画像出力 (iTerm2 OSC 1337) ----
// AA(ハーフブロック/braille)ではなく、端末のインライン画像プロトコルで実画像を表示する。

// 端末がインライン画像(iTerm2 OSC1337)に対応しているか。iTerm2 / WezTerm を対応とみなす。
// Terminal.app 等は非対応。tmux は対象外(パススルーが必要なため判定しない)。
pub fn image_capable() -> bool {
    if let Ok(tp) = std::env::var("TERM_PROGRAM") {
        if tp == "iTerm.app" || tp == "WezTerm" { return true; }
    }
    if std::env::var("LC_TERMINAL").map(|v| v == "iTerm2").unwrap_or(false) { return true; }
    std::env::var_os("ITERM_SESSION_ID").is_some()
}

// 標準base64符号化(依存追加なしのため自前実装)。パディングは '=' で埋める。
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

// RgbImage を PNG 化 → 自前base64 → iTerm2 インライン画像(OSC1337)として out へ出力する。
// cell_w / cell_h は表示セル数(端末セル単位)。カーソルは呼び出し側で左上セルへ移動済みが前提。
// preserveAspectRatio=0 で指定セル矩形にちょうど収める。PNG符号化に失敗した場合は何も出力しない。
pub fn emit_iterm2_image<W: std::io::Write>(out: &mut W, rgb: &RgbImage, cell_w: u32, cell_h: u32) -> std::io::Result<()> {
    use image::ImageEncoder;
    let mut png: Vec<u8> = Vec::new();
    if image::codecs::png::PngEncoder::new(&mut png)
        .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
        .is_err()
    {
        return Ok(());
    }
    let b64 = base64_encode(&png);
    write!(out, "\x1b]1337;File=inline=1;width={cell_w};height={cell_h};preserveAspectRatio=0:{b64}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 テストベクタ
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_all_bytes_roundtrip_length() {
        // 全256バイトを符号化しても長さは4の倍数・パディング規則に従う
        let data: Vec<u8> = (0u16..256).map(|b| b as u8).collect();
        let enc = base64_encode(&data);
        assert_eq!(enc.len() % 4, 0);
        assert_eq!(enc.len(), data.len().div_ceil(3) * 4);
    }

    #[test]
    fn emit_iterm2_image_wraps_osc1337() {
        let img = RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
        let mut buf: Vec<u8> = Vec::new();
        emit_iterm2_image(&mut buf, &img, 4, 3).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("\x1b]1337;File=inline=1;width=4;height=3;preserveAspectRatio=0:"));
        assert!(s.ends_with('\x07'));
    }

    // テスト検証用の base64 復号(標準アルファベット・パディング対応)。
    fn base64_decode(s: &str) -> Vec<u8> {
        fn val(c: u8) -> u32 {
            match c {
                b'A'..=b'Z' => (c - b'A') as u32,
                b'a'..=b'z' => (c - b'a' + 26) as u32,
                b'0'..=b'9' => (c - b'0' + 52) as u32,
                b'+' => 62,
                b'/' => 63,
                _ => 0, // '=' 等
            }
        }
        let bytes: Vec<u8> = s.bytes().collect();
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let n = (val(chunk[0]) << 18)
                | (val(*chunk.get(1).unwrap_or(&b'A')) << 12)
                | (val(*chunk.get(2).unwrap_or(&b'A')) << 6)
                | val(*chunk.get(3).unwrap_or(&b'A'));
            out.push((n >> 16) as u8);
            if chunk.get(2).map_or(false, |&c| c != b'=') { out.push((n >> 8) as u8); }
            if chunk.get(3).map_or(false, |&c| c != b'=') { out.push(n as u8); }
        }
        out
    }

    #[test]
    fn emit_iterm2_image_produces_decodable_png() {
        // emit した base64 を復号 → 実PNGとしてデコードでき、画素が保存されることを確認
        let mut img = RgbImage::new(3, 2);
        img.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        img.put_pixel(2, 1, image::Rgb([0, 128, 255]));
        let mut buf: Vec<u8> = Vec::new();
        emit_iterm2_image(&mut buf, &img, 3, 2).unwrap();
        let s = String::from_utf8(buf).unwrap();
        let b64 = s
            .strip_prefix("\x1b]1337;File=inline=1;width=3;height=2;preserveAspectRatio=0:")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        let png = base64_decode(b64);
        let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (3, 2));
        assert_eq!(decoded.get_pixel(0, 0), &image::Rgb([255, 0, 0]));
        assert_eq!(decoded.get_pixel(2, 1), &image::Rgb([0, 128, 255]));
    }
}
