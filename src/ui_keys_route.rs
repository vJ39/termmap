// ルート(経由地)まわりの画面のキー処理。ui_keys.rs の Focus 分岐から関心ごとに切り出した1つ。
// おまかせ周回の距離ゲージ・ルート名の保存・お気に入りルートの一覧・道路の塊の一覧・
// 経由地の並べ替えビュー・ルートパネル。
//
// 経由地を触る分岐はどれも trigger_route() を呼び直す(2点以上あるとそこで別スレッドの経路計算が
// 始まる)。「配列をいじって引き直す」の繰り返しという形はもとのまま残してある。
//
// 引数は「そのフレームの値」のうち各画面が実際に使うものだけを受け取る(何に依存しているかを
// 引数で見えるようにするため。3つ以上必要になる画面は KeyCtx をまとめて受け取る)。

use crate::focus::Focus;
use crate::geo::*;
use crate::menu::ROUTE_ACTS;
use crate::route::*;
use crate::textedit::edit_line;
use crate::ui_keys::KeyCtx;
use crate::uistate::UiState;
use crate::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::io::Write;

pub(crate) fn wander_form(st: &mut UiState, k: KeyEvent, mut dist_km: f64, kx: &KeyCtx) {
    // 分岐の中身は ui.rs から動かしていないので、フレームの値はもとと同じ名前で受け取る。
    let KeyCtx { a, lat, lon, .. } = *kx;
    match k.code { // おまかせ周回: 距離ゲージ
        KeyCode::Left | KeyCode::Right => {
            let step = if k.modifiers.contains(KeyModifiers::SHIFT) { 20.0 } else { 5.0 };
            let d = if k.code == KeyCode::Left { -step } else { step };
            dist_km = (dist_km + d).clamp(10.0, 200.0);
            st.focus = Focus::WanderForm { dist_km };
        }
        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Map; }
        KeyCode::Enter => {
            let origin = (lat, lon);
            let shape = a.shape.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let r = wander_route(origin, dist_km, &shape);
                let _ = tx.send(r);
            });
            st.wander_job = Some(rx);
            st.addr = format!("走りまくり: {dist_km:.0}km圏を検索中…");
            st.focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
        }
        _ => st.focus = Focus::WanderForm { dist_km },
    }
}

pub(crate) fn save_name(st: &mut UiState, k: KeyEvent, mut buf: String) {
    match k.code {
        KeyCode::Enter => {
            let name = buf.trim().to_string();
            if !name.is_empty() {
                if list_named_routes().contains(&name) {
                    st.save_confirm = Some(name);
                    st.focus = Focus::SaveName(buf); // 上書き確認中も編集状態を保持(取消時はそのまま名前を変えられる)
                } else {
                    st.addr = match save_named_route(&name, &st.mode, &st.wps) { Ok(_) => { st.snd.play("confirm"); st.route_name_hint = name.clone(); format!("保存: {name}") }, Err(e) => format!("({e})") };
                }
            }
        }
        KeyCode::Esc => { st.snd.play("back"); }
        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::SaveName(buf); }
    }
}

pub(crate) fn route_fav_menu(st: &mut UiState, k: KeyEvent, sel: usize) {
    match k.code { // お気に入りルート: 保存/呼び出しの小メニュー(Sキー)
        KeyCode::Up | KeyCode::Char('w') => { st.focus = Focus::RouteFavMenu { sel: sel.saturating_sub(1) }; }
        KeyCode::Down | KeyCode::Char('s') => { st.focus = Focus::RouteFavMenu { sel: (sel + 1).min(1) }; }
        KeyCode::Enter => {
            if sel == 0 { st.input_cur = st.route_name_hint.chars().count(); st.focus = Focus::SaveName(st.route_name_hint.clone()); }
            else {
                st.route_names = list_named_routes(); st.rn_sel = 0;
                if st.route_names.is_empty() { st.addr = "お気に入り無し".into(); st.focus = Focus::Map; }
                else { st.focus = Focus::RouteList; }
            }
        }
        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Map; }
        _ => st.focus = Focus::RouteFavMenu { sel },
    }
}

pub(crate) fn route_list(st: &mut UiState, k: KeyEvent, route_nogos: &str) {
    match k.code {
        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.rn_sel = st.rn_sel.saturating_sub(1); st.focus = Focus::RouteList; }
        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); if st.rn_sel + 1 < st.route_names.len() { st.rn_sel += 1; } st.focus = Focus::RouteList; }
        KeyCode::Enter => {
            if let Some(name) = st.route_names.get(st.rn_sel) {
                if let Some((w, m)) = load_named_route(name) {
                    let (nx, ny) = deg_to_pixel(w[0].0, w[0].1, st.z); st.cx = nx; st.cy = ny;
                    st.wps = w; st.mode = m; st.wp_sel = 0;
                    st.route_name_hint = name.clone(); // 保存時にこの名前をそのまま提示する
                    { let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; }
                }
            }
        }
        KeyCode::Esc => {}
        _ => st.focus = Focus::RouteList,
    }
}

pub(crate) fn road_list(st: &mut UiState, k: KeyEvent, out: &mut dyn Write) {
    match k.code { // 道路の塊の一覧(個別削除)
        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.road_sel = st.road_sel.saturating_sub(1); st.focus = Focus::RoadList; }
        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); if st.road_sel + 1 < st.road_segs.len() { st.road_sel += 1; } st.focus = Focus::RoadList; }
        KeyCode::Char('x') => { // 選択した道路の塊を削除
            if st.road_sel < st.road_segs.len() {
                st.road_segs.remove(st.road_sel);
                if st.road_sel >= st.road_segs.len() && st.road_sel > 0 { st.road_sel -= 1; }
                st.sync_roads();
            }
            if st.road_segs.is_empty() { // 空になったら閉じる。左袖の残像を残さないよう全消去する
                st.addr = "道路を全削除".into();
                st.focus = Focus::Map;
                let _ = write!(out, "\x1b[2J");
                st.force_reemit = true;
            } else { st.focus = Focus::RoadList; }
        }
        // 閉じる → Map。左袖(道路一覧)の残像を残さないよう全消去する(Menu閉じる時と同じ理由)。
        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Map; let _ = write!(out, "\x1b[2J"); st.force_reemit = true; }
        _ => st.focus = Focus::RoadList,
    }
}

// 並べ替えビュー: ↑↓で選択(地図が追従)、Spaceで掴む↔置く、掴み中は↑↓で地点を移動
pub(crate) fn waypoint_list(st: &mut UiState, k: KeyEvent, kx: &KeyCtx, out: &mut dyn Write) {
    let KeyCtx { lat, lon, nogos: route_nogos, .. } = *kx;
    match k.code {
        KeyCode::Up | KeyCode::BackTab | KeyCode::Char('w') => {
            if !st.wps.is_empty() {
                if st.grab && st.wp_sel > 0 { st.wps.swap(st.wp_sel, st.wp_sel - 1); st.wp_sel -= 1; let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; }
                else { st.wp_sel = (st.wp_sel + st.wps.len() - 1) % st.wps.len(); }
                if let Some(&(la, lo)) = st.wps.get(st.wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny; }
            }
            st.focus = Focus::WaypointList;
        }
        KeyCode::Down | KeyCode::Tab | KeyCode::Char('s') => {
            if !st.wps.is_empty() {
                if st.grab && st.wp_sel + 1 < st.wps.len() { st.wps.swap(st.wp_sel, st.wp_sel + 1); st.wp_sel += 1; let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; }
                else { st.wp_sel = (st.wp_sel + 1) % st.wps.len(); }
                if let Some(&(la, lo)) = st.wps.get(st.wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny; }
            }
            st.focus = Focus::WaypointList;
        }
        KeyCode::Char(' ') => { if !st.wps.is_empty() { st.grab = !st.grab; st.snd.play(if st.grab { "blip" } else { "pop" }); } st.focus = Focus::WaypointList; }
        KeyCode::Char('+') | KeyCode::Char('=') => { if st.z < 19 { st.z += 1; st.cx *= 2.0; st.cy *= 2.0; st.restart_prefetch_on_zoom(); } st.focus = Focus::WaypointList; }
        KeyCode::Char('-') | KeyCode::Char('_') => { if st.z > 2 { st.z -= 1; st.cx /= 2.0; st.cy /= 2.0; st.restart_prefetch_on_zoom(); } st.focus = Focus::WaypointList; }
        KeyCode::Char('[') => { if st.wp_sel > 0 && st.wp_sel < st.wps.len() { st.wps.swap(st.wp_sel, st.wp_sel - 1); st.wp_sel -= 1; let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; if let Some(&(la, lo)) = st.wps.get(st.wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny; } } st.focus = Focus::WaypointList; }
        KeyCode::Char(']') => { if st.wp_sel + 1 < st.wps.len() { st.wps.swap(st.wp_sel, st.wp_sel + 1); st.wp_sel += 1; let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; if let Some(&(la, lo)) = st.wps.get(st.wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny; } } st.focus = Focus::WaypointList; }
        KeyCode::Char('x') => {
            if !st.wps.is_empty() { let i = st.wp_sel.min(st.wps.len() - 1); st.wps.remove(i); if st.wp_sel >= st.wps.len() && st.wp_sel > 0 { st.wp_sel -= 1; } let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; }
            st.grab = false;
            if !st.wps.is_empty() { if let Some(&(la, lo)) = st.wps.get(st.wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny; } st.focus = Focus::WaypointList; } // 空になったら閉じる
        }
        KeyCode::Char('v') => { // 中心に地点を追加し、追加した点を選択(リストは wps から即再生成される)
            st.snd.play("pop");
            wp_add(&mut st.wps, (lat, lon));
            st.wp_sel = st.wps.len().saturating_sub(1);
            st.grab = false;
            let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_;
            st.addr = format!("地点を追加 #{}", st.wps.len());
            st.focus = Focus::WaypointList;
        }
        // 閉じる → Map。左袖(経由地一覧)の残像を残さないよう全消去する(Menu閉じる時と同じ理由)。
        KeyCode::Esc | KeyCode::Enter => { st.grab = false; st.focus = Focus::Map; let _ = write!(out, "\x1b[2J"); st.force_reemit = true; }
        _ => st.focus = Focus::WaypointList,
    }
}

// ルート一覧にフォーカス中: ↑↓で点/操作行を選択、Enterで実行。矢印はパンでなく選択。
pub(crate) fn route_panel(st: &mut UiState, k: KeyEvent, kx: &KeyCtx, out: &mut dyn Write) {
    let KeyCtx { a, lat, lon, nogos: route_nogos, .. } = *kx;
    match k.code {
        KeyCode::Up | KeyCode::Char('w') => {
            st.route_sel = st.route_sel.saturating_sub(1);
            if st.route_sel < st.wps.len() { st.wp_sel = st.route_sel; let (la, lo) = st.wps[st.wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny; }
            st.focus = Focus::RoutePanel;
        }
        KeyCode::Down | KeyCode::Char('s') => {
            let total = st.wps.len() + ROUTE_ACTS.len();
            if st.route_sel + 1 < total { st.route_sel += 1; }
            if st.route_sel < st.wps.len() { st.wp_sel = st.route_sel; let (la, lo) = st.wps[st.wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny; }
            st.focus = Focus::RoutePanel;
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            if st.route_sel >= st.wps.len() { // 操作行を実行(run_action側でfocus遷移する場合あり=その時はそちら優先)
                let ai = st.route_sel - st.wps.len();
                if ai < ROUTE_ACTS.len() { let act = ROUTE_ACTS[ai].1; ui_action::run_action(st, a, act, lat, lon, &route_nogos); }
            } else { // 点を選択中: 地図を寄せてパネルに留まる
                let (la, lo) = st.wps[st.route_sel]; let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny;
                st.focus = Focus::RoutePanel;
            }
        }
        KeyCode::Char('[') => { if st.route_sel < st.wps.len() && st.route_sel > 0 { st.wps.swap(st.route_sel, st.route_sel - 1); st.route_sel -= 1; st.wp_sel = st.route_sel; let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; } st.focus = Focus::RoutePanel; }
        KeyCode::Char(']') => { if st.route_sel + 1 < st.wps.len() { st.wps.swap(st.route_sel, st.route_sel + 1); st.route_sel += 1; st.wp_sel = st.route_sel; let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; } st.focus = Focus::RoutePanel; }
        KeyCode::Char('x') => {
            if st.route_sel < st.wps.len() { st.wps.remove(st.route_sel); if st.route_sel >= st.wps.len() && st.route_sel > 0 { st.route_sel -= 1; } st.wp_sel = st.route_sel.min(st.wps.len().saturating_sub(1)); let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; }
            if !st.wps.is_empty() { st.focus = Focus::RoutePanel; }
            else { // 空になったら地図へ。左袖の残像を残さないよう全消去する
                st.focus = Focus::Map;
                let _ = write!(out, "\x1b[2J");
                st.force_reemit = true;
            }
        }
        KeyCode::Char('v') => { st.snd.play("pop"); wp_add(&mut st.wps, (lat, lon)); let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; st.addr = format!("地点を追加 #{}", st.wps.len()); st.focus = Focus::RoutePanel; }
        KeyCode::Char('+') | KeyCode::Char('=') => { if st.z < 19 { st.z += 1; st.cx *= 2.0; st.cy *= 2.0; st.restart_prefetch_on_zoom(); } st.focus = Focus::RoutePanel; }
        KeyCode::Char('-') | KeyCode::Char('_') => { if st.z > 2 { st.z -= 1; st.cx /= 2.0; st.cy /= 2.0; st.restart_prefetch_on_zoom(); } st.focus = Focus::RoutePanel; }
        // 地図へ戻る。左袖(ルート一覧)の残像を残さないよう全消去する(Menu閉じる時と同じ理由)。
        KeyCode::Esc | KeyCode::Tab => { st.snd.play("back"); st.focus = Focus::Map; let _ = write!(out, "\x1b[2J"); st.force_reemit = true; }
        _ => { st.focus = Focus::RoutePanel; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadseg::RoadSeg;
    use crate::tiles::{Cache, TileLoader};
    use crate::uistate::testing::*;
    use crossterm::event::KeyModifiers;

    // TileLoader はワーカースレッドを起こすのでテスト全体で1つだけ使い回す。
    fn shared_loader() -> &'static TileLoader {
        static L: std::sync::OnceLock<TileLoader> = std::sync::OnceLock::new();
        L.get_or_init(|| TileLoader::start(std::sync::Arc::new(std::sync::Mutex::new(Cache::new()))))
    }

    fn shared_args() -> &'static Args {
        static A: std::sync::OnceLock<Args> = std::sync::OnceLock::new();
        A.get_or_init(test_args)
    }

    // そのフレームの値。画面中心は箱根あたり・地図部分は 640x400px とする。
    fn kctx() -> KeyCtx<'static> {
        KeyCtx { a: shared_args(), loader: shared_loader(), lat: 35.2, lon: 139.0, nogos: "", ow: 640, oh: 400 }
    }

    fn ch(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }
    fn code(c: KeyCode) -> KeyEvent { KeyEvent::new(c, KeyModifiers::NONE) }
    fn shift(c: KeyCode) -> KeyEvent { KeyEvent::new(c, KeyModifiers::SHIFT) }

    // ui_keys::dispatch は focus を Map へ倒してから呼ぶので、テストも同じ前提で始める
    // (「画面を出したままにする」分岐だけが focus を書き戻す)。
    //
    // 経由地が2点以上ある状態で trigger_route() を通る分岐(掴んだままの上下・[ ]の入れ替え)は
    // BRouterへ問い合わせるスレッドを起こすので、テストからは触らない。ルート名の保存も
    // $HOME/.config/termmap/routes/ を書くので触らない。ここで確かめるのは入力の受け付け方・
    // カーソル移動・画面遷移・配列の並べ替え/削除の結果だけ。
    fn base() -> UiState {
        let mut st = test_state();
        st.focus = Focus::Map;
        st
    }

    // 経由地を3点入れた並べ替えビューの状態(掴んでいない)。
    fn with_wps() -> UiState {
        let mut st = base();
        st.wps = vec![(35.0, 139.0), (35.5, 139.5), (36.0, 140.0)];
        st.wp_sel = 0;
        st.grab = false;
        st
    }

    fn capture(f: impl FnOnce(&mut dyn Write)) -> String {
        let mut out: Vec<u8> = Vec::new();
        f(&mut out);
        String::from_utf8_lossy(&out).to_string()
    }

    #[test]
    fn the_distance_gauge_steps_and_clamps() {
        let mut st = base();
        let kx = kctx();
        wander_form(&mut st, code(KeyCode::Left), 30.0, &kx);
        match st.focus { Focus::WanderForm { dist_km } => assert_eq!(dist_km, 25.0, "無印は5km刻み"), _ => panic!("ゲージのまま") }

        st.focus = Focus::Map;
        wander_form(&mut st, shift(KeyCode::Right), 30.0, &kx);
        match st.focus { Focus::WanderForm { dist_km } => assert_eq!(dist_km, 50.0, "Shiftは20km刻み"), _ => panic!("ゲージのまま") }

        st.focus = Focus::Map;
        wander_form(&mut st, code(KeyCode::Left), 12.0, &kx);
        match st.focus { Focus::WanderForm { dist_km } => assert_eq!(dist_km, 10.0, "下限10km"), _ => panic!("ゲージのまま") }

        st.focus = Focus::Map;
        wander_form(&mut st, shift(KeyCode::Right), 195.0, &kx);
        match st.focus { Focus::WanderForm { dist_km } => assert_eq!(dist_km, 200.0, "上限200km"), _ => panic!("ゲージのまま") }
    }

    #[test]
    fn esc_closes_the_distance_gauge_and_other_keys_keep_it() {
        let mut st = base();
        let kx = kctx();
        wander_form(&mut st, ch('z'), 40.0, &kx);
        match st.focus { Focus::WanderForm { dist_km } => assert_eq!(dist_km, 40.0, "関係ないキーでは距離も画面も変えない"), _ => panic!("ゲージのまま") }
        assert!(st.wander_job.is_none());

        st.focus = Focus::Map;
        wander_form(&mut st, code(KeyCode::Esc), 40.0, &kx);
        assert!(matches!(st.focus, Focus::Map));
        assert!(st.wander_job.is_none(), "Escでは検索を始めない");
    }

    #[test]
    fn the_route_name_box_ignores_an_empty_name() {
        let mut st = base();
        save_name(&mut st, code(KeyCode::Enter), "   ".to_string());
        assert!(st.save_confirm.is_none());
        assert!(matches!(st.focus, Focus::Map), "空欄のEnterでは何も起きない");

        st.focus = Focus::Map;
        save_name(&mut st, code(KeyCode::Esc), "箱根周回".to_string());
        assert!(matches!(st.focus, Focus::Map), "Escは入力を捨てて閉じる");
        assert!(st.save_confirm.is_none());
    }

    #[test]
    fn typing_keeps_the_route_name_box_open() {
        let mut st = base();
        save_name(&mut st, ch('箱'), String::new());
        match &st.focus {
            Focus::SaveName(buf) => assert_eq!(buf, "箱"),
            _ => panic!("文字入力中は入力欄のまま"),
        }
        assert_eq!(st.input_cur, 1);
    }

    #[test]
    fn the_favorites_menu_has_two_rows() {
        let mut st = base();
        route_fav_menu(&mut st, code(KeyCode::Up), 0);
        match st.focus { Focus::RouteFavMenu { sel } => assert_eq!(sel, 0, "先頭より上へは行かない"), _ => panic!("メニューのまま") }

        st.focus = Focus::Map;
        route_fav_menu(&mut st, code(KeyCode::Down), 0);
        match st.focus { Focus::RouteFavMenu { sel } => assert_eq!(sel, 1), _ => panic!("メニューのまま") }

        st.focus = Focus::Map;
        route_fav_menu(&mut st, ch('s'), 1);
        match st.focus { Focus::RouteFavMenu { sel } => assert_eq!(sel, 1, "2行しかないので末尾で止まる"), _ => panic!("メニューのまま") }

        st.focus = Focus::Map;
        route_fav_menu(&mut st, code(KeyCode::Esc), 1);
        assert!(matches!(st.focus, Focus::Map), "Escで地図へ戻る");
    }

    #[test]
    fn enter_on_the_save_row_opens_the_name_box_with_the_last_name() {
        let mut st = base();
        st.route_name_hint = "箱根周回".to_string();
        route_fav_menu(&mut st, code(KeyCode::Enter), 0);
        match &st.focus {
            Focus::SaveName(buf) => assert_eq!(buf, "箱根周回", "前回の名前をそのまま出す"),
            _ => panic!("保存行のEnterは名前入力へ"),
        }
        assert_eq!(st.input_cur, 4, "カーソルは末尾");
    }

    #[test]
    fn enter_on_the_load_row_reads_the_saved_names() {
        let mut st = base();
        st.rn_sel = 5;
        route_fav_menu(&mut st, code(KeyCode::Enter), 1);
        assert_eq!(st.route_names, list_named_routes(), "保存済みの一覧を読み直す");
        assert_eq!(st.rn_sel, 0, "選択は先頭から");
        if st.route_names.is_empty() {
            assert_eq!(st.addr, "お気に入り無し");
            assert!(matches!(st.focus, Focus::Map), "1件も無ければ開かない");
        } else {
            assert!(matches!(st.focus, Focus::RouteList));
        }
    }

    #[test]
    fn the_saved_route_list_moves_the_cursor_and_closes_on_esc() {
        let mut st = base();
        st.route_names = vec!["A".to_string(), "B".to_string()];
        route_list(&mut st, code(KeyCode::Down), "");
        assert_eq!(st.rn_sel, 1);

        st.focus = Focus::Map;
        route_list(&mut st, ch('s'), "");
        assert_eq!(st.rn_sel, 1, "末尾より下へは行かない");
        assert!(matches!(st.focus, Focus::RouteList), "移動だけなら一覧のまま");

        st.focus = Focus::Map;
        route_list(&mut st, code(KeyCode::Up), "");
        assert_eq!(st.rn_sel, 0);

        st.focus = Focus::Map;
        route_list(&mut st, code(KeyCode::Esc), "");
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn enter_on_a_route_that_cannot_be_read_does_nothing() {
        let mut st = base();
        // 実在しない名前(読めなければ何も起きず、一覧は閉じる=既存の挙動)。
        st.route_names = vec!["termmap-test-存在しないルート".to_string()];
        route_list(&mut st, code(KeyCode::Enter), "");
        assert!(st.wps.is_empty());
        assert!(st.route_job.is_none(), "経路計算は始まらない");
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn the_road_list_moves_the_cursor() {
        let mut st = base();
        st.road_segs = vec![
            RoadSeg { name: "国道1号".into(), color: [1, 2, 3], pts: vec![(35.0, 139.0)] },
            RoadSeg { name: "県道".into(), color: [4, 5, 6], pts: vec![(35.1, 139.1)] },
        ];
        let o = capture(|out| road_list(&mut st, code(KeyCode::Down), out));
        assert_eq!(st.road_sel, 1);
        assert!(o.is_empty(), "移動では画面を消さない");

        st.focus = Focus::Map;
        capture(|out| road_list(&mut st, ch('s'), out));
        assert_eq!(st.road_sel, 1, "末尾より下へは行かない");
        assert!(matches!(st.focus, Focus::RoadList));

        st.focus = Focus::Map;
        capture(|out| road_list(&mut st, code(KeyCode::Up), out));
        assert_eq!(st.road_sel, 0);
    }

    #[test]
    fn x_deletes_the_selected_road_and_keeps_the_list() {
        let mut st = base();
        st.road_segs = vec![
            RoadSeg { name: "国道1号".into(), color: [1, 2, 3], pts: vec![(35.0, 139.0)] },
            RoadSeg { name: "県道".into(), color: [4, 5, 6], pts: vec![(35.1, 139.1)] },
        ];
        st.road_sel = 1;
        capture(|out| road_list(&mut st, ch('x'), out));
        assert_eq!(st.road_segs.len(), 1);
        assert_eq!(st.road_segs[0].name, "国道1号");
        assert_eq!(st.road_sel, 0, "末尾を消したら1つ上へ寄せる");
        assert_eq!(st.spec.roads.len(), 1, "描画用の道路レイヤも作り直す");
        assert!(matches!(st.focus, Focus::RoadList), "まだ残っていれば一覧のまま");
    }

    #[test]
    fn deleting_the_last_road_closes_the_list() {
        let mut st = base();
        st.road_segs = vec![RoadSeg { name: "国道1号".into(), color: [1, 2, 3], pts: vec![(35.0, 139.0)] }];
        let o = capture(|out| road_list(&mut st, ch('x'), out));
        assert!(st.road_segs.is_empty());
        assert!(st.spec.roads.is_empty());
        assert_eq!(st.addr, "道路を全削除");
        assert!(matches!(st.focus, Focus::Map));
        assert_eq!(o, "\x1b[2J", "左袖の残像を消す");
        assert!(st.force_reemit);
    }

    #[test]
    fn esc_closes_the_road_list_and_clears_the_left_gutter() {
        let mut st = base();
        st.road_segs = vec![RoadSeg { name: "国道1号".into(), color: [1, 2, 3], pts: vec![(35.0, 139.0)] }];
        let o = capture(|out| road_list(&mut st, code(KeyCode::Esc), out));
        assert_eq!(st.road_segs.len(), 1, "Escでは消さない");
        assert!(matches!(st.focus, Focus::Map));
        assert_eq!(o, "\x1b[2J");
        assert!(st.force_reemit);
    }

    #[test]
    fn the_waypoint_list_wraps_the_cursor_and_follows_the_map() {
        let mut st = with_wps();
        let kx = kctx();
        capture(|out| waypoint_list(&mut st, code(KeyCode::Down), &kx, out));
        assert_eq!(st.wp_sel, 1);
        let (ex, ey) = deg_to_pixel(35.5, 139.5, st.z);
        assert_eq!((st.cx, st.cy), (ex, ey), "選択に地図が追従する");
        assert!(matches!(st.focus, Focus::WaypointList));

        st.focus = Focus::Map;
        st.wp_sel = 0;
        capture(|out| waypoint_list(&mut st, code(KeyCode::Up), &kx, out));
        assert_eq!(st.wp_sel, 2, "先頭で上へ行くと末尾へ回り込む");
        assert!(st.route_job.is_none(), "掴んでいなければ経路は引き直さない");
    }

    #[test]
    fn space_grabs_and_releases_the_selected_waypoint() {
        let mut st = with_wps();
        let kx = kctx();
        capture(|out| waypoint_list(&mut st, ch(' '), &kx, out));
        assert!(st.grab, "Spaceで掴む");

        st.focus = Focus::Map;
        capture(|out| waypoint_list(&mut st, ch(' '), &kx, out));
        assert!(!st.grab, "もう一度Spaceで置く");

        let mut empty = base();
        empty.wps.clear();
        capture(|out| waypoint_list(&mut empty, ch(' '), &kx, out));
        assert!(!empty.grab, "地点が無ければ掴めない");
    }

    #[test]
    fn the_zoom_keys_scale_the_center_in_the_waypoint_list() {
        let mut st = with_wps();
        let kx = kctx();
        st.cx = 1000.0;
        st.cy = 2000.0;
        capture(|out| waypoint_list(&mut st, ch('+'), &kx, out));
        assert_eq!((st.z, st.cx, st.cy), (15, 2000.0, 4000.0));

        st.focus = Focus::Map;
        capture(|out| waypoint_list(&mut st, ch('-'), &kx, out));
        assert_eq!((st.z, st.cx, st.cy), (14, 1000.0, 2000.0));
        assert!(matches!(st.focus, Focus::WaypointList));
    }

    #[test]
    fn v_adds_the_center_as_a_waypoint() {
        let mut st = base();
        let kx = kctx();
        capture(|out| waypoint_list(&mut st, ch('v'), &kx, out));
        assert_eq!(st.wps, vec![(35.2, 139.0)], "画面中心を足す");
        assert_eq!(st.wp_sel, 0, "足した点を選ぶ");
        assert!(!st.grab);
        assert_eq!(st.addr, "地点を追加 #1");
        assert!(st.route_job.is_none(), "1点だけならまだ経路は引かない");
        assert!(matches!(st.focus, Focus::WaypointList));
    }

    #[test]
    fn deleting_the_last_waypoint_closes_the_list() {
        let mut st = base();
        let kx = kctx();
        st.wps = vec![(35.0, 139.0)];
        st.grab = true;
        capture(|out| waypoint_list(&mut st, ch('x'), &kx, out));
        assert!(st.wps.is_empty());
        assert!(!st.grab, "掴んだままにしない");
        assert!(matches!(st.focus, Focus::Map), "空になったら閉じる");
    }

    #[test]
    fn esc_closes_the_waypoint_list_and_clears_the_left_gutter() {
        let mut st = with_wps();
        let kx = kctx();
        st.grab = true;
        let o = capture(|out| waypoint_list(&mut st, code(KeyCode::Esc), &kx, out));
        assert!(!st.grab);
        assert_eq!(st.wps.len(), 3, "閉じるだけで点は触らない");
        assert!(matches!(st.focus, Focus::Map));
        assert_eq!(o, "\x1b[2J");
        assert!(st.force_reemit);
    }

    #[test]
    fn the_route_panel_selection_follows_the_map_and_stops_at_the_last_row() {
        let mut st = base();
        let kx = kctx();
        st.wps = vec![(35.0, 139.0), (35.5, 139.5)];
        capture(|out| route_panel(&mut st, code(KeyCode::Down), &kx, out));
        assert_eq!(st.route_sel, 1);
        assert_eq!(st.wp_sel, 1, "点の行なら経由地の選択も合わせる");
        let (ex, ey) = deg_to_pixel(35.5, 139.5, st.z);
        assert_eq!((st.cx, st.cy), (ex, ey));

        let total = st.wps.len() + ROUTE_ACTS.len();
        for _ in 0..total + 3 {
            st.focus = Focus::Map;
            capture(|out| route_panel(&mut st, ch('s'), &kx, out));
        }
        assert_eq!(st.route_sel, total - 1, "操作行の末尾で止まる");
        assert_eq!(st.wp_sel, 1, "操作行では経由地の選択は動かさない");

        st.focus = Focus::Map;
        capture(|out| route_panel(&mut st, code(KeyCode::Up), &kx, out));
        assert_eq!(st.route_sel, total - 2);
        assert!(matches!(st.focus, Focus::RoutePanel));
    }

    #[test]
    fn enter_on_a_waypoint_row_centers_and_keeps_the_panel() {
        let mut st = base();
        let kx = kctx();
        st.wps = vec![(35.0, 139.0), (35.5, 139.5)];
        st.route_sel = 1;
        st.cx = 0.0;
        st.cy = 0.0;
        capture(|out| route_panel(&mut st, code(KeyCode::Enter), &kx, out));
        let (ex, ey) = deg_to_pixel(35.5, 139.5, st.z);
        assert_eq!((st.cx, st.cy), (ex, ey));
        assert!(matches!(st.focus, Focus::RoutePanel), "パネルに留まる");
    }

    #[test]
    fn v_adds_the_center_to_an_empty_route() {
        let mut st = base();
        let kx = kctx();
        capture(|out| route_panel(&mut st, ch('v'), &kx, out));
        assert_eq!(st.wps, vec![(35.2, 139.0)]);
        assert_eq!(st.addr, "地点を追加 #1");
        assert!(st.route_job.is_none(), "1点だけならまだ経路は引かない");
        assert!(matches!(st.focus, Focus::RoutePanel));
    }

    #[test]
    fn deleting_the_last_waypoint_closes_the_route_panel() {
        let mut st = base();
        let kx = kctx();
        st.wps = vec![(35.0, 139.0)];
        st.route_sel = 0;
        let o = capture(|out| route_panel(&mut st, ch('x'), &kx, out));
        assert!(st.wps.is_empty());
        assert!(matches!(st.focus, Focus::Map));
        assert_eq!(o, "\x1b[2J", "左袖の残像を消す");
        assert!(st.force_reemit);
    }

    #[test]
    fn esc_closes_the_route_panel_and_an_unknown_key_keeps_it() {
        let mut st = base();
        let kx = kctx();
        st.wps = vec![(35.0, 139.0)];
        capture(|out| route_panel(&mut st, ch('Z'), &kx, out));
        assert!(matches!(st.focus, Focus::RoutePanel), "知らないキーでは何もしない");

        st.focus = Focus::Map;
        let o = capture(|out| route_panel(&mut st, code(KeyCode::Esc), &kx, out));
        assert!(matches!(st.focus, Focus::Map));
        assert_eq!(o, "\x1b[2J");
        assert!(st.force_reemit);
        assert_eq!(st.wps.len(), 1, "閉じるだけで点は触らない");
    }
}
