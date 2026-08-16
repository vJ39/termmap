// 過去災害の発生履歴(防災科学技術研究所(NIED)「災害事例データベース」)。
// 豪雨・地震・台風・斜面災害等が「この土地で過去に何回記録されてきたか」を地図へ重ねる。
// 現在の危険度ではなく履歴なので、ツーリングの計画中に「ここは水害が繰り返されている」
// 「この一帯は斜面災害が多い」を読むためのレイヤになる。
// 調査は docs/disaster-history-data-investigation.md、設計は docs/disaster-history-overlay-design.md。
//
// 実測で確認済みの構造(2026/08/16):
//   - ArcGIS Server REST。認証・APIキー不要。1リクエストの上限(maxRecordCount)は2000件。
//   - 座標は市区町村の代表点で、1点に何十件も重なる(1次メッシュ5339で座標118種・最大166件)。
//     そのため生レコードではなく groupByFieldsForStatistics で「座標×種別ごとの件数」を取る。
//     1次メッシュ1枚が236行・25KB以下に収まり、ページングが要らない(§2.3)。
//   - グループ化キーに SAIGAI_YEAR を足すと2000行で打ち切られるので、年は where で絞る。
//     取得側と被覆側で同じしきい値を使う必要があり、キーに年を含める(plotlayer.rs)。
//   - 集計クエリは f=geojson を受け付けない(実測 "Requested format is not supported.")ので f=json。
//     座標は幾何ではなく fX/fY フィールドから取る(GeoJSON経由の再投影で乗る丸めを含まない生値)。
//   - SAIGAI_SYUBETSU_1 は数値ではなく文字列("3" 等)。値の対応表はレイヤ定義の codedValue から採取。
//   - 被害統計(SHIBOU_SU 等)と発生月日には、実数と符号付きコードと null が混ざる(DamageValue/format_date)。
// traffic.rs/regulation.rs/camera.rs と同じ方針で std + ureq + serde_json のみに依存し、
// crate:: を参照しない(ネットワークに触れない部分だけで単体テストが完結する)。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const ENDPOINT: &str = "https://agis.bosai.go.jp/webgis/rest/services/dil-db/saigai/MapServer/0/query";
const USER_AGENT: &str = "termmap/0.1 (personal experiment)";
const HTTP_TIMEOUT_SECS: u64 = 20;

/// 既定の年代しきい値。全期間だと西暦567年からの事例が入るが、古い事例は位置が現代の
/// 行政区分からの推定で被害統計も揃わない。近代的な統計が揃い始める1926年で切ることで、
/// マーカーの件数が「読める記録の積み重ね」を指すようにする(通信量のためではない)。
pub const DEFAULT_SINCE_YEAR: i32 = 1926;

/// 詳細取得(2段目)で地点を指す矩形の半辺(度)。約50m。
/// fX/fY の浮動小数一致ではなく矩形にするのは、fX の小数桁が6桁と12桁で混在しており
/// 文字列へ戻すときの桁で一致が外れる余地があるため(矩形なら桁に依存しない)。
const POINT_EPS_DEG: f64 = 0.0005;

/// 詳細表示で出す事例の既定件数。1地点最大166件を全部出しても読めないので新しい順に切る。
pub const EVENT_LIMIT: u32 = 20;

// 集計クエリが打ち切られた(exceededTransferLimit)ことが一度でもあったか。
// 打ち切られるとマーカーの件数が黙って過少になるため、詳細パネルの脚注で見えるようにする。
// ワーカースレッドが書き、UIスレッドが読むだけなので Relaxed で足りる。
static TRUNCATED: AtomicBool = AtomicBool::new(false);

/// 集計の打ち切りを一度でも観測したか(詳細パネルの脚注用)。
pub fn truncation_seen() -> bool {
    TRUNCATED.load(Ordering::Relaxed)
}

/// 災害種別(SAIGAI_SYUBETSU_1 の6分類)。詳細コード(SAIGAI_SYUBETSU_MORE_*、36種)は扱わない。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DisasterKind {
    Earthquake,
    Volcano,
    Storm,
    Slope,
    Snow,
    OtherWeather,
    Unknown,
}

impl DisasterKind {
    /// レイヤ定義の codedValue ドメインそのままの対応(推測ではない)。未知の値は Unknown へ寄せる。
    pub fn from_code(code: &str) -> Self {
        match code {
            "1" => DisasterKind::Earthquake,
            "2" => DisasterKind::Volcano,
            "3" => DisasterKind::Storm,
            "4" => DisasterKind::Slope,
            "5" => DisasterKind::Snow,
            "9" => DisasterKind::OtherWeather,
            _ => DisasterKind::Unknown,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            DisasterKind::Earthquake => "地震災害",
            DisasterKind::Volcano => "火山災害",
            DisasterKind::Storm => "風水害",
            DisasterKind::Slope => "斜面災害",
            DisasterKind::Snow => "雪氷災害",
            DisasterKind::OtherWeather => "その他気象災害",
            DisasterKind::Unknown => "災害",
        }
    }

    /// マーカーの色。既存レイヤ(交通量の緑/黄/赤・カメラの紫・規制の赤橙黄水色)となるべく離す。
    /// 実データの9割近くが風水害なので、既定では画面がほぼ青一色になる前提の配色にしてある。
    pub fn color(&self) -> [u8; 3] {
        match self {
            DisasterKind::Earthquake => [235, 80, 80],
            DisasterKind::Volcano => [255, 130, 40],
            DisasterKind::Storm => [70, 130, 245],
            DisasterKind::Slope => [150, 100, 60],
            DisasterKind::Snow => [180, 230, 245],
            DisasterKind::OtherWeather => [160, 160, 160],
            DisasterKind::Unknown => [200, 200, 200],
        }
    }

    // 件数が同じ種別が並んだときの優先順(dominant の決まり方を固定するためだけの順序)。
    // 応答の行順に依存させると、同じ地点でも取得のたびに色が入れ替わりうる。
    fn rank(&self) -> u8 {
        match self {
            DisasterKind::Earthquake => 0,
            DisasterKind::Volcano => 1,
            DisasterKind::Storm => 2,
            DisasterKind::Slope => 3,
            DisasterKind::Snow => 4,
            DisasterKind::OtherWeather => 5,
            DisasterKind::Unknown => 6,
        }
    }
}

/// 集計クエリ1行に対応する、ある地点のある種別の積み上げ。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KindCount {
    pub kind: DisasterKind,
    pub count: u32,
    pub year_min: i32,
    pub year_max: i32,
}

/// 地図に打つ単位。座標1つ=マーカー1つ(同じ座標に何十件も重なるため、事例1件=1マーカーにしない)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisasterSite {
    pub lat: f64,
    pub lon: f64,
    pub kinds: Vec<KindCount>,
}

impl DisasterSite {
    /// その地点の全種別合計。マーカーの大きさはこれで決める(§6.2)。
    pub fn total(&self) -> u32 {
        self.kinds.iter().map(|k| k.count).sum()
    }

    /// 最も件数の多い種別(マーカーの色)。同数のときは DisasterKind::rank の順で決める。
    pub fn dominant(&self) -> DisasterKind {
        let mut best: Option<&KindCount> = None;
        for k in &self.kinds {
            let take = match best {
                None => true,
                Some(b) => k.count > b.count || (k.count == b.count && k.kind.rank() < b.kind.rank()),
            };
            if take {
                best = Some(k);
            }
        }
        best.map_or(DisasterKind::Unknown, |k| k.kind)
    }

    /// 記録のある最も古い年(種別をまたいだ最小)。事例が無ければ None。
    pub fn year_min(&self) -> Option<i32> {
        self.kinds.iter().map(|k| k.year_min).filter(|y| *y != 0).min()
    }

    /// 記録のある最も新しい年(種別をまたいだ最大)。事例が無ければ None。
    pub fn year_max(&self) -> Option<i32> {
        self.kinds.iter().map(|k| k.year_max).filter(|y| *y != 0).max()
    }
}

/// マーカーの外周半径(件数3段階)。閾値は実測(1地点あたり中央値18件・最大166件)から、
/// 3段階が概ね均等に散る位置に置いた。
pub fn marker_radius(total: u32) -> i32 {
    if total >= 50 {
        4
    } else if total >= 10 {
        3
    } else {
        2
    }
}

/// 被害統計フィールド1つぶんの値。実数と符号付きコードと未記入を型で分ける。
/// **コード値を数値として合算しない**: -1(不明)を0として足すと被害を過小に、-8 を足すと
/// 符号が逆向きに効く。traffic.rs が欠測方向を0扱いせず加算から外しているのと同じ扱い。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DamageValue {
    NotRecorded,
    NoDamage,
    Count(u32),
    Unknown,
    Reported,
    Major,
    Catastrophic,
}

impl DamageValue {
    /// レイヤ定義の codedValue そのままの対応。未知の負値は「不明」へ寄せる
    /// (知らないコードを実数として扱うと、負の被害数という無い値が画面に出る)。
    pub fn from_raw(v: Option<i64>) -> Self {
        match v {
            None => DamageValue::NotRecorded,
            Some(0) => DamageValue::NoDamage,
            Some(-1) => DamageValue::Unknown,
            Some(-2) => DamageValue::Reported,
            Some(-7) => DamageValue::Major,
            Some(-8) => DamageValue::Catastrophic,
            Some(n) if n > 0 => DamageValue::Count(n.min(u32::MAX as i64) as u32),
            Some(_) => DamageValue::Unknown,
        }
    }

    /// 表示文。単位は項目ごとに違う(死者=名 / 全壊=棟)ので引数で受ける
    /// (設計の label(&self) から単位引数を足した。1つの enum を人にも建物にも使うため)。
    pub fn label(&self, unit: &str) -> String {
        match self {
            DamageValue::NotRecorded => "記載なし".to_string(),
            DamageValue::NoDamage => "なし".to_string(),
            DamageValue::Count(n) => format!("{n}{unit}"),
            DamageValue::Unknown => "不明".to_string(),
            DamageValue::Reported => "あり(数不明)".to_string(),
            DamageValue::Major => "大規模被害".to_string(),
            DamageValue::Catastrophic => "壊滅的被害".to_string(),
        }
    }

    /// 記録として何か言っているか。NotRecorded はサンプルの97%を占めるので画面に出さない。
    pub fn is_recorded(&self) -> bool {
        !matches!(self, DamageValue::NotRecorded)
    }
}

/// 詳細表示(2段目)で扱う事例1件。ディスクへは保存しないので serde の derive は付けない。
#[derive(Clone, Debug, PartialEq)]
pub struct DisasterEvent {
    pub jirei: String,
    pub name: String,
    pub year: i32,
    pub month: i32,
    pub day: i32,
    pub kind: DisasterKind,
    pub pref: String,
    pub city: String,
    pub deaths: DamageValue,
    pub missing: DamageValue,
    pub houses_lost: DamageValue,
    pub flooded: DamageValue,
    pub accuracy: String,
}

// SAIGAI_MONTH の負値は季節コード(レイヤ定義の codedValue そのまま)。
// 冬だけ -120/-10/-20 と連番が飛ぶので、順序を推測せず表引きにする。
fn season_label(code: i32) -> Option<&'static str> {
    Some(match code {
        -30 => "春の初め頃",
        -40 => "春の中頃",
        -50 => "春の終り頃",
        -60 => "夏の初め頃",
        -70 => "夏の中頃",
        -80 => "夏の終り頃",
        -90 => "秋の初め頃",
        -100 => "秋の中頃",
        -110 => "秋の終り頃",
        -120 => "冬の初め頃",
        -10 => "冬の中頃",
        -20 => "冬の終り頃",
        _ => return None,
    })
}

/// 発生年月日の整形。月・日は素直な整数ではない(負値=季節コード / 100,200,300=上旬,中旬,下旬)。
/// 未記入(元データの null)は 0 で渡す。
pub fn format_date(year: i32, month: i32, day: i32) -> String {
    if let Some(s) = season_label(month) {
        return format!("{year}年{s}");
    }
    if !(1..=12).contains(&month) {
        // 月が未記入(0)・未知のコードなら年だけ。日だけ分かっていても年月が無いと意味を成さない。
        return format!("{year}年");
    }
    let day_txt = match day {
        100 => "上旬".to_string(),
        200 => "中旬".to_string(),
        300 => "下旬".to_string(),
        d if (1..=31).contains(&d) => format!("{d}日"),
        _ => String::new(), // 未記入(0)・未知のコードは月までで止める
    };
    format!("{year}年{month}月{day_txt}")
}

/// 詳細パネルの1行。日付・名称・種別と、記録のある被害統計だけを並べる。
/// 桁揃え(全角混じりのパディング)はしない。パネル側が表示幅で切るため。
pub fn event_line(ev: &DisasterEvent) -> String {
    let mut s = format_date(ev.year, ev.month, ev.day);
    if !ev.name.is_empty() {
        s.push(' ');
        s.push_str(&ev.name);
    }
    s.push(' ');
    s.push_str(ev.kind.label());
    for (label, value, unit) in [
        ("死者", ev.deaths, "名"),
        ("不明", ev.missing, "名"),
        ("全壊", ev.houses_lost, "棟"),
        ("床上浸水", ev.flooded, "棟"),
    ] {
        if value.is_recorded() {
            s.push_str(&format!(" {label}{}", value.label(unit)));
        }
    }
    s
}

/// 詳細パネルの中身(見出し, 本文行)。件数と年幅は集計側(1段目)の `site` が持っている値を使う
/// (事例一覧は新しい順に limit 件へ切ってあるので、そこからは合計も最古の年も出せない)。
/// `since_year` は年幅が分からなかったときの見出しにだけ使う。
pub fn panel_content(events: &[DisasterEvent], site: &DisasterSite, since_year: i32) -> (String, Vec<String>) {
    let period = match (site.year_min(), site.year_max()) {
        (Some(a), Some(b)) if a < b => format!("{a}〜{b}年"),
        (Some(a), Some(_)) => format!("{a}年"),
        _ if since_year > 0 => format!("{since_year}年以降"),
        _ => "全期間".to_string(),
    };
    let place = events
        .first()
        .map(|e| format!("{} {}", e.pref, e.city).trim().to_string())
        .unwrap_or_default();
    let head = if place.is_empty() { "過去災害".to_string() } else { place };
    let title = format!("{head} ─ 記録 {}件({period})", site.total());
    let mut lines: Vec<String> = events.iter().map(event_line).collect();
    if lines.is_empty() {
        lines.push("(この地点の事例を取得できなかった)".to_string());
    }
    (title, lines)
}

// ---- 取得 ----

/// bbox内の地点集計(1段目)。ディスクキャッシュに載るのはこちら。
/// `since_year` が 0 以下なら全期間。
pub fn fetch_sites(
    lat_min: f64,
    lon_min: f64,
    lat_max: f64,
    lon_max: f64,
    since_year: i32,
) -> Result<Vec<DisasterSite>, String> {
    let url = sites_url(lat_min, lon_min, lat_max, lon_max, since_year);
    let body = http_get(&url, "過去災害")?;
    // 空 Vec と失敗を混同しない(混同するとオフラインでマーカーが消える)。
    if let Some(msg) = error_message(&body) {
        return Err(format!("過去災害: {msg}"));
    }
    if truncated(&body) {
        TRUNCATED.store(true, Ordering::Relaxed);
    }
    Ok(parse_sites(&body))
}

/// 1地点の事例一覧(2段目)。押したときだけ引き、保存はしない。
pub fn fetch_events(lat: f64, lon: f64, since_year: i32, limit: u32) -> Result<Vec<DisasterEvent>, String> {
    let url = events_url(lat, lon, since_year, limit);
    let body = http_get(&url, "災害事例")?;
    if let Some(msg) = error_message(&body) {
        return Err(format!("災害事例: {msg}"));
    }
    Ok(parse_events(&body))
}

fn http_get(url: &str, what: &str) -> Result<String, String> {
    ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("{what}: {e}"))?
        .into_string()
        .map_err(|e| format!("{what}の読み取り: {e}"))
}

fn where_clause(since_year: i32) -> String {
    if since_year > 0 {
        format!("SAIGAI_YEAR>={since_year}")
    } else {
        "1=1".to_string()
    }
}

// 集計クエリのURL(ネットワークに触れない純関数)。
fn sites_url(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64, since_year: i32) -> String {
    const STATS: &str = concat!(
        r#"[{"statisticType":"count","onStatisticField":"OBJECTID","outStatisticFieldName":"N"},"#,
        r#"{"statisticType":"min","onStatisticField":"SAIGAI_YEAR","outStatisticFieldName":"YMIN"},"#,
        r#"{"statisticType":"max","onStatisticField":"SAIGAI_YEAR","outStatisticFieldName":"YMAX"}]"#
    );
    let geom = format!("{lon_min},{lat_min},{lon_max},{lat_max}");
    format!(
        "{ENDPOINT}?where={}&geometry={}&geometryType=esriGeometryEnvelope&inSR=4326\
         &groupByFieldsForStatistics={}&outStatistics={}&returnGeometry=false&f=json",
        urlencode(&where_clause(since_year)),
        urlencode(&geom),
        urlencode("fX,fY,SAIGAI_SYUBETSU_1"),
        urlencode(STATS),
    )
}

// 事例一覧クエリのURL(ネットワークに触れない純関数)。
fn events_url(lat: f64, lon: f64, since_year: i32, limit: u32) -> String {
    const FIELDS: &str = "JIREI_BANGO,SAIGAI_MEISYO,SAIGAI_MEISYO_JMA,SAIGAI_YEAR,SAIGAI_MONTH,SAIGAI_DAY,\
                          SAIGAI_SYUBETSU_1,BASHO_KEN,BASHO_SHI,ACCURACY,SHIBOU_SU,YUKUEHUMEI_SU,ZENKAI,YUKAUESHINSUI";
    // 小数6桁(約0.1m)へ丸める。±0.0005度(約50m)の矩形に対して桁違いに細かいので範囲は変わらず、
    // 引き算の誤差(139.874328 が 139.87432800000002 になる類)がURLに出てこなくなる。
    let r6 = |v: f64| (v * 1e6).round() / 1e6;
    let geom = format!(
        "{},{},{},{}",
        r6(lon - POINT_EPS_DEG),
        r6(lat - POINT_EPS_DEG),
        r6(lon + POINT_EPS_DEG),
        r6(lat + POINT_EPS_DEG)
    );
    format!(
        "{ENDPOINT}?where={}&geometry={}&geometryType=esriGeometryEnvelope&inSR=4326\
         &outFields={}&returnGeometry=false&orderByFields={}&resultRecordCount={limit}&f=json",
        urlencode(&where_clause(since_year)),
        urlencode(&geom),
        urlencode(FIELDS),
        urlencode("SAIGAI_YEAR DESC"),
    )
}

// ---- パース(ネットワークに触れない純関数) ----

/// 応答が `{"error":{...}}` ならそのメッセージ。正常なら None。
pub fn error_message(body: &str) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let err = v.get("error")?;
    let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("エラー応答");
    match err.get("code").and_then(|c| c.as_i64()) {
        Some(code) => Some(format!("{msg}({code})")),
        None => Some(msg.to_string()),
    }
}

/// 応答が上限で打ち切られたか(集計行が maxRecordCount を超えた)。
pub fn truncated(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("exceededTransferLimit").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// 集計応答 → Vec<DisasterSite>。同じ座標の複数行(種別ごと)は1つの DisasterSite へ畳む。
/// 座標欠損・件数不明の行は黙って捨て、壊れた入力でも panic せず空 Vec を返す。
pub fn parse_sites(body: &str) -> Vec<DisasterSite> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return Vec::new() };
    let Some(features) = v.get("features").and_then(|f| f.as_array()) else { return Vec::new() };
    let mut out: Vec<DisasterSite> = Vec::new();
    // 座標のビット列 → out の添字。f64 の等値比較を避けつつ、応答の並び順を保つ。
    let mut index: HashMap<(u64, u64), usize> = HashMap::new();
    for f in features {
        let Some(a) = f.get("attributes") else { continue };
        let (Some(lon), Some(lat)) = (
            a.get("fX").and_then(|x| x.as_f64()),
            a.get("fY").and_then(|x| x.as_f64()),
        ) else {
            continue;
        };
        if !lon.is_finite() || !lat.is_finite() {
            continue;
        }
        let count = a.get("N").and_then(|x| x.as_i64()).unwrap_or(0);
        if count <= 0 {
            continue; // 件数の無い行は地図に出しても意味が無い
        }
        let kc = KindCount {
            kind: DisasterKind::from_code(&code_str(a.get("SAIGAI_SYUBETSU_1"))),
            count: count.min(u32::MAX as i64) as u32,
            year_min: as_i32(a.get("YMIN")),
            year_max: as_i32(a.get("YMAX")),
        };
        let key = (lat.to_bits(), lon.to_bits());
        match index.get(&key) {
            Some(&i) => out[i].kinds.push(kc),
            None => {
                index.insert(key, out.len());
                out.push(DisasterSite { lat, lon, kinds: vec![kc] });
            }
        }
    }
    out
}

/// 事例一覧応答 → Vec<DisasterEvent>。応答の並び(新しい順)をそのまま保つ。
/// 発生年が無い行は日付を組めないので捨てる。
pub fn parse_events(body: &str) -> Vec<DisasterEvent> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return Vec::new() };
    let Some(features) = v.get("features").and_then(|f| f.as_array()) else { return Vec::new() };
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(a) = f.get("attributes") else { continue };
        let Some(year) = a.get("SAIGAI_YEAR").and_then(|x| x.as_i64()) else { continue };
        out.push(DisasterEvent {
            jirei: text(a.get("JIREI_BANGO")),
            name: pick_name(a),
            year: year as i32,
            month: as_i32(a.get("SAIGAI_MONTH")),
            day: as_i32(a.get("SAIGAI_DAY")),
            kind: DisasterKind::from_code(&code_str(a.get("SAIGAI_SYUBETSU_1"))),
            pref: text(a.get("BASHO_KEN")),
            city: text(a.get("BASHO_SHI")),
            deaths: DamageValue::from_raw(a.get("SHIBOU_SU").and_then(|x| x.as_i64())),
            missing: DamageValue::from_raw(a.get("YUKUEHUMEI_SU").and_then(|x| x.as_i64())),
            houses_lost: DamageValue::from_raw(a.get("ZENKAI").and_then(|x| x.as_i64())),
            flooded: DamageValue::from_raw(a.get("YUKAUESHINSUI").and_then(|x| x.as_i64())),
            accuracy: text(a.get("ACCURACY")),
        });
    }
    out
}

// 名称は気象庁命名(SAIGAI_MEISYO_JMA)を優先し、無ければ SAIGAI_MEISYO、それも無ければ空。
// SAIGAI_MEISYO は "令和元年台風第15号|台風15号" のように別名を '|' で連ねた行が実在するので
// 先頭の1つだけを採る(設計時には出ていなかった実データの形)。
fn pick_name(a: &serde_json::Value) -> String {
    for key in ["SAIGAI_MEISYO_JMA", "SAIGAI_MEISYO"] {
        let s = text(a.get(key));
        let first = s.split('|').next().unwrap_or("").trim().to_string();
        if !first.is_empty() {
            return first;
        }
    }
    String::new()
}

// null や数値が混ざっても壊れないよう、文字列は必ず String へ落として扱う。
fn text(v: Option<&serde_json::Value>) -> String {
    v.and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
}

// 種別コードは実測では文字列("3")だが、数値で返ってきても同じ意味として扱えるようにする。
fn code_str(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

// 数値フィールド。null・非数値は 0(=未記入)にする。0 は月・日・年のいずれでも実在しない値なので
// 未記入の印として使える(被害統計は 0 が「被害なし」の意味を持つため、こちらは別扱いにしてある)。
fn as_i32(v: Option<&serde_json::Value>) -> i32 {
    v.and_then(|x| x.as_i64()).unwrap_or(0) as i32
}

// application/x-www-form-urlencoded 相当の最小限のパーセントエンコード(traffic.rs と同じ実装)。
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

#[cfg(test)]
mod tests {
    use super::*;

    // 実際の集計応答の抜粋(2026/08/16 実測、1次メッシュ5339・1926年以降)。
    // 1行目と4行目は同じ座標で種別だけが違う(=1つの DisasterSite へ畳まれる)。
    const SITES_SAMPLE: &str = r#"{"displayFieldName":"","fieldAliases":{"fX":"経度"},
      "fields":[{"name":"fX","type":"esriFieldTypeDouble"}],
      "features":[
        {"attributes":{"fX":139.87482800000001,"fY":35.955106000000001,"SAIGAI_SYUBETSU_1":"3","N":60,"YMIN":1926,"YMAX":2019}},
        {"attributes":{"fX":139.27499399999999,"fY":35.788170999999998,"SAIGAI_SYUBETSU_1":"1","N":33,"YMIN":1926,"YMAX":2011}},
        {"attributes":{"fX":139.14885200000001,"fY":35.726840000000003,"SAIGAI_SYUBETSU_1":"5","N":20,"YMIN":1927,"YMAX":1974}},
        {"attributes":{"fX":139.87482800000001,"fY":35.955106000000001,"SAIGAI_SYUBETSU_1":"9","N":10,"YMIN":1929,"YMAX":1996}}
      ]}"#;

    // 実際の事例一覧応答の抜粋(2026/08/16 実測、千葉県野田市の地点)。
    // 3件目は SAIGAI_MEISYO が null で SAIGAI_MEISYO_JMA だけがある実在の形。
    const EVENTS_SAMPLE: &str = r#"{"displayFieldName":"JIREI_BANGO","features":[
        {"attributes":{"JIREI_BANGO":"2019-09-xx_NJM068_Rxxxxx_JP12208-061366-19","SAIGAI_MEISYO":"令和元年台風第15号|台風15号","SAIGAI_MEISYO_JMA":"令和元年房総半島台風","SAIGAI_YEAR":2019,"SAIGAI_MONTH":9,"SAIGAI_DAY":null,"SAIGAI_SYUBETSU_1":"3","BASHO_KEN":"千葉県","BASHO_SHI":"野田市","ACCURACY":"A","SHIBOU_SU":null,"YUKUEHUMEI_SU":null,"ZENKAI":null,"YUKAUESHINSUI":null}},
        {"attributes":{"JIREI_BANGO":"2012-03-14_N1-198j_Exxxxx_JP12208-017036-13","SAIGAI_MEISYO":"千葉県東方沖地震","SAIGAI_MEISYO_JMA":"平成24年千葉県東方沖の地震","SAIGAI_YEAR":2012,"SAIGAI_MONTH":3,"SAIGAI_DAY":14,"SAIGAI_SYUBETSU_1":"1","BASHO_KEN":"千葉県","BASHO_SHI":"野田市","ACCURACY":"E","SHIBOU_SU":-2,"YUKUEHUMEI_SU":null,"ZENKAI":3,"YUKAUESHINSUI":null}},
        {"attributes":{"JIREI_BANGO":"2009-10-06_N3-127j_Rxxxxx_JP12208-016975-13","SAIGAI_MEISYO":null,"SAIGAI_MEISYO_JMA":"平成21年台風第18号による暴風・大雨","SAIGAI_YEAR":2009,"SAIGAI_MONTH":10,"SAIGAI_DAY":6,"SAIGAI_SYUBETSU_1":"3","BASHO_KEN":"千葉県","BASHO_SHI":"野田市","ACCURACY":"E","SHIBOU_SU":null,"YUKUEHUMEI_SU":null,"ZENKAI":null,"YUKAUESHINSUI":null}},
        {"attributes":{"JIREI_BANGO":"1926-xx-xx_Xxxxxx","SAIGAI_MEISYO":null,"SAIGAI_MEISYO_JMA":null,"SAIGAI_YEAR":1926,"SAIGAI_MONTH":-30,"SAIGAI_DAY":null,"SAIGAI_SYUBETSU_1":"9","BASHO_KEN":"千葉県","BASHO_SHI":"野田市","ACCURACY":"E","SHIBOU_SU":null,"YUKUEHUMEI_SU":null,"ZENKAI":null,"YUKAUESHINSUI":null}}
      ]}"#;

    // ---- 種別 ----

    #[test]
    fn disaster_kind_from_code_covers_the_domain_values() {
        assert_eq!(DisasterKind::from_code("1"), DisasterKind::Earthquake);
        assert_eq!(DisasterKind::from_code("2"), DisasterKind::Volcano);
        assert_eq!(DisasterKind::from_code("3"), DisasterKind::Storm);
        assert_eq!(DisasterKind::from_code("4"), DisasterKind::Slope);
        assert_eq!(DisasterKind::from_code("5"), DisasterKind::Snow);
        assert_eq!(DisasterKind::from_code("9"), DisasterKind::OtherWeather);
    }

    #[test]
    fn disaster_kind_from_unknown_or_empty_code_is_unknown() {
        for c in ["", "0", "6", "7", "8", "10", "abc", " 3"] {
            assert_eq!(DisasterKind::from_code(c), DisasterKind::Unknown, "code={c:?}");
        }
    }

    #[test]
    fn every_disaster_kind_has_a_label_and_a_distinct_colour() {
        let kinds = [
            DisasterKind::Earthquake,
            DisasterKind::Volcano,
            DisasterKind::Storm,
            DisasterKind::Slope,
            DisasterKind::Snow,
            DisasterKind::OtherWeather,
            DisasterKind::Unknown,
        ];
        let mut colors: Vec<[u8; 3]> = kinds.iter().map(|k| k.color()).collect();
        for k in kinds {
            assert!(!k.label().is_empty(), "{k:?}");
        }
        colors.sort();
        colors.dedup();
        assert_eq!(colors.len(), kinds.len(), "種別ごとに違う色であること");
    }

    #[test]
    fn every_disaster_kind_survives_a_json_round_trip() {
        for k in [
            DisasterKind::Earthquake,
            DisasterKind::Volcano,
            DisasterKind::Storm,
            DisasterKind::Slope,
            DisasterKind::Snow,
            DisasterKind::OtherWeather,
            DisasterKind::Unknown,
        ] {
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(serde_json::from_str::<DisasterKind>(&json).unwrap(), k, "{json}");
        }
    }

    // ---- 集計のパース ----

    #[test]
    fn parse_sites_folds_rows_that_share_a_coordinate_into_one_site() {
        let got = parse_sites(SITES_SAMPLE);
        assert_eq!(got.len(), 3, "4行・座標3種 → 3地点: {got:?}");
        let first = &got[0];
        // 応答の桁("35.955106000000001")は f64 では 35.955106 と同じ値になる。
        assert_eq!(first.lat, 35.955106);
        assert_eq!(first.lon, 139.874828);
        assert_eq!(first.kinds.len(), 2, "風水害60件 + その他気象10件");
        assert_eq!(first.kinds[0], KindCount { kind: DisasterKind::Storm, count: 60, year_min: 1926, year_max: 2019 });
        assert_eq!(first.kinds[1], KindCount { kind: DisasterKind::OtherWeather, count: 10, year_min: 1929, year_max: 1996 });
        assert_eq!(first.total(), 70);
        assert_eq!(first.dominant(), DisasterKind::Storm);
        assert_eq!(first.year_min(), Some(1926));
        assert_eq!(first.year_max(), Some(2019));
    }

    #[test]
    fn parse_sites_keeps_the_order_the_response_arrived_in() {
        let got = parse_sites(SITES_SAMPLE);
        assert_eq!(got[1].kinds[0].kind, DisasterKind::Earthquake);
        assert_eq!(got[2].kinds[0].kind, DisasterKind::Snow);
    }

    #[test]
    fn parse_sites_drops_rows_without_a_usable_coordinate_or_count() {
        let body = r#"{"features":[
            {"attributes":{"fY":35.9,"SAIGAI_SYUBETSU_1":"3","N":5}},
            {"attributes":{"fX":139.8,"SAIGAI_SYUBETSU_1":"3","N":5}},
            {"attributes":{"fX":"abc","fY":35.9,"SAIGAI_SYUBETSU_1":"3","N":5}},
            {"attributes":{"fX":139.8,"fY":35.9,"SAIGAI_SYUBETSU_1":"3","N":0}},
            {"attributes":{"fX":139.8,"fY":35.9,"SAIGAI_SYUBETSU_1":"3"}},
            {"attributes":{"fX":139.8,"fY":35.9,"SAIGAI_SYUBETSU_1":"3","N":1}}
        ]}"#;
        let got = parse_sites(body);
        assert_eq!(got.len(), 1, "最後の1行だけが残る: {got:?}");
        assert_eq!(got[0].total(), 1);
    }

    #[test]
    fn parse_sites_handles_garbage_without_panicking() {
        assert!(parse_sites("not json").is_empty());
        assert!(parse_sites("{}").is_empty());
        assert!(parse_sites(r#"{"features":[]}"#).is_empty());
        assert!(parse_sites(r#"{"features":{}}"#).is_empty());
        assert!(parse_sites(r#"{"features":[{}]}"#).is_empty());
        assert!(parse_sites(r#"{"error":{"code":400,"message":"Failed to execute query."}}"#).is_empty());
    }

    #[test]
    fn parse_sites_accepts_a_numeric_kind_code_as_well_as_a_string() {
        let body = r#"{"features":[{"attributes":{"fX":139.8,"fY":35.9,"SAIGAI_SYUBETSU_1":1,"N":2}}]}"#;
        assert_eq!(parse_sites(body)[0].kinds[0].kind, DisasterKind::Earthquake);
    }

    #[test]
    fn a_site_with_no_kinds_reports_zero_and_unknown() {
        let s = DisasterSite { lat: 35.0, lon: 139.0, kinds: Vec::new() };
        assert_eq!(s.total(), 0);
        assert_eq!(s.dominant(), DisasterKind::Unknown);
        assert_eq!(s.year_min(), None);
    }

    #[test]
    fn dominant_breaks_a_tie_by_a_fixed_kind_order_not_by_row_order() {
        let a = DisasterSite {
            lat: 35.0,
            lon: 139.0,
            kinds: vec![
                KindCount { kind: DisasterKind::Storm, count: 7, year_min: 1930, year_max: 2000 },
                KindCount { kind: DisasterKind::Earthquake, count: 7, year_min: 1930, year_max: 2000 },
            ],
        };
        // 行順を入れ替えても結果が変わらない(取得のたびに色が入れ替わらない)。
        let mut b = a.clone();
        b.kinds.reverse();
        assert_eq!(a.dominant(), DisasterKind::Earthquake);
        assert_eq!(b.dominant(), DisasterKind::Earthquake);
    }

    // ---- 事例一覧のパース ----

    #[test]
    fn parse_events_reads_the_fields_the_panel_shows() {
        let got = parse_events(EVENTS_SAMPLE);
        assert_eq!(got.len(), 4);
        let e = &got[1];
        assert_eq!(e.year, 2012);
        assert_eq!(e.month, 3);
        assert_eq!(e.day, 14);
        assert_eq!(e.kind, DisasterKind::Earthquake);
        assert_eq!(e.pref, "千葉県");
        assert_eq!(e.city, "野田市");
        assert_eq!(e.accuracy, "E");
        assert_eq!(e.deaths, DamageValue::Reported);
        assert_eq!(e.missing, DamageValue::NotRecorded);
        assert_eq!(e.houses_lost, DamageValue::Count(3));
        assert!(e.jirei.starts_with("2012-03-14"));
    }

    #[test]
    fn parse_events_prefers_the_jma_name_then_the_plain_one_then_nothing() {
        let got = parse_events(EVENTS_SAMPLE);
        assert_eq!(got[0].name, "令和元年房総半島台風", "JMA名がある行はそちら");
        assert_eq!(got[2].name, "平成21年台風第18号による暴風・大雨", "MEISYOがnullでもJMA名で出る");
        assert_eq!(got[3].name, "", "両方nullの行は空(日付と種別だけで出す)");
        // MEISYO しか無い行では、'|' で連ねた別名の先頭だけを採る。
        let body = r#"{"features":[{"attributes":{"SAIGAI_YEAR":2019,"SAIGAI_MEISYO":"令和元年台風第15号|台風15号","SAIGAI_MEISYO_JMA":null}}]}"#;
        assert_eq!(parse_events(body)[0].name, "令和元年台風第15号");
    }

    #[test]
    fn parse_events_drops_rows_without_a_year() {
        let body = r#"{"features":[
            {"attributes":{"SAIGAI_MEISYO":"名前だけの行","SAIGAI_YEAR":null}},
            {"attributes":{"SAIGAI_YEAR":1959,"SAIGAI_MEISYO":"伊勢湾台風"}}
        ]}"#;
        let got = parse_events(body);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].year, 1959);
    }

    #[test]
    fn parse_events_handles_garbage_without_panicking() {
        assert!(parse_events("not json").is_empty());
        assert!(parse_events("{}").is_empty());
        assert!(parse_events(r#"{"features":[]}"#).is_empty());
        assert!(parse_events(r#"{"error":{"code":400,"message":"Failed to execute query."}}"#).is_empty());
    }

    // ---- 被害統計のコード値 ----

    #[test]
    fn damage_value_separates_real_counts_from_codes_and_blanks() {
        assert_eq!(DamageValue::from_raw(None), DamageValue::NotRecorded);
        assert_eq!(DamageValue::from_raw(Some(0)), DamageValue::NoDamage);
        assert_eq!(DamageValue::from_raw(Some(1)), DamageValue::Count(1));
        assert_eq!(DamageValue::from_raw(Some(4127)), DamageValue::Count(4127));
        assert_eq!(DamageValue::from_raw(Some(-1)), DamageValue::Unknown);
        assert_eq!(DamageValue::from_raw(Some(-2)), DamageValue::Reported);
        assert_eq!(DamageValue::from_raw(Some(-7)), DamageValue::Major);
        assert_eq!(DamageValue::from_raw(Some(-8)), DamageValue::Catastrophic);
    }

    #[test]
    fn damage_value_pushes_unknown_negative_codes_to_unknown() {
        for n in [-3, -4, -5, -6, -9, -99] {
            assert_eq!(DamageValue::from_raw(Some(n)), DamageValue::Unknown, "n={n}");
        }
    }

    #[test]
    fn damage_value_labels_carry_the_unit_only_for_real_counts() {
        assert_eq!(DamageValue::from_raw(Some(3)).label("名"), "3名");
        assert_eq!(DamageValue::from_raw(Some(3)).label("棟"), "3棟");
        assert_eq!(DamageValue::from_raw(Some(-2)).label("名"), "あり(数不明)");
        assert_eq!(DamageValue::from_raw(Some(0)).label("棟"), "なし");
        assert_eq!(DamageValue::from_raw(None).label("名"), "記載なし");
        assert_eq!(DamageValue::from_raw(Some(-8)).label("棟"), "壊滅的被害");
    }

    #[test]
    fn only_a_blank_damage_value_is_left_out_of_the_panel() {
        assert!(!DamageValue::NotRecorded.is_recorded());
        for v in [
            DamageValue::NoDamage,
            DamageValue::Count(1),
            DamageValue::Unknown,
            DamageValue::Reported,
            DamageValue::Major,
            DamageValue::Catastrophic,
        ] {
            assert!(v.is_recorded(), "{v:?}");
        }
    }

    // ---- 日付 ----

    #[test]
    fn format_date_writes_a_plain_year_month_day() {
        assert_eq!(format_date(2019, 9, 6), "2019年9月6日");
        assert_eq!(format_date(2011, 3, 11), "2011年3月11日");
    }

    #[test]
    fn format_date_turns_the_ten_day_codes_into_words() {
        assert_eq!(format_date(2019, 9, 100), "2019年9月上旬");
        assert_eq!(format_date(2019, 9, 200), "2019年9月中旬");
        assert_eq!(format_date(2019, 9, 300), "2019年9月下旬");
    }

    #[test]
    fn format_date_turns_the_negative_month_codes_into_seasons() {
        // レイヤ定義の codedValue 全12種。冬は -120/-10/-20 と連番が飛ぶ。
        assert_eq!(format_date(1926, -30, 0), "1926年春の初め頃");
        assert_eq!(format_date(1926, -40, 0), "1926年春の中頃");
        assert_eq!(format_date(1926, -50, 0), "1926年春の終り頃");
        assert_eq!(format_date(1926, -60, 0), "1926年夏の初め頃");
        assert_eq!(format_date(1926, -70, 0), "1926年夏の中頃");
        assert_eq!(format_date(1926, -80, 0), "1926年夏の終り頃");
        assert_eq!(format_date(1926, -90, 0), "1926年秋の初め頃");
        assert_eq!(format_date(1926, -100, 0), "1926年秋の中頃");
        assert_eq!(format_date(1926, -110, 0), "1926年秋の終り頃");
        assert_eq!(format_date(1926, -120, 0), "1926年冬の初め頃");
        assert_eq!(format_date(1926, -10, 0), "1926年冬の中頃");
        assert_eq!(format_date(1926, -20, 0), "1926年冬の終り頃");
    }

    #[test]
    fn format_date_falls_back_as_far_as_the_data_allows() {
        assert_eq!(format_date(1926, 0, 0), "1926年", "月日とも未記入");
        assert_eq!(format_date(2019, 9, 0), "2019年9月", "日だけ未記入");
        assert_eq!(format_date(1926, 0, 14), "1926年", "月が無ければ日は出さない");
        assert_eq!(format_date(2019, 13, 1), "2019年", "未知の月コード");
        assert_eq!(format_date(2019, -35, 0), "2019年", "未知の季節コード");
        assert_eq!(format_date(2019, 9, 400), "2019年9月", "未知の日コード");
        assert_eq!(format_date(684, 11, 26), "684年11月26日", "西暦3桁でも桁を足さない");
    }

    // ---- 表示の組み立て ----

    #[test]
    fn marker_radius_grows_in_three_steps() {
        assert_eq!(marker_radius(0), 2);
        assert_eq!(marker_radius(1), 2);
        assert_eq!(marker_radius(9), 2);
        assert_eq!(marker_radius(10), 3);
        assert_eq!(marker_radius(49), 3);
        assert_eq!(marker_radius(50), 4);
        assert_eq!(marker_radius(166), 4);
    }

    #[test]
    fn event_line_shows_date_name_kind_and_only_the_recorded_damage() {
        let evs = parse_events(EVENTS_SAMPLE);
        assert_eq!(evs[0].to_line(), "2019年9月 令和元年房総半島台風 風水害");
        assert_eq!(
            evs[1].to_line(),
            "2012年3月14日 平成24年千葉県東方沖の地震 地震災害 死者あり(数不明) 全壊3棟",
            "記載なしの項目(不明者・床上浸水)は出さない"
        );
        assert_eq!(evs[3].to_line(), "1926年春の初め頃 その他気象災害", "名称が無い行は日付と種別だけ");
    }

    // 集計側の1地点(風水害60件 1926-2019 / その他気象10件 1929-1996 = 70件・1926〜2019年)。
    fn sample_site() -> DisasterSite {
        parse_sites(SITES_SAMPLE).remove(0)
    }

    #[test]
    fn panel_content_heads_with_the_place_the_total_and_the_year_span() {
        let evs = parse_events(EVENTS_SAMPLE);
        // 件数も年幅も集計側の値を使う(事例一覧は新しい順に切ってあるので合計にならない)。
        let (title, lines) = panel_content(&evs, &sample_site(), 1926);
        assert_eq!(title, "千葉県 野田市 ─ 記録 70件(1926〜2019年)");
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("2019年9月"));
    }

    #[test]
    fn panel_content_falls_back_to_the_threshold_when_the_span_is_unknown() {
        let evs = parse_events(EVENTS_SAMPLE);
        let blank = DisasterSite { lat: 35.0, lon: 139.0, kinds: Vec::new() };
        let (title, _) = panel_content(&evs, &blank, 1926);
        assert!(title.contains("1926年以降"), "{title}");
        let (title_all, _) = panel_content(&evs, &blank, 0);
        assert!(title_all.contains("全期間"), "{title_all}");
    }

    #[test]
    fn panel_content_collapses_a_single_year_span() {
        let site = DisasterSite {
            lat: 35.0,
            lon: 139.0,
            kinds: vec![KindCount { kind: DisasterKind::Snow, count: 2, year_min: 1963, year_max: 1963 }],
        };
        let (title, _) = panel_content(&[], &site, 1926);
        assert_eq!(title, "過去災害 ─ 記録 2件(1963年)");
    }

    #[test]
    fn panel_content_stays_readable_when_nothing_came_back() {
        let (title, lines) = panel_content(&[], &sample_site(), 1926);
        assert_eq!(title, "過去災害 ─ 記録 70件(1926〜2019年)", "場所は事例側にしか無いので伏せる");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("取得できなかった"));
    }

    // ---- 応答の状態判定 ----

    #[test]
    fn error_message_picks_up_the_arcgis_error_envelope() {
        // 実測(存在しないフィールドを where に入れたときの応答)。
        let body = r#"{"error":{"code":400,"message":"Failed to execute query.","details":[]}}"#;
        assert_eq!(error_message(body).as_deref(), Some("Failed to execute query.(400)"));
        assert_eq!(error_message(r#"{"error":{}}"#).as_deref(), Some("エラー応答"));
    }

    #[test]
    fn error_message_is_none_for_a_normal_response() {
        assert!(error_message(SITES_SAMPLE).is_none());
        assert!(error_message(r#"{"features":[]}"#).is_none());
        assert!(error_message("not json").is_none(), "壊れた本文はエラー応答とは別扱い");
    }

    #[test]
    fn truncated_reads_the_transfer_limit_flag() {
        assert!(truncated(r#"{"exceededTransferLimit":true,"features":[]}"#));
        assert!(!truncated(r#"{"exceededTransferLimit":false,"features":[]}"#));
        assert!(!truncated(SITES_SAMPLE), "実測の集計応答にはこのキー自体が無い");
        assert!(!truncated("not json"));
    }

    // ---- URL 組み立て ----

    #[test]
    fn sites_url_carries_the_bbox_the_group_by_and_the_year_threshold() {
        let u = sites_url(35.33, 139.0, 36.0, 140.0, 1926);
        assert!(u.starts_with(ENDPOINT), "{u}");
        assert!(u.contains("where=SAIGAI_YEAR%3E%3D1926"), "{u}");
        assert!(u.contains("geometry=139%2C35.33%2C140%2C36"), "経度,緯度の順: {u}");
        assert!(u.contains("groupByFieldsForStatistics=fX%2CfY%2CSAIGAI_SYUBETSU_1"), "{u}");
        assert!(u.contains("outStatistics=%5B%7B%22statisticType%22%3A%22count%22"), "{u}");
        assert!(u.contains("returnGeometry=false"), "{u}");
        assert!(u.ends_with("f=json"), "集計は f=geojson を受け付けない: {u}");
    }

    #[test]
    fn sites_url_asks_for_every_year_when_the_threshold_is_off() {
        for since in [0, -1] {
            let u = sites_url(35.0, 139.0, 36.0, 140.0, since);
            assert!(u.contains("where=1%3D1"), "since={since}: {u}");
        }
    }

    #[test]
    fn events_url_boxes_a_tiny_rectangle_around_the_point() {
        let u = events_url(35.955106, 139.874828, 1926, 20);
        // ±0.0005度(約50m)。浮動小数の一致ではなく矩形にしてある。
        assert!(u.contains("geometry=139.874328%2C35.954606%2C139.875328%2C35.955606"), "{u}");
        assert!(u.contains("resultRecordCount=20"), "{u}");
        assert!(u.contains("orderByFields=SAIGAI_YEAR%20DESC"), "新しい順: {u}");
        assert!(u.contains("SHIBOU_SU"), "被害統計を要求している: {u}");
    }

    #[test]
    fn urlencode_escapes_non_ascii_and_symbols() {
        assert_eq!(urlencode("abc123-_.~"), "abc123-_.~");
        assert_eq!(urlencode("SAIGAI_YEAR>=1926"), "SAIGAI_YEAR%3E%3D1926");
        assert_eq!(urlencode("災害"), "%E7%81%BD%E5%AE%B3");
    }

    // ---- ディスクキャッシュへ保存する形 ----

    #[test]
    fn disaster_sites_round_trip_through_json() {
        let s = DisasterSite {
            lat: 35.955106,
            lon: 139.874828,
            kinds: vec![KindCount { kind: DisasterKind::Storm, count: 60, year_min: 1926, year_max: 2019 }],
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(
            json,
            r#"{"lat":35.955106,"lon":139.874828,"kinds":[{"kind":"Storm","count":60,"year_min":1926,"year_max":2019}]}"#
        );
        assert_eq!(serde_json::from_str::<DisasterSite>(&json).unwrap(), s);
    }

    // テストの読みやすさのための小道具(本体は event_line)。
    impl DisasterEvent {
        fn to_line(&self) -> String {
            event_line(self)
        }
    }

    // 実ネットワークを叩く手動確認用(CIでは走らない)。`cargo test --release -- --ignored`で実行。
    #[test]
    #[ignore]
    fn live_fetch_real_disaster_data() {
        // 1次メッシュ5339(東京〜千葉西部)。
        let sites = fetch_sites(35.333334, 139.000001, 35.999999, 139.999999, DEFAULT_SINCE_YEAR)
            .expect("live fetch should succeed");
        println!("sites: {}", sites.len());
        for s in sites.iter().take(5) {
            println!(
                "{:.6},{:.6} total={} dominant={:?} r={}",
                s.lat,
                s.lon,
                s.total(),
                s.dominant(),
                marker_radius(s.total())
            );
        }
        assert!(!sites.is_empty(), "実際に関東で0地点は考えにくい");

        let s = &sites[0];
        let events = fetch_events(s.lat, s.lon, DEFAULT_SINCE_YEAR, EVENT_LIMIT).expect("live events");
        let (title, lines) = panel_content(&events, s, DEFAULT_SINCE_YEAR);
        println!("{title}");
        for l in lines.iter().take(5) {
            println!("  {l}");
        }
        assert!(!events.is_empty(), "集計に出た地点なら事例も引けるはず");
    }
}
