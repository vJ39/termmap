// roadsearch: Overpassで道路名/refを検索し、線分断片(点列+oneway)を取得する。
// roadtrace::assemble_polyline に渡す前段。座標は (f64, f64) = (lat, lon)。
// このファイルは ureq(外部crate)を使うため、cargo経由でしかビルド/テストできない
// (parse_road_fragments のロジック自体は自前JSON走査でstdのみ)。

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

/// s の中で open_idx にある開き括弧(open)に対応する閉じ括弧(close)のindexを、
/// 文字列リテラル内を無視して探す。深さは open/close の種類だけをカウントするので、
/// 内部に異種の括弧(例: [...]の中の{...})が混在しても正しく対応点を見つけられる。
fn matching_bracket(s: &str, open_idx: usize, open: u8, close: u8) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(open_idx) != Some(&open) {
        return None;
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut i = open_idx;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else if b == b'"' {
            in_str = true;
        } else if b == open {
            depth += 1;
        } else if b == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// s の中にある「深さ0の {...} オブジェクト」を出現順に全て切り出す(スライスの参照を返す)。
/// 文字列リテラル内の波括弧は無視する。ネストしたオブジェクトの内側は個別には返さない。
fn top_level_objects(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut in_str = false;
    let mut esc = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
        } else {
            match b {
                b'"' => in_str = true,
                b'{' => {
                    if depth == 0 {
                        start = i;
                    }
                    depth += 1;
                }
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(&s[start..=i]);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    out
}

/// `"key": 数値` の数値を取り出す(コロン前後の空白を許容)。
fn json_num(s: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\"");
    let i = s.find(&pat)? + pat.len();
    let rest = s[i..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `"key":"value"` の文字列値を取り出す(コロン前後の空白を許容)。
fn json_str(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = s.find(&pat)? + pat.len();
    let rest = s[i..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 1つの要素("way"等)オブジェクトから、geometryの点列とoneway("yes"のときtrue)を取り出す。
/// geometryキーが無い/点が1つも取れない要素は None (呼び出し側でスキップする)。
fn parse_element(obj: &str) -> Option<(Vec<(f64, f64)>, bool)> {
    let gi = obj.find("\"geometry\"")?;
    let bracket_off = obj[gi..].find('[')?;
    let open = gi + bracket_off;
    let close = matching_bracket(obj, open, b'[', b']')?;
    let geom = &obj[open + 1..close];

    let mut pts = Vec::new();
    for point_obj in top_level_objects(geom) {
        if let (Some(lat), Some(lon)) = (json_num(point_obj, "lat"), json_num(point_obj, "lon")) {
            pts.push((lat, lon));
        }
    }
    if pts.is_empty() {
        return None;
    }

    let oneway = json_str(obj, "oneway").as_deref() == Some("yes");
    Some((pts, oneway))
}

/// Overpassの `out geom` 応答
/// (`{"elements":[{"geometry":[{"lat":..,"lon":..},...],"tags":{"oneway":"yes",...}},...]}`)
/// から、道路断片ごとの点列(lat,lon)とonewayフラグを取り出す。serde不使用の自前JSON走査。
/// 壊れたJSON・elementsキー無し・空配列はすべて空Vecを返す(panicしない)。
pub fn parse_road_fragments(overpass_json: &str) -> Vec<(Vec<(f64, f64)>, bool)> {
    let ei = match overpass_json.find("\"elements\"") {
        Some(i) => i,
        None => return Vec::new(),
    };
    let bracket_off = match overpass_json[ei..].find('[') {
        Some(off) => off,
        None => return Vec::new(),
    };
    let open = ei + bracket_off;
    let close = match matching_bracket(overpass_json, open, b'[', b']') {
        Some(c) => c,
        None => return Vec::new(),
    };
    let elements = &overpass_json[open + 1..close];

    top_level_objects(elements)
        .into_iter()
        .filter_map(parse_element)
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
}
