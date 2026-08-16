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
    // same_tukokisei_info_id。詳細(規制原因等)を取るfetch_detailのキー。
    // 既存ディスクキャッシュとの互換のためdefault(空文字=詳細取得不可)を許容する。
    #[serde(default)]
    pub detail_id: String,
}

// pcTukokiseiDetail_{id}.html(規制1件の詳細ページ)から取れる、地図の線だけでは
// わからない情報。「なぜ通れないか」に answer するのが目的なので cause が主役。
#[derive(Clone, Debug, PartialEq)]
pub struct ClosureDetail {
    pub route_name: String,
    pub direction: String,
    pub start_point: String,
    pub end_point: String,
    pub length: String,
    pub content: String,
    pub cause: String, // 規制原因(例: 工事/道路陥没)。これが「通行止めの理由」
    pub start_datetime: String,
    pub end_datetime: String,
    pub status: String,
    pub detour: String,
    pub note: String,
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
        let detail_id = item.get("same_tukokisei_info_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        out.push(ClosureEvent { line, kind: RegulationKind::from_code(code), detail_id });
    }
    out
}

const DETAIL_BASE: &str = "https://www.road-info-prvs.mlit.go.jp/roadinfo/pc/";

// pcTukokiseiDetail_{id}.html本文から「規制1件の詳細」を取る。discover_json_baseと違い
// パスが固定(タイムスタンプ付きbackupパスを経由しない)なので2段階フェッチは不要。
// idはClosureEvent::detail_id(same_tukokisei_info_id)。
pub fn fetch_detail(id: &str) -> Result<ClosureDetail, String> {
    let url = format!("{DETAIL_BASE}pcTukokiseiDetail_{id}.html");
    let html = ureq::get(&url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("通行規制の詳細: {e}"))?
        .into_string()
        .map_err(|e| format!("通行規制の詳細の読み取り: {e}"))?;
    parse_detail(&html).ok_or_else(|| "通行規制の詳細の解析に失敗".to_string())
}

// 詳細ページのtable(<td class="shosaiTitleCell">見出し</td><td class="shosaiValueCell">値</td>の
// 繰り返し)から(見出し, 値)の対を全て取り出す。値セルの中に<img>/<span>等のタグが
// 混ざる行(規制実施状況等)があるため、値側はタグを取り除いてから返す。
// ネットワークに触れない純関数。壊れたHTML・該当行なしは空Vecを返す(panicしない)。
fn extract_shosai_fields(html: &str) -> Vec<(String, String)> {
    const TITLE_MARK: &str = "shosaiTitleCell";
    const VALUE_MARK: &str = "shosaiValueCell";
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(ti) = rest.find(TITLE_MARK) {
        rest = &rest[ti + TITLE_MARK.len()..];
        // 見出しセルの開始タグの">"を探し、そこから"</td>"までがラベル本文(タグは含まない前提)。
        let Some(gt) = rest.find('>') else { break };
        let after_label_tag = &rest[gt + 1..];
        let Some(label_end) = after_label_tag.find("</td>") else { break };
        let label = after_label_tag[..label_end].trim().to_string();
        rest = &after_label_tag[label_end + "</td>".len()..];

        let Some(vi) = rest.find(VALUE_MARK) else { break };
        rest = &rest[vi + VALUE_MARK.len()..];
        let Some(gt2) = rest.find('>') else { break };
        let after_value_tag = &rest[gt2 + 1..];
        let Some(value_end) = after_value_tag.find("</td>") else { break };
        let raw_value = &after_value_tag[..value_end];
        // 値の中に混ざるタグ(<img>/<span>等)を取り除き、テキストだけにする。
        let value = strip_tags(raw_value).trim().to_string();
        rest = &after_value_tag[value_end + "</td>".len()..];

        if !label.is_empty() {
            out.push((label, value));
        }
    }
    out
}

// "<...>"を全て取り除く最小限のタグ除去(HTMLパーサではない。この詳細ページの
// 値セルにネストしたタグしか出てこない前提の簡易実装)。
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

// extract_shosai_fieldsの(見出し, 値)一覧からClosureDetailを組み立てる。既知の見出しが
// 1つも無ければNone(壊れたページ/フォーマット変更を検知するため、空のClosureDetailで
// 誤魔化さない)。個々の項目が無い場合はその項目だけ空文字にする(欠けても他は活かす)。
pub fn parse_detail(html: &str) -> Option<ClosureDetail> {
    let fields = extract_shosai_fields(html);
    if fields.is_empty() {
        return None;
    }
    let get = |label: &str| fields.iter().find(|(l, _)| l == label).map(|(_, v)| v.clone()).unwrap_or_default();
    Some(ClosureDetail {
        route_name: get("路線名"),
        direction: get("方向"),
        start_point: get("規制開始地点"),
        end_point: get("規制終了地点"),
        length: get("規制延長"),
        content: get("規制内容"),
        cause: get("規制原因"),
        start_datetime: get("規制開始日時"),
        end_datetime: get("規制終了予定日時"),
        status: get("規制実施状況"),
        detour: get("う回路"),
        note: get("備考"),
    })
}

// disaster::panel_content と同じ形((見出し, 本文行))で中央パネル表示用に整形する。
// 「なぜ通れないか」の核心である規制原因を1行目に出す。
pub fn detail_panel_content(d: &ClosureDetail) -> (String, Vec<String>) {
    let title = if d.route_name.is_empty() { "通行規制".to_string() } else { d.route_name.clone() };
    let mut lines = Vec::new();
    lines.push(format!("原因: {}", if d.cause.is_empty() { "不明" } else { &d.cause }));
    if !d.content.is_empty() || !d.direction.is_empty() {
        lines.push(format!("規制内容: {} ({})", d.content, d.direction));
    }
    if !d.start_point.is_empty() || !d.end_point.is_empty() {
        let extra = if d.length.is_empty() { String::new() } else { format!(" ({})", d.length) };
        lines.push(format!("区間: {} → {}{extra}", d.start_point, d.end_point));
    }
    if !d.start_datetime.is_empty() {
        lines.push(format!("開始: {}", d.start_datetime));
    }
    if !d.end_datetime.is_empty() {
        lines.push(format!("終了予定: {}", d.end_datetime));
    }
    if !d.status.is_empty() {
        lines.push(format!("状況: {}", d.status));
    }
    if !d.detour.is_empty() && d.detour != "-" {
        lines.push(format!("う回路: {}", d.detour));
    }
    if !d.note.is_empty() && d.note != "-" {
        lines.push(format!("備考: {}", d.note));
    }
    (title, lines)
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
        "same_tukokisei_info_id": "2431834e238b0959",
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
        assert_eq!(got[0].detail_id, "2431834e238b0959");
    }

    #[test]
    fn parse_closures_missing_detail_id_defaults_to_empty() {
        let body = r#"[{"kisei_naiyo_cd":"01","geo_json":"{\"geometry\":{\"type\":\"LineString\",\"coordinates\":[[139.0,35.0],[139.1,35.1]]}}"}]"#;
        let got = parse_closures(body);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].detail_id, "");
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
            detail_id: "2431834e238b1115".to_string(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"line":[[35.64085,139.733125],[35.64044,139.732903]],"kind":"Closed","detail_id":"2431834e238b1115"}"#
        );
        assert_eq!(serde_json::from_str::<ClosureEvent>(&json).unwrap(), ev);
    }

    #[test]
    fn closure_event_missing_detail_id_key_defaults_to_empty_for_backward_compat() {
        // detail_id導入前にディスクへ保存された古いキャッシュ(このキーを持たない)を読んでも
        // 壊れないこと(#[serde(default)]の効果)。
        let json = r#"{"line":[[35.0,139.0],[35.1,139.1]],"kind":"Closed"}"#;
        let ev: ClosureEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.detail_id, "");
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

    // 実際のpcTukokiseiDetail_*.htmlの抜粋(2026/08/17 実測、規制原因="道路陥没")。
    // 実物はtableに囲まれ他の行も挟まるが、抽出対象のtd構造だけ再現すれば足りる。
    const DETAIL_SAMPLE: &str = r#"<div id="popUpTitle_green"><div class="noButtonWidth">通行止（国道）</div></div>
        <table class="tukoKiseiShosai"><tbody>
        <tr><td class="shosaiTitleCell">路線名</td><td class="shosaiValueCell">国道418号</td></tr>
        <tr><td class="shosaiTitleCell">方向</td><td class="shosaiValueCell">上下</td></tr>
        <tr><td class="shosaiTitleCell">規制開始地点</td><td class="shosaiValueCell">八百津町南戸</td></tr>
        <tr><td class="shosaiTitleCell">規制終了地点</td><td class="shosaiValueCell">恵那市飯地町川平</td></tr>
        <tr><td class="shosaiTitleCell">規制延長</td><td class="shosaiValueCell">8.44km</td></tr>
        <tr><td class="shosaiTitleCell">規制内容</td><td class="shosaiValueCell">通行止</td></tr>
        <tr><td class="shosaiTitleCell">規制原因</td><td class="shosaiValueCell">道路陥没</td>
        <tr><td class="shosaiTitleCell">規制開始日時</td><td class="shosaiValueCell">2026年06月24日 12:00</td></tr>
        <tr><td class="shosaiTitleCell">規制終了予定日時</td><td class="shosaiValueCell">----年--月--日 --:--</td></tr>
        <tr><td class="shosaiTitleCell">規制実施状況</td><td class="shosaiValueCell"><img src="./../img/icon/pcsp/icon_stop.png" alt="" /><span id="kiseiJokyo">実施中</span></td></tr>
        <tr><td class="shosaiTitleCell">う回路</td><td class="shosaiValueCell">-</td></tr>
        <tr><td class="shosaiTitleCell">備考</td><td class="shosaiValueCell">-</td></tr>
        </tbody></table>"#;

    #[test]
    fn extract_shosai_fields_reads_label_value_pairs_and_strips_inner_tags() {
        let fields = extract_shosai_fields(DETAIL_SAMPLE);
        assert_eq!(fields.len(), 12);
        assert_eq!(fields[0], ("路線名".to_string(), "国道418号".to_string()));
        // 規制実施状況は値セルに<img>/<span>が混ざるが、テキストだけになる。
        assert_eq!(fields[9], ("規制実施状況".to_string(), "実施中".to_string()));
    }

    #[test]
    fn extract_shosai_fields_handles_garbage() {
        assert!(extract_shosai_fields("").is_empty());
        assert!(extract_shosai_fields("<html>no table here</html>").is_empty());
    }

    #[test]
    fn parse_detail_extracts_the_cause_which_answers_why_it_is_closed() {
        let d = parse_detail(DETAIL_SAMPLE).expect("実測相当のHTMLは解析できるはず");
        assert_eq!(d.route_name, "国道418号");
        assert_eq!(d.direction, "上下");
        assert_eq!(d.start_point, "八百津町南戸");
        assert_eq!(d.end_point, "恵那市飯地町川平");
        assert_eq!(d.length, "8.44km");
        assert_eq!(d.content, "通行止");
        assert_eq!(d.cause, "道路陥没", "「なぜ通れないか」の核心");
        assert_eq!(d.start_datetime, "2026年06月24日 12:00");
        assert_eq!(d.end_datetime, "----年--月--日 --:--");
        assert_eq!(d.status, "実施中");
        assert_eq!(d.detour, "-");
        assert_eq!(d.note, "-");
    }

    #[test]
    fn parse_detail_returns_none_when_no_known_fields_are_found() {
        assert!(parse_detail("not html at all").is_none());
        assert!(parse_detail("<html><body>empty</body></html>").is_none());
    }

    #[test]
    fn parse_detail_leaves_missing_individual_fields_empty_rather_than_failing() {
        let partial = r#"<td class="shosaiTitleCell">規制原因</td><td class="shosaiValueCell">工事</td>"#;
        let d = parse_detail(partial).unwrap();
        assert_eq!(d.cause, "工事");
        assert_eq!(d.route_name, "", "無い項目は空文字(取得失敗ではない)");
    }

    #[test]
    fn detail_panel_content_puts_the_cause_on_the_first_line() {
        let d = parse_detail(DETAIL_SAMPLE).unwrap();
        let (title, lines) = detail_panel_content(&d);
        assert_eq!(title, "国道418号");
        assert_eq!(lines[0], "原因: 道路陥没", "「なぜ通れないか」が本文の先頭に来ること");
        assert!(lines.iter().any(|l| l.contains("八百津町南戸") && l.contains("恵那市飯地町川平")));
        assert!(!lines.iter().any(|l| l.contains("う回路")), "う回路が\"-\"のときは行を出さない");
    }

    #[test]
    fn detail_panel_content_shows_unknown_cause_and_titles_as_regulation_when_route_name_is_missing() {
        let empty = ClosureDetail {
            route_name: String::new(), direction: String::new(), start_point: String::new(),
            end_point: String::new(), length: String::new(), content: String::new(),
            cause: String::new(), start_datetime: String::new(), end_datetime: String::new(),
            status: String::new(), detour: String::new(), note: String::new(),
        };
        let (title, lines) = detail_panel_content(&empty);
        assert_eq!(title, "通行規制");
        assert_eq!(lines[0], "原因: 不明");
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    #[test]
    #[ignore]
    fn live_fetch_real_detail_data() {
        let base = discover_json_base().expect("配信元パスの発見");
        let events = fetch_mesh(&base, 5339).expect("メッシュ取得");
        let with_id = events.iter().find(|e| !e.detail_id.is_empty()).expect("detail_id付きが1件は無いと詳細取得できない");
        let detail = fetch_detail(&with_id.detail_id).expect("詳細の取得");
        println!("{detail:?}");
        assert!(!detail.cause.is_empty() || !detail.content.is_empty(), "何かしらの本文は取れるはず");
    }
}
