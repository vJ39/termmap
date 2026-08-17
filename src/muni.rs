// 市区町村の境界(気象庁 class20s の区域ポリゴン)。過去災害レイヤをコロプレス(市区町村を
// 記録の多さで塗り分ける)で出すための下地。設計は docs/disaster-choropleth-design.md §2。
//
// 実測で確認済みの構造(2026/08/17):
//   - 10ファイル分割(class20s_0 〜 _9)・合計2.98MB・区域1,806・総頂点148,424(1区域平均82頂点)。
//     _0 が北海道。0始まりなので _0 を取りこぼすと道内179市町村が丸ごと欠ける。
//   - 幾何は全件 MultiPolygon。穴は0件・全リング閉合。リングの巻き方向は不統一(CW/CCW混在)
//     なので、向きに依存する塗り方(nonzero winding)は使えない → even-odd(geopoly.rs)。
//   - 座標は小数4桁(緯度で約11m)。JMA側で既に簡略化されており、こちらで間引く処理は要らない。
//   - properties は code(7桁文字列)/name(和名)/enName/labelPoints。
//   - **code は数値とは限らない**。"hoppo"(根室地方)が1件混入しているので、整数へパースせず
//     文字列のまま扱い、先頭5桁もバイト長を確かめてから切る。
//   - code の先頭5桁が全国地方公共団体コード。1,806区域が1,757個の5桁コードに対応する
//     (40市区町村が複数区域に分かれている。例 横浜市北部/南部・釧路市釧路/阿寒/音別)。
//
// NIED(災害事例データベース)の CHIDAN_CODE と突き合わせるための丸め方は muni_code() を参照。
// traffic.rs/disaster.rs と同じ方針で std + ureq + serde_json に依存し、crate:: は
// geopoly(ネットワークにもクレートの状態にも触れない純粋な幾何)だけを参照する。
// そのため単体テストが実ネットワーク無しで完結する性質は変わらない。

use crate::geopoly;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// 開発者向けに文書化されていないエンドポイント(雨雲ナウキャストのタイルと同じ立場)なので、
// URLはここ1か所に閉じる。仕様変更で取れなくなったら Err を返し、手元のキャッシュ
// (stale上限なし)を出し続ける。境界が取れなければ塗りを出さず従来のマーカー表示へ落ちる。
const BASE_URL: &str = "https://www.jma.go.jp/bosai/common/const/geojson";
const USER_AGENT: &str = "termmap/0.1 (personal experiment)";
const HTTP_TIMEOUT_SECS: u64 = 20;

/// 区域ファイルの数(class20s_0 〜 _9)。
pub const RELM_COUNT: usize = 10;

/// `https://www.jma.go.jp/bosai/common/const/relm.json` の写し。`RELM[i]` が
/// `class20s_{i}.json` の中身の外接矩形で、並びは (lat_min, lon_min, lat_max, lon_max)
/// (plotlayer::Bbox と同じ)。
///
/// 取りに行かず定数として持つのは、10要素の小さな配列で内容が変わらないため。境界ファイルを
/// 1枚も取れない状態でも被覆計算だけは動く。矩形どうしは重なっているが、各ファイルの中身の
/// 外接矩形なので「視野に入る区域は必ず、視野と交差する矩形のファイルのどれかに入っている」。
const RELM: [(f64, f64, f64, f64); RELM_COUNT] = [
    (41.3521, 139.3344, 45.5569, 148.8922), // 0 北海道
    (38.7478, 139.6932, 41.5559, 142.0725), // 1 東北北部
    (36.7365, 137.6350, 39.2086, 141.6747), // 2 東北南部〜北陸
    (24.2254, 138.3971, 37.1543, 153.9864), // 3 関東〜伊豆小笠原
    (34.5781, 136.2439, 37.8553, 139.1766), // 4 中部
    (33.4330, 134.2527, 36.2953, 136.9877), // 5 近畿
    (32.7025, 131.6680, 37.2429, 134.8208), // 6 中国東部〜四国
    (31.9887, 128.3437, 34.7987, 132.4913), // 7 中国西部〜九州北部
    (27.0187, 128.3955, 33.1944, 131.8857), // 8 九州南部〜奄美
    (24.0456, 122.9337, 27.8853, 131.3312), // 9 沖縄
];

/// 政令指定都市のうち、JMA が区単位で区域を持つ市の区コード → 市コード。
///
/// NIED は政令市を市単位のコード1つでしか持たない(`JP34100` 広島市 278件 /
/// `JP28100` 神戸市 137件)。区単位で持たれているのはこの2市だけで、他の政令市は
/// 市まるごとか、横浜市型(`1410011`/`1410012` のように下2桁だけが細分)なので
/// `code[..5]` で素直に合う。この表が無いと全国63,849件のうち415件(0.65%)が塗られない。
///
/// 表の網羅性はテストで固定するが、JMA が将来ほかの政令市も区単位へ変えた場合は検出できない
/// (その市が塗られなくなる)。
const WARD_TO_CITY: [(&str, &str); 17] = [
    ("34101", "34100"), // 広島市中区
    ("34102", "34100"), // 広島市東区
    ("34103", "34100"), // 広島市南区
    ("34104", "34100"), // 広島市西区
    ("34105", "34100"), // 広島市安佐南区
    ("34106", "34100"), // 広島市安佐北区
    ("34107", "34100"), // 広島市安芸区
    ("34108", "34100"), // 広島市佐伯区
    ("28101", "28100"), // 神戸市東灘区
    ("28102", "28100"), // 神戸市灘区
    ("28105", "28100"), // 神戸市兵庫区
    ("28106", "28100"), // 神戸市長田区
    ("28107", "28100"), // 神戸市須磨区
    ("28108", "28100"), // 神戸市垂水区
    ("28109", "28100"), // 神戸市北区
    ("28110", "28100"), // 神戸市中央区
    ("28111", "28100"), // 神戸市西区
];

/// 区域1つ。ディスクキャッシュ(plotcache の boundary レイヤ)へそのまま保存する。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MuniArea {
    /// 7桁。先頭5桁が全国地方公共団体コード(ただし "hoppo" のような非数値も実在する)。
    pub code: String,
    /// 和名(ステータス行に出す)。
    pub name: String,
    /// MultiPolygon の全リングを平らに並べたもの。1点は (緯度, 経度)。
    /// 外周と穴と離島を区別しない(even-odd で塗る/判定するため)。
    pub rings: Vec<Vec<(f64, f64)>>,
    /// 事前計算した外接矩形 (lat_min, lon_min, lat_max, lon_max)。視野での絞り込みと
    /// 内外判定の前段に使う。
    pub bbox: (f64, f64, f64, f64),
}

impl MuniArea {
    /// NIED の CHIDAN_CODE と突き合わせる5桁のコード。
    ///
    /// 7桁の先頭5桁を採り、広島市・神戸市の区コードだけ WARD_TO_CITY で市コードへ丸める。
    /// "hoppo" のような非数値コードもあるので整数へパースせず、5バイト未満・文字境界でない
    /// コードは空文字を返す(空なら塗る対象にならないだけで壊れない)。
    ///
    /// **ポリゴン自体は7桁のまま扱う**。5桁へ統合(リングを連結)すると、統合した区域どうしの
    /// 内部境界にまで縁取りが引かれてしまう。割り当ては5桁・描画は7桁にすれば、横浜市の
    /// 2区域が同じ色で塗られたうえで区域ごとの縁取りが残る。
    pub fn muni_code(&self) -> &str {
        if self.code.len() < 5 || !self.code.is_char_boundary(5) {
            return "";
        }
        let five = &self.code[..5];
        for (ward, city) in WARD_TO_CITY {
            if ward == five {
                return city;
            }
        }
        five
    }
}

/// 視野bbox (lat_min, lon_min, lat_max, lon_max) を覆う区域ファイルの番号。
/// 矩形が重なる位置では複数返る(取りこぼさない側に倒す)。日本の外なら空。
pub fn relm_indices(b: (f64, f64, f64, f64)) -> Vec<usize> {
    RELM.iter()
        .enumerate()
        .filter(|(_, r)| b.0 <= r.2 && b.2 >= r.0 && b.1 <= r.3 && b.3 >= r.1)
        .map(|(i, _)| i)
        .collect()
}

/// 区域ファイル1枚を取ってパースする。
/// 空になった応答は Err にする(fresh 180日のレイヤなので、空を成功として保存すると
/// 半年間ずっと「境界が無い」状態が固定されてしまう)。
pub fn fetch_relm(index: usize) -> Result<Vec<MuniArea>, String> {
    if index >= RELM_COUNT {
        return Err(format!("市区町村境界: 領域番号が範囲外 {index}"));
    }
    let url = format!("{BASE_URL}/class20s_{index}.json");
    let body = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("市区町村境界: {e}"))?
        .into_string()
        .map_err(|e| format!("市区町村境界の読み取り: {e}"))?;
    let areas = parse_areas(&body);
    if areas.is_empty() {
        return Err(format!("市区町村境界: class20s_{index} に区域が無い"));
    }
    Ok(areas)
}

// ---- パース(ネットワークに触れない純関数) ----

/// GeoJSON の FeatureCollection → Vec<MuniArea>。
/// code が無い行・幾何が読めない行は黙って捨て、壊れた入力でも panic せず空 Vec を返す。
pub fn parse_areas(body: &str) -> Vec<MuniArea> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return Vec::new() };
    let Some(features) = v.get("features").and_then(|f| f.as_array()) else { return Vec::new() };
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let props = f.get("properties");
        let code = text(props.and_then(|p| p.get("code")));
        if code.is_empty() {
            continue; // コードが無いと件数を割り当てられない
        }
        let Some(g) = f.get("geometry") else { continue };
        let rings = geometry_rings(g);
        let Some(bbox) = geopoly::rings_bbox(&rings) else { continue };
        out.push(MuniArea { code, name: text(props.and_then(|p| p.get("name"))), rings, bbox });
    }
    out
}

// Polygon / MultiPolygon の全リングを平らに並べる。GeoJSON は [経度, 緯度] の順だが、
// termmap は一貫して (緯度, 経度) の組で持つのでここで入れ替える。
fn geometry_rings(g: &serde_json::Value) -> Vec<Vec<(f64, f64)>> {
    let Some(coords) = g.get("coordinates").and_then(|c| c.as_array()) else { return Vec::new() };
    let mut out = Vec::new();
    match g.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        // 実測では全件 MultiPolygon だが、Polygon で返ってきても読めるようにしておく。
        "MultiPolygon" => {
            for poly in coords {
                if let Some(rings) = poly.as_array() {
                    for r in rings {
                        push_ring(r, &mut out);
                    }
                }
            }
        }
        "Polygon" => {
            for r in coords {
                push_ring(r, &mut out);
            }
        }
        _ => {}
    }
    out
}

fn push_ring(v: &serde_json::Value, out: &mut Vec<Vec<(f64, f64)>>) {
    let Some(arr) = v.as_array() else { return };
    let mut ring = Vec::with_capacity(arr.len());
    for p in arr {
        let Some(pair) = p.as_array() else { continue };
        let (Some(lon), Some(lat)) = (
            pair.first().and_then(|x| x.as_f64()),
            pair.get(1).and_then(|x| x.as_f64()),
        ) else {
            continue;
        };
        if !lon.is_finite() || !lat.is_finite() {
            continue;
        }
        ring.push((lat, lon));
    }
    if ring.len() >= 3 {
        out.push(ring); // 面を持たないリング(点・線に潰れたもの)は捨てる
    }
}

// null や数値が混ざっても壊れないよう、文字列は必ず String へ落として扱う。
fn text(v: Option<&serde_json::Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 実応答の抜粋(2026/08/17 実測)。
    //   1件目 利島村 = class20s_3 の実データそのまま(9頂点・1ポリゴン)。
    //   2件目 横浜市北部 = 実在のコードと名前。リングは読みやすさのため4頂点×2ポリゴンへ間引いた
    //          (離島を持つ多重ポリゴンが平らに並ぶことの確認用)。
    //   3件目 根室地方 = code が数値でない実在の行(整数パースで落ちないことの確認用)。
    const AREAS_SAMPLE: &str = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","geometry":{"type":"MultiPolygon","coordinates":[[[[139.2806,34.5333],[139.2903,34.5294],[139.2931,34.5242],[139.2819,34.5125],[139.2759,34.5111],[139.2681,34.5173],[139.2677,34.5251],[139.2757,34.5338],[139.2806,34.5333]]]]},
         "properties":{"code":"1336200","name":"利島村","enName":"Toshima Village","labelPoints":[[139.2777,34.5211]]}},
        {"type":"Feature","geometry":{"type":"MultiPolygon","coordinates":[
            [[[139.5000,35.5000],[139.6000,35.5000],[139.6000,35.6000],[139.5000,35.6000]]],
            [[[139.6500,35.5500],[139.6600,35.5500],[139.6600,35.5600],[139.6500,35.5600]]]]},
         "properties":{"code":"1410011","name":"横浜市北部","enName":"Northern Yokohama City","labelPoints":[[139.55,35.55]]}},
        {"type":"Feature","geometry":{"type":"MultiPolygon","coordinates":[[[[146.0000,44.0000],[146.1000,44.0000],[146.1000,44.1000],[146.0000,44.1000]]]]},
         "properties":{"code":"hoppo","name":"根室地方","enName":"Nemuro Region","labelPoints":[[146.05,44.05]]}}
      ]}"#;

    fn sample() -> Vec<MuniArea> {
        parse_areas(AREAS_SAMPLE)
    }

    // ---- パース ----

    #[test]
    fn parse_areas_reads_the_code_the_name_and_the_rings() {
        let got = sample();
        assert_eq!(got.len(), 3, "{got:?}");
        let toshima = &got[0];
        assert_eq!(toshima.code, "1336200");
        assert_eq!(toshima.name, "利島村");
        assert_eq!(toshima.rings.len(), 1);
        assert_eq!(toshima.rings[0].len(), 9);
        // GeoJSON の [経度, 緯度] が (緯度, 経度) へ入れ替わっていること。
        assert_eq!(toshima.rings[0][0], (34.5333, 139.2806));
    }

    #[test]
    fn parse_areas_flattens_every_ring_of_a_multipolygon() {
        let yokohama = &sample()[1];
        assert_eq!(yokohama.rings.len(), 2, "本島と離島が平らに並ぶ");
        assert_eq!(yokohama.rings[1][0], (35.55, 139.65));
    }

    #[test]
    fn parse_areas_precomputes_the_bounding_box_over_all_rings() {
        let yokohama = &sample()[1];
        assert_eq!(yokohama.bbox, (35.5, 139.5, 35.6, 139.66));
        let toshima = &sample()[0];
        assert_eq!(toshima.bbox.0, 34.5111);
        assert_eq!(toshima.bbox.3, 139.2931);
    }

    #[test]
    fn parse_areas_keeps_a_non_numeric_code_as_a_string() {
        let hoppo = &sample()[2];
        assert_eq!(hoppo.code, "hoppo", "整数としてパースしない");
        assert_eq!(hoppo.name, "根室地方");
    }

    #[test]
    fn parse_areas_drops_rows_without_a_code_or_a_usable_geometry() {
        let body = r#"{"features":[
            {"geometry":{"type":"MultiPolygon","coordinates":[[[[139.0,35.0],[139.1,35.0],[139.1,35.1]]]]},"properties":{"name":"コード無し"}},
            {"properties":{"code":"1310100","name":"幾何無し"}},
            {"geometry":{"type":"MultiPolygon","coordinates":[]},"properties":{"code":"1310200","name":"座標無し"}},
            {"geometry":{"type":"MultiPolygon","coordinates":[[[[139.0,35.0],[139.1,35.0]]]]},"properties":{"code":"1310300","name":"2点だけ"}},
            {"geometry":{"type":"Point","coordinates":[139.0,35.0]},"properties":{"code":"1310400","name":"点"}},
            {"geometry":{"type":"MultiPolygon","coordinates":[[[[139.0,35.0],[139.1,35.0],[139.1,35.1]]]]},"properties":{"code":"1310500","name":"まともな行"}}
        ]}"#;
        let got = parse_areas(body);
        assert_eq!(got.len(), 1, "最後の1行だけが残る: {got:?}");
        assert_eq!(got[0].code, "1310500");
    }

    #[test]
    fn parse_areas_handles_garbage_without_panicking() {
        assert!(parse_areas("not json").is_empty());
        assert!(parse_areas("{}").is_empty());
        assert!(parse_areas(r#"{"features":[]}"#).is_empty());
        assert!(parse_areas(r#"{"features":{}}"#).is_empty());
        assert!(parse_areas(r#"{"features":[{}]}"#).is_empty());
        assert!(parse_areas(r#"{"features":[{"properties":{"code":123}}]}"#).is_empty());
    }

    #[test]
    fn parse_areas_also_accepts_a_plain_polygon() {
        let body = r#"{"features":[{"geometry":{"type":"Polygon","coordinates":[[[139.0,35.0],[139.1,35.0],[139.1,35.1]]]},"properties":{"code":"1310100","name":"千代田区"}}]}"#;
        let got = parse_areas(body);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].rings.len(), 1);
    }

    // ---- 5桁への丸め ----

    #[test]
    fn muni_code_takes_the_first_five_digits() {
        let a = |code: &str| MuniArea {
            code: code.to_string(),
            name: String::new(),
            rings: Vec::new(),
            bbox: (0.0, 0.0, 0.0, 0.0),
        };
        assert_eq!(a("1220800").muni_code(), "12208", "野田市");
        assert_eq!(a("0110000").muni_code(), "01100", "札幌市(先頭の0を落とさない)");
        assert_eq!(a("2710000").muni_code(), "27100", "大阪市");
        // 横浜市型: 下2桁が細分。2つの区域が同じ5桁へ丸まる = 同じ色で塗られる。
        assert_eq!(a("1410011").muni_code(), "14100");
        assert_eq!(a("1410012").muni_code(), "14100");
        // 釧路市型(3分割)。
        for c in ["0120601", "0120602", "0120603"] {
            assert_eq!(a(c).muni_code(), "01206", "{c}");
        }
    }

    #[test]
    fn muni_code_rounds_the_hiroshima_and_kobe_wards_up_to_their_city() {
        let a = |code: &str| MuniArea {
            code: code.to_string(),
            name: String::new(),
            rings: Vec::new(),
            bbox: (0.0, 0.0, 0.0, 0.0),
        };
        // 広島市8区(3410100 中区 〜 3410800 佐伯区)→ NIED の JP34100。
        for n in 1..=8 {
            assert_eq!(a(&format!("341{n:02}00")).muni_code(), "34100", "広島市 区{n}");
        }
        // 神戸市9区。03/04(旧 葺合区/生田区)は1980年に中央区へ統合済みで実データに無い。
        for n in [1, 2, 5, 6, 7, 8, 9, 10, 11] {
            assert_eq!(a(&format!("281{n:02}00")).muni_code(), "28100", "神戸市 区{n}");
        }
        // 他の政令市は市まるごと(または横浜市型)なので丸めない。
        assert_eq!(a("2610000").muni_code(), "26100", "京都市");
        assert_eq!(a("4010000").muni_code(), "40100", "北九州市");
    }

    #[test]
    fn muni_code_never_panics_on_a_short_or_non_numeric_code() {
        let a = |code: &str| MuniArea {
            code: code.to_string(),
            name: String::new(),
            rings: Vec::new(),
            bbox: (0.0, 0.0, 0.0, 0.0),
        };
        assert_eq!(a("hoppo").muni_code(), "hoppo", "5文字ちょうどはそのまま(どの件数にも当たらない)");
        assert_eq!(a("").muni_code(), "");
        assert_eq!(a("133").muni_code(), "");
        assert_eq!(a("東京都千代田区").muni_code(), "", "非ASCIIで文字境界を割らない");
    }

    #[test]
    fn the_ward_table_has_no_duplicates_and_covers_exactly_the_two_cities() {
        assert_eq!(WARD_TO_CITY.len(), 17, "広島市8区 + 神戸市9区");
        let mut wards: Vec<&str> = WARD_TO_CITY.iter().map(|(w, _)| *w).collect();
        let n = wards.len();
        wards.sort_unstable();
        wards.dedup();
        assert_eq!(wards.len(), n, "区コードが重複している");
        // 丸め先は広島市・神戸市の2つだけ。
        let mut cities: Vec<&str> = WARD_TO_CITY.iter().map(|(_, c)| *c).collect();
        cities.sort_unstable();
        cities.dedup();
        assert_eq!(cities, vec!["28100", "34100"]);
        // 実データ(class20s)の区コードと一致すること。広島市は 34101〜34108 の連番、
        // 神戸市は 28101/28102 と 28105〜28111。
        let hiroshima: Vec<String> = (1..=8).map(|n| format!("341{n:02}")).collect();
        let kobe: Vec<String> = [1, 2, 5, 6, 7, 8, 9, 10, 11].iter().map(|n| format!("281{n:02}")).collect();
        for w in hiroshima.iter().chain(kobe.iter()) {
            assert!(WARD_TO_CITY.iter().any(|(k, _)| k == w), "表に {w} が無い");
        }
        assert_eq!(WARD_TO_CITY.len(), hiroshima.len() + kobe.len(), "表に余計な行がある");
        // 丸め先そのもの(34100/28100)は区コードとして表に入っていない(自己参照しない)。
        for (_, city) in WARD_TO_CITY {
            assert!(!WARD_TO_CITY.iter().any(|(w, _)| *w == city), "{city} が区側にも入っている");
        }
    }

    // ---- 領域の被覆 ----

    #[test]
    fn relm_indices_pick_the_file_that_holds_each_city() {
        // 点を包む極小のbboxで引く(実測どおりの番号が返ること)。
        let at = |lat: f64, lon: f64| relm_indices((lat, lon, lat, lon));
        assert_eq!(at(35.68, 139.77), vec![3], "東京");
        assert_eq!(at(43.06, 141.35), vec![0], "札幌");
        assert_eq!(at(34.69, 135.52), vec![5], "大阪");
        assert_eq!(at(26.21, 127.68), vec![9], "那覇");
    }

    #[test]
    fn relm_indices_return_every_file_whose_rectangle_overlaps() {
        // relm の矩形は互いに重なっている。重なりの中では複数返る(取りこぼさない側)。
        let both = relm_indices((36.5, 138.7, 36.5, 138.7));
        assert!(both.len() >= 2, "重なり位置で1枚しか返っていない: {both:?}");
        assert!(both.contains(&3) && both.contains(&4), "{both:?}");
        // 広い視野なら当然もっと返る。
        let wide = relm_indices((33.0, 133.0, 38.0, 141.0));
        assert!(wide.len() >= 3, "{wide:?}");
    }

    #[test]
    fn relm_indices_are_empty_outside_japan() {
        assert!(relm_indices((48.85, 2.35, 48.85, 2.35)).is_empty(), "パリ");
        assert!(relm_indices((0.0, 0.0, 0.0, 0.0)).is_empty(), "ギニア湾");
        assert!(relm_indices((60.0, 140.0, 61.0, 141.0)).is_empty(), "日本の北");
    }

    #[test]
    fn every_relm_rectangle_is_ordered_south_west_north_east() {
        assert_eq!(RELM.len(), RELM_COUNT);
        for (i, r) in RELM.iter().enumerate() {
            assert!(r.0 < r.2, "{i}: 緯度の順序");
            assert!(r.1 < r.3, "{i}: 経度の順序");
        }
    }

    #[test]
    fn fetch_relm_refuses_an_index_outside_the_file_range() {
        // ネットワークに出る前に弾く(範囲外は URL を組む前にエラー)。
        assert!(fetch_relm(RELM_COUNT).is_err());
        assert!(fetch_relm(99).is_err());
    }

    // ---- ディスクキャッシュへ保存する形 ----

    #[test]
    fn muni_areas_round_trip_through_json() {
        let a = MuniArea {
            code: "1220800".to_string(),
            name: "野田市".to_string(),
            rings: vec![vec![(35.9, 139.8), (35.9, 139.9), (36.0, 139.9)]],
            bbox: (35.9, 139.8, 36.0, 139.9),
        };
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(
            json,
            r#"{"code":"1220800","name":"野田市","rings":[[[35.9,139.8],[35.9,139.9],[36.0,139.9]]],"bbox":[35.9,139.8,36.0,139.9]}"#
        );
        assert_eq!(serde_json::from_str::<MuniArea>(&json).unwrap(), a);
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    #[test]
    #[ignore]
    fn live_fetch_real_boundaries() {
        let mut total_areas = 0usize;
        let mut total_verts = 0usize;
        for i in 0..RELM_COUNT {
            let areas = fetch_relm(i).expect("live fetch should succeed");
            let verts: usize = areas.iter().map(|a| a.rings.iter().map(|r| r.len()).sum::<usize>()).sum();
            println!("class20s_{i}: {} 区域 / {verts} 頂点", areas.len());
            total_areas += areas.len();
            total_verts += verts;
        }
        println!("合計 {total_areas} 区域 / {total_verts} 頂点");
        // 実測値(2026/08/17): 1,806区域・148,424頂点。合併等で多少動くので幅を持たせる。
        assert!((1700..1900).contains(&total_areas), "区域数が想定から外れている: {total_areas}");

        // 代表的な市区町村が読めていること(コードの丸めまで含めて)。
        let kanto = fetch_relm(3).expect("live fetch should succeed");
        let noda = kanto.iter().find(|a| a.code == "1220800").expect("野田市が無い");
        assert_eq!(noda.name, "野田市");
        assert_eq!(noda.muni_code(), "12208");
        assert!(
            crate::geopoly::point_in_rings(&noda.rings, (35.955106, 139.874828)),
            "#75 の設計書に出てくる代表点が野田市のポリゴンに入らない"
        );
        let chugoku = fetch_relm(6).expect("live fetch should succeed");
        let hiroshima: Vec<&MuniArea> = chugoku.iter().filter(|a| a.muni_code() == "34100").collect();
        assert_eq!(hiroshima.len(), 8, "広島市が8区で持たれているという前提が崩れている");
        let kinki = fetch_relm(5).expect("live fetch should succeed");
        let kobe: Vec<&MuniArea> = kinki.iter().filter(|a| a.muni_code() == "28100").collect();
        assert_eq!(kobe.len(), 9, "神戸市が9区で持たれているという前提が崩れている");
    }
}
