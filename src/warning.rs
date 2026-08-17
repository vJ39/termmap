// 気象警報・注意報(気象庁防災情報API)。ルート沿いのclass10s領域(geoarea.rs)に対応する
// 気象台コードごとにwarning.jsonを取得し、現在発表中の警報/注意報を返す。
// gpslive.rs/radar.rs/traffic.rs等と同じ方針でstd+ureq+serde_jsonのみに依存し、
// crate::を参照しない。
//
// 実測で確認済みの構造(2026/08/17、東京都130000):
//   {"areaTypes":[{"areas":[
//     {"code":"130010","warnings":[{"status":"発表警報・注意報はなし"}]},
//     {"code":"130020","warnings":[{"code":"14","status":"継続"},{"code":"20","status":"継続"}]}
//   ]}, ...]}
// warningsの各要素は、発表中のものだけ"code"キーを持つ(codeが無い場合はstatusのみで
// 「発表なし」を意味する)。statusが"解除"のものは現在は効力が無いはずだが、実データで
// "解除"直後の扱いを確認できていないため、安全側でstatusが"継続"または"発表"のものだけを
// 「現在有効」として扱う(未知のstatusは有効扱いしない)。
//
// 警報コードの名称対応表はWeb上の複数の一致する情報源で確認したもので、気象庁一次資料での
// 確認はできていない(docs/weather-warning-overlay-design.md §2に記載の通り)。表に無い
// コードは「気象情報」(特別警報/警報/注意報のいずれの文字列も含まない無難な表示)へ
// フォールバックする。
// 色分けは正確なコード対応より名称文字列(特別警報/警報/注意報)によるseverityを優先する
// (コード対応表の精度が不確かでも、名称の文字列パターンは安定しているため)。

use serde::Deserialize;
use std::time::Duration;

const ENDPOINT: &str = "https://www.jma.go.jp/bosai/warning/data/warning/";
const USER_AGENT: &str = "termmap/0.1 (personal experiment)";
const HTTP_TIMEOUT_SECS: u64 = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Special, // 特別警報
    Warning, // 警報
    Advisory, // 注意報
    Other,
}

impl Severity {
    pub fn color(&self) -> [u8; 3] {
        match self {
            Severity::Special => [230, 30, 200],  // 特別警報: 気象庁の慣例(紫/マゼンタ系)に寄せる
            Severity::Warning => [230, 60, 30],   // 警報: 赤
            Severity::Advisory => [230, 200, 40], // 注意報: 黄
            Severity::Other => [150, 150, 150],
        }
    }
}

// コード→名称の対応表(実測・複数情報源で確認。気象庁一次資料での確認はまだ)。
// 表に無いコードは"気象情報"へフォールバックする(severity_ofが"警報"/"注意報"を
// 含む文字列で誤分類しないよう、意図的にそれらの語を含まない表現にしている)。
fn code_name(code: &str) -> &'static str {
    match code {
        "02" => "暴風雪警報",
        "03" => "大雨警報",
        "04" => "洪水警報",
        "05" => "暴風警報",
        "06" => "大雪警報",
        "07" => "波浪警報",
        "08" => "高潮警報",
        "09" => "土砂災害警報",
        "10" => "大雨注意報",
        "12" => "大雪注意報",
        "13" => "風雪注意報",
        "14" => "雷注意報",
        "15" => "強風注意報",
        "16" => "波浪注意報",
        "17" => "融雪注意報",
        "18" => "洪水注意報",
        "19" => "高潮注意報",
        "20" => "濃霧注意報",
        "21" => "乾燥注意報",
        "22" => "なだれ注意報",
        "23" => "低温注意報",
        "24" => "霜注意報",
        "25" => "着氷注意報",
        "26" => "着雪注意報",
        "29" => "土砂災害注意報",
        "32" => "暴風雪特別警報",
        "33" => "大雨特別警報",
        "35" => "暴風特別警報",
        "36" => "大雪特別警報",
        _ => "気象情報",
    }
}

fn severity_of(name: &str) -> Severity {
    if name.contains("特別警報") {
        Severity::Special
    } else if name.contains("警報") {
        Severity::Warning
    } else if name.contains("注意報") {
        Severity::Advisory
    } else {
        Severity::Other
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveWarning {
    pub area_code: String, // class10s コード(geoarea::Region::code)
    pub name: String,      // 表示名(例: "濃霧注意報")
    pub severity: Severity,
}

#[derive(Deserialize)]
struct WarningItem {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    status: String,
}
#[derive(Deserialize)]
struct AreaItem {
    code: String,
    #[serde(default)]
    warnings: Vec<WarningItem>,
}
#[derive(Deserialize)]
struct AreaTypeItem {
    #[serde(default)]
    areas: Vec<AreaItem>,
}
#[derive(Deserialize)]
struct WarningResp {
    #[serde(default, rename = "areaTypes")]
    area_types: Vec<AreaTypeItem>,
}

// warning.json本文 → 現在有効な警報/注意報一覧。ネットワークに触れない純関数。
// statusが"継続"/"発表"以外(codeが無い"発表警報・注意報はなし"、または未知の値)は含めない。
pub fn parse_warnings(body: &str) -> Vec<ActiveWarning> {
    let Ok(resp) = serde_json::from_str::<WarningResp>(body) else { return Vec::new() };
    let mut out = Vec::new();
    for at in resp.area_types {
        for area in at.areas {
            for w in area.warnings {
                let Some(code) = &w.code else { continue }; // codeが無い="発表なし"
                if w.status != "継続" && w.status != "発表" {
                    continue; // "解除"等、現在有効でないもの・未知のstatusは含めない
                }
                let name = code_name(code).to_string();
                let severity = severity_of(&name);
                out.push(ActiveWarning { area_code: area.code.clone(), name, severity });
            }
        }
    }
    out
}

// 気象台コード(geoarea::Region::office_code)1つぶんの警報・注意報一覧を取得する。
pub fn fetch_warnings(office_code: &str) -> Result<Vec<ActiveWarning>, String> {
    let url = format!("{ENDPOINT}{office_code}.json");
    let body = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("気象警報({office_code}): {e}"))?
        .into_string()
        .map_err(|e| format!("気象警報({office_code})の読み取り: {e}"))?;
    Ok(parse_warnings(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 実際のwarning/data/warning/130000.jsonの抜粋(2026/08/17実測)。
    const SAMPLE: &str = r#"{"areaTypes":[{"areas":[
        {"code":"130010","warnings":[{"status":"発表警報・注意報はなし"}]},
        {"code":"130020","warnings":[{"code":"14","status":"継続"},{"code":"20","status":"継続"}]}
    ]}]}"#;

    #[test]
    fn parse_warnings_extracts_only_active_ones_with_a_code() {
        let got = parse_warnings(SAMPLE);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].area_code, "130020");
        assert_eq!(got[0].name, "雷注意報");
        assert_eq!(got[0].severity, Severity::Advisory);
        assert_eq!(got[1].name, "濃霧注意報");
    }

    #[test]
    fn parse_warnings_skips_area_with_no_active_warning() {
        let got = parse_warnings(SAMPLE);
        assert!(!got.iter().any(|w| w.area_code == "130010"));
    }

    #[test]
    fn parse_warnings_skips_unknown_status_values() {
        let body = r#"{"areaTypes":[{"areas":[
            {"code":"130010","warnings":[{"code":"03","status":"解除"}]}
        ]}]}"#;
        assert!(parse_warnings(body).is_empty());
    }

    #[test]
    fn parse_warnings_accepts_hatsuhyou_status_too() {
        let body = r#"{"areaTypes":[{"areas":[
            {"code":"130010","warnings":[{"code":"33","status":"発表"}]}
        ]}]}"#;
        let got = parse_warnings(body);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "大雨特別警報");
        assert_eq!(got[0].severity, Severity::Special);
    }

    #[test]
    fn parse_warnings_handles_garbage_without_panicking() {
        assert!(parse_warnings("not json").is_empty());
        assert!(parse_warnings("{}").is_empty());
        assert!(parse_warnings(r#"{"areaTypes":[]}"#).is_empty());
    }

    #[test]
    fn code_name_falls_back_for_unknown_codes() {
        assert_eq!(code_name("99"), "気象情報");
        assert_eq!(severity_of(code_name("99")), Severity::Other);
    }

    #[test]
    fn severity_of_classifies_by_name_pattern() {
        assert_eq!(severity_of("大雨特別警報"), Severity::Special);
        assert_eq!(severity_of("大雨警報"), Severity::Warning);
        assert_eq!(severity_of("大雨注意報"), Severity::Advisory);
    }

    #[test]
    fn severity_colors_are_distinct() {
        let colors = [Severity::Special.color(), Severity::Warning.color(), Severity::Advisory.color(), Severity::Other.color()];
        for i in 0..colors.len() {
            for j in (i + 1)..colors.len() {
                assert_ne!(colors[i], colors[j], "severity color {i} and {j} should differ");
            }
        }
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    #[test]
    #[ignore]
    fn live_fetch_real_warning_data() {
        let warnings = fetch_warnings("130000").expect("live fetch should succeed");
        println!("warnings: {}", warnings.len());
        for w in warnings.iter().take(5) {
            println!("{:?}", w);
        }
    }
}
