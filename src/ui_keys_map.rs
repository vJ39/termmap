// 地図そのものと Space メニューのキー処理。ui_keys.rs の Focus 分岐から関心ごとに切り出した1つ。
//
// 地図(Focus::Map)はキーの数が一番多い画面で、パン・ズーム・各レイヤの表示切替・ルート編集の
// 直打ちが並ぶ。メニュー(Focus::Menu)はその同じ操作をラベル付きで選べるようにしたもので、
// 実処理はどちらも ui_action::run_action へ寄せてある。
//
// map() の戻り値は「対話ループを抜けるか(=アプリ終了)」。q だけが true を返す。ループを持って
// いるのは ui.rs 側なので、break を関数の中に隠さずここから返す。

use crate::focus::Focus;
use crate::geo::*;
use crate::menu::{MenuAction, MENU_CATEGORIES, MenuLevel, menu_action_for_key, ROUTE_ACTS};
use crate::route::*;
use crate::share::*;
use crate::spots::*;
use crate::ui_helpers::*;
use crate::ui_keys::KeyCtx;
use crate::uistate::UiState;
use crate::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::io::Write;

// Space メニュー・トップ(カテゴリ選択)。文字キーは全カテゴリ横断で直接実行できる。
pub(crate) fn menu_categories(st: &mut UiState, k: KeyEvent, kx: &KeyCtx, out: &mut dyn Write) {
    // 分岐の中身は ui.rs から動かしていないので、フレームの値はもとと同じ名前で受け取る。
    let KeyCtx { a, lat, lon, nogos: route_nogos, .. } = *kx;
    match k.code {
        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.menu_cat_sel = st.menu_cat_sel.saturating_sub(1); st.focus = Focus::Menu(MenuLevel::Categories); }
        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); if st.menu_cat_sel + 1 < MENU_CATEGORIES.len() { st.menu_cat_sel += 1; } st.focus = Focus::Menu(MenuLevel::Categories); }
        KeyCode::Enter => { st.snd.play("click"); st.menu_item_sel = 0; st.focus = Focus::Menu(MenuLevel::Items(st.menu_cat_sel)); }
        // メニューを閉じる → Map。左袖(カテゴリ一覧)はマップとは別の列に描かれており、
        // 通常のマップ再描画では上書きされない列が残ることがあるため、全消去してから
        // 次フレームで確実に再構築させる(Resize時の扱いと同じ)。
        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Map; let _ = write!(out, "\x1b[2J"); st.force_reemit = true; }
        KeyCode::Char(c) => match menu_action_for_key(c) {
            Some(act) => ui_action::run_action(st, a, act, lat, lon, &route_nogos),
            None => st.focus = Focus::Menu(MenuLevel::Categories),
        },
        _ => st.focus = Focus::Menu(MenuLevel::Categories),
    }
}

// Space メニュー・展開(項目選択)。キーはそのカテゴリ内だけ有効(スコープ限定)。
pub(crate) fn menu_items(st: &mut UiState, k: KeyEvent, ci: usize, kx: &KeyCtx) {
    let KeyCtx { a, lat, lon, nogos: route_nogos, .. } = *kx;
    let items = MENU_CATEGORIES[ci].items;
    match k.code {
        KeyCode::Up | KeyCode::Char('w') if !items.iter().any(|it| it.key == 'w') => { st.snd.play("click"); st.menu_item_sel = st.menu_item_sel.saturating_sub(1); st.focus = Focus::Menu(MenuLevel::Items(ci)); }
        KeyCode::Down | KeyCode::Char('s') if !items.iter().any(|it| it.key == 's') => { st.snd.play("click"); if st.menu_item_sel + 1 < items.len() { st.menu_item_sel += 1; } st.focus = Focus::Menu(MenuLevel::Items(ci)); }
        // 選択中の項目を先に取り出す(&mut st を渡す式の中で st を読めないため)
        KeyCode::Enter => { let act = items[st.menu_item_sel].action; ui_action::run_action(st, a, act, lat, lon, &route_nogos); }
        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Menu(MenuLevel::Categories); } // 上位カテゴリへ戻る
        KeyCode::Char(c) => match items.iter().find(|it| it.key == c) {
            Some(it) => ui_action::run_action(st, a, it.action, lat, lon, &route_nogos),
            None => st.focus = Focus::Menu(MenuLevel::Items(ci)),
        },
        _ => st.focus = Focus::Menu(MenuLevel::Items(ci)),
    }
}

pub(crate) fn map(st: &mut UiState, k: KeyEvent, kx: &KeyCtx, out: &mut dyn Write) -> bool {
    // 分岐の中身は ui.rs から動かしていないので、フレームの値はもとと同じ名前で受け取る。
    let KeyCtx { a, lat, lon, nogos: route_nogos, ow, oh, .. } = *kx;
    // Shift+矢印/大文字HJKL=常に高速(固定)。無印(矢印/小文字hjkl)=既定は細かい1歩で、
    // 同方向を短間隔(220ms以内)で押し続ける/連打するほど徐々に加速し、上限は高速の
    // 手前まで。方向転換や間隔が空くと streak がリセットされ、また細かい1歩に戻る。
    // hjklは矢印と全く同じ挙動モデル(大文字/小文字がShiftの有無に対応)。大文字は
    // 修飾キーの拡張シーケンスに依存しない普通の文字なので、端末がShift+矢印の拡張
    // CSIを送れない場合(iSH等)でも常時高速パンが確実に効く。
    let is_pan = matches!(k.code, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
        | KeyCode::Char('h') | KeyCode::Char('j') | KeyCode::Char('k') | KeyCode::Char('l')
        | KeyCode::Char('H') | KeyCode::Char('J') | KeyCode::Char('K') | KeyCode::Char('L'));
    if is_pan {
        if st.last_pan_dir == Some(k.code) && st.last_pan_at.elapsed() < std::time::Duration::from_millis(220) {
            st.pan_streak = (st.pan_streak + 1).min(20);
        } else {
            st.pan_streak = 0;
        }
        st.last_pan_dir = Some(k.code);
        st.last_pan_at = std::time::Instant::now();
    }
    let fine = oh as f64 / 64.0;
    let fast = oh as f64 / 4.0;
    let is_fast_key = k.modifiers.contains(KeyModifiers::SHIFT)
        || matches!(k.code, KeyCode::Char('H') | KeyCode::Char('J') | KeyCode::Char('K') | KeyCode::Char('L'));
    let step = if is_fast_key {
        fast
    } else {
        (fine * (1.0 + st.pan_streak as f64 * 0.35)).min(fast)
    }.max(1.0);
    let mut quit = false;
    match k.code {
        KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('H') => { st.cx -= step; st.addr.clear(); }
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('L') => { st.cx += step; st.addr.clear(); }
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => { st.cy -= step; st.addr.clear(); }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => { st.cy += step; st.addr.clear(); }
        KeyCode::Char('+') | KeyCode::Char('=') => if st.z < 19 { st.z += 1; st.cx *= 2.0; st.cy *= 2.0; st.addr.clear(); st.restart_prefetch_on_zoom(); },
        KeyCode::Char('-') | KeyCode::Char('_') => if st.z > 2 { st.z -= 1; st.cx /= 2.0; st.cy /= 2.0; st.addr.clear(); st.restart_prefetch_on_zoom(); },
        KeyCode::Enter if !st.wps.is_empty() && st.route_sel >= st.wps.len() && st.route_sel < st.wps.len() + ROUTE_ACTS.len() => {
            // w/sで操作行(保存/GPX等)を選択中はEnterでその操作を実行
            let ai = st.route_sel - st.wps.len();
            let act = ROUTE_ACTS[ai].1;
            ui_action::run_action(st, a, act, lat, lon, &route_nogos);
        }
        KeyCode::Enter => { // 中心付近の最寄りお気に入りにスナップ＋名前表示
            let mut best: Option<(f64, usize)> = None;
            for (i, s) in st.spots.iter().enumerate() {
                let (gx, gy) = deg_to_pixel(s.lat, s.lon, st.z);
                let dpx = ((gx - st.cx).powi(2) + (gy - st.cy).powi(2)).sqrt();
                if best.map_or(true, |(bd, _)| dpx < bd) { best = Some((dpx, i)); }
            }
            match best {
                Some((dpx, i)) if dpx <= (ow.min(oh) as f64) * 0.25 => {
                    let s = &st.spots[i];
                    let (nx, ny) = deg_to_pixel(s.lat, s.lon, st.z); st.cx = nx; st.cy = ny;
                    st.popup = Some(if s.name.is_empty() { "★ (無名スポット)".into() } else { format!("★ {} [{}]", s.name, s.cat) });
                }
                Some(_) => st.addr = "近くにお気に入り無し".into(),
                None => st.addr = "お気に入り未登録".into(),
            }
        }
        KeyCode::Char('a') => st.addr = reverse_geocode(lat, lon).unwrap_or_else(|e| format!("({e})")),
        KeyCode::Char('/') => { st.input_cur = 0; st.focus = Focus::Search(String::new()); }
        KeyCode::Char('f') => st.focus = Focus::PoiMenu,
        KeyCode::Char('S') => { st.focus = Focus::RouteFavMenu { sel: 0 }; } // お気に入りルート: 保存/呼び出しの小メニュー
        KeyCode::Char('v') => { // 地図中心に地点を追加(末尾)。役割は並び順で自動(先頭=始点/末尾=終点)
            st.snd.play("pop"); wp_add(&mut st.wps, (lat, lon));
            st.wp_sel = st.wps.len() - 1; st.route_sel = st.wp_sel; // 追加した点を選択状態にする(左袖のハイライトが追従)
            let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_;
            st.addr = format!("地点を追加 #{}", st.wps.len());
        }
        // w/s: Tabで一覧へ入らなくても、地図(パン)はそのまま左袖(ルート点+操作行)の
        // 選択だけ上下できる。操作行(保存/GPX等)まで選べて、Enterでそのまま実行できる
        KeyCode::Char('w') if !st.wps.is_empty() => {
            let total = st.wps.len() + ROUTE_ACTS.len();
            st.route_sel = (st.route_sel + total - 1) % total;
            if st.route_sel < st.wps.len() {
                st.wp_sel = st.route_sel;
                let (la, lo) = st.wps[st.wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny;
            }
        }
        KeyCode::Char('s') if !st.wps.is_empty() => {
            let total = st.wps.len() + ROUTE_ACTS.len();
            st.route_sel = (st.route_sel + 1) % total;
            if st.route_sel < st.wps.len() {
                st.wp_sel = st.route_sel;
                let (la, lo) = st.wps[st.wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny;
            }
        }
        KeyCode::Tab | KeyCode::BackTab => { if !st.wps.is_empty() { st.route_sel = st.route_sel.min(st.wps.len() + ROUTE_ACTS.len() - 1); st.focus = Focus::RoutePanel; } } // 左のルート一覧にフォーカス(そこで↑↓選択・Enter実行)
        KeyCode::Char(' ') => { st.snd.play("click"); st.menu_cat_sel = 0; st.focus = Focus::Menu(MenuLevel::Categories); } // Space=メニュー(カテゴリ→展開の2階層)
        KeyCode::Char('?') => { st.help = true; st.help_page = 0; }
        KeyCode::Char('P') => { st.cat_sel = 0; st.focus = Focus::SpotCatList; } // マイスポット(カテゴリ一覧)
        KeyCode::Char(',') => { st.set_sel = 0; st.focus = Focus::Settings; voice::warm_voice_list(); } // 設定画面
        KeyCode::Char('r') => { st.input_cur = 0; st.focus = Focus::RoadSearch(String::new()); } // 道路名でルート(現在view内)
        KeyCode::Char('@') => { // おすすめツーリングスポット提案(claude -p)
            if !st.cfg.llm_recommend_enabled { st.snd.play("error"); st.addr = "おすすめ: 設定でOFF(,でON)".into(); }
            else if !recommend::claude_available(&st.cfg.llm_command) { st.snd.play("error"); st.addr = "おすすめ: claudeが無い(設定のLLM/コマンド確認)".into(); }
            else { st.input_cur = 0; st.focus = Focus::Recommend(String::new()); }
        }
        KeyCode::Char('V') => { st.show_spots = !st.show_spots; apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots); st.addr = if st.show_spots { "マイスポット表示".into() } else { "マイスポット非表示".into() }; }
        // ルート一覧(左袖)の表示切替。ルート自体(wps)は消さない。狙いは
        // 画面が狭い端末で「ルートがある間ずっと出っぱなし」を隠せるようにすること。
        // 左袖はマップ本体の再描画では上書きされない列に描かれているため、隠す方向の
        // 切替では全消去してから次フレームで再構築させる(Menu閉じる時と同じ理由)。
        KeyCode::Char('R') => {
            st.route_panel_hidden = !st.route_panel_hidden;
            st.addr = if st.route_panel_hidden { "ルート一覧: 非表示".into() } else { "ルート一覧: 表示".into() };
            if st.route_panel_hidden { let _ = write!(out, "\x1b[2J"); }
            st.force_reemit = true;
        }
        KeyCode::Char('E') => { // 標高プロファイルの表示/非表示
            st.show_elev = !st.show_elev;
            if st.show_elev && (st.spec.routes.is_empty() || !st.route_ele.iter().any(|&z| z != 0.0)) { st.addr = "標高: ルート確定後に表示".into(); }
        }
        KeyCode::Char('C') => { st.radar_toggle(); } // 雨雲レーダー(気象庁ナウキャスト)の表示/非表示。Spaceメニュー・設定画面と共通処理
        KeyCode::Char('>') => { // 表示時刻を未来へ1コマ(OFFなら発見しやすさのためONにする)
            if !st.radar_on {
                st.radar_turn_on();
            } else if !st.radar_tl.is_empty() {
                st.radar_idx = (st.radar_idx + 1).min(st.radar_tl.frames.len() - 1); // 折り返さない
                // 「現在」ちょうどに戻ったら追従モードへ復帰、それより未来なら外れる。
                if st.radar_idx == st.radar_tl.now_idx { st.radar_follow = true; }
                else if st.radar_idx > st.radar_tl.now_idx { st.radar_follow = false; }
                st.addr = format!("雨雲 {}", radar::frame_label(&st.radar_tl, st.radar_idx));
            }
        }
        KeyCode::Char('<') => { // 表示時刻を過去へ1コマ(OFFのときは何もしない=誤爆で勝手にONにしない)
            if st.radar_on && !st.radar_tl.is_empty() {
                st.radar_idx = st.radar_idx.saturating_sub(1);
                st.radar_follow = false;
                st.addr = format!("雨雲 {}", radar::frame_label(&st.radar_tl, st.radar_idx));
            }
        }
        KeyCode::Char('A') => ui_action::run_action(st, a, MenuAction::PlayRoute, lat, lon, &route_nogos),
        KeyCode::Char('G') => { // ライブ現在地(ブレッドクラム)の ON/OFF
            if st.gps_rx.is_some() { st.gps_rx = None; st.addr = "ライブ現在地: OFF".into(); }
            else {
                let bin = if std::path::Path::new("/opt/homebrew/bin/CoreLocationCLI").exists() { "/opt/homebrew/bin/CoreLocationCLI" } else { "CoreLocationCLI" };
                if gpslive::available(bin) { st.gps_rx = Some(gpslive::start_poller(bin.to_string(), 5)); st.gps_trail.clear(); st.gps_pos = None; st.addr = "ライブ現在地: ON(5秒ごと)".into(); }
                else { st.addr = "ライブ: CoreLocationCLI無し(brew install corelocationcli)".into(); }
            }
        }
        KeyCode::Char('i') => { // 実写(Street View)を中心地点で開く
            if !st.cfg.streetview_enabled { st.snd.play("error"); st.addr = "実写: OFF(設定で有効化)".into(); }
            else if !streetview::available(&st.cfg.google_maps_api_key) { st.snd.play("error"); st.addr = "実写: Google APIキー未設定([google] maps_api_key)".into(); }
            else {
                // 実写取得を別スレッドへ(focusはMapのまま=スピナーが回る)
                st.sv_fov = 90.0; // 開き直しなので既定ズームに戻す
                let (la, lo) = (lat, lon);
                let key = st.cfg.google_maps_api_key.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let r = streetview::fetch(la, lo, 0, 640, 480, 90.0, &key);
                    let _ = tx.send((la, lo, 0, r));
                });
                st.street_job = Some(rx);
            }
        }
        KeyCode::Char('I') => { // 実画像モード(iTerm2インライン画像)の ON/OFF
            st.cfg.image_mode = !st.cfg.image_mode;
            st.force_reemit = true; // 切替直後は必ず描き直す
            st.addr = if st.cfg.image_mode {
                if image_capable() { "実画像モード: ON".into() } else { "実画像モード: ON(この端末は非対応・AA継続)".into() }
            } else { "実画像モード: OFF".into() };
        }
        // キー選定: C/K/L/V/P/I等の自然な字は全て他機能で使用済みのため空いている'N'を割当
        KeyCode::Char('N') => ui_action::run_action(st, a, MenuAction::ViewCamera, lat, lon, &route_nogos),
        // 過去災害: 中心に一番近い地点の事例一覧を中央パネルへ(防災のB)。
        KeyCode::Char('B') => {
            if !st.cfg.disaster_enabled { st.snd.play("error"); st.addr = "過去災害: OFF(設定で有効化)".into(); }
            else {
                // 視野内で中心に一番近い地点。カメラのNと同じく、フレーム先頭で
                // 切り出した一覧の借用はここ(tick後)まで生きられないので層から直接引く。
                let nearest = st.disaster_layer.items(plotlayer::view_bbox(st.cx, st.cy, st.z)).into_iter()
                    .min_by(|a, b| {
                        let da = (a.lat - lat).powi(2) + (a.lon - lon).powi(2);
                        let db = (b.lat - lat).powi(2) + (b.lon - lon).powi(2);
                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .cloned();
                match nearest {
                    None => { st.snd.play("error"); st.addr = "過去災害: 周辺に記録無し".into(); }
                    Some(s) => {
                        // 事例本体(名称・日付・被害統計)は集計に入っていないので、
                        // ここで初めて取りに行く。保存はしない(押したときだけ)。
                        let since = plotlayer::disaster_since();
                        let (tx, rx) = std::sync::mpsc::channel();
                        std::thread::spawn(move || {
                            let r = disaster::fetch_events(s.lat, s.lon, since, disaster::EVENT_LIMIT)
                                .map(|evs| disaster::panel_content(&evs, &s, since));
                            let _ = tx.send(r);
                        });
                        st.disaster_job = Some(rx);
                        st.addr = "🌊災害事例を取得中…".into();
                    }
                }
            }
        }
        // 通行規制の詳細(なぜ通れないか): 中心に一番近い区間の規制原因を中央パネルへ。
        KeyCode::Char('T') => {
            if !st.cfg.regulation_enabled { st.snd.play("error"); st.addr = "通行規制: OFF(設定で有効化)".into(); }
            else {
                // B/Nと同じく、フレーム先頭で切り出した一覧の借用はここまで生きられないので層から直接引く。
                let nearest = st.regulation_layer.items(plotlayer::view_bbox(st.cx, st.cy, st.z)).into_iter()
                    .filter(|ev| !ev.detail_id.is_empty())
                    .min_by(|a, b| {
                        let da = a.line.iter().map(|&p| (p.0 - lat).powi(2) + (p.1 - lon).powi(2)).fold(f64::INFINITY, f64::min);
                        let db = b.line.iter().map(|&p| (p.0 - lat).powi(2) + (p.1 - lon).powi(2)).fold(f64::INFINITY, f64::min);
                        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                    });
                match nearest {
                    None => { st.snd.play("error"); st.addr = "通行規制: 周辺に詳細あり区間無し".into(); }
                    Some(ev) => {
                        let id = ev.detail_id.clone();
                        let (tx, rx) = std::sync::mpsc::channel();
                        std::thread::spawn(move || { let _ = tx.send(regulation::fetch_detail(&id)); });
                        st.regulation_detail_job = Some(rx);
                        st.addr = "🚧規制詳細を取得中…".into();
                    }
                }
            }
        }
        KeyCode::Char('n') => { // BRouter の代替ルート候補を巡回
            if st.wps.len() >= 2 {
                st.route_alt = (st.route_alt + 1) % 4;
                let (nn, jj) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, st.route_alt, &st.cfg.google_maps_api_key, &route_nogos);
                st.route_note = nn; st.route_job = jj;
            } else { st.snd.play("error"); st.addr = "ルート未確定".into(); }
        }
        KeyCode::Char('W') => { st.focus = Focus::WanderForm { dist_km: a.dist.unwrap_or(40.0) }; } // 走りまくり: 距離ゲージを開く
        KeyCode::Char('o') => { // スマホ共有(GoogleマップQR)
            if st.wps.len() >= 2 {
                let (url, _) = gmaps_url(&st.wps);
                match qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L) {
                    Ok(c) => st.qr_view = Some(build_qr_view(&c, &st.cfg.qr_style)),
                    Err(_) => st.addr = "QR生成失敗".into(),
                }
            } else { st.snd.play("error"); st.addr = "ルート未確定".into(); }
        }
        KeyCode::Char('x') => { wp_remove(&mut st.wps, &mut st.wp_sel); st.route_sel = st.wp_sel; { let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; } }
        KeyCode::Char('[') => { if st.play.is_some() { st.play_speed = (st.play_speed / 1.5).max(0.1); st.play_speed_bits.store(st.play_speed.to_bits(), std::sync::atomic::Ordering::Relaxed); st.addr = format!("再生速度 {:.2}x", st.play_speed); } else { wp_swap(&mut st.wps, &mut st.wp_sel, true); st.route_sel = st.wp_sel; { let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; } } }
        KeyCode::Char(']') => { if st.play.is_some() { st.play_speed = (st.play_speed * 1.5).min(32.0); st.play_speed_bits.store(st.play_speed.to_bits(), std::sync::atomic::Ordering::Relaxed); st.addr = format!("再生速度 {:.2}x", st.play_speed); } else { wp_swap(&mut st.wps, &mut st.wp_sel, false); st.route_sel = st.wp_sel; { let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; } } }
        KeyCode::Char('m') => { st.mode = match mode_label(&st.mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); { let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; } }
        KeyCode::Char('c') => ui_action::run_action(st, a, MenuAction::ClearRoute, lat, lon, &route_nogos),
        KeyCode::Char('g') => match st.spec.routes.last() {
            Some(rt) => st.addr = match write_gpx("termmap-route.gpx", &rt.pts) { Ok(_) => "GPX保存: termmap-route.gpx".into(), Err(e) => format!("({e})") },
            None => { st.snd.play("error"); st.addr = "ルート未確定".into(); }
        },
        KeyCode::Char('q') => quit = true, // qは確認なしで即終了
        KeyCode::Esc => { // Escを600ms以内に2回押すと終了確認を出す(誤爆防止)
            if st.last_esc_at.map_or(false, |t| t.elapsed() < std::time::Duration::from_millis(600)) {
                st.quit_confirm = true;
                st.last_esc_at = None;
            } else {
                st.last_esc_at = Some(std::time::Instant::now());
                st.addr = "もう一度Escで終了確認".into();
            }
        }
        _ => {}
    }
    if quit { return true; }
    let n = (TILE as f64) * 2f64.powi(st.z as i32);
    if st.cx < 0.0 { st.cx += n; } else if st.cx >= n { st.cx -= n; }
    st.cy = st.cy.clamp(0.0, n - 1.0);
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::{Cache, TileLoader};
    use crate::uistate::testing::*;

    // TileLoader はワーカースレッドを起こすのでテスト全体で1つだけ使い回す
    // (この画面は地図種別を変えないので実際には触られない)。
    fn shared_loader() -> &'static TileLoader {
        static L: std::sync::OnceLock<TileLoader> = std::sync::OnceLock::new();
        L.get_or_init(|| TileLoader::start(std::sync::Arc::new(std::sync::Mutex::new(Cache::new()))))
    }

    fn shared_args() -> &'static Args {
        static A: std::sync::OnceLock<Args> = std::sync::OnceLock::new();
        A.get_or_init(test_args)
    }

    // そのフレームの値。地図部分は 640x320px なので、細かい1歩=5px(oh/64)・高速=80px(oh/4)、
    // お気に入りスナップの当たり範囲は min(ow,oh)*0.25=80px になる。
    fn kctx() -> KeyCtx<'static> {
        KeyCtx { a: shared_args(), loader: shared_loader(), lat: 35.0, lon: 139.0, nogos: "", ow: 640, oh: 320 }
    }

    fn ch(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }
    fn code(c: KeyCode) -> KeyEvent { KeyEvent::new(c, KeyModifiers::NONE) }

    // ui_keys::dispatch は focus を Map へ倒してから呼ぶので、テストも同じ前提で始める。
    //
    // 通信・外部コマンド・ディスクへ行く分岐(a=住所/C・>=雨雲/G=現在地/N=カメラ/n・o の
    // 2点以上でのルート再計算/@ をONにした場合/, の設定画面)は触らない。ここで確かめるのは
    // キーの受け付け方・画面遷移・配列の書き換え・表示メッセージだけ。
    fn base() -> UiState {
        let mut st = test_state();
        st.focus = Focus::Map;
        st.cfg.llm_recommend_enabled = false; // @ は「OFF」の枝だけ通す(claude を起動しないため)
        st
    }

    fn capture(f: impl FnOnce(&mut dyn Write)) -> String {
        let mut out: Vec<u8> = Vec::new();
        f(&mut out);
        String::from_utf8_lossy(&out).to_string()
    }

    fn press(st: &mut UiState, k: KeyEvent) -> (bool, String) {
        let kx = kctx();
        let mut out: Vec<u8> = Vec::new();
        let quit = map(st, k, &kx, &mut out);
        (quit, String::from_utf8_lossy(&out).to_string())
    }

    // ---- Space メニュー(カテゴリ選択) ----

    #[test]
    fn the_category_cursor_stops_at_both_ends() {
        let mut st = base();
        let kx = kctx();
        capture(|out| menu_categories(&mut st, code(KeyCode::Up), &kx, out));
        assert_eq!(st.menu_cat_sel, 0, "先頭より上へは行かない");
        assert!(matches!(st.focus, Focus::Menu(MenuLevel::Categories)));

        st.focus = Focus::Map;
        st.menu_cat_sel = MENU_CATEGORIES.len() - 1;
        capture(|out| menu_categories(&mut st, ch('s'), &kx, out));
        assert_eq!(st.menu_cat_sel, MENU_CATEGORIES.len() - 1, "末尾より下へは行かない");

        st.focus = Focus::Map;
        capture(|out| menu_categories(&mut st, code(KeyCode::Up), &kx, out));
        assert_eq!(st.menu_cat_sel, MENU_CATEGORIES.len() - 2);
    }

    #[test]
    fn enter_opens_the_selected_category_from_the_top() {
        let mut st = base();
        let kx = kctx();
        st.menu_cat_sel = 2;
        st.menu_item_sel = 4;
        capture(|out| menu_categories(&mut st, code(KeyCode::Enter), &kx, out));
        assert!(matches!(st.focus, Focus::Menu(MenuLevel::Items(2))));
        assert_eq!(st.menu_item_sel, 0);
    }

    #[test]
    fn esc_closes_the_menu_and_clears_the_left_gutter() {
        let mut st = base();
        let kx = kctx();
        let o = capture(|out| menu_categories(&mut st, code(KeyCode::Esc), &kx, out));
        assert!(matches!(st.focus, Focus::Map));
        assert_eq!(o, "\x1b[2J");
        assert!(st.force_reemit);
    }

    #[test]
    fn a_letter_on_the_top_menu_runs_that_item_from_any_category() {
        let mut st = base();
        let kx = kctx();
        st.help_page = 3;
        capture(|out| menu_categories(&mut st, ch('?'), &kx, out)); // ヘルプ(設定・ヘルプ)
        assert!(st.help);
        assert_eq!(st.help_page, 0);
        assert!(matches!(st.focus, Focus::Map), "実行したらメニューは閉じる");
    }

    #[test]
    fn an_unknown_letter_keeps_the_top_menu() {
        let mut st = base();
        let kx = kctx();
        capture(|out| menu_categories(&mut st, ch('Z'), &kx, out));
        assert!(matches!(st.focus, Focus::Menu(MenuLevel::Categories)));
        assert!(!st.help);
    }

    // ---- Space メニュー(項目選択) ----

    #[test]
    fn the_item_cursor_stops_at_both_ends() {
        let mut st = base();
        let kx = kctx();
        let ci = 2; // スポット(2項目)
        menu_items(&mut st, code(KeyCode::Up), ci, &kx);
        assert_eq!(st.menu_item_sel, 0);
        assert!(matches!(st.focus, Focus::Menu(MenuLevel::Items(2))));

        st.focus = Focus::Map;
        menu_items(&mut st, ch('s'), ci, &kx);
        assert_eq!(st.menu_item_sel, 1);

        st.focus = Focus::Map;
        menu_items(&mut st, code(KeyCode::Down), ci, &kx);
        assert_eq!(st.menu_item_sel, 1, "項目は2つなので末尾で止まる");
    }

    #[test]
    fn enter_runs_the_selected_item() {
        let mut st = base();
        let kx = kctx();
        let ci = MENU_CATEGORIES.iter().position(|c| c.label == "設定・ヘルプ").expect("設定・ヘルプ");
        st.menu_item_sel = MENU_CATEGORIES[ci].items.iter().position(|it| it.key == '?').expect("ヘルプ");
        menu_items(&mut st, code(KeyCode::Enter), ci, &kx);
        assert!(st.help);
        assert!(matches!(st.focus, Focus::Map), "実行したらメニューは閉じる");
    }

    #[test]
    fn a_letter_is_only_valid_inside_the_open_category() {
        let mut st = base();
        let kx = kctx();
        let ci = MENU_CATEGORIES.iter().position(|c| c.label == "スポット").expect("スポット");

        // そのカテゴリの項目キーなら実行する。
        st.show_spots = true;
        menu_items(&mut st, ch('V'), ci, &kx);
        assert!(!st.show_spots);
        assert_eq!(st.addr, "マイスポット非表示");
        assert!(matches!(st.focus, Focus::Map));

        // 他のカテゴリでは有効なキーでも、ここでは効かない(スコープ限定)。
        st.focus = Focus::Map;
        menu_items(&mut st, ch('?'), ci, &kx);
        assert!(!st.help, "ヘルプは「設定・ヘルプ」の中だけ");
        assert!(matches!(st.focus, Focus::Menu(MenuLevel::Items(2))), "メニューは開いたまま");
    }

    #[test]
    fn esc_goes_back_to_the_category_list() {
        let mut st = base();
        let kx = kctx();
        menu_items(&mut st, code(KeyCode::Esc), 1, &kx);
        assert!(matches!(st.focus, Focus::Menu(MenuLevel::Categories)), "1段上へ戻る(地図へは戻らない)");
    }

    #[test]
    fn an_unknown_key_keeps_the_item_menu() {
        let mut st = base();
        let kx = kctx();
        menu_items(&mut st, code(KeyCode::F(1)), 1, &kx);
        assert!(matches!(st.focus, Focus::Menu(MenuLevel::Items(1))));
    }

    // ---- 地図 ----

    #[test]
    fn the_zoom_keys_scale_the_center_and_stop_at_the_limits() {
        let mut st = base();
        st.cx = 1000.0;
        st.cy = 2000.0;
        st.addr = "どこか".into();
        press(&mut st, ch('+'));
        assert_eq!((st.z, st.cx, st.cy), (15, 2000.0, 4000.0));
        assert!(st.addr.is_empty(), "住所表示は縮尺を変えたら消す");

        press(&mut st, ch('-'));
        assert_eq!((st.z, st.cx, st.cy), (14, 1000.0, 2000.0));

        st.z = 19;
        press(&mut st, ch('='));
        assert_eq!(st.z, 19, "これ以上寄れない");

        st.z = 2;
        press(&mut st, ch('_'));
        assert_eq!(st.z, 2, "これ以上引けない");
    }

    #[test]
    fn enter_snaps_to_the_nearest_favorite_and_names_it() {
        let mut st = base();
        st.spots = vec![Spot { lat: 35.1, lon: 139.1, cat: "温泉".into(), name: "箱根".into() }];
        let (sx, sy) = deg_to_pixel(35.1, 139.1, st.z);
        st.cx = sx + 10.0; // 当たり範囲(80px)の内側
        st.cy = sy;
        press(&mut st, code(KeyCode::Enter));
        assert_eq!((st.cx, st.cy), (sx, sy), "スポットの真上へ寄せる");
        assert_eq!(st.popup.as_deref(), Some("★ 箱根 [温泉]"));
    }

    #[test]
    fn enter_reports_when_the_nearest_favorite_is_too_far() {
        let mut st = base();
        st.spots = vec![Spot { lat: 35.1, lon: 139.1, cat: "温泉".into(), name: "箱根".into() }];
        let (sx, sy) = deg_to_pixel(35.1, 139.1, st.z);
        st.cx = sx + 200.0; // 当たり範囲(80px)の外
        st.cy = sy;
        press(&mut st, code(KeyCode::Enter));
        assert_eq!(st.addr, "近くにお気に入り無し");
        assert_eq!(st.cx, sx + 200.0, "地図は動かさない");
        assert!(st.popup.is_none());
    }

    #[test]
    fn enter_reports_when_there_is_no_favorite_at_all() {
        let mut st = base();
        press(&mut st, code(KeyCode::Enter));
        assert_eq!(st.addr, "お気に入り未登録");
        assert!(st.popup.is_none());
    }

    #[test]
    fn enter_on_an_action_row_runs_that_action() {
        let mut st = base();
        st.wps = vec![(35.0, 139.0)];
        let ai = ROUTE_ACTS.iter().position(|(_, a)| *a == MenuAction::SaveGpx).expect("GPX書き出し");
        st.route_sel = st.wps.len() + ai;
        press(&mut st, code(KeyCode::Enter));
        assert_eq!(st.addr, "ルート未確定", "ルートが無ければ書き出さない");
    }

    #[test]
    fn v_adds_the_center_and_selects_it() {
        let mut st = base();
        press(&mut st, ch('v'));
        assert_eq!(st.wps, vec![(35.0, 139.0)], "そのフレームの中心を足す");
        assert_eq!((st.wp_sel, st.route_sel), (0, 0));
        assert_eq!(st.addr, "地点を追加 #1");
        assert!(st.route_job.is_none(), "1点だけならルート計算は始めない");
    }

    #[test]
    fn w_and_s_walk_the_left_gutter_without_opening_it() {
        let mut st = base();
        st.wps = vec![(35.0, 139.0), (35.5, 139.5)];
        let total = st.wps.len() + ROUTE_ACTS.len();

        press(&mut st, ch('s'));
        assert_eq!(st.route_sel, 1);
        assert_eq!(st.wp_sel, 1, "点の行なら経由地の選択も合わせる");
        let (ex, ey) = deg_to_pixel(35.5, 139.5, st.z);
        assert_eq!((st.cx, st.cy), (ex, ey), "選択に地図が追従する");
        assert!(matches!(st.focus, Focus::Map), "一覧へは入らない");

        press(&mut st, ch('s'));
        assert_eq!(st.route_sel, 2, "操作行へ進む");
        assert_eq!(st.wp_sel, 1, "操作行では経由地の選択は動かさない");
        assert_eq!((st.cx, st.cy), (ex, ey), "地図も動かさない");

        press(&mut st, ch('w'));
        assert_eq!(st.route_sel, 1);

        st.route_sel = 0;
        press(&mut st, ch('w'));
        assert_eq!(st.route_sel, total - 1, "先頭で戻ると末尾へ回り込む");
    }

    #[test]
    fn tab_enters_the_route_panel_only_when_there_is_a_route() {
        let mut st = base();
        press(&mut st, code(KeyCode::Tab));
        assert!(matches!(st.focus, Focus::Map), "点が無ければ入らない");

        st.wps = vec![(35.0, 139.0)];
        st.route_sel = 99;
        press(&mut st, code(KeyCode::Tab));
        assert!(matches!(st.focus, Focus::RoutePanel));
        assert_eq!(st.route_sel, st.wps.len() + ROUTE_ACTS.len() - 1, "行数に収める");
    }

    #[test]
    fn space_opens_the_menu_from_the_first_category() {
        let mut st = base();
        st.menu_cat_sel = 3;
        press(&mut st, ch(' '));
        assert!(matches!(st.focus, Focus::Menu(MenuLevel::Categories)));
        assert_eq!(st.menu_cat_sel, 0);
    }

    #[test]
    fn the_single_letter_keys_open_their_screens() {
        let mut st = base();
        st.input_cur = 5;
        press(&mut st, ch('/'));
        assert!(matches!(&st.focus, Focus::Search(q) if q.is_empty()));
        assert_eq!(st.input_cur, 0);

        let mut st = base();
        press(&mut st, ch('f'));
        assert!(matches!(st.focus, Focus::PoiMenu));

        let mut st = base();
        press(&mut st, ch('S'));
        assert!(matches!(st.focus, Focus::RouteFavMenu { sel: 0 }));

        let mut st = base();
        press(&mut st, ch('r'));
        assert!(matches!(&st.focus, Focus::RoadSearch(q) if q.is_empty()));

        let mut st = base();
        press(&mut st, ch('W'));
        assert!(matches!(st.focus, Focus::WanderForm { dist_km } if dist_km == 40.0), "CLI未指定なら既定40km");
    }

    #[test]
    fn the_display_toggles_flip_and_report() {
        let mut st = base();
        st.show_spots = true;
        press(&mut st, ch('V'));
        assert!(!st.show_spots);
        assert_eq!(st.addr, "マイスポット非表示");

        let mut st = base();
        press(&mut st, ch('E'));
        assert!(st.show_elev);
        assert_eq!(st.addr, "標高: ルート確定後に表示");

        let mut st = base();
        assert!(!st.cfg.image_mode);
        press(&mut st, ch('I'));
        assert!(st.cfg.image_mode);
        assert!(st.addr.starts_with("実画像モード: ON"), "端末が非対応なら断り書きが付く: {}", st.addr);
        assert!(st.force_reemit);
        press(&mut st, ch('I'));
        assert!(!st.cfg.image_mode);
        assert_eq!(st.addr, "実画像モード: OFF");
    }

    #[test]
    fn hiding_the_route_gutter_clears_the_screen_but_showing_it_does_not() {
        let mut st = base();
        let (_, o) = press(&mut st, ch('R'));
        assert!(st.route_panel_hidden);
        assert_eq!(st.addr, "ルート一覧: 非表示");
        assert_eq!(o, "\x1b[2J");

        let (_, o) = press(&mut st, ch('R'));
        assert!(!st.route_panel_hidden);
        assert_eq!(st.addr, "ルート一覧: 表示");
        assert!(o.is_empty(), "出す方向はマップの再描画で足りる");
    }

    #[test]
    fn the_features_that_are_off_say_so_instead_of_reaching_out() {
        let mut st = base();
        press(&mut st, ch('@'));
        assert_eq!(st.addr, "おすすめ: 設定でOFF(,でON)");
        assert!(matches!(st.focus, Focus::Map));

        let mut st = base();
        assert!(!st.cfg.disaster_enabled);
        press(&mut st, ch('B'));
        assert_eq!(st.addr, "過去災害: OFF(設定で有効化)");
        assert!(st.disaster_job.is_none());

        let mut st = base();
        assert!(!st.cfg.regulation_enabled);
        press(&mut st, ch('T'));
        assert_eq!(st.addr, "通行規制: OFF(設定で有効化)");
        assert!(st.regulation_detail_job.is_none());

        let mut st = base();
        assert!(st.cfg.google_maps_api_key.is_empty());
        press(&mut st, ch('i'));
        assert!(st.addr.contains("Google APIキー未設定"));
        assert!(st.street_job.is_none());
    }

    #[test]
    fn the_route_keys_report_when_there_is_no_route_yet() {
        let mut st = base();
        press(&mut st, ch('n'));
        assert_eq!(st.addr, "ルート未確定");
        assert_eq!(st.route_alt, 0, "代替ルートの番号も進めない");

        let mut st = base();
        st.wps = vec![(35.0, 139.0)];
        press(&mut st, ch('o'));
        assert_eq!(st.addr, "ルート未確定");
        assert!(st.qr_view.is_none());

        let mut st = base();
        press(&mut st, ch('g'));
        assert_eq!(st.addr, "ルート未確定");
    }

    #[test]
    fn x_removes_the_selected_waypoint() {
        let mut st = base();
        st.wps = vec![(35.0, 139.0)];
        st.wp_sel = 0;
        press(&mut st, ch('x'));
        assert!(st.wps.is_empty());
        assert_eq!(st.route_sel, st.wp_sel);
        assert!(st.route_job.is_none());
    }

    #[test]
    fn m_walks_the_travel_modes() {
        let mut st = base();
        assert_eq!(st.mode, "surface");
        press(&mut st, ch('m'));
        assert_eq!(st.mode, "highway");
        press(&mut st, ch('m'));
        assert_eq!(st.mode, "short");
        press(&mut st, ch('m'));
        assert_eq!(st.mode, "surface", "3種を巡回して戻る");
    }

    #[test]
    fn the_radar_step_back_does_nothing_while_the_radar_is_off() {
        let mut st = base();
        assert!(!st.radar_on);
        press(&mut st, ch('<'));
        assert_eq!(st.radar_idx, 0);
        assert!(st.addr.is_empty(), "OFFのときは勝手にONにしない");
    }

    #[test]
    fn q_quits_and_other_keys_do_not() {
        let mut st = base();
        assert!(press(&mut st, ch('q')).0);
        assert!(!press(&mut st, ch('Z')).0);
    }
}
