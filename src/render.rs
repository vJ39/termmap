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
        // SGR は端末側の「状態」なので、直前のセルと同じ色なら発行しなくても同じ絵になる
        // (設計 docs/web-pan-smoothness-design.md §5.3 対策C-1)。地図は同色の面が広いので、
        // 見た目を1ドットも変えずに1フレームのバイト数が減り、そのぶんコマ数が増える。
        // 行末で \x1b[0m を出しているため、状態は行内に閉じる(行頭で必ず None から始める)。
        let mut last_fg: Option<(u8, u8, u8)> = None;
        let mut last_bg: Option<(u8, u8, u8)> = None;
        for x in 0..w {
            let t = img.get_pixel(x, y);
            let b = img.get_pixel(x, y + 1);
            let fg = (t[0], t[1], t[2]);
            let bg = (b[0], b[1], b[2]);
            if last_fg != Some(fg) {
                out.push_str(&sgr_fg(fg.0, fg.1, fg.2, truecolor));
                last_fg = Some(fg);
            }
            if last_bg != Some(bg) {
                out.push_str(&sgr_bg(bg.0, bg.1, bg.2, truecolor));
                last_bg = Some(bg);
            }
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
        // halfblock と同じ SGR 重複除去(設計 §5.3 対策C-1)。braille は背景を使わないので前景だけ。
        // インクの無いセルは空白1バイトを置くだけで前景色の状態を変えないため、空白を挟んで
        // 同じ色が続く場合も発行を省ける。行末の \x1b[0m にあわせて行頭で状態をリセットする。
        let mut last_fg: Option<(u8, u8, u8)> = None;
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
            else {
                // 色の決め方は従来どおり(オーバーレイ > classify > 平均色)。SGR を出すかどうかだけを
                // 直前のセルとの比較で決めるため、まず色を確定させてから発行を判断する。
                let (r, g, b) = if ovn > 0 {
                    ((ovr / ovn) as u8, (ovg / ovn) as u8, (ovb / ovn) as u8)
                } else if classify_on {
                    let bi = (0..6).max_by_key(|&i| cc[i]).unwrap();
                    cat_color([Cat::Water, Cat::Park, Cat::RoadMajor, Cat::Rail, Cat::Building, Cat::Other][bi])
                } else {
                    // braille はインク=暗い画素の平均色になりがちで沈むので輝度を持ち上げる
                    let br = |s: u32| ((s as f64 / n as f64) * 1.6).min(255.0) as u8;
                    (br(sr), br(sg), br(sb))
                };
                if last_fg != Some((r, g, b)) {
                    out.push_str(&sgr_fg(r, g, b, truecolor));
                    last_fg = Some((r, g, b));
                }
                out.push(ch);
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
// radar は braille/edge 経路の雨雲インク(None なら従来と同じ)。最背面に最初に焼く。
pub fn build_overlay(spec: &OverlaySpec, cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32,
                 scale_x: f64, scale_y: f64, out_w: u32, out_h: u32,
                 radar: Option<RadarInk>) -> OverlayLayer {
    let mut ov = OverlayLayer::new(out_w, out_h);
    // 雨雲(最背面)。リング/経路/道路/POI/スポットはこの後に描かれるので、常に雨雲より前面になる。
    if let Some(r) = radar { ink_radar_into_overlay(&mut ov, r.layer, RADAR_INK_MIN_ALPHA, r.density); }
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

// braille/edge 経路で雨雲を焼くときの指定。build_overlay へ渡す。
// layer は build_radar_window_nowait が返す降水レイヤ(降水なし=透明)、density はディザの密度。
pub struct RadarInk<'a> { pub layer: &'a RgbaImage, pub density: f64 }

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
        let ink = RadarInk { layer: &layer, density: 1.0 };
        let ov = build_overlay(&spec, cx, cy, z, 8, 8, 1.0, 1.0, 8, 8, Some(ink));
        assert_eq!(ov.get(4, 4), Some([1, 2, 3]));    // 窓中心のマーカーが雨雲を上書きしている
        assert_eq!(ov.get(0, 0), Some([200, 0, 0]));  // マーカーの無い所は雨雲のインク

        // radar=None なら従来どおり(マーカー以外は何も置かれない)。
        let ov2 = build_overlay(&spec, cx, cy, z, 8, 8, 1.0, 1.0, 8, 8, None);
        assert_eq!(ov2.get(4, 4), Some([1, 2, 3]));
        assert_eq!(ov2.get(0, 0), None);
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

    // ---- SGR 重複除去 (docs/web-pan-smoothness-design.md §5.3 対策C-1) ----

    /// ANSI を解釈した結果の1セル。SGR は「状態」なので、重複を省いても復号結果が同じなら
    /// 端末に見える絵は変わっていない。
    #[derive(Clone, PartialEq, Debug)]
    struct Cell {
        fg: Option<String>,
        bg: Option<String>,
        ch: char,
    }

    /// 描画結果を行ごとのセル列へ復号する。想定外のエスケープが混ざったら panic して気づけるようにする。
    fn decode_cells(s: &str) -> Vec<Vec<Cell>> {
        let mut rows: Vec<Vec<Cell>> = Vec::new();
        let mut row: Vec<Cell> = Vec::new();
        let (mut fg, mut bg): (Option<String>, Option<String>) = (None, None);
        let mut it = s.chars();
        while let Some(c) = it.next() {
            match c {
                '\x1b' => {
                    assert_eq!(it.next(), Some('['), "CSI 以外のエスケープは出さない");
                    let mut params = String::new();
                    for d in it.by_ref() {
                        if d == 'm' {
                            break;
                        }
                        params.push(d);
                    }
                    if params == "0" {
                        fg = None;
                        bg = None;
                    } else if params.starts_with("38;") {
                        fg = Some(params);
                    } else if params.starts_with("48;") {
                        bg = Some(params);
                    } else {
                        panic!("想定外のSGR: {params:?}");
                    }
                }
                '\r' => {
                    assert_eq!(it.next(), Some('\n'), "改行は CRLF");
                    rows.push(std::mem::take(&mut row));
                }
                _ => row.push(Cell { fg: fg.clone(), bg: bg.clone(), ch: c }),
            }
        }
        if !row.is_empty() {
            rows.push(row);
        }
        rows
    }

    /// 復号したセル列から「除去前」の出力を組み直す。全セルで無条件に SGR を発行していた
    /// 従来の形に戻したもので、削減バイト数を測る基準にする。braille の色決定ロジックを
    /// テストへ写して二重管理にしないよう、実出力の復号結果から作る。
    /// インクの無いセル(空白)は従来も SGR を出していないので、そこは発行しない。
    fn reencode_without_dedup(rows: &[Vec<Cell>]) -> String {
        let mut out = String::new();
        for row in rows {
            for c in row {
                if c.ch != ' ' {
                    if let Some(fg) = &c.fg {
                        out.push_str("\x1b[");
                        out.push_str(fg);
                        out.push('m');
                    }
                    if let Some(bg) = &c.bg {
                        out.push_str("\x1b[");
                        out.push_str(bg);
                        out.push('m');
                    }
                }
                out.push(c.ch);
            }
            out.push_str("\x1b[0m\r\n");
        }
        out
    }

    /// 地図らしい画像(広い同色の面 + 少しの模様)。実地図に近い形で削減量を測る。
    fn maplike_image(w: u32, h: u32) -> RgbImage {
        let mut img = RgbImage::from_pixel(w, h, image::Rgb([242, 239, 233])); // 下地
        for y in 0..h {
            for x in 0..w {
                if y == h / 3 || y == h / 3 + 1 {
                    img.put_pixel(x, y, image::Rgb([240, 200, 70])); // 横に走る道路
                }
                if x == w / 2 {
                    img.put_pixel(x, y, image::Rgb([240, 200, 70])); // 縦に走る道路
                }
                if x < w / 5 && y > h * 2 / 3 {
                    img.put_pixel(x, y, image::Rgb([86, 170, 222])); // 水域
                }
            }
        }
        img
    }

    // 重複を省いても、復号したセル色列は元画像から直接決まる色と完全に一致する(見た目が変わらない)。
    #[test]
    fn halfblock_dedup_keeps_the_same_visible_cells() {
        let img = maplike_image(40, 20);
        let rows = decode_cells(&render_halfblock(&img, true));
        assert_eq!(rows.len(), 10, "1セル=2画素なので10行");
        for (ry, row) in rows.iter().enumerate() {
            assert_eq!(row.len(), 40);
            for (x, cell) in row.iter().enumerate() {
                let t = img.get_pixel(x as u32, ry as u32 * 2);
                let b = img.get_pixel(x as u32, ry as u32 * 2 + 1);
                let want_fg = format!("38;2;{};{};{}", t[0], t[1], t[2]);
                let want_bg = format!("48;2;{};{};{}", b[0], b[1], b[2]);
                assert_eq!(cell.ch, '\u{2580}');
                assert_eq!(cell.fg.as_deref(), Some(want_fg.as_str()), "({x},{ry}) の前景");
                assert_eq!(cell.bg.as_deref(), Some(want_bg.as_str()), "({x},{ry}) の背景");
            }
        }
    }

    // 前景と背景は独立に追跡する。前景が同じでも背景が変われば背景だけ再発行される。
    #[test]
    fn halfblock_dedup_tracks_foreground_and_background_independently() {
        let mut img = RgbImage::new(2, 2);
        img.put_pixel(0, 0, image::Rgb([10, 10, 10]));
        img.put_pixel(1, 0, image::Rgb([10, 10, 10])); // 上半分は同色
        img.put_pixel(0, 1, image::Rgb([20, 20, 20]));
        img.put_pixel(1, 1, image::Rgb([30, 30, 30])); // 下半分だけ変わる
        let out = render_halfblock(&img, true);
        assert_eq!(out.matches("\x1b[38;2;10;10;10m").count(), 1, "前景は同色なので1回: {out:?}");
        assert_eq!(out.matches("\x1b[48;2;20;20;20m").count(), 1);
        assert_eq!(out.matches("\x1b[48;2;30;30;30m").count(), 1);
    }

    // 行末で \x1b[0m を出しているので、状態は行内に閉じる。次の行は同じ色でも必ず再発行する。
    #[test]
    fn halfblock_resets_sgr_state_at_every_row() {
        let img = RgbImage::from_pixel(1, 4, image::Rgb([1, 2, 3])); // 1桁×2行すべて同色
        let out = render_halfblock(&img, true);
        assert_eq!(out.matches("\x1b[38;2;1;2;3m").count(), 2, "行またぎでは再発行: {out:?}");
        assert_eq!(out.matches("\x1b[48;2;1;2;3m").count(), 2);
    }

    #[test]
    fn halfblock_dedup_shortens_output() {
        let img = maplike_image(94, 44); // §2.2 の計測と同じ 94桁×22行ぶん
        let out = render_halfblock(&img, true);
        let naive = reencode_without_dedup(&decode_cells(&out));
        let cut = 100.0 * (naive.len() - out.len()) as f64 / naive.len() as f64;
        println!("halfblock: 除去前 {} B → 除去後 {} B ({cut:.1}% 削減)", naive.len(), out.len());
        assert!(out.len() < naive.len(), "除去後 {} / 除去前 {}", out.len(), naive.len());
    }

    // インクのあるセルで同じ色が続くなら前景SGRは1回。色が変われば都度発行する。
    #[test]
    fn braille_dedup_emits_one_sgr_per_color_run() {
        let mut img = RgbImage::from_pixel(4, 4, image::Rgb([0, 0, 0])); // 2セル分・全ドットON
        let same = render_braille(&img, false, false, 250, false, None, true);
        assert_eq!(same.matches("\x1b[38;2;").count(), 1, "同色の2セルは1回: {same:?}");
        for y in 0..4 {
            for x in 2..4 {
                img.put_pixel(x, y, image::Rgb([0, 0, 40])); // 右のセルだけ色を変える
            }
        }
        let diff = render_braille(&img, false, false, 250, false, None, true);
        assert_eq!(diff.matches("\x1b[38;2;").count(), 2, "色が変われば2回: {diff:?}");
    }

    // インクの無いセルは空白1バイトで前景色の状態を変えない。空白を挟んでも同色なら1回でよい。
    #[test]
    fn braille_blank_cells_do_not_break_dedup() {
        let mut img = RgbImage::from_pixel(6, 4, image::Rgb([255, 255, 255])); // 明るい = インク無し
        for y in 0..4 {
            img.put_pixel(0, y, image::Rgb([0, 0, 0])); // 1セル目にインク
            img.put_pixel(4, y, image::Rgb([0, 0, 0])); // 3セル目に同じ色のインク
        }
        let out = render_braille(&img, false, false, 128, false, None, true);
        assert_eq!(out.matches(' ').count(), 1, "中央はインク無しの空白1セル: {out:?}");
        assert_eq!(out.matches("\x1b[38;2;").count(), 1, "空白を挟んでも同色なら1回: {out:?}");
    }

    #[test]
    fn braille_resets_sgr_state_at_every_row() {
        let img = RgbImage::from_pixel(2, 8, image::Rgb([0, 0, 0])); // 1桁×2行すべて同色
        let out = render_braille(&img, false, false, 250, false, None, true);
        assert_eq!(out.matches("\x1b[38;2;0;0;0m").count(), 2, "行またぎでは再発行: {out:?}");
    }

    // mono は SGR を一切出さない経路。重複除去を入れても出力は変わらない。
    #[test]
    fn braille_mono_emits_no_sgr() {
        let img = maplike_image(20, 8);
        let out = render_braille(&img, true, false, 128, false, None, true);
        assert!(!out.contains('\x1b'), "mono ではエスケープを出さない: {out:?}");
    }

    #[test]
    fn braille_dedup_shortens_output() {
        let img = maplike_image(94, 44);
        // 閾値220: 下地(輝度239)はインク無し、道路(197)と水域(151)がインクになる。
        // 既定の128だとこの画像は全セルがインク無し=空白だけになり、何も測れない。
        let out = render_braille(&img, false, false, 220, false, None, true);
        let naive = reencode_without_dedup(&decode_cells(&out));
        let cut = 100.0 * (naive.len() - out.len()) as f64 / naive.len() as f64;
        println!("braille: 除去前 {} B → 除去後 {} B ({cut:.1}% 削減)", naive.len(), out.len());
        assert!(out.len() < naive.len(), "除去後 {} / 除去前 {}", out.len(), naive.len());
    }

    // truecolor 非対応端末(256色)でも、量子化後の色列は変わらない。
    #[test]
    fn dedup_keeps_visible_cells_in_256_color_mode() {
        let img = maplike_image(40, 20);
        let rows = decode_cells(&render_halfblock(&img, false));
        for (ry, row) in rows.iter().enumerate() {
            for (x, cell) in row.iter().enumerate() {
                let t = img.get_pixel(x as u32, ry as u32 * 2);
                let b = img.get_pixel(x as u32, ry as u32 * 2 + 1);
                let want_fg = format!("38;5;{}", rgb_to_ansi256(t[0], t[1], t[2]));
                let want_bg = format!("48;5;{}", rgb_to_ansi256(b[0], b[1], b[2]));
                assert_eq!(cell.fg.as_deref(), Some(want_fg.as_str()), "({x},{ry}) の前景");
                assert_eq!(cell.bg.as_deref(), Some(want_bg.as_str()), "({x},{ry}) の背景");
            }
        }
    }
}
