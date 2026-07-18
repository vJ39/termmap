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

// idx は Focus::Settings 側の項目行番号と対応(4=地図種別/5=既定ルート/9=提案AIモデル/12=画像解像度/18=QR表示方式)。
// 中心十字の色(idx=16)は cfg.cross_color_idx が String でなく u8 なので、この表とは別枠(is_pickable等で16を特別扱い)。
pub(crate) const CHOICES: &[SettingChoice] = &[
    SettingChoice { idx: 4, values: &["osm", "voyager", "dark", "light", "topo"], labels: &["osm", "voyager", "dark", "light", "topo"] },
    SettingChoice { idx: 5, values: &["car-fast", "moped", "shortest"], labels: &["高速", "下道", "最短"] },
    SettingChoice { idx: 9, values: &["claude-sonnet-5", "claude-haiku-4-5", "claude-opus-4-8"], labels: &["sonnet", "haiku", "opus"] },
    SettingChoice { idx: 12, values: &["high", "mid", "low"], labels: &["高", "中", "低"] },
    SettingChoice { idx: 18, values: &["dense", "quadrant", "braille"], labels: &["標準", "小型A", "極小B"] },
];

fn choice_for(idx: usize) -> Option<&'static SettingChoice> { CHOICES.iter().find(|c| c.idx == idx) }

// idx が SettingsPick(一覧選択)の対象か。中心十字の色(16)も対象に含む。
pub(crate) fn is_pickable(idx: usize) -> bool { idx == 16 || choice_for(idx).is_some() }

// 一覧に出す表示ラベル(現在値のハイライト位置は pick_current で別途求める)。
pub(crate) fn pick_labels(idx: usize) -> Vec<&'static str> {
    if idx == 16 { PALETTE_NAMES.to_vec() } else { choice_for(idx).map(|c| c.labels.to_vec()).unwrap_or_default() }
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
        18 => "QR表示方式: スマホ共有QRの描画密度。Enterで一覧を開いて選択(標準=読み取り安定・現状のサイズ / 小型A=ブロックのまま小型化するが縦長に歪む / 極小B=正方形を保ったまま最小化するが丸ドットになる。A/Bは実機でスキャン確認推奨)",
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
        format!("{} QR表示方式 {}", arrow(18), match cfg.qr_style.as_str() { "quadrant" => "小型A", "braille" => "極小B", _ => "標準" }),
    ];
    // アコーディオン展開: 選択中の項目がpickable(3択以上)ならその直下に候補をインデント挿入し、他行を押し下げる
    let mut sel = set_sel;
    if let Some(idx) = picking {
        let labels = pick_labels(idx);
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
    fn pickable_covers_the_four_multi_choice_items_and_cross_color() {
        for idx in [4usize, 5, 9, 12, 16, 18] {
            assert!(is_pickable(idx), "idx {idx} should be pickable");
        }
        for idx in [0usize, 1, 2, 3, 6, 7, 8, 10, 11, 13, 14, 15, 17] {
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
        for idx in [4usize, 5, 9, 12, 18] {
            let c = choice_for(idx).unwrap();
            assert_eq!(pick_labels(idx).len(), c.values.len());
        }
        assert_eq!(pick_labels(16).len(), PALETTE_NAMES.len());
    }

    #[test]
    fn pick_current_and_apply_pick_roundtrip_qr_style() {
        let mut cfg = Config::default();
        let mut style = "osm".to_string();
        assert_eq!(pick_current(18, &cfg, &style), 0); // dense は values[0]
        let eff = apply_pick(18, 2, &mut cfg, &mut style); // 2 => "braille"
        assert_eq!(cfg.qr_style, "braille");
        assert!(!eff.cache_clear);
        assert!(!eff.force_reemit);
        assert_eq!(pick_current(18, &cfg, &style), 2);
    }

    #[test]
    fn setting_description_covers_every_known_row_distinctly() {
        // 0〜16,18 は個別の説明文を持つ(idx=11は端末対応有無で文言が変わるが、いずれにせよ空でない。
        // 17=Google APIキーはフォールバック経由で別テストで確認するためここでは含めない)。
        let mut seen = Vec::new();
        for idx in (0usize..=16).chain(std::iter::once(18)) {
            let d = setting_description(idx);
            assert!(!d.is_empty(), "idx {idx} should have a description");
            if idx != 11 { seen.push(d); } // 11は環境依存で文言が2通りあるため一意性判定から除外
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
}
