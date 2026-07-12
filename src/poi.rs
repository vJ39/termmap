// ジオコーディング(Nominatim) と 目的地検索(Overpass)
use crate::render::PoiCat;
use serde::Deserialize;

// 外部API呼び出しの失敗種別。「通信/サーバ障害」と「結果0件」を呼び出し側で区別するため。
// (0件は Ok(空Vec) で表す。ApiError は障害のみ)
#[derive(Debug)]
pub enum ApiError {
    Transport(String), // 接続失敗・タイムアウト等
    Http(u16),         // 4xx/5xx
    Decode(String),    // JSON パース失敗
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ApiError::Transport(e) => write!(f, "通信失敗: {e}"),
            ApiError::Http(c) => write!(f, "サーバ応答エラー({c})"),
            ApiError::Decode(e) => write!(f, "応答解析失敗: {e}"),
        }
    }
}

// ureq リクエストを実行し本文文字列を得る。status(4xx/5xx)/transport/decode を型で区別する。
fn call_text(req: ureq::Request) -> Result<String, ApiError> {
    match req.call() {
        Ok(r) => r.into_string().map_err(|e| ApiError::Transport(e.to_string())),
        Err(ureq::Error::Status(code, _)) => Err(ApiError::Http(code)),
        Err(ureq::Error::Transport(t)) => Err(ApiError::Transport(t.to_string())),
    }
}

// Nominatim /search の1件。lat/lon は文字列で返るので後段でパースする。
#[derive(Deserialize)]
struct NomItem { lat: String, lon: String, #[serde(default)] display_name: String }

// Google Geocoding の応答(必要なフィールドのみ)。
#[derive(Deserialize)]
struct GLoc { lat: f64, lng: f64 }
#[derive(Deserialize)]
struct GGeom { location: GLoc }
#[derive(Deserialize)]
struct GResult { geometry: GGeom, #[serde(default)] formatted_address: String }
#[derive(Deserialize)]
struct GResp { #[serde(default)] results: Vec<GResult> }

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
pub fn json_first(body: &str, key: &str) -> Option<String> {
    let i = body.find(key)? + key.len();
    let rest = &body[i..];
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}
// 地名/施設名を座標に。near があれば現在地周辺を優先し、他県へ飛ぶのを防ぐ。
// 優先順: ① Google Geocoding(キーあり・現在地bounds) → ② Nominatim(near周辺viewbox) → ③ Nominatim(全国)
pub fn geocode(place: &str, near: Option<(f64, f64)>, google_key: &str) -> Result<(f64, f64), String> {
    match geocode_list(place, near, google_key) {
        Ok(v) => v.into_iter().next().map(|(la, lo, _)| (la, lo))
            .ok_or_else(|| format!("住所が見つかりません: {place}")),
        Err(e) => Err(e.to_string()),
    }
}

// 候補を最大8件返す。まずフル住所で検索し、0件なら末尾の地番を落として大字/町名レベルで再検索する。
// (Nominatim=OSMは日本の地番/建物レベルの住所を持たないため、番地付き文字列は round-trip しない)
// Ok(空)=該当なし / Err=通信・サーバ・解析の障害、として呼び出し側で区別できる。
pub fn geocode_list(place: &str, near: Option<(f64, f64)>, google_key: &str) -> Result<Vec<(f64, f64, String)>, ApiError> {
    let r = geocode_once(place, near, google_key)?;
    if !r.is_empty() { return Ok(r); }
    let trimmed = strip_trailing_banchi(place);
    if trimmed != place && !trimmed.trim().is_empty() {
        return geocode_once(&trimmed, near, google_key);
    }
    Ok(Vec::new())
}

// 1回分の検索。①Google(現在地bounds) → ②Nominatim(near周辺viewbox) → ③Nominatim(全国)
fn geocode_once(place: &str, near: Option<(f64, f64)>, google_key: &str) -> Result<Vec<(f64, f64, String)>, ApiError> {
    if !google_key.trim().is_empty() {
        // Google が障害でも Nominatim にフォールバックする(検索全体を落とさない)。0件なら次へ。
        if let Ok(g) = google_geocode_list(place, near, google_key) {
            if !g.is_empty() { return Ok(g); }
        }
    }
    if let Some((lat, lon)) = near {
        let d = 0.35; // ≈ ±35km
        let vb = format!("{},{},{},{}", lon - d, lat - d, lon + d, lat + d);
        let url = format!("https://nominatim.openstreetmap.org/search?format=json&limit=8&accept-language=ja&bounded=1&viewbox={}&q={}", vb, urlencode(place));
        let l = nominatim_list(&url)?;
        if !l.is_empty() { return Ok(l); }
    }
    let url = format!("https://nominatim.openstreetmap.org/search?format=json&limit=8&accept-language=ja&q={}", urlencode(place));
    nominatim_list(&url)
}

// 末尾の地番(丁目/番地/番/号 と 数字・ハイフン・「の」)を落として大字/町名レベルへ丸める。
// 例: 「山梨県南都留郡山中湖村山中23」→「山梨県南都留郡山中湖村山中」/「港区六本木6丁目10-1」→「港区六本木」
fn strip_trailing_banchi(s: &str) -> String {
    let mut t = s.trim().to_string();
    loop {
        let before = t.len();
        for suf in ["丁目", "番地", "番", "号"] {
            if t.ends_with(suf) { let n = t.len() - suf.len(); t.truncate(n); }
        }
        while let Some(c) = t.chars().last() {
            let is_banchi = c.is_ascii_digit() || ('０'..='９').contains(&c)
                || c == '-' || c == 'ー' || c == '－' || c == 'の' || c.is_whitespace();
            if is_banchi { t.pop(); } else { break; }
        }
        if t.len() == before { break; }
    }
    t
}

fn nominatim_list(url: &str) -> Result<Vec<(f64, f64, String)>, ApiError> {
    let req = ureq::get(url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .timeout(std::time::Duration::from_secs(20));
    let body = call_text(req)?;
    let items: Vec<NomItem> = serde_json::from_str(&body).map_err(|e| ApiError::Decode(e.to_string()))?;
    Ok(items.into_iter().filter_map(|it| {
        let la = it.lat.parse::<f64>().ok()?;
        let lo = it.lon.parse::<f64>().ok()?;
        Some((la, lo, it.display_name))
    }).take(8).collect())
}

fn google_geocode_list(place: &str, near: Option<(f64, f64)>, key: &str) -> Result<Vec<(f64, f64, String)>, ApiError> {
    let mut url = format!("https://maps.googleapis.com/maps/api/geocode/json?language=ja&region=jp&address={}&key={}", urlencode(place), key);
    if let Some((lat, lon)) = near {
        let d = 0.3;
        url.push_str(&format!("&bounds={},{}|{},{}", lat - d, lon - d, lat + d, lon + d)); // sw|ne
    }
    let body = call_text(ureq::get(&url).timeout(std::time::Duration::from_secs(20)))?;
    // status が REQUEST_DENIED 等でも results が空なら Ok(空)。Nominatim にフォールバックさせる。
    let resp: GResp = serde_json::from_str(&body).map_err(|e| ApiError::Decode(e.to_string()))?;
    Ok(resp.results.into_iter().take(8)
        .map(|r| (r.geometry.location.lat, r.geometry.location.lng, r.formatted_address))
        .collect())
}

// 逆ジオコーディング (Nominatim reverse) → 住所文字列(display_name)
pub fn reverse_geocode(lat: f64, lon: f64) -> Result<String, String> {
    let url = format!("https://nominatim.openstreetmap.org/reverse?format=json&accept-language=ja&zoom=18&lat={lat}&lon={lon}");
    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .timeout(std::time::Duration::from_secs(20)).call().map_err(|e| format!("revgeo: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    json_first(&body, "\"display_name\":\"").ok_or_else(|| "住所が取得できません".to_string())
}

// キーワードを区切り(ハイフン/中黒/空白)を任意許容する Overpass 正規表現に。
// 「セブンイレブン」で name=「セブン-イレブン」を拾えるようにする。
fn overpass_name_pattern(q: &str) -> String {
    let parts: Vec<String> = q.trim().chars().filter(|c| !c.is_whitespace() && *c != '　').map(|c| {
        match c { '\\' | '"' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' => format!("\\{c}"), _ => c.to_string() }
    }).collect();
    parts.join("[-ー・‐ 　]?")
}
// 現在表示範囲(bbox)でキーワード周辺検索。name/brand に q を含む地物を Overpass で(部分一致・区切り許容・大小無視)。
// Nominatim(ジオコーダ)はチェーン店の近傍列挙に弱いので Overpass の name/brand 正規表現検索を使う。
pub fn search_nearby(q: &str, s: f64, w: f64, n: f64, e: f64) -> Vec<(f64, f64, String)> {
    let pat = overpass_name_pattern(q);
    let b = format!("{:.5},{:.5},{:.5},{:.5}", s, w, n, e);
    let query = format!(
        "[out:json][timeout:25];(nwr[\"name\"~\"{pat}\",i]({b});nwr[\"brand\"~\"{pat}\",i]({b}););out center;"
    );
    let url = format!("https://overpass-api.de/api/interpreter?data={}", urlencode(&query));
    let body = match ureq::get(&url).set("User-Agent", "termmap/0.1 (personal experiment)").set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(20)).call() {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(_) => return Vec::new(),
    };
    parse_overpass(&body)
}

// ---- 目的地検索 (Overpass) ----
pub struct PoiKind { pub key: char, pub label: &'static str, pub filter: &'static str, pub cat: PoiCat }
pub const POI_KINDS: [PoiKind; 7] = [
    PoiKind { key: '1', label: "ガソスタ", filter: "nwr[\"amenity\"=\"fuel\"]", cat: PoiCat::Fuel },
    PoiKind { key: '2', label: "カフェ", filter: "nwr[\"amenity\"=\"cafe\"]", cat: PoiCat::Food },
    PoiKind { key: '3', label: "コンビニ", filter: "nwr[\"shop\"=\"convenience\"]", cat: PoiCat::Shop },
    PoiKind { key: '4', label: "道の駅", filter: "nwr[\"name\"~\"道の駅\"][\"highway\"!~\"traffic_signals|bus_stop\"]", cat: PoiCat::Waypoint },
    PoiKind { key: '5', label: "展望", filter: "nwr[\"tourism\"=\"viewpoint\"]", cat: PoiCat::Other },
    PoiKind { key: '6', label: "公園", filter: "nwr[\"leisure\"=\"park\"]", cat: PoiCat::Other },
    PoiKind { key: '7', label: "峠道", filter: "nwr[\"mountain_pass\"=\"yes\"]", cat: PoiCat::Danger },
];
// 文字列フィールド抽出。key は裸のキー名("name" 等)。コロン後の空白を許容。
fn json_str(s: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = s.find(&pat)? + pat.len();
    let rest = &s[i..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q1 = after.find('"')?;
    let q2 = after[q1 + 1..].find('"')?;
    Some(after[q1 + 1..q1 + 1 + q2].to_string())
}
// 数値フィールド抽出("lat": 35.7 のような)
fn json_num(s: &str, key: &str) -> Option<f64> {
    let i = s.find(key)? + key.len();
    let rest = s[i..].trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}
// Overpass out:json の elements から (lat,lon,name) を取り出す。文字列内の括弧を無視する走査。
fn parse_overpass(body: &str) -> Vec<(f64, f64, String)> {
    let mut out = Vec::new();
    let ei = match body.find("\"elements\"") { Some(i) => i, None => return out };
    let bytes = body.as_bytes();
    let mut i = ei;
    while i < bytes.len() && bytes[i] != b'[' { i += 1; }
    let (mut depth, mut obj_start, mut in_obj, mut in_str, mut esc) = (0i32, 0usize, false, false, false);
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            if esc { esc = false; } else if b == b'\\' { esc = true; } else if b == b'"' { in_str = false; }
        } else {
            match b {
                b'"' => in_str = true,
                b'{' => { if depth == 0 { obj_start = i; in_obj = true; } depth += 1; }
                b'}' => { depth -= 1; if depth == 0 && in_obj {
                    let obj = &body[obj_start..=i];
                    if let (Some(la), Some(lo)) = (json_num(obj, "\"lat\":"), json_num(obj, "\"lon\":")) {
                        out.push((la, lo, json_str(obj, "name").unwrap_or_default()));
                    }
                    in_obj = false;
                }}
                b']' => { if depth == 0 { break; } }
                _ => {}
            }
        }
        i += 1;
    }
    out
}
// 表示bbox(south,west,north,east)で kind を検索
pub fn fetch_pois(kind: &PoiKind, s: f64, w: f64, n: f64, e: f64) -> Result<Vec<(f64, f64, String)>, String> {
    let q = format!("[out:json][timeout:25];({}({:.5},{:.5},{:.5},{:.5}););out center;", kind.filter, s, w, n, e);
    let url = format!("https://overpass-api.de/api/interpreter?data={}", urlencode(&q));
    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(20)).call().map_err(|e| format!("overpass: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    Ok(parse_overpass(&body))
}

// 中心付近(表示範囲2.5倍と半径2kmの広い方)で kind を検索し、名前重複除去・中心から近い順・最大50件で返す。
pub fn poi_search(kind: &PoiKind, cx: f64, cy: f64, z: u32, ow: u32, oh: u32, lat: f64, lon: f64) -> Result<Vec<(f64, f64, String, PoiCat)>, String> {
    let (vt, vl) = crate::geo::pixel_to_deg(cx - ow as f64 * 1.25, cy - oh as f64 * 1.25, z);
    let (vb, vr) = crate::geo::pixel_to_deg(cx + ow as f64 * 1.25, cy + oh as f64 * 1.25, z);
    let rlat = 2.0 / 111.0;
    let rlon = 2.0 / (111.0 * lat.to_radians().cos().abs().max(0.1));
    let v = fetch_pois(kind, vb.min(lat - rlat), vl.min(lon - rlon), vt.max(lat + rlat), vr.max(lon + rlon))?;
    let mut items: Vec<(f64, f64, String, PoiCat)> = v.into_iter().map(|(la, lo, nm)| (la, lo, nm, kind.cat)).collect();
    items.sort_by(|p, q| p.2.cmp(&q.2));
    items.dedup_by(|p, q| !p.2.is_empty() && p.2 == q.2);
    items.sort_by(|p, q| crate::geo::haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&crate::geo::haversine_km((lat, lon), (q.0, q.1))).unwrap_or(std::cmp::Ordering::Equal));
    items.truncate(50);
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_overpass_node_and_way() {
        let body = r#"{"elements":[
          {"type":"node","lat":35.75,"lon":139.73,"tags":{"name":"あ店","amenity":"fuel"}},
          {"type":"way","center":{"lat":35.76,"lon":139.74},"tags":{"name":"い店"}}
        ]}"#;
        let v = parse_overpass(body);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].2, "あ店"); // 空白後コロンでも名前を取れる
        assert!((v[1].0 - 35.76).abs() < 1e-9); // wayはcenter
    }

    #[test]
    fn json_helpers() {
        assert_eq!(json_str(r#"{"name": "王子駅"}"#, "name").as_deref(), Some("王子駅"));
        assert!((json_num(r#"{"lat": 35.7}"#, "\"lat\":").unwrap() - 35.7).abs() < 1e-9);
    }

    #[test]
    fn strip_banchi_drops_trailing_address_number() {
        // 番地23を落として大字レベルへ(Nominatimはこの粗さでヒットする)
        assert_eq!(strip_trailing_banchi("山梨県南都留郡山中湖村山中23"), "山梨県南都留郡山中湖村山中");
        // 丁目+ハイフン地番
        assert_eq!(strip_trailing_banchi("港区六本木6丁目10-1"), "港区六本木");
        // 全角数字・番地表記
        assert_eq!(strip_trailing_banchi("渋谷区神南１番地"), "渋谷区神南");
        // 地番が無ければそのまま
        assert_eq!(strip_trailing_banchi("山中湖"), "山中湖");
        // 施設名(末尾が数字でない)は不変
        assert_eq!(strip_trailing_banchi("東京駅"), "東京駅");
    }

    #[test]
    fn google_resp_parses_via_serde() {
        let body = r#"{"results":[{"geometry":{"location":{"lat":35.6094,"lng":139.7402}},"formatted_address":"日本、東京"}],"status":"OK"}"#;
        let resp: GResp = serde_json::from_str(body).unwrap();
        assert_eq!(resp.results.len(), 1);
        assert!((resp.results[0].geometry.location.lat - 35.6094).abs() < 1e-6);
        // REQUEST_DENIED 等 results 無しは空(status は無視)
        let denied: GResp = serde_json::from_str(r#"{"status":"REQUEST_DENIED"}"#).unwrap();
        assert!(denied.results.is_empty());
    }

    #[test]
    fn nominatim_item_parses_escaped_and_unicode() {
        // エスケープされた引用符・Unicode escape を含む display_name を serde が正しく復元する
        // (手書き切り出しだと壊れやすかったケース)
        let body = r#"[{"lat":"35.68","lon":"139.76","display_name":"ラーメン\"横綱\""}]"#;
        let items: Vec<NomItem> = serde_json::from_str(body).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].display_name, "ラーメン\"横綱\"");
        assert_eq!(items[0].lat, "35.68");
    }
}
