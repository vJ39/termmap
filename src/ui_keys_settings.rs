// 設定画面(,キー)のキー処理。ui_keys.rs の Focus 分岐から関心ごとに切り出した1つ。
// 画面は3つ: 一覧(Settings)・その場の文字/数値編集(SettingsEdit)・3択以上の選択(SettingsPick)。
//
// 引数は「そのフレームの値」のうち各画面が実際に使うものだけを受け取る(何に依存しているかを
// 引数で見えるようにするため。3つ以上必要になる画面は KeyCtx をまとめて受け取る)。

use crate::focus::Focus;
use crate::spots::apply_spots;
use crate::textedit::edit_line;
use crate::tiles::TileLoader;
use crate::ui_helpers::onboarded_marker;
use crate::uistate::UiState;
use crate::*;
use crossterm::event::{KeyCode, KeyEvent};
use std::io::Write;

// 設定画面の一覧。stay=false にした分岐(編集・ピッカーを開く/閉じる)だけ画面が変わる。
pub(crate) fn settings(st: &mut UiState, k: KeyEvent, route_nogos: &str, out: &mut dyn Write) {
    let mut stay = true;
    let mut changed = false;
    match k.code {
        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.set_sel = st.set_sel.saturating_sub(1); }
        // 下端は settings.rs の行数定義から取る(生の数値で持つと項目追加のたびに手で同期する羽目になる)
        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); if st.set_sel + 1 < settings::SETTINGS_ROW_COUNT { st.set_sel += 1; } }
        KeyCode::Left | KeyCode::Right => {
            if st.set_sel == 6 { let d = if k.code == KeyCode::Left { -100.0 } else { 100.0 }; st.cfg.sample_interval_m = (st.cfg.sample_interval_m + d).clamp(100.0, 5000.0); changed = true; }
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            if st.set_sel == 6 { // 道路の点間隔: インライン数値編集を開く
                let b = format!("{}", st.cfg.sample_interval_m as i64);
                st.input_cur = b.chars().count();
                st.focus = Focus::SettingsEdit(6, b);
                stay = false;
            } else if st.set_sel == 17 { // Google APIキー: インラインテキスト編集を開く(Cmd+V貼付も引き続き可)
                let b = st.cfg.google_maps_api_key.clone();
                st.input_cur = b.chars().count();
                st.focus = Focus::SettingsEdit(17, b);
                stay = false;
            } else if settings::is_pickable(st.set_sel) { // 3択以上の項目: サイドの一覧(SettingsPick)を開いて直接選ぶ
                st.set_pick_sel = settings::pick_current(st.set_sel, &st.cfg, &st.opts.style);
                st.focus = Focus::SettingsPick(st.set_sel);
                stay = false;
            } else {
                changed = true;
                match st.set_sel {
                    // 表示に効くAAスタイル(braille/classify/edge/mono)は、map_sigに含まれない
                    // opts側の状態なので、切替時はforce_reemitで確実に次フレーム反映させる。
                    0 => { st.opts.braille = !st.opts.braille; st.force_reemit = true; }
                    1 => { st.opts.classify = !st.opts.classify; st.force_reemit = true; }
                    2 => { st.opts.edge = !st.opts.edge; st.force_reemit = true; }
                    3 => { st.opts.mono = !st.opts.mono; st.force_reemit = true; }
                    7 => { st.cfg.show_spots = !st.cfg.show_spots; st.show_spots = st.cfg.show_spots; apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots); }
                    8 => st.cfg.llm_recommend_enabled = !st.cfg.llm_recommend_enabled,
                    10 => st.cfg.streetview_enabled = !st.cfg.streetview_enabled,
                    11 => { st.cfg.image_mode = !st.cfg.image_mode; st.force_reemit = true; }
                    13 => st.cfg.image_settle_low_res = !st.cfg.image_settle_low_res,
                    14 => { st.cfg.sound_enabled = !st.cfg.sound_enabled; st.snd = sound::Sound::new(st.cfg.sound_enabled); st.snd.play("confirm"); }
                    15 => { // オンボーディング: マーカーの削除=毎回表示 / 作成=次回から非表示
                        if let Some(p) = onboarded_marker() {
                            if p.exists() { let _ = std::fs::remove_file(&p); st.addr = "オンボーディング: 毎回表示に戻した".into(); }
                            else { let _ = crate::fsutil::write_atomic(&p, b"1", None); st.addr = "オンボーディング: 次回から非表示".into(); }
                        }
                    }
                    19 => { // 雨雲レーダー: 起動時の既定を切り替え、いま表示中の地図にも即反映する
                        st.cfg.radar_enabled = !st.cfg.radar_enabled;
                        if st.cfg.radar_enabled != st.radar_on { st.radar_toggle(); }
                    }
                    21 => { // ルート音声案内: ONにした時、既にルートがあれば曲がり角を取りに行く
                        st.cfg.voice_guide_enabled = !st.cfg.voice_guide_enabled;
                        if st.cfg.voice_guide_enabled {
                            if let Some(pts) = st.spec.routes.last().map(|rt| rt.pts.clone()) {
                                st.turn_job = Some(trigger_turn_points(&st.wps, &st.mode, 0, &pts, &route_nogos));
                            }
                        }
                    }
                    // 道路交通量/ライブカメラ/通行規制: ONにした時の後始末は不要。
                    // 次のtickでセル表を見に行き、キャッシュがfreshならディスクから
                    // 即座に出す(ONにした瞬間に前回の内容が出て、必要なら裏で更新される)。
                    22 => { st.cfg.traffic_enabled = !st.cfg.traffic_enabled; }
                    23 => { st.cfg.voice_speak_local = !st.cfg.voice_speak_local; }
                    24 => { st.cfg.camera_enabled = !st.cfg.camera_enabled; }
                    25 => { st.cfg.regulation_enabled = !st.cfg.regulation_enabled; }
                    26 => { // 過去災害: ONにした直後だけ出典を1回出す(雨雲レーダーと同じ扱い)
                        st.cfg.disaster_enabled = !st.cfg.disaster_enabled;
                        if st.cfg.disaster_enabled { st.addr = "過去災害: 防災科学技術研究所 災害事例データベース".into(); }
                    }
                    28 => { // 渋滞状況の色分け: 次にルートが確定したタイミングで初めて問い合わせる
                        st.cfg.route_traffic_enabled = !st.cfg.route_traffic_enabled;
                        if st.cfg.route_traffic_enabled && st.cfg.google_maps_api_key.trim().is_empty() {
                            st.addr = "渋滞状況の色分け: Google APIキー未設定".into();
                        }
                    }
                    _ => {}
                }
            }
        }
        KeyCode::Esc => { st.snd.play("back"); stay = false; let _ = write!(out, "\x1b[2J"); st.force_reemit = true; } // 閉じる→Map。他の左袖パネルと同じく残像防止に全消去+再emit
        _ => {}
    }
    if changed { // 変更のたびに opts→cfg を同期して即保存(sを押さなくてよい)
        st.cfg.braille = st.opts.braille; st.cfg.classify = st.opts.classify; st.cfg.edge = st.opts.edge; st.cfg.mono = st.opts.mono; st.cfg.style = st.opts.style.clone();
        let _ = config::save_config(&st.cfg);
    }
    if stay { st.focus = Focus::Settings; }
}

pub(crate) fn settings_edit(st: &mut UiState, k: KeyEvent, idx: usize, mut buf: String) {
    match k.code {
        KeyCode::Enter => {
            if idx == 6 {
                match buf.trim().parse::<f64>() {
                    Ok(v) => { st.cfg.sample_interval_m = v.clamp(100.0, 5000.0); let _ = config::save_config(&st.cfg); st.addr = format!("道路の点間隔: {}m", st.cfg.sample_interval_m as i64); }
                    Err(_) => { st.snd.play("error"); st.addr = "数値を入力してください(例: 800)".into(); }
                }
            } else if idx == 17 {
                let v = buf.trim().to_string();
                if v.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                    st.cfg.google_maps_api_key = v; let _ = config::save_config(&st.cfg); st.addr = "APIキー設定(自動保存)".into();
                } else { st.snd.play("error"); st.addr = "APIキーに使えない文字が含まれています".into(); }
            }
            st.focus = Focus::Settings;
        }
        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Settings; } // 編集を破棄
        // 数値欄(道路の点間隔)は数字/小数点/マイナスのみ受け付ける。APIキー欄は制御文字・改行を弾く。
        KeyCode::Char(c) if idx == 6 && !(c.is_ascii_digit() || c == '.' || c == '-') => {}
        KeyCode::Char(c) if idx == 17 && !(c.is_ascii_graphic() || c == ' ') => {}
        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::SettingsEdit(idx, buf); }
    }
}

// 設定画面の一覧ピッカー: 地図種別/既定ルート/AIモデル/画像解像度/中心十字の色を↑↓/w・sで選びEnterで確定
pub(crate) fn settings_pick(st: &mut UiState, k: KeyEvent, idx: usize, loader: &TileLoader) {
    let n = settings::pick_labels(idx, &st.cfg).len().max(1);
    match k.code {
        KeyCode::Up | KeyCode::Char('w') => { st.set_pick_sel = (st.set_pick_sel + n - 1) % n; st.focus = Focus::SettingsPick(idx); }
        KeyCode::Down | KeyCode::Char('s') => { st.set_pick_sel = (st.set_pick_sel + 1) % n; st.focus = Focus::SettingsPick(idx); }
        // 読み上げの声(27)だけ: Spaceでカーソル位置の声を試聴(確定せず一覧も閉じない)。
        KeyCode::Char(' ') if idx == 27 => {
            if let Some((v, _)) = settings::voice_choices(&st.cfg).get(st.set_pick_sel) {
                st.voice_preview_job = Some(voice::preview_voice(v, "300メートル先、左折です"));
                let name = if v.is_empty() { "システム既定".to_string() } else { voice::display_voice_name(v).to_string() };
                st.addr = format!("試聴: {name}(この端末で再生)");
            }
            st.focus = Focus::SettingsPick(idx);
        }
        KeyCode::Enter => {
            let eff = settings::apply_pick(idx, st.set_pick_sel, &mut st.cfg, &mut st.opts.style);
            // スタイル変更時、キャッシュ自体はもう消さない(TileKeyがstyleを含むため
            // 別styleと混ざる心配は無く、むしろ残しておくことで切替直後に旧styleを
            // フォールバック仮表示できる)。ローダーの未着手依頼だけ捨てる(旧styleの
            // 取得依頼が溜まり続けないように)。
            if eff.cache_clear { loader.clear_pending(); }
            if eff.force_reemit { st.force_reemit = true; }
            let _ = config::save_config(&st.cfg);
            // 読み上げの声(27)は確定した声が実際に使えるか、確定時にも1回再生して確かめる。
            if idx == 27 {
                st.voice_preview_job = Some(voice::preview_voice(&st.cfg.voice_name, "300メートル先、左折です"));
                let name = if st.cfg.voice_name.is_empty() { "システム既定".to_string() } else { voice::display_voice_name(&st.cfg.voice_name).to_string() };
                st.addr = format!("試聴: {name}(この端末で再生)");
            }
            st.focus = Focus::Settings;
        }
        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Settings; } // 変更せず閉じる
        _ => st.focus = Focus::SettingsPick(idx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::Cache;
    use crate::uistate::testing::*;
    use crossterm::event::KeyModifiers;

    // TileLoader はワーカースレッドを起こすのでテスト全体で1つだけ使い回す。
    fn shared_loader() -> &'static TileLoader {
        static L: std::sync::OnceLock<TileLoader> = std::sync::OnceLock::new();
        L.get_or_init(|| TileLoader::start(std::sync::Arc::new(std::sync::Mutex::new(Cache::new()))))
    }

    fn ch(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }
    fn code(c: KeyCode) -> KeyEvent { KeyEvent::new(c, KeyModifiers::NONE) }

    // ui_keys::dispatch は focus を Map へ倒してから呼ぶので、テストも同じ前提で始める
    // (「画面を出したままにする」分岐だけが focus を書き戻す)。test_state() の既定が Map。
    fn settings_state() -> UiState {
        let mut st = test_state();
        st.focus = Focus::Map;
        st
    }

    // 設定を保存する分岐($HOME/.config/termmap/config.toml を書く)はテストから触らない。
    // ここで確かめるのはカーソル移動・画面遷移・入力の受け付け方だけ。
    fn press(st: &mut UiState, k: KeyEvent) -> String {
        let mut out: Vec<u8> = Vec::new();
        settings(st, k, "", &mut out);
        String::from_utf8_lossy(&out).to_string()
    }

    #[test]
    fn arrow_keys_move_the_cursor_and_stop_at_both_ends() {
        let mut st = settings_state();
        press(&mut st, code(KeyCode::Up));
        assert_eq!(st.set_sel, 0, "先頭より上へは行かない");
        press(&mut st, code(KeyCode::Down));
        assert_eq!(st.set_sel, 1);
        assert!(matches!(st.focus, Focus::Settings), "移動だけなら設定画面のまま");

        st.set_sel = settings::SETTINGS_ROW_COUNT - 1;
        press(&mut st, ch('s'));
        assert_eq!(st.set_sel, settings::SETTINGS_ROW_COUNT - 1, "末尾より下へは行かない");
    }

    #[test]
    fn enter_on_the_number_row_opens_the_inline_editor() {
        let mut st = settings_state();
        st.set_sel = 6;
        st.cfg.sample_interval_m = 800.0;
        press(&mut st, code(KeyCode::Enter));
        match &st.focus {
            Focus::SettingsEdit(idx, buf) => { assert_eq!(*idx, 6); assert_eq!(buf, "800", "現在値を初期値に入れる"); }
            _ => panic!("数値行のEnterは編集画面へ"),
        }
        assert_eq!(st.input_cur, 3, "カーソルは末尾");
    }

    #[test]
    fn enter_on_a_picker_row_opens_the_side_list_at_the_current_value() {
        let mut st = settings_state();
        st.set_sel = 4; // 地図種別(3択以上)
        st.opts.style = "gsi".to_string();
        press(&mut st, code(KeyCode::Enter));
        assert!(matches!(st.focus, Focus::SettingsPick(4)));
        assert_eq!(st.set_pick_sel, settings::pick_current(4, &st.cfg, "gsi"), "いまの値の行から始める");
    }

    #[test]
    fn esc_closes_the_settings_screen_and_clears_the_left_gutter() {
        let mut st = settings_state();
        let written = press(&mut st, code(KeyCode::Esc));
        assert!(matches!(st.focus, Focus::Map), "閉じる=Mapのまま(focusを書き戻さない)");
        assert!(written.contains("\x1b[2J"), "左袖の残像を消す");
        assert!(st.force_reemit);
    }

    #[test]
    fn the_number_row_editor_takes_digits_only() {
        let mut st = settings_state();
        st.input_cur = 0;
        settings_edit(&mut st, ch('8'), 6, String::new());
        match &st.focus {
            Focus::SettingsEdit(6, buf) => assert_eq!(buf, "8"),
            _ => panic!("入力中は編集画面のまま"),
        }

        // 数字以外は捨てる。いまの実装では focus を書き戻さないので編集画面も閉じる
        // (切り出し前からの挙動。このリファクタでは直さない)。
        let mut st = settings_state();
        settings_edit(&mut st, ch('x'), 6, "80".into());
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn the_api_key_editor_rejects_non_ascii_but_keeps_editing() {
        let mut st = settings_state();
        st.input_cur = 0;
        settings_edit(&mut st, ch('あ'), 17, String::new());
        assert!(matches!(st.focus, Focus::Map), "受け付けない文字(切り出し前からの挙動)");

        let mut st = settings_state();
        st.input_cur = 0;
        settings_edit(&mut st, ch('A'), 17, String::new());
        match &st.focus {
            Focus::SettingsEdit(17, buf) => assert_eq!(buf, "A"),
            _ => panic!("ASCIIは受け付ける"),
        }
    }

    #[test]
    fn esc_discards_the_edit_and_returns_to_the_list() {
        let mut st = settings_state();
        st.cfg.sample_interval_m = 800.0;
        settings_edit(&mut st, code(KeyCode::Esc), 6, "4000".into());
        assert!(matches!(st.focus, Focus::Settings));
        assert_eq!(st.cfg.sample_interval_m, 800.0, "編集中の値は捨てる");
    }

    #[test]
    fn the_picker_wraps_at_both_ends() {
        let n = settings::pick_labels(4, &test_cfg()).len();
        assert!(n >= 2, "この確認には候補が2つ以上必要");

        let mut st = settings_state();
        st.set_pick_sel = 0;
        settings_pick(&mut st, code(KeyCode::Up), 4, shared_loader());
        assert_eq!(st.set_pick_sel, n - 1, "先頭で↑は末尾へ回り込む");
        assert!(matches!(st.focus, Focus::SettingsPick(4)));

        settings_pick(&mut st, code(KeyCode::Down), 4, shared_loader());
        assert_eq!(st.set_pick_sel, 0, "末尾で↓は先頭へ戻る");
    }

    #[test]
    fn the_picker_esc_keeps_the_current_setting() {
        let mut st = settings_state();
        st.opts.style = "osm".to_string();
        st.set_pick_sel = 2;
        settings_pick(&mut st, code(KeyCode::Esc), 4, shared_loader());
        assert!(matches!(st.focus, Focus::Settings), "設定一覧へ戻る");
        assert_eq!(st.opts.style, "osm", "選び直さずに閉じたので変えない");
    }
}
