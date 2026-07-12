// ルーティング (BRouter 公開API)・高速料金/expressway 計算・GPX 出力
use crate::render::{OverlaySpec, Poi, PoiCat};
use serde::Deserialize;

pub struct RouteResult { pub pts: Vec<(f64, f64)>, pub ele: Vec<f64>, pub dist_m: f64, pub time_s: f64, pub hw_m: f64, pub ascend_m: f64 }

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
// mode: "short"=最短(shortest) / それ以外=裏道(safety)。wps は (lat,lon) 列。
pub fn fetch_route(wps: &[(f64, f64)], mode: &str, alt: u32) -> Result<RouteResult, String> {
    if wps.len() < 2 { return Err("--route は始点と終点(2点以上)が必要".into()); }
    let profile = route_profile(mode);
    let alt = alt.min(3); // BRouter の代替ルートは 0..=3
    let lonlats = wps.iter().map(|(la, lo)| format!("{lo},{la}")).collect::<Vec<_>>().join("|");
    let url = format!("https://brouter.de/brouter?lonlats={lonlats}&profile={profile}&alternativeidx={alt}&format=geojson");
    let body = ureq::get(&url)
        .set("User-Agent", "termmap/0.1 (personal experiment)")
        .timeout(std::time::Duration::from_secs(20)).call().map_err(|e| format!("route: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    let pts = parse_geojson_line(&body).ok_or("route: geometry parse失敗")?;
    let ele = parse_geojson_ele(&body);
    let (dist_m, time_s, ascend_m) = parse_geojson_props(&body);
    let hw_m = expressway_meters(&body);
    Ok(RouteResult { pts, ele, dist_m, time_s, hw_m, ascend_m })
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
pub fn trigger_route(spec: &mut OverlaySpec, wps: &[(f64, f64)], pois: &[(f64, f64, String, PoiCat)], mode: &str, alt: u32) -> (Option<String>, Option<RouteRx>) {
    set_markers(spec, wps, pois);
    spec.routes.clear();
    if wps.len() >= 2 {
        let (tx, rx) = std::sync::mpsc::channel();
        let (w, m) = (wps.to_vec(), mode.to_string());
        std::thread::spawn(move || { let _ = tx.send(fetch_route(&w, &m, alt)); });
        (Some("計算中… (Ctrl+Cで中断)".to_string()), Some(rx))
    } else {
        (None, None)
    }
}

// ---- waypoint 操作(純粋・テスト対象。route再計算は呼び出し側) ----
pub fn wp_set_start(wps: &mut Vec<(f64, f64)>, p: (f64, f64)) {
    if wps.is_empty() { wps.push(p); } else { wps[0] = p; }
}
pub fn wp_set_end(wps: &mut Vec<(f64, f64)>, p: (f64, f64)) {
    if wps.len() >= 2 { let l = wps.len() - 1; wps[l] = p; } else { wps.push(p); }
}
pub fn wp_add_via(wps: &mut Vec<(f64, f64)>, p: (f64, f64)) {
    if wps.len() < 2 { wps.push(p); } else { let i = wps.len() - 1; wps.insert(i, p); }
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
        wp_set_start(&mut w, (1.0, 1.0)); // 空→push
        assert_eq!(w, vec![(1.0, 1.0)]);
        wp_set_end(&mut w, (2.0, 2.0)); // len<2→push
        assert_eq!(w, vec![(1.0, 1.0), (2.0, 2.0)]);
        wp_add_via(&mut w, (1.5, 1.5)); // 終点手前へ
        assert_eq!(w, vec![(1.0, 1.0), (1.5, 1.5), (2.0, 2.0)]);
        wp_set_start(&mut w, (0.0, 0.0)); // 先頭置換
        assert_eq!(w[0], (0.0, 0.0));
        wp_set_end(&mut w, (9.0, 9.0)); // 末尾置換
        assert_eq!(*w.last().unwrap(), (9.0, 9.0));
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
}
