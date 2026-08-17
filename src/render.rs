// 端末描画 (halfblock/braille/edge/classify) と オーバーレイ(POI/経路/リング)の構築・合成
use image::{RgbImage, RgbaImage};
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

pub fn render_halfblock(img: &RgbImage, truecolor: bool) -> String {
    let (w, h) = img.dimensions();
    let mut out = String::with_capacity(w as usize * h as usize * 20);
    let mut y = 0;
    while y + 1 < h {
        for x in 0..w {
            let t = img.get_pixel(x, y);
            let b = img.get_pixel(x, y + 1);
            out.push_str(&sgr_fg(t[0], t[1], t[2], truecolor));
            out.push_str(&sgr_bg(b[0], b[1], b[2], truecolor));
            out.push('\u{2580}');
        }
        out.push_str("\x1b[0m\r\n");
        y += 2;
    }
    out
}
pub fn render_braille(img: &RgbImage, mono: bool, classify_on: bool, threshold: u8, edge: bool, ov: Option<&OverlayLayer>, truecolor: bool) -> String {
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
            else if ovn > 0 { out.push_str(&sgr_fg((ovr / ovn) as u8, (ovg / ovn) as u8, (ovb / ovn) as u8, truecolor)); out.push(ch); }
            else if classify_on {
                let bi = (0..6).max_by_key(|&i| cc[i]).unwrap();
                let (r, g, b) = cat_color([Cat::Water, Cat::Park, Cat::RoadMajor, Cat::Rail, Cat::Building, Cat::Other][bi]);
                out.push_str(&sgr_fg(r, g, b, truecolor)); out.push(ch);
            } else {
                // braille はインク=暗い画素の平均色になりがちで沈むので輝度を持ち上げる
                let br = |s: u32| ((s as f64 / n as f64) * 1.6).min(255.0) as u8;
                out.push_str(&sgr_fg(br(sr), br(sg), br(sb), truecolor)); out.push(ch);
            }
        }
        if !mono { out.push_str("\x1b[0m"); }
        out.push_str("\r\n");
    }
    out
}

// ---- QRコード画像描画 ----
// dark[y*width+x] (true=QRの黒モジュール) の正方格子を、1モジュール=module_px四方のソリッド正方形
// として実ピクセル画像に焼く(qrcodeクレートに依存しない純関数)。文字セル密度の制約を受けないため、
// iTerm2等のインライン画像で表示すれば、セル数(見た目の大きさ)をモジュール数と切り離して自由に
// 小さくできる。quiet_modulesはQR仕様の静穏領域(四辺に確保、既定4モジュール)。
pub fn render_qr_image(dark: &[bool], width: usize, module_px: u32, quiet_modules: u32) -> RgbImage {
    let side_mod = width as u32 + quiet_modules * 2;
    let side_px = (side_mod * module_px).max(1);
    let mut img = RgbImage::from_pixel(side_px, side_px, image::Rgb([255, 255, 255]));
    for y in 0..width {
        for x in 0..width {
            if dark.get(y * width + x).copied().unwrap_or(false) {
                let px0 = (x as u32 + quiet_modules) * module_px;
                let py0 = (y as u32 + quiet_modules) * module_px;
                for dy in 0..module_px {
                    for dx in 0..module_px {
                        img.put_pixel(px0 + dx, py0 + dy, image::Rgb([0, 0, 0]));
                    }
                }
            }
        }
    }
    img
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
// traffic_segments は渋滞状況の色分け(#渋滞情報)用の別レイヤ。routes[0]と同じ経路を区間ごとに
// 塗り直した色付きの線を保持する(routesの中身自体は差し替えない)。GPX保存・標高表示・
// 次の曲がり案内は routes.last() を「ルート全体」として参照しているため、そちらを壊さないよう
// 独立フィールドにしている。
pub struct OverlaySpec { pub pois: Vec<Poi>, pub routes: Vec<Route>, pub roads: Vec<Route>, pub traffic_segments: Vec<Route>, pub rings: Vec<Ring>, pub spots: Vec<(f64, f64, [u8; 3], u8)> }
impl OverlaySpec {
    pub fn is_empty(&self) -> bool { self.pois.is_empty() && self.routes.is_empty() && self.roads.is_empty() && self.traffic_segments.is_empty() && self.rings.is_empty() && self.spots.is_empty() }
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
// マーカー形状。0=四角 1=三角(上向) 2=丸 3=菱形 4=十字 5=星(8方向) 6=✕(対角線・規制原因アイコン用)。カテゴリ別の識別用。
pub const NUM_MARKER_SHAPES: u8 = 7;
fn marker_inside(dx: i32, dy: i32, half: i32, shape: u8) -> bool {
    match shape {
        1 => dx.abs() <= dy + half,                    // 三角(頂点上)
        2 => dx * dx + dy * dy <= half * half + 1,      // 丸
        3 => dx.abs() + dy.abs() <= half,               // 菱形
        4 => dx == 0 || dy == 0,                        // 十字
        5 => dx == 0 || dy == 0 || dx.abs() == dy.abs(), // 星(8方向)
        6 => dx.abs() == dy.abs(),                       // ✕(対角線のみ、十字線は無し)
        _ => true,                                      // 四角
    }
}
pub fn draw_marker(ov: &mut OverlayLayer, ix: i32, iy: i32, color: [u8; 3], size: i32, shape: u8) {
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
// inks は braille/edge 経路で最背面へ焼く半透明レイヤ群(空なら従来と同じ)。
// **配列の順序がそのまま重ね順**(先頭が最背面)。呼び出し側は [コロプレス, 雨雲] を渡す。
pub fn build_overlay(spec: &OverlaySpec, cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32,
                 scale_x: f64, scale_y: f64, out_w: u32, out_h: u32,
                 inks: &[InkLayer]) -> OverlayLayer {
    let mut ov = OverlayLayer::new(out_w, out_h);
    // インク層(最背面)。リング/経路/道路/POI/スポットはこの後に描かれるので、常にインクより前面になる。
    for ink in inks {
        match ink {
            InkLayer::Dither { layer, density } => ink_radar_into_overlay(&mut ov, layer, RADAR_INK_MIN_ALPHA, *density),
            InkLayer::Stipple { layer, spacing } => stipple_rgba_into_overlay(&mut ov, layer, *spacing),
        }
    }
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
    for tr in &spec.traffic_segments { // 渋滞状況の色分け(BRouterルートと同じ経路を上塗り)
        let pts: Vec<(i32, i32)> = tr.pts.iter().map(|&(la, lo)| to_img(la, lo)).collect();
        draw_polyline(&mut ov, &pts, tr.color, tr.thickness);
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

// 半透明レイヤ(雨雲など)を base の上に source-over 合成する。
// a' = (src.a / 255) * opacity として out = base*(1-a') + src*a'。opacity は 0.0..=1.0 にクランプ。
// 寸法が違う場合は重なる範囲だけ処理する(パニックしない)。
// OverlayLayer(不透明1色インク)と違い、地図が下に透けて見えるのが要点。
pub fn blend_rgba_over(base: &mut RgbImage, layer: &RgbaImage, opacity: f64) {
    let op = opacity.clamp(0.0, 1.0);
    if op <= 0.0 { return; }
    let (bw, bh) = base.dimensions();
    let (lw, lh) = layer.dimensions();
    for y in 0..bh.min(lh) {
        for x in 0..bw.min(lw) {
            let s = layer.get_pixel(x, y);
            let a = (s[3] as f64 / 255.0) * op;
            if a <= 0.0 { continue; } // 降水なし(透明)の画素は地図をそのまま残す
            let d = base.get_pixel(x, y);
            let mix = |dv: u8, sv: u8| ((dv as f64) * (1.0 - a) + (sv as f64) * a).round().clamp(0.0, 255.0) as u8;
            base.put_pixel(x, y, image::Rgb([mix(d[0], s[0]), mix(d[1], s[1]), mix(d[2], s[2])]));
        }
    }
}

// braille/edge 経路で半透明レイヤを OverlayLayer のインクとして焼くときの指定。build_overlay へ渡す。
// 間引き方を2種類に分けるのは、覆う面積の性質が違うため(下の stipple_rgba_into_overlay を参照)。
pub enum InkLayer<'a> {
    // 雨雲(降水なし=透明)。4x4 Bayer のディザで間引く。density は 0.0..=1.0。
    Dither { layer: &'a RgbaImage, density: f64 },
    // 市区町村の塗り等の「大きな面」。spacing 画素ごとに1点だけ置く疎な点描で間引く。
    Stipple { layer: &'a RgbaImage, spacing: u32 },
}

// インクを置く「降水あり」のアルファ下限。気象庁ナウキャストのタイルは降水域=不透明/降水なし=
// 完全透明の2値(実測: 4bitパレット+tRNS。透明indexのalphaが0、降水色は全て255)なので、
// タイル拡大時の補間等で生じうる中途半端なアルファだけを弾く控えめな値にしてある。
const RADAR_INK_MIN_ALPHA: u8 = 32;

// 4x4 Bayer(ディザ)閾値マトリクス。値(0..15)が density*16 未満の画素だけインクを置く。
// density=0.5 でちょうど (x+y)%2==0 の市松、0.75 で4画素に3つ、0.35 前後で3画素に1つ、
// 1.0 で全塗りになる(設計 §7.3 の 薄い/標準/濃い をそのまま密度で表現できる)。
const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

// braille/edge 用。降水域を OverlayLayer の「インク」として置く。
// これらのモードは「ドットが立つか立たないか」しかなく背景色の概念が無いため、アルファ合成では
// 降水として読めない(braille)/降水の境界が全部輪郭になって線画が壊れる(edge)。
// alpha が min_alpha 以上の画素のみ対象。さらに density(0.0..=1.0)でディザ間引きして
// スクリーンドア状に下の地図を透かす(全画素塗ると不透明インクなので地図が完全に隠れる)。
// インクの色は降水強度の色(気象庁の配色)をそのまま使うので、braille でも強弱が色で読める。
pub fn ink_radar_into_overlay(ov: &mut OverlayLayer, layer: &RgbaImage, min_alpha: u8, density: f64) {
    let d = density.clamp(0.0, 1.0);
    if d <= 0.0 { return; }
    let th = d * 16.0;
    let (lw, lh) = layer.dimensions();
    // min_alpha=0 でも完全透明(降水なし)の画素は置かない。全面がインクで埋まる事故を型で防げない分ここで止める。
    let floor = min_alpha.max(1);
    for y in 0..ov.h.min(lh) {
        for x in 0..ov.w.min(lw) {
            let p = layer.get_pixel(x, y);
            if p[3] < floor { continue; }                                      // 降水なし
            if (BAYER4[(y % 4) as usize][(x % 4) as usize] as f64) >= th { continue; } // ディザ間引き
            ov.put(x as i32, y as i32, [p[0], p[1], p[2]]);
        }
    }
}

// 疎な点描でインクを置くときの「不透明」の下限。完全透明(何も無い)だけを弾く。
const STIPPLE_MIN_ALPHA: u8 = 1;

// braille/edge へ「大きな面」を乗せるための疎な点描。spacing×spacing のブロックごとに1点だけ置く。
//
// **面塗りに BAYER4 のディザ(ink_radar_into_overlay)をそのまま使ってはいけない**。
// render_braille は「そのセル(2x4画素)にオーバーレイのインクが1つでもあれば、セル全体の文字色を
// インク色にする」実装(`else if ovn > 0` の分岐が地図側の色を捨てる)で、BAYER4 は4x4周期なので
// braille の1セルはそのタイルのちょうど半分を覆う。最も薄い density=0.35 でも、どの位相でも
// 必ず3画素にインクが乗る = 全セルが塗り色に化ける。雨雲は降っている場所だけなので実害が無いが、
// 市区町村の塗りは画面の大半を覆うため線画が丸ごと単色になってしまう。
// spacing=8 なら色が付くセルは約12%(braille セル8個に1点)に収まる。
//
// ブロック内で最初に見つかった不透明画素の位置へ置く(ブロックの原点を固定で見ない)のは、
// 面だけでなく1px幅の縁取りも点として残すため。原点固定だと縁取りがほぼ全部間引かれて消える。
pub fn stipple_rgba_into_overlay(ov: &mut OverlayLayer, layer: &RgbaImage, spacing: u32) {
    let s = spacing.max(1); // 0 を渡されても全画素に置く(=1)側へ倒す。0除算・無限ループを作らない
    let (lw, lh) = layer.dimensions();
    let (w, h) = (ov.w.min(lw), ov.h.min(lh));
    let mut by = 0;
    while by < h {
        let mut bx = 0;
        while bx < w {
            'blk: for y in by..(by + s).min(h) {
                for x in bx..(bx + s).min(w) {
                    let p = layer.get_pixel(x, y);
                    if p[3] >= STIPPLE_MIN_ALPHA {
                        ov.put(x as i32, y as i32, [p[0], p[1], p[2]]);
                        break 'blk; // 1ブロック1点
                    }
                }
            }
            bx += s;
        }
        by += s;
    }
}

// ---- ポリゴンのラスタライズ(市区町村の塗り) ----
// draw_line/draw_ring/draw_marker と同じ「純粋な描画プリミティブ」。どの区域を何色で塗るかは
// 呼び出し側(choropleth.rs)の責務で、ここは画面座標のリング列しか知らない。

fn put_rgba(img: &mut RgbaImage, x: i32, y: i32, c: [u8; 4]) {
    let (w, h) = img.dimensions();
    if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h { return; }
    img.put_pixel(x as u32, y as u32, image::Rgba(c));
}

// リング列の外接矩形(画面座標)。頂点が無ければ None。
fn rings_px_bbox(rings: &[Vec<(i32, i32)>]) -> Option<(i32, i32, i32, i32)> {
    let mut b = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    let mut seen = false;
    for r in rings { for &(x, y) in r {
        b.0 = b.0.min(x); b.1 = b.1.min(y); b.2 = b.2.max(x); b.3 = b.3.max(y);
        seen = true;
    }}
    seen.then_some(b)
}

// 画面座標のリング列(外周+穴+離島)を even-odd 規則で塗る。リングは閉じている必要はない
// (末尾と先頭を暗黙に結ぶ)。外接矩形と画像の重なりだけを走査するので、画面外のポリゴンは即座に返る。
//
// even-odd を選ぶのは、穴(飛地に囲まれた区域)と多重ポリゴン(離島)を1回の走査で同時に処理
// できるから。外周と穴を区別して持つ必要がなく、GeoJSON の Polygon/MultiPolygon のリングを
// 全部同じ配列に並べるだけで正しく塗れる(class20s はリングの巻き方向が不統一なので、
// 向きに依存する nonzero winding は使えない)。
pub fn fill_rings_rgba(img: &mut RgbaImage, rings: &[Vec<(i32, i32)>], color: [u8; 4]) {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 { return; }
    let Some(bb) = rings_px_bbox(rings) else { return };
    if bb.2 < 0 || bb.0 >= w as i32 || bb.3 < 0 || bb.1 >= h as i32 { return; } // 画面外
    let y0 = bb.1.max(0);
    let y1 = bb.3.min(h as i32 - 1);
    let mut xs: Vec<i32> = Vec::new();
    for y in y0..=y1 {
        xs.clear();
        for ring in rings {
            let n = ring.len();
            if n < 3 { continue; } // 面を持たない
            let mut j = n - 1;
            for i in 0..n {
                let (xi, yi) = ring[i];
                let (xj, yj) = ring[j];
                // 半開区間規則。頂点をちょうど通る走査線で交点を二重に数えて塗りが裏返るのを防ぐ。
                if (yi <= y) != (yj <= y) {
                    let t = (y - yi) as f64 / (yj - yi) as f64; // 分母は 0 にならない
                    xs.push((xi as f64 + t * (xj - xi) as f64).round() as i32);
                }
                j = i;
            }
        }
        if xs.len() < 2 { continue; }
        xs.sort_unstable();
        // 交点は必ず偶数個になる(半開区間規則の帰結)。念のため最後の余りは捨てて走査する。
        for pair in xs.chunks_exact(2) {
            let (a, b) = (pair[0].max(0), pair[1].min(w as i32 - 1));
            for x in a..=b { put_rgba(img, x, y, color); }
        }
    }
}

// リングの輪郭(縁取り)。塗りより前面・雨雲より背面。リングは閉じている必要はない。
pub fn stroke_rings_rgba(img: &mut RgbaImage, rings: &[Vec<(i32, i32)>], color: [u8; 4], thickness: u32) {
    for ring in rings {
        if ring.len() < 2 { continue; }
        let mut prev = *ring.last().expect("checked above");
        for &p in ring {
            line_rgba(img, prev, p, color, thickness);
            prev = p;
        }
    }
}

// 1辺ぶんの線(Bresenham)。両端が画像の外で同じ側にある辺は走査せず捨てる(区域の大半が
// 画面外にあるとき、見えない画素を延々と辿らないための足切り)。
fn line_rgba(img: &mut RgbaImage, a: (i32, i32), b: (i32, i32), color: [u8; 4], thickness: u32) {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let t = thickness.max(1) as i32 - 1;
    // 太さ t のぶん、1点が実際に塗るのは [x, x+t] × [y, y+t]。両端ともその範囲が画像の
    // 同じ側へ完全に外れているなら、この辺は1画素も見えない。
    if (a.0 < -t && b.0 < -t) || (a.0 >= w && b.0 >= w) { return; }
    if (a.1 < -t && b.1 < -t) || (a.1 >= h && b.1 >= h) { return; }
    let (mut x0, mut y0) = a;
    let (x1, y1) = b;
    let dx = (x1 - x0).abs(); let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs(); let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        for oy in 0..=t { for ox in 0..=t { put_rgba(img, x0 + ox, y0 + oy, color); }}
        if x0 == x1 && y0 == y1 { break; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x0 += sx; }
        if e2 <= dx { err += dx; y0 += sy; }
    }
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

// halfblock/braille描画(1文字ごとにtruecolorのSGRを2〜3個発行する高密度な出力)を、24bit色の
// まま出しても安定して描画できるか。macOS標準Terminal.app(TERM_PROGRAM=Apple_Terminal)は
// COLORTERM=truecolorを名乗るが、実際にはこの密度のtruecolorシーケンスを捌ききれず、色の
// 状態を見失って帯状に色がにじむ表示崩れを起こすことを確認済み(termmap/aquaterm両方で再現)。
// 個別に除外し、それ以外はCOLORTERM=truecolor/24bitの申告を信用する(不明な端末は256色側)。
pub fn truecolor_safe() -> bool {
    if std::env::var("TERM_PROGRAM").ok().as_deref() == Some("Apple_Terminal") { return false; }
    matches!(std::env::var("COLORTERM").ok().as_deref(), Some("truecolor") | Some("24bit"))
}

// RGBをxterm 256色パレット(16〜231=6x6x6キューブ, 232〜255=24階調グレー)の最近傍indexへ。
// truecolor不安定な端末向けのフォールバック用(近似でよく、正確なCIE距離等は不要)。
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    if r.abs_diff(g) < 10 && g.abs_diff(b) < 10 && r.abs_diff(b) < 10 {
        let avg = (r as u32 + g as u32 + b as u32) / 3;
        if avg < 8 { return 16; }
        if avg > 248 { return 231; }
        return (232 + (avg - 8) * 24 / 247) as u8;
    }
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let level = |c: u8| -> u8 {
        LEVELS.iter().enumerate()
            .min_by_key(|(_, &lv)| (c as i32 - lv as i32).abs())
            .map(|(i, _)| i as u8).unwrap()
    };
    16 + 36 * level(r) + 6 * level(g) + level(b)
}

// 前景色のSGRを1つ発行する(truecolor不安定端末では256色に量子化)。ui.rs側のロゴ描画等でも使う。
pub(crate) fn sgr_fg(r: u8, g: u8, b: u8, truecolor: bool) -> String {
    if truecolor { format!("\x1b[38;2;{r};{g};{b}m") } else { format!("\x1b[38;5;{}m", rgb_to_ansi256(r, g, b)) }
}
// 背景色のSGRを1つ発行する(同上)。
fn sgr_bg(r: u8, g: u8, b: u8, truecolor: bool) -> String {
    if truecolor { format!("\x1b[48;2;{r};{g};{b}m") } else { format!("\x1b[48;5;{}m", rgb_to_ansi256(r, g, b)) }
}

// 標準base64符号化(依存追加なしのため自前実装)。パディングは '=' で埋める。
pub(crate) fn base64_encode(data: &[u8]) -> String {
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
    let size = png.len(); // base64化前の生PNGバイト数。addon-image側はsize未指定(既定0)だと
                          // "!this._header.size" が真になり、本体を1バイトもデコードせず即中断する
                          // (ブラウザで実画像が常に真っ黒/無表示になっていた原因)。
    write!(out, "\x1b]1337;File=inline=1;size={size};width={cell_w};height={cell_h};preserveAspectRatio=0:{b64}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- blend_rgba_over(雨雲などの半透明レイヤ合成) ----

    fn base_img(v: u8) -> RgbImage { RgbImage::from_pixel(4, 3, image::Rgb([v, v, v])) }

    // alpha=0(降水なし)は地図をそのまま残す。
    #[test]
    fn blend_transparent_layer_leaves_base_unchanged() {
        let mut b = base_img(100);
        let l = RgbaImage::from_pixel(4, 3, image::Rgba([255, 0, 0, 0]));
        blend_rgba_over(&mut b, &l, 1.0);
        assert!(b.pixels().all(|p| *p == image::Rgb([100, 100, 100])));
    }

    // alpha=255 かつ opacity=1.0 は完全置換。
    #[test]
    fn blend_opaque_layer_at_full_opacity_replaces() {
        let mut b = base_img(100);
        let l = RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255]));
        blend_rgba_over(&mut b, &l, 1.0);
        assert!(b.pixels().all(|p| *p == image::Rgb([10, 20, 30])));
    }

    // opacity=0.5 は中間値(200 と 100 の中間=150)。
    #[test]
    fn blend_half_opacity_is_midpoint() {
        let mut b = base_img(200);
        let l = RgbaImage::from_pixel(4, 3, image::Rgba([100, 100, 100, 255]));
        blend_rgba_over(&mut b, &l, 0.5);
        assert!(b.pixels().all(|p| *p == image::Rgb([150, 150, 150])));
    }

    // レイヤのalphaとopacityは掛け合わされる(a'=0.5*0.5=0.25 → 200 と 0 で 150)。
    #[test]
    fn blend_multiplies_layer_alpha_and_opacity() {
        let mut b = base_img(200);
        let l = RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 128]));
        blend_rgba_over(&mut b, &l, 0.5);
        // a' = (128/255)*0.5 ≒ 0.251 → 200*(1-0.251) ≒ 149.8 → 150
        assert_eq!(*b.get_pixel(0, 0), image::Rgb([150, 150, 150]));
        // レイヤ範囲外(1x1なので(1,0)以降)は変化しない。
        assert_eq!(*b.get_pixel(1, 0), image::Rgb([200, 200, 200]));
    }

    // opacity=0 は何もしない。範囲外(負/1超)はクランプされる。
    #[test]
    fn blend_opacity_bounds() {
        let l = RgbaImage::from_pixel(4, 3, image::Rgba([0, 0, 0, 255]));
        let mut b = base_img(200);
        blend_rgba_over(&mut b, &l, 0.0);
        assert!(b.pixels().all(|p| *p == image::Rgb([200, 200, 200])));
        blend_rgba_over(&mut b, &l, -5.0);
        assert!(b.pixels().all(|p| *p == image::Rgb([200, 200, 200])));
        blend_rgba_over(&mut b, &l, 9.0); // 1.0 として扱う=完全置換
        assert!(b.pixels().all(|p| *p == image::Rgb([0, 0, 0])));
    }

    // 寸法が違ってもパニックせず、重なる範囲だけ処理する(レイヤが大きい/小さい両方)。
    #[test]
    fn blend_mismatched_dimensions_do_not_panic() {
        let mut b = base_img(200); // 4x3
        let big = RgbaImage::from_pixel(10, 10, image::Rgba([0, 0, 0, 255]));
        blend_rgba_over(&mut b, &big, 1.0);
        assert!(b.pixels().all(|p| *p == image::Rgb([0, 0, 0])));

        let mut b2 = base_img(200);
        let small = RgbaImage::from_pixel(2, 1, image::Rgba([0, 0, 0, 255]));
        blend_rgba_over(&mut b2, &small, 1.0);
        assert_eq!(*b2.get_pixel(0, 0), image::Rgb([0, 0, 0]));
        assert_eq!(*b2.get_pixel(1, 0), image::Rgb([0, 0, 0]));
        assert_eq!(*b2.get_pixel(2, 0), image::Rgb([200, 200, 200]));
        assert_eq!(*b2.get_pixel(0, 1), image::Rgb([200, 200, 200]));

        // 空レイヤでも安全。
        let mut b3 = base_img(200);
        blend_rgba_over(&mut b3, &RgbaImage::new(0, 0), 1.0);
        assert!(b3.pixels().all(|p| *p == image::Rgb([200, 200, 200])));
    }

    // ---- ink_radar_into_overlay(braille/edge 用の雨雲インク) ----

    // 全画素が降水(不透明)のレイヤ。
    fn rain_layer(w: u32, h: u32) -> RgbaImage { RgbaImage::from_pixel(w, h, image::Rgba([0, 65, 255, 255])) }
    // ov に置かれたインクの数。
    fn ink_count(ov: &OverlayLayer) -> usize { ov.ink.iter().filter(|c| c.is_some()).count() }

    // alpha が min_alpha 未満(降水なし/ごく薄い縁)の画素にはインクを置かない。
    #[test]
    fn ink_skips_pixels_below_min_alpha() {
        let mut ov = OverlayLayer::new(4, 4);
        let layer = RgbaImage::from_pixel(4, 4, image::Rgba([0, 65, 255, 31]));
        ink_radar_into_overlay(&mut ov, &layer, 32, 1.0);
        assert_eq!(ink_count(&ov), 0);
        // 閾値ちょうどは対象(「min_alpha 以上」)。
        let mut ov2 = OverlayLayer::new(4, 4);
        let layer2 = RgbaImage::from_pixel(4, 4, image::Rgba([0, 65, 255, 32]));
        ink_radar_into_overlay(&mut ov2, &layer2, 32, 1.0);
        assert_eq!(ink_count(&ov2), 16);
    }

    // min_alpha=0 を渡されても、完全透明(降水なし)は置かない(全面インクで地図が消える事故を防ぐ)。
    #[test]
    fn ink_min_alpha_zero_still_skips_fully_transparent() {
        let mut ov = OverlayLayer::new(4, 4);
        let layer = RgbaImage::from_pixel(4, 4, image::Rgba([0, 65, 255, 0]));
        ink_radar_into_overlay(&mut ov, &layer, 0, 1.0);
        assert_eq!(ink_count(&ov), 0);
    }

    // density=1.0 は間引きなし(全塗り)。density=0.0 は1つも置かない。
    #[test]
    fn ink_density_bounds() {
        let layer = rain_layer(4, 4);
        let mut full = OverlayLayer::new(4, 4);
        ink_radar_into_overlay(&mut full, &layer, 32, 1.0);
        assert_eq!(ink_count(&full), 16);

        let mut none = OverlayLayer::new(4, 4);
        ink_radar_into_overlay(&mut none, &layer, 32, 0.0);
        assert_eq!(ink_count(&none), 0);

        // 範囲外はクランプ(負=何もしない / 1超=全塗り)。
        let mut neg = OverlayLayer::new(4, 4);
        ink_radar_into_overlay(&mut neg, &layer, 32, -1.0);
        assert_eq!(ink_count(&neg), 0);
        let mut over = OverlayLayer::new(4, 4);
        ink_radar_into_overlay(&mut over, &layer, 32, 5.0);
        assert_eq!(ink_count(&over), 16);
    }

    // density=0.5 はちょうど市松((x+y)%2==0 の画素だけ)。下の地図が半分透ける。
    #[test]
    fn ink_density_half_is_checkerboard() {
        let mut ov = OverlayLayer::new(8, 8);
        ink_radar_into_overlay(&mut ov, &rain_layer(8, 8), 32, 0.5);
        assert_eq!(ink_count(&ov), 32);
        for y in 0..8u32 {
            for x in 0..8u32 {
                let on = ov.get(x, y).is_some();
                assert_eq!(on, (x + y) % 2 == 0, "({x},{y}) の市松が期待と違う");
            }
        }
    }

    // 設計 §7.3 の密度換算: 薄い(0.35)≒3画素に1つ / 標準(0.55)≒半分 / 濃い(0.75)=4画素に3つ。
    #[test]
    fn ink_density_matches_designed_coverage() {
        let cases = [(0.35, 6usize), (0.55, 9), (0.75, 12)]; // 4x4=16画素あたりの点数
        for (d, want) in cases {
            let mut ov = OverlayLayer::new(4, 4);
            ink_radar_into_overlay(&mut ov, &rain_layer(4, 4), 32, d);
            assert_eq!(ink_count(&ov), want, "density={d}");
        }
        // 間引き模様は4x4周期。8x8では4x4ブロックと同じ密度がタイル状に繰り返される。
        let mut ov = OverlayLayer::new(8, 8);
        ink_radar_into_overlay(&mut ov, &rain_layer(8, 8), 32, 0.75);
        assert_eq!(ink_count(&ov), 12 * 4);
    }

    // インクの色は降水強度の色(気象庁の配色)をそのまま使う。braille でも強弱が色で読める。
    #[test]
    fn ink_uses_precipitation_color() {
        let mut ov = OverlayLayer::new(2, 2);
        let mut layer = RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        layer.put_pixel(0, 0, image::Rgba([255, 40, 0, 255]));   // 強い雨(赤)
        layer.put_pixel(1, 1, image::Rgba([160, 210, 255, 255])); // 弱い雨(淡い青)
        ink_radar_into_overlay(&mut ov, &layer, 32, 1.0);
        assert_eq!(ov.get(0, 0), Some([255, 40, 0]));
        assert_eq!(ov.get(1, 1), Some([160, 210, 255]));
        assert_eq!(ov.get(1, 0), None); // 降水なしはインクを置かない
    }

    // 寸法が違ってもパニックせず、重なる範囲だけ処理する。
    #[test]
    fn ink_mismatched_dimensions_do_not_panic() {
        let mut ov = OverlayLayer::new(4, 4);
        ink_radar_into_overlay(&mut ov, &rain_layer(10, 10), 32, 1.0); // レイヤが大きい
        assert_eq!(ink_count(&ov), 16);

        let mut ov2 = OverlayLayer::new(4, 4);
        ink_radar_into_overlay(&mut ov2, &rain_layer(2, 1), 32, 1.0); // レイヤが小さい
        assert_eq!(ink_count(&ov2), 2);
        assert!(ov2.get(0, 0).is_some());
        assert!(ov2.get(0, 1).is_none());

        let mut ov3 = OverlayLayer::new(4, 4);
        ink_radar_into_overlay(&mut ov3, &RgbaImage::new(0, 0), 32, 1.0); // 空レイヤ
        assert_eq!(ink_count(&ov3), 0);
    }

    // build_overlay に雨雲を渡すと最背面に入る = 経路/マーカーは必ず雨雲より前面に残る。
    #[test]
    fn build_overlay_puts_radar_behind_markers() {
        let (lat, lon, z) = (35.0, 139.0, 10u32);
        let (cx, cy) = deg_to_pixel(lat, lon, z);
        let spec = OverlaySpec { pois: Vec::new(), routes: Vec::new(), roads: Vec::new(), traffic_segments: Vec::new(),
                                 rings: Vec::new(), spots: vec![(lat, lon, [1, 2, 3], 0)] };
        let layer = RgbaImage::from_pixel(8, 8, image::Rgba([200, 0, 0, 255]));
        let ink = InkLayer::Dither { layer: &layer, density: 1.0 };
        let ov = build_overlay(&spec, cx, cy, z, 8, 8, 1.0, 1.0, 8, 8, &[ink]);
        assert_eq!(ov.get(4, 4), Some([1, 2, 3]));    // 窓中心のマーカーが雨雲を上書きしている
        assert_eq!(ov.get(0, 0), Some([200, 0, 0]));  // マーカーの無い所は雨雲のインク

        // インク層なしなら従来どおり(マーカー以外は何も置かれない)。
        let ov2 = build_overlay(&spec, cx, cy, z, 8, 8, 1.0, 1.0, 8, 8, &[]);
        assert_eq!(ov2.get(4, 4), Some([1, 2, 3]));
        assert_eq!(ov2.get(0, 0), None);
    }

    // 複数のインク層は配列の順序どおりに重なる(先頭が最背面)。呼び出し側は
    // [コロプレス, 雨雲] を渡すので、雨雲がコロプレスの上に来る。
    #[test]
    fn build_overlay_stacks_ink_layers_in_array_order() {
        let (lat, lon, z) = (35.0, 139.0, 10u32);
        let (cx, cy) = deg_to_pixel(lat, lon, z);
        let spec = OverlaySpec { pois: Vec::new(), routes: Vec::new(), roads: Vec::new(), traffic_segments: Vec::new(),
                                 rings: Vec::new(), spots: Vec::new() };
        let back = RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]));  // コロプレス
        let front = RgbaImage::from_pixel(8, 8, image::Rgba([200, 0, 0, 255]));  // 雨雲
        let ov = build_overlay(&spec, cx, cy, z, 8, 8, 1.0, 1.0, 8, 8, &[
            InkLayer::Stipple { layer: &back, spacing: 1 },
            InkLayer::Dither { layer: &front, density: 1.0 },
        ]);
        assert_eq!(ov.get(0, 0), Some([200, 0, 0]), "後ろの要素(雨雲)が前面");
        // 順序を入れ替えれば結果も入れ替わる。
        let ov2 = build_overlay(&spec, cx, cy, z, 8, 8, 1.0, 1.0, 8, 8, &[
            InkLayer::Dither { layer: &front, density: 1.0 },
            InkLayer::Stipple { layer: &back, spacing: 1 },
        ]);
        assert_eq!(ov2.get(0, 0), Some([10, 20, 30]));
    }

    // ---- stipple_rgba_into_overlay(braille/edge 用の面塗りインク) ----

    #[test]
    fn stipple_places_exactly_one_dot_per_block() {
        // 8x8 の全面塗り。spacing=4 なら 2x2=4ブロック → 4点。
        let mut ov = OverlayLayer::new(8, 8);
        stipple_rgba_into_overlay(&mut ov, &rain_layer(8, 8), 4);
        assert_eq!(ink_count(&ov), 4);
        for (x, y) in [(0u32, 0u32), (4, 0), (0, 4), (4, 4)] {
            assert!(ov.get(x, y).is_some(), "ブロック原点({x},{y})に点が無い");
        }
        // spacing=8 なら 8x8 全体で1ブロック=1点(設計の既定値。braille セル8個に1点)。
        let mut ov8 = OverlayLayer::new(8, 8);
        stipple_rgba_into_overlay(&mut ov8, &rain_layer(8, 8), 8);
        assert_eq!(ink_count(&ov8), 1);
    }

    // Bayer(4x4周期)より遥かに疎になること。同じ面を覆っても braille のセルが単色に潰れない。
    #[test]
    fn stipple_is_far_sparser_than_the_bayer_dither() {
        let layer = rain_layer(16, 16);
        let mut dither = OverlayLayer::new(16, 16);
        ink_radar_into_overlay(&mut dither, &layer, 32, 0.35); // 最も薄いディザ
        let mut stipple = OverlayLayer::new(16, 16);
        stipple_rgba_into_overlay(&mut stipple, &layer, 8);
        assert_eq!(ink_count(&stipple), 4, "16x16 を spacing=8 で覆うと4点");
        assert!(ink_count(&stipple) * 8 < ink_count(&dither), "{} vs {}", ink_count(&stipple), ink_count(&dither));
    }

    #[test]
    fn stipple_never_places_a_dot_on_a_transparent_pixel() {
        // 全面透明なら1点も置かない。
        let mut ov = OverlayLayer::new(8, 8);
        stipple_rgba_into_overlay(&mut ov, &RgbaImage::from_pixel(8, 8, image::Rgba([0, 65, 255, 0])), 4);
        assert_eq!(ink_count(&ov), 0);
        // ブロック内に不透明画素が1つだけあるとき、点はその画素の上に置かれる(原点ではなく)。
        // 1px幅の縁取りが間引きで消えないようにするための性質。
        let mut layer = RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 0]));
        layer.put_pixel(3, 2, image::Rgba([9, 8, 7, 255]));
        let mut ov2 = OverlayLayer::new(8, 8);
        stipple_rgba_into_overlay(&mut ov2, &layer, 8);
        assert_eq!(ink_count(&ov2), 1);
        assert_eq!(ov2.get(3, 2), Some([9, 8, 7]));
        assert_eq!(ov2.get(0, 0), None);
    }

    #[test]
    fn stipple_uses_the_layer_colour() {
        let mut layer = RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 0]));
        layer.put_pixel(0, 0, image::Rgba([70, 130, 245, 170])); // 風水害の色
        let mut ov = OverlayLayer::new(4, 4);
        stipple_rgba_into_overlay(&mut ov, &layer, 4);
        assert_eq!(ov.get(0, 0), Some([70, 130, 245]), "アルファは落として色だけ使う");
    }

    #[test]
    fn stipple_handles_odd_spacings_and_mismatched_dimensions() {
        // spacing=0 は 1 として扱う(0除算・無限ループを作らない)。
        let mut ov = OverlayLayer::new(4, 4);
        stipple_rgba_into_overlay(&mut ov, &rain_layer(4, 4), 0);
        assert_eq!(ink_count(&ov), 16);
        // 端数ブロック(8 を spacing=3 で割ると 3+3+2)でもはみ出さない。
        let mut ov2 = OverlayLayer::new(8, 8);
        stipple_rgba_into_overlay(&mut ov2, &rain_layer(8, 8), 3);
        assert_eq!(ink_count(&ov2), 9, "3x3 ブロック");
        // 寸法違い・空レイヤでパニックしない。
        let mut ov3 = OverlayLayer::new(4, 4);
        stipple_rgba_into_overlay(&mut ov3, &rain_layer(10, 10), 4);
        assert_eq!(ink_count(&ov3), 1);
        let mut ov4 = OverlayLayer::new(4, 4);
        stipple_rgba_into_overlay(&mut ov4, &rain_layer(2, 1), 4);
        assert_eq!(ink_count(&ov4), 1);
        let mut ov5 = OverlayLayer::new(4, 4);
        stipple_rgba_into_overlay(&mut ov5, &RgbaImage::new(0, 0), 4);
        assert_eq!(ink_count(&ov5), 0);
    }

    // ---- fill_rings_rgba / stroke_rings_rgba(市区町村の塗り) ----

    fn blank_rgba(w: u32, h: u32) -> RgbaImage { RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 0])) }
    fn painted(img: &RgbaImage) -> usize { img.pixels().filter(|p| p[3] > 0).count() }
    // 閉じていない矩形(末尾と先頭は暗黙に結ばれる)。
    fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    #[test]
    fn fill_paints_the_inside_of_a_rectangle() {
        let mut img = blank_rgba(10, 10);
        fill_rings_rgba(&mut img, &[rect(2, 2, 6, 6)], [1, 2, 3, 200]);
        assert_eq!(*img.get_pixel(4, 4), image::Rgba([1, 2, 3, 200]), "内側");
        assert_eq!(*img.get_pixel(0, 0), image::Rgba([0, 0, 0, 0]), "外側は触らない");
        assert_eq!(*img.get_pixel(9, 9), image::Rgba([0, 0, 0, 0]));
        // 半開区間規則により、辺の片側だけを含む(隣接区域で同じ画素を二重に塗らない)。
        assert_eq!(painted(&img), 5 * 4, "x=2..=6 の5列 × y=2..5 の4行");
    }

    #[test]
    fn fill_leaves_a_hole_unpainted() {
        // 外周(1..9)の中に穴(4..6)。even-odd なので並べるだけで穴が抜ける。
        let mut img = blank_rgba(12, 12);
        fill_rings_rgba(&mut img, &[rect(1, 1, 9, 9), rect(4, 4, 6, 6)], [9, 9, 9, 255]);
        assert_eq!(img.get_pixel(2, 2)[3], 255, "ドーナツ部分は塗る");
        assert_eq!(img.get_pixel(5, 5)[3], 0, "穴は抜ける");
        assert_eq!(img.get_pixel(8, 8)[3], 255, "穴の反対側も塗る");
    }

    #[test]
    fn fill_paints_every_island_of_a_multipolygon() {
        let mut img = blank_rgba(20, 20);
        fill_rings_rgba(&mut img, &[rect(1, 1, 4, 4), rect(12, 12, 16, 16)], [5, 5, 5, 255]);
        assert_eq!(img.get_pixel(2, 2)[3], 255);
        assert_eq!(img.get_pixel(14, 14)[3], 255);
        assert_eq!(img.get_pixel(8, 8)[3], 0, "島と島の間は塗らない");
    }

    #[test]
    fn fill_clips_a_polygon_that_hangs_off_the_edge() {
        let mut img = blank_rgba(8, 8);
        fill_rings_rgba(&mut img, &[rect(-100, -100, 4, 4)], [7, 7, 7, 255]);
        assert_eq!(img.get_pixel(0, 0)[3], 255);
        assert_eq!(img.get_pixel(3, 3)[3], 255);
        assert_eq!(img.get_pixel(6, 6)[3], 0);
    }

    #[test]
    fn fill_writes_nothing_for_a_polygon_that_is_entirely_off_screen() {
        for r in [rect(-50, -50, -10, -10), rect(100, 100, 200, 200), rect(-50, 2, -10, 6), rect(2, -50, 6, -10)] {
            let mut img = blank_rgba(8, 8);
            fill_rings_rgba(&mut img, std::slice::from_ref(&r), [1, 1, 1, 255]);
            assert_eq!(painted(&img), 0, "{r:?}");
        }
    }

    // 頂点をちょうど通る走査線で交点を二重に数えると、その行だけ塗りが裏返る(縞になる)。
    #[test]
    fn fill_does_not_flip_on_a_scanline_through_a_vertex() {
        // 菱形。y=4 の走査線が左右の頂点(0,4)/(8,4)をちょうど通る。
        let mut img = blank_rgba(12, 12);
        fill_rings_rgba(&mut img, &[vec![(4, 0), (8, 4), (4, 8), (0, 4)]], [3, 3, 3, 255]);
        assert_eq!(img.get_pixel(4, 4)[3], 255, "中心が抜けている(交点の二重計上)");
        assert_eq!(img.get_pixel(4, 2)[3], 255);
        assert_eq!(img.get_pixel(4, 6)[3], 255);
        assert_eq!(img.get_pixel(0, 0)[3], 0, "菱形の外");
        // 塗られた画素が y ごとに1本の連続した区間になっている(縞や飛びが無い)。
        for y in 1..8u32 {
            let xs: Vec<u32> = (0..12).filter(|&x| img.get_pixel(x, y)[3] > 0).collect();
            assert!(!xs.is_empty(), "y={y} が空");
            assert_eq!(xs.last().unwrap() - xs[0] + 1, xs.len() as u32, "y={y} が連続していない: {xs:?}");
        }
    }

    #[test]
    fn fill_ignores_degenerate_rings_without_panicking() {
        let mut img = blank_rgba(8, 8);
        fill_rings_rgba(&mut img, &[], [1, 1, 1, 255]);
        fill_rings_rgba(&mut img, &[Vec::new()], [1, 1, 1, 255]);
        fill_rings_rgba(&mut img, &[vec![(1, 1)]], [1, 1, 1, 255]);
        fill_rings_rgba(&mut img, &[vec![(1, 1), (5, 5)]], [1, 1, 1, 255]);
        assert_eq!(painted(&img), 0, "面を持たないリングは何も塗らない");
        // 0寸法の画像でもパニックしない。
        let mut empty = blank_rgba(0, 0);
        fill_rings_rgba(&mut empty, &[rect(0, 0, 4, 4)], [1, 1, 1, 255]);
        // まともなリングに潰れたリングが混ざっても、塗りは壊れない。
        let mut img2 = blank_rgba(8, 8);
        fill_rings_rgba(&mut img2, &[rect(1, 1, 5, 5), vec![(2, 2), (3, 3)]], [1, 1, 1, 255]);
        assert_eq!(img2.get_pixel(3, 3)[3], 255);
    }

    #[test]
    fn stroke_draws_the_outline_and_leaves_the_inside_alone() {
        let mut img = blank_rgba(12, 12);
        stroke_rings_rgba(&mut img, &[rect(2, 2, 8, 8)], [4, 5, 6, 255], 1);
        assert_eq!(*img.get_pixel(2, 2), image::Rgba([4, 5, 6, 255]), "角");
        assert_eq!(img.get_pixel(5, 2)[3], 255, "上辺");
        assert_eq!(img.get_pixel(2, 5)[3], 255, "左辺");
        assert_eq!(img.get_pixel(8, 8)[3], 255, "リングは閉じている(末尾→先頭も結ぶ)");
        assert_eq!(img.get_pixel(5, 5)[3], 0, "内側は塗らない");
        assert_eq!(img.get_pixel(0, 0)[3], 0, "外側も塗らない");
    }

    #[test]
    fn stroke_thickness_widens_the_line() {
        let mut thin = blank_rgba(12, 12);
        stroke_rings_rgba(&mut thin, &[rect(2, 2, 8, 8)], [1, 1, 1, 255], 1);
        let mut thick = blank_rgba(12, 12);
        stroke_rings_rgba(&mut thick, &[rect(2, 2, 8, 8)], [1, 1, 1, 255], 2);
        assert!(painted(&thick) > painted(&thin));
    }

    #[test]
    fn stroke_clips_and_never_panics() {
        let mut img = blank_rgba(8, 8);
        stroke_rings_rgba(&mut img, &[rect(-100, -100, 100, 100)], [1, 1, 1, 255], 1);
        assert_eq!(painted(&img), 0, "画面をまたぐ辺しかない矩形は輪郭が画面外");
        let mut img2 = blank_rgba(8, 8);
        stroke_rings_rgba(&mut img2, &[Vec::new(), vec![(1, 1)]], [1, 1, 1, 255], 1);
        assert_eq!(painted(&img2), 0);
        let mut empty = blank_rgba(0, 0);
        stroke_rings_rgba(&mut empty, &[rect(0, 0, 4, 4)], [1, 1, 1, 255], 1);
    }

    // 塗り → 縁取り の順で呼ぶと、縁取りが塗りの上に乗る(choropleth.rs の呼び順)。
    #[test]
    fn stroke_over_fill_keeps_the_outline_visible() {
        let mut img = blank_rgba(12, 12);
        let rings = [rect(2, 2, 8, 8)];
        fill_rings_rgba(&mut img, &rings, [10, 10, 10, 100]);
        stroke_rings_rgba(&mut img, &rings, [20, 20, 20, 255], 1);
        assert_eq!(*img.get_pixel(2, 5), image::Rgba([20, 20, 20, 255]), "縁は縁取りの色");
        assert_eq!(*img.get_pixel(5, 5), image::Rgba([10, 10, 10, 100]), "内側は塗りの色のまま");
    }

    // classify(量子化)と雨雲合成の順序。recolor の「後」に混ぜないと、淡い青の降水が
    // classify() の水域条件に合致して湖(Cat::Water)の色に化ける(設計 §8.4)。
    #[test]
    fn classify_must_blend_radar_after_recolor() {
        let base = RgbImage::from_pixel(2, 2, image::Rgb([245, 245, 245])); // 何にも分類されない下地
        let rain = RgbaImage::from_pixel(2, 2, image::Rgba([160, 210, 255, 255])); // 弱い雨(淡い青)

        let mut correct = recolor(&base); // 量子化 → 合成(正しい順序)
        blend_rgba_over(&mut correct, &rain, 1.0);
        assert_eq!(*correct.get_pixel(0, 0), image::Rgb([160, 210, 255]), "降水の色がそのまま残るはず");

        let mut mixed = base.clone(); // 合成 → 量子化(誤った順序)
        blend_rgba_over(&mut mixed, &rain, 1.0);
        let wrong = recolor(&mixed);
        assert_eq!(*wrong.get_pixel(0, 0), image::Rgb(cat_color(Cat::Water).into()), "誤順序では湖に化ける");
    }

    // xterm256の既知の代表色(黒/白/純色RGB)が期待indexへ変換されること。
    #[test]
    fn rgb_to_ansi256_known_colors() {
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);       // 黒(グレースケール下限)
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231); // 白(カラーキューブ上限)
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);     // 純赤
        assert_eq!(rgb_to_ansi256(0, 255, 0), 46);      // 純緑
        assert_eq!(rgb_to_ansi256(0, 0, 255), 21);      // 純青
    }

    // グレースケール(R≈G≈B)は24階調グレー領域(232〜255)に落ちる。
    #[test]
    fn rgb_to_ansi256_grayscale_uses_gray_ramp() {
        let idx = rgb_to_ansi256(128, 128, 128);
        assert!((232..=255).contains(&idx), "mid-gray should map into the gray ramp, got {idx}");
    }

    // 近い色は同じか隣接indexになる(量子化が単調であることの簡易確認)。
    #[test]
    fn rgb_to_ansi256_nearby_colors_map_close() {
        let a = rgb_to_ansi256(100, 150, 200);
        let b = rgb_to_ansi256(102, 148, 198);
        assert!(a.abs_diff(b) <= 1, "a={a} b={b} should be identical or adjacent");
    }

    // 全モジュールfalse(光)なら、静穏領域込みの全面が白になる。
    #[test]
    fn render_qr_image_all_light_is_all_white() {
        let dark = vec![false; 4]; // 2x2
        let img = render_qr_image(&dark, 2, 3, 1); // module_px=3, quiet=1 → side=(2+2)*3=12
        assert_eq!(img.dimensions(), (12, 12));
        assert!(img.pixels().all(|p| *p == image::Rgb([255, 255, 255])));
    }

    // 全モジュールtrueなら、静穏領域(quiet_modules分)は白のまま残り、内側は黒で埋まる。
    #[test]
    fn render_qr_image_all_dark_fills_inside_quiet_zone() {
        let dark = vec![true; 4]; // 2x2
        let img = render_qr_image(&dark, 2, 2, 1); // module_px=2, quiet=1 → side=(2+2)*2=8
        assert_eq!(img.dimensions(), (8, 8));
        // 静穏領域(外周quiet_modules*module_px=2px)は白
        assert_eq!(*img.get_pixel(0, 0), image::Rgb([255, 255, 255]));
        assert_eq!(*img.get_pixel(1, 1), image::Rgb([255, 255, 255]));
        // モジュール本体(内側)は黒
        assert_eq!(*img.get_pixel(2, 2), image::Rgb([0, 0, 0]));
        assert_eq!(*img.get_pixel(5, 5), image::Rgb([0, 0, 0]));
    }

    // 1モジュールだけdarkな場合、そのモジュールの領域だけが黒くなり、他は白のまま。
    #[test]
    fn render_qr_image_single_dark_module_is_localized() {
        let dark = vec![true, false, false, false]; // (0,0)のみdark
        let img = render_qr_image(&dark, 2, 4, 0); // module_px=4, quiet=0 → side=8
        assert_eq!(img.dimensions(), (8, 8));
        // 左上モジュール(0..4, 0..4)は黒
        assert_eq!(*img.get_pixel(0, 0), image::Rgb([0, 0, 0]));
        assert_eq!(*img.get_pixel(3, 3), image::Rgb([0, 0, 0]));
        // 右下モジュール(4..8, 4..8)は白のまま
        assert_eq!(*img.get_pixel(4, 4), image::Rgb([255, 255, 255]));
        assert_eq!(*img.get_pixel(7, 7), image::Rgb([255, 255, 255]));
    }

    // 画像1辺のピクセル数は (width + quiet_modules*2) * module_px になる。
    #[test]
    fn render_qr_image_dimensions_match_formula() {
        let dark = vec![true; 25]; // 5x5
        let img = render_qr_image(&dark, 5, 4, 4);
        let expected = (5 + 4 * 2) * 4; // 52
        assert_eq!(img.dimensions(), (expected, expected));
    }

    // width=0かつquiet_modules=0(=side_mod=0)でもpanicせず1x1の白画像を返す(0px画像を作らない安全側)。
    #[test]
    fn render_qr_image_zero_side_does_not_panic() {
        let img = render_qr_image(&[], 0, 4, 0);
        assert_eq!(img.dimensions(), (1, 1));
        assert_eq!(*img.get_pixel(0, 0), image::Rgb([255, 255, 255]));
    }

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
        // size= の値はPNGバイト数に依存するため固定文字列にはできない。整数として付いていて
        // 後続が期待通りかだけを見る(size省略はaddon-image側の即中断バグを再発させる)。
        let rest = s.strip_prefix("\x1b]1337;File=inline=1;size=").unwrap();
        let size_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        assert!(!size_str.is_empty());
        assert!(rest[size_str.len()..].starts_with(";width=4;height=3;preserveAspectRatio=0:"));
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
        let rest = s.strip_prefix("\x1b]1337;File=inline=1;size=").unwrap();
        let size_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let declared_size: usize = size_str.parse().unwrap();
        let b64 = rest[size_str.len()..]
            .strip_prefix(";width=3;height=2;preserveAspectRatio=0:")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        let png = base64_decode(b64);
        assert_eq!(png.len(), declared_size); // size= が実バイト数と一致すること(addon-image側はこれで即中断するかが決まる)
        let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (3, 2));
        assert_eq!(decoded.get_pixel(0, 0), &image::Rgb([255, 0, 0]));
        assert_eq!(decoded.get_pixel(2, 1), &image::Rgb([0, 128, 255]));
    }
}
