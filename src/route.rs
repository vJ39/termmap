// ルーティング (BRouter 公開API)・高速料金/expressway 計算・GPX 出力
use crate::render::{OverlaySpec, Poi, PoiCat};
use crate::regulation::{ClosureEvent, RegulationKind};
use crate::roadtrace::{point_at, polyline_len, sample_every};
use serde::Deserialize;

// hw_segments は高速区間の pts 上のインデックス範囲(両端を含む)。hw_m(距離の合計)と同じ判定で
// 求めるので、表示する距離と色を塗る区間は必ず一致する。#[serde(default)] は via_google と同じ
// 理由(旧形式のキャッシュJSONでパースを失敗させない)で付けるが、中身の正しさは
// route_cache_path のスキーマ版で担保する。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RouteResult { pub pts: Vec<(f64, f64)>, pub ele: Vec<f64>, pub dist_m: f64, pub time_s: f64, pub hw_m: f64, #[serde(default)] pub hw_segments: Vec<(usize, usize)>, pub ascend_m: f64, #[serde(default)] pub via_google: bool }

// ---- 通行止め回避(#通行止めを推奨しない) ----
//
// BRouterのnogosパラメータ(lon,lat,半径m,weight|...)で、実施中の通行止め区間を
// 絶対回避エリア(weight省略)として渡す。対象はRegulationKind::Closed かつ
// ClosureEvent::active(kisei_jishi_jyokyo=="1")のみ。車線規制等は通れなくはないので
// 対象外、予定段階(まだ始まっていない)の通行止めも対象外にする(過剰回避を避ける)。
pub const NOGO_RADIUS_M: f64 = 100.0; // 道幅+GPS誤差を吸収しつつ、近くの別の道路まで塞がない程度
const NOGO_SAMPLE_INTERVAL_M: f64 = 150.0; // 半径100mの円が隣同士で重なるよう、直径200mより狭い間隔でサンプリング
pub const NOGO_MAX_COUNT: usize = 50; // BRouter watchdogタイムアウト対策の上限(実測: 60個=13s際どい/100個以上=400で失敗。安全マージンを取って200から引き下げ。2026/08/17)

// 通行止めのラインを円の列へ変換する。center(経由地の中心等)に近い円を優先し、
// 上限(NOGO_MAX_COUNT)を超えたら遠い分を切り捨てる。戻り値の bool は切り捨てが発生したか。
pub fn closures_to_nogos(closures: &[&ClosureEvent], center: (f64, f64)) -> (Vec<(f64, f64, f64)>, bool) {
    let mut circles: Vec<(f64, f64, f64)> = Vec::new();
    for ev in closures {
        if ev.kind != RegulationKind::Closed || !ev.active {
            continue;
        }
        for &(lat, lon) in &sample_every(&ev.line, NOGO_SAMPLE_INTERVAL_M) {
            circles.push((lat, lon, NOGO_RADIUS_M));
        }
    }
    let truncated = circles.len() > NOGO_MAX_COUNT;
    if truncated {
        circles.sort_by(|a, b| {
            let da = (a.0 - center.0).powi(2) + (a.1 - center.1).powi(2);
            let db = (b.0 - center.0).powi(2) + (b.1 - center.1).powi(2);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
        circles.truncate(NOGO_MAX_COUNT);
    }
    (circles, truncated)
}

// BRouterのnogosクエリパラメータ値("lon,lat,半径|lon,lat,半径|..."形式)を組み立てる。
// weightは付けない(省略=絶対回避)。circlesが空なら空文字(呼び出し側は&nogos=を付けない)。
pub fn nogos_query_param(circles: &[(f64, f64, f64)]) -> String {
    circles.iter().map(|(lat, lon, r)| format!("{lon},{lat},{r:.0}")).collect::<Vec<_>>().join("|")
}

// 経由地(始点〜終点、経由点全部)を全部覆うbboxに、実際の道はカーブして直線から外れる分の
// マージンを足す。regulation_layer.items(bbox)へ渡す(lat_min,lon_min,lat_max,lon_max)形式。
pub fn waypoints_bbox_with_margin(wps: &[(f64, f64)], margin_deg: f64) -> Option<(f64, f64, f64, f64)> {
    if wps.is_empty() {
        return None;
    }
    let (mut lat_min, mut lat_max) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut lon_min, mut lon_max) = (f64::INFINITY, f64::NEG_INFINITY);
    for &(la, lo) in wps {
        lat_min = lat_min.min(la); lat_max = lat_max.max(la);
        lon_min = lon_min.min(lo); lon_max = lon_max.max(lo);
    }
    Some((lat_min - margin_deg, lon_min - margin_deg, lat_max + margin_deg, lon_max + margin_deg))
}

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
        // 区間数は2つ以上のときだけ出す。「途中で一度高速を降りる」ことが分かる場合にだけ
        // 意味がある情報で、常に出すと通常のルート(1区間)で冗長になる。
        let segs = if r.hw_segments.len() >= 2 { format!("({}区間)", r.hw_segments.len()) } else { String::new() };
        s.push_str(&format!(" 高速{km:.1}km{segs} ¥{}概算", (km * 30.0).round() as i64));
    }
    if r.via_google {
        s.push_str(" (Google経由)");
    }
    s
}
// ---- 高速道路区間(#高速区間、docs/route-expressway-segment-design.md) ----
//
// BRouterの properties.messages は [[ヘッダ],[行..]] の文字列表で、各行は「直前の行の座標から、
// その行の座標まで」の区間を表す。Longitude/Latitude は整数マイクロ度の文字列(139701812 =
// 139.701812度)で、その座標は geometry.coordinates の頂点そのもの(=RouteResult.pts の頂点)と
// インデックス昇順で一致する(3ルート424行で実測・未一致0)。よって座標の一致だけで pts の
// インデックス範囲へ落とせ、距離を按分して位置を推定する必要はない。

// 高速区間の色。日本の道路案内標識(高速=緑・一般道=青)に合わせる。ルート本体はシアンのまま。
pub const EXPRESSWAY_COLOR: [u8; 3] = [0, 230, 100];
// 座標一致の許容(マイクロ度)。±2マイクロ度≒0.2mで、丸め誤差だけを吸収する。
const COORD_MATCH_TOL_UDEG: i64 = 2;

// 度をマイクロ度の整数へ。messages 側の整数表現と突き合わせるために使う。
fn to_micro_deg(d: f64) -> i64 { (d * 1e6).round() as i64 }

// WayTags 1行分が高速道路か。"highway=motorway" の部分一致は "highway=motorway_link" にも
// 当たる。ランプ・JCT連絡路を高速に含めるのは意図した挙動で(IC入口からIC出口まで線が途切れない)、
// hw_m(料金概算)の集計と判定を揃えるために距離集計と区間抽出の両方からこの関数を呼ぶ。
fn is_expressway_tags(waytags: &str) -> bool { waytags.contains("highway=motorway") }

// BRouterの応答本文と、そこから作った pts から、(高速の合計メートル, pts のインデックス範囲)を
// 求める。ネットワークに触れない純関数。
//
// 位置特定に一度でも失敗したら範囲は空で返し、距離だけ返す(色分けは出ないが距離と料金概算は
// 従来通り出る)。距離の集計は行ごとに独立しているので、位置特定の成否に影響されない。
pub fn expressway_segments(body: &str, pts: &[(f64, f64)]) -> (f64, Vec<(usize, usize)>) {
    let messages = match parse_brouter(body) {
        Some(r) => r.features.into_iter().find_map(|f| f.properties.map(|p| p.messages)).unwrap_or_default(),
        None => return (0.0, Vec::new()),
    };
    if messages.is_empty() { return (0.0, Vec::new()); }
    let head = &messages[0];
    let di = head.iter().position(|h| h == "Distance");
    let wi = head.iter().position(|h| h == "WayTags");
    let (di, wi) = match (di, wi) { (Some(d), Some(w)) => (d, w), _ => return (0.0, Vec::new()) };
    let lon_i = head.iter().position(|h| h == "Longitude");
    let lat_i = head.iter().position(|h| h == "Latitude");

    // pts をマイクロ度の整数へ丸めた配列を1度だけ作る(行ごとに丸め直さない)。
    let micro: Vec<(i64, i64)> = pts.iter().map(|&(la, lo)| (to_micro_deg(la), to_micro_deg(lo))).collect();
    // 位置特定できる条件(座標の列がある・pts が空でない)。途中で失敗したら false に落とす。
    let (mut locate, lon_i, lat_i) = match (lon_i, lat_i) {
        (Some(a), Some(b)) if !micro.is_empty() => (true, a, b),
        _ => (false, 0, 0),
    };

    let mut meters = 0.0;
    let mut raw: Vec<(usize, usize)> = Vec::new();
    let mut cursor = 0usize; // pts 走査の開始位置。前方向にしか進めない
    let mut prev_idx = 0usize; // 直前の行が指した頂点(=この行の区間の始点)
    for row in &messages[1..] {
        let hw = row.get(wi).is_some_and(|w| is_expressway_tags(w));
        if hw {
            if let Some(Ok(v)) = row.get(di).map(|d| d.parse::<f64>()) { meters += v; }
        }
        if !locate { continue; }
        // 座標をマイクロ度の整数として読み、cursor から前方へ走査して一致する頂点を探す。
        // 前方向にしか進まないので、折り返す経路で同じ座標が2回出ても手前を誤って選ばない。
        let parsed = match (row.get(lon_i), row.get(lat_i)) {
            (Some(lo), Some(la)) => match (lo.trim().parse::<i64>(), la.trim().parse::<i64>()) {
                (Ok(lo), Ok(la)) => Some((lo, la)),
                _ => None,
            },
            _ => None,
        };
        let Some((lo_u, la_u)) = parsed else { locate = false; continue };
        let found = (cursor..micro.len())
            .find(|&i| (micro[i].0 - la_u).abs() <= COORD_MATCH_TOL_UDEG && (micro[i].1 - lo_u).abs() <= COORD_MATCH_TOL_UDEG);
        let Some(end_idx) = found else { locate = false; continue };
        if hw { raw.push((prev_idx, end_idx)); }
        prev_idx = end_idx;
        // cursor は end_idx + 1 にしない。長さ0の行(同じ頂点を2回指す)が来ても同じ頂点で
        // 受けられるようにするため。長さ0の範囲は最後に捨てる。
        cursor = end_idx;
    }
    if !locate { return (meters, Vec::new()); }

    // 隣り合う範囲を統合する(次の始点が直前の終点以下なら1つに繋ぐ)。
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (a, b) in raw {
        match merged.last_mut() {
            Some(last) if a <= last.1 => { if b > last.1 { last.1 = b; } }
            _ => merged.push((a, b)),
        }
    }
    // 始点と終点が同じ範囲は線として描けないので捨てる。距離は Distance 列の合計なので影響しない。
    merged.retain(|&(a, b)| b > a);
    (meters, merged)
}

// インデックス範囲を描画用の点列へ。pts の範囲外・2点未満になる範囲は捨てる。
pub fn expressway_polylines(pts: &[(f64, f64)], segs: &[(usize, usize)]) -> Vec<Vec<(f64, f64)>> {
    segs.iter()
        .filter(|&&(a, b)| b > a && b < pts.len()) // b > a で2点以上、b < len で範囲内(a < b なので a も範囲内)
        .map(|&(a, b)| pts[a..=b].to_vec())
        .collect()
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
// ルート結果のディスクキャッシュ先。キーは (profile, alt, 丸めたwps列, nogos) の FNV-1a ハッシュ。
// profile で正規化するので 下道/surface/quiet 等は同一ルートを共有。プロット不変なら再起動後も再利用。
// nogos をキーに含めるのは必須(#通行止め回避): 含めないと、新しい通行止めが出現した後も
// 通行止めを無視した古いキャッシュ済みルートを出し続けてしまう。
// RouteResult の中身の作り方を変えたらこの値を上げる(古い保存分を読まないようにするため)。
// v2: hw_segments(高速区間)を追加。ルートキャッシュは期限を持たないので、版を上げないと
// 「hw_m > 0 なのに hw_segments が空=距離は出るのに色が出ない」保存分が消えずに残る。
const ROUTE_CACHE_SCHEMA: &str = "v2";
// キー文字列とファイル名は、HOME に依存しない純関数として切り出してある(スキーマ版を上げれば
// 別のファイル名になることをテストで確かめられるようにするため)。
fn route_cache_key(schema: &str, wps: &[(f64, f64)], mode: &str, alt: u32, nogos: &str) -> String {
    let mut key = format!("{}|{}|{}|{}", schema, route_profile(mode), alt, nogos);
    for (la, lo) in wps { key.push_str(&format!("|{la:.6},{lo:.6}")); }
    key
}
fn route_cache_file_name(key: &str) -> String {
    let mut h: u64 = 0xcbf29ce4_84222325;
    for b in key.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
    format!("{h:016x}.json")
}
fn route_cache_path(wps: &[(f64, f64)], mode: &str, alt: u32, nogos: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let key = route_cache_key(ROUTE_CACHE_SCHEMA, wps, mode, alt, nogos);
    Some(std::path::Path::new(&home).join(".config/termmap/route-cache").join(route_cache_file_name(&key)))
}

// mode: "short"=最短(shortest) / それ以外=裏道(safety)。wps は (lat,lon) 列。
// key: Google Maps APIキー。BRouterが最終的に失敗した時だけ Google Directions へフォールバックする(空なら試さない)。
// nogos: BRouterのnogosパラメータ値(nogos_query_paramで組み立てた"lon,lat,半径|..."形式)。
// 空文字なら付けない(#通行止め回避が無効、または周辺に対象が無い場合)。
pub fn fetch_route(wps: &[(f64, f64)], mode: &str, alt: u32, key: &str, nogos: &str) -> Result<RouteResult, String> {
    if wps.len() < 2 { return Err("--route は始点と終点(2点以上)が必要".into()); }
    let alt = alt.min(3); // BRouter の代替ルートは 0..=3
    // ディスクキャッシュ: 同じプロット(wps,profile,alt,nogos)なら BRouter を叩かず再利用(再起動後も)
    let cpath = route_cache_path(wps, mode, alt, nogos);
    if let Some(p) = &cpath {
        if let Ok(s) = std::fs::read_to_string(p) {
            if let Ok(r) = serde_json::from_str::<RouteResult>(&s) { return Ok(r); }
        }
    }
    let primary = route_profile(mode);
    // まず希望プロファイルで。target island(その道路網に点が繋がらない)なら car-fast で必ず線を出す。
    // BRouter が(ISLAND救済含め)最終的に失敗した場合のみ、key があれば Google へフォールバックする。
    // 注意: Google Directionsはnogosを知らないため、この最終フォールバックだけは通行止めを
    // 避けられない可能性がある(BRouterが絶対回避に失敗する場合自体まれなので許容する)。
    let result = match fetch_route_once(wps, primary, alt, nogos) {
        Ok(r) => r,
        Err(e) if e == "ISLAND" => {
            if primary == "car-fast" {
                return Err("この点は道路網に繋がらない(点を道路上へ動かして)".to_string());
            }
            match fetch_route_once(wps, "car-fast", alt, nogos) {
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
    // 成功時のみ保存(ベストエフォート)。Googleフォールバック結果はキャッシュしない
    // (BRouterの一時的失敗(watchdogタイムアウト等)でGoogle経由の下道ルートが一度でも
    // 生成されると、それがディスクに永続化されBRouter復旧後も古い下道ルートを出し
    // 続けてしまう事故があったため。[高速]設定なのに[下道 (Google経由)]が固定表示され
    // 続けた根本原因)。
    if should_persist_route_cache(&result) {
        if let Some(p) = &cpath {
            if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
            if let Ok(s) = serde_json::to_string(&result) { let _ = std::fs::write(p, s); }
        }
    }
    Ok(result)
}

// Googleフォールバック結果はディスクキャッシュへ保存しない(次回また新規に
// BRouterへ再挑戦させ、一時的な失敗から自己修復できるようにするため)。
fn should_persist_route_cache(r: &RouteResult) -> bool {
    !r.via_google
}

// 1プロファイル分の取得。target island は sentinel "ISLAND" を返し、呼び出し側でフォールバック判定する。
fn fetch_route_once(wps: &[(f64, f64)], profile: &str, alt: u32, nogos: &str) -> Result<RouteResult, String> {
    let lonlats = wps.iter().map(|(la, lo)| format!("{lo},{la}")).collect::<Vec<_>>().join("|");
    let nogos_param = if nogos.is_empty() { String::new() } else { format!("&nogos={nogos}") };
    let url = format!("https://brouter.de/brouter?lonlats={lonlats}&profile={profile}&alternativeidx={alt}&format=geojson{nogos_param}");
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
    let (hw_m, hw_segments) = expressway_segments(&body, &pts);
    Ok(RouteResult { pts, ele, dist_m, time_s, hw_m, hw_segments, ascend_m, via_google: false })
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
// nogos: trigger_routeへ渡したのと同じ値を渡すこと(でないと表示中のルートと違う経路の
// 曲がり案内になってしまう)。
pub fn trigger_turn_points(wps: &[(f64, f64)], mode: &str, alt: u32, pts: &[(f64, f64)], nogos: &str) -> TurnRx {
    let (tx, rx) = std::sync::mpsc::channel();
    let (w, m, p, n) = (wps.to_vec(), mode.to_string(), pts.to_vec(), nogos.to_string());
    std::thread::spawn(move || { let _ = tx.send(fetch_turn_points(&w, &m, alt, &p, &n)); });
    rx
}

// 失敗しても呼び出し側は「曲がり案内なし」に静かにフォールバックできるよう常にVecを返す
// (ルート自体の表示は既存のgeojson取得に依存しており、こちらの失敗で壊さない)。
pub fn fetch_turn_points(wps: &[(f64, f64)], mode: &str, alt: u32, pts: &[(f64, f64)], nogos: &str) -> Vec<TurnPoint> {
    if wps.len() < 2 || pts.is_empty() {
        return Vec::new();
    }
    let alt = alt.min(3);
    let profile = route_profile(mode);
    let lonlats = wps.iter().map(|(la, lo)| format!("{lo},{la}")).collect::<Vec<_>>().join("|");
    let nogos_param = if nogos.is_empty() { String::new() } else { format!("&nogos={nogos}") };
    let url = format!("https://brouter.de/brouter?lonlats={lonlats}&profile={profile}&alternativeidx={alt}&format=gpx&turnInstructionMode=3{nogos_param}");
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

// origin/destination/waypoints/avoid=highwaysの共通クエリ文字列を組み立てる
// (fetch_google_route/fetch_traffic_coloringで共有)。
fn directions_common_params(wps: &[(f64, f64)], mode: &str, key: &str) -> String {
    let origin = format!("{},{}", wps[0].0, wps[0].1);
    let destination = format!("{},{}", wps[wps.len() - 1].0, wps[wps.len() - 1].1);
    let mut s = format!("origin={origin}&destination={destination}&key={key}");
    if wps.len() > 2 {
        let via: Vec<String> = wps[1..wps.len() - 1].iter().map(|(la, lo)| format!("{la},{lo}")).collect();
        s.push_str(&format!("&waypoints={}", via.join("|")));
    }
    // route_profile(mode) が "moped"(下道=高速回避)ならavoid=highwaysを付ける。
    // "shortest"はGoogle側に直接の等価オプションが無いため素のまま(車での既定経路)。
    if route_profile(mode) == "moped" {
        s.push_str("&avoid=highways");
    }
    s
}

// Google Directions API(旧・レガシー版、Routes APIではない)でのフォールバック取得。
// BRouterが失敗した時だけ最終手段として呼ばれる。標高データは提供されないため ele は空Vec、
// 高速区間の判定手段が無いため hw_m は 0.0(料金概算は出ない=呼び出し側で自然にスキップされる)、
// hw_segments も空(高速区間の色分けは出ない)。
fn fetch_google_route(wps: &[(f64, f64)], mode: &str, key: &str) -> Result<RouteResult, String> {
    if key.trim().is_empty() { return Err("Google APIキー未設定".to_string()); }
    let params = directions_common_params(wps, mode, key);
    let url = format!("https://maps.googleapis.com/maps/api/directions/json?{params}");
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
    Ok(RouteResult { pts, ele: Vec::new(), dist_m, time_s, hw_m: 0.0, hw_segments: Vec::new(), ascend_m: 0.0, via_google: true })
}

// ---- 渋滞状況の色分け(#渋滞情報、docs/route-traffic-coloring-design.md) ----
//
// BRouterには渋滞データが無いため、確定したルート pts を距離ベースで区間分割し、
// 区間境界を中間waypointとしてGoogle Directions(departure_time=now)へ1回問い合わせ、
// 区間ごとのduration_in_trafficから緑/黄/赤の色分けを作る。道路網全体ではなく、
// 表示中のルート線だけを塗り分ける(TrafficLayer相当の面データはGoogle側に取得手段が無い)。

const TRAFFIC_SEGMENT_TARGET_M: f64 = 5_000.0; // 目標区間長。短いルートは実際の区間数がこれより少ない
const TRAFFIC_MAX_WAYPOINTS: usize = 23; // Google Directions APIの中間waypoint上限

// ルート総延長(m)から区間数を決める(1以上、TRAFFIC_MAX_WAYPOINTS+1以下)。
pub fn traffic_segment_count(total_len_m: f64) -> usize {
    if total_len_m <= 0.0 { return 1; }
    ((total_len_m / TRAFFIC_SEGMENT_TARGET_M).round() as usize).clamp(1, TRAFFIC_MAX_WAYPOINTS + 1)
}

// 区間境界の累積距離(m)の配列(区間数-1個。0とtotal_len_mそのものは含まない)。
pub fn traffic_breakpoints_m(total_len_m: f64, segments: usize) -> Vec<f64> {
    if segments <= 1 { return Vec::new(); }
    (1..segments).map(|i| total_len_m * i as f64 / segments as f64).collect()
}

// 区間境界のwaypoint座標(pts上の対応点)。
pub fn traffic_waypoints(pts: &[(f64, f64)], breakpoints_m: &[f64]) -> Vec<(f64, f64)> {
    breakpoints_m.iter().map(|&d| point_at(pts, d)).collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrafficLevel { Smooth, Moderate, Heavy }

// duration_in_traffic/durationの比で3段階に分類する。欠損・duration<=0はSmooth扱い
// (渋滞として過剰に赤く塗らない側へ倒す)。
pub fn traffic_level(duration_s: f64, duration_in_traffic_s: Option<f64>) -> TrafficLevel {
    let Some(t) = duration_in_traffic_s else { return TrafficLevel::Smooth };
    if duration_s <= 0.0 { return TrafficLevel::Smooth; }
    let ratio = t / duration_s;
    if ratio >= 1.5 { TrafficLevel::Heavy } else if ratio >= 1.15 { TrafficLevel::Moderate } else { TrafficLevel::Smooth }
}

// Smoothは呼び出し側(colorize_route_by_traffic)で上塗りしない扱いのため、実際に描画で
// 使われるのはModerate/Heavyのみ(Smoothの色は到達しない。列挙の網羅性のためだけに残す)。
pub fn traffic_level_color(level: TrafficLevel) -> [u8; 3] {
    match level {
        TrafficLevel::Smooth => [0, 200, 60],
        TrafficLevel::Moderate => [230, 200, 0],
        TrafficLevel::Heavy => [220, 40, 40],
    }
}

// legs((duration_s, duration_in_traffic_s))をptsに沿って色分けした(色, 点列)の列へ変換する。
// Smooth(順調)区間はエントリを作らない(基調色の青のまま=何も上塗りしない)。
// legs.len() != breakpoints_m.len()+1 なら空Vec(呼び出し側は基調色のままにフォールバックする)。
// 出力されたエントリ同士は、間にSmooth区間を挟まない限り境界点を共有する
// (線が途切れて見えないように)。
pub fn colorize_route_by_traffic(
    pts: &[(f64, f64)],
    breakpoints_m: &[f64],
    legs: &[(f64, Option<f64>)],
) -> Vec<([u8; 3], Vec<(f64, f64)>)> {
    if pts.len() < 2 || legs.len() != breakpoints_m.len() + 1 {
        return Vec::new();
    }
    let cum = cumulative_distances_m(pts);
    let mut out = Vec::new();
    let mut start_idx = 0usize;
    for (i, &(duration, dit)) in legs.iter().enumerate() {
        let level = traffic_level(duration, dit);
        let end_idx = if i < breakpoints_m.len() {
            let bp = breakpoints_m[i];
            cum.iter().position(|&d| d >= bp).unwrap_or(pts.len() - 1).max(start_idx + 1).min(pts.len() - 1)
        } else {
            pts.len() - 1
        };
        // Smooth(順調)区間は上塗りしない(=基調色の青のまま)。start_idxだけは
        // 進めておき、後続区間の切り出し位置がずれないようにする。
        if level != TrafficLevel::Smooth {
            out.push((traffic_level_color(level), pts[start_idx..=end_idx].to_vec()));
        }
        start_idx = end_idx;
    }
    out
}

// wps・modeはfetch_google_routeと同じ意味だが、waypointsは pts を区間分割して作った中間点
// (元のユーザー経由地とは無関係)。失敗しても呼び出し側は「色分けなし」に静かにフォール
// バックできるよう常に(空なら空)Vecを返す。
fn fetch_traffic_coloring(pts: &[(f64, f64)], mode: &str, key: &str) -> Vec<([u8; 3], Vec<(f64, f64)>)> {
    if key.trim().is_empty() || pts.len() < 2 {
        return Vec::new();
    }
    let total_len = polyline_len(pts);
    let segments = traffic_segment_count(total_len);
    let breakpoints = traffic_breakpoints_m(total_len, segments);
    let waypoints = traffic_waypoints(pts, &breakpoints);
    let mut via_wps = Vec::with_capacity(waypoints.len() + 2);
    via_wps.push(pts[0]);
    via_wps.extend(waypoints.iter().copied());
    via_wps.push(pts[pts.len() - 1]);
    let params = directions_common_params(&via_wps, mode, key);
    let url = format!("https://maps.googleapis.com/maps/api/directions/json?{params}&departure_time=now");
    let body = match ureq::get(&url).timeout(std::time::Duration::from_secs(20)).call() {
        Ok(r) => match r.into_string() { Ok(s) => s, Err(_) => return Vec::new() },
        Err(_) => return Vec::new(),
    };
    let legs = match parse_directions_legs(&body) { Ok(l) => l, Err(_) => return Vec::new() };
    colorize_route_by_traffic(pts, &breakpoints, &legs)
}

// trigger_route/trigger_turn_pointsと同じ非ブロッキング方針。
pub type TrafficColorRx = std::sync::mpsc::Receiver<Vec<([u8; 3], Vec<(f64, f64)>)>>;
pub fn trigger_traffic_coloring(pts: &[(f64, f64)], mode: &str, key: &str) -> TrafficColorRx {
    let (tx, rx) = std::sync::mpsc::channel();
    let (p, m, k) = (pts.to_vec(), mode.to_string(), key.to_string());
    std::thread::spawn(move || { let _ = tx.send(fetch_traffic_coloring(&p, &m, &k)); });
    rx
}

// Directions APIの生レスポンス(JSON文字列)からlegsの(duration, duration_in_traffic)を抜き出す。
// ネットワークに触れない純関数。fetch_google_routeと同じエラー処理方針(status!=OK/routes無しはErr)。
pub fn parse_directions_legs(body: &str) -> Result<Vec<(f64, Option<f64>)>, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| format!("Directions parse: {e}"))?;
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if status != "OK" {
        let msg = v.get("error_message").and_then(|m| m.as_str()).unwrap_or(status);
        return Err(format!("Directions: {msg}"));
    }
    let route = v.get("routes").and_then(|r| r.get(0)).ok_or("Directions: routes無し")?;
    let legs_arr = route.get("legs").and_then(|l| l.as_array()).ok_or("Directions: legs無し")?;
    if legs_arr.is_empty() {
        return Err("Directions: legs無し".to_string());
    }
    Ok(legs_arr
        .iter()
        .map(|leg| {
            let duration = leg.get("duration").and_then(|d| d.get("value")).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let duration_in_traffic = leg.get("duration_in_traffic").and_then(|d| d.get("value")).and_then(|v| v.as_f64());
            (duration, duration_in_traffic)
        })
        .collect())
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
pub fn trigger_route(spec: &mut OverlaySpec, wps: &[(f64, f64)], pois: &[(f64, f64, String, PoiCat)], mode: &str, alt: u32, key: &str, nogos: &str) -> (Option<String>, Option<RouteRx>) {
    set_markers(spec, wps, pois);
    spec.routes.clear();
    spec.expressway_segments.clear(); // 古いルートの高速区間を引き継がない
    if wps.len() >= 2 {
        let (tx, rx) = std::sync::mpsc::channel();
        let (w, m, k, n) = (wps.to_vec(), mode.to_string(), key.to_string(), nogos.to_string());
        std::thread::spawn(move || { let _ = tx.send(fetch_route(&w, &m, alt, &k, &n)); });
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

    // ---- 高速道路区間(#高速区間) ----

    // BRouter format=geojson の properties.messages を組み立てるテスト用ヘルパ。
    // rows は (Longitude(マイクロ度), Latitude(マイクロ度), Distance(m), WayTags)。
    // 列の並びは実測した応答(2026/08/17)と同じ。
    fn br_body(rows: &[(i64, i64, &str, &str)]) -> String {
        const HEAD: &str = r#"["Longitude","Latitude","Elevation","Distance","CostPerKm","ElevCost","TurnCost","NodeCost","InitialCost","WayTags","NodeTags","Time","Energy"]"#;
        let rows: Vec<String> = rows.iter()
            .map(|(lon, lat, d, w)| format!(r#"["{lon}","{lat}","3","{d}","0","0","0","0","0","{w}","","0","0"]"#))
            .collect();
        format!(r#"{{"features":[{{"properties":{{"messages":[{HEAD},{}]}}}}]}}"#, rows.join(","))
    }
    // マイクロ度 → (lat, lon) の度。messages 側と同じ座標で pts を組むために使う。
    fn pt(lat_u: i64, lon_u: i64) -> (f64, f64) { (lat_u as f64 / 1e6, lon_u as f64 / 1e6) }
    // 実測値まわりの新宿付近の頂点列(6点)。インデックス0..5。
    fn sample_pts() -> Vec<(f64, f64)> {
        vec![
            pt(35_689_780, 139_701_812), // 0
            pt(35_690_000, 139_702_000), // 1
            pt(35_691_000, 139_703_000), // 2
            pt(35_692_000, 139_704_000), // 3
            pt(35_693_000, 139_705_000), // 4
            pt(35_694_000, 139_706_000), // 5
        ]
    }

    #[test]
    fn expressway_segments_sums_motorway_and_locates_one_range() {
        // 高速1区間: 該当行の指す頂点(1→3)がインデックス範囲として1件返る
        let body = br_body(&[
            (139_702_000, 35_690_000, "50", "highway=residential"),
            (139_704_000, 35_692_000, "100", "highway=motorway maxspeed=80"),
            (139_706_000, 35_694_000, "50", "highway=residential"),
        ]);
        let (m, segs) = expressway_segments(&body, &sample_pts());
        assert!((m - 100.0).abs() < 1e-9);
        assert_eq!(segs, vec![(1, 3)]);
    }

    #[test]
    fn expressway_segments_merges_consecutive_rows() {
        // 連続する複数行(1→2→3→4)は1つの範囲(1,4)へ統合される。motorway_link も高速に数える。
        let body = br_body(&[
            (139_702_000, 35_690_000, "50", "highway=residential"),
            (139_703_000, 35_691_000, "100", "highway=motorway"),
            (139_704_000, 35_692_000, "200", "highway=motorway"),
            (139_705_000, 35_693_000, "30", "highway=motorway_link"),
            (139_706_000, 35_694_000, "50", "highway=residential"),
        ]);
        let (m, segs) = expressway_segments(&body, &sample_pts());
        assert!((m - 330.0).abs() < 1e-9, "motorway_link(30m)も距離に入る: {m}");
        assert_eq!(segs, vec![(1, 4)]);
    }

    #[test]
    fn expressway_segments_keeps_two_ranges_split_by_surface_road() {
        // 一般道を挟んだ高速2区間は統合されず2件になる(=途中で一度高速を降りている)
        let body = br_body(&[
            (139_702_000, 35_690_000, "100", "highway=motorway"),
            (139_703_000, 35_691_000, "50", "highway=primary"),
            (139_704_000, 35_692_000, "80", "highway=tertiary"),
            (139_706_000, 35_694_000, "100", "highway=motorway"),
        ]);
        let (m, segs) = expressway_segments(&body, &sample_pts());
        assert!((m - 200.0).abs() < 1e-9);
        assert_eq!(segs, vec![(0, 1), (3, 5)]);
    }

    #[test]
    fn expressway_segments_counts_motorway_link_alone() {
        // ランプ(motorway_link)だけの行も高速として距離・範囲の両方に入る
        let body = br_body(&[
            (139_702_000, 35_690_000, "40", "highway=motorway_link oneway=yes"),
        ]);
        let (m, segs) = expressway_segments(&body, &sample_pts());
        assert!((m - 40.0).abs() < 1e-9);
        assert_eq!(segs, vec![(0, 1)]);
    }

    #[test]
    fn expressway_segments_returns_distance_only_when_coords_do_not_match() {
        // 座標が pts のどの頂点とも一致しない(位置特定に失敗)場合、hw_m は従来通り返り
        // 範囲だけが空になる。色分けは出ないが距離と料金概算は出る。
        let body = br_body(&[
            (139_702_000, 35_690_000, "100", "highway=motorway"),
        ]);
        let other_pts = vec![pt(34_000_000, 135_000_000), pt(34_001_000, 135_001_000)];
        let (m, segs) = expressway_segments(&body, &other_pts);
        assert!((m - 100.0).abs() < 1e-9);
        assert!(segs.is_empty());
        // pts が空の場合も同じ(位置特定できないが距離は出る)
        assert_eq!(expressway_segments(&body, &[]).1.len(), 0);
        assert!((expressway_segments(&body, &[]).0 - 100.0).abs() < 1e-9);
    }

    #[test]
    fn expressway_segments_needs_distance_and_waytags_columns() {
        // ヘッダに Distance/WayTags が無ければ何も分からない
        let body = r#"{"features":[{"properties":{"messages":[
          ["Longitude","Latitude","Elevation"],
          ["139702000","35690000","3"]
        ]}}]}"#;
        assert_eq!(expressway_segments(body, &sample_pts()), (0.0, Vec::new()));
    }

    #[test]
    fn expressway_segments_handles_empty_and_broken_body() {
        assert_eq!(expressway_segments(r#"{"features":[{"properties":{"messages":[]}}]}"#, &sample_pts()), (0.0, Vec::new()));
        assert_eq!(expressway_segments("not json", &sample_pts()), (0.0, Vec::new()));
        assert_eq!(expressway_segments(r#"{"features":[]}"#, &sample_pts()), (0.0, Vec::new()));
    }

    #[test]
    fn expressway_segments_drops_zero_length_range() {
        // 同じ頂点を2回指す長さ0の行は線として描けないので範囲から捨てる。
        // 距離は Distance 列の合計なので、この切り捨ての影響を受けない。
        let body = br_body(&[
            (139_702_000, 35_690_000, "50", "highway=residential"),
            (139_702_000, 35_690_000, "60", "highway=motorway"), // 同じ頂点(1)を指す
        ]);
        let (m, segs) = expressway_segments(&body, &sample_pts());
        assert!((m - 60.0).abs() < 1e-9);
        assert!(segs.is_empty());
    }

    #[test]
    fn expressway_segments_cursor_moves_forward_on_looping_route() {
        // 折り返して同じ座標を2回通る経路。2行目のBは手前の頂点1ではなく、カーソル以降の
        // 頂点3に一致しなければならない(前方向カーソルが効いていること)。
        let pts = vec![
            pt(35_690_000, 139_702_000), // 0 A
            pt(35_691_000, 139_703_000), // 1 B
            pt(35_692_000, 139_704_000), // 2 C
            pt(35_691_000, 139_703_000), // 3 B(折り返して再訪)
            pt(35_693_000, 139_705_000), // 4 D
        ];
        let body = br_body(&[
            (139_704_000, 35_692_000, "50", "highway=residential"), // → 頂点2
            (139_703_000, 35_691_000, "100", "highway=motorway"),   // → 頂点3(頂点1ではない)
        ]);
        let (m, segs) = expressway_segments(&body, &pts);
        assert!((m - 100.0).abs() < 1e-9);
        assert_eq!(segs, vec![(2, 3)]);
    }

    #[test]
    fn expressway_segments_tolerates_rounding_within_two_micro_degrees() {
        // 丸め誤差(±2マイクロ度≒0.2m)は一致とみなす
        let body = br_body(&[
            (139_702_001, 35_689_999, "100", "highway=motorway"),
        ]);
        let (m, segs) = expressway_segments(&body, &sample_pts());
        assert!((m - 100.0).abs() < 1e-9);
        assert_eq!(segs, vec![(0, 1)]);
    }

    #[test]
    fn expressway_polylines_maps_ranges_to_point_lists() {
        let pts = sample_pts();
        // 正常な範囲は pts[a..=b] と同じ点列
        let got = expressway_polylines(&pts, &[(1, 3)]);
        assert_eq!(got, vec![pts[1..=3].to_vec()]);
        // pts の範囲外を含む範囲は除外
        assert!(expressway_polylines(&pts, &[(1, 99)]).is_empty());
        // 2点未満(始点=終点、逆順)になる範囲は除外
        assert!(expressway_polylines(&pts, &[(2, 2)]).is_empty());
        assert!(expressway_polylines(&pts, &[(3, 1)]).is_empty());
        // 複数範囲はそのまま複数の点列になる
        assert_eq!(expressway_polylines(&pts, &[(0, 1), (3, 5)]).len(), 2);
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

    // テスト用の RouteResult。距離・時間・高速区間だけを差し替える。
    fn rr(dist_m: f64, time_s: f64, hw_m: f64, hw_segments: Vec<(usize, usize)>, via_google: bool) -> RouteResult {
        RouteResult { pts: vec![], ele: vec![], dist_m, time_s, hw_m, hw_segments, ascend_m: 0.0, via_google }
    }

    #[test]
    fn route_summary_marks_google_source() {
        let g = rr(261865.0, 11232.0, 0.0, vec![], true);
        assert!(route_summary("highway", &g).contains("(Google経由)"));
        let b = rr(1000.0, 60.0, 0.0, vec![], false);
        assert!(!route_summary("highway", &b).contains("(Google経由)"));
    }

    #[test]
    fn route_summary_shows_segment_count_only_when_two_or_more() {
        // 1区間では区間数を出さない(従来通りの表示)
        let one = rr(90000.0, 3600.0, 54738.0, vec![(95, 854)], false);
        let s = route_summary("highway", &one);
        assert!(s.contains("高速54.7km ¥1642概算"), "{s}");
        assert!(!s.contains("区間"), "1区間では区間数を出さない: {s}");
        // 2区間以上で「(N区間)」が付く
        let two = rr(49291.0, 3000.0, 32294.0, vec![(95, 311), (628, 897)], false);
        let s2 = route_summary("highway", &two);
        assert!(s2.contains("高速32.3km(2区間) ¥969概算"), "{s2}");
        // hw_m <= 50.0 では高速の表示自体が出ない(区間が入っていても出さない)
        let short = rr(1000.0, 60.0, 50.0, vec![(0, 3), (5, 9)], false);
        let s3 = route_summary("highway", &short);
        assert!(!s3.contains("概算"), "{s3}");
        assert!(!s3.contains("区間"), "{s3}");
    }

    #[test]
    fn should_persist_route_cache_rejects_google_fallback_results() {
        // Googleフォールバック結果をキャッシュしてしまうと、BRouterの一時的失敗が
        // 永久に固定表示され続ける事故になる(2026/08/17)。
        assert!(!should_persist_route_cache(&rr(1.0, 1.0, 0.0, vec![], true)));
        assert!(should_persist_route_cache(&rr(1.0, 1.0, 0.0, vec![], false)));
    }

    #[test]
    fn route_result_serde_back_compat() {
        // via_google / hw_segments が無い旧キャッシュJSONも #[serde(default)] で読める
        let old = r#"{"pts":[],"ele":[],"dist_m":0.0,"time_s":0.0,"hw_m":0.0,"ascend_m":0.0}"#;
        let r: RouteResult = serde_json::from_str(old).expect("旧JSONが読めること");
        assert!(!r.via_google);
        assert!(r.hw_segments.is_empty());
        // 新形式は往復して同じ内容に戻る
        let saved = serde_json::to_string(&rr(1.0, 1.0, 100.0, vec![(1, 3)], false)).unwrap();
        let back: RouteResult = serde_json::from_str(&saved).unwrap();
        assert_eq!(back.hw_segments, vec![(1, 3)]);
    }

    #[test]
    fn route_cache_key_carries_schema_version() {
        // ルートキャッシュは期限を持たないので、RouteResult の作り方を変えたら
        // スキーマ版で旧ファイルを読まないようにする(hw_segments が空のまま読まれると
        // 「距離は出るのに色が出ない」状態が消えずに残る)。
        let wps = [(35.690000, 139.701812), (35.255000, 139.152000)];
        let key = route_cache_key(ROUTE_CACHE_SCHEMA, &wps, "highway", 0, "");
        assert!(key.starts_with("v2|car-fast|0|"), "スキーマ版がキー先頭に入る: {key}");
        // 版が違えば別のキー=別のファイル名になる(=旧キャッシュを拾わない)
        let old_key = route_cache_key("v1", &wps, "highway", 0, "");
        assert_ne!(key, old_key);
        assert_ne!(route_cache_file_name(&key), route_cache_file_name(&old_key));
        // 実際のパスも現行スキーマ版のファイル名になっている
        if let Some(p) = route_cache_path(&wps, "highway", 0, "") {
            assert_eq!(p.file_name().unwrap().to_string_lossy(), route_cache_file_name(&key));
        }
        // 従来通り profile で正規化され、nogos の違いは別キーになる
        assert_eq!(key, route_cache_key(ROUTE_CACHE_SCHEMA, &wps, "fast", 0, ""));
        assert_ne!(key, route_cache_key(ROUTE_CACHE_SCHEMA, &wps, "highway", 0, "139.7,35.6,100"));
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

    fn closure(line: Vec<(f64, f64)>, kind: RegulationKind, active: bool) -> ClosureEvent {
        ClosureEvent { line, kind, detail_id: String::new(), active }
    }

    #[test]
    fn closures_to_nogos_ignores_non_closed_and_inactive_kinds() {
        let events = vec![
            closure(vec![(35.0, 139.0), (35.01, 139.0)], RegulationKind::LaneRestriction, true), // 通行止めではない
            closure(vec![(36.0, 140.0), (36.01, 140.0)], RegulationKind::Closed, false), // まだ予定段階
        ];
        let refs: Vec<&ClosureEvent> = events.iter().collect();
        let (circles, truncated) = closures_to_nogos(&refs, (35.0, 139.0));
        assert!(circles.is_empty(), "車線規制・予定段階の通行止めはnogo対象外");
        assert!(!truncated);
    }

    #[test]
    fn closures_to_nogos_converts_active_closed_line_to_overlapping_circles() {
        // 経線に沿った約1.1kmの直線(0.01度)。150m間隔サンプリングで複数円が並ぶはず。
        let ev = closure(vec![(35.0, 139.0), (35.01, 139.0)], RegulationKind::Closed, true);
        let refs = vec![&ev];
        let (circles, truncated) = closures_to_nogos(&refs, (35.0, 139.0));
        assert!(!truncated);
        assert!(circles.len() >= 5, "1.1kmを150m間隔で刻めば7点前後になるはず: {}", circles.len());
        for (_, _, r) in &circles {
            assert_eq!(*r, NOGO_RADIUS_M);
        }
    }

    #[test]
    fn closures_to_nogos_truncates_to_max_count_keeping_nearest_to_center() {
        // 経線に沿った約55km分の長い通行止め(150m間隔で刻むと370点前後になり上限(NOGO_MAX_COUNT)を超える)。
        let far_line: Vec<(f64, f64)> = (0..5000).map(|i| (35.0 + i as f64 * 0.0001, 139.0)).collect();
        let ev = closure(far_line, RegulationKind::Closed, true);
        let refs = vec![&ev];
        let center = (35.0, 139.0); // ラインの先頭付近を中心にする
        let (circles, truncated) = closures_to_nogos(&refs, center);
        assert!(truncated, "上限(NOGO_MAX_COUNT)を超えるはず");
        assert_eq!(circles.len(), NOGO_MAX_COUNT);
        // 中心に一番近い(先頭側)点が残っているはず(末尾側=遠い点は切り捨てられる)。
        let has_near_start = circles.iter().any(|(lat, _, _)| (*lat - 35.0).abs() < 0.001);
        assert!(has_near_start, "中心に近い点が優先して残るはず");
    }

    #[test]
    fn nogos_query_param_formats_as_lon_lat_radius_pipe_separated() {
        let circles = vec![(35.5, 139.5, 100.0), (36.0, 140.0, 100.0)];
        assert_eq!(nogos_query_param(&circles), "139.5,35.5,100|140,36,100");
    }

    #[test]
    fn nogos_query_param_empty_for_no_circles() {
        assert_eq!(nogos_query_param(&[]), "");
    }

    #[test]
    fn waypoints_bbox_with_margin_covers_all_points_plus_margin() {
        let wps = vec![(35.0, 139.0), (35.5, 139.8), (35.2, 139.3)];
        let (lat_min, lon_min, lat_max, lon_max) = waypoints_bbox_with_margin(&wps, 0.05).unwrap();
        assert!((lat_min - (35.0 - 0.05)).abs() < 1e-9);
        assert!((lon_min - (139.0 - 0.05)).abs() < 1e-9);
        assert!((lat_max - (35.5 + 0.05)).abs() < 1e-9);
        assert!((lon_max - (139.8 + 0.05)).abs() < 1e-9);
    }

    #[test]
    fn waypoints_bbox_with_margin_none_for_empty_waypoints() {
        assert!(waypoints_bbox_with_margin(&[], 0.05).is_none());
    }

    // ---- 渋滞状況の色分け(route-traffic-coloring-design.md) ----

    #[test]
    fn traffic_segment_count_clamps_between_1_and_max() {
        assert_eq!(traffic_segment_count(0.0), 1);
        assert_eq!(traffic_segment_count(-100.0), 1);
        assert_eq!(traffic_segment_count(1_000.0), 1); // 5km未満の短いルート→1区間
        assert_eq!(traffic_segment_count(15_000.0), 3); // 目標5kmちょうど3区間
        assert_eq!(traffic_segment_count(10_000_000.0), TRAFFIC_MAX_WAYPOINTS + 1); // 長大ルートは上限でクランプ
    }

    #[test]
    fn traffic_breakpoints_m_empty_for_single_segment() {
        assert_eq!(traffic_breakpoints_m(10_000.0, 1), Vec::<f64>::new());
    }

    #[test]
    fn traffic_breakpoints_m_splits_evenly() {
        let bp = traffic_breakpoints_m(30_000.0, 3);
        assert_eq!(bp, vec![10_000.0, 20_000.0]);
    }

    #[test]
    fn traffic_waypoints_matches_point_at() {
        let pts = vec![(35.0, 139.0), (35.1, 139.0)]; // 経線に沿った約11.1km
        let total = polyline_len(&pts);
        let bp = vec![total / 2.0];
        let wps = traffic_waypoints(&pts, &bp);
        assert_eq!(wps, vec![point_at(&pts, total / 2.0)]);
    }

    #[test]
    fn traffic_level_thresholds() {
        assert_eq!(traffic_level(900.0, Some(900.0 * 1.10)), TrafficLevel::Smooth); // 1.15未満
        assert_eq!(traffic_level(900.0, Some(900.0 * 1.15)), TrafficLevel::Moderate); // 境界=Moderate側
        assert_eq!(traffic_level(900.0, Some(900.0 * 1.49)), TrafficLevel::Moderate);
        assert_eq!(traffic_level(900.0, Some(900.0 * 1.5)), TrafficLevel::Heavy); // 境界=Heavy側
        assert_eq!(traffic_level(900.0, Some(900.0 * 2.0)), TrafficLevel::Heavy);
        assert_eq!(traffic_level(900.0, None), TrafficLevel::Smooth); // 欠損は過剰に赤く塗らない側
        assert_eq!(traffic_level(0.0, Some(100.0)), TrafficLevel::Smooth); // duration<=0は判定不能
    }

    #[test]
    fn traffic_level_color_moderate_and_heavy_are_distinct() {
        // Smoothは上塗りしない(呼び出し側で使わない)ため、区別すべきはModerate/Heavyのみ。
        assert_ne!(traffic_level_color(TrafficLevel::Moderate), traffic_level_color(TrafficLevel::Heavy));
    }

    #[test]
    fn colorize_route_by_traffic_all_smooth_is_empty() {
        // 全区間順調なら、基調色(青)のまま=上塗りするエントリは無い。
        let pts: Vec<(f64, f64)> = (0..=4).map(|i| (35.0 + i as f64 * 0.01, 139.0)).collect();
        let total = polyline_len(&pts);
        let breakpoints = traffic_breakpoints_m(total, 4);
        let legs = vec![(900.0, Some(900.0)); 4];
        assert!(colorize_route_by_traffic(&pts, &breakpoints, &legs).is_empty());
    }

    #[test]
    fn colorize_route_by_traffic_skips_smooth_and_keeps_boundary_alignment() {
        // 経線に沿った5点(35.00〜35.04、0.01度=約1.1km間隔)を4区間(各区間=1点分)に分割。
        // 区間順: 順調(スキップ)/混雑/順調(スキップ)/やや混雑、という並び。
        let pts: Vec<(f64, f64)> = (0..=4).map(|i| (35.0 + i as f64 * 0.01, 139.0)).collect();
        let total = polyline_len(&pts);
        let breakpoints = traffic_breakpoints_m(total, 4);
        let legs = vec![
            (900.0, Some(900.0)),       // 区間0: 順調→出力されない
            (900.0, Some(900.0 * 1.6)), // 区間1: 混雑(赤)
            (900.0, Some(900.0)),       // 区間2: 順調→出力されない
            (900.0, Some(900.0 * 1.2)), // 区間3: やや混雑(黄)
        ];
        let segs = colorize_route_by_traffic(&pts, &breakpoints, &legs);
        assert_eq!(segs.len(), 2, "順調な2区間はエントリを作らないはず: {segs:?}");
        assert_eq!(segs[0].0, traffic_level_color(TrafficLevel::Heavy));
        assert_eq!(segs[1].0, traffic_level_color(TrafficLevel::Moderate));
        // 先頭(区間0=順調)は正しくスキップされ、混雑区間はptsの始点からは始まらない。
        assert_ne!(segs[0].1.first(), pts.first());
        // 最後のleg(区間3)はptsの終点まで届く。
        assert_eq!(segs[1].1.last(), pts.last());
        // 間に順調区間(区間2)を挟んでいるため、出力された2エントリの境界点はつながらない
        // (=その間は基調色の青のまま、途切れて見えるのは正しい)。
        assert_ne!(segs[0].1.last(), segs[1].1.first());
    }

    #[test]
    fn colorize_route_by_traffic_empty_on_leg_count_mismatch() {
        let pts = vec![(35.0, 139.0), (35.1, 139.0)];
        let breakpoints = vec![5_000.0];
        let legs = vec![(900.0, Some(900.0))]; // 2区間分のbreakpointsに対しlegが1つしか無い
        assert!(colorize_route_by_traffic(&pts, &breakpoints, &legs).is_empty());
    }

    #[test]
    fn colorize_route_by_traffic_empty_for_too_few_points() {
        let legs = vec![(900.0, Some(900.0))];
        assert!(colorize_route_by_traffic(&[(35.0, 139.0)], &[], &legs).is_empty());
    }

    // 実際のDirections APIレスポンス形の抜粋(2legs、うち1つはduration_in_traffic無し)。
    const DIRECTIONS_SAMPLE: &str = r#"{
      "status": "OK",
      "routes": [{
        "legs": [
          {
            "distance": {"text": "10 km", "value": 10000},
            "duration": {"text": "15 mins", "value": 900},
            "duration_in_traffic": {"text": "23 mins", "value": 1380}
          },
          {
            "distance": {"text": "5 km", "value": 5000},
            "duration": {"text": "10 mins", "value": 600}
          }
        ]
      }]
    }"#;

    #[test]
    fn parse_directions_legs_extracts_duration_and_duration_in_traffic() {
        let legs = parse_directions_legs(DIRECTIONS_SAMPLE).unwrap();
        assert_eq!(legs, vec![(900.0, Some(1380.0)), (600.0, None)]);
    }

    #[test]
    fn parse_directions_legs_errors_on_non_ok_status() {
        let body = r#"{"status": "ZERO_RESULTS", "routes": []}"#;
        assert!(parse_directions_legs(body).is_err());
    }

    #[test]
    fn parse_directions_legs_errors_on_missing_routes_or_legs() {
        assert!(parse_directions_legs(r#"{"status": "OK", "routes": []}"#).is_err());
        assert!(parse_directions_legs(r#"{"status": "OK", "routes": [{}]}"#).is_err());
        assert!(parse_directions_legs("not json").is_err());
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。事故調査(新宿→釈迦堂PA、長距離ルートで
    // nogosが多すぎてBRouterのwatchdogに殺され、通行止め回避が意図せずGoogleフォールバックへ
    // 落ちた件)の再現・検証用。`cargo test --release -- --ignored --nocapture`で実行。
    #[test]
    #[ignore]
    fn live_probe_real_nogos_count_and_brouter_survival_for_a_long_route() {
        let wps = vec![(35.690, 139.701), (35.645, 138.714)]; // 新宿 → 釈迦堂PA(甲府方面)相当
        let bbox = waypoints_bbox_with_margin(&wps, 0.05).unwrap();
        let base = crate::regulation::discover_json_base().expect("配信元パスの発見");
        let meshes = crate::mesh::primary_codes(bbox.0, bbox.1, bbox.2, bbox.3);
        println!("meshes: {:?}", meshes);
        let mut all: Vec<crate::regulation::ClosureEvent> = Vec::new();
        for m in &meshes {
            if let Ok(evs) = crate::regulation::fetch_mesh(&base, *m) {
                all.extend(evs);
            }
        }
        println!("total closure events: {}", all.len());
        let refs: Vec<&crate::regulation::ClosureEvent> = all.iter().collect();
        let center = ((bbox.0 + bbox.2) / 2.0, (bbox.1 + bbox.3) / 2.0);
        let (circles, truncated) = closures_to_nogos(&refs, center);
        println!("nogo circles: {} truncated={}", circles.len(), truncated);
        let nogos = nogos_query_param(&circles);
        println!("nogos url length: {}", nogos.len());

        let url = format!(
            "https://brouter.de/brouter?lonlats=139.701,35.690|138.714,35.645&profile=car-fast&alternativeidx=0&format=geojson&nogos={nogos}"
        );
        let start = std::time::Instant::now();
        let result = ureq::get(&url).timeout(std::time::Duration::from_secs(30)).call();
        println!("BRouter took {:?}, ok={}", start.elapsed(), result.is_ok());
        let ok = result.is_ok();
        if let Err(ureq::Error::Status(code, resp)) = result {
            println!("status={code} body={}", resp.into_string().unwrap_or_default());
        }
        assert!(ok, "現状のNOGO_MAX_COUNT({NOGO_MAX_COUNT})でBRouterが失敗するなら要修正");
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。新宿→小田原(car-fast)は実測で
    // 高速1区間(インデックス95〜854・54,738m)になる。BRouterの出力形式が変わって位置特定が
    // 効かなくなったらここで気付ける。`cargo test --release -- --ignored --nocapture`で実行。
    #[test]
    #[ignore]
    fn live_expressway_segments_match_hw_meters_for_shinjuku_odawara() {
        let url = "https://brouter.de/brouter?lonlats=139.701812,35.689780|139.152000,35.255000&profile=car-fast&alternativeidx=0&format=geojson";
        let body = ureq::get(url)
            .set("User-Agent", "termmap/0.1 (personal experiment)")
            .timeout(std::time::Duration::from_secs(30)).call()
            .expect("BRouter応答").into_string().expect("本文");
        let pts = parse_geojson_line(&body).expect("geometry");
        let (hw_m, segs) = expressway_segments(&body, &pts);
        println!("pts={} hw_m={hw_m} segs={segs:?}", pts.len());
        assert!(hw_m > 1000.0, "高速を通るルートのはず: hw_m={hw_m}");
        assert_eq!(segs.len(), 1, "実測では1区間: {segs:?}");
        // 範囲の点列をhaversineで測った長さは、BRouterのDistance合計と概ね一致する
        // (実測の差は77.7kmに対し7m)。5%まで許容する。
        let sum: f64 = expressway_polylines(&pts, &segs).iter().map(|p| polyline_len(p)).sum();
        assert!((sum - hw_m).abs() / hw_m < 0.05, "区間長{sum} と hw_m {hw_m} が乖離");
    }
}
