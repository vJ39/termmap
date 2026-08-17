// Focus(いまどの画面を触っているか)ごとのキー処理の入口。もとは ui.rs の interactive() 内に
// べた書きされていた28分岐の match で、状態を UiState へ集約したことでそのまま関数へ移せた。
// 分岐の中身は画面の関心ごとに ui_keys_map / ui_keys_route / ui_keys_poi / ui_keys_spots /
// ui_keys_settings へ分けてあり、このファイルは「どの Focus をどれに渡すか」だけを持つ。
//
// 端末ハンドル out とタイルローダー loader は UiState に持たせていない(uistate.rs を通信も
// ディスクも触らない素のデータに保つため)ので、引数で受け取る。
// 戻り値は「対話ループを抜けるか(=アプリ終了)」。q キーだけが true を返す。ループを持って
// いるのは ui.rs 側なので、break を関数の中に隠さずここから返す。

use crate::focus::Focus;
use crate::menu::MenuLevel;
use crate::tiles::TileLoader;
use crate::uistate::UiState;
use crate::*;
use crossterm::event::KeyEvent;
use std::io::Write;

// 各分岐が共通で必要とする「そのフレームの値」。毎フレーム計算し直す値なので UiState には
// 置かず、まとめて渡す(ui_gutter::GutterCtx / ui_status::StatusCtx と同じ形)。
#[derive(Clone, Copy)]
pub(crate) struct KeyCtx<'a> {
    pub a: &'a Args,            // 起動時の引数(走りまくりの既定距離・形状など)
    pub loader: &'a TileLoader, // タイル取得の常駐スレッド(地図種別の変更で未着手の依頼を捨てる)
    pub lat: f64,               // 画面中心の緯度
    pub lon: f64,               // 画面中心の経度
    pub nogos: &'a str,         // 通行止め回避の指定(BRouterへ渡す)
    pub ow: u32,                // 地図部分の幅(px)
    pub oh: u32,                // 地図部分の高さ(px)
}

pub(crate) fn dispatch(st: &mut UiState, k: KeyEvent, cx: &KeyCtx, out: &mut dyn Write) -> bool {
    // 先に focus を Map へ倒しておく。各分岐は「その画面を出したままにしたいときだけ」focus を
    // 書き戻す(閉じる分岐に毎回 Focus::Map を書かせないための既定値)。
    let cur = std::mem::replace(&mut st.focus, Focus::Map);
    match cur {
        Focus::Search(buf) => ui_keys_poi::search(st, k, buf, cx.lat, cx.lon),
        Focus::SpotCatList => ui_keys_spots::spot_cat_list(st, k, out),
        Focus::Settings => ui_keys_settings::settings(st, k, cx.nogos, out),
        Focus::SettingsEdit(idx, buf) => ui_keys_settings::settings_edit(st, k, idx, buf),
        Focus::RoadSearch(buf) => ui_keys_poi::road_search(st, k, buf, cx.ow, cx.oh),
        Focus::Recommend(buf) => ui_keys_poi::recommend(st, k, buf, cx.lat, cx.lon),
        Focus::SpotList => ui_keys_spots::spot_list(st, k),
        Focus::SpotEditName(buf, gi) => ui_keys_spots::spot_edit_name(st, k, buf, gi),
        Focus::NewCat(buf) => ui_keys_spots::new_cat(st, k, buf),
        Focus::SpotRename(buf, idx) => ui_keys_spots::spot_rename(st, k, buf, idx),
        Focus::SpotForm { name, url, field } => ui_keys_spots::spot_form(st, k, name, url, field, cx.lat, cx.lon),
        Focus::PoiKindForm { label, tag, field } => ui_keys_poi::poi_kind_form(st, k, label, tag, field),
        Focus::WanderForm { dist_km } => ui_keys_route::wander_form(st, k, dist_km, cx),
        Focus::NearSearch(buf) => ui_keys_poi::near_search(st, k, buf, cx),
        Focus::PoiMenu => ui_keys_poi::poi_menu(st, k, cx),
        Focus::PoiList => ui_keys_poi::poi_list(st, k, cx.oh, cx.nogos),
        Focus::SaveName(buf) => ui_keys_route::save_name(st, k, buf),
        Focus::RouteFavMenu { sel } => ui_keys_route::route_fav_menu(st, k, sel),
        Focus::RouteList => ui_keys_route::route_list(st, k, cx.nogos),
        Focus::RoadList => ui_keys_route::road_list(st, k, out),
        Focus::WaypointList => ui_keys_route::waypoint_list(st, k, cx, out),
        Focus::Menu(MenuLevel::Categories) => ui_keys_map::menu_categories(st, k, cx, out),
        Focus::Menu(MenuLevel::Items(ci)) => ui_keys_map::menu_items(st, k, ci, cx),
        Focus::ColorPick { cat } => ui_keys_spots::color_pick(st, k, cat),
        Focus::ShapePick { cat } => ui_keys_spots::shape_pick(st, k, cat),
        Focus::SettingsPick(idx) => ui_keys_settings::settings_pick(st, k, idx, cx.loader),
        Focus::RoutePanel => ui_keys_route::route_panel(st, k, cx, out),
        Focus::Map => return ui_keys_map::map(st, k, cx, out),
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::TILE;
    use crate::tiles::Cache;
    use crate::uistate::testing::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    // TileLoader はワーカースレッドを起こすのでテスト全体で1つだけ使い回す
    // (地図種別を変える分岐を通さないので実際には触られない)。
    fn shared_loader() -> &'static TileLoader {
        static L: std::sync::OnceLock<TileLoader> = std::sync::OnceLock::new();
        L.get_or_init(|| TileLoader::start(std::sync::Arc::new(std::sync::Mutex::new(Cache::new()))))
    }

    // そのフレームの値。地図部分は 640x320px、画面中心は東京付近として組む。
    // oh=320 なので細かい1歩=5px(oh/64)・高速=80px(oh/4)になる。
    fn ctx(a: &Args) -> KeyCtx<'_> {
        KeyCtx { a, loader: shared_loader(), lat: 35.0, lon: 139.0, nogos: "", ow: 640, oh: 320 }
    }

    // 画面中心を世界地図の真ん中へ置いた状態(test_state() の既定は左上端なので、
    // パンの1歩を見たいテストが端の回り込み・上下の止めに掛かってしまう)。
    fn centered_state() -> UiState {
        let mut st = test_state();
        let n = (TILE as f64) * 2f64.powi(14);
        st.cx = n / 2.0;
        st.cy = n / 2.0;
        st
    }

    fn ch(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }
    fn code(c: KeyCode) -> KeyEvent { KeyEvent::new(c, KeyModifiers::NONE) }

    // dispatch を1回呼ぶ。端末への書き出しはテストでは Vec に受ける。
    fn press(st: &mut UiState, k: KeyEvent) -> (bool, String) {
        let a = test_args();
        let mut out: Vec<u8> = Vec::new();
        let quit = dispatch(st, k, &ctx(&a), &mut out);
        (quit, String::from_utf8_lossy(&out).to_string())
    }

    #[test]
    fn q_quits_only_from_the_map() {
        let mut st = test_state();
        assert!(press(&mut st, ch('q')).0, "地図でのqは即終了");

        // 一覧・フォームの中では q は普通の文字扱い(誤爆で終了しない)。
        let mut st = test_state();
        st.focus = Focus::SpotCatList;
        assert!(!press(&mut st, ch('q')).0);
        assert!(matches!(st.focus, Focus::SpotCatList), "画面はそのまま");
    }

    #[test]
    fn pan_moves_the_center_and_clears_the_address() {
        let mut st = centered_state();
        st.addr = "どこか".into();
        let x0 = st.cx;
        assert!(!press(&mut st, code(KeyCode::Left)).0);
        assert_eq!(st.cx, x0 - 5.0, "無印の1歩は oh/64");
        assert!(st.addr.is_empty(), "住所表示は動かしたら消す");
    }

    #[test]
    fn holding_the_same_direction_accelerates() {
        let mut st = centered_state();
        let x0 = st.cx;
        press(&mut st, code(KeyCode::Left));
        let first = x0 - st.cx;
        let x1 = st.cx;
        press(&mut st, code(KeyCode::Left)); // 220ms以内の同方向
        let second = x1 - st.cx;
        assert!(second > first, "連打するほど1歩が伸びる({first} → {second})");
        assert_eq!(st.pan_streak, 1);

        // 方向を変えたら細かい1歩に戻る。
        press(&mut st, code(KeyCode::Right));
        assert_eq!(st.pan_streak, 0);
    }

    #[test]
    fn shift_pans_fast_from_the_first_press() {
        let mut st = centered_state();
        let x0 = st.cx;
        press(&mut st, KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(st.cx - x0, 80.0, "Shift+矢印は常に高速(oh/4)");
    }

    #[test]
    fn esc_twice_asks_before_quitting() {
        let mut st = test_state();
        assert!(!press(&mut st, code(KeyCode::Esc)).0);
        assert!(!st.quit_confirm, "1回目は確認を出さない(誤爆防止)");
        assert!(st.last_esc_at.is_some());
        assert_eq!(st.addr, "もう一度Escで終了確認");

        assert!(!press(&mut st, code(KeyCode::Esc)).0, "確認を出すだけでループは抜けない");
        assert!(st.quit_confirm);
        assert!(st.last_esc_at.is_none(), "確認を出したら押下履歴は捨てる");
    }

    #[test]
    fn help_and_my_spots_open_from_the_map() {
        let mut st = test_state();
        st.help_page = 3;
        press(&mut st, ch('?'));
        assert!(st.help);
        assert_eq!(st.help_page, 0, "ヘルプは1ページ目から");

        let mut st = test_state();
        st.cat_sel = 5;
        press(&mut st, ch('P'));
        assert!(matches!(st.focus, Focus::SpotCatList));
        assert_eq!(st.cat_sel, 0);
    }

    #[test]
    fn unknown_key_on_the_map_changes_nothing() {
        let mut st = test_state();
        let (x0, y0, z0) = (st.cx, st.cy, st.z);
        st.addr = "そのまま".into();
        let (quit, written) = press(&mut st, ch('Z'));
        assert!(!quit);
        assert_eq!((st.cx, st.cy, st.z), (x0, y0, z0));
        assert_eq!(st.addr, "そのまま");
        assert!(matches!(st.focus, Focus::Map));
        assert!(written.is_empty(), "端末へも何も書かない");
    }

    #[test]
    fn esc_on_a_sub_screen_returns_to_the_map_and_clears_the_screen() {
        let mut st = test_state();
        st.focus = Focus::SpotCatList;
        st.pending_spot = Some((35.0, 139.0, "移動先".into()));
        let (quit, written) = press(&mut st, code(KeyCode::Esc));
        assert!(!quit);
        assert!(matches!(st.focus, Focus::Map));
        assert!(st.pending_spot.is_none(), "登録待ちの地点も捨てる");
        assert!(written.contains("\x1b[2J"), "左袖が残らないよう全消去する");
        assert!(st.force_reemit, "次フレームで作り直す");
    }

    #[test]
    fn search_esc_falls_back_to_the_map_by_default() {
        // 分岐側で focus を書かなければ地図に戻る(先頭の mem::replace の既定値)。
        let mut st = test_state();
        st.focus = Focus::Search("とうきょう".into());
        press(&mut st, code(KeyCode::Esc));
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn typing_keeps_the_search_focus() {
        let mut st = test_state();
        st.focus = Focus::Search(String::new());
        st.input_cur = 0;
        press(&mut st, ch('あ'));
        match &st.focus {
            Focus::Search(buf) => assert_eq!(buf, "あ"),
            _ => panic!("入力中は検索画面のまま"),
        }
        assert_eq!(st.input_cur, 1);
    }

    #[test]
    fn cached_search_uses_the_frame_center_for_the_key() {
        // KeyCtx の lat/lon がキャッシュキーに使われている(=フレームの値が届いている)ことの確認。
        // ヒットすれば通信せずその場で候補一覧へ移る。
        let mut st = test_state();
        let key = searchcache::make_key("n", "ja", "とうきょう", 35.0, 139.0);
        st.scache.insert(key.clone(), searchcache::CacheEntry {
            results: vec![(35.68, 139.76, "東京駅".into())],
            created_at: 0,
            last_used_at: 0,
        });
        st.focus = Focus::Search("とうきょう".into());
        press(&mut st, code(KeyCode::Enter));

        assert!(matches!(st.focus, Focus::PoiList));
        assert_eq!(st.pois.len(), 1);
        assert_eq!(st.pois[0].2, "東京駅");
        assert_eq!(st.poi_label, "検索:とうきょう");
        assert!(st.search_job.is_none(), "ヒット時はスレッドを起こさない");
        assert!(st.scache[&key].last_used_at > 0, "使った印(LRUの基準)を更新する");
    }

    #[test]
    fn hiding_the_route_panel_clears_the_screen() {
        let mut st = test_state();
        assert!(!st.route_panel_hidden);
        let (_, written) = press(&mut st, ch('R'));
        assert!(st.route_panel_hidden);
        assert_eq!(st.addr, "ルート一覧: 非表示");
        assert!(written.contains("\x1b[2J"), "隠す方向は全消去してから作り直す");

        // 出す方向は全消去しない(マップ側の再描画で足りる)。
        let (_, written) = press(&mut st, ch('R'));
        assert!(!st.route_panel_hidden);
        assert!(!written.contains("\x1b[2J"));
    }

    #[test]
    fn panning_off_the_edge_wraps_east_west_and_clamps_north_south() {
        let n = (TILE as f64) * 2f64.powi(14);

        // 西端をまたいだら東端へ回り込む(経度は地球を1周する)。
        let mut st = test_state();
        st.cx = 1.0;
        press(&mut st, code(KeyCode::Left));
        assert_eq!(st.cx, n - 4.0);

        // 北端は回り込まず止める(緯度は極で終わり)。
        let mut st = test_state();
        st.cy = 0.0;
        press(&mut st, code(KeyCode::Up));
        assert_eq!(st.cy, 0.0);
    }
}
