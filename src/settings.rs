// Focus::Settings(設定画面)のうち「3択以上」の項目を、アコーディオン式(選択中の行の直下に
// 候補をインデント展開し、他行を押し下げる)で直接選べるようにするための実装。
// パネル描画自体はui.rs側の左袖一覧描画に統合されている(ここでは選択肢テーブル・現在値の
// 算出・確定処理のみを持つ)。
//
// Focus enum 自体・対話ループ本体は ui.rs の interactive() 内ローカル状態(cx/cy/wps/cache 等)に
// 強く依存しているためここには移せない。ここに切り出したのは、その状態を必要としない純粋な部分のみ。

use crate::config::Config;

// 色ピッカー(ColorPick)と同じ並びの色名。中心十字の色選択・設定画面の表示に使う。
pub(crate) const PALETTE_NAMES: [&str; 10] = ["赤", "橙", "金", "黄緑", "水色", "紫", "桃", "緑青", "茶", "灰"];

// Focus::Settings の何行目(idx)が一覧選択(SettingsPick)の対象かのテーブル。
// values = cfg/opts に書き込む内部値、labels = 一覧に出す表示名(values と同じ並び)。
pub(crate) struct SettingChoice {
    pub idx: usize,
    pub values: &'static [&'static str],
    pub labels: &'static [&'static str],
}

// idx は Focus::Settings 側の項目行番号と対応(4=地図種別/5=既定ルート/9=提案AIモデル/12=画像解像度)。
// 中心十字の色(idx=16)は cfg.cross_color_idx が String でなく u8 なので、この表とは別枠(is_pickable等で16を特別扱い)。
pub(crate) const CHOICES: &[SettingChoice] = &[
    SettingChoice { idx: 4, values: &["osm", "voyager", "dark", "light"], labels: &["osm", "voyager", "dark", "light"] },
    SettingChoice { idx: 5, values: &["car-fast", "moped", "shortest"], labels: &["高速", "下道", "最短"] },
    SettingChoice { idx: 9, values: &["claude-sonnet-5", "claude-haiku-4-5", "claude-opus-4-8"], labels: &["sonnet", "haiku", "opus"] },
    SettingChoice { idx: 12, values: &["high", "mid", "low"], labels: &["高", "中", "低"] },
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
        _ => {}
    }
    eff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn pickable_covers_the_four_multi_choice_items_and_cross_color() {
        for idx in [4usize, 5, 9, 12, 16] {
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
        for idx in [4usize, 5, 9, 12] {
            let c = choice_for(idx).unwrap();
            assert_eq!(pick_labels(idx).len(), c.values.len());
        }
        assert_eq!(pick_labels(16).len(), PALETTE_NAMES.len());
    }
}
