// ジオコーディング(Nominatim) と 目的地検索(Overpass)
use crate::render::PoiCat;

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
    if !google_key.trim().is_empty() {
        if let Ok(v) = google_geocode(place, near, google_key) {
            return Ok(v);
        }
    }
    if let Some((lat, lon)) = near {
        let d = 0.35; // ≈ ±35km
        let vb = format!("{},{},{},{}", lon - d, lat - d, lon + d, lat + d);
        let url = format!("https://nominatim.openstreetmap.org/search?format=json&limit=1&accept-language=ja&bounded=1&viewbox={}&q={}", vb, urlencode(place));
        if let Ok(v) = nominatim_req(&url) {
            return Ok(v);
        }
    }
    let url = format!("https://nominatim.openstreetmap.org/search?format=json&limit=1&accept-language=ja&q={}", urlencode(place));
    nominatim_req(&url).map_err(|_| format!("住所が見つかりません: {place}"))
}

fn nominatim_req(url: &str) -> Result<(f64, f64), String> {
    let body = ureq::get(url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .timeout(std::time::Duration::from_secs(20)).call().map_err(|e| format!("geocode: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    let lat = json_first(&body, "\"lat\":\"").ok_or("no result")?;
    let lon = json_first(&body, "\"lon\":\"").ok_or("no result")?;
    let lat: f64 = lat.parse().map_err(|_| "lat parse失敗".to_string())?;
    let lon: f64 = lon.parse().map_err(|_| "lon parse失敗".to_string())?;
    Ok((lat, lon))
}

fn google_geocode(place: &str, near: Option<(f64, f64)>, key: &str) -> Result<(f64, f64), String> {
    let mut url = format!("https://maps.googleapis.com/maps/api/geocode/json?language=ja&region=jp&address={}&key={}", urlencode(place), key);
    if let Some((lat, lon)) = near {
        let d = 0.3;
        url.push_str(&format!("&bounds={},{}|{},{}", lat - d, lon - d, lat + d, lon + d)); // sw|ne
    }
    let body = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(20)).call().map_err(|e| format!("google geocode: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    parse_google_latlng(&body).ok_or_else(|| "google no result".to_string())
}

// Google Geocoding応答から最初の geometry.location の (lat,lng) を拾う。数値形式("lat" : 35.6)。
fn parse_google_latlng(body: &str) -> Option<(f64, f64)> {
    let loc = body.find("\"location\"")?;
    let s = &body[loc..];
    let lat = num_after_key(s, "\"lat\"")?;
    let lng = num_after_key(s, "\"lng\"")?;
    Some((lat, lng))
}

fn num_after_key(s: &str, key: &str) -> Option<f64> {
    let i = s.find(key)?;
    let b = s[i + key.len()..].as_bytes();
    let mut a = 0;
    while a < b.len() && !(b[a].is_ascii_digit() || b[a] == b'-' || b[a] == b'.') { a += 1; }
    let mut e = a;
    while e < b.len() && (b[e].is_ascii_digit() || b[e] == b'-' || b[e] == b'.') { e += 1; }
    std::str::from_utf8(&b[a..e]).ok()?.parse().ok()
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
    fn google_latlng_from_geometry() {
        let body = r#"{"results":[{"geometry":{"location":{"lat":35.6094,"lng":139.7402}}}],"status":"OK"}"#;
        let (la, lo) = parse_google_latlng(body).unwrap();
        assert!((la - 35.6094).abs() < 1e-6 && (lo - 139.7402).abs() < 1e-6);
        // REQUEST_DENIED等 location無しは None
        assert!(parse_google_latlng(r#"{"status":"REQUEST_DENIED"}"#).is_none());
    }
}
