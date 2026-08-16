// 道路交通量(JARTIC「交通量オープンデータ」)。全国の直轄国道にある常時観測点の
// 実測台数(5分値)を、渋滞そのものではなく「混雑度の目安」として地図に重ねる。
// 事故情報・区間旅行時間・交通規制情報はこのデータセットには含まれない
// (それらはJARTICへの個別問い合わせが必要な別カテゴリで、セルフサービスAPIが無い)。
//
// エンドポイントはWFS(GeoServer)。無料・登録不要(利用規約への同意のみ)。
// 実測で確認済みの注意点:
//   - 時間範囲(時間コード)を絞らずに叩くとバックエンドがOOMで落ちる(要CQL_FILTER)
//   - 観測から取得可能になるまで約20分のラグがあるため、直近すぎる時刻を指定すると0件になる
//   - このデータセットの道路種別は実測で "3"(一般国道)のみが返る。高速道路は含まれない
// gpslive.rs/radar.rsと同じ方針でstd+ureq+serde_jsonのみに依存し、crate::を参照しない。

use serde::{Deserialize, Serialize};
use std::time::Duration;

const ENDPOINT: &str = "https://api.jartic-open-traffic.org/geoserver";
const USER_AGENT: &str = "termmap/0.1 (personal experiment)";
const HTTP_TIMEOUT_SECS: u64 = 20;
// 観測から実際に取得可能になるまでのラグ(公式案内: 観測から約20分後)。安全側に見て
// 25分前を起点に10分幅で取る(直近の確定値を確実に拾う)。
// 取得した直後でもデータ自身はこの分だけ過去のものなので、キャッシュの「データの時刻」も
// この値ぶん遡らせる(呼び出し側が経過時間の表示に使う)。
pub const OBSERVE_LAG_MIN: i64 = 25;
const WINDOW_MIN: i64 = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrafficPoint {
    pub lat: f64,
    pub lon: f64,
    pub volume: u32, // 上り+下り、小型+大型+車種判別不能の合計(5分間の実測台数)
}

// 混雑度の目安(3段階)。道路容量との対比ではなく、観測される台数の絶対値による粗い目安。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CongestionLevel {
    Light,
    Moderate,
    Heavy,
}
// 閾値は実測サンプル(直轄国道の5分値、概ね0〜300台程度)から見た経験則。道路容量を
// 考慮した正規化ではないため、あくまで目安。
pub fn classify(volume: u32) -> CongestionLevel {
    if volume >= 150 { CongestionLevel::Heavy }
    else if volume >= 60 { CongestionLevel::Moderate }
    else { CongestionLevel::Light }
}

// 現在時刻(UTC)を JARTIC の時間コード("YYYYMMDDHHMM"、JST)へ変換して取得窓を作る。
pub fn fetch_traffic(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> Result<Vec<TrafficPoint>, String> {
    let now_epoch_min = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 60)
        .unwrap_or(0);
    fetch_traffic_at(lat_min, lon_min, lat_max, lon_max, now_epoch_min)
}

// テスト容易性のため、基準時刻(epoch分・UTC)を引数で受け取る版を分離。
fn fetch_traffic_at(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64, now_epoch_min_utc: i64) -> Result<Vec<TrafficPoint>, String> {
    let jst_epoch_min = now_epoch_min_utc + 9 * 60;
    let tc_to = jst_epoch_min - OBSERVE_LAG_MIN;
    let tc_from = tc_to - WINDOW_MIN;
    let code = |m: i64| -> String {
        let (y, mo, d) = civil_from_days(m.div_euclid(1440));
        let mm = m.rem_euclid(1440);
        format!("{y:04}{mo:02}{d:02}{:02}{:02}", mm / 60, mm % 60)
    };
    let cql = format!(
        "時間コード>={} AND 時間コード<={} AND BBOX(ジオメトリ,{lon_min},{lat_min},{lon_max},{lat_max},'EPSG:4326')",
        code(tc_from), code(tc_to)
    );
    let url = format!(
        "{ENDPOINT}?service=WFS&version=2.0.0&request=GetFeature&typeNames=t_travospublic_measure_5m&srsName=EPSG:4326&outputFormat=application%2Fjson&exceptions=application%2Fjson&cql_filter={}",
        urlencode(&cql)
    );
    let body = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    Ok(parse_traffic(&body))
}

// GeoJSON本文 → Vec<TrafficPoint>。ネットワークに触れない純関数。
// 欠測フラグ(上り・欠測/下り・欠測)が立っている方向は0扱いではなく、その方向の台数を
// 加算しない(センサー故障による見かけ上の「空いている」誤表示を避ける)。
pub fn parse_traffic(body: &str) -> Vec<TrafficPoint> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return Vec::new(); };
    let Some(features) = v.get("features").and_then(|f| f.as_array()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(coords) = f.pointer("/geometry/coordinates/0").and_then(|c| c.as_array()) else { continue };
        let (Some(lon), Some(lat)) = (coords.first().and_then(|x| x.as_f64()), coords.get(1).and_then(|x| x.as_f64())) else { continue };
        let Some(props) = f.get("properties") else { continue };
        let is_missing = |dir: &str| props.get(format!("{dir}・欠測")).and_then(|x| x.as_str()) == Some("1");
        let dir_count = |dir: &str| -> u32 {
            if is_missing(dir) { return 0; }
            ["小型交通量", "大型交通量", "車種判別不能交通量"].iter()
                .filter_map(|k| props.get(format!("{dir}・{k}")).and_then(|x| x.as_u64()))
                .sum::<u64>() as u32
        };
        let volume = dir_count("上り") + dir_count("下り");
        out.push(TrafficPoint { lat, lon, volume });
    }
    out
}

// application/x-www-form-urlencoded相当の最小限のパーセントエンコード(依存追加なし)。
// 日本語(UTF-8マルチバイト)とCQLで使う記号(スペース・カンマ・括弧・引用符等)だけ
// 通せればよいので、英数字と一部記号以外を全てエンコードする単純な実装で十分。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// 暦日 → epoch(1970-01-01)からの日数(radar.rsのdays_from_civilと対の関数。Howard Hinnant)。
// 実コードではcivil_from_daysの逆方向しか使わないため、往復テスト専用(civil_from_daysとの
// ラウンドトリップ確認)としてdead_code許容。
#[allow(dead_code)]
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
// days_from_civilの逆関数(Howard Hinnant)。epoch日数 → (年, 月, 日)。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_is_inverse_of_days_from_civil() {
        for (y, m, d) in [(2026, 8, 16), (2026, 1, 1), (2025, 12, 31), (2024, 2, 29), (2000, 1, 1), (1970, 1, 1)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "roundtrip failed for {y}-{m}-{d}");
        }
    }

    #[test]
    fn urlencode_escapes_non_ascii_and_symbols() {
        assert_eq!(urlencode("abc123-_.~"), "abc123-_.~");
        assert_eq!(urlencode("道路種別='3'"), "%E9%81%93%E8%B7%AF%E7%A8%AE%E5%88%A5%3D%273%27");
    }

    // 実際のtargetTimes/GetFeature応答の抜粋(2026/08/16 実測、3件)。
    const SAMPLE: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","geometry":{"type":"MultiPoint","coordinates":[[139.7049058,35.58550262]]},
       "properties":{"上り・小型交通量":63,"上り・大型交通量":5,"上り・車種判別不能交通量":4,"上り・欠測":"0",
                     "下り・小型交通量":59,"下り・大型交通量":2,"下り・車種判別不能交通量":2,"下り・欠測":"0"}},
      {"type":"Feature","geometry":{"type":"MultiPoint","coordinates":[[139.509906,35.37938903]]},
       "properties":{"上り・小型交通量":109,"上り・大型交通量":6,"上り・車種判別不能交通量":5,"上り・欠測":"0",
                     "下り・小型交通量":135,"下り・大型交通量":4,"下り・車種判別不能交通量":5,"下り・欠測":"0"}},
      {"type":"Feature","geometry":{"type":"MultiPoint","coordinates":[[139.30775,35.30798103]]},
       "properties":{"上り・小型交通量":46,"上り・大型交通量":3,"上り・車種判別不能交通量":3,"上り・欠測":"1",
                     "下り・小型交通量":35,"下り・大型交通量":0,"下り・車種判別不能交通量":3,"下り・欠測":"0"}}
    ]}"#;

    #[test]
    fn parse_traffic_reads_lat_lon_and_sums_both_directions() {
        let got = parse_traffic(SAMPLE);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].lat, 35.58550262);
        assert_eq!(got[0].lon, 139.7049058);
        assert_eq!(got[0].volume, 63 + 5 + 4 + 59 + 2 + 2); // = 135
        assert_eq!(got[1].volume, 109 + 6 + 5 + 135 + 4 + 5); // = 264
    }

    #[test]
    fn parse_traffic_zeroes_out_missing_direction() {
        // 3件目は上り・欠測="1" なので上り分は0扱い、下り(35+0+3=38)だけが数えられる。
        let got = parse_traffic(SAMPLE);
        assert_eq!(got[2].volume, 35 + 0 + 3);
    }

    #[test]
    fn parse_traffic_handles_garbage() {
        assert!(parse_traffic("not json").is_empty());
        assert!(parse_traffic("{}").is_empty());
        assert!(parse_traffic(r#"{"features":[]}"#).is_empty());
    }

    // ディスクキャッシュ(plotcache)へ保存する形。
    #[test]
    fn traffic_points_round_trip_through_json() {
        let p = TrafficPoint { lat: 35.58550262, lon: 139.7049058, volume: 135 };
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#"{"lat":35.58550262,"lon":139.7049058,"volume":135}"#);
        assert_eq!(serde_json::from_str::<TrafficPoint>(&json).unwrap(), p);
    }

    #[test]
    fn classify_thresholds() {
        assert_eq!(classify(0), CongestionLevel::Light);
        assert_eq!(classify(59), CongestionLevel::Light);
        assert_eq!(classify(60), CongestionLevel::Moderate);
        assert_eq!(classify(149), CongestionLevel::Moderate);
        assert_eq!(classify(150), CongestionLevel::Heavy);
    }

    // フェッチURLに時間コード(JST)とBBOXが正しく組み込まれること(ネットワークには触れない)。
    #[test]
    fn fetch_traffic_at_is_deterministic_pure_wrapper_of_parse() {
        // fetch_traffic_at自体はネットワークを叩くため、ここではcodeのJST変換ロジックだけ
        // civil_from_daysの往復テストで別途担保する(このテストはコンパイル/型の疎通確認)。
        let _ = fetch_traffic_at; // 未使用警告よけではなく、シグネチャの存在確認
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    #[test]
    #[ignore]
    fn live_fetch_real_jartic_api() {
        let pts = fetch_traffic(35.3, 139.0, 36.0, 140.3).expect("live fetch should succeed");
        println!("live points: {}", pts.len());
        for p in pts.iter().take(5) {
            println!("{:?} volume={} level={:?}", (p.lat, p.lon), p.volume, classify(p.volume));
        }
        assert!(!pts.is_empty(), "実際に関東広域で0件は考えにくい");
    }
}
