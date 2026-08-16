// 通行規制情報(国土交通省「道路情報提供システム」road-info-prvs.mlit.go.jp)。
// JARTICの交通量とは別の非公式システムで、認証・APIキー無しで叩ける(実測確認済み)。
// 通行止め/車線規制/片側交互通行/チェーン規制/移動規制の実際の区間ライン(GeoJSON)を返す。
// JARTICでは取れない「事故・災害・工事による通行止め」に近い情報がここにある
// (原因コードの正確な文言対応表までは特定できていないが、規制区間そのものは実データ)。
//
// 実測で確認済みの注意点:
//   - データの実体は "{JSON配信元}/TukoKisei/{1次メッシュコード}.json" にあるが、
//     配信元パス(タイムスタンプ+ランダムハッシュ)は更新のたびに変わるため、
//     まずpcTukokisei_81_1.htmlを取得してパスを都度発見する必要がある(2段階フェッチ)。
//   - 1次メッシュコード(JIS X 0410、約80km四方)単位でファイルが分かれているため、
//     表示範囲を覆う全メッシュコードを列挙して個別に取得する。
//     bbox→メッシュコードの割り出しは mesh.rs が持ち、ここはメッシュ1枚を取る役に徹する
//     (呼び出し側がセル単位でキャッシュするため、配信元パスの発見とメッシュ取得を分けてある)。
// gpslive.rs/radar.rs/traffic.rsと同じ方針でstd+ureq+serde_jsonのみに依存し、
// crate::を参照しない。

use serde::{Deserialize, Serialize};
use std::time::Duration;

const BASE: &str = "https://www.road-info-prvs.mlit.go.jp/roadinfo";
const TUKOKISEI_PAGE: &str = "https://www.road-info-prvs.mlit.go.jp/roadinfo/pc/pcTukokisei_81_1.html";
const USER_AGENT: &str = "termmap/0.1 (personal experiment)";
const HTTP_TIMEOUT_SECS: u64 = 20;

// serdeのユニットバリアントは既定でバリアント名の文字列になる("Closed" 等)。ディスクへ
// 保存するときはこの名前で持つ(元の kisei_naiyo_cd は ClosureEvent に残っていないため)。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RegulationKind {
    Closed,              // 通行止め(冬期含む)
    LaneRestriction,     // 車線規制
    AlternatingOneLane,  // 片側交互通行
    ChainRequired,       // チェーン規制
    MovementRestriction, // 移動規制
    Other,
}

impl RegulationKind {
    // kisei_naiyo_cd(規制内容CD、実測: "01"=通行止め/"04"=車線規制/"05"=片側交互通行/
    // "06"=チェーン規制/"09"=移動規制)から分類する。"08"もTukokisei.js側で通行止け相当
    // として扱われていたため同様に扱う。未知の値は黙ってOtherへ。
    fn from_code(code: &str) -> Self {
        match code {
            "01" | "08" => RegulationKind::Closed,
            "04" => RegulationKind::LaneRestriction,
            "05" => RegulationKind::AlternatingOneLane,
            "06" => RegulationKind::ChainRequired,
            "09" => RegulationKind::MovementRestriction,
            _ => RegulationKind::Other,
        }
    }
    // 元データ(geo_json.style.color)は実測でほぼ灰色(#808080)一色で見づらいため、
    // 種別ごとに視認性の良い独自配色にする。
    pub fn color(&self) -> [u8; 3] {
        match self {
            RegulationKind::Closed => [220, 30, 30],
            RegulationKind::LaneRestriction => [230, 140, 30],
            RegulationKind::AlternatingOneLane => [230, 200, 40],
            RegulationKind::ChainRequired => [60, 170, 230],
            RegulationKind::MovementRestriction => [160, 80, 200],
            RegulationKind::Other => [150, 150, 150],
        }
    }
    // 現状は地図に色分けした線を引くだけでui.rsからは未使用(将来の詳細表示用に残す)。
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            RegulationKind::Closed => "通行止め",
            RegulationKind::LaneRestriction => "車線規制",
            RegulationKind::AlternatingOneLane => "片側交互通行",
            RegulationKind::ChainRequired => "チェーン規制",
            RegulationKind::MovementRestriction => "移動規制",
            RegulationKind::Other => "規制",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClosureEvent {
    pub line: Vec<(f64, f64)>, // (lat, lon)の順(termmapの座標順に合わせる。ソースはlon,lat順なので変換する)
    pub kind: RegulationKind,
}

// pcTukokisei_81_1.html本文から、その時点のJSON配信元ベースURLを取り出す。
// <script src="../backup/{timestamp}/{hash}/xxx.js"> の形を探す(実測で確認済みの形)。
fn extract_json_base(html: &str) -> Option<String> {
    let needle = "../backup/";
    let i = html.find(needle)?;
    let rest = &html[i + needle.len()..];
    // "{timestamp}/{hash}/" の2階層ぶんだけ切り出す(3つ目の '/' の手前まで)。
    let mut slashes = rest.match_indices('/');
    let (first, _) = slashes.next()?;
    let (second, _) = slashes.next()?;
    let _ = first;
    let dir = &rest[..second + 1];
    Some(format!("{BASE}/backup/{dir}"))
}

// その時点のJSON配信元ベースURLを発見する(1段目)。パスは更新のたびに変わるので保存できず、
// 取得のたびに引き直す必要がある。複数メッシュを取るときは1回だけ呼んで使い回す。
pub fn discover_json_base() -> Result<String, String> {
    let html = ureq::get(TUKOKISEI_PAGE)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("通行規制の配信元: {e}"))?
        .into_string()
        .map_err(|e| format!("通行規制の配信元の読み取り: {e}"))?;
    extract_json_base(&html).ok_or_else(|| "通行規制の配信元パスが見つからない".to_string())
}

// 1次メッシュ1枚ぶんの規制情報を取る(2段目)。base は discover_json_base() の戻り値。
// 失敗と0件を区別できるよう Result を返す。以前は両方 Vec::new() だったため、圏外に入った
// 瞬間に呼び出し側が「規制0件」で上書きし、直前まで見えていた通行止めが消えていた。
pub fn fetch_mesh(base: &str, mesh: u32) -> Result<Vec<ClosureEvent>, String> {
    let url = format!("{base}TukoKisei/{mesh}.json");
    let body = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("通行規制(メッシュ{mesh}): {e}"))?
        .into_string()
        .map_err(|e| format!("通行規制(メッシュ{mesh})の読み取り: {e}"))?;
    Ok(parse_closures(&body))
}

// TukoKisei/{mesh}.json 本文 → Vec<ClosureEvent>。ネットワークに触れない純関数。
pub fn parse_closures(body: &str) -> Vec<ClosureEvent> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return Vec::new(); };
    let Some(arr) = v.as_array() else { return Vec::new(); };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(code) = item.get("kisei_naiyo_cd").and_then(|x| x.as_str()) else { continue };
        let Some(geo_json_str) = item.get("geo_json").and_then(|x| x.as_str()) else { continue };
        let Ok(geo) = serde_json::from_str::<serde_json::Value>(geo_json_str) else { continue };
        let Some(coords) = geo.pointer("/geometry/coordinates").and_then(|c| c.as_array()) else { continue };
        let line: Vec<(f64, f64)> = coords
            .iter()
            .filter_map(|c| {
                let pair = c.as_array()?;
                let lon = pair.first()?.as_f64()?;
                let lat = pair.get(1)?.as_f64()?;
                Some((lat, lon))
            })
            .collect();
        if line.len() < 2 {
            continue; // 線として描けない(座標欠損)ものは黙って捨てる
        }
        out.push(ClosureEvent { line, kind: RegulationKind::from_code(code) });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // bbox→1次メッシュコードの計算はこのモジュールから mesh.rs へ移した
    // (呼び出し側がセル単位でキャッシュするようになり、ここでbboxを割る必要がなくなったため)。
    // 当時ここで固定していた既知値は mesh.rs のテストが引き継いでいる。

    #[test]
    fn extract_json_base_finds_backup_path() {
        let html = r#"<script src="../backup/20260816154000/dT2NBtTsc6ZmtWLu/sWhmyWPN.js"></script>"#;
        assert_eq!(
            extract_json_base(html).unwrap(),
            format!("{BASE}/backup/20260816154000/dT2NBtTsc6ZmtWLu/")
        );
    }

    #[test]
    fn extract_json_base_none_when_missing() {
        assert!(extract_json_base("<html>no backup path here</html>").is_none());
    }

    #[test]
    fn regulation_kind_from_code_covers_known_values() {
        assert_eq!(RegulationKind::from_code("01"), RegulationKind::Closed);
        assert_eq!(RegulationKind::from_code("08"), RegulationKind::Closed);
        assert_eq!(RegulationKind::from_code("04"), RegulationKind::LaneRestriction);
        assert_eq!(RegulationKind::from_code("05"), RegulationKind::AlternatingOneLane);
        assert_eq!(RegulationKind::from_code("06"), RegulationKind::ChainRequired);
        assert_eq!(RegulationKind::from_code("09"), RegulationKind::MovementRestriction);
        assert_eq!(RegulationKind::from_code("99"), RegulationKind::Other);
    }

    // 実際のTukoKisei/5339.jsonの抜粋(2026/08/16 実測、1件)。
    const SAMPLE: &str = r##"[
      {
        "kisei_kaishi_nichiji": "2026-08-03 09:00:00",
        "kisei_naiyo_cd": "04",
        "kisei_naiyo_shosai_cd": "001",
        "genin_jisho_cd": "05",
        "kisei_jishi_jyokyo": "1",
        "doro_cd": "3",
        "geo_json": "{\"type\": \"Feature\", \"properties\": {\"style\": {\"color\":\"#99CC00\", \"weight\":4, \"opacity\":1}}, \"geometry\":{\"type\":\"LineString\",\"coordinates\":[[139.73312545468,35.6408503440756],[139.732902715776,35.6404397830738]]}}"
      },
      {
        "kisei_naiyo_cd": "01",
        "geo_json": "{\"geometry\":{\"type\":\"LineString\",\"coordinates\":[[139.1,35.1]]}}"
      }
    ]"##;

    #[test]
    fn parse_closures_extracts_line_and_kind() {
        let got = parse_closures(SAMPLE);
        // 2件目は座標が1点しかない(線にならない)ので除外され、1件だけ残る。
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, RegulationKind::LaneRestriction);
        assert_eq!(got[0].line, vec![(35.6408503440756, 139.73312545468), (35.6404397830738, 139.732902715776)]);
    }

    #[test]
    fn parse_closures_handles_garbage() {
        assert!(parse_closures("not json").is_empty());
        assert!(parse_closures("[]").is_empty());
        assert!(parse_closures(r#"[{"kisei_naiyo_cd":"01"}]"#).is_empty()); // geo_json欠如
    }

    // ディスクへ保存する形(設計 §5.3)。kind はバリアント名の文字列で持つ。
    #[test]
    fn closure_events_round_trip_through_json_as_line_and_kind_name() {
        let ev = ClosureEvent {
            line: vec![(35.64085, 139.733125), (35.64044, 139.732903)],
            kind: RegulationKind::Closed,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"line":[[35.64085,139.733125],[35.64044,139.732903]],"kind":"Closed"}"#
        );
        assert_eq!(serde_json::from_str::<ClosureEvent>(&json).unwrap(), ev);
    }

    #[test]
    fn every_regulation_kind_survives_a_json_round_trip() {
        for k in [
            RegulationKind::Closed,
            RegulationKind::LaneRestriction,
            RegulationKind::AlternatingOneLane,
            RegulationKind::ChainRequired,
            RegulationKind::MovementRestriction,
            RegulationKind::Other,
        ] {
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(serde_json::from_str::<RegulationKind>(&json).unwrap(), k, "{json}");
        }
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    #[test]
    #[ignore]
    fn live_fetch_real_regulation_data() {
        let base = discover_json_base().expect("配信元パスの発見");
        let events = fetch_mesh(&base, 5339).expect("メッシュ取得");
        println!("events: {}", events.len());
        for e in events.iter().take(5) {
            println!("{:?} pts={} color={:?}", e.kind, e.line.len(), e.kind.color());
        }
        assert!(!events.is_empty(), "実際に関東広域で0件は考えにくい");
    }
}
