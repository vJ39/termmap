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
// Ok(空)=該当なし / Err=通信・サーバ・解析の障害、として呼び出し側で区別できる。
pub fn search_nearby(q: &str, s: f64, w: f64, n: f64, e: f64) -> Result<Vec<(f64, f64, String)>, ApiError> {
    let pat = overpass_name_pattern(q);
    let b = format!("{:.5},{:.5},{:.5},{:.5}", s, w, n, e);
    let query = format!(
        "[out:json][timeout:25];(nwr[\"name\"~\"{pat}\",i]({b});nwr[\"brand\"~\"{pat}\",i]({b}););out center;"
    );
    let url = format!("https://overpass-api.de/api/interpreter?data={}", urlencode(&query));
    let req = ureq::get(&url).set("User-Agent", "termmap/0.1 (personal experiment)").set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(20));
    let body = call_text(req)?;
    parse_overpass(&body)
}

// ---- 目的地検索 (Overpass) ----
// label/filterは所有String(ユーザーが並べ替え/追加できるよう永続化するため&'static strから変更)。
#[derive(Clone)]
pub struct PoiKind { pub key: char, pub label: String, pub filter: String, pub cat: PoiCat }

// 既定の目的地カテゴリ8種。初回起動(永続ファイル未作成)はこれを使う。
pub fn poi_kind_defaults() -> Vec<PoiKind> {
    vec![
        PoiKind { key: '1', label: "ガソスタ".into(), filter: "nwr[\"amenity\"=\"fuel\"]".into(), cat: PoiCat::Fuel },
        PoiKind { key: '2', label: "カフェ".into(), filter: "nwr[\"amenity\"=\"cafe\"]".into(), cat: PoiCat::Food },
        PoiKind { key: '3', label: "コンビニ".into(), filter: "nwr[\"shop\"=\"convenience\"]".into(), cat: PoiCat::Shop },
        PoiKind { key: '4', label: "道の駅".into(), filter: "nwr[\"name\"~\"道の駅\"][\"highway\"!~\"traffic_signals|bus_stop\"]".into(), cat: PoiCat::Waypoint },
        PoiKind { key: '5', label: "展望".into(), filter: "nwr[\"tourism\"=\"viewpoint\"]".into(), cat: PoiCat::Other },
        PoiKind { key: '6', label: "公園".into(), filter: "nwr[\"leisure\"=\"park\"]".into(), cat: PoiCat::Other },
        PoiKind { key: '7', label: "峠道".into(), filter: "nwr[\"mountain_pass\"=\"yes\"]".into(), cat: PoiCat::Danger },
        // 二輪/バイク駐車場。OSMは名称に依らず amenity=motorcycle_parking でタグ付けされる
        PoiKind { key: '8', label: "バイク駐車場".into(), filter: "nwr[\"amenity\"=\"motorcycle_parking\"]".into(), cat: PoiCat::Other },
    ]
}

fn poi_cat_to_str(c: PoiCat) -> &'static str {
    match c { PoiCat::Home => "home", PoiCat::Food => "food", PoiCat::Fuel => "fuel", PoiCat::Shop => "shop", PoiCat::Danger => "danger", PoiCat::Waypoint => "waypoint", PoiCat::Other => "other" }
}
fn poi_cat_from_str(s: &str) -> PoiCat {
    match s { "home" => PoiCat::Home, "food" => PoiCat::Food, "fuel" => PoiCat::Fuel, "shop" => PoiCat::Shop, "danger" => PoiCat::Danger, "waypoint" => PoiCat::Waypoint, _ => PoiCat::Other }
}
// ラベル/タグにタブ・改行を入れない(永続ファイルの区切りと衝突するため)
pub fn poi_kind_clean(s: &str) -> String { s.trim().replace(['\t', '\n'], " ") }

fn poi_kinds_path() -> Option<std::path::PathBuf> {
    Some(std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".config/termmap/poi-kinds.txt"))
}
// 行形式: key\tlabel\tfilter\tcat。パース不能な行は読み飛ばす(壊れたファイルでpanicしない)。
fn parse_poi_kinds_text(s: &str) -> Vec<PoiKind> {
    let mut v = Vec::new();
    for l in s.lines() {
        let mut it = l.splitn(4, '\t');
        if let (Some(k), Some(label), Some(filter), Some(cat)) = (it.next(), it.next(), it.next(), it.next()) {
            if let Some(key) = k.chars().next() {
                v.push(PoiKind { key, label: label.to_string(), filter: filter.to_string(), cat: poi_cat_from_str(cat.trim()) });
            }
        }
    }
    v
}
fn format_poi_kinds_text(v: &[PoiKind]) -> String {
    v.iter().map(|k| format!("{}\t{}\t{}\t{}\n", k.key, k.label, k.filter, poi_cat_to_str(k.cat))).collect()
}
// 永続ファイルが無ければ既定8種を返す(ファイルは触らない=カスタマイズするまで挙動不変)。
pub fn load_poi_kinds() -> Vec<PoiKind> {
    let Some(p) = poi_kinds_path() else { return poi_kind_defaults(); };
    let Ok(s) = std::fs::read_to_string(&p) else { return poi_kind_defaults(); };
    let v = parse_poi_kinds_text(&s);
    if v.is_empty() { poi_kind_defaults() } else { v }
}
pub fn save_poi_kinds(v: &[PoiKind]) -> Result<(), String> {
    let p = poi_kinds_path().ok_or("HOME不明")?;
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    std::fs::write(p, format_poi_kinds_text(v)).map_err(|e| e.to_string())
}
// 未使用のキー(数字優先→英小文字)を1つ選ぶ。n/xはメニュー操作(新規/削除)の予約キーなので外す。
// 全て埋まっていたら'?'を返す(表示上の目印)。
pub fn next_free_key(v: &[PoiKind]) -> char {
    for c in "1234567890abcdefghijklmopqrstuvwyz".chars() {
        if !v.iter().any(|k| k.key == c) { return c; }
    }
    '?'
}
// Overpass out:json の要素。node は lat/lon、way/relation は center を使う。
#[derive(Deserialize)]
struct OverLatLon { lat: f64, lon: f64 }
#[derive(Deserialize)]
struct OverTags { #[serde(default)] name: String }
#[derive(Deserialize)]
struct OverElement {
    #[serde(default)] lat: Option<f64>,
    #[serde(default)] lon: Option<f64>,
    #[serde(default)] center: Option<OverLatLon>,
    #[serde(default)] tags: Option<OverTags>,
}
#[derive(Deserialize)]
struct OverResp { #[serde(default)] elements: Vec<OverElement> }
// Overpass out:json の elements から (lat,lon,name) を取り出す。
// node は自身の lat/lon、way は center の lat/lon を使う(既存挙動を維持)。
// パース不能な応答は ApiError::Decode(呼び出し側で「0件」と区別できる障害として扱う)。
fn parse_overpass(body: &str) -> Result<Vec<(f64, f64, String)>, ApiError> {
    let resp: OverResp = serde_json::from_str(body).map_err(|e| ApiError::Decode(e.to_string()))?;
    let mut out = Vec::new();
    for el in resp.elements {
        // node=自身の lat/lon / way=center。どちらも無ければスキップ。
        let (la, lo) = match (el.lat, el.lon) {
            (Some(la), Some(lo)) => (la, lo),
            _ => match el.center { Some(c) => (c.lat, c.lon), None => continue },
        };
        let name = el.tags.map(|t| t.name).unwrap_or_default();
        out.push((la, lo, name));
    }
    Ok(out)
}
// 表示bbox(south,west,north,east)で kind を検索。Ok(空)=該当なし / Err=通信・サーバ・解析の障害。
pub fn fetch_pois(kind: &PoiKind, s: f64, w: f64, n: f64, e: f64) -> Result<Vec<(f64, f64, String)>, ApiError> {
    let q = format!("[out:json][timeout:25];({}({:.5},{:.5},{:.5},{:.5}););out center;", kind.filter, s, w, n, e);
    let url = format!("https://overpass-api.de/api/interpreter?data={}", urlencode(&q));
    let req = ureq::get(&url).set("User-Agent", "termmap/0.1 (personal experiment)").set("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(20));
    let body = call_text(req)?;
    parse_overpass(&body)
}

// 中心付近(表示範囲2.5倍と半径2kmの広い方)で kind を検索し、名前重複除去・中心から近い順・最大50件で返す。
// Ok(空)=該当なし / Err=通信・サーバ・解析の障害。
pub fn poi_search(kind: &PoiKind, cx: f64, cy: f64, z: u32, ow: u32, oh: u32, lat: f64, lon: f64) -> Result<Vec<(f64, f64, String, PoiCat)>, ApiError> {
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
        let v = parse_overpass(body).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].2, "あ店"); // node は自身の lat/lon
        assert!((v[1].0 - 35.76).abs() < 1e-9); // wayはcenter
    }

    #[test]
    fn parse_overpass_edge_cases() {
        // tags 無しの要素は name 空。center も lat/lon も無い要素はスキップ。
        let body = r#"{"elements":[
          {"type":"node","lat":35.0,"lon":139.0},
          {"type":"relation","tags":{"name":"座標なし"}}
        ]}"#;
        let v = parse_overpass(body).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].2, ""); // tags 無し → 空名
        // 「elements キー無し」は0件(該当なし)であって障害ではない
        assert_eq!(parse_overpass(r#"{"version":0.6}"#).unwrap().len(), 0);
    }

    #[test]
    fn parse_overpass_malformed_json_is_decode_error_not_empty() {
        // 壊れたJSON応答は「0件」ではなく障害(ApiError::Decode)として区別できる(panicしない)
        match parse_overpass("not json") {
            Err(ApiError::Decode(_)) => {}
            other => panic!("expected Decode error, got {other:?}"),
        }
    }

    #[test]
    fn poi_kinds_text_round_trip_preserves_order_and_fields() {
        // 並べ替え/追加後の保存→読込で順序・key・label・filter・catが保たれる(カスタムカテゴリ永続化)
        let v = vec![
            PoiKind { key: '2', label: "カフェ".to_string(), filter: "nwr[\"amenity\"=\"cafe\"]".to_string(), cat: PoiCat::Food },
            PoiKind { key: '1', label: "ガソスタ".to_string(), filter: "nwr[\"amenity\"=\"fuel\"]".to_string(), cat: PoiCat::Fuel },
            PoiKind { key: 'b', label: "パン屋".to_string(), filter: "nwr[\"shop\"=\"bakery\"]".to_string(), cat: PoiCat::Other },
        ];
        let text = format_poi_kinds_text(&v);
        let back = parse_poi_kinds_text(&text);
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].key, '2'); // 並び順が保たれる(並べ替えの結果を表す)
        assert_eq!(back[1].key, '1');
        assert_eq!(back[2].label, "パン屋");
        assert_eq!(back[2].filter, "nwr[\"shop\"=\"bakery\"]");
        assert!(matches!(back[2].cat, PoiCat::Other));
        assert!(matches!(back[0].cat, PoiCat::Food));
    }

    #[test]
    fn next_free_key_skips_reserved_and_used() {
        // n/x はメニュー操作(新規/削除)の予約キーなので割り当てない
        let v = vec![PoiKind { key: '1', label: "a".into(), filter: "f".into(), cat: PoiCat::Other }];
        let k = next_free_key(&v);
        assert_ne!(k, '1'); // 使用済み
        assert_ne!(k, 'n');
        assert_ne!(k, 'x');
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
