// geoarea: 気象庁の一次細分区域(class10s)境界データを使った点-in-領域判定。
// std + serde_json のみに依存し、crate:: を参照しない(traffic.rs等と同じ方針・単体コンパイル可能)。
// 座標は (f64, f64) = (lat, lon)。
//
// データはビルド時に静的埋め込み(include_str!)する。境界ポリゴンはユーザー操作と無関係に
// 必要な基礎データで、ネットワーク障害時にこれが取れないと機能全体が動かなくなるのは
// 過剰なため(traffic/camera/regulation/disasterのような都度取得+TTLではない)。
//   - assets/jma-class10s.json: jma.go.jp/bosai/common/const/geojson/class10s.json (153件・289KB)
//   - assets/jma-class10s-parent.json: area.json の class10s[code].parent だけを抜いた142件の対応表
//     (所属する気象台コード。warning.json の取得先URLに使う。例: 130010→130000)
// 2つのファイルのcode件数が食い違う(153 vs 142)ことは実測で確認済み。parentが引けない
// regionは黙って読み込み対象から外す(警報取得先が分からないものは扱えないため)。
//
// GeoJSON MultiPolygonの穴(内側リング)は実測でこのデータセットには存在しない
// (153件全部で各ポリゴンのリング数は1)。将来穴入りデータに変わった場合に備え、
// 判定自体は外周のみを見る前提を明記しておく。

use std::sync::OnceLock;

const CLASS10S_GEOJSON: &str = include_str!("../assets/jma-class10s.json");
const CLASS10S_PARENT: &str = include_str!("../assets/jma-class10s-parent.json");

#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub code: String,        // class10s コード(例: "130020"=伊豆諸島北部)
    pub name: String,        // 地域名(例: "伊豆諸島北部")
    pub office_code: String, // 所属気象台コード。warning.json/{office_code}.json の取得先
    pub rings: Vec<Vec<(f64, f64)>>, // ポリゴンごとの外周(穴は無視)。複数=離島等の分離した塊
}

// レイキャスティング法。ring は閉じていなくてもよい(先頭末尾が重複していない前提で
// wrap-aroundして最後の辺も見る)。境界線上の点の扱いは規定しない(実用上は誤差の範囲)。
pub fn point_in_polygon(point: (f64, f64), ring: &[(f64, f64)]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let (py, px) = point; // (lat, lon) = (y, x)として扱う
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let (yi, xi) = ring[i];
        let (yj, xj) = ring[j];
        if (yi > py) != (yj > py) {
            let x_at_y = xi + (py - yi) / (yj - yi) * (xj - xi);
            if px < x_at_y {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

pub fn point_in_multi_polygon(point: (f64, f64), rings: &[Vec<(f64, f64)>]) -> bool {
    rings.iter().any(|r| point_in_polygon(point, r))
}

fn parse_regions(geojson: &str, parent_map: &str) -> Vec<Region> {
    let Ok(g) = serde_json::from_str::<serde_json::Value>(geojson) else { return Vec::new() };
    let Ok(parents) = serde_json::from_str::<serde_json::Value>(parent_map) else { return Vec::new() };
    let Some(features) = g.get("features").and_then(|f| f.as_array()) else { return Vec::new() };
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(code) = f.pointer("/properties/code").and_then(|c| c.as_str()) else { continue };
        let Some(office_code) = parents.get(code).and_then(|p| p.as_str()) else { continue }; // 対応表に無いものは扱えないので外す
        let name = f.pointer("/properties/name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let Some(polygons) = f.pointer("/geometry/coordinates").and_then(|c| c.as_array()) else { continue };
        let mut rings = Vec::new();
        for poly in polygons {
            let Some(poly_rings) = poly.as_array() else { continue };
            let Some(outer) = poly_rings.first().and_then(|r| r.as_array()) else { continue }; // 穴(2番目以降のリング)は無視
            let ring: Vec<(f64, f64)> = outer
                .iter()
                .filter_map(|pt| {
                    let p = pt.as_array()?;
                    let lon = p.first()?.as_f64()?;
                    let lat = p.get(1)?.as_f64()?;
                    Some((lat, lon))
                })
                .collect();
            if ring.len() >= 3 {
                rings.push(ring);
            }
        }
        if rings.is_empty() {
            continue;
        }
        out.push(Region { code: code.to_string(), name, office_code: office_code.to_string(), rings });
    }
    out
}

static CLASS10S_REGIONS: OnceLock<Vec<Region>> = OnceLock::new();

pub fn class10s_regions() -> &'static [Region] {
    CLASS10S_REGIONS.get_or_init(|| parse_regions(CLASS10S_GEOJSON, CLASS10S_PARENT))
}

// pointが属するregionを1つ返す(重なりが無い前提のデータなので最初に当たったものを返す)。
pub fn region_at(point: (f64, f64)) -> Option<&'static Region> {
    class10s_regions().iter().find(|r| point_in_multi_polygon(point, &r.rings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_in_polygon_detects_inside_and_outside_a_square() {
        let square = vec![(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0)];
        assert!(point_in_polygon((5.0, 5.0), &square));
        assert!(!point_in_polygon((15.0, 5.0), &square));
        assert!(!point_in_polygon((-1.0, 5.0), &square));
    }

    #[test]
    fn point_in_polygon_handles_a_concave_shape() {
        // C字型(凹型)。くびれの外側は外、内側の凹みは外、本体は内。
        let c_shape = vec![
            (0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 6.0),
            (3.0, 6.0), (3.0, 4.0), (10.0, 4.0), (10.0, 0.0),
        ];
        assert!(point_in_polygon((5.0, 1.0), &c_shape), "下の腕の中");
        assert!(point_in_polygon((5.0, 9.0), &c_shape), "上の腕の中");
        assert!(!point_in_polygon((5.0, 5.0), &c_shape), "くびれの凹み(本体の外)");
    }

    #[test]
    fn point_in_polygon_too_few_points_is_always_outside() {
        assert!(!point_in_polygon((0.0, 0.0), &[]));
        assert!(!point_in_polygon((0.0, 0.0), &[(0.0, 0.0), (1.0, 1.0)]));
    }

    #[test]
    fn point_in_multi_polygon_true_if_any_ring_contains_it() {
        let near = vec![(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
        let far = vec![(50.0, 50.0), (50.0, 51.0), (51.0, 51.0), (51.0, 50.0)];
        assert!(point_in_multi_polygon((0.5, 0.5), &[far.clone(), near.clone()]));
        assert!(!point_in_multi_polygon((25.0, 25.0), &[far, near]));
    }

    #[test]
    fn parse_regions_extracts_code_name_office_and_rings() {
        let geojson = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"MultiPolygon","coordinates":[[[[139.0,35.0],[139.0,36.0],[140.0,36.0]]]]},
             "properties":{"code":"130010","name":"東京地方"}}
        ]}"#;
        let parents = r#"{"130010":"130000"}"#;
        let regions = parse_regions(geojson, parents);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].code, "130010");
        assert_eq!(regions[0].name, "東京地方");
        assert_eq!(regions[0].office_code, "130000");
        assert_eq!(regions[0].rings, vec![vec![(35.0, 139.0), (36.0, 139.0), (36.0, 140.0)]]);
    }

    #[test]
    fn parse_regions_skips_codes_with_no_resolvable_parent() {
        // GeoJSON側に153件あってもparent対応表に無いもの(実測で確認済みの食い違い)は
        // 警報取得先が分からないため黙って外す。
        let geojson = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"MultiPolygon","coordinates":[[[[0.0,0.0],[0.0,1.0],[1.0,1.0]]]]},
             "properties":{"code":"999999","name":"存在しないコード"}}
        ]}"#;
        let parents = r#"{"130010":"130000"}"#;
        assert!(parse_regions(geojson, parents).is_empty());
    }

    #[test]
    fn parse_regions_handles_garbage_without_panicking() {
        assert!(parse_regions("not json", "{}").is_empty());
        assert!(parse_regions("{}", "not json").is_empty());
        assert!(parse_regions(r#"{"features":[]}"#, "{}").is_empty());
    }

    #[test]
    fn parse_regions_keeps_multiple_disjoint_polygons_per_feature() {
        // 離島等、1つのregionが複数の分離したポリゴンを持つケース。
        let geojson = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"MultiPolygon","coordinates":[
                [[[139.0,35.0],[139.0,36.0],[140.0,36.0]]],
                [[[150.0,25.0],[150.0,26.0],[151.0,26.0]]]
            ]},
             "properties":{"code":"130040","name":"小笠原諸島"}}
        ]}"#;
        let parents = r#"{"130040":"130000"}"#;
        let regions = parse_regions(geojson, parents);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].rings.len(), 2);
    }

    // 実データ(assets/jma-class10s.json + assets/jma-class10s-parent.json)を使った確認。
    // 実測(2026/08/17)通り153件のGeoJSON中142件だけがparentを解決できることを固定する。
    #[test]
    fn class10s_regions_loads_the_embedded_real_data() {
        // 実測(2026/08/17): GeoJSON側153件のFeature中、"hoppo"(北方領土。特殊コードで
        // parent対応表に無い)だけが解決できず152件になる(1コードが複数Featureに
        // 分かれて出てくることはあるが、それも別々のRegionとしてそのまま持つ)。
        let regions = class10s_regions();
        assert_eq!(regions.len(), 152, "件数が変わったらデータ更新([hoppo]以外の欠落増?)を確認");
        assert!(!regions.iter().any(|r| r.code == "hoppo"), "parentが引けないhoppoは除外されているはず");
        assert!(regions.iter().any(|r| r.code == "130010" && r.office_code == "130000"), "東京地方が読めているか");
    }

    #[test]
    fn region_at_finds_tokyo_for_a_point_in_central_tokyo() {
        // 新宿駅付近。東京地方(130010)に入っているはず。
        let r = region_at((35.6896, 139.7006)).expect("新宿は東京地方に入るはず");
        assert_eq!(r.code, "130010");
        assert_eq!(r.office_code, "130000");
    }

    #[test]
    fn region_at_returns_none_for_a_point_far_out_at_sea() {
        assert!(region_at((0.0, 0.0)).is_none());
    }
}
