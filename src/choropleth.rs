// 過去災害のコロプレス(市区町村の境界ポリゴンを記録の多さで塗り分ける)の組み立て。
// 設計は docs/disaster-choropleth-design.md §3〜§5。
//
// 「どの市区町村を何色で塗るか」だけを持ち、描画そのものは render.rs のプリミティブへ、
// 幾何は geopoly.rs へ、境界データは muni.rs へ、件数は disaster.rs へ委ねる。
// ui.rs へ持ち込まないのは、あちらが既に2,600行あって並列作業のぶつかりどころになっているため。
//
// 色の軸は「最も件数の多い災害種別の色 × 件数のアルファ5段」。マーカーが持っていた情報
// (色=最多種別・大きさ=件数)を1つも落とさずに点から面へ移す。実データの9割近くが風水害なので、
// 実際には画面のほぼ全部が青系の濃淡になり、色相が変わるのは地震・雪氷が上回っている土地だけになる。

use crate::disaster::{self, DisasterKind, DisasterSite};
use crate::geo::{deg_to_pixel, pixel_to_deg};
use crate::geopoly;
use crate::muni::MuniArea;
use crate::render::fill_rings_rgba;
use image::RgbaImage;
use std::collections::HashMap;

/// 塗りの既定の濃さ(地図へ合成するときの不透明度)。
/// 雨雲(既定0.55)より弱くするのは、雨雲が降っている場所だけを覆うのに対し、コロプレスは
/// 常に画面の大半を覆うため。同じ濃さだと下の地図が読めなくなる。
pub const DEFAULT_OPACITY: f64 = 0.45;

/// braille/edge で面を点描にするときの、**最も濃い階級での**間隔(画素)。
/// 薄い階級ほど render 側で間隔が広がる(6 / 9 / 12 / 18)ので、件数が点の密度として読める。
/// 固定間隔8から6へ下げるのは、階級を疎らな側へ広げるぶん最も濃い側を詰めて全体の見え方を保つため
/// (docs/disaster-choropleth-wide-zoom-design.md §3.2)。
/// 6なら 6x6=36画素に1点で、braille のセル(2x4画素)4〜5個に1点に収まる。
pub const STIPPLE_SPACING: u32 = 6;

/// 塗りをどう出すか。`opacity` はこのモジュールでは使わず、呼び出し側が
/// `render::blend_rgba_over` / `InkLayer` へ渡す濃さとして持ち回る
/// (層の中身は濃さに依存しないので、濃さを変えてもラスタライズし直す必要が無い)。
///
/// 硬い輪郭線(旧・stroke_rings_rgba)は廃止した。市区町村1区域が19px程度まで縮む広域
/// ズームでは、1px幅の縁取りだけで面積の1割前後を占めてしまい、輪郭が塗りより目立って
/// しまう(docs/disaster-choropleth-wide-zoom-design.md §2.5)。代わりに`blur_radius`で
/// 塗り自体をボックスブラーし、隣接区域の境界を滲ませて見せる(フォントのアンチエイリアシングと
/// 同じ考え方。docs/disaster-choropleth-unlimited-zoom-design.md §4)。0ならブラー無し。
pub struct Shading {
    pub opacity: f64,
    pub fill: bool,
    pub blur_radius: u32,
}

/// 5桁の市区町村コード → (件数, 最多種別)。
///
/// 設計では `areas` も引数に取る形だったが、集計に境界は要らない(コードだけで積める)ので
/// 落とした。呼び出し側は build_layer / area_summary 経由で使う。
///
/// 同じコードに複数の地点が落ちた場合は種別ごとに件数を足し合わせてから最多種別を決める
/// (地点単位で比べると、細かく分かれた種別が合算後の最多と食い違う)。実測では市区町村あたり
/// 代表点は1つだが、重複しても壊れない形にしておく。
pub fn tally(sites: &[&DisasterSite]) -> HashMap<String, (u32, DisasterKind)> {
    let mut merged: HashMap<String, DisasterSite> = HashMap::new();
    for s in sites {
        if s.muni_code.is_empty() {
            continue; // コードを読めなかった地点は塗る先が決まらない(マーカーだけになる)
        }
        let e = merged.entry(s.muni_code.clone()).or_insert_with(|| DisasterSite {
            lat: s.lat,
            lon: s.lon,
            muni_code: s.muni_code.clone(),
            kinds: Vec::new(),
        });
        for k in &s.kinds {
            match e.kinds.iter_mut().find(|x| x.kind == k.kind) {
                Some(x) => {
                    x.count = x.count.saturating_add(k.count);
                    // 年幅は表示に使わないが、混ぜたときに嘘の値にならないよう素直に広げる
                    // (0 は「未記入」の印なので最小側の判定から外す)。
                    if x.year_min == 0 || (k.year_min != 0 && k.year_min < x.year_min) {
                        x.year_min = k.year_min;
                    }
                    x.year_max = x.year_max.max(k.year_max);
                }
                None => e.kinds.push(k.clone()),
            }
        }
    }
    merged.into_iter().map(|(code, s)| (code, (s.total(), s.dominant()))).collect()
}

/// 表示中の市区町村を災害件数で塗った層を1枚作る。塗る対象が無ければ None。
///
/// `cx`/`cy` は視野中心のグローバル画素、`w`/`h` は作る層の画素寸法(=描画に使う画像と同じ)。
/// ラスタライズは地図を組み直すフレームでしか走らない(ui.rs の map_sig)ので、走査線方式の
/// 素直な実装のままにしてある(1区域あたり平均82頂点)。
pub fn build_layer(
    sites: &[&DisasterSite],
    areas: &[&MuniArea],
    cx: f64,
    cy: f64,
    z: u32,
    w: u32,
    h: u32,
    sh: Shading,
) -> Option<RgbaImage> {
    if w == 0 || h == 0 || !sh.fill {
        return None;
    }
    let counts = tally(sites);
    if counts.is_empty() {
        return None;
    }
    let (left, top) = (cx - w as f64 / 2.0, cy - h as f64 / 2.0);
    let view = view_bbox(cx, cy, z, w, h);
    let mut img = RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 0]));
    let mut drew = false;
    for a in areas {
        if !intersects(a.bbox, view) {
            continue; // 視野外(外接矩形で粗く落とす)
        }
        let Some(&(total, kind)) = counts.get(a.muni_code()) else {
            continue; // 記録の無い市区町村は塗らない
        };
        let rings: Vec<Vec<(i32, i32)>> = a
            .rings
            .iter()
            .map(|r| {
                r.iter()
                    .map(|&(lat, lon)| {
                        let (gx, gy) = deg_to_pixel(lat, lon, z);
                        ((gx - left).floor() as i32, (gy - top).floor() as i32)
                    })
                    .collect()
            })
            .collect();
        let c = kind.color();
        fill_rings_rgba(&mut img, &rings, [c[0], c[1], c[2], disaster::fill_alpha(total)]);
        drew = true;
    }
    if !drew {
        return None;
    }
    if sh.blur_radius > 0 {
        img = blur_rgba(&img, sh.blur_radius);
    }
    Some(img)
}

/// ズームに応じたブラー半径(px)。広域ほど区域が小さく縁が荒れて見えるため強めに掛ける。
/// z9未満・fill無効時は呼び出し側で使わない想定なので、範囲外のズームは0(ブラー無し)。
pub fn blur_radius_for_zoom(z: u32) -> u32 {
    match z {
        9 => 3,
        10 => 2,
        11 => 1,
        _ => 0,
    }
}

/// RGBA画像に軽いボックスブラーを掛ける(radius=0はimgをそのまま返す)。事前乗算アルファ
/// (色×アルファ)で平均してから戻すことで、記録の無い市区町村(透明)との境界でも
/// 黒ずんだ縁(暗いハロー)が出ずに自然に滲む。横→縦の分離ブラーなので
/// O(w*h*radius)で済む(市区町村の輪郭線を廃止する代わりに使う。設計 §4.1)。
pub fn blur_rgba(img: &RgbaImage, radius: u32) -> RgbaImage {
    if radius == 0 {
        return img.clone();
    }
    let (w, h) = img.dimensions();
    let premult: Vec<[f64; 4]> = img
        .pixels()
        .map(|p| {
            let a = p[3] as f64 / 255.0;
            [p[0] as f64 * a, p[1] as f64 * a, p[2] as f64 * a, a]
        })
        .collect();
    let h_pass = box_blur_pass(&premult, w as usize, h as usize, radius, true);
    let v_pass = box_blur_pass(&h_pass, w as usize, h as usize, radius, false);
    let mut out = RgbaImage::new(w, h);
    for (i, px) in v_pass.iter().enumerate() {
        let a = px[3];
        let (r, g, b) = if a > 1e-6 { (px[0] / a, px[1] / a, px[2] / a) } else { (0.0, 0.0, 0.0) };
        let x = (i as u32) % w;
        let y = (i as u32) / w;
        out.put_pixel(
            x,
            y,
            image::Rgba([r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, b.clamp(0.0, 255.0) as u8, (a.clamp(0.0, 1.0) * 255.0) as u8]),
        );
    }
    out
}

// 事前乗算済みの4チャンネル(r*a,g*a,b*a,a)配列に、1軸ぶんの単純平均(ボックス)ブラーを掛ける。
fn box_blur_pass(data: &[[f64; 4]], w: usize, h: usize, radius: u32, horizontal: bool) -> Vec<[f64; 4]> {
    let r = radius as i64;
    let mut out = vec![[0.0f64; 4]; data.len()];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0f64; 4];
            let mut count = 0.0f64;
            for d in -r..=r {
                let (xx, yy) = if horizontal { (x as i64 + d, y as i64) } else { (x as i64, y as i64 + d) };
                if xx < 0 || xx >= w as i64 || yy < 0 || yy >= h as i64 {
                    continue;
                }
                let idx = yy as usize * w + xx as usize;
                for c in 0..4 {
                    sum[c] += data[idx][c];
                }
                count += 1.0;
            }
            let idx = y * w + x;
            for c in 0..4 {
                out[idx][c] = sum[c] / count;
            }
        }
    }
    out
}

/// 中心十字がいる市区町村の名前と件数(ステータス行用)。
/// どの区域にも入らない/その区域に記録が無いなら None(呼び出し側は従来の地点数表示に戻る)。
///
/// 境界は簡略化された形なので、これは**目安**であって「ここは○○市の中か」の判定には使わない。
pub fn area_summary(sites: &[&DisasterSite], areas: &[&MuniArea], lat: f64, lon: f64) -> Option<(String, u32)> {
    let mut counts: Option<HashMap<String, (u32, DisasterKind)>> = None;
    for a in areas {
        // 外接矩形で粗く落としてから、リングの内外判定(重い方)へ進む。
        if lat < a.bbox.0 || lat > a.bbox.2 || lon < a.bbox.1 || lon > a.bbox.3 {
            continue;
        }
        if !geopoly::point_in_rings(&a.rings, (lat, lon)) {
            continue;
        }
        // 区域は重ならないので、包む区域が見つかった時点で答えは決まる。
        let c = counts.get_or_insert_with(|| tally(sites));
        return c.get(a.muni_code()).map(|&(total, _)| (a.name.clone(), total));
    }
    None
}

// 表示している画素矩形が覆う緯度経度の範囲 (lat_min, lon_min, lat_max, lon_max)。
fn view_bbox(cx: f64, cy: f64, z: u32, w: u32, h: u32) -> (f64, f64, f64, f64) {
    let (lat_max, lon_min) = pixel_to_deg(cx - w as f64 / 2.0, cy - h as f64 / 2.0, z);
    let (lat_min, lon_max) = pixel_to_deg(cx + w as f64 / 2.0, cy + h as f64 / 2.0, z);
    (lat_min, lon_min, lat_max, lon_max)
}

fn intersects(a: (f64, f64, f64, f64), b: (f64, f64, f64, f64)) -> bool {
    a.0 <= b.2 && a.2 >= b.0 && a.1 <= b.3 && a.3 >= b.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disaster::KindCount;

    fn site(muni: &str, kinds: &[(DisasterKind, u32)]) -> DisasterSite {
        DisasterSite {
            lat: 35.5,
            lon: 139.5,
            muni_code: muni.to_string(),
            kinds: kinds
                .iter()
                .map(|&(kind, count)| KindCount { kind, count, year_min: 1926, year_max: 2019 })
                .collect(),
        }
    }

    // 画面中心(35.5,139.5)のまわりに置く、緯度経度で 0.1 度四方の区域。
    fn area(code: &str, name: &str, lat0: f64, lon0: f64) -> MuniArea {
        let rings = vec![vec![
            (lat0, lon0),
            (lat0, lon0 + 0.1),
            (lat0 + 0.1, lon0 + 0.1),
            (lat0 + 0.1, lon0),
        ]];
        let bbox = geopoly::rings_bbox(&rings).expect("頂点がある");
        MuniArea { code: code.to_string(), name: name.to_string(), rings, bbox }
    }

    fn refs<'a, T>(v: &'a [T]) -> Vec<&'a T> {
        v.iter().collect()
    }

    // z11・320x200画素の窓。中心(35.5,139.55)なら上の区域がだいたい画面に入る。
    fn window() -> (f64, f64, u32, u32, u32) {
        let z = 11;
        let (cx, cy) = deg_to_pixel(35.55, 139.55, z);
        (cx, cy, z, 320, 200)
    }

    fn shading() -> Shading {
        Shading { opacity: DEFAULT_OPACITY, fill: true, blur_radius: 0 }
    }

    #[test]
    fn blur_radius_for_zoom_gets_weaker_as_zoom_increases_then_zero() {
        assert_eq!(blur_radius_for_zoom(9), 3);
        assert_eq!(blur_radius_for_zoom(10), 2);
        assert_eq!(blur_radius_for_zoom(11), 1);
        assert_eq!(blur_radius_for_zoom(12), 0);
        assert_eq!(blur_radius_for_zoom(13), 0);
        assert_eq!(blur_radius_for_zoom(8), 0, "z8はfillの対象外なので値は使われないが0で安全側");
    }

    fn painted(img: &RgbaImage) -> usize {
        img.pixels().filter(|p| p[3] > 0).count()
    }

    // ---- tally ----

    #[test]
    fn tally_keys_the_counts_by_municipality_code() {
        let sites = vec![
            site("12208", &[(DisasterKind::Storm, 60), (DisasterKind::OtherWeather, 10)]),
            site("13205", &[(DisasterKind::Earthquake, 33)]),
        ];
        let t = tally(&refs(&sites));
        assert_eq!(t.len(), 2);
        assert_eq!(t["12208"], (70, DisasterKind::Storm));
        assert_eq!(t["13205"], (33, DisasterKind::Earthquake));
    }

    #[test]
    fn tally_adds_up_several_sites_that_share_a_code() {
        // 同じコードに2地点。種別ごとに足してから最多を決めるので、
        // 地点単位では地震(50)が最多に見えても、合算では風水害(30+30=60)が勝つ。
        let sites = vec![
            site("14100", &[(DisasterKind::Storm, 30)]),
            site("14100", &[(DisasterKind::Storm, 30), (DisasterKind::Earthquake, 50)]),
        ];
        let t = tally(&refs(&sites));
        assert_eq!(t.len(), 1);
        assert_eq!(t["14100"], (110, DisasterKind::Storm));
    }

    #[test]
    fn tally_ignores_sites_without_a_code() {
        let sites = vec![site("", &[(DisasterKind::Storm, 99)]), site("12208", &[(DisasterKind::Storm, 1)])];
        let t = tally(&refs(&sites));
        assert_eq!(t.len(), 1);
        assert!(t.contains_key("12208"));
        assert!(tally(&[]).is_empty());
    }

    #[test]
    fn tally_breaks_a_tie_the_same_way_a_single_site_does() {
        // 同数なら DisasterKind の固定順(地震が先)。地点の並び順に依存しない。
        let a = vec![site("12208", &[(DisasterKind::Storm, 7)]), site("12208", &[(DisasterKind::Earthquake, 7)])];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(tally(&refs(&a))["12208"], (14, DisasterKind::Earthquake));
        assert_eq!(tally(&refs(&b))["12208"], (14, DisasterKind::Earthquake));
    }

    // ---- build_layer ----

    #[test]
    fn build_layer_paints_only_the_municipalities_that_have_records() {
        let sites = vec![site("12208", &[(DisasterKind::Storm, 60)])];
        let areas = vec![area("1220800", "野田市", 35.5, 139.5), area("1310100", "千代田区", 35.5, 139.6)];
        let (cx, cy, z, w, h) = window();
        let img = build_layer(&refs(&sites), &refs(&areas), cx, cy, z, w, h, shading()).expect("塗る対象がある");
        // 野田市の内側は風水害の色、記録の無い千代田区の内側は透明のまま。
        let at = |lat: f64, lon: f64| {
            let (gx, gy) = deg_to_pixel(lat, lon, z);
            *img.get_pixel((gx - (cx - w as f64 / 2.0)) as u32, (gy - (cy - h as f64 / 2.0)) as u32)
        };
        let noda = at(35.55, 139.55);
        assert_eq!([noda[0], noda[1], noda[2]], DisasterKind::Storm.color());
        assert_eq!(noda[3], disaster::fill_alpha(60), "件数5段のアルファ");
        assert_eq!(at(35.55, 139.65)[3], 0, "記録の無い市区町村は塗らない");
    }

    #[test]
    fn build_layer_paints_split_areas_of_one_city_in_the_same_colour() {
        // 横浜市は class20s では北部/南部の2区域だが、5桁へ丸めると同じ 14100。
        let sites = vec![site("14100", &[(DisasterKind::Earthquake, 54)])];
        let areas = vec![area("1410011", "横浜市北部", 35.5, 139.5), area("1410012", "横浜市南部", 35.5, 139.6)];
        let (cx, cy, z, w, h) = window();
        let img = build_layer(&refs(&sites), &refs(&areas), cx, cy, z, w, h, shading()).expect("塗る対象がある");
        let at = |lat: f64, lon: f64| {
            let (gx, gy) = deg_to_pixel(lat, lon, z);
            *img.get_pixel((gx - (cx - w as f64 / 2.0)) as u32, (gy - (cy - h as f64 / 2.0)) as u32)
        };
        assert_eq!(at(35.55, 139.55), at(35.55, 139.65), "7桁が違っても同じ5桁なら同じ色");
        assert_eq!(at(35.55, 139.55)[3], disaster::fill_alpha(54));
    }

    #[test]
    fn build_layer_rounds_the_hiroshima_wards_onto_the_city_count() {
        // NIED は広島市を市単位(34100)でしか持たない。区単位の区域が同じ色で塗られること。
        let sites = vec![site("34100", &[(DisasterKind::Storm, 278)])];
        let areas = vec![area("3410100", "広島市中区", 35.5, 139.5), area("3410800", "広島市佐伯区", 35.5, 139.6)];
        let (cx, cy, z, w, h) = window();
        let img = build_layer(&refs(&sites), &refs(&areas), cx, cy, z, w, h, shading()).expect("塗る対象がある");
        let at = |lat: f64, lon: f64| {
            let (gx, gy) = deg_to_pixel(lat, lon, z);
            *img.get_pixel((gx - (cx - w as f64 / 2.0)) as u32, (gy - (cy - h as f64 / 2.0)) as u32)
        };
        assert_eq!(at(35.55, 139.55)[3], 255, "278件は最も濃い段");
        assert_eq!(at(35.55, 139.55), at(35.55, 139.65));
    }

    #[test]
    fn build_layer_skips_areas_outside_the_view() {
        // 記録はあるが、区域が画面から遠い(視野bboxと交差しない)。
        let sites = vec![site("12208", &[(DisasterKind::Storm, 60)])];
        let areas = vec![area("1220800", "野田市", 20.0, 120.0)];
        let (cx, cy, z, w, h) = window();
        assert!(build_layer(&refs(&sites), &refs(&areas), cx, cy, z, w, h, shading()).is_none());
    }

    #[test]
    fn build_layer_is_none_when_there_is_nothing_to_paint() {
        let sites = vec![site("12208", &[(DisasterKind::Storm, 60)])];
        let areas = vec![area("1220800", "野田市", 35.5, 139.5)];
        let (cx, cy, z, w, h) = window();
        // 地点が無い / コードが無い / 境界が無い / 寸法ゼロ / 塗りOFF
        assert!(build_layer(&[], &refs(&areas), cx, cy, z, w, h, shading()).is_none());
        let nocode = vec![site("", &[(DisasterKind::Storm, 60)])];
        assert!(build_layer(&refs(&nocode), &refs(&areas), cx, cy, z, w, h, shading()).is_none());
        assert!(build_layer(&refs(&sites), &[], cx, cy, z, w, h, shading()).is_none());
        assert!(build_layer(&refs(&sites), &refs(&areas), cx, cy, z, 0, h, shading()).is_none());
        let off = Shading { opacity: DEFAULT_OPACITY, fill: false, blur_radius: 0 };
        assert!(build_layer(&refs(&sites), &refs(&areas), cx, cy, z, w, h, off).is_none());
    }

    #[test]
    fn build_layer_blurs_the_fill_when_blur_radius_is_set() {
        let sites = vec![site("12208", &[(DisasterKind::Storm, 60)])];
        let areas = vec![area("1220800", "野田市", 35.5, 139.5)];
        let (cx, cy, z, w, h) = window();
        let sharp = build_layer(&refs(&sites), &refs(&areas), cx, cy, z, w, h, shading()).expect("塗る対象がある");
        let blurred_sh = Shading { opacity: DEFAULT_OPACITY, fill: true, blur_radius: 3 };
        let blurred = build_layer(&refs(&sites), &refs(&areas), cx, cy, z, w, h, blurred_sh).expect("塗る対象がある");
        // ブラーは境界を滲ませて画素数(不透明な画素)を広げる。中身が全く同じなら意味が無い。
        assert!(painted(&blurred) >= painted(&sharp), "ブラー後に不透明画素が減るのはおかしい");
        assert_ne!(sharp.as_raw(), blurred.as_raw(), "ブラーで画素が変わっていない");
    }

    #[test]
    fn build_layer_colours_by_the_dominant_kind() {
        let (cx, cy, z, w, h) = window();
        let areas = vec![area("1220800", "野田市", 35.5, 139.5)];
        for (kind, count) in [
            (DisasterKind::Earthquake, 3u32),
            (DisasterKind::Snow, 20),
            (DisasterKind::Slope, 120),
        ] {
            let sites = vec![site("12208", &[(kind, count), (DisasterKind::OtherWeather, 1)])];
            let img = build_layer(&refs(&sites), &refs(&areas), cx, cy, z, w, h, shading()).expect("塗る対象がある");
            let (gx, gy) = deg_to_pixel(35.55, 139.55, z);
            let p = *img.get_pixel((gx - (cx - w as f64 / 2.0)) as u32, (gy - (cy - h as f64 / 2.0)) as u32);
            assert_eq!([p[0], p[1], p[2]], kind.color(), "{kind:?}");
            assert_eq!(p[3], disaster::fill_alpha(count + 1), "{kind:?}");
        }
    }

    // ---- area_summary ----

    #[test]
    fn area_summary_names_the_municipality_under_the_crosshair() {
        let sites = vec![site("12208", &[(DisasterKind::Storm, 60), (DisasterKind::Earthquake, 29)])];
        let areas = vec![area("1220800", "野田市", 35.5, 139.5), area("1310100", "千代田区", 35.5, 139.6)];
        assert_eq!(
            area_summary(&refs(&sites), &refs(&areas), 35.55, 139.55),
            Some(("野田市".to_string(), 89))
        );
    }

    #[test]
    fn area_summary_is_none_outside_any_area_or_without_records() {
        let sites = vec![site("12208", &[(DisasterKind::Storm, 60)])];
        let areas = vec![area("1220800", "野田市", 35.5, 139.5), area("1310100", "千代田区", 35.5, 139.6)];
        // どの区域にも入らない。
        assert_eq!(area_summary(&refs(&sites), &refs(&areas), 34.0, 138.0), None);
        // 区域には入るが、その市区町村に記録が無い(千代田区)。
        assert_eq!(area_summary(&refs(&sites), &refs(&areas), 35.55, 139.65), None);
        // 境界が1枚も無い。
        assert_eq!(area_summary(&refs(&sites), &[], 35.55, 139.55), None);
    }

    #[test]
    fn area_summary_follows_the_five_digit_rounding() {
        // 中心が広島市中区にあっても、件数は市単位(34100)から引く。
        let sites = vec![site("34100", &[(DisasterKind::Storm, 278)])];
        let areas = vec![area("3410100", "広島市中区", 35.5, 139.5)];
        assert_eq!(
            area_summary(&refs(&sites), &refs(&areas), 35.55, 139.55),
            Some(("広島市中区".to_string(), 278)),
            "名前は区・件数は市"
        );
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    // NIED の件数と気象庁の境界を実データで突き合わせ、コード直結合が本当に成立するかを見る
    // (単体テストは作り物の座標なので、実データの版ずれ・コード体系の食い違いはここでしか出ない)。
    #[test]
    #[ignore]
    fn live_paint_real_municipalities_around_tokyo() {
        // 1次メッシュ5339(東京〜千葉西部)の集計と、その範囲を覆う区域ファイル全部。
        // ファイルを1枚だけ取ると県境をまたぐ市区町村が落ちる(実測: メッシュ5339には
        // 山梨県上野原市・道志村が入るが、それらのポリゴンは class20s_4 側にある)。
        // 実機では plotlayer の boundary_cells が relm_indices を使って同じ選び方をする。
        let mesh = (35.333334, 139.000001, 35.999999, 139.999999);
        let sites = crate::disaster::fetch_sites(mesh.0, mesh.1, mesh.2, mesh.3, disaster::DEFAULT_SINCE_YEAR)
            .expect("live fetch should succeed");
        let indices = crate::muni::relm_indices(mesh);
        assert!(indices.len() >= 2, "メッシュ5339は2枚以上の区域ファイルに掛かるはず: {indices:?}");
        let mut areas: Vec<MuniArea> = Vec::new();
        for i in indices {
            areas.extend(crate::muni::fetch_relm(i).expect("live fetch should succeed"));
        }
        let site_refs: Vec<&DisasterSite> = sites.iter().collect();
        let area_refs: Vec<&MuniArea> = areas.iter().collect();

        // 件数側のコードが境界側に1つも見つからない、という取りこぼしの規模を数える
        // (合併等でNIED側が旧コードのまま残っている市区町村は黙って塗られなくなる。
        // 全国では那珂川市の1件だけが該当する実測なので、この範囲では0件のはず)。
        let counts = tally(&site_refs);
        let known: std::collections::HashSet<&str> = areas.iter().map(|a| a.muni_code()).collect();
        let orphans: Vec<&String> = counts.keys().filter(|c| !known.contains(c.as_str())).collect();
        println!("地点 {} / 区域 {} / 集計コード {} / 境界に無いコード {}",
                 sites.len(), areas.len(), counts.len(), orphans.len());
        for c in &orphans {
            println!("  境界に無いコード: {c}");
        }
        assert!(orphans.is_empty(), "件数はあるのに塗れない市区町村がある: {orphans:?}");

        // 東京駅中心・z11 の画面に、実際に色が乗ること。
        let z = 11;
        let (cx, cy) = deg_to_pixel(35.681236, 139.767125, z);
        let (w, h) = (320u32, 200u32);
        let sh = Shading { opacity: DEFAULT_OPACITY, fill: true, blur_radius: 0 };
        let img = build_layer(&site_refs, &area_refs, cx, cy, z, w, h, sh).expect("東京で1区域も塗れないのはおかしい");
        let filled = img.pixels().filter(|p| p[3] > 0).count();
        let coverage = filled as f64 / (w * h) as f64;
        println!("塗られた画素 {filled}/{} ({:.1}%)", w * h, coverage * 100.0);
        assert!(coverage > 0.5, "都心の画面がほとんど塗られていない: {coverage:.3}");

        // 野田市(#75 の設計書に出てくる代表点)が、名前と件数の両方で引けること。
        let (name, total) = area_summary(&site_refs, &area_refs, 35.955106, 139.874828).expect("野田市が引けない");
        println!("野田市の代表点 → {name} {total}件");
        assert_eq!(name, "野田市");
        assert!(total > 0);
        // 海の上はどの市区町村にも入らない。
        assert_eq!(area_summary(&site_refs, &area_refs, 35.0, 140.5), None, "海上で市区町村が引けている");
    }

    // 広域ズーム(z9)の裏取り。実ネットワークを叩く手動確認用。
    // 「広域なら地方でも見比べられる」(広域版 §0.7)という主張が実データで成り立つかを見る。
    // 取得自体はdisaster::fetch_sitesを直接1回叩く(plotlayerの1次メッシュ分割は経由しない。
    // ここで確認したいのは描画側の見え方で、取得のセル分割は無関係)。
    #[test]
    #[ignore]
    fn live_paint_a_wide_zoom_screen_at_z9() {
        // 最も混む範囲(緯度34.67〜37.33度・経度136〜140度、旧・広域セル1309相当)。
        // 東京も岐阜北部もこの中に入る。
        let cell = crate::mesh::shrink((34.666667, 136.0, 37.333333, 140.0));
        let sites = crate::disaster::fetch_sites(cell.0, cell.1, cell.2, cell.3, disaster::DEFAULT_SINCE_YEAR)
            .expect("live fetch should succeed");
        println!("範囲内: 地点 {} 件", sites.len());
        assert!(
            !crate::disaster::truncation_seen(),
            "2,000行の打ち切りに当たっている(市区町村がまるごと塗られなくなる)"
        );
        let mut areas: Vec<MuniArea> = Vec::new();
        for i in crate::muni::relm_indices(cell) {
            areas.extend(crate::muni::fetch_relm(i).expect("live fetch should succeed"));
        }
        let site_refs: Vec<&DisasterSite> = sites.iter().collect();
        let area_refs: Vec<&MuniArea> = areas.iter().collect();

        let (z, w, h) = (9u32, 320u32, 200u32);
        // 都心: 画面のほとんどが塗られる。
        let (cx, cy) = deg_to_pixel(35.681236, 139.767125, z);
        let img = build_layer(&site_refs, &area_refs, cx, cy, z, w, h, shading()).expect("東京 z9 で1区域も塗れない");
        let coverage = painted(&img) as f64 / (w * h) as f64;
        println!("東京 z9 の塗られた画素 {}/{} ({:.1}%)", painted(&img), w * h, coverage * 100.0);
        assert!(coverage > 0.5, "都心の z9 画面がほとんど塗られていない: {coverage:.3}");

        // 地方(岐阜北部): z11 では画面が1区域=全画面1色になるが、z9 なら複数区域が
        // 別の色/濃さで並ぶ。
        let (cx, cy) = deg_to_pixel(36.24, 137.25, z);
        let img = build_layer(&site_refs, &area_refs, cx, cy, z, w, h, shading()).expect("岐阜北部 z9 で1区域も塗れない");
        let shades: std::collections::HashSet<[u8; 4]> =
            img.pixels().filter(|p| p[3] > 0).map(|p| [p[0], p[1], p[2], p[3]]).collect();
        println!("岐阜北部 z9 の塗り分け {} 種類: {shades:?}", shades.len());
        assert!(shades.len() >= 2, "地方の z9 が1色に潰れている: {shades:?}");

        // ブラーを掛けると、輪郭線無しでも隣接区域の境界が滲んで見える
        // (硬いエッジではなく段階的な変化になる。設計 unlimited-zoom §4)。
        let blurred_sh = Shading { opacity: DEFAULT_OPACITY, fill: true, blur_radius: 2 };
        let blurred = build_layer(&site_refs, &area_refs, cx, cy, z, w, h, blurred_sh).expect("岐阜北部 z9 で1区域も塗れない");
        assert!(painted(&blurred) >= painted(&img), "ブラー後に不透明画素が減るのはおかしい");
    }
}
