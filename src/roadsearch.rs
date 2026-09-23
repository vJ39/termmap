// roadsearch: Overpassで道路名/refを検索し、線分断片(点列+oneway)を取得する。
// roadtrace::assemble_polyline に渡す前段。座標は (f64, f64) = (lat, lon)。
// このファイルは ureq(外部crate)を使うため、cargo経由でしかビルド/テストできない
// (parse_road_fragments は serde_json でデシリアライズする)。
use serde::Deserialize;

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

// Overpass QL の文字列リテラル用エスケープ(ダブルクォート/バックスラッシュのみ)。
// "="の完全一致値に使う。正規表現メタ文字はそのまま(意図的にエスケープしない)。
fn escape_ql(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                o.push('\\');
                o.push(c);
            }
            _ => o.push(c),
        }
    }
    o
}

// "~"の正規表現部分一致値に使う。regexメタ文字も含めて全てエスケープするので
// 入力文字列そのままの部分一致として振る舞う。
fn escape_regex(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' | '"' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' => {
                o.push('\\');
                o.push(c);
            }
            _ => o.push(c),
        }
    }
    o
}

// Overpass `out geom` 応答の要素。geometry=点列、tags.oneway で一方通行を判定する。
#[derive(Deserialize)]
struct RoadPoint { lat: f64, lon: f64 }
#[derive(Deserialize)]
struct RoadTags { #[serde(default)] oneway: Option<String> }
#[derive(Deserialize)]
struct RoadElement {
    #[serde(default)] geometry: Option<Vec<RoadPoint>>,
    #[serde(default)] tags: Option<RoadTags>,
}
#[derive(Deserialize)]
struct RoadResp { #[serde(default)] elements: Vec<RoadElement> }

/// Overpassの `out geom` 応答
/// (`{"elements":[{"geometry":[{"lat":..,"lon":..},...],"tags":{"oneway":"yes",...}},...]}`)
/// から、道路断片ごとの点列(lat,lon)とonewayフラグを取り出す。壊れたJSON・elementsキー無し・空配列は
/// すべて空Vecを返し(panicしない)、geometryキーが無い/点が1つも無い要素はスキップする。
pub fn parse_road_fragments(overpass_json: &str) -> Vec<(Vec<(f64, f64)>, bool)> {
    let resp: RoadResp = match serde_json::from_str(overpass_json) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    resp.elements
        .into_iter()
        .filter_map(|el| {
            let pts: Vec<(f64, f64)> = el.geometry?.into_iter().map(|p| (p.lat, p.lon)).collect();
            if pts.is_empty() {
                return None;
            }
            // oneway="yes" のときだけ true(no / -1 / タグ無しは false)。
            let oneway = el.tags.and_then(|t| t.oneway).as_deref() == Some("yes");
            Some((pts, oneway))
        })
        .collect()
}

/// 表示bbox(south,west,north,east)内で、道路名またはrefで道路を検索し、
/// 断片(点列+oneway)の一覧を返す。name_or_ref は ref完全一致 と name部分一致 の両方を
/// OR で試す(どちらにヒットしても対象)。
pub fn fetch(
    name_or_ref: &str,
    s: f64,
    w: f64,
    n: f64,
    e: f64,
) -> Result<Vec<(Vec<(f64, f64)>, bool)>, String> {
    let q = name_or_ref.trim();
    if q.is_empty() {
        return Err("道路名/refが空です".to_string());
    }

    let bbox = format!("{s:.5},{w:.5},{n:.5},{e:.5}");
    let ref_val = escape_ql(q);
    let name_pat = escape_regex(q);
    let query = format!(
        "[out:json][timeout:25];(way[\"ref\"=\"{ref_val}\"]({bbox});way[\"name\"~\"{name_pat}\"]({bbox}););out geom;"
    );
    let url = format!("https://overpass-api.de/api/interpreter?data={}", urlencode(&query));

    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|e| format!("overpass道路検索: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())?;

    Ok(parse_road_fragments(&body))
}

/// 表示bbox(south,west,north,east)内の主要道路(highway=trunk/primary)を、
/// 道路交通量(traffic.rs)の観測点をラインへスナップする下地として取得する。
/// fetchと違い名前/refでは絞らず、タグだけで広く取る(JARTICの観測点がどの路線名かは
/// 元データに無いため、まず近くの主要道路を全部集めてnearest_way_segmentで最寄りを選ぶ)。
pub fn fetch_major_roads(s: f64, w: f64, n: f64, e: f64) -> Result<Vec<(Vec<(f64, f64)>, bool)>, String> {
    let bbox = format!("{s:.5},{w:.5},{n:.5},{e:.5}");
    let query = format!(
        "[out:json][timeout:25];way[\"highway\"~\"^(trunk|primary)$\"]({bbox});out geom;"
    );
    let url = format!("https://overpass-api.de/api/interpreter?data={}", urlencode(&query));

    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|e| format!("overpass主要道路取得: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())?;

    Ok(parse_road_fragments(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_road_fragments_empty_input_is_safe() {
        assert_eq!(parse_road_fragments(""), Vec::new());
        assert_eq!(parse_road_fragments("not json at all"), Vec::new());
        assert_eq!(parse_road_fragments(r#"{"version":0.6}"#), Vec::new());
        assert_eq!(parse_road_fragments(r#"{"elements":[]}"#), Vec::new());
    }

    #[test]
    fn parse_road_fragments_truncated_json_is_safe() {
        // elements配列自体が閉じていない(通信エラー等で応答が途切れたケース)
        let body = r#"{"elements":[{"geometry":[{"lat":1.0,"lon":2.0}]"#;
        assert_eq!(parse_road_fragments(body), Vec::new());
    }

    #[test]
    fn parse_road_fragments_extracts_points_and_oneway() {
        let body = r#"{
            "version": 0.6,
            "generator": "Overpass API",
            "elements": [
                {
                    "type": "way",
                    "id": 111,
                    "geometry": [
                        {"lat": 35.1, "lon": 139.1},
                        {"lat": 35.2, "lon": 139.2},
                        {"lat": 35.3, "lon": 139.3}
                    ],
                    "tags": {"name": "国道1号", "ref": "1", "oneway": "yes", "highway": "trunk"}
                },
                {
                    "type": "way",
                    "id": 222,
                    "geometry": [
                        {"lat": 34.0, "lon": 138.0},
                        {"lat": 34.1, "lon": 138.1}
                    ],
                    "tags": {"name": "県道2号", "highway": "primary"}
                }
            ]
        }"#;

        let frags = parse_road_fragments(body);
        assert_eq!(frags.len(), 2);

        assert_eq!(
            frags[0].0,
            vec![(35.1, 139.1), (35.2, 139.2), (35.3, 139.3)]
        );
        assert!(frags[0].1, "oneway=yes は true になるべき");

        assert_eq!(frags[1].0, vec![(34.0, 138.0), (34.1, 138.1)]);
        assert!(!frags[1].1, "onewayタグ無しは false になるべき");
    }

    #[test]
    fn parse_road_fragments_skips_elements_without_geometry() {
        let body = r#"{"elements":[
            {"type":"way","id":1,"tags":{"name":"線形情報なし"}},
            {"type":"way","id":2,"geometry":[{"lat":1.0,"lon":2.0}],"tags":{}}
        ]}"#;
        let frags = parse_road_fragments(body);
        assert_eq!(frags.len(), 1);
        assert_eq!(frags[0].0, vec![(1.0, 2.0)]);
        assert!(!frags[0].1);
    }

    #[test]
    fn parse_road_fragments_oneway_only_true_for_yes_value() {
        let body = r#"{"elements":[
            {"geometry":[{"lat":1.0,"lon":1.0},{"lat":2.0,"lon":2.0}],"tags":{"oneway":"no"}},
            {"geometry":[{"lat":3.0,"lon":3.0},{"lat":4.0,"lon":4.0}],"tags":{"oneway":"-1"}}
        ]}"#;
        let frags = parse_road_fragments(body);
        assert_eq!(frags.len(), 2);
        assert!(!frags[0].1);
        assert!(!frags[1].1);
    }

    #[test]
    fn parse_road_fragments_skips_elements_with_an_empty_geometry() {
        let body = r#"{"elements":[{"geometry":[]},{"geometry":[{"lat":1.0,"lon":2.0}]}]}"#;
        assert_eq!(parse_road_fragments(body), vec![(vec![(1.0, 2.0)], false)]);
    }

    #[test]
    fn urlencode_keeps_unreserved_characters_and_encodes_the_rest() {
        assert_eq!(urlencode("Az09-_.~"), "Az09-_.~");
        assert_eq!(urlencode("[out:json];"), "%5Bout%3Ajson%5D%3B");
        assert_eq!(urlencode("国"), "%E5%9B%BD", "UTF-8のバイト単位で符号化");
    }

    // ref の完全一致値。引用符とバックスラッシュだけを逃がし、それ以外(正規表現記号も)はそのまま。
    #[test]
    fn escape_ql_only_escapes_quotes_and_backslashes() {
        assert_eq!(escape_ql(r#"E20"];out;"#), r#"E20\"];out;"#, "引用符で文字列リテラルを閉じさせない");
        assert_eq!(escape_ql(r"a\b"), r"a\\b");
        assert_eq!(escape_ql("国道1.*"), "国道1.*");
    }

    // name の部分一致パターン。正規表現記号も全部逃がして、入力そのままの部分一致にする。
    #[test]
    fn escape_regex_escapes_every_regex_metacharacter() {
        assert_eq!(escape_regex(r#"\".*+?()[]{}|^$"#), r#"\\\"\.\*\+\?\(\)\[\]\{\}\|\^\$"#);
        assert_eq!(escape_regex("国道16号"), "国道16号");
    }

    #[test]
    fn fetch_refuses_a_blank_query_without_touching_the_network() {
        assert_eq!(fetch("  ", 35.0, 139.0, 36.0, 140.0).unwrap_err(), "道路名/refが空です");
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    #[test]
    #[ignore]
    fn live_fetch_major_roads_returns_real_ways() {
        // 東京都内、国道1号/4号沿い等が含まれるはずの範囲。
        let frags = fetch_major_roads(35.6, 139.6, 35.8, 139.9).unwrap();
        println!("ways: {}", frags.len());
        assert!(!frags.is_empty(), "都内で主要道路0件は考えにくい");
        assert!(frags.iter().any(|(pts, _)| pts.len() >= 2), "2点以上のwayが1本も無い");
    }
}
