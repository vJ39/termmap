// 道路ライブカメラ(国土交通省「道路情報提供システム」road-info-prvs.mlit.go.jp)。
// regulation.rsと同じ非公式システムで、認証・APIキー無しで叩ける(実測確認済み)。
//
// 実測で確認済みの構造:
//   - カメラ一覧は地方整備局CD(81=北海道開発局〜90=沖縄総合事務局)ごとのページ
//     pcImage_{整備局CD}_1.html に、input#kokudoJson の value 属性(シングルクォート)として
//     直接JSONが埋め込まれている(regulation.rsのTukoKiseiと違い、別ファイルへの2段階フェッチは不要)。
//     この属性値はHTMLエンティティエスケープされておらず生JSONそのもの(実測確認済み)。
//   - JSON構造: {"路線コード&路線名": [ {"R_路線コード": {カメラ本体}} , ... ], ...}
//   - カメラ本体には gis_point(lon,lat の文字列2要素)・image_name(地点名)・
//     fileList(直近の撮影一覧、新しい順。各要素 get_datetime/file)を含む。
//   - 画像本体は https://www.road-info-prvs.mlit.go.jp/roadinfo/img/doro_gazo/pc/{fileList[].file}
//     で直接取得できる(実測確認済み・200 OK)。file名が"s_"始まりならサムネイル(148x98)、
//     それを外すとフル画像(720x480)になる(pcImageDetail_{id}.htmlの<img>タグから特定)。
//   - 整備局は10局しかなく管轄境界の正確なポリゴンは持っていないため、bboxの中心に一番近い
//     局のカメラだけを取得する簡易割当にする(局境界付近のカメラを取りこぼす可能性はあるが、
//     termmapの表示範囲は通常1局の管轄より十分小さいため実用上問題ない)。
// gpslive.rs/radar.rs/traffic.rs/regulation.rsと同じ方針でstd+ureq+serde_jsonのみに依存し、
// crate::を参照しない。

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::Duration;

const HTML_BASE: &str = "https://www.road-info-prvs.mlit.go.jp/roadinfo/pc/pcImage_";
const IMG_BASE: &str = "https://www.road-info-prvs.mlit.go.jp/roadinfo/img/doro_gazo/pc/";
const USER_AGENT: &str = "termmap/0.1 (personal experiment)";
const HTTP_TIMEOUT_SECS: u64 = 20;

// 地方整備局CDと代表座標(実測: common/js/Common.jsのSEIBIKYOKU_LISTから採取)。
// 実際の管轄境界ではなく代表点なので、bboxの中心に一番近い局を選ぶための目安。
const BUREAUS: &[(u32, f64, f64)] = &[
    (81, 43.07098307, 141.3518977), // 北海道開発局
    (82, 38.2673, 140.8729778),     // 東北地方整備局
    (83, 35.89113488, 139.6341152), // 関東地方整備局
    (84, 37.89482198, 139.019011),  // 北陸地方整備局
    (85, 35.17692021, 136.897057),  // 中部地方整備局
    (86, 34.68861644, 135.5193277), // 近畿地方整備局
    (87, 34.39959478, 132.4614834), // 中国地方整備局
    (88, 34.35236213, 134.0455805), // 四国地方整備局
    (89, 33.58793265, 130.4244733), // 九州地方整備局
    (90, 26.22762325, 127.6909037), // 沖縄総合事務局
];

// ディスクキャッシュ(plotcache)へ保存するのは設置位置(id/lat/lon/name)だけにする。
// 写真URLは15分ごとの撮影ディレクトリを含む(実測 .../20260816160000/811C200101.jpeg)ので、
// 1時間後には404になる。保存すると「キャッシュから読んだのに写真が出ない」状態を作るだけなので
// skip し、読み込み時は None / 空文字になる(必要になった時点で取り直す)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoadCamera {
    pub id: String,
    pub lat: f64,
    pub lon: f64,
    pub name: String,
    #[serde(skip)]
    pub thumb_url: Option<String>, // 直近のサムネイル(一覧用、小さい)
    #[serde(skip)]
    pub full_url: Option<String>, // 直近のフル画像(詳細表示用)
    #[serde(skip)]
    pub taken_at: String, // フル画像の撮影時刻(get_datetime)
}

// 指定座標に最も近い地方整備局CDを返す(常にどれか1つは返る)。
pub fn nearest_bureau(lat: f64, lon: f64) -> u32 {
    BUREAUS
        .iter()
        .min_by(|a, b| {
            let da = (a.1 - lat).powi(2) + (a.2 - lon).powi(2);
            let db = (b.1 - lat).powi(2) + (b.2 - lon).powi(2);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|b| b.0)
        .unwrap_or(83)
}

// pcImage_{cd}_1.html 本文から input#kokudoJson の value(シングルクォート囲み・生JSON)を取り出す。
fn extract_kokudo_json(html: &str) -> Option<&str> {
    let marker = "id=\"kokudoJson\"";
    let i = html.find(marker)?;
    let rest = &html[i + marker.len()..];
    let vmarker = "value='";
    let vstart = rest.find(vmarker)? + vmarker.len();
    let rest2 = &rest[vstart..];
    let vend = rest2.find('\'')?;
    Some(&rest2[..vend])
}

// 地方整備局1局ぶんのカメラを全件返す(bboxでは絞らない。絞り込みは呼び出し側のメモリ上で行う)。
// 失敗と0件を区別できるよう Result を返す。以前は両方 Vec::new() だったため、圏外に入った
// 瞬間に呼び出し側が「カメラ0台」で上書きし、直前まで見えていたカメラが消えていた。
pub fn fetch_bureau(cd: u32) -> Result<Vec<RoadCamera>, String> {
    let url = format!("{HTML_BASE}{cd}_1.html");
    let html = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("道路カメラ一覧: {e}"))?
        .into_string()
        .map_err(|e| format!("道路カメラ一覧の読み取り: {e}"))?;
    Ok(parse_cameras(&html))
}

// pcImage_{cd}_1.html 本文 → Vec<RoadCamera>。ネットワークに触れない純関数。
pub fn parse_cameras(html: &str) -> Vec<RoadCamera> {
    let Some(raw) = extract_kokudo_json(html) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else { return Vec::new() };
    let Some(routes) = v.as_object() else { return Vec::new() };
    let mut out = Vec::new();
    for arr in routes.values() {
        let Some(arr) = arr.as_array() else { continue };
        for item in arr {
            let Some(item_obj) = item.as_object() else { continue };
            for cam in item_obj.values() {
                let Some(id) = cam.get("doro_gazo_joho_kanri_id").and_then(|x| x.as_str()) else { continue };
                let Some(gis) = cam.get("gis_point").and_then(|x| x.as_array()) else { continue };
                let Some(lon) = gis.first().and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()) else { continue };
                let Some(lat) = gis.get(1).and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()) else { continue };
                let name = cam.get("image_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let latest = cam.get("fileList").and_then(|x| x.as_array()).and_then(|files| files.first());
                let (thumb_url, full_url, taken_at) = match latest {
                    Some(f) => {
                        let file = f.get("file").and_then(|x| x.as_str()).unwrap_or("");
                        let taken_at = f.get("get_datetime").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        if file.is_empty() {
                            (None, None, taken_at)
                        } else {
                            let thumb = format!("{IMG_BASE}{file}");
                            let full = format!("{IMG_BASE}{}", file.replace("/s_", "/"));
                            (Some(thumb), Some(full), taken_at)
                        }
                    }
                    None => (None, None, String::new()),
                };
                out.push(RoadCamera { id: id.to_string(), lat, lon, name, thumb_url, full_url, taken_at });
            }
        }
    }
    out
}

// カメラのフル画像(url=RoadCamera::full_url)をRgbImageとして取得する。
pub fn fetch_image(url: &str) -> Result<image::RgbImage, String> {
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("camera image: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf).map_err(|e| format!("camera image read: {e}"))?;
    image::load_from_memory(&buf).map(|i| i.to_rgb8()).map_err(|e| format!("画像デコード: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_bureau_picks_hokkaido_for_a_hokkaido_point() {
        assert_eq!(nearest_bureau(43.07, 141.35), 81);
    }

    #[test]
    fn nearest_bureau_picks_kanto_for_tokyo() {
        assert_eq!(nearest_bureau(35.68, 139.77), 83);
    }

    #[test]
    fn nearest_bureau_picks_okinawa() {
        assert_eq!(nearest_bureau(26.2, 127.7), 90);
    }

    #[test]
    fn extract_kokudo_json_finds_single_quoted_value() {
        let html = r#"<input type="hidden" id="kokudoJson"    value='{"a":1}' />"#;
        assert_eq!(extract_kokudo_json(html), Some(r#"{"a":1}"#));
    }

    #[test]
    fn extract_kokudo_json_none_when_missing() {
        assert!(extract_kokudo_json("<html>no camera data here</html>").is_none());
    }

    // 実際のpcImage_81_1.html kokudoJsonの抜粋(2026/08/16 実測、1カメラぶん)。
    const SAMPLE_HTML: &str = r##"<input type="hidden" id="kokudoJson"    value='{"30005&国道5号":[{"R_30005":{"doro_gazo_joho_kanri_id":"811C200101","seibikyoku_cd":"81","gis_point":["140.364438390416","42.4982926115278"],"image_name":"長万部町大浜情報板","fileList":[{"get_datetime":"2026-08-16 16:00:36","kiki_jotai_cd":1,"file":"20260816160000/s_811C200101.jpeg"},{"get_datetime":"2026-08-16 15:45:37","kiki_jotai_cd":1,"file":"20260816154500/s_811C200101.jpeg"}]}}]}' />"##;

    #[test]
    fn parse_cameras_extracts_position_name_and_urls() {
        let got = parse_cameras(SAMPLE_HTML);
        assert_eq!(got.len(), 1);
        let c = &got[0];
        assert_eq!(c.id, "811C200101");
        assert_eq!(c.lat, 42.4982926115278);
        assert_eq!(c.lon, 140.364438390416);
        assert_eq!(c.name, "長万部町大浜情報板");
        assert_eq!(c.taken_at, "2026-08-16 16:00:36");
        assert_eq!(
            c.thumb_url.as_deref(),
            Some("https://www.road-info-prvs.mlit.go.jp/roadinfo/img/doro_gazo/pc/20260816160000/s_811C200101.jpeg")
        );
        assert_eq!(
            c.full_url.as_deref(),
            Some("https://www.road-info-prvs.mlit.go.jp/roadinfo/img/doro_gazo/pc/20260816160000/811C200101.jpeg")
        );
    }

    #[test]
    fn parse_cameras_handles_garbage() {
        assert!(parse_cameras("not html at all").is_empty());
        assert!(parse_cameras(r#"<input id="kokudoJson" value='not json' />"#).is_empty());
        assert!(parse_cameras(r#"<input id="kokudoJson" value='{}' />"#).is_empty());
    }

    #[test]
    fn parse_cameras_skips_entries_missing_required_fields() {
        // gis_point欠如のカメラは黙って除外(座標が無いと地図に置けない)。
        let html = r#"<input id="kokudoJson" value='{"r":[{"R_1":{"doro_gazo_joho_kanri_id":"X"}}]}' />"#;
        assert!(parse_cameras(html).is_empty());
    }

    // 位置だけを保存し、撮影時刻つきURLは保存しない(読み戻すと None / 空文字になる)。
    #[test]
    fn serde_keeps_the_position_and_drops_the_expiring_photo_urls() {
        let c = parse_cameras(SAMPLE_HTML).remove(0);
        assert!(c.full_url.is_some(), "取得直後はURLを持っている");
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("full_url"), "URLは保存しない: {json}");
        assert!(!json.contains("20260816160000"), "撮影ディレクトリを持ち越さない: {json}");
        let back: RoadCamera = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, c.id);
        assert_eq!(back.lat, c.lat);
        assert_eq!(back.lon, c.lon);
        assert_eq!(back.name, c.name);
        assert_eq!(back.full_url, None);
        assert_eq!(back.thumb_url, None);
        assert_eq!(back.taken_at, "");
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    #[test]
    #[ignore]
    fn live_fetch_real_camera_data() {
        // 関東地方整備局(東京周辺)。実際に何件かカメラがあるはず。
        let cams = fetch_bureau(nearest_bureau(35.68, 139.77)).expect("live fetch should succeed");
        println!("cameras: {}", cams.len());
        for c in cams.iter().take(5) {
            println!("{} {} {:.4},{:.4} full={:?}", c.id, c.name, c.lat, c.lon, c.full_url);
        }
        assert!(!cams.is_empty(), "実際に関東で0件は考えにくい");
    }
}
