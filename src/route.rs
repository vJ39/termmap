// ルーティング (BRouter 公開API)・高速料金/expressway 計算・GPX 出力
use crate::render::{OverlaySpec, Poi, PoiCat};
use serde::Deserialize;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct RouteResult { pub pts: Vec<(f64, f64)>, pub ele: Vec<f64>, pub dist_m: f64, pub time_s: f64, pub hw_m: f64, pub ascend_m: f64, #[serde(default)] pub via_google: bool }

// BRouter geojson の応答。features[0] の geometry.coordinates([[lon,lat,ele?],...]) と
// properties(track-length/total-time/filtered ascend は全て文字列, messages は文字列表)を読む。
#[derive(Deserialize)]
struct BrGeometry { #[serde(default)] coordinates: Vec<Vec<f64>> }
#[derive(Deserialize)]
struct BrProperties {
    #[serde(rename = "track-length", default)] track_length: Option<String>,
    #[serde(rename = "total-time", default)] total_time: Option<String>,
    #[serde(rename = "filtered ascend", default)] filtered_ascend: Option<String>,
    #[serde(default)] messages: Vec<Vec<String>>,
}
#[derive(Deserialize)]
struct BrFeature {
    #[serde(default)] geometry: Option<BrGeometry>,
    #[serde(default)] properties: Option<BrProperties>,
}
#[derive(Deserialize)]
struct BrResp { #[serde(default)] features: Vec<BrFeature> }
// 応答本文を serde でパース。壊れていれば None(各パーサが既定値を返せるように)。
fn parse_brouter(body: &str) -> Option<BrResp> { serde_json::from_str(body).ok() }
// features から最初の geometry.coordinates を取り出す。
fn first_coords(body: &str) -> Option<Vec<Vec<f64>>> {
    parse_brouter(body)?.features.into_iter().find_map(|f| f.geometry.map(|g| g.coordinates))
}
// short=最短 / highway=高速OK(car-fast) / それ以外=下道(高速回避, moped). 既知名は透過。
fn route_profile(mode: &str) -> &str {
    match mode {
        "short" | "shortest" => "shortest",
        "highway" | "fast" | "高速" => "car-fast",
        "surface" | "下道" | "quiet" | "car" => "moped",
        other => other,
    }
}
pub fn mode_label(mode: &str) -> &'static str {
    match mode {
        "short" | "shortest" => "最短",
        "highway" | "fast" | "高速" => "高速",
        _ => "下道",
    }
}
// 距離/時間/(高速なら)料金概算の要約。料金=高速区間km×¥30(普通車概算, 割引なし)。
pub fn route_summary(mode: &str, r: &RouteResult) -> String {
    let mut s = format!("{} {:.1}km {}分", mode_label(mode), r.dist_m / 1000.0, (r.time_s / 60.0).round() as i64);
    if r.hw_m > 50.0 {
        let km = r.hw_m / 1000.0;
        s.push_str(&format!(" 高速{km:.1}km ¥{}概算", (km * 30.0).round() as i64));
    }
    if r.via_google {
        s.push_str(" (Google経由)");
    }
    s
}
// 高速(motorway=有料道)区間の総メートル。料金概算に使う。
// properties.messages([[headers],[row..]] は全て文字列)から Distance/WayTags 列を引く。
fn expressway_meters(body: &str) -> f64 {
    let messages = match parse_brouter(body) {
        Some(r) => r.features.into_iter().find_map(|f| f.properties.map(|p| p.messages)).unwrap_or_default(),
        None => return 0.0,
    };
    if messages.is_empty() { return 0.0; }
    let di = messages[0].iter().position(|h| h == "Distance");
    let wi = messages[0].iter().position(|h| h == "WayTags");
    let (di, wi) = match (di, wi) { (Some(d), Some(w)) => (d, w), _ => return 0.0 };
    let mut m = 0.0;
    for r in &messages[1..] {
        if let (Some(d), Some(w)) = (r.get(di), r.get(wi)) {
            if w.contains("highway=motorway") {
                if let Ok(v) = d.parse::<f64>() { m += v; }
            }
        }
    }
    m
}
// geojson の LineString coordinates([[lon,lat,elev?],...]) を (lat,lon) 列へ。
// lon/lat のどちらかを欠く点があれば None(既存挙動: 点が壊れていれば全体失敗)。
fn parse_geojson_line(body: &str) -> Option<Vec<(f64, f64)>> {
    let coords = first_coords(body)?;
    let mut pts = Vec::with_capacity(coords.len());
    for c in &coords {
        let lon = *c.first()?;
        let lat = *c.get(1)?;
        pts.push((lat, lon)); // (lat, lon)順に格納
    }
    if pts.is_empty() { None } else { Some(pts) }
}
// geojson の各点 [lon,lat,elev] の3つ目(標高m)を pts と並行に収集する。
// 欠損点(elev無し)は 0.0 を入れて pts と件数を一致させる。
fn parse_geojson_ele(body: &str) -> Vec<f64> {
    match first_coords(body) {
        Some(coords) => coords.iter().map(|c| c.get(2).copied().unwrap_or(0.0)).collect(),
        None => Vec::new(),
    }
}
// geojson properties の track-length/total-time/filtered ascend(全て文字列)を数値化して返す。
// 欠損・非数は 0.0。
fn parse_geojson_props(body: &str) -> (f64, f64, f64) {
    let props = parse_brouter(body).and_then(|r| r.features.into_iter().find_map(|f| f.properties));
    match props {
        Some(p) => {
            let num = |o: &Option<String>| o.as_deref().and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0);
            (num(&p.track_length), num(&p.total_time), num(&p.filtered_ascend))
        }
        None => (0.0, 0.0, 0.0),
    }
}
// ルート結果のディスクキャッシュ先。キーは (profile, alt, 丸めたwps列) の FNV-1a ハッシュ。
// profile で正規化するので 下道/surface/quiet 等は同一ルートを共有。プロット不変なら再起動後も再利用。
fn route_cache_path(wps: &[(f64, f64)], mode: &str, alt: u32) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut key = format!("{}|{}", route_profile(mode), alt);
    for (la, lo) in wps { key.push_str(&format!("|{la:.6},{lo:.6}")); }
    let mut h: u64 = 0xcbf29ce4_84222325;
    for b in key.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
    Some(std::path::Path::new(&home).join(".config/termmap/route-cache").join(format!("{h:016x}.json")))
}

// mode: "short"=最短(shortest) / それ以外=裏道(safety)。wps は (lat,lon) 列。
// key: Google Maps APIキー。BRouterが最終的に失敗した時だけ Google Directions へフォールバックする(空なら試さない)。
pub fn fetch_route(wps: &[(f64, f64)], mode: &str, alt: u32, key: &str) -> Result<RouteResult, String> {
    if wps.len() < 2 { return Err("--route は始点と終点(2点以上)が必要".into()); }
    let alt = alt.min(3); // BRouter の代替ルートは 0..=3
    // ディスクキャッシュ: 同じプロット(wps,profile,alt)なら BRouter を叩かず再利用(再起動後も)
    let cpath = route_cache_path(wps, mode, alt);
    if let Some(p) = &cpath {
        if let Ok(s) = std::fs::read_to_string(p) {
            if let Ok(r) = serde_json::from_str::<RouteResult>(&s) { return Ok(r); }
        }
    }
    let primary = route_profile(mode);
    // まず希望プロファイルで。target island(その道路網に点が繋がらない)なら car-fast で必ず線を出す。
    // BRouter が(ISLAND救済含め)最終的に失敗した場合のみ、key があれば Google へフォールバックする。
    let result = match fetch_route_once(wps, primary, alt) {
        Ok(r) => r,
        Err(e) if e == "ISLAND" => {
            if primary == "car-fast" {
                return Err("この点は道路網に繋がらない(点を道路上へ動かして)".to_string());
            }
            match fetch_route_once(wps, "car-fast", alt) {
                Ok(r) => r, // 下道で繋がらないので車道優先で表示
                Err(e2) if e2 == "ISLAND" => return Err("この点は道路網に繋がらない(点を道路上へ動かして)".to_string()),
                Err(e2) => match fetch_google_route(wps, mode, key) {
                    Ok(r) => r,
                    Err(_) => return Err(e2), // Googleも失敗→元のBRouterエラーを返す
                },
            }
        }
        Err(e) => match fetch_google_route(wps, mode, key) {
            Ok(r) => r,
            Err(_) => return Err(e), // Googleも失敗→元のBRouterエラーを返す
        },
    };
    // 成功時のみ保存(ベストエフォート)
    if let Some(p) = &cpath {
        if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        if let Ok(s) = serde_json::to_string(&result) { let _ = std::fs::write(p, s); }
    }
    Ok(result)
}

// 1プロファイル分の取得。target island は sentinel "ISLAND" を返し、呼び出し側でフォールバック判定する。
fn fetch_route_once(wps: &[(f64, f64)], profile: &str, alt: u32) -> Result<RouteResult, String> {
    let lonlats = wps.iter().map(|(la, lo)| format!("{lo},{la}")).collect::<Vec<_>>().join("|");
    let url = format!("https://brouter.de/brouter?lonlats={lonlats}&profile={profile}&alternativeidx={alt}&format=geojson");
    let body = match ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .timeout(std::time::Duration::from_secs(20)).call() {
        Ok(r) => r.into_string().map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(_, r)) => {
            let msg = r.into_string().unwrap_or_default();
            if msg.contains("target island") { return Err("ISLAND".to_string()); }
            return Err(format!("ルート取得失敗: {}", msg.trim()));
        }
        Err(e) => return Err(format!("ルート取得失敗: {e}")),
    };
    let pts = parse_geojson_line(&body).ok_or("route: geometry parse失敗")?;
    let ele = parse_geojson_ele(&body);
    let (dist_m, time_s, ascend_m) = parse_geojson_props(&body);
    let hw_m = expressway_meters(&body);
    Ok(RouteResult { pts, ele, dist_m, time_s, hw_m, ascend_m, via_google: false })
}

// ---- 曲がり角(ターンバイターン音声案内用) ----
//
// BRouterは format=geojson だと曲がり角情報を返さないが、format=gpx に
// turnInstructionMode=3 を付けると <rtept> ごとに <turn>コード</turn>(TL/TR/TSLL/TSLR/
// TSHL/TSHR/KL/KR/C/TU等)と <turn-angle> が入った出力になる(実測確認済み)。start/destination
// にはturnタグが無く <desc>start</desc>/<desc>destination</desc> だけが入る。
// 座標系はgeojson版と同じ経路なので、pts(既存取得済みのポリライン)へ最近傍投影して
// 「ルート起点からの累積距離」を求め、音声案内側は距離だけで残り時間を判断できるようにする。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnPoint {
    pub lat: f64,
    pub lon: f64,
    pub turn: String, // BRouterのコード、または到着を表す "ARRIVE"(自前の印)
    pub dist_from_start_m: f64,
}

// trigger_route(RouteRx)と同じ非ブロッキング方針。バックグラウンドスレッドで取得し、
// 受信チャネルをUIループ側でポーリングする。
pub type TurnRx = std::sync::mpsc::Receiver<Vec<TurnPoint>>;
pub fn trigger_turn_points(wps: &[(f64, f64)], mode: &str, alt: u32, pts: &[(f64, f64)]) -> TurnRx {
    let (tx, rx) = std::sync::mpsc::channel();
    let (w, m, p) = (wps.to_vec(), mode.to_string(), pts.to_vec());
    std::thread::spawn(move || { let _ = tx.send(fetch_turn_points(&w, &m, alt, &p)); });
    rx
}

// 失敗しても呼び出し側は「曲がり案内なし」に静かにフォールバックできるよう常にVecを返す
// (ルート自体の表示は既存のgeojson取得に依存しており、こちらの失敗で壊さない)。
pub fn fetch_turn_points(wps: &[(f64, f64)], mode: &str, alt: u32, pts: &[(f64, f64)]) -> Vec<TurnPoint> {
    if wps.len() < 2 || pts.is_empty() {
        return Vec::new();
    }
    let alt = alt.min(3);
    let profile = route_profile(mode);
    let lonlats = wps.iter().map(|(la, lo)| format!("{lo},{la}")).collect::<Vec<_>>().join("|");
    let url = format!("https://brouter.de/brouter?lonlats={lonlats}&profile={profile}&alternativeidx={alt}&format=gpx&turnInstructionMode=3");
    let body = match ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .timeout(std::time::Duration::from_secs(20))
        .call()
    {
        Ok(r) => match r.into_string() {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    turn_points_from_gpx(&body, pts)
}

// GPX本文 → TurnPoint一覧。ネットワークに触れない純関数(テスト容易性のためfetch_turn_pointsから分離)。
fn turn_points_from_gpx(body: &str, pts: &[(f64, f64)]) -> Vec<TurnPoint> {
    let cum = cumulative_distances_m(pts);
    parse_gpx_turnpoints(body)
        .into_iter()
        .filter_map(|(lat, lon, desc, turn)| {
            let turn = if desc == "start" {
                return None; // 出発点は案内不要
            } else if desc == "destination" {
                "ARRIVE".to_string()
            } else if turn.is_empty() {
                return None; // 想定外(コード無し)は黙ってスキップ
            } else {
                turn
            };
            let dist_from_start_m = project_onto_route((lat, lon), pts, &cum)?;
            Some(TurnPoint { lat, lon, turn, dist_from_start_m })
        })
        .collect()
}

// <rtept lat=".." lon="..">...<desc>..</desc>...<turn>..</turn>...</rtept> を抜き出す。
// 一般XMLパーサではなく、BRouterが実際に出す構造だけを対象にした最小限の手書きスキャナ
// (依存追加なし。config.tomlの自前パーサと同じ方針)。
fn parse_gpx_turnpoints(body: &str) -> Vec<(f64, f64, String, String)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<rtept ") {
        rest = &rest[start..];
        let Some(tag_end) = rest.find('>') else { break };
        let (lat, lon) = {
            let tag = &rest[..tag_end];
            (xml_attr(tag, "lat").and_then(|s| s.parse::<f64>().ok()), xml_attr(tag, "lon").and_then(|s| s.parse::<f64>().ok()))
        };
        let Some(block_end) = rest.find("</rtept>") else { break };
        let block = &rest[tag_end..block_end];
        let desc = xml_tag(block, "desc").unwrap_or_default();
        let turn = xml_tag(block, "turn").unwrap_or_default();
        if let (Some(la), Some(lo)) = (lat, lon) {
            out.push((la, lo, desc, turn));
        }
        rest = &rest[block_end + "</rtept>".len()..];
    }
    out
}

fn xml_attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let i = tag.find(&needle)? + needle.len();
    let j = tag[i..].find('"')?;
    Some(tag[i..i + j].to_string())
}

fn xml_tag(block: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let i = block.find(&open)? + open.len();
    let j = block[i..].find(&close)?;
    Some(block[i..i + j].to_string())
}

// pts(ルートのポリライン)に沿った、各点の起点からの累積距離(メートル)。pts と同じ長さ。
fn cumulative_distances_m(pts: &[(f64, f64)]) -> Vec<f64> {
    let mut acc = Vec::with_capacity(pts.len());
    let mut d = 0.0;
    for i in 0..pts.len() {
        if i > 0 {
            d += crate::geo::haversine_km(pts[i - 1], pts[i]) * 1000.0;
        }
        acc.push(d);
    }
    acc
}

// 現在地(lat,lon)をルートのポリラインへ投影し、起点からの進捗距離(メートル)を返す。
// voice::VoiceGuide::tick に渡す progress_m はこれで求める(TurnPoint.dist_from_start_mと
// 同じ物差し=同じcumulative_distances_m/project_onto_routeを使うので、直接比較できる)。
pub fn progress_along_route(pos: (f64, f64), pts: &[(f64, f64)]) -> Option<f64> {
    if pts.is_empty() {
        return None;
    }
    let cum = cumulative_distances_m(pts);
    project_onto_route(pos, pts, &cum)
}

// 曲がり角(lat,lon)を pts 上の最近傍点に投影し、その点の累積距離を返す。
fn project_onto_route(pt: (f64, f64), pts: &[(f64, f64)], cum: &[f64]) -> Option<f64> {
    let mut best_i = 0usize;
    let mut best_d = f64::MAX;
    for (i, p) in pts.iter().enumerate() {
        let d = crate::geo::haversine_km(pt, *p);
        if d < best_d {
            best_d = d;
            best_i = i;
        }
    }
    cum.get(best_i).copied()
}

// Google Directions API(旧・レガシー版、Routes APIではない)でのフォールバック取得。
// BRouterが失敗した時だけ最終手段として呼ばれる。標高データは提供されないため ele は空Vec、
// 高速区間の判定手段が無いため hw_m は 0.0(料金概算は出ない=呼び出し側で自然にスキップされる)。
fn fetch_google_route(wps: &[(f64, f64)], mode: &str, key: &str) -> Result<RouteResult, String> {
    if key.trim().is_empty() { return Err("Google APIキー未設定".to_string()); }
    let origin = format!("{},{}", wps[0].0, wps[0].1);
    let destination = format!("{},{}", wps[wps.len() - 1].0, wps[wps.len() - 1].1);
    let mut url = format!("https://maps.googleapis.com/maps/api/directions/json?origin={origin}&destination={destination}&key={key}");
    if wps.len() > 2 {
        let via: Vec<String> = wps[1..wps.len() - 1].iter().map(|(la, lo)| format!("{la},{lo}")).collect();
        url.push_str(&format!("&waypoints={}", via.join("|")));
    }
    // route_profile(mode) が "moped"(下道=高速回避)ならavoid=highwaysを付ける。
    // "shortest"はGoogle側に直接の等価オプションが無いため素のまま(車での既定経路)。
    if route_profile(mode) == "moped" {
        url.push_str("&avoid=highways");
    }
    let body = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(20)).call()
        .map_err(|e| format!("Google route: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("Google route parse: {e}"))?;
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if status != "OK" {
        let msg = v.get("error_message").and_then(|m| m.as_str()).unwrap_or(status);
        return Err(format!("Google route: {msg}"));
    }
    let route = v.get("routes").and_then(|r| r.get(0)).ok_or("Google route: routes無し")?;
    let encoded = route.get("overview_polyline").and_then(|p| p.get("points")).and_then(|p| p.as_str())
        .ok_or("Google route: polyline無し")?;
    let pts = decode_google_polyline(encoded);
    if pts.is_empty() { return Err("Google route: 空のポリライン".to_string()); }
    let legs = route.get("legs").and_then(|l| l.as_array()).cloned().unwrap_or_default();
    let mut dist_m = 0.0;
    let mut time_s = 0.0;
    for leg in &legs {
        dist_m += leg.get("distance").and_then(|d| d.get("value")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        time_s += leg.get("duration").and_then(|d| d.get("value")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    }
    Ok(RouteResult { pts, ele: Vec::new(), dist_m, time_s, hw_m: 0.0, ascend_m: 0.0, via_google: true })
}

// Googleのポリラインエンコーディング(https://developers.google.com/maps/documentation/utilities/polylinealgorithm)
// をデコードして (lat,lon) 列にする(ネットワーク非依存の純粋関数)。
fn decode_google_polyline(encoded: &str) -> Vec<(f64, f64)> {
    let mut points = Vec::new();
    let bytes = encoded.as_bytes();
    let mut idx = 0usize;
    let (mut lat, mut lon) = (0i64, 0i64);
    // 1つの符号化数値を取り出す(可変長・zigzag)。idx を進める。
    fn decode_value(bytes: &[u8], idx: &mut usize) -> i64 {
        let mut result: i64 = 0;
        let mut shift = 0u32;
        loop {
            let b = bytes[*idx] as i64 - 63;
            *idx += 1;
            result |= (b & 0x1f) << shift;
            shift += 5;
            if b < 0x20 { break; }
        }
        if result & 1 != 0 { !(result >> 1) } else { result >> 1 }
    }
    while idx < bytes.len() {
        lat += decode_value(bytes, &mut idx);
        lon += decode_value(bytes, &mut idx);
        points.push((lat as f64 / 1e5, lon as f64 / 1e5));
    }
    points
}
pub fn write_gpx(path: &str, pts: &[(f64, f64)]) -> Result<(), String> {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gpx version=\"1.1\" creator=\"termmap\" xmlns=\"http://www.topografix.com/GPX/1/1\">\n<trk><name>termmap route</name><trkseg>\n");
    for (la, lo) in pts { s.push_str(&format!("<trkpt lat=\"{la}\" lon=\"{lo}\"></trkpt>\n")); }
    s.push_str("</trkseg></trk>\n</gpx>\n");
    crate::fsutil::write_atomic(std::path::Path::new(path), s.as_bytes(), None).map_err(|e| format!("gpx write {path}: {e}"))
}

// waypoints/pois/mode から spec の pois/routes を作り直し、ルート要約を返す(rings は保持)。
pub fn set_markers(spec: &mut OverlaySpec, wps: &[(f64, f64)], pois: &[(f64, f64, String, PoiCat)]) {
    spec.pois.clear();
    for (la, lo, _, cat) in pois { spec.pois.push(Poi { lat: *la, lon: *lo, cat: *cat }); }
    let n = wps.len();
    for (idx, (la, lo)) in wps.iter().enumerate() {
        let cat = if idx == 0 { PoiCat::Waypoint } else if idx == n - 1 { PoiCat::Home } else { PoiCat::Food };
        spec.pois.push(Poi { lat: *la, lon: *lo, cat });
    }
}
pub type RouteRx = std::sync::mpsc::Receiver<Result<RouteResult, String>>;
// マーカーは即反映し、ルートはバックグラウンドスレッドで計算する(受信チャネルを返す)。
// Ctrl+C で受信側を捨てれば計算を中断できる(スレッドはtimeoutまで走るが結果は無視)。
pub fn trigger_route(spec: &mut OverlaySpec, wps: &[(f64, f64)], pois: &[(f64, f64, String, PoiCat)], mode: &str, alt: u32, key: &str) -> (Option<String>, Option<RouteRx>) {
    set_markers(spec, wps, pois);
    spec.routes.clear();
    if wps.len() >= 2 {
        let (tx, rx) = std::sync::mpsc::channel();
        let (w, m, k) = (wps.to_vec(), mode.to_string(), key.to_string());
        std::thread::spawn(move || { let _ = tx.send(fetch_route(&w, &m, alt, &k)); });
        (Some("計算中… (Ctrl+Cで中断)".to_string()), Some(rx))
    } else {
        (None, None)
    }
}

// ---- waypoint 操作(純粋・テスト対象。route再計算は呼び出し側) ----
// 地点を末尾に追加する。役割(始点/終点)は並び順で決まる(先頭=始点・末尾=終点)。
pub fn wp_add(wps: &mut Vec<(f64, f64)>, p: (f64, f64)) {
    wps.push(p);
}
pub fn wp_remove(wps: &mut Vec<(f64, f64)>, sel: &mut usize) {
    if !wps.is_empty() {
        let i = (*sel).min(wps.len() - 1);
        wps.remove(i);
        if *sel >= wps.len() && *sel > 0 { *sel -= 1; }
    }
}
pub fn wp_swap(wps: &mut [(f64, f64)], sel: &mut usize, back: bool) {
    if back {
        if *sel > 0 && *sel < wps.len() { wps.swap(*sel, *sel - 1); *sel -= 1; }
    } else if *sel + 1 < wps.len() {
        wps.swap(*sel, *sel + 1);
        *sel += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poi::json_first; // fetch_route_parses_ele_and_ascend の整合確認用

    #[test]
    fn waypoint_ops() {
        let mut w: Vec<(f64, f64)> = Vec::new();
        wp_add(&mut w, (1.0, 1.0)); // 追加した順に並ぶ(先頭=始点/末尾=終点)
        wp_add(&mut w, (2.0, 2.0));
        wp_add(&mut w, (3.0, 3.0));
        assert_eq!(w, vec![(1.0, 1.0), (2.0, 2.0), (3.0, 3.0)]);
        let mut sel = 1usize;
        wp_swap(&mut w, &mut sel, false); // 後ろへ
        assert_eq!(sel, 2);
        wp_swap(&mut w, &mut sel, true); // 前へ
        assert_eq!(sel, 1);
        wp_remove(&mut w, &mut sel); // 中央削除
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn profiles_and_labels() {
        assert_eq!(route_profile("short"), "shortest");
        assert_eq!(route_profile("highway"), "car-fast");
        assert_eq!(route_profile("surface"), "moped");
        assert_eq!(mode_label("highway"), "高速");
        assert_eq!(mode_label("short"), "最短");
        assert_eq!(mode_label("surface"), "下道");
    }

    #[test]
    fn parse_route_geometry() {
        let body = r#"{"features":[{"geometry":{"coordinates":[[139.7,35.7,9.0],[139.71,35.71,10.0]]}}]}"#;
        let pts = parse_geojson_line(body).unwrap();
        assert_eq!(pts.len(), 2);
        assert!((pts[0].0 - 35.7).abs() < 1e-9 && (pts[0].1 - 139.7).abs() < 1e-9); // (lat,lon)順
        // 標高(3つ目)も pts と並行に拾えていること
        let ele = parse_geojson_ele(body);
        assert_eq!(ele.len(), pts.len());
        assert!((ele[0] - 9.0).abs() < 1e-9 && (ele[1] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn fetch_route_parses_ele_and_ascend() {
        // filtered ascend が properties から拾えること(単体パーサの整合確認)
        let body = r#"{"features":[{"properties":{"filtered ascend": "123"}}]}"#;
        let asc = json_first(body, "\"filtered ascend\": \"")
            .or_else(|| json_first(body, "\"filtered ascend\":\""))
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(0.0);
        assert!((asc - 123.0).abs() < 1e-9);
    }

    #[test]
    fn parse_props_reads_hyphenated_string_values() {
        // properties のハイフン/空白入りキーを serde(rename)で読み、文字列値を数値化する
        let body = r#"{"features":[{"properties":{"track-length":"12345","total-time":"600","filtered ascend":"78"}}]}"#;
        let (dist, time, asc) = parse_geojson_props(body);
        assert!((dist - 12345.0).abs() < 1e-9);
        assert!((time - 600.0).abs() < 1e-9);
        assert!((asc - 78.0).abs() < 1e-9);
        // properties 欠損は全て 0.0
        assert_eq!(parse_geojson_props(r#"{"features":[{}]}"#), (0.0, 0.0, 0.0));
    }

    #[test]
    fn parse_geojson_line_handles_missing_elevation() {
        // 標高(3要素目)が無い点でも lat/lon は取れ、ele は 0.0 で件数一致
        let body = r#"{"features":[{"geometry":{"coordinates":[[139.7,35.7],[139.71,35.71,10.0]]}}]}"#;
        let pts = parse_geojson_line(body).unwrap();
        let ele = parse_geojson_ele(body);
        assert_eq!(pts.len(), 2);
        assert_eq!(ele.len(), 2);
        assert!((ele[0] - 0.0).abs() < 1e-9); // 欠損→0.0
        assert!((ele[1] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn expressway_meters_sums_motorway() {
        let body = r#"{"features":[{"properties":{"messages":[
          ["Longitude","Latitude","Elevation","Distance","CostPerKm","ElevCost","TurnCost","NodeCost","InitialCost","WayTags","NodeTags","Time","Energy"],
          ["1","2","3","100","0","0","0","0","0","highway=motorway maxspeed=80","","0","0"],
          ["1","2","3","50","0","0","0","0","0","highway=residential","","0","0"]
        ]}}]}"#;
        assert!((expressway_meters(body) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn decode_google_polyline_known_vector() {
        // Google公式ドキュメントの標準テストベクタ(polylinealgorithm)
        let pts = decode_google_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@");
        let want = [(38.5, -120.2), (40.7, -120.95), (43.252, -126.453)];
        assert_eq!(pts.len(), want.len());
        for (got, exp) in pts.iter().zip(want.iter()) {
            assert!((got.0 - exp.0).abs() < 1e-4, "lat {got:?} != {exp:?}");
            assert!((got.1 - exp.1).abs() < 1e-4, "lon {got:?} != {exp:?}");
        }
    }

    #[test]
    fn route_summary_marks_google_source() {
        let g = RouteResult { pts: vec![], ele: vec![], dist_m: 261865.0, time_s: 11232.0, hw_m: 0.0, ascend_m: 0.0, via_google: true };
        assert!(route_summary("highway", &g).contains("(Google経由)"));
        let b = RouteResult { pts: vec![], ele: vec![], dist_m: 1000.0, time_s: 60.0, hw_m: 0.0, ascend_m: 0.0, via_google: false };
        assert!(!route_summary("highway", &b).contains("(Google経由)"));
    }

    #[test]
    fn route_result_serde_back_compat() {
        // via_google が無い旧キャッシュJSONも #[serde(default)] で false として読める
        let old = r#"{"pts":[],"ele":[],"dist_m":0.0,"time_s":0.0,"hw_m":0.0,"ascend_m":0.0}"#;
        let r: RouteResult = serde_json::from_str(old).expect("旧JSONが読めること");
        assert!(!r.via_google);
    }

    // ---- 曲がり角(ターンバイターン) ----

    // 実際の BRouter format=gpx&turnInstructionMode=3 応答の抜粋(2026/08/16 実測、要約)。
    const GPX_TURNS_SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gpx>
<rte>
 <rtept lat="35.681006" lon="139.765553">
   <desc>start</desc>
   <extensions><time>21</time><offset>0</offset></extensions>
 </rtept>
 <rtept lat="35.680748" lon="139.765058">
   <desc>left</desc>
   <extensions><time>13</time><turn>TL</turn><turn-angle>-78</turn-angle><offset>11</offset></extensions>
 </rtept>
 <rtept lat="35.658316" lon="139.745120">
   <desc>destination</desc>
   <extensions><time>0</time><offset>137</offset></extensions>
 </rtept>
</rte>
</gpx>"#;

    #[test]
    fn parse_gpx_turnpoints_extracts_desc_and_turn() {
        let got = parse_gpx_turnpoints(GPX_TURNS_SAMPLE);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], (35.681006, 139.765553, "start".to_string(), String::new()));
        assert_eq!(got[1], (35.680748, 139.765058, "left".to_string(), "TL".to_string()));
        assert_eq!(got[2], (35.658316, 139.745120, "destination".to_string(), String::new()));
    }

    #[test]
    fn cumulative_distances_start_at_zero_and_increase() {
        let pts = vec![(35.68, 139.76), (35.681, 139.761), (35.682, 139.762)];
        let cum = cumulative_distances_m(&pts);
        assert_eq!(cum.len(), 3);
        assert_eq!(cum[0], 0.0);
        assert!(cum[1] > 0.0);
        assert!(cum[2] > cum[1]);
    }

    #[test]
    fn project_onto_route_picks_nearest_point() {
        let pts = vec![(35.680, 139.760), (35.681, 139.761), (35.682, 139.762)];
        let cum = cumulative_distances_m(&pts);
        // pts[1]のすぐ近くの点はpts[1]の累積距離に投影されるはず。
        let d = project_onto_route((35.6811, 139.7611), &pts, &cum).unwrap();
        assert_eq!(d, cum[1]);
    }

    // start(出発点)は案内対象から除外、destinationは"ARRIVE"、通常のturnはコードのまま残る。
    #[test]
    fn turn_points_from_gpx_skips_start_and_marks_arrival() {
        // GPX_TURNS_SAMPLEの3点にほぼ一致する経路ポリライン(投影先)を用意
        let pts = vec![(35.681006, 139.765553), (35.680748, 139.765058), (35.658316, 139.745120)];
        let got = turn_points_from_gpx(GPX_TURNS_SAMPLE, &pts);
        assert_eq!(got.len(), 2, "startは除外され、left+destinationの2件になる");
        assert_eq!(got[0].turn, "TL");
        assert!(got[0].dist_from_start_m > 0.0);
        assert_eq!(got[1].turn, "ARRIVE");
        assert!(got[1].dist_from_start_m > got[0].dist_from_start_m);
    }

    #[test]
    fn turn_points_from_gpx_empty_on_garbage() {
        assert!(turn_points_from_gpx("not xml at all", &[(35.0, 139.0)]).is_empty());
        assert!(turn_points_from_gpx(GPX_TURNS_SAMPLE, &[]).is_empty());
    }
}
