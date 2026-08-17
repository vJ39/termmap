// Focus::Settings(設定画面)のうち「3択以上」の項目を、アコーディオン式(選択中の行の直下に
// 候補をインデント展開し、他行を押し下げる)で直接選べるようにするための実装。
// パネル描画自体はui.rs側の左袖一覧描画に統合されている(ここでは選択肢テーブル・現在値の
// 算出・確定処理のみを持つ)。
//
// Focus enum 自体・対話ループ本体は ui.rs の interactive() 内ローカル状態(cx/cy/wps/cache 等)に
// 強く依存しているためここには移せない。ここに切り出したのは、その状態を必要としない純粋な部分のみ。

use crate::config::Config;
use crate::render::image_capable;
use crate::Args;

// 色ピッカー(ColorPick)と同じ並びの色名。中心十字の色選択・設定画面の表示に使う。
pub(crate) const PALETTE_NAMES: [&str; 10] = ["赤", "橙", "金", "黄緑", "水色", "紫", "桃", "緑青", "茶", "灰"];

// Focus::Settings の何行目(idx)が一覧選択(SettingsPick)の対象かのテーブル。
// values = cfg/opts に書き込む内部値、labels = 一覧に出す表示名(values と同じ並び)。
pub(crate) struct SettingChoice {
    pub idx: usize,
    pub values: &'static [&'static str],
    pub labels: &'static [&'static str],
}

// idx は Focus::Settings 側の項目行番号と対応(4=地図種別/5=既定ルート/9=提案AIモデル/12=画像解像度/18=QR表示方式/20=雨雲の濃さ/31=人口の濃さ)。
// 中心十字の色(idx=16)は cfg.cross_color_idx が String でなく u8 なので、この表とは別枠(is_pickable等で16を特別扱い)。
// 人口の年次(idx=32)も cfg.population_year が u16 なので同じく別枠。
// 項目を増やすときは必ず末尾に足す(既存 idx を動かすと ui.rs の set_sel == 6 / 17 等の生の数値比較と食い違う)。
pub(crate) const CHOICES: &[SettingChoice] = &[
    SettingChoice { idx: 4, values: &["osm", "voyager", "dark", "light", "topo"], labels: &["osm", "voyager", "dark", "light", "topo"] },
    SettingChoice { idx: 5, values: &["car-fast", "moped", "shortest"], labels: &["高速", "下道", "最短"] },
    SettingChoice { idx: 9, values: &["claude-sonnet-5", "claude-haiku-4-5", "claude-opus-4-8"], labels: &["sonnet", "haiku", "opus"] },
    SettingChoice { idx: 12, values: &["high", "mid", "low"], labels: &["高", "中", "低"] },
    SettingChoice { idx: 18, values: &["dense", "image"], labels: &["標準", "画像(小型)"] },
    SettingChoice { idx: 20, values: &["light", "mid", "strong"], labels: &["薄い", "標準", "濃い"] },
    SettingChoice { idx: 31, values: &["light", "mid", "strong"], labels: &["薄い", "標準", "濃い"] },
];

// 設定画面の項目行数(アコーディオン未展開時)。ui.rs のカーソル下移動の上限がこれを参照する。
// settings_rows() が返す行数と必ず一致すること(下の回帰テスト settings_row_count_matches_rows で固定)。
pub(crate) const SETTINGS_ROW_COUNT: usize = 34;

fn choice_for(idx: usize) -> Option<&'static SettingChoice> { CHOICES.iter().find(|c| c.idx == idx) }

// idx が SettingsPick(一覧選択)の対象か。中心十字の色(16)・読み上げの声(27)も対象に含む。
pub(crate) fn is_pickable(idx: usize) -> bool { idx == 16 || idx == 27 || idx == 32 || choice_for(idx).is_some() }

// 人口メッシュの年次(idx=32)の候補。2020年は令和2年国勢調査に基づく実績で、それ以降は推計値。
// 見分けが付かないと誤読されるので、ラベルで実績/推計を明示する。
pub(crate) fn population_year_labels() -> Vec<String> {
    crate::config::POPULATION_YEARS
        .iter()
        .map(|y| if *y == 2020 { format!("{y}年(実績)") } else { format!("{y}年(推計)") })
        .collect()
}

// 読み上げの声(idx=27)の候補一覧。他の項目と違い実行環境と現在値の両方に依存するため
// CHOICESの静的テーブルには載せず、ここで組み立てる。戻り値は(保存値, 表示名)。
// 先頭は常に"システム既定"(空文字)。cfg.voice_nameが列挙結果に無く、かつ列挙結果が
// 空でない場合だけ末尾に"<表示名> (未検出)"を足す(音声をアンインストール/config手書き後に
// 現在値が一覧から消えて黙って別の声に置き換わるのを防ぐ)。
pub(crate) fn voice_choices(cfg: &Config) -> Vec<(String, String)> {
    let mut out = vec![("".to_string(), "システム既定".to_string())];
    let installed = crate::voice::japanese_voices();
    for name in installed {
        out.push((name.clone(), crate::voice::display_voice_name(name).to_string()));
    }
    if !cfg.voice_name.is_empty() && !installed.iter().any(|n| n == &cfg.voice_name) && !installed.is_empty() {
        out.push((cfg.voice_name.clone(), format!("{} (未検出)", crate::voice::display_voice_name(&cfg.voice_name))));
    }
    out
}

// 一覧に出す表示ラベル(現在値のハイライト位置は pick_current で別途求める)。
// idx=27(読み上げの声)は実行時に決まるため所有文字列を返す(他は静的テーブルからの&'static str)。
pub(crate) fn pick_labels(idx: usize, cfg: &Config) -> Vec<String> {
    if idx == 16 {
        PALETTE_NAMES.iter().map(|s| s.to_string()).collect()
    } else if idx == 27 {
        voice_choices(cfg).into_iter().map(|(_, label)| label).collect()
    } else if idx == 32 {
        population_year_labels()
    } else {
        choice_for(idx).map(|c| c.labels.iter().map(|s| s.to_string()).collect()).unwrap_or_default()
    }
}

// 現在の設定値が一覧の何番目かを返す(未知の値は0扱い)。
// style(地図種別)は cfg.style でなく実際に描画に使っている opts.style を渡す(呼び出し側で同期済み)。
pub(crate) fn pick_current(idx: usize, cfg: &Config, style: &str) -> usize {
    match idx {
        4 => choice_for(4).and_then(|c| c.values.iter().position(|v| *v == style)).unwrap_or(0),
        5 => choice_for(5).and_then(|c| c.values.iter().position(|v| *v == cfg.route_profile)).unwrap_or(0),
        9 => choice_for(9).and_then(|c| c.values.iter().position(|v| *v == cfg.llm_model)).unwrap_or(0),
        12 => choice_for(12).and_then(|c| c.values.iter().position(|v| *v == cfg.image_res)).unwrap_or(0),
        16 => cfg.cross_color_idx as usize % PALETTE_NAMES.len(),
        18 => choice_for(18).and_then(|c| c.values.iter().position(|v| *v == cfg.qr_style)).unwrap_or(0),
        20 => choice_for(20).and_then(|c| c.values.iter().position(|v| *v == cfg.radar_opacity)).unwrap_or(0),
        27 => voice_choices(cfg).iter().position(|(v, _)| *v == cfg.voice_name).unwrap_or(0),
        31 => choice_for(31).and_then(|c| c.values.iter().position(|v| *v == cfg.population_opacity)).unwrap_or(0),
        32 => crate::config::POPULATION_YEARS.iter().position(|y| *y == cfg.population_year).unwrap_or(0),
        _ => 0,
    }
}

// SettingsPick で Enter を押したときの副作用のうち、呼び出し側(ui.rs)の状態(タイルキャッシュ/画像再emit)に
// 関わる分だけをフラグで返す。実際のキャッシュクリア等はここでは行わない(ui.rsの責務)。
pub(crate) struct ApplyEffect {
    pub cache_clear: bool,   // 地図種別変更: タイルキャッシュを作り直す必要がある
    pub force_reemit: bool,  // 画像解像度/中心十字の色変更: 実画像を強制的に再描画する必要がある
}

// 選択(sel番目)を確定して cfg (地図種別だけは opts.style)へ反映する。
pub(crate) fn apply_pick(idx: usize, sel: usize, cfg: &mut Config, style: &mut String) -> ApplyEffect {
    let mut eff = ApplyEffect { cache_clear: false, force_reemit: false };
    match idx {
        4 => if let Some(v) = choice_for(4).and_then(|c| c.values.get(sel)) { *style = v.to_string(); eff.cache_clear = true; }
        5 => if let Some(v) = choice_for(5).and_then(|c| c.values.get(sel)) { cfg.route_profile = v.to_string(); }
        9 => if let Some(v) = choice_for(9).and_then(|c| c.values.get(sel)) { cfg.llm_model = v.to_string(); }
        12 => if let Some(v) = choice_for(12).and_then(|c| c.values.get(sel)) { cfg.image_res = v.to_string(); eff.force_reemit = true; }
        16 => { cfg.cross_color_idx = (sel % PALETTE_NAMES.len()) as u8; eff.force_reemit = true; }
        18 => if let Some(v) = choice_for(18).and_then(|c| c.values.get(sel)) { cfg.qr_style = v.to_string(); }
        // 雨雲の濃さは今表示している地図の見た目が変わるので、確定した時点で描き直す。
        20 => if let Some(v) = choice_for(20).and_then(|c| c.values.get(sel)) { cfg.radar_opacity = v.to_string(); eff.force_reemit = true; }
        27 => if let Some((v, _)) = voice_choices(cfg).get(sel) { cfg.voice_name = v.clone(); }
        // 人口の濃さ・年次は今表示している地図の見た目が変わるので、確定した時点で描き直す。
        31 => if let Some(v) = choice_for(31).and_then(|c| c.values.get(sel)) { cfg.population_opacity = v.to_string(); eff.force_reemit = true; }
        32 => if let Some(y) = crate::config::POPULATION_YEARS.get(sel) { cfg.population_year = *y; eff.force_reemit = true; }
        _ => {}
    }
    eff
}

// Focus::Settings のステータス行(画面下部)に出す、選択中の行(set_sel)ごとの説明文。
// set_sel だけを引数に取る純関数(cfg/opts等の外部状態には触れない)。
pub(crate) fn setting_description(idx: usize) -> &'static str {
    match idx {
        0 => "braille: 点字ドットで高精細描画(色は淡め)。OFFはハーフブロック",
        1 => "classify: 地物を色分け(水域/緑地/道路/建物)。地形が見やすい",
        2 => "edge: 輪郭抽出表示(線画風)",
        3 => "mono: 単色描画(色を使わない)",
        4 => "style: タイル種別。Enterで一覧を開いて選択(osm=標準/voyager/dark=暗/light=淡)",
        5 => "既定mode: 起動時のルート種別。Enterで一覧を開いて選択(car-fast=高速優先 / moped=下道(高速回避) / shortest=最短距離)",
        6 => "道路の点間隔: rの道路名ルートで、その道を何mおきの点でなぞるか(小=忠実で点多/大=粗い)。Enterで数値入力/←→で微調整",
        7 => "spot既定: 起動時にお気に入りスポットを表示するか",
        8 => "おすすめ: claude -p でツーリングスポットを提案する機能のON/OFF(未実装)",
        9 => "LLM: おすすめに使うモデル。Enterで一覧を開いて選択(claude-sonnet-5/haiku/opus)",
        10 => "実写: iで中心地点のStreet Viewを開く機能のON/OFF(要Google APIキー)",
        11 => if image_capable() { "画像表示: 地図と実写をiTerm2インライン画像で実画像表示(AAでなく実画像)。Iキーでも切替" } else { "画像表示: この端末は画像非対応(iTerm2/WezTermで有効)" },
        12 => "画像解像度: 実画像モードの精細さ。Enterで一覧を開いて選択(高=scale4/中=scale2/低=scale1)",
        13 => "移動中の低解像度化: ONなら地図移動中(動いた直後〜静止350ms)は自動で低解像度にして速く描く。OFFなら常に設定解像度",
        14 => "サウンド: 操作音のON/OFF(macOSのafplayで再生)",
        15 => "オンボーディング: 毎回表示/非表示を切替(dキーでも次回から非表示にできる)",
        16 => "中心十字の色: 地図中心のクロスヘアの色。Enterで一覧を開いて選択(spots.rsの配色から選択)",
        18 => "QR表示方式: スマホ共有QRの表示方法。Enterで一覧を開いて選択(標準=文字描画・全端末対応 / 画像(小型)=iTerm2等のインライン画像でモジュール数に関係なく小さく表示。画像非対応端末では自動的に標準へフォールバック)",
        19 => "雨雲レーダー: 気象庁ナウキャストの降水を地図に重ねる。ここでのON/OFFは起動時の既定にもなる(Cキーでも切替・< > で表示時刻を過去〜未来(直近60分は5分刻み、それより先は降水短時間予報で最大+15時間まで1時間刻み)に移動)",
        20 => "雨雲の濃さ: 重ねる強さ。Enterで一覧を開いて選択(薄い=地図優先 / 標準 / 濃い=雨優先)",
        21 => "ルート音声案内: 曲がり角の300m手前/直前でmacOSの読み上げ(sayコマンド)またはブラウザの読み上げで案内する。ONにした人だけがBRouterへ追加問い合わせする",
        22 => "道路交通量: 国道の実測交通量(JARTICオープンデータ)を混雑度の目安として地図に重ねる。事故情報・渋滞度そのものではない。ONにした人だけが外部サービスへ問い合わせる",
        23 => "音声案内をこの端末でも再生: OFFにするとmacOSのsayコマンドでは鳴らさず、ブラウザ側(web版)の読み上げだけになる。web版で見ている時に手元のMac本体が同時に喋るのを避けたい場合はOFF",
        24 => "道路ライブカメラ: 国交省の道路カメラを地図に重ねる(Nキーで中心近くのカメラの写真を表示)。ONにした人だけが外部サービスへ問い合わせる",
        25 => "通行規制: 通行止め・車線規制等の区間(国交省road-info-prvs)を地図に線で重ねる。事故・工事・冬期閉鎖等の原因は区別しない(Tキーで規制原因等の詳細)。ONにすると実施中の通行止めをルート計算でも回避するようになる。ONにした人だけが外部サービスへ問い合わせる",
        26 => "過去災害: 豪雨・地震・台風等が過去に記録された地点(防災科学技術研究所 災害事例データベース・1926年以降)を地図に重ねる。件数で丸が大きく・種別で色が変わる(Bキーでその地点の事例一覧)。今の危険度ではなく履歴。ONにした人だけが外部サービスへ問い合わせる",
        27 => if cfg!(target_os = "macos") {
            "読み上げの声: ルート音声案内をこの端末(macOSのsay)で読み上げるときの声。Enterで一覧を開いて選択(Spaceで試聴)。インストール済みの日本語音声だけが並ぶ。web版(ブラウザ)の声はブラウザ側が自動で選ぶのでここでは変わらない"
        } else {
            "読み上げの声: この端末(macOS以外)では読み上げ自体が動かないため効果は無い"
        },
        29 => "過去災害の塗り: 過去災害を市区町村の境界ごとに塗り分けて出す(ハザードマップのような面表示)。色=最も多い災害種別・濃さ=記録の件数。OFFにすると従来の代表点マーカーだけになる。「過去災害」自体がOFFのときは何も出ない",
        30 => "人口メッシュ: 500mメッシュごとの推計人口(国土数値情報)を人口密度の濃さで地図に重ねる。補給・宿・明かりが期待できる帯と、人がいない帯が読める(Uキーでも切替)。1都道府県が最大31MBあり、取得は都道府県まるごと・数十秒かかる。一度取れば1年は取り直さない。ONにした人だけが外部サービスへ問い合わせる",
        31 => "人口の濃さ: 重ねる強さ。Enterで一覧を開いて選択(薄い=地図優先 / 標準 / 濃い=人口優先)。人口の少ない土地は元から薄く塗るので、濃くしても地図は残る",
        32 => "人口の年次: 表示する年。Enterで一覧を開いて選択。2020年は国勢調査に基づく実績、2025年以降は推計値。年を変えても取り直しは起きない(全年次をまとめて保存している)",
        28 => "渋滞状況の色分け: ルート確定後、Google Directionsで区間ごとの渋滞状況を追加確認し、混雑している区間だけルート線を黄(やや混雑)/赤(混雑)で上塗りする(順調な区間は基調色の青のまま)。道路網全体ではなく表示中のルートのみ。要Google APIキー。区間数に応じて1回のAdvanced課金対象リクエストを送る(無料枠超過分は1000件$8、個人利用なら通常は無料枠内)",
        33 => "ルート沿い気象警報: ルート確定後、その先が通る気象庁の一次細分区域を判定し、警報・注意報が出ている区間だけルート線を特別警報(紫)/警報(赤)/注意報(黄)で上塗りする。表示中のルートのみ。ONにした人だけがルート確定のたびに気象庁へ問い合わせる",
        _ => "Google APIキー: 検索(Geocoding)とStreet View共通。Enterで入力欄を開く(Cmd+V貼付も可)。環境変数TERMMAP_GOOGLE_API_KEYでも可",
    }
}

// Focus::Settings描画時の左袖項目一覧(its)の組み立て。ui.rs側のonboarded_marker()(ファイルIO)や
// set_sel/set_pick_sel(interactive()のローカル選択状態)は呼び出し側で評価/取得し、値(bool/usize)として
// 渡す(この関数自体はopts/cfg/引数の値だけを見る純関数)。戻り値は(見出し, 項目一覧, 選択位置)で、
// ui.rs側の他フォーカスの分岐が返す形と同じ。
// - picking: Focus::SettingsPick(idx)ならSome(idx)(サイド一覧を展開表示中の項目)
// - onboarded: オンボーディング済み(marker存在)ならtrue
// - set_sel: 設定画面での選択中の行番号
// - set_pick_sel: SettingsPick(一覧選択)展開中の候補選択位置
pub(crate) fn settings_rows(opts: &Args, cfg: &Config, picking: Option<usize>, onboarded: bool, set_sel: usize, set_pick_sel: usize) -> (String, Vec<String>, usize) {
    let onoff = |b: bool| if b { "ON" } else { "OFF" };
    let keyset = if cfg.google_maps_api_key.trim().is_empty() { "未設定" } else { "設定済" };
    let mode_ja = match cfg.route_profile.as_str() { "car-fast" => "高速", "moped" => "下道", "shortest" => "最短", o => o };
    let model_ja = match cfg.llm_model.as_str() { "claude-sonnet-5" => "sonnet", "claude-haiku-4-5" => "haiku", "claude-opus-4-8" => "opus", o => o };
    let arrow = |idx: usize| if picking == Some(idx) { "▾" } else { "▸" };
    let mut its = vec![
        format!("点字ドット {}", onoff(opts.braille)),
        format!("地物色分け {}", onoff(opts.classify)),
        format!("輪郭抽出 {}", onoff(opts.edge)),
        format!("単色 {}", onoff(opts.mono)),
        format!("{} 地図種別 {}", arrow(4), opts.style),
        format!("{} 既定ルート {}", arrow(5), mode_ja),
        format!("道路の点間隔 {}m", cfg.sample_interval_m as i64),
        format!("スポット既定表示 {}", onoff(cfg.show_spots)),
        format!("おすすめ {}", onoff(cfg.llm_recommend_enabled)),
        format!("{} 提案AIモデル {}", arrow(9), model_ja),
        format!("実写(StreetView) {}", onoff(cfg.streetview_enabled)),
        format!("画像表示(iTerm2) {}", onoff(cfg.image_mode)),
        format!("{} 画像解像度 {}", arrow(12), match cfg.image_res.as_str() { "high" => "高", "low" => "低", _ => "中" }),
        format!("移動中の低解像度化 {}", onoff(cfg.image_settle_low_res)),
        format!("サウンド {}", onoff(cfg.sound_enabled)),
        format!("オンボーディング {}", if onboarded { "非表示" } else { "毎回表示" }),
        format!("{} 中心十字の色 {}", arrow(16), PALETTE_NAMES[cfg.cross_color_idx as usize % PALETTE_NAMES.len()]),
        format!("Google APIキー {}", keyset),
        format!("{} QR表示方式 {}", arrow(18), match cfg.qr_style.as_str() { "image" => "画像(小型)", _ => "標準" }),
        format!("雨雲レーダー {}", onoff(cfg.radar_enabled)),
        format!("{} 雨雲の濃さ {}", arrow(20), match cfg.radar_opacity.as_str() { "light" => "薄い", "strong" => "濃い", _ => "標準" }),
        format!("ルート音声案内 {}", onoff(cfg.voice_guide_enabled)),
        format!("道路交通量 {}", onoff(cfg.traffic_enabled)),
        format!("音声をこの端末でも再生 {}", onoff(cfg.voice_speak_local)),
        format!("道路ライブカメラ {}", onoff(cfg.camera_enabled)),
        format!("通行規制 {}", onoff(cfg.regulation_enabled)),
        format!("過去災害 {}", onoff(cfg.disaster_enabled)),
        format!("{} 読み上げの声 {}", arrow(27), if cfg.voice_name.is_empty() { "システム既定".to_string() } else { crate::voice::display_voice_name(&cfg.voice_name).to_string() }),
        format!("渋滞状況の色分け {}", onoff(cfg.route_traffic_enabled)),
        format!("過去災害の塗り {}", onoff(cfg.disaster_fill)),
        format!("人口メッシュ {}", onoff(cfg.population_enabled)),
        format!("{} 人口の濃さ {}", arrow(31), match cfg.population_opacity.as_str() { "light" => "薄い", "strong" => "濃い", _ => "標準" }),
        format!("{} 人口の年次 {}年", arrow(32), cfg.population_year),
        format!("ルート沿い気象警報 {}", onoff(cfg.weather_warning_enabled)),
    ];
    debug_assert_eq!(its.len(), SETTINGS_ROW_COUNT, "SETTINGS_ROW_COUNT と行数がずれている");
    // アコーディオン展開: 選択中の項目がpickable(3択以上)ならその直下に候補をインデント挿入し、他行を押し下げる
    let mut sel = set_sel;
    if let Some(idx) = picking {
        let labels = pick_labels(idx, cfg);
        let sub: Vec<String> = labels.iter().map(|l| format!("    {l}")).collect();
        let at = idx + 1;
        for (i, s) in sub.into_iter().enumerate() { its.insert(at + i, s); }
        sel = at + set_pick_sel;
    }
    ("設定".to_string(), its, sel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn pickable_covers_the_multi_choice_items_and_cross_color() {
        for idx in [4usize, 5, 9, 12, 16, 18, 20, 27, 31, 32] {
            assert!(is_pickable(idx), "idx {idx} should be pickable");
        }
        for idx in [0usize, 1, 2, 3, 6, 7, 8, 10, 11, 13, 14, 15, 17, 19, 28, 29, 30] {
            assert!(!is_pickable(idx), "idx {idx} should not be pickable");
        }
    }

    #[test]
    fn pick_current_finds_existing_value() {
        let mut cfg = Config::default();
        cfg.route_profile = "moped".to_string();
        assert_eq!(pick_current(5, &cfg, "osm"), 1); // moped は values[1]
        assert_eq!(pick_current(4, &cfg, "dark"), 2); // style は cfg でなく渡した style 引数を見る
    }

    #[test]
    fn pick_current_unknown_value_defaults_to_zero() {
        let mut cfg = Config::default();
        cfg.llm_model = "something-unknown".to_string();
        assert_eq!(pick_current(9, &cfg, "osm"), 0);
    }

    #[test]
    fn apply_pick_writes_style_and_flags_cache_clear() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        let eff = apply_pick(4, 2, &mut cfg, &mut style); // 2 => "dark"
        assert_eq!(style, "dark");
        assert!(eff.cache_clear);
        assert!(!eff.force_reemit);
    }

    #[test]
    fn apply_pick_writes_route_profile_without_side_effects() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        let eff = apply_pick(5, 2, &mut cfg, &mut style); // 2 => "shortest"
        assert_eq!(cfg.route_profile, "shortest");
        assert!(!eff.cache_clear);
        assert!(!eff.force_reemit);
    }

    #[test]
    fn apply_pick_writes_cross_color_and_flags_force_reemit() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        let eff = apply_pick(16, 3, &mut cfg, &mut style);
        assert_eq!(cfg.cross_color_idx, 3);
        assert!(eff.force_reemit);
        assert!(!eff.cache_clear);
    }

    #[test]
    fn apply_pick_out_of_range_sel_is_ignored() {
        let mut cfg = Config::default();
        let before = cfg.image_res.clone();
        let mut style = "osm".to_string();
        let eff = apply_pick(12, 99, &mut cfg, &mut style); // labels は3個しかない
        assert_eq!(cfg.image_res, before);
        assert!(!eff.force_reemit);
    }

    #[test]
    fn pick_labels_len_matches_choice_values_len() {
        let cfg = Config::default();
        for idx in [4usize, 5, 9, 12, 18, 20, 31] {
            let c = choice_for(idx).unwrap();
            assert_eq!(pick_labels(idx, &cfg).len(), c.values.len());
        }
        assert_eq!(pick_labels(16, &cfg).len(), PALETTE_NAMES.len());
        assert_eq!(pick_labels(32, &cfg).len(), crate::config::POPULATION_YEARS.len());
    }

    // voice_choices/pick_labels(27,..)は実行環境にインストール済みの音声(say -v '?')に依存する
    // ため、具体的な件数・名前には依存しない不変条件だけを確認する。
    #[test]
    fn voice_choices_always_starts_with_system_default() {
        let cfg = Config::default();
        assert_eq!(voice_choices(&cfg)[0], ("".to_string(), "システム既定".to_string()));
    }

    #[test]
    fn voice_choices_never_flags_not_found_when_voice_name_is_empty() {
        let mut cfg = Config::default();
        cfg.voice_name = "".to_string();
        assert!(voice_choices(&cfg).iter().all(|(_, label)| !label.contains("(未検出)")));
    }

    #[test]
    fn voice_choices_flags_unknown_voice_name_as_not_found_only_when_some_voice_is_installed() {
        let mut cfg = Config::default();
        cfg.voice_name = "TermmapTestVoiceThatDoesNotExist".to_string();
        let choices = voice_choices(&cfg);
        if crate::voice::japanese_voices().is_empty() {
            // 列挙できていない(非macOS/say失敗)ときは「未検出」と断定しない(システム既定のみ)。
            assert_eq!(choices.len(), 1);
        } else {
            let last = choices.last().unwrap();
            assert_eq!(last.0, cfg.voice_name);
            assert!(last.1.contains("(未検出)"), "{:?}", last);
        }
    }

    #[test]
    fn pick_current_and_apply_pick_roundtrip_voice_name_system_default() {
        let mut cfg = Config::default();
        cfg.voice_name = "".to_string();
        let mut style = "osm".to_string();
        assert_eq!(pick_current(27, &cfg, &style), 0); // システム既定は常に先頭
        let eff = apply_pick(27, 0, &mut cfg, &mut style);
        assert_eq!(cfg.voice_name, "");
        assert!(!eff.cache_clear);
        assert!(!eff.force_reemit);
    }

    #[test]
    fn apply_pick_voice_name_out_of_range_sel_is_ignored() {
        let mut cfg = Config::default();
        cfg.voice_name = "Kyoko".to_string();
        let mut style = "osm".to_string();
        let eff = apply_pick(27, 9999, &mut cfg, &mut style);
        assert_eq!(cfg.voice_name, "Kyoko");
        assert!(!eff.force_reemit);
    }

    #[test]
    fn settings_rows_shows_system_default_label_for_empty_voice_name() {
        let mut cfg = Config::default();
        cfg.voice_name = "".to_string();
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 27, 0);
        assert_eq!(its[27], "▸ 読み上げの声 システム既定");
        // 既存項目(24〜26)の並びが動いていないことの回帰確認
        assert!(its[24].starts_with("道路ライブカメラ"));
        assert!(its[25].starts_with("通行規制"));
        assert!(its[26].starts_with("過去災害"));
    }

    #[test]
    fn settings_rows_shows_route_traffic_row_after_voice_name() {
        let mut cfg = Config::default();
        cfg.route_traffic_enabled = true;
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 28, 0);
        assert_eq!(its.len(), SETTINGS_ROW_COUNT);
        assert_eq!(its[28], "渋滞状況の色分け ON");
        assert!(its[27].contains("読み上げの声"), "既存項目(27)の並びが動いていない");
    }

    #[test]
    fn settings_rows_shows_the_disaster_fill_row_after_route_traffic() {
        // 項目は必ず末尾に足す(既存 idx を動かすと ui.rs の生の数値比較と食い違う)。
        // 過去災害の塗り=29 は固定で、人口メッシュの3行(30〜32)はその後ろに並ぶ。
        let cfg = Config::default();
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 29, 0);
        assert_eq!(its.len(), SETTINGS_ROW_COUNT);
        assert_eq!(its[29], "過去災害の塗り ON", "既定はON");
        assert!(its[26].starts_with("過去災害 "), "レイヤ自体のON/OFF(26)は別の行のまま");
        assert!(its[28].starts_with("渋滞状況の色分け"), "既存項目(28)の並びが動いていない");
        let mut off = Config::default();
        off.disaster_fill = false;
        let (_, its_off, _) = settings_rows(&test_args(), &off, None, false, 29, 0);
        assert_eq!(its_off[29], "過去災害の塗り OFF");
    }

    #[test]
    fn setting_description_for_the_disaster_fill_row_explains_the_two_axes() {
        let d = setting_description(29);
        assert!(d.contains("過去災害の塗り"), "{d}");
        assert!(d.contains("市区町村"), "何を塗るのかに触れる: {d}");
        assert!(d.contains("種別") && d.contains("件数"), "色と濃さの意味に触れる: {d}");
        assert_ne!(d, setting_description(26), "レイヤ本体の説明と混ざっていない");
        assert_ne!(d, setting_description(17), "フォールバック(Google APIキー)と混ざっていない");
    }

    #[test]
    fn the_disaster_fill_row_is_a_plain_toggle_not_a_picker() {
        assert!(!is_pickable(29), "ON/OFF なのでアコーディオンを開かない");
    }

    #[test]
    fn setting_description_for_route_traffic_row_mentions_google_and_coloring() {
        let d = setting_description(28);
        assert!(d.contains("渋滞状況の色分け"), "{d}");
        assert!(d.contains("黄") && d.contains("赤"), "混雑区間の色分けであることが説明文に無い: {d}");
        assert!(d.contains("Google"), "{d}");
        assert_ne!(d, setting_description(17), "28がフォールバック(Google APIキー)と混ざっていない");
    }

    #[test]
    fn pick_current_and_apply_pick_roundtrip_qr_style() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        assert_eq!(pick_current(18, &cfg, &style), 0); // dense は values[0]
        let eff = apply_pick(18, 1, &mut cfg, &mut style); // 1 => "image"
        assert_eq!(cfg.qr_style, "image");
        assert!(!eff.cache_clear);
        assert!(!eff.force_reemit);
        assert_eq!(pick_current(18, &cfg, &style), 1);
    }

    #[test]
    fn setting_description_covers_every_known_row_distinctly() {
        // 0〜16,18〜33 は個別の説明文を持つ(idx=11/27は端末対応有無で文言が変わるが、いずれにせよ
        // 空でない。17=Google APIキーはフォールバック経由で別テストで確認するためここでは含めない)。
        let mut seen = Vec::new();
        for idx in (0usize..=16).chain(18..=33) {
            let d = setting_description(idx);
            assert!(!d.is_empty(), "idx {idx} should have a description");
            if idx != 11 && idx != 27 { seen.push(d); } // 11/27は環境依存で文言が2通りあるため一意性判定から除外
        }
        let mut uniq = seen.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(seen.len(), uniq.len(), "each settings row should have a distinct description");
    }

    #[test]
    fn setting_description_out_of_range_falls_back_to_google_api_key() {
        assert_eq!(setting_description(17), setting_description(999));
        assert!(setting_description(17).contains("Google APIキー"));
    }

    #[test]
    fn pick_current_and_apply_pick_roundtrip_radar_opacity() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        assert_eq!(pick_current(20, &cfg, &style), 1); // 既定 "mid" は values[1]
        let eff = apply_pick(20, 2, &mut cfg, &mut style); // 2 => "strong"
        assert_eq!(cfg.radar_opacity, "strong");
        assert!(eff.force_reemit); // 濃さを変えたら即描き直す
        assert!(!eff.cache_clear);
        assert_eq!(pick_current(20, &cfg, &style), 2);

        let eff2 = apply_pick(20, 0, &mut cfg, &mut style); // 0 => "light"
        assert_eq!(cfg.radar_opacity, "light");
        assert!(eff2.force_reemit);
    }

    #[test]
    fn apply_pick_radar_opacity_out_of_range_sel_is_ignored() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        let eff = apply_pick(20, 99, &mut cfg, &mut style); // 候補は3個しかない
        assert_eq!(cfg.radar_opacity, Config::default().radar_opacity);
        assert!(!eff.force_reemit);
    }

    #[test]
    fn pick_current_unknown_radar_opacity_defaults_to_zero() {
        let mut cfg = Config::default();
        cfg.radar_opacity = "bogus".to_string(); // configを手書きで壊された場合
        assert_eq!(pick_current(20, &cfg, "osm"), 0);
    }

    // settings_rows() を呼ぶテスト用の Args(既定値。parse_args() の初期値と同じ)。
    fn test_args() -> Args {
        Args { lat: None, lon: None, place: None, zoom: 14, width: None, win_px: 640,
               style: "osm".to_string(), braille: false, mono: false, classify: false,
               edge: false, here: false, threshold: None,
               range: Vec::new(), home: None, route: None, route_mode: "surface".to_string(),
               gpx: None, load_route: None, save_route: None, list_routes: false, share: false,
               wander: false, dist: None, shape: "loop".to_string(), image: None, png: None }
    }

    // ui.rs のカーソル移動上限が参照する SETTINGS_ROW_COUNT が、実際の行数とずれていないこと。
    // ここがずれると「設定画面の最終行まで下がれない/存在しない行を選べる」壊れ方をする。
    #[test]
    fn settings_row_count_matches_rows() {
        let cfg = Config::default();
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 0, 0);
        assert_eq!(its.len(), SETTINGS_ROW_COUNT);
    }

    #[test]
    fn settings_rows_end_with_the_external_data_layers() {
        // 外部データレイヤ(交通量/カメラ/規制/過去災害)は末尾に並んでいる。
        // 過去災害(26)を足したときに既存行が動いていないことの回帰確認も兼ねる。
        let mut cfg = Config::default();
        cfg.disaster_enabled = true;
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 26, 0);
        assert_eq!(its[22], "道路交通量 OFF");
        assert_eq!(its[24], "道路ライブカメラ OFF");
        assert_eq!(its[25], "通行規制 OFF");
        assert_eq!(its[26], "過去災害 ON");
    }

    #[test]
    fn settings_rows_row33_shows_the_weather_warning_row_after_population_year() {
        let mut cfg = Config::default();
        cfg.weather_warning_enabled = true;
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 33, 0);
        assert_eq!(its.len(), SETTINGS_ROW_COUNT);
        assert_eq!(its[33], "ルート沿い気象警報 ON");
        // 既存項目(28〜32)の並びが動いていないことの回帰確認
        assert!(its[28].starts_with("渋滞状況の色分け"));
        assert!(its[32].contains("人口の年次"));
    }

    #[test]
    fn setting_description_for_the_disaster_row_mentions_its_source_and_key() {
        let d = setting_description(26);
        assert!(d.contains("過去災害"), "{d}");
        assert!(d.contains("防災科学技術研究所"), "出典を出す: {d}");
        assert!(d.contains("B"), "詳細表示のキーに触れる: {d}");
        assert_ne!(d, setting_description(25), "通行規制の説明と混ざっていない");
    }

    #[test]
    fn setting_description_for_voice_row_never_falls_back_to_google_key() {
        let d = setting_description(27);
        assert!(d.contains("読み上げの声"), "{d}");
        assert_ne!(d, setting_description(17), "27がフォールバック(Google APIキー)と混ざっていない");
    }

    #[test]
    fn settings_rows_end_with_the_two_radar_rows() {
        let mut cfg = Config::default();
        cfg.radar_enabled = true;
        cfg.radar_opacity = "strong".to_string();
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 19, 0);
        assert_eq!(its[19], "雨雲レーダー ON");
        assert_eq!(its[20], "▸ 雨雲の濃さ 濃い");
        // 既存項目(0〜18)の並びが動いていないことの回帰確認
        assert!(its[0].starts_with("点字ドット"));
        assert!(its[17].starts_with("Google APIキー"));
        assert!(its[18].contains("QR表示方式"));
    }

    #[test]
    fn settings_rows_expands_radar_opacity_accordion_below_its_row() {
        let cfg = Config::default();
        // idx=20 を展開: 20行目の直下に候補3件がインデントで挿入され、選択位置はその中を指す
        let (_, its, sel) = settings_rows(&test_args(), &cfg, Some(20), false, 20, 1);
        assert_eq!(its.len(), SETTINGS_ROW_COUNT + 3);
        assert_eq!(its[20], "▾ 雨雲の濃さ 標準"); // 展開中は矢印が▾になる
        assert_eq!(its[21], "    薄い");
        assert_eq!(its[22], "    標準");
        assert_eq!(its[23], "    濃い");
        assert_eq!(sel, 22); // 21(先頭候補) + set_pick_sel(1)
    }

    // ---- 人口メッシュ(設計 §9) ----

    #[test]
    fn settings_rows_end_with_the_three_population_rows() {
        let mut cfg = Config::default();
        cfg.population_enabled = true;
        cfg.population_opacity = "strong".to_string();
        cfg.population_year = 2050;
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 30, 0);
        assert_eq!(its.len(), SETTINGS_ROW_COUNT);
        assert_eq!(its[30], "人口メッシュ ON");
        assert_eq!(its[31], "▸ 人口の濃さ 濃い");
        assert_eq!(its[32], "▸ 人口の年次 2050年");
        // 既存項目の並びが動いていないことの回帰確認。
        assert_eq!(its[28], "渋滞状況の色分け OFF");
        assert!(its[29].starts_with("過去災害の塗り"));
        assert!(its[26].starts_with("過去災害"));
        assert!(its[19].starts_with("雨雲レーダー"));
    }

    #[test]
    fn the_population_rows_default_to_off_mid_and_2025() {
        let cfg = Config::default();
        let (_, its, _) = settings_rows(&test_args(), &cfg, None, false, 0, 0);
        assert_eq!(its[30], "人口メッシュ OFF", "既定OFF(1都道府県が最大31MBあるため)");
        assert_eq!(its[31], "▸ 人口の濃さ 標準");
        assert_eq!(its[32], "▸ 人口の年次 2025年");
    }

    #[test]
    fn pick_current_and_apply_pick_roundtrip_population_opacity() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        assert_eq!(pick_current(31, &cfg, &style), 1); // 既定 "mid"
        let eff = apply_pick(31, 2, &mut cfg, &mut style);
        assert_eq!(cfg.population_opacity, "strong");
        assert!(eff.force_reemit, "濃さを変えたら即描き直す");
        assert!(!eff.cache_clear);
        assert_eq!(pick_current(31, &cfg, &style), 2);
        // 範囲外の選択は無視する。
        let before = cfg.population_opacity.clone();
        let eff2 = apply_pick(31, 99, &mut cfg, &mut style);
        assert_eq!(cfg.population_opacity, before);
        assert!(!eff2.force_reemit);
    }

    #[test]
    fn pick_current_and_apply_pick_roundtrip_population_year() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        assert_eq!(pick_current(32, &cfg, &style), 1, "既定2025は2番目(先頭は2020)");
        let eff = apply_pick(32, 0, &mut cfg, &mut style);
        assert_eq!(cfg.population_year, 2020);
        assert!(eff.force_reemit);
        assert_eq!(pick_current(32, &cfg, &style), 0);
        let eff2 = apply_pick(32, 10, &mut cfg, &mut style);
        assert_eq!(cfg.population_year, 2070);
        assert!(eff2.force_reemit);
        // 範囲外の選択は無視する。
        let eff3 = apply_pick(32, 99, &mut cfg, &mut style);
        assert_eq!(cfg.population_year, 2070);
        assert!(!eff3.force_reemit);
    }

    #[test]
    fn pick_current_unknown_population_values_default_to_zero() {
        let mut cfg = Config::default();
        cfg.population_opacity = "bogus".to_string(); // configを手書きで壊された場合
        cfg.population_year = 2021; // 5年刻みに無い年
        assert_eq!(pick_current(31, &cfg, "osm"), 0);
        assert_eq!(pick_current(32, &cfg, "osm"), 0);
    }

    // 2020年は実績・それ以降は推計であることがラベルで分かる(混ぜて読むと誤読される)。
    #[test]
    fn the_year_labels_distinguish_the_census_year_from_the_projections() {
        let labels = population_year_labels();
        assert_eq!(labels.len(), 11);
        assert_eq!(labels[0], "2020年(実績)");
        assert_eq!(labels[1], "2025年(推計)");
        assert_eq!(labels[10], "2070年(推計)");
    }

    #[test]
    fn setting_description_for_the_population_rows_mentions_the_cost_and_the_key() {
        let d = setting_description(30);
        assert!(d.contains("人口メッシュ"), "{d}");
        assert!(d.contains("国土数値情報"), "出典を出す: {d}");
        assert!(d.contains("31MB"), "通信量が予期できること: {d}");
        assert!(d.contains("U"), "切替キーに触れる: {d}");
        assert_ne!(d, setting_description(17), "フォールバック(Google APIキー)と混ざっていない");
        assert_ne!(d, setting_description(29), "過去災害の塗り(29)の説明と混ざっていない");
        assert!(setting_description(31).contains("濃さ"));
        assert!(setting_description(32).contains("実績"), "2020年が実績であることに触れる");
    }

    #[test]
    fn settings_rows_expands_the_population_year_accordion_below_its_row() {
        let cfg = Config::default();
        let (_, its, sel) = settings_rows(&test_args(), &cfg, Some(32), false, 32, 2);
        assert_eq!(its.len(), SETTINGS_ROW_COUNT + crate::config::POPULATION_YEARS.len());
        assert_eq!(its[32], "▾ 人口の年次 2025年");
        assert_eq!(its[33], "    2020年(実績)");
        assert_eq!(its[34], "    2025年(推計)");
        assert_eq!(sel, 35); // 33(先頭候補) + set_pick_sel(2)
    }
}
