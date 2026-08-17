// 対話UIループ。main.rs から機械的に切り出したもの(挙動は不変)。
// HELP / TermGuard / interactive を収める。fit_cells 等はクレートルート(main.rs)側に残す。

use crate::*;
use crate::geo::*;
use crate::tiles::*;
use crate::render::*;
use crate::route::*;
use crate::poi::*;
use crate::spots::*;
use crate::share::*;
use std::io::Write;
use image::{RgbImage, RgbaImage, imageops::FilterType};
// 単一行テキスト入力欄の共通編集ロジック(char_byte/insert_str_at/form_cur/edit_line/
// render_with_cursor/draw_input_panel)は textedit.rs へ切り出し済み。
use crate::textedit::{edit_line, form_cur, insert_str_at};
// 左袖リストのスクロール追従(ensure_visible)は listview.rs、行の組み立ては ui_gutter.rs へ切り出し済み。

// PALETTE_NAMES(中心十字の色名。SPOT_PALETTEと同じ並び)・その利用箇所は settings.rs に移設。
// 緑グラデのワードマーク(LOGO・ヘルプ画面で使用)は keymap.rs へ移設済み。

use crate::keymap::{HELP, LOGO};

// Space メニュー(MenuAction/MenuItem/MenuCategory/MENU_CATEGORIES/MenuLevel/menu_action_for_key/
// disp_width/menu_row/ROUTE_ACTS)は menu.rs へ切り出し済み。ここでは crate::menu を参照する。
use crate::menu::{MenuAction, MENU_CATEGORIES, MenuLevel, menu_action_for_key, ROUTE_ACTS};

// 道路名検索(r)で追加した道路の塊(RoadSeg)・その表示色選択(road_color_for)は roadseg.rs へ切り出し済み。
use crate::roadseg::{RoadSeg, road_color_for};

// interactive() の外にあった小さなヘルパー(onboarded_marker/QrView/build_qr_view/radar_opacity_value/
// radar_refresh_secs/maybe_speak_turn/persist_full_state/TermGuard)は ui_helpers.rs へ切り出し済み。
// 画面状態の Focus enum は focus.rs へ切り出し済み(描画側の ui_gutter/ui_status/ui_overlay からも参照する)。
use crate::ui_helpers::*;
use crate::focus::Focus;

// ---- 対話モード (crossterm) ----

pub(crate) fn interactive(cx: f64, cy: f64, z: u32, a: &Args) -> std::io::Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    let _guard = TermGuard::enter()?; // Drop で必ず端末復元
    // タイルキャッシュは常駐ローダーとメイン描画で共有する(Arc<Mutex>)。未取得タイルはメインが
    // グレーで即描画し、ローダーが現在viewに近い順で裏取得→cacheへ→次フレームで自動反映される。
    let cache = std::sync::Arc::new(std::sync::Mutex::new(Cache::new()));
    let loader = TileLoader::start(std::sync::Arc::clone(&cache));
    let mut out = std::io::stdout();
    // 対話ループの状態(もとは約110個のローカル変数)は uistate.rs の UiState へ集約した。
    // 端末ハンドル out・タイルローダー loader・端末復元用の TermGuard だけはここに残す
    // (UiState を設定ファイルもネットワークも触らずに作れる素のデータに保ち、
    // 状態遷移をテストできる状態にしておくため)。経緯は docs/ui-refactor-design.md。
    let mut st = uistate::UiState::new(a, cx, cy, z);

    // メニュー項目/直接キー どちらからでも同じ処理を走らせる。
    // lat/lon/cols/tr/route_nogos は各ループで再計算されるフレーム値。マクロ衛生性のため引数で受け取る。
    macro_rules! run_action { ($act:expr, $lat:expr, $lon:expr, $cols:expr, $tr:expr, $nogos:expr) => {{
        match $act {
            MenuAction::SearchPlace => { st.input_cur = 0; st.focus = Focus::Search(String::new()); }
            MenuAction::SearchPoi => { st.focus = Focus::PoiMenu; }
            MenuAction::ShowAddress => { st.addr = reverse_geocode($lat, $lon).unwrap_or_else(|e| format!("({e})")); }
            MenuAction::Recommend => {
                if !st.cfg.llm_recommend_enabled { st.snd.play("error"); st.addr = "おすすめ: 設定でOFF(,でON)".into(); }
                else if !recommend::claude_available(&st.cfg.llm_command) { st.snd.play("error"); st.addr = "おすすめ: claudeが無い(設定のLLM/コマンド確認)".into(); }
                else { st.input_cur = 0; st.focus = Focus::Recommend(String::new()); }
            }
            MenuAction::RouteForm => { if st.wps.is_empty() { st.addr = "先に v で地点を置いてね".into(); } else { st.wp_sel = 0; st.grab = false; st.focus = Focus::WaypointList; } }
            MenuAction::AddVia => { st.snd.play("pop"); wp_add(&mut st.wps, ($lat, $lon)); let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, $nogos); st.route_note = n_; st.route_job = j_; st.addr = format!("地点を追加 #{}", st.wps.len()); }
            MenuAction::RoadRoute => { st.input_cur = 0; st.focus = Focus::RoadSearch(String::new()); }
            MenuAction::Wander => { st.focus = Focus::WanderForm { dist_km: a.dist.unwrap_or(40.0) }; } // 距離ゲージを開く(Enterで検索開始)
            MenuAction::CycleMode => { st.mode = match mode_label(&st.mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, $nogos); st.route_note = n_; st.route_job = j_; }
            MenuAction::AltRoute => {
                if st.wps.len() >= 2 {
                    st.route_alt = (st.route_alt + 1) % 4;
                    let (nn, jj) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, st.route_alt, &st.cfg.google_maps_api_key, $nogos);
                    st.route_note = nn; st.route_job = jj;
                } else { st.snd.play("error"); st.addr = "ルート未確定".into(); }
            }
            MenuAction::ClearRoute => { if !st.wps.is_empty() || !st.road_segs.is_empty() { st.clear_route_confirm = true; } }
            MenuAction::ManageRoads => { if st.road_segs.is_empty() { st.snd.play("error"); st.addr = "道路の塊がまだ無い(rで道路を追加)".into(); } else { st.road_sel = 0; st.focus = Focus::RoadList; } }
            MenuAction::ManageSpots => { st.cat_sel = 0; st.focus = Focus::SpotCatList; }
            MenuAction::ToggleSpots => { st.show_spots = !st.show_spots; apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots); st.addr = if st.show_spots { "マイスポット表示".into() } else { "マイスポット非表示".into() }; }
            MenuAction::ToggleElevation => {
                st.show_elev = !st.show_elev;
                if st.show_elev && (st.spec.routes.is_empty() || !st.route_ele.iter().any(|&z| z != 0.0)) { st.addr = "標高: ルート確定後に表示".into(); }
            }
            MenuAction::StreetView => {
                if !streetview::available(&st.cfg.google_maps_api_key) { st.snd.play("error"); st.addr = "実写: APIキー未設定(config.toml [streetview])".into(); }
                else {
                    // 実写取得を別スレッドへ。focus は Map のまま(メニューは既に閉じている)でスピナーが回る。
                    st.sv_fov = 90.0; // 開き直しなので既定ズームに戻す
                    let (la, lo) = ($lat, $lon);
                    let key = st.cfg.google_maps_api_key.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let r = streetview::fetch(la, lo, 0, 640, 480, 90.0, &key);
                        let _ = tx.send((la, lo, 0, r));
                    });
                    st.street_job = Some(rx);
                }
            }
            MenuAction::PlayRoute => {
                if st.spec.routes.last().map_or(false, |r| r.pts.len() >= 2) {
                    if st.play.is_some() {
                        st.play = None; st.play_last_tick = None;
                        st.play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        st.play_prefetch_rx = None; st.play_prefetch_held = None;
                        st.addr = "再生: 停止".into();
                    } else {
                        st.play = Some(0.0);
                        st.play_last_tick = Some(std::time::Instant::now());
                        st.play_wants_prefetch = true; // 実画像モードなら次フレームで先読みスレッドを起動する
                        st.addr = "再生: 開始(Aで停止)".into();
                    }
                } else { st.snd.play("error"); st.addr = "再生: ルート未確定".into(); }
            }
            MenuAction::ToggleGps => {
                if st.gps_rx.is_some() { st.gps_rx = None; st.addr = "ライブ現在地: OFF".into(); }
                else {
                    let bin = if std::path::Path::new("/opt/homebrew/bin/CoreLocationCLI").exists() { "/opt/homebrew/bin/CoreLocationCLI" } else { "CoreLocationCLI" };
                    if gpslive::available(bin) { st.gps_rx = Some(gpslive::start_poller(bin.to_string(), 5)); st.gps_trail.clear(); st.gps_pos = None; st.addr = "ライブ現在地: ON(5秒ごと)".into(); }
                    else { st.addr = "ライブ: CoreLocationCLI無し(brew install corelocationcli)".into(); }
                }
            }
            MenuAction::ToggleRadar => { st.radar_toggle(); } // 雨雲レーダー(地図の C キーと同じ)
            MenuAction::ViewCamera => { // 道路ライブカメラ(地図の N キーと同じ)
                if !st.cfg.camera_enabled { st.snd.play("error"); st.addr = "道路ライブカメラ: OFF(設定で有効化)".into(); }
                else {
                    // 視野内で中心に一番近いカメラ。ここで層から直接引くのは、フレーム先頭で
                    // 切り出した一覧の借用がこの時点(tick後)まで生きていられないため。
                    let nearest = st.camera_layer.items(plotlayer::view_bbox(st.cx, st.cy, st.z)).into_iter()
                        .min_by(|a, b| {
                            let da = (a.lat - $lat).powi(2) + (a.lon - $lon).powi(2);
                            let db = (b.lat - $lat).powi(2) + (b.lon - $lon).powi(2);
                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .cloned();
                    match nearest {
                        None => { st.snd.play("error"); st.addr = "道路ライブカメラ: 周辺に無し".into(); }
                        Some(c) => {
                            // キャッシュから読んだカメラは写真URLを持たない(URLに15分ごとの撮影
                            // ディレクトリが入るため保存していない)。その場合だけ整備局ページを
                            // 取り直してURLを補ってから画像を取る(押したときだけの1回)。
                            if c.full_url.is_none() { st.addr = "📷カメラ情報を更新中…".into(); }
                            let (tx, rx) = std::sync::mpsc::channel();
                            std::thread::spawn(move || {
                                let c = match c.full_url {
                                    Some(_) => c,
                                    None => camera::fetch_bureau(camera::nearest_bureau(c.lat, c.lon))
                                        .ok()
                                        .and_then(|cams| cams.into_iter().find(|x| x.id == c.id))
                                        .unwrap_or(c),
                                };
                                let r = match c.full_url.clone() {
                                    Some(url) => camera::fetch_image(&url),
                                    None => Err("画像URLを取得できない".to_string()),
                                };
                                let _ = tx.send((c, r));
                            });
                            st.cam_job = Some(rx);
                        }
                    }
                }
            }
            MenuAction::SaveRoute => { st.input_cur = st.route_name_hint.chars().count(); st.focus = Focus::SaveName(st.route_name_hint.clone()); }
            MenuAction::LoadRoute => { st.route_names = list_named_routes(); st.rn_sel = 0; if st.route_names.is_empty() { st.addr = "お気に入り無し".into(); } else { st.focus = Focus::RouteList; } }
            MenuAction::SaveGpx => match st.spec.routes.last() {
                Some(rt) => st.addr = match write_gpx("termmap-route.gpx", &rt.pts) { Ok(_) => "GPX保存: termmap-route.gpx".into(), Err(e) => format!("({e})") },
                None => { st.snd.play("error"); st.addr = "ルート未確定".into(); }
            },
            MenuAction::ShareQr => {
                if st.wps.len() >= 2 {
                    let (url, _) = gmaps_url(&st.wps);
                    match qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L) {
                        Ok(c) => st.qr_view = Some(build_qr_view(&c, &st.cfg.qr_style)),
                        Err(_) => st.addr = "QR生成失敗".into(),
                    }
                } else { st.snd.play("error"); st.addr = "ルート未確定".into(); }
            }
            MenuAction::Settings => { st.set_sel = 0; st.focus = Focus::Settings; voice::warm_voice_list(); }
            MenuAction::Help => { st.help = true; st.help_page = 0; }
        }
    }};}
    let _ = write!(out, "\x1b[2J");
    loop {
        st.spin = st.spin.wrapping_add(1); // 通信中スピナーのアニメ用(毎フレーム進める)
        let (tc, tr) = crossterm::terminal::size().unwrap_or((100, 40));
        let cols = tc.max(20) as u32;
        let map_rows = (tr.max(3) - 1) as u32;
        if st.help { // ヘルプ全画面。画面高に収まらなければページ送り(最終ページで任意キー→閉じる)
            let _ = write!(out, "\x1b[2J\x1b[H");
            for (i, (bold, (r, g, b), ln)) in LOGO.iter().enumerate() { // 先頭に緑ワードマーク
                let bold_code = if *bold { "\x1b[1m" } else { "" };
                let col = format!("{bold_code}{}", sgr_fg(*r, *g, *b, truecolor_safe()));
                let _ = write!(out, "\x1b[{};2H{}{}\x1b[0m\x1b[K", i + 1, col, ln);
            }
            let off = LOGO.len() + 1; // ロゴ4行 + 空行1
            let per_page = (map_rows as usize).saturating_sub(off).max(1);
            let content_len = HELP.len().saturating_sub(1); // 先頭行(見出し)はLOGOと重複するため除く
            let total_pages = content_len.div_ceil(per_page).max(1);
            st.help_page = st.help_page.min(total_pages - 1);
            for (i, l) in HELP.iter().skip(1 + st.help_page * per_page).enumerate().take(per_page) {
                let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + off + 1, l);
            }
            let has_more = st.help_page + 1 < total_pages;
            let hint = if total_pages > 1 {
                if has_more { format!(" {}/{} ページ (任意のキーで次へ) ", st.help_page + 1, total_pages) }
                else { format!(" {}/{} ページ (任意のキーで閉じる) ", st.help_page + 1, total_pages) }
            } else { " 任意のキーで閉じる ".to_string() };
            let _ = write!(out, "\x1b[{};1H\x1b[7m{hint}\x1b[0m\x1b[K", tr);
            let _ = out.flush();
            if let Event::Key(_) = event::read()? {
                if has_more { st.help_page += 1; } else { st.help = false; st.help_page = 0; }
            }
            st.force_reemit = true; // ヘルプで全画面クリアした→地図に戻ったら画像を再emit
            continue;
        }
        if st.street.is_some() { // 実写(Street View)全画面。←→で向き、Esc/qで戻る
            { // 描画(不変借用のスコープ)
                let (img, heading, slat, slon) = st.street.as_ref().unwrap();
                if st.cfg.image_mode && image_capable() {
                    // 実画像モード: 実写を全幅×map_rows のインライン画像で表示
                    let _ = write!(out, "\x1b[H");
                    let _ = emit_iterm2_image(&mut out, img, cols, map_rows);
                } else {
                    let rs = image::imageops::resize(img, cols.max(10), map_rows * 2, FilterType::Triangle);
                    let art = render_halfblock(&rs, truecolor_safe());
                    let sv_lines: Vec<&str> = art.split("\r\n").collect();
                    let _ = write!(out, "\x1b[H");
                    for i in 0..map_rows as usize {
                        let ln = sv_lines.get(i).copied().unwrap_or("");
                        let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + 1, ln);
                    }
                }
                let hd = ((heading % 360) + 360) % 360;
                let arrow = heading_arrow(hd as f64);
                let bar = fit_cells_scroll(&format!(" 実写 {arrow} h{hd}° fov{:.0}°  ←→向き ↑↓移動(地図も追従) +/-ズーム (Shiftで微調整)  Esc/q戻る  {slat:.4},{slon:.4} ", st.sv_fov), cols as usize, st.spin);
                let _ = write!(out, "\x1b[{};1H\x1b[7m{bar}\x1b[0m\x1b[K", tr);
                let _ = out.flush();
            }
            let (hd_c, slat_c, slon_c) = { let (_, h, la, lo) = st.street.as_ref().unwrap(); (*h, *la, *lo) };
            // 押しっぱなし/連打で溜まった同種キーは最新の1個へ間引く(#4のMap focusと同じ理由)。
            // Esc/q等の別系統キーが混ざっていたら間引きを止めてそちらを即座に優先する。
            let is_sv_key = |c: KeyCode| matches!(c, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('-') | KeyCode::Char('_'));
            let mut ev = event::read()?;
            if let Event::Key(first) = &ev {
                if is_sv_key(first.code) {
                    while let Ok(true) = event::poll(std::time::Duration::from_millis(0)) {
                        match event::read()? {
                            Event::Key(next) if is_sv_key(next.code) => ev = Event::Key(next),
                            other => { ev = other; break; }
                        }
                    }
                }
            }
            if let Event::Key(k) = ev {
                match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                        // ←→=向き回転(既定10°・Shiftで45°) / ↑↓=向き方向に前後移動(既定5m・Shiftで20m)
                        // Map focusのパン(既定=細かく・Shiftで常に高速)と同じ規則に揃える。
                        let fine = !k.modifiers.contains(KeyModifiers::SHIFT);
                        let rot = if fine { 10 } else { 45 };
                        let dist = if fine { 5.0 } else { 20.0 };
                        let (nlat, nlon, nhd) = match k.code {
                            KeyCode::Left => (slat_c, slon_c, hd_c - rot),
                            KeyCode::Right => (slat_c, slon_c, hd_c + rot),
                            KeyCode::Up => { let (a, b) = streetview::step(slat_c, slon_c, hd_c as f64, dist); (a, b, hd_c) }
                            _ => { let (a, b) = streetview::step(slat_c, slon_c, hd_c as f64 + 180.0, dist); (a, b, hd_c) }
                        };
                        if let Ok(im) = streetview::fetch(nlat, nlon, nhd, 640, 480, st.sv_fov, &st.cfg.google_maps_api_key) {
                            st.street = Some((im, nhd, nlat, nlon)); // Err時は現状維持(行き止まり等)
                            // 地図連動: 前後移動(↑↓)で歩いた先に地図の中心も追従させる。実写を
                            // 閉じたとき、元の地点でなく実際に歩いた地点で地図が表示されるようにする。
                            if matches!(k.code, KeyCode::Up | KeyCode::Down) {
                                let (nx, ny) = deg_to_pixel(nlat, nlon, st.z);
                                st.cx = nx; st.cy = ny;
                            }
                        }
                    }
                    KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Char('-') | KeyCode::Char('_') => {
                        // ズーム: fov(画角)を上げ下げ。小さいほどズームイン。Shiftで細かく調整
                        let fine = k.modifiers.contains(KeyModifiers::SHIFT);
                        let step = if fine { 5.0 } else { 10.0 };
                        let zoom_in = matches!(k.code, KeyCode::Char('+') | KeyCode::Char('='));
                        let nfov = (st.sv_fov + if zoom_in { -step } else { step }).clamp(20.0, 100.0);
                        if let Ok(im) = streetview::fetch(slat_c, slon_c, hd_c, 640, 480, nfov, &st.cfg.google_maps_api_key) {
                            st.sv_fov = nfov;
                            st.street = Some((im, hd_c, slat_c, slon_c));
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => st.street = None,
                    KeyCode::Char('I') => { // 実写表示中も画像モードON/OFFを切替できるように(Map focusと同じキー)
                        st.cfg.image_mode = !st.cfg.image_mode;
                        st.addr = if st.cfg.image_mode {
                            if image_capable() { "実画像モード: ON".into() } else { "実画像モード: ON(この端末は非対応・AA継続)".into() }
                        } else { "実画像モード: OFF".into() };
                    }
                    _ => {}
                }
            }
            st.force_reemit = true; // 実写で全画面を覆った→地図に戻ったら画像を再emit
            continue;
        }
        if st.cam_view.is_some() { // 道路ライブカメラの写真を全画面表示。streetと同じ早期returnパターン
            // 道路カメラは固定視点の1枚画像なのでstreetと違いパン/ズームは無い(Esc/qで戻るのみ)。
            { // 描画(不変借用のスコープ)
                let (img, cam) = st.cam_view.as_ref().unwrap();
                if st.cfg.image_mode && image_capable() {
                    let _ = write!(out, "\x1b[H");
                    let _ = emit_iterm2_image(&mut out, img, cols, map_rows);
                } else {
                    let rs = image::imageops::resize(img, cols.max(10), map_rows * 2, FilterType::Triangle);
                    let art = render_halfblock(&rs, truecolor_safe());
                    let cam_lines: Vec<&str> = art.split("\r\n").collect();
                    let _ = write!(out, "\x1b[H");
                    for i in 0..map_rows as usize {
                        let ln = cam_lines.get(i).copied().unwrap_or("");
                        let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + 1, ln);
                    }
                }
                let bar = fit_cells_scroll(&format!(" 道路カメラ {}({})  Esc/q戻る  {:.4},{:.4} ", cam.name, cam.taken_at, cam.lat, cam.lon), cols as usize, st.spin);
                let _ = write!(out, "\x1b[{};1H\x1b[7m{bar}\x1b[0m\x1b[K", tr);
                let _ = out.flush();
            }
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') => st.cam_view = None,
                    KeyCode::Char('I') => { // 表示中も画像モードON/OFFを切替できるように(Map focusと同じキー)
                        st.cfg.image_mode = !st.cfg.image_mode;
                        st.addr = if st.cfg.image_mode {
                            if image_capable() { "実画像モード: ON".into() } else { "実画像モード: ON(この端末は非対応・AA継続)".into() }
                        } else { "実画像モード: OFF".into() };
                    }
                    _ => {}
                }
            }
            st.force_reemit = true;
            continue;
        }
        // 標高プロファイル帯を出すぶん地図の行数を減らす(E)
        let elev_on = st.show_elev && !st.spec.routes.is_empty() && st.route_ele.len() >= 2 && st.route_ele.iter().any(|&z| z != 0.0);
        let elev_h: u32 = if elev_on { (map_rows / 3).clamp(4, 12) } else { 0 };
        let map_rows = if elev_h > 0 { map_rows.saturating_sub(elev_h + 1).max(3) } else { map_rows };
        let show_routes = matches!(st.focus, Focus::RouteList);
        let show_wps = matches!(st.focus, Focus::WaypointList);
        let show_route = (matches!(st.focus, Focus::Map) && !st.wps.is_empty() && !st.route_panel_hidden) || matches!(st.focus, Focus::RoutePanel); // 地点一覧を左袖に(Map中・R非表示でなければ/パネルフォーカス中は常に)
        let show_splist = matches!(st.focus, Focus::SpotList);
        let show_catlist = matches!(st.focus, Focus::SpotCatList);
        let show_settings = matches!(st.focus, Focus::Settings | Focus::SettingsPick(_));
        let show_menu = matches!(st.focus, Focus::Menu(_));
        let show_poimenu = matches!(st.focus, Focus::PoiMenu);
        let show_roadlist = matches!(st.focus, Focus::RoadList);
        let show_favmenu = matches!(st.focus, Focus::RouteFavMenu { .. });
        let gut: u32 = if !st.pois.is_empty() || show_routes || show_wps || show_route || show_splist || show_catlist || show_settings || show_menu || show_poimenu || show_roadlist || show_favmenu { 28 } else { 0 };
        let map_cols = cols.saturating_sub(gut).max(10);
        let (ow, oh) = if st.opts.braille || st.opts.edge { (map_cols * 2, map_rows * 4) } else { (map_cols, map_rows * 2) };
        if let Some(p) = &st.gps_rx { // ライブ現在地を取り込み、自位置に追従
            while let Ok((la, lo)) = p.rx.try_recv() {
                st.gps_pos = Some((la, lo));
                st.gps_trail.push((la, lo));
                if st.gps_trail.len() > 300 { st.gps_trail.remove(0); }
                let (nx, ny) = deg_to_pixel(la, lo, st.z); st.cx = nx; st.cy = ny;
                maybe_speak_turn(&st.cfg, &st.spec, &st.turn_points, &mut st.voice_guide, (la, lo));
            }
        }
        let img_inline = st.cfg.image_mode && image_capable(); // 実画像モード(iTerm2系端末のみ)。play処理より先に要る
        // 雨雲の合成方式は描画モードで2系統に分かれる(設計 §2.1)。どのモードでも表示はできる。
        //   実画像 / halfblock … 地図へ直接アルファ合成(下の地図が透ける)
        //   classify         … recolor()で6色へ量子化した「後」に合成(先に混ぜると淡い青の降水が湖に化ける)
        //   braille / edge   … 背景色の概念が無いので OverlayLayer へディザ間引きしたインクとして焼く
        // mono は単体では描画経路を変えない(render_braille の色を落とすだけ)ので、braille/edge が
        // 立っていなければ halfblock と同じアルファ合成になる。
        let radar_ink = !img_inline && (st.opts.braille || st.opts.edge);
        if st.play.is_some() { // ルート再生: 実時間ベースで位置を進めて自動パン(想定巡航速度×play_speed倍率)
            // 実画像モードは先読みスレッドが返した画像をベース地図に使う。オーバーレイ(ルート線/
            // クロスヘア)をそれと違う位置で描くと、ベースとオーバーレイがズレてルートがガタつい
            // て見えるバグになるため、その画像が実際に描かれた位置(frame_d)を表示位置の正とする。
            if img_inline {
                if let Some(rx) = &st.play_prefetch_rx {
                    let mut latest = None;
                    while let Ok(f) = rx.try_recv() { latest = Some(f); }
                    if let Some(f) = latest { st.play_prefetch_held = Some(f); }
                }
            }
            let prefetched_d = if img_inline { st.play_prefetch_held.as_ref().map(|(d, _)| *d) } else { None };
            if let Some(rt) = st.spec.routes.last().map(|r| r.pts.clone()) {
                if rt.len() >= 2 {
                    let total = roadtrace::polyline_len(&rt);
                    let d = if let Some(fd) = prefetched_d {
                        fd
                    } else {
                        let now = std::time::Instant::now();
                        let dt = st.play_last_tick.map_or(0.0, |t| now.duration_since(t).as_secs_f64());
                        st.play.unwrap() + roadtrace::play_step_distance_m(st.cfg.route_play_speed_kmh, st.play_speed, dt)
                    };
                    st.play_last_tick = Some(std::time::Instant::now()); // 次回差分計算の基点(先読み経路でも維持)
                    if d >= total {
                        st.play = None; st.play_last_tick = None;
                        st.play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        st.play_prefetch_rx = None; st.play_prefetch_held = None;
                        st.addr = "再生: 終了".into();
                    } else {
                        st.play = Some(d);
                        let (pla, plo) = roadtrace::point_at(&rt, d);
                        let (nx, ny) = deg_to_pixel(pla, plo, st.z); st.cx = nx; st.cy = ny;
                        maybe_speak_turn(&st.cfg, &st.spec, &st.turn_points, &mut st.voice_guide, (pla, plo));
                    }
                } else {
                    st.play = None; st.play_last_tick = None;
                    st.play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    st.play_prefetch_rx = None; st.play_prefetch_held = None;
                }
            } else {
                st.play = None; st.play_last_tick = None;
                st.play_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                st.play_prefetch_rx = None; st.play_prefetch_held = None;
            }
        }
        let (lat, lon) = pixel_to_deg(st.cx, st.cy, st.z);

        // プロットデータ(道路交通量/主要道路/道路ライブカメラ/通行規制)の、いま表示範囲に
        // 掛かるぶんを1フレーム1回だけ切り出す(地図のオーバーレイ描画とステータス行で共用)。
        // キャッシュは視野より広いセル単位で持っているので、ここで視野へ絞る。
        let plot_view = plotlayer::view_bbox(st.cx, st.cy, st.z);
        let plot_now = plotcache::now_secs(); // 経過時間表示の基準(1フレーム内で揃える)
        let traffic_points = st.traffic_layer.items(plot_view);
        // 観測点のスナップ先は点列だけあればよいので、線形への参照だけを借りる(複製しない)。
        let major_roads: Vec<&[(f64, f64)]> =
            st.roads_layer.items(plot_view).into_iter().map(|r| r.pts.as_slice()).collect();
        let camera_points = st.camera_layer.items(plot_view);
        let regulation_events = st.regulation_layer.items(plot_view);
        // 規制原因アイコン(#規制原因アイコン): 表示中のClosedイベントから未分類の1件を選び、
        // 他にジョブが走っていなければバックグラウンドで規制原因を取得する
        // (同時に1件だけ=道路情報提供システムへの負荷を抑えるレート制限)。
        if st.cfg.regulation_enabled && st.cause_job.is_none() {
            let visible_closed: Vec<&regulation::ClosureEvent> = regulation_events.iter().copied()
                .filter(|e| e.kind == regulation::RegulationKind::Closed)
                .collect();
            if let Some(id) = next_closure_to_categorize(&visible_closed, &st.cause_cache) {
                let id = id.to_string();
                let (tx, rx) = std::sync::mpsc::channel();
                let id2 = id.clone();
                std::thread::spawn(move || { let _ = tx.send((id2, regulation::fetch_detail(&id))); });
                st.cause_job = Some(rx);
            }
        }
        let disaster_sites = st.disaster_layer.items(plot_view);

        // 通行止めルート回避(#通行止めを推奨しない)。表示中の視野(plot_view)ではなく、
        // 経由地全体を覆うbboxで実施中の通行止めを見て、BRouterのnogosへ変換する。
        // 通行規制の設定(cfg.regulation_enabled)と連動させる: OFFなら外部へ問い合わせず
        // regulation_layerにデータ自体が無いため、ここでも自然に空になる。
        let (route_nogos, route_nogos_truncated) = if st.cfg.regulation_enabled {
            match route::waypoints_bbox_with_margin(&st.wps, 0.05) {
                Some(bbox) => {
                    let closures = st.regulation_layer.items(bbox);
                    let center = ((bbox.0 + bbox.2) / 2.0, (bbox.1 + bbox.3) / 2.0);
                    let (circles, truncated) = route::closures_to_nogos(&closures, center);
                    (route::nogos_query_param(&circles), truncated)
                }
                None => (String::new(), false),
            }
        } else {
            (String::new(), false)
        };

        // 移動検知(解像度非依存): 直近に描画したフレームと(cx,cy,z)が違えば「動いた」。
        // 動いた直後〜350ms は低解像度(delta=0)で速く描き、動きが止まって350ms経ったら
        // 設定解像度(高/中/低)へ上げる。GPS追従(gps_rx)のように断続的に動くケースは
        // 自然に低解像度のまま張り付く(=負荷とメモリを抑える)。
        // ただしルート再生(play)中は毎フレーム動き続けるため、この判定に従うと恒久的に
        // 低解像度画像が高頻度で切り替わり続けてちらついて見える。プレビューは見た目重視の
        // 機能なので、再生中は「動いている」扱いにせず常に設定解像度を使う。
        if st.prev_render_cxyz != Some((st.cx, st.cy, st.z)) { st.moved_at = Some(std::time::Instant::now()); }
        st.prev_render_cxyz = Some((st.cx, st.cy, st.z));
        let settling = img_inline && st.cfg.image_settle_low_res && st.play.is_none() && st.moved_at.map_or(false, |t| t.elapsed() < std::time::Duration::from_millis(350));

        // 実画像モードの描画寸法とズーム。AAと同じ地理範囲を、深いズーム段(タイルの上限z18まで)
        // で取得して高精細化する。scale=2^Δ で、地図領域のセル数×(横scale/縦2*scale px)の実ピクセル
        // 解像度になる。設定(image_res)で上限を選べる: high=+2(横4/縦8px per cell) / mid=+1 / low=+0。
        // rz>z のときグローバル画素座標は 2^Δ 倍になるので中心 cx/cy も scale 倍する。
        let base_delta: u32 = match st.cfg.image_res.as_str() { "high" => 2, "low" => 0, _ => 1 };
        let delta = if !img_inline { 0 } else if settling { 0 } else { base_delta.min(18u32.saturating_sub(st.z)) };
        let scale = 1u32 << delta;
        let (rw, rh, rz, rcx, rcy) = if img_inline {
            (map_cols * scale, map_rows * 2 * scale, st.z + delta, st.cx * scale as f64, st.cy * scale as f64)
        } else {
            (ow, oh, st.z, st.cx, st.cy)
        };
        // ローダーへ今の表示位置(実描画のズーム/中心)を毎フレーム渡す。need_buildがfalseで再構築を
        // 省くフレームでも、裏取得の近傍優先が最新の現在地を使えるよう常に更新しておく。
        loader.set_view(rcx, rcy, rz, &st.opts.style);

        // 再生開始直後、実画像モードならrw/rh/rz確定を待って先読みスレッドを起こす(1フレーム遅延)。
        // build_window(重い/ネットワーク)を裏で進めておき、メインは受け取った画像を使うだけにして
        // ちらつきを抑える。ASCII描画時はネットワーク待ちが無く不要なので起こさない。
        if st.play_wants_prefetch {
            st.play_wants_prefetch = false;
            if img_inline {
                if let Some(r) = st.spec.routes.last().filter(|r| r.pts.len() >= 2) {
                    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let speed_bits = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(st.play_speed.to_bits()));
                    let (tx, rx) = std::sync::mpsc::sync_channel(6);
                    let route_pts = r.pts.clone();
                    let style = st.opts.style.clone();
                    let (pw, ph, pz) = (rw, rh, rz);
                    let speed_kmh = st.cfg.route_play_speed_kmh;
                    // 再開位置。再生開始直後はplay=Some(0.0)なので先頭からになるが、ズーム変更に
                    // よる先読み再起動(restart_prefetch_on_zoom!)時はplayが現在の走行距離を持って
                    // いるので、そこから続ける(先頭に戻さない)。
                    let start_d = st.play.unwrap_or(0.0);
                    let cancel2 = std::sync::Arc::clone(&cancel);
                    let speed_bits2 = std::sync::Arc::clone(&speed_bits);
                    std::thread::spawn(move || {
                        let mut local_cache = Cache::new();
                        let total = roadtrace::polyline_len(&route_pts);
                        let mut d = start_d;
                        let dt = 0.08; // 80ms刻みで先読み(メインの再描画間隔に合わせる)
                        while d < total {
                            if cancel2.load(std::sync::atomic::Ordering::Relaxed) { break; }
                            let (la, lo) = roadtrace::point_at(&route_pts, d);
                            let (gx, gy) = deg_to_pixel(la, lo, pz);
                            if let Ok(img) = build_window(gx, gy, pz, pw, ph, &style, &mut local_cache) {
                                if tx.send((d, img)).is_err() { break; } // 受信側が止まったら終了
                            }
                            let speed = f64::from_bits(speed_bits2.load(std::sync::atomic::Ordering::Relaxed));
                            d += roadtrace::play_step_distance_m(speed_kmh, speed, dt);
                        }
                    });
                    st.play_cancel = cancel;
                    st.play_speed_bits = speed_bits;
                    st.play_prefetch_rx = Some(rx);
                    st.play_prefetch_held = None;
                }
            }
        }

        // 再描画判定シグネチャ。地図に効く状態(中心/ズーム/寸法/配置/オーバーレイ)が前回emit時と
        // 同じなら描き直さない。以前は実画像モード限定だったが、AAモードも同じ判定に乗せることで
        // タイル非同期ロード中(#35)にloader.generation()だけが変わり続けて毎ポーリング(80ms毎)
        // 無条件に全画面書き込みが発生する問題を解消する(macOS標準Terminal.appでの描画崩れの原因)。
        // このフレームの再描画判定に使うgeneration値をここで固定する。ローダーのワーカーは
        // 「cacheへinsert→generation加算→pending.inflightから除去」の順で動くため、この直後に
        // is_busy()を見る時点までの間に最後の1枚がちょうど着地すると、is_busy()はfalse(もう待たない)
        // だが今フレームの再構築には間に合っていない、という取りこぼしが起き得る(#53)。
        // その場合pollingがfalseになりevent::read()でブロックしてしまい、実際は届いているのに
        // 次のキー入力までLOADING表示が残り続ける。スナップショットして後段で比較し、その間に
        // 進んでいたら強制的にポーリング継続させることでこの1フレーム分の取りこぼしを防ぐ。
        let loader_gen_snapshot = loader.generation();
        let map_sig: Option<u64> = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            rcx.to_bits().hash(&mut h); rcy.to_bits().hash(&mut h);
            rz.hash(&mut h); rw.hash(&mut h); rh.hash(&mut h);
            gut.hash(&mut h); map_cols.hash(&mut h); map_rows.hash(&mut h);
            st.opts.style.hash(&mut h);
            // 裏取得でタイルが1枚届くたびに世代が変わる→sigが変わり次フレームで再構築され、
            // グレーのプレースホルダーが実タイルへ順次置き換わる。
            loader_gen_snapshot.hash(&mut h);
            st.spec.routes.len().hash(&mut h);
            st.spec.roads.len().hash(&mut h);
            st.spec.traffic_segments.len().hash(&mut h);
            for rt in st.spec.routes.iter().chain(st.spec.roads.iter()).chain(st.spec.traffic_segments.iter()) {
                rt.color.hash(&mut h); rt.thickness.hash(&mut h);
                for &(a2, b2) in &rt.pts { a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); }
            }
            st.spec.pois.len().hash(&mut h);
            for p in &st.spec.pois { p.lat.to_bits().hash(&mut h); p.lon.to_bits().hash(&mut h); (p.cat as u8).hash(&mut h); }
            st.spec.rings.len().hash(&mut h);
            for r in &st.spec.rings {
                r.lat.to_bits().hash(&mut h); r.lon.to_bits().hash(&mut h);
                r.color.hash(&mut h); r.thickness.hash(&mut h);
                for k in &r.radii_km { k.to_bits().hash(&mut h); }
            }
            st.spec.spots.len().hash(&mut h);
            for &(a2, b2, c2, s2) in &st.spec.spots { a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); c2.hash(&mut h); s2.hash(&mut h); }
            match st.gps_pos { Some((a2, b2)) => { 1u8.hash(&mut h); a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); } None => 0u8.hash(&mut h) }
            st.gps_trail.len().hash(&mut h);
            for &(a2, b2) in &st.gps_trail { a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); }
            st.wps.len().hash(&mut h);
            for &(a2, b2) in &st.wps { a2.to_bits().hash(&mut h); b2.to_bits().hash(&mut h); }
            st.wp_sel.hash(&mut h);
            // 雨雲レーダー: ON/OFF と表示中コマ(basetime+validtime)が変われば描き直す。
            // < > でコマを送ったとき、また targetTimes 更新で表示時刻が変わったときに効く。
            st.radar_on.hash(&mut h);
            if st.radar_on {
                if let Some(f) = st.radar_tl.get(st.radar_idx) { f.basetime.hash(&mut h); f.validtime.hash(&mut h); }
            }
            radar_opacity_value(&st.cfg).to_bits().hash(&mut h); // 濃さ(設定)を変えたら描き直す
            // プロットデータ: セル表が変わる(新しいセルが届く/期限切れで入れ替わる)たびに
            // 世代が進む。これを混ぜておかないと、位置もズームも動いていないフレームでは
            // sigが変わらず、届いたばかりの交通量/規制がオーバーレイに反映されない。
            st.traffic_layer.generation().hash(&mut h);
            st.roads_layer.generation().hash(&mut h);
            st.camera_layer.generation().hash(&mut h);
            st.regulation_layer.generation().hash(&mut h);
            st.disaster_layer.generation().hash(&mut h);
            Some(h.finish())
        };

        let mut map_img: Option<RgbImage> = None; // 実画像モードで描く overlay 合成済み画像
        // 状態が前回emitと同一なら、地図の再構築/再emit・AA再描画をスキップ(直近の描画を残す)。
        // 空文字を書いても既存セルは上書きされない(何も描かれない)ため、スキップ時は前フレームの
        // 内容がそのまま画面に残る(iTerm2の画像もAAの文字も同じ理屈で安全にスキップできる)。
        let need_build = st.force_reemit || st.last_map_sig != map_sig;
        // 先読みの受信(最新への間引き含む)はplayブロック側で既に行っている(表示位置と
        // ベース画像の位置を一致させるため)。ここではplay_prefetch_heldを読むだけ。
        let body = if !need_build {
            String::new()
        } else {
            let prefetched = if st.play.is_some() && img_inline { st.play_prefetch_held.as_ref().map(|(_, img)| img.clone()) } else { None };
            let built = match prefetched {
                Some(img) => Ok(img),
                // 非ブロッキング版: 未取得タイルはグレーで即返し、取得はローダーが裏で進める。
                None => build_window_nowait(rcx, rcy, rz, rw, rh, &st.opts.style, &loader),
            };
            match built {
                Ok(mut img) => {
                    // 雨雲レーダーの降水レイヤ。未取得タイルは透明のまま返る(グレー箱もLOADING
                    // 透かしも出さない)。視野が日本国外/広域すぎる場合は None = 何も重ねない。
                    let radar_layer: Option<RgbaImage> = if st.radar_on {
                        st.radar_tl.get(st.radar_idx)
                            .and_then(|f| build_radar_window_nowait(rcx, rcy, rz, rw, rh, f, &loader))
                    } else { None };
                    // 実画像モードはここで地図へ直接アルファ合成する(オーバーレイはこの後に焼くので
                    // 経路/POI/中心十字は常に雨雲より前面に残る)。
                    if img_inline {
                        if let Some(l) = &radar_layer { blend_rgba_over(&mut img, l, radar_opacity_value(&st.cfg)); }
                    }
                    // braille/edge は OverlayLayer へインクとして焼く(build_overlay の先頭で最背面に入る)。
                    let ink = if radar_ink {
                        radar_layer.as_ref().map(|l| RadarInk { layer: l, density: radar_opacity_value(&st.cfg) })
                    } else { None };
                    let mut ov = build_overlay(&st.spec, rcx, rcy, rz, rw, rh, 1.0, 1.0, rw, rh, ink);
                    let (mx, my) = (rw as i32 / 2, rh as i32 / 2); // 中心クロスヘア(色は設定で選択可)
                    let cross = SPOT_PALETTE[st.cfg.cross_color_idx as usize % SPOT_PALETTE.len()];
                    draw_line(&mut ov, mx - 6, my, mx + 6, my, cross, 1);
                    draw_line(&mut ov, mx, my - 6, mx, my + 6, cross, 1);
                    if st.gps_pos.is_some() { // ライブ現在地: トレイル(薄青)+自位置(赤)
                        for (tla, tlo) in &st.gps_trail {
                            let (gx, gy) = deg_to_pixel(*tla, *tlo, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            draw_ring(&mut ov, ix, iy, 1, [80, 160, 255], 1);
                        }
                        if let Some((gla, glo)) = st.gps_pos {
                            let (gx, gy) = deg_to_pixel(gla, glo, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            draw_ring(&mut ov, ix, iy, 4, [255, 60, 60], 2);
                        }
                    }
                    if !st.wps.is_empty() { // 選択中(Tab)の waypoint を白丸で強調
                        let s = st.wp_sel.min(st.wps.len() - 1);
                        let (gx, gy) = deg_to_pixel(st.wps[s].0, st.wps[s].1, rz);
                        let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                        let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                        draw_ring(&mut ov, ix, iy, 3, [255, 255, 255], 1);
                    }
                    if st.cfg.traffic_enabled { // 道路交通量(混雑度の目安。事故情報・渋滞度そのものではない)
                        // 観測点を最寄りの主要道路(major_roads)へスナップし、前後
                        // TRAFFIC_SNAP_RADIUS個ぶんの区間をラインとして色分け表示する。
                        // OSMのway分割は実測でかなり細かい(2026-08-16 東京都内サンプル:
                        // ノード間隔中央値約20m・90%タイル74m、1way平均7ノード≒100〜150m)。
                        // wayを跨いで延長する処理は入れていないため、線の長さはway次第で
                        // ばらつく(短いwayなら前後にはみ出さずクランプされる)。
                        // major_roadsが空(未取得中・取得失敗直後)の間は、従来通り観測点を
                        // 丸で表示するフォールバックにする。
                        const TRAFFIC_SNAP_RADIUS: usize = 15;
                        // 周囲に主要道路データが無い観測点を無関係な遠い道へ誤ってスナップしない
                        // ための上限。500m以内に主要道路の頂点が無ければ点表示のフォールバックへ回る。
                        const TRAFFIC_SNAP_MAX_DIST_M: f64 = 500.0;
                        for p in &traffic_points {
                            let color = match traffic::classify(p.volume) {
                                traffic::CongestionLevel::Light => [80, 200, 90],
                                traffic::CongestionLevel::Moderate => [230, 200, 40],
                                traffic::CongestionLevel::Heavy => [220, 50, 40],
                            };
                            let seg = roadtrace::nearest_way_segment_within(&major_roads, (p.lat, p.lon), TRAFFIC_SNAP_RADIUS, TRAFFIC_SNAP_MAX_DIST_M);
                            if seg.len() >= 2 {
                                let pts: Vec<(i32, i32)> = seg.iter().map(|&(la, lo)| {
                                    let (gx, gy) = deg_to_pixel(la, lo, rz);
                                    ((gx - (rcx - rw as f64 / 2.0)).floor() as i32, (gy - (rcy - rh as f64 / 2.0)).floor() as i32)
                                }).collect();
                                for w in pts.windows(2) { draw_line(&mut ov, w[0].0, w[0].1, w[1].0, w[1].1, color, 3); }
                            } else {
                                let (gx, gy) = deg_to_pixel(p.lat, p.lon, rz);
                                let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                                let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                                draw_ring(&mut ov, ix, iy, 3, color, 3);
                            }
                        }
                    }
                    if st.cfg.camera_enabled { // 道路ライブカメラ(紫系。Nで中心近くのカメラの写真を表示)
                        for c in &camera_points {
                            let (gx, gy) = deg_to_pixel(c.lat, c.lon, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            draw_ring(&mut ov, ix, iy, 3, [170, 90, 220], 2);
                        }
                    }
                    if st.cfg.regulation_enabled { // 通行規制(通行止め/車線規制等の区間を種別ごとの色で線描画)
                        for ev in &regulation_events {
                            let pts: Vec<(i32, i32)> = ev.line.iter().map(|&(la, lo)| {
                                let (gx, gy) = deg_to_pixel(la, lo, rz);
                                ((gx - (rcx - rw as f64 / 2.0)).floor() as i32, (gy - (rcy - rh as f64 / 2.0)).floor() as i32)
                            }).collect();
                            for w in pts.windows(2) { draw_line(&mut ov, w[0].0, w[0].1, w[1].0, w[1].1, ev.kind.color(), 3); }
                            // 規制原因アイコン(#規制原因アイコン): 事故✕/工事のみ、区間の中点に重ね描き。
                            if let Some(category) = st.cause_cache.get(&ev.detail_id) {
                                if let Some((color, shape)) = regulation::cause_icon(*category) {
                                    if let Some((la, lo)) = closure_icon_position(&ev.line) {
                                        let (gx, gy) = deg_to_pixel(la, lo, rz);
                                        let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                                        let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                                        draw_marker(&mut ov, ix, iy, color, 4, shape);
                                    }
                                }
                            }
                        }
                    }
                    if st.cfg.disaster_enabled { // 過去災害(Bでその地点の事例一覧)
                        // 座標が市区町村の代表点で1点に何十件も重なるため、事例1件=1マーカーには
                        // しない。1座標=1マーカーにして、件数を外周リングの半径、最も多い種別を
                        // 色で表す。外周を細くするのは地図と他レイヤを覆い隠さないため
                        // (中心の塊があるので細くても位置は読める)。
                        for s in &disaster_sites {
                            let (gx, gy) = deg_to_pixel(s.lat, s.lon, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            let color = s.dominant().color();
                            draw_ring(&mut ov, ix, iy, 1, color, 2);
                            draw_ring(&mut ov, ix, iy, disaster::marker_radius(s.total()), color, 1);
                        }
                    }
                    st.last_map_sig = map_sig; // このsigで描いた内容がこのフレームでemitされる
                    if img_inline {
                        // 実画像モード: 取得画像に overlay を焼き込んで保持し、AA文字列は空にする。
                        let mut c = img;
                        composite(&mut c, &ov);
                        map_img = Some(c);
                        String::new()
                    } else {
                        // インク経路(braille/edge)は ov に入れ済みなので render 側では合成しない。
                        // halfblock/classify はここで渡し、classify は recolor 後に混ざる。
                        let rd = if radar_ink { None } else { radar_layer.as_ref().map(|l| (l, radar_opacity_value(&st.cfg))) };
                        render(&img, &st.opts, Some(&ov), rd)
                    }
                }
                Err(e) => {
                    st.last_map_sig = None; // 失敗時は次フレームで再取得
                    format!("取得失敗: {e}\r\n")
                }
            }
        };
        st.force_reemit = false; // 強制再emitは消費済み(image_inlineの被り解消は下でmap_coveredが再設定)

        // 左袖リスト(POI か お気に入り)の各行を組む。組み立ては ui_gutter.rs へ切り出し済み。
        let glines: Vec<String> = ui_gutter::build_gutter_lines(&ui_gutter::GutterCtx {
            gut, map_rows, focus: &st.focus,
            show_menu, show_route, show_wps, show_splist, show_catlist, show_settings,
            show_poimenu, show_routes, show_favmenu, show_roadlist,
            menu_cat_sel: st.menu_cat_sel, menu_item_sel: st.menu_item_sel,
            wps: &st.wps, route_sel: st.route_sel, grab: st.grab, wp_sel: st.wp_sel,
            spots: &st.spots, cur_cat: &st.cur_cat, sp_sel: st.sp_sel, lat, lon,
            spot_cats: &st.spot_cats, cat_sel: st.cat_sel,
            opts: &st.opts, cfg: &st.cfg, set_sel: st.set_sel, set_pick_sel: st.set_pick_sel,
            poi_kinds: &st.poi_kinds, poimenu_sel: st.poimenu_sel,
            route_names: &st.route_names, rn_sel: st.rn_sel,
            road_segs: &st.road_segs, road_sel: st.road_sel,
            pois: &st.pois, poi_label: &st.poi_label, poi_sel: st.poi_sel,
        }, &mut st.list_offset);

        // 左袖 + 地図 を絶対座標で配置
        let _ = write!(out, "\x1b[H");
        let lines: Vec<&str> = body.split("\r\n").collect();
        let blank = fit_cells("", gut as usize);
        for i in 0..map_rows as usize {
            let ln = lines.get(i).copied().unwrap_or("");
            if gut > 0 {
                let g = glines.get(i).cloned().unwrap_or_else(|| blank.clone());
                write!(out, "\x1b[{};1H{}\x1b[{};{}H{}", i + 1, g, i + 1, gut + 1, ln)?;
            } else {
                write!(out, "\x1b[{};1H{}", i + 1, ln)?;
            }
        }
        if let Some(mi) = &map_img { // 実画像モード: 地図領域の左上セルへ移動してインライン画像を出力
            let _ = write!(out, "\x1b[1;{}H", gut + 1);
            let _ = emit_iterm2_image(&mut out, mi, map_cols, map_rows);
            // インライン画像はscrollbackに積もりiTermのメモリを肥大させる(Cmd+Kのclear buffer相当)。
            // 可視画面は変えず、一定枚数emitごとにscrollbackだけ捨てて自動で溜め込みを防ぐ。
            st.emit_count += 1;
            if st.emit_count % 40 == 0 { let _ = write!(out, "\x1b[3J"); }
        }
        if elev_h > 0 { // 標高プロファイル帯(地図の下・ステータスの上)。描画は ui_overlay.rs へ切り出し済み
            ui_overlay::draw_elevation_band(&mut out, cols, map_rows, elev_h, &st.route_ele, st.route_ascend, &st.spec, lat, lon);
        }
        // ステータス行の文面組み立ては ui_status.rs へ切り出し済み。通信中スピナーの判定に使う
        // 各ジョブは有無しか見ないのでここで1つのフラグに畳んでから渡す。
        let jobs_active = st.route_job.is_some() || st.search_job.is_some() || st.near_job.is_some() || st.street_job.is_some() || st.cam_job.is_some() || st.recommend_job.is_some() || st.road_job.is_some() || st.catpoi_job.is_some() || st.wander_job.is_some() || st.disaster_job.is_some() || st.regulation_detail_job.is_some() || st.traffic_color_job.is_some() || st.cause_job.is_some();
        // 次の曲がり角の画面表示。音声案内(maybe_speak_turn)と同じくturn_points+現在地から
        // 求めるが、読み上げ済みかの状態は見ない(何度描画しても同じ内容を出したいため)。
        let next_turn = st.spec.routes.last()
            .and_then(|rt| route::progress_along_route((lat, lon), &rt.pts))
            .and_then(|progress_m| voice::next_turn_display(&st.turn_points, progress_m))
            .map(|(remaining, phrase)| format!("↳{}m {phrase} ", voice::round_to_50(remaining)));
        let status = ui_status::build_status_line(ui_status::StatusCtx {
            focus: &st.focus, save_confirm: &st.save_confirm, spot_move_confirm: st.spot_move_confirm, spots: &st.spots,
            cur_cat: &st.cur_cat, pending_spot: st.pending_spot.is_some(), set_sel: st.set_sel, poi_label: &st.poi_label,
            route_note: &st.route_note, clear_route_confirm: st.clear_route_confirm, jobs_active, spin: st.spin,
            gps_live: st.gps_rx.is_some(), web_gps_active: st.web_gps_active, play: st.play, play_speed: st.play_speed,
            radar_on: st.radar_on, radar_tl: &st.radar_tl, radar_idx: st.radar_idx, radar_follow: st.radar_follow,
            loader: &loader, rcx, rcy, rz, rw, rh,
            cfg: &st.cfg,
            // 主要道路は交通量のスナップ下地で、それ自体はステータスに出さない(描画も未実装)。
            traffic: ui_status::PlotStatus {
                count: traffic_points.len(),
                job_active: st.traffic_layer.job_active() || st.roads_layer.job_active(),
                stale_age_secs: st.traffic_layer.stale_age_secs(plot_now),
                wide_area: st.traffic_layer.suppressed(),
            },
            camera: ui_status::PlotStatus {
                count: camera_points.len(),
                job_active: st.camera_layer.job_active(),
                stale_age_secs: st.camera_layer.stale_age_secs(plot_now),
                wide_area: st.camera_layer.suppressed(),
            },
            regulation: ui_status::PlotStatus {
                count: regulation_events.len(),
                job_active: st.regulation_layer.job_active(),
                stale_age_secs: st.regulation_layer.stale_age_secs(plot_now),
                wide_area: st.regulation_layer.suppressed(),
            },
            // 過去災害は事例数でなく地点数を出す(1地点に最大166件が重なるため)。
            // 事例一覧(Bキー)の取得中もスピナーではなくこのレイヤの表示で分かるようにする。
            disaster: ui_status::PlotStatus {
                count: disaster_sites.len(),
                job_active: st.disaster_layer.job_active() || st.disaster_job.is_some(),
                stale_age_secs: st.disaster_layer.stale_age_secs(plot_now),
                wide_area: st.disaster_layer.suppressed(),
            },
            addr: &st.addr, wps: &st.wps, z: st.z, lat, lon, next_turn: &next_turn,
        });
        let status = fit_cells_scroll(&status, cols as usize, st.spin);
        write!(out, "\x1b[{};1H\x1b[7m{status}\x1b[0m", tr)?;

        // 中央に重ねるパネル/ポップアップ類の描画は ui_overlay.rs へ切り出し済み。
        if st.quit_confirm { ui_overlay::draw_quit_confirm(&mut out, cols, map_rows); }
        if let Some(msg) = &st.popup { ui_overlay::draw_popup(&mut out, cols, map_rows, msg); }
        if let Some((title, lines)) = &st.disaster_view {
            ui_overlay::draw_disaster_panel(&mut out, cols, map_rows, title, lines, disaster::truncation_seen());
        }
        if let Some((title, lines)) = &st.regulation_detail_view {
            ui_overlay::draw_regulation_detail_panel(&mut out, cols, map_rows, title, lines);
        }
        if let Some(QrView::Text(q)) = &st.qr_view { ui_overlay::draw_qr_text(&mut out, cols, map_rows, tr, q); }
        if let Some(QrView::Image(img)) = &st.qr_view { ui_overlay::draw_qr_image(&mut out, cols, map_rows, tr, img); }
        if let Focus::SpotForm { name, url, field } = &st.focus { ui_overlay::draw_spot_form(&mut out, cols, map_rows, name, url, *field, st.input_cur, &st.cur_cat); }
        if let Focus::PoiKindForm { label, tag, field } = &st.focus { ui_overlay::draw_poi_kind_form(&mut out, cols, map_rows, label, tag, *field, st.input_cur); }
        if let Focus::WanderForm { dist_km } = &st.focus { ui_overlay::draw_wander_form(&mut out, cols, map_rows, *dist_km); }
        ui_overlay::draw_text_input(&mut out, cols, map_rows, &st.focus, st.input_cur);
        if let Focus::ColorPick { .. } = &st.focus { ui_overlay::draw_color_pick(&mut out, cols, map_rows, st.color_sel); }
        if let Focus::ShapePick { .. } = &st.focus { ui_overlay::draw_shape_pick(&mut out, cols, map_rows, st.shape_sel); }
        if st.onboard { ui_overlay::draw_onboarding(&mut out, cols, map_rows); }
        // 地図矩形を覆う中央オーバーレイ/パネルが「閉じた」フレーム(エッジ)でだけ画像を再emitして
        // 残像を消す。覆われている間(検索文字入力中など)は毎打鍵で強制再emitしない(メモリ/負荷対策)。
        let map_covered = st.popup.is_some() || st.qr_view.is_some() || st.onboard || st.quit_confirm || st.disaster_view.is_some() || st.regulation_detail_view.is_some()
            || matches!(st.focus,
                Focus::SpotForm { .. } | Focus::Search(_) | Focus::SaveName(_) | Focus::NearSearch(_)
                | Focus::NewCat(_) | Focus::RoadSearch(_) | Focus::Recommend(_)
                | Focus::SpotRename(..) | Focus::SpotEditName(..) | Focus::ColorPick { .. } | Focus::ShapePick { .. } | Focus::SettingsEdit(..) | Focus::PoiKindForm { .. } | Focus::WanderForm { .. });
        if st.prev_map_covered && !map_covered { st.force_reemit = true; }
        st.prev_map_covered = map_covered;
        // web版(ブラウザ)へ現在のドラッグ軸モードを通知する(#87 設計書 §5.2)。Focus は
        // interactive() 内の30か所以上で書き換わり、非同期ジョブの完了で勝手に変わる箇所も
        // ある(例: 周辺検索の結果適用で Map → PoiList)。変更箇所ごとに通知を足すのではなく
        // フレーム末で前回値と比較する方式にして、呼び出しをこの1か所に閉じている。
        // 認識しない端末(通常のターミナル)では無視されるだけなので、web以外でも害は無い。
        let cur_drag_axes = dragmode::axes(&st.focus);
        if st.prev_drag_axes != Some(cur_drag_axes) || st.drag_mode_req_pending {
            dragmode::emit_web_drag_mode(cur_drag_axes);
            st.prev_drag_axes = Some(cur_drag_axes);
            st.drag_mode_req_pending = false;
        }
        out.flush()?;

        // バックグラウンドジョブの結果を毎フレーム取り込む(route/search/near/street/recommend)。
        // Ok=適用しjob=None / Empty=保持 / Disconnected=None。結果を適用したフレームはブロックせず即再描画する。
        use std::sync::mpsc::TryRecvError;
        let mut got_result = false;
        if st.route_job.is_some() {
            match st.route_job.as_ref().unwrap().try_recv() {
                Ok(Ok(r)) => {
                    st.spec.routes.clear();
                    st.spec.traffic_segments.clear(); // 古いルートの色分けを引き継がない
                    st.route_note = Some(route_summary(&st.mode, &r));
                    // 通行止め回避が件数上限で一部反映できなかった場合、黙って進めると
                    // 「回避できた」と誤解されるのでひとこと添える。
                    if route_nogos_truncated {
                        st.route_note = st.route_note.map(|n| format!("{n} (通行止めの一部は回避対象外)"));
                    }
                    // 渋滞状況の色分け(#渋滞情報): ルートが変わるたびに問い合わせ直す。
                    st.traffic_color_job = if st.cfg.route_traffic_enabled && !st.cfg.google_maps_api_key.trim().is_empty() && r.pts.len() >= 2 {
                        Some(route::trigger_traffic_coloring(&r.pts, &st.mode, &st.cfg.google_maps_api_key))
                    } else {
                        None
                    };
                    st.route_ele = r.ele;
                    st.route_ascend = r.ascend_m;
                    let tile_coords = geo::route_tile_coords(&r.pts, st.z);
                    loader.request_route_tiles(&st.opts.style, st.z, &tile_coords);
                    // ルートが変わった(=曲がり角も変わりうる)ので、音声案内の状態は一旦捨てる。
                    // 取得は ON にした人だけがBRouterへ追加問い合わせする(既定OFF)。
                    st.turn_points = Vec::new();
                    st.voice_guide = None;
                    if st.cfg.voice_guide_enabled {
                        st.turn_job = Some(trigger_turn_points(&st.wps, &st.mode, 0, &r.pts, &route_nogos));
                    }
                    st.spec.routes.push(Route { pts: r.pts, color: [0, 220, 255], thickness: 2 });
                    st.route_job = None; got_result = true;
                }
                Ok(Err(e)) => { st.route_note = Some(format!("({e})")); st.route_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.route_job = None; got_result = true; }
            }
        }
        if st.turn_job.is_some() {
            match st.turn_job.as_ref().unwrap().try_recv() {
                Ok(v) => { st.turn_points = v; st.voice_guide = Some(voice::VoiceGuide::new(&st.turn_points)); st.turn_job = None; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.turn_job = None; }
            }
        }
        // プロットデータ4種の取得。各レイヤが「視野を覆うセルのうち、fresh なものが手元に
        // 無いぶん」だけを1本のジョブで取りに行き、ディスクの読み書きもそのジョブの中で行う
        // (詳細は plotlayer.rs)。ここは毎フレーム tick して、セル表が変わったら即座に描き直す。
        // OFFのレイヤも tick は呼ぶ(走っていたジョブを取りこぼさず畳むため)。
        // 主要道路(#73)は交通量の観測点をラインへスナップする下地なので交通量と同じ条件で回す。
        got_result |= st.traffic_layer.tick(st.cx, st.cy, st.z, st.cfg.traffic_enabled);
        got_result |= st.roads_layer.tick(st.cx, st.cy, st.z, st.cfg.traffic_enabled);
        got_result |= st.camera_layer.tick(st.cx, st.cy, st.z, st.cfg.camera_enabled);
        got_result |= st.regulation_layer.tick(st.cx, st.cy, st.z, st.cfg.regulation_enabled);
        got_result |= st.disaster_layer.tick(st.cx, st.cy, st.z, st.cfg.disaster_enabled);
        if let Some(job) = &st.disaster_job { // Bキーで頼んだ事例一覧(2段目)の到着
            match job.try_recv() {
                Ok(Ok(panel)) => { st.disaster_view = Some(panel); st.disaster_job = None; got_result = true; }
                Ok(Err(e)) => { st.snd.play("error"); st.addr = format!("災害事例: {e}"); st.disaster_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.disaster_job = None; }
            }
        }
        if let Some(job) = &st.regulation_detail_job { // Tキーで頼んだ規制詳細の到着
            match job.try_recv() {
                Ok(Ok(d)) => { st.regulation_detail_view = Some(regulation::detail_panel_content(&d)); st.regulation_detail_job = None; got_result = true; }
                Ok(Err(e)) => { st.snd.play("error"); st.addr = format!("通行規制: {e}"); st.regulation_detail_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.regulation_detail_job = None; }
            }
        }
        if let Some(job) = &st.traffic_color_job { // 渋滞状況の色分け(#渋滞情報)の到着
            match job.try_recv() {
                Ok(segs) => {
                    if !segs.is_empty() {
                        st.spec.traffic_segments = segs.into_iter().map(|(color, pts)| Route { pts, color, thickness: 2 }).collect();
                        st.route_note = st.route_note.map(|n| format!("{n} (渋滞あり: 黄/赤)"));
                    } // 空(失敗・APIキー無し等)なら単色ルート線のまま静かに諦める
                    st.traffic_color_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.traffic_color_job = None; }
            }
        }
        if let Some(job) = &st.cause_job { // 規制原因アイコン(#規制原因アイコン)の分類結果到着
            match job.try_recv() {
                Ok((id, result)) => {
                    // 失敗時もOther相当でキャッシュする(でないと同じ1件を毎フレーム
                    // 再試行し続け、cause_jobが常にSomeになってレート制限が効かなくなる)。
                    let category = result.map(|d| regulation::categorize_cause(&d.cause)).unwrap_or(regulation::CauseCategory::Other);
                    st.cause_cache.insert(id, category);
                    st.cause_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.cause_job = None; }
            }
        }
        if let Some(job) = &st.voice_preview_job { // 読み上げの声(#78)の試聴結果
            match job.try_recv() {
                Ok(Ok(())) => { st.voice_preview_job = None; got_result = true; }
                Ok(Err(e)) => { st.snd.play("error"); st.addr = e; st.voice_preview_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.voice_preview_job = None; }
            }
        }
        if let Some(job) = &st.cam_job {
            match job.try_recv() {
                Ok((c, Ok(img))) => { st.cam_view = Some((img, c)); st.cam_job = None; }
                Ok((_, Err(e))) => { st.addr = format!("カメラ画像取得失敗: {e}"); st.cam_job = None; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.cam_job = None; }
            }
        }
        if st.search_job.is_some() {
            match st.search_job.as_ref().unwrap().try_recv() {
                Ok((ckey, q, res)) => {
                    match res {
                        Err(e) => { st.snd.play("error"); st.addr = format!("検索できません（{e}）"); }
                        Ok(v) if v.is_empty() => { st.snd.play("error"); st.addr = format!("見つからない: {q}"); }
                        Ok(v) => {
                            let now = searchcache::now_secs();
                            st.scache.insert(ckey, searchcache::CacheEntry { results: v.clone(), created_at: now, last_used_at: now });
                            let _ = searchcache::save(&st.scache);
                            st.pois = v.into_iter().take(8).map(|(la, lo, nm)| (la, lo, nm, PoiCat::Waypoint)).collect();
                            st.poi_sel = 0;
                            st.poi_label = format!("検索:{q}");
                            set_markers(&mut st.spec, &st.wps, &st.pois);
                            if matches!(st.focus, Focus::Map) { st.focus = Focus::PoiList; } // 別画面へ移っていたら奪わない
                        }
                    }
                    st.search_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.search_job = None; got_result = true; }
            }
        }
        if st.near_job.is_some() {
            match st.near_job.as_ref().unwrap().try_recv() {
                Ok((q, res)) => {
                    // ローカルの★スポット一致(距離順)を先頭、Overpass結果(距離順)を後ろにマージ。
                    // Overpassが障害の場合でも★一致だけは出す(0件=該当なしと障害を混同しない)。
                    let ql = q.to_lowercase();
                    let mut mine: Vec<(f64, f64, String, PoiCat)> = st.spots.iter()
                        .filter(|s| s.name.to_lowercase().contains(&ql))
                        .map(|s| (s.lat, s.lon, format!("★{}", s.name), PoiCat::Home)).collect();
                    mine.sort_by(|p, r| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (r.0, r.1))).unwrap_or(std::cmp::Ordering::Equal));
                    match res {
                        Ok(osm) => {
                            let mut got: Vec<(f64, f64, String, PoiCat)> = osm.into_iter().map(|(a, b, nm)| (a, b, nm, PoiCat::Other)).collect();
                            got.sort_by(|p, r| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (r.0, r.1))).unwrap_or(std::cmp::Ordering::Equal));
                            mine.extend(got);
                            if mine.is_empty() { st.snd.play("error"); st.addr = format!("周辺に無し: {q}"); }
                            else {
                                st.pois = mine; st.poi_sel = 0; st.poi_label = format!("周辺:{q}");
                                set_markers(&mut st.spec, &st.wps, &st.pois);
                                if matches!(st.focus, Focus::Map) { st.focus = Focus::PoiList; }
                            }
                        }
                        Err(e) => {
                            st.snd.play("error");
                            if mine.is_empty() {
                                st.addr = format!("周辺検索: {e}"); // 障害。「該当なし」と文言を分ける
                            } else {
                                st.addr = format!("★のみ表示({e})");
                                st.pois = mine; st.poi_sel = 0; st.poi_label = format!("周辺:{q}");
                                set_markers(&mut st.spec, &st.wps, &st.pois);
                                if matches!(st.focus, Focus::Map) { st.focus = Focus::PoiList; }
                            }
                        }
                    }
                    st.near_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.near_job = None; got_result = true; }
            }
        }
        if st.road_job.is_some() {
            match st.road_job.as_ref().unwrap().try_recv() {
                Ok((name, res)) => {
                    match res {
                        Ok(frags) if !frags.is_empty() => {
                            let rf: Vec<roadtrace::RoadFrag> = frags.into_iter().map(|(pts, oneway)| roadtrace::RoadFrag { pts, oneway }).collect();
                            let poly = roadtrace::assemble_polyline(&rf);
                            let seg = roadtrace::nearest_segment(&poly, (lat, lon), 500.0);
                            if seg.len() >= 2 {
                                let color = road_color_for(st.road_segs.len());
                                st.road_segs.push(RoadSeg { name: name.clone(), color, pts: seg });
                                st.sync_roads();
                                st.addr = format!("道路: {name} を塊で追加(計{}本)", st.road_segs.len());
                            } else { st.addr = "道路: 点が足りない(拡大/移動して再検索)".into(); }
                        }
                        Ok(_) => st.addr = format!("道路が見つからない: {name}(view内に無い)"),
                        Err(e) => st.addr = format!("道路: {e}"),
                    }
                    st.road_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.road_job = None; got_result = true; }
            }
        }
        if st.catpoi_job.is_some() {
            match st.catpoi_job.as_ref().unwrap().try_recv() {
                Ok((label, res)) => {
                    match res {
                        Ok(items) if !items.is_empty() => { st.pois = items; st.poi_sel = 0; st.poi_label = label; set_markers(&mut st.spec, &st.wps, &st.pois); st.focus = Focus::PoiList; }
                        Ok(_) => { st.snd.play("error"); st.addr = format!("周辺2kmに{label}無し"); if matches!(st.focus, Focus::Map) { st.focus = Focus::PoiMenu; } }
                        Err(e) => { st.addr = format!("({e})"); if matches!(st.focus, Focus::Map) { st.focus = Focus::PoiMenu; } }
                    }
                    st.catpoi_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.catpoi_job = None; got_result = true; }
            }
        }
        if st.wander_job.is_some() {
            match st.wander_job.as_ref().unwrap().try_recv() {
                Ok(res) => {
                    match res {
                        Ok(w) => { st.wps = w; st.wp_sel = 0; st.route_sel = 0; let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_; }
                        Err(e) => { st.snd.play("error"); st.addr = format!("({e})"); }
                    }
                    st.wander_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.wander_job = None; got_result = true; }
            }
        }
        if st.street_job.is_some() {
            match st.street_job.as_ref().unwrap().try_recv() {
                Ok((la, lo, hd, res)) => {
                    match res {
                        Ok(img) => { st.street = Some((img, hd, la, lo)); st.addr.clear(); }
                        Err(e) => st.addr = format!("実写: {e}"),
                    }
                    st.street_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.street_job = None; got_result = true; }
            }
        }
        // 雨雲レーダーの時刻一覧(5分ごと)。届いていれば最新の1件だけを採用する。
        // targetTimes は更新のたびに basetime が動き、古いコマは JMA 側から消えるため、
        // 表示位置は index でなく直前に見ていた validtime を基準に取り直す(reanchor)。
        if let Some(rc) = &st.radar_clock {
            let mut latest: Option<radar::Timeline> = None;
            while let Ok(tl) = rc.rx.try_recv() { latest = Some(tl); }
            if let Some(tl) = latest {
                let prev_vt = st.radar_tl.get(st.radar_idx).map(|f| f.validtime.clone());
                let (idx, follow, msg) = tl.reanchor(prev_vt.as_deref(), st.radar_follow);
                st.radar_tl = tl;
                st.radar_idx = idx;
                st.radar_follow = follow;
                if let Some(m) = msg { st.addr = format!("雨雲: {m}"); }
                // 一覧から消えたコマのタイルはもう取得できない。キャッシュと取得キューから捨てる。
                loader.drop_radar_frames_except(&st.radar_tl.frames);
                got_result = true;
            }
        }
        if st.recommend_job.is_some() {
            match st.recommend_job.as_ref().unwrap().try_recv() {
                Ok(res) => {
                    match res {
                        Ok(v) if v.is_empty() => st.addr = "おすすめ: 実在確認できる地点なし".into(),
                        Ok(v) => {
                            st.pois = v.into_iter().map(|(la, lo, nm)| (la, lo, nm, PoiCat::Home)).collect();
                            st.poi_sel = 0; st.poi_label = "おすすめ".into();
                            set_markers(&mut st.spec, &st.wps, &st.pois);
                            if matches!(st.focus, Focus::Map) { st.focus = Focus::PoiList; }
                        }
                        Err(e) => st.addr = format!("おすすめ: {e}"),
                    }
                    st.recommend_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { st.recommend_job = None; got_result = true; }
            }
        }

        // 入力待ち。結果適用直後は即再描画(None)。ジョブ/GPS/再生/移動settling中はポーリング。
        // settling中は短間隔(60ms)で見に行き、動きが止まったフレームで高解像度に上げ直す。
        // ローダーがまだ未取得タイルを抱えている間もポーリング側に倒す(read()でブロックすると
        // 無入力時に届いたタイルが画面へ反映されないため)。
        // is_busy()に加えgenerationのスナップショット比較も見る(#53): このフレームの再構築後、
        // is_busy()を読むまでの間に最後の1枚がちょうど着地しinflightが空になっていた場合、
        // is_busy()だけではその1枚の反映漏れを検知できずread()でブロックしてしまうため。
        let polling = st.route_job.is_some() || st.search_job.is_some() || st.near_job.is_some() || st.street_job.is_some() || st.cam_job.is_some() || st.recommend_job.is_some() || st.road_job.is_some() || st.catpoi_job.is_some() || st.wander_job.is_some() || st.gps_rx.is_some() || st.play.is_some() || settling || loader.is_busy() || loader.generation() != loader_gen_snapshot
            || st.radar_clock.is_some() // 雨雲: 背景ポーラーからの時刻一覧を取りこぼさない
            // 道路交通量/主要道路/ライブカメラ/通行規制の背景取得完了を、キー入力無しでも
            // 取りこぼさない(結果が最大60秒(IDLE_SAVE_INTERVAL)反映されない事故を防ぐ)。
            // 主要道路は以前この条件から漏れていたが、4レイヤとも同じ扱いにする。
            || st.traffic_layer.job_active() || st.roads_layer.job_active()
            || st.camera_layer.job_active() || st.regulation_layer.job_active() || st.disaster_layer.job_active()
            || st.disaster_job.is_some() || st.voice_preview_job.is_some() || st.regulation_detail_job.is_some() || st.traffic_color_job.is_some() || st.cause_job.is_some();
        let mut ev: Option<Event> = if got_result {
            None
        } else if polling {
            let ms = if settling { 60 } else { 80 };
            if event::poll(std::time::Duration::from_millis(ms))? { Some(event::read()?) } else { None }
        } else if event::poll(IDLE_SAVE_INTERVAL)? {
            Some(event::read()?)
        } else {
            // 無操作がIDLE_SAVE_INTERVALだけ続いた。read()で無限ブロックする代わりにpollで
            // 区切り、強制終了/クラッシュに備えて状態を保存する(#69)。キー入力があれば
            // pollは即trueを返すため応答性への影響は無い。
            persist_full_state(st.cx, st.cy, st.z, &st.opts, &st.wps, &st.mode, &mut st.cfg, st.radar_on, st.show_spots);
            // ついでにプロットキャッシュの掃除もここで起こす。プロットデータは取得のたびに
            // その場で1ファイル書いているので「保存待ち」は無く、フラッシュする対象は無い。
            // 一方GCはディレクトリ走査を伴うので無操作中に回すのが都合がよい。
            // ここへ来るのはジョブが1本も走っていない時だけなので取得とも競合しない。
            if !st.plot_gc_done {
                st.plot_gc_done = true; // 1セッション1回だけ
                std::thread::spawn(plotcache::gc);
            }
            None
        };
        // 押しっぱなし/連打でパン系イベントが溜まっている間は、都度の再描画を待たずに
        // 溜まった分を最新の1個へ間引く(SSH等で1回の再描画に往復が乗ると、律速して
        // メニュー操作等の割り込みが後回しになるため)。別系統のキーが混ざっていたら
        // 間引きを止めてそちらを即座に優先する。
        if matches!(st.focus, Focus::Map) {
            if let Some(Event::Key(first)) = &ev {
                let is_pan_key = |c: KeyCode| matches!(c, KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Char('h') | KeyCode::Char('j') | KeyCode::Char('k') | KeyCode::Char('l'));
                if is_pan_key(first.code) {
                    while let Ok(true) = event::poll(std::time::Duration::from_millis(0)) {
                        match event::read()? {
                            Event::Key(next) if is_pan_key(next.code) => ev = Some(Event::Key(next)),
                            other => { ev = Some(other); break; }
                        }
                    }
                }
            }
        }
        // web版(ブラウザ)からのパン量マーカー(#87 設計書 §6.3)。上のキー間引きは「溜まった分を
        // 最新1個で上書き」=捨てる方式だが、パン量は相対値なので足し合わせれば取りこぼしがゼロに
        // なる。描画が遅れて数フレーム分溜まっても、指を離した時点の位置に必ず追いつく。
        // ev を別イベントで上書きしても合算値はこの変数に残るので、途中で別のキーが割り込んでも
        // 移動分は失われない。Focus::Map 以外(PoiList の横パン等)でも効かせるためキー間引きの
        // 内側には置かない。
        let mut pan_fx = 0.0f64;
        let mut pan_fy = 0.0f64;
        let mut got_pan = false;
        if matches!(&ev, Some(Event::Paste(s)) if s.starts_with(dragmode::PAN_MARKER)) {
            if let Some(Event::Paste(s)) = &ev {
                if let Some((fx, fy)) = dragmode::parse_pan_marker(s) { pan_fx += fx; pan_fy += fy; }
            }
            got_pan = true;
            ev = None; // このイベントは消費済み。以降の match へ素通しさせない(検索欄への誤入力防止)
            while let Ok(true) = event::poll(std::time::Duration::from_millis(0)) {
                match event::read()? {
                    Event::Paste(s) if s.starts_with(dragmode::PAN_MARKER) => {
                        if let Some((fx, fy)) = dragmode::parse_pan_marker(&s) { pan_fx += fx; pan_fy += fy; }
                    }
                    other => { ev = Some(other); break; }
                }
            }
        }
        if got_pan {
            // 軸ゲート・向きの反転・座標の正規化は dragmode::apply_pan に閉じてある
            // (ここに直書きするとテストが書けないため。設計書 §6.2 の適用条件)。
            let lay = dragmode::Layout { cols, rows: tr as u32, map_cols, map_rows, ow, oh };
            let (ncx, ncy, moved) = dragmode::apply_pan(st.cx, st.cy, st.z, dragmode::axes(&st.focus), pan_fx, pan_fy, &lay);
            if moved {
                st.cx = ncx;
                st.cy = ncy;
                // 中心が動いたので、'a'で引いた住所表示は古くなる。矢印キーでのパンと同じく
                // 地図フォーカスのときだけ消す(PoiListの微パンはキー経路でも消していない)。
                if matches!(st.focus, Focus::Map) { st.addr.clear(); }
                // キーボードの加速(pan_streak)と混ざらないようリセットする。ドラッグは
                // 指の移動量そのものが移動量なので、加速を掛けると1:1でなくなる。
                st.pan_streak = 0;
                st.last_pan_dir = None;
            }
        }
        match ev {
            None => {} // 再描画のみ(計算待ち)
            Some(Event::Key(k)) if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl-C: 進行中の全ジョブを中断(アプリは終了しない)
                let any = st.route_job.is_some() || st.search_job.is_some() || st.near_job.is_some() || st.street_job.is_some() || st.cam_job.is_some() || st.recommend_job.is_some() || st.road_job.is_some() || st.catpoi_job.is_some() || st.wander_job.is_some() || st.disaster_job.is_some() || st.regulation_detail_job.is_some() || st.traffic_color_job.is_some() || st.cause_job.is_some();
                if any {
                    if st.route_job.is_some() { st.route_note = Some("中断".to_string()); }
                    st.route_job = None; st.search_job = None; st.near_job = None; st.street_job = None; st.cam_job = None; st.recommend_job = None; st.road_job = None; st.catpoi_job = None; st.wander_job = None; st.disaster_job = None; st.regulation_detail_job = None; st.traffic_color_job = None; st.cause_job = None;
                    st.addr = "中断".into();
                }
            }
            Some(Event::Key(k)) if st.onboard => { // 何かキーで閉じる。d のときだけ「次回から非表示」マーカーを書く(既定は毎回表示)
                if matches!(k.code, KeyCode::Char('d') | KeyCode::Char('D')) {
                    if let Some(p) = onboarded_marker() { let _ = crate::fsutil::write_atomic(&p, b"1", None); }
                    st.addr = "オンボーディング: 次回から非表示(設定で再表示)".into();
                }
                st.onboard = false;
                st.force_reemit = true; // 次フレームで確実に地図を再構築・再emitし、覆っていた分の残像を消す
                st.last_map_sig = None; // 実画像モードのsig一致スキップに巻き込まれず必ず再取得させる
            }
            Some(Event::Key(k)) if st.quit_confirm => { // 終了確認: y=終了/他=取消
                if let KeyCode::Char('y') | KeyCode::Char('Y') = k.code { break; }
                st.quit_confirm = false;
            }
            Some(Event::Key(_)) if st.qr_view.is_some() => { st.qr_view = None; st.force_reemit = true; } // ポップアップを閉じる(即座に再emitして残像を消す)
            Some(Event::Key(_)) if st.popup.is_some() => { st.popup = None; st.force_reemit = true; } // 名前ポップアップを閉じる(同上)
            // 災害事例パネルを閉じる。qr_view/popup と同じく任意キーで閉じる(Esc/qを含む)。
            // ここで全キーを受け止めないと、パネルに覆われた地図側のキー(v で地点追加等)が
            // 見えないまま発火してしまう。
            Some(Event::Key(_)) if st.disaster_view.is_some() => { st.disaster_view = None; st.force_reemit = true; }
            // 通行規制の詳細パネルを閉じる。disaster_view と同じく任意キーで閉じる。
            Some(Event::Key(_)) if st.regulation_detail_view.is_some() => { st.regulation_detail_view = None; st.force_reemit = true; }
            Some(Event::Key(k)) if st.spot_move_confirm.is_some() => { // 「中心へ移動」の確認(y=実行/他=取消)
                let gi = st.spot_move_confirm.take().unwrap();
                if let KeyCode::Char('y') = k.code {
                    st.snd.play("confirm");
                    if let Some(s) = st.spots.get_mut(gi) { s.lat = lat; s.lon = lon; }
                    let _ = save_all_spots(&st.spots); apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots);
                    st.addr = "スポット位置を中心へ移動".into();
                } else { st.addr = "移動を取消".into(); }
            }
            Some(Event::Key(k)) if st.save_confirm.is_some() => { // 同名の上書き確認(y=上書き/他=名前を変更して新規登録)
                let name = st.save_confirm.take().unwrap();
                if let KeyCode::Char('y') = k.code {
                    st.addr = match save_named_route(&name, &st.mode, &st.wps) { Ok(_) => { st.snd.play("confirm"); st.route_name_hint = name.clone(); format!("上書き保存: {name}") }, Err(e) => format!("({e})") };
                    st.focus = Focus::Map;
                }
                // else: キャンセル。focusは既にFocus::SaveNameのままなので、名前を変えて新規登録できる
            }
            Some(Event::Key(k)) if st.clear_route_confirm => { // ルート全消去の確認(y=消去/他=取消)
                st.clear_route_confirm = false;
                if let KeyCode::Char('y') = k.code {
                    st.wps.clear(); st.wp_sel = 0; st.route_sel = 0; st.road_segs.clear(); st.spec.roads.clear();
                    let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_;
                    st.addr = "ルート消去".into();
                } else { st.addr = "消去を取消".into(); }
            }
            // Map表示中のEscは進行中ジョブの中断に使う(サブ画面のEscは各Focusの取消のまま)
            Some(Event::Key(k)) if k.code == KeyCode::Esc && matches!(st.focus, Focus::Map)
                && (st.route_job.is_some() || st.search_job.is_some() || st.near_job.is_some() || st.street_job.is_some() || st.cam_job.is_some() || st.recommend_job.is_some() || st.road_job.is_some() || st.catpoi_job.is_some() || st.wander_job.is_some() || st.disaster_job.is_some() || st.regulation_detail_job.is_some() || st.traffic_color_job.is_some() || st.cause_job.is_some()) => {
                if st.route_job.is_some() { st.route_note = Some("中断".to_string()); }
                st.route_job = None; st.search_job = None; st.near_job = None; st.street_job = None; st.cam_job = None; st.recommend_job = None; st.road_job = None; st.catpoi_job = None; st.wander_job = None; st.disaster_job = None; st.regulation_detail_job = None; st.traffic_color_job = None; st.cause_job = None;
                st.addr = "中断".into();
            }
            Some(Event::Key(k)) => {
                let cur = std::mem::replace(&mut st.focus, Focus::Map);
                match cur {
                    Focus::Search(mut buf) => match k.code {
                        KeyCode::Enter => { // 候補を一覧表示(左袖)。Enterで移動/s e vで経路点
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                // provider は Google キーの有無で分ける(キーあり=Google優先"g"/無し=Nominatim"n")。言語は ja 固定。
                                let provider = if st.cfg.google_maps_api_key.trim().is_empty() { "n" } else { "g" };
                                let ckey = searchcache::make_key(provider, "ja", &q, lat, lon);
                                // キャッシュヒットは即適用(同期)。ミス時のみ別スレッドで検索(通信/サーバ障害は0件と区別)。
                                // ヒット時は last_used を更新(LRU破棄の基準。次回 save 時に永続化される)。
                                let hit = st.scache.get_mut(&ckey).map(|e| { e.last_used_at = searchcache::now_secs(); e.results.clone() });
                                if let Some(v) = hit {
                                    if v.is_empty() { st.snd.play("error"); st.addr = format!("見つからない: {q}"); }
                                    else {
                                        st.pois = v.into_iter().take(8).map(|(la, lo, nm)| (la, lo, nm, PoiCat::Waypoint)).collect();
                                        st.poi_sel = 0;
                                        st.poi_label = format!("検索:{q}");
                                        set_markers(&mut st.spec, &st.wps, &st.pois);
                                        st.focus = Focus::PoiList;
                                    }
                                } else {
                                    let q2 = q.clone(); let ckey2 = ckey.clone();
                                    let key = st.cfg.google_maps_api_key.clone();
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    std::thread::spawn(move || {
                                        let r = geocode_list(&q2, Some((lat, lon)), &key).map_err(|e| e.to_string());
                                        let _ = tx.send((ckey2, q2, r));
                                    });
                                    st.search_job = Some(rx);
                                    st.focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                                }
                            }
                        }
                        KeyCode::Esc => { st.snd.play("back"); }
                        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::Search(buf); } // ←→/文字/BS/Del/Home/End
                    },
                    Focus::SpotCatList => match k.code { // カテゴリ一覧(P)
                        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.cat_sel = st.cat_sel.saturating_sub(1); st.focus = Focus::SpotCatList; }
                        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); if st.cat_sel + 1 < st.spot_cats.len() { st.cat_sel += 1; } st.focus = Focus::SpotCatList; }
                        KeyCode::Char('n') => { st.input_cur = 0; st.focus = Focus::NewCat(String::new()); }
                        KeyCode::Char('[') => { // 選択カテゴリを上へ
                            if st.cat_sel > 0 && st.cat_sel < st.spot_cats.len() { st.spot_cats.swap(st.cat_sel, st.cat_sel - 1); st.cat_sel -= 1; let _ = save_all_cats(&st.spot_cats); }
                            st.focus = Focus::SpotCatList;
                        }
                        KeyCode::Char(']') => { // 選択カテゴリを下へ
                            if st.cat_sel + 1 < st.spot_cats.len() { st.spot_cats.swap(st.cat_sel, st.cat_sel + 1); st.cat_sel += 1; let _ = save_all_cats(&st.spot_cats); }
                            st.focus = Focus::SpotCatList;
                        }
                        KeyCode::Char('r') => { if let Some((n, _, _)) = st.spot_cats.get(st.cat_sel) { st.input_cur = n.chars().count(); st.focus = Focus::SpotRename(n.clone(), st.cat_sel); } else { st.focus = Focus::SpotCatList; } }
                        KeyCode::Char('c') => {
                            match st.spot_cats.get(st.cat_sel) {
                                Some((_, ci, _)) => { st.color_sel = *ci; st.focus = Focus::ColorPick { cat: st.cat_sel }; }
                                None => st.focus = Focus::SpotCatList,
                            }
                        }
                        KeyCode::Char('M') => { // 形状ピッカー(色 c とは独立に形を選ぶ)
                            match st.spot_cats.get(st.cat_sel) {
                                Some((_, _, sh)) => { st.shape_sel = *sh; st.focus = Focus::ShapePick { cat: st.cat_sel }; }
                                None => st.focus = Focus::SpotCatList,
                            }
                        }
                        KeyCode::Char('x') => {
                            if let Some((name, _, _)) = st.spot_cats.get(st.cat_sel).cloned() {
                                if st.spots.iter().any(|s| s.cat == name) { st.addr = format!("使用中: {name}(先に空に)"); }
                                else { st.spot_cats.remove(st.cat_sel); if st.cat_sel >= st.spot_cats.len() && st.cat_sel > 0 { st.cat_sel -= 1; } let _ = save_all_cats(&st.spot_cats); }
                            }
                            st.focus = Focus::SpotCatList;
                        }
                        KeyCode::Enter => {
                            let cat = st.spot_cats.get(st.cat_sel).map(|(c, _, _)| c.clone());
                            if let Some((la, lo, nm)) = st.pending_spot.take() {
                                // 検索結果からの登録: 選択カテゴリに新規スポットとして保存
                                if let Some(cat) = cat {
                                    st.snd.play("pop");
                                    let s = Spot { lat: la, lon: lo, cat: cat.clone(), name: spot_clean(&nm) };
                                    let _ = append_spot(&s);
                                    st.spots.push(s);
                                    st.show_spots = true;
                                    apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots);
                                    st.addr = format!("★登録: {} [{}]", if nm.is_empty() { "(無名)" } else { nm.as_str() }, cat);
                                }
                                st.focus = Focus::Map;
                            } else if let Some(cat) = cat {
                                st.cur_cat = cat; st.sp_sel = 0; st.focus = Focus::SpotList;
                            } else { st.focus = Focus::SpotCatList; }
                        }
                        // 登録キャンセル時も保留を消す→Mapへ。左袖(カテゴリ一覧)の残像を残さないよう
                        // 全消去してから次フレームで再構築させる(Menu閉じる時と同じ理由)。
                        KeyCode::Esc => { st.snd.play("back"); st.pending_spot = None; st.focus = Focus::Map; let _ = write!(out, "\x1b[2J"); st.force_reemit = true; }
                        _ => st.focus = Focus::SpotCatList,
                    },
                    Focus::Settings => { let mut stay = true; let mut changed = false; match k.code { // 設定画面
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
                    if stay { st.focus = Focus::Settings; } },
                    Focus::SettingsEdit(idx, mut buf) => match k.code {
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
                    },
                    Focus::RoadSearch(mut buf) => match k.code { // 道路名/ref で現在view内をルート化
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            if !name.is_empty() {
                                let (n_lat, w_lon) = pixel_to_deg(st.cx - ow as f64 / 2.0, st.cy - oh as f64 / 2.0, st.z);
                                let (s_lat, e_lon) = pixel_to_deg(st.cx + ow as f64 / 2.0, st.cy + oh as f64 / 2.0, st.z);
                                let (tx, rx) = std::sync::mpsc::channel();
                                let name2 = name.clone();
                                std::thread::spawn(move || {
                                    let r = roadsearch::fetch(&name2, s_lat, w_lon, n_lat, e_lon);
                                    let _ = tx.send((name2, r));
                                });
                                st.road_job = Some(rx);
                                st.focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                            }
                        }
                        KeyCode::Esc => { st.snd.play("back"); }
                        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::RoadSearch(buf); }
                    },
                    Focus::Recommend(mut buf) => match k.code { // おすすめ: 方向性→claude -p→実在確認→候補一覧
                        KeyCode::Enter => {
                            let dir = buf.trim().to_string();
                            if !dir.is_empty() {
                                // AI提案→実在確認(geocode)ループを別スレッドで回し、検証済みスポット列を返す。
                                let cmd = st.cfg.llm_command.clone();
                                let model = st.cfg.llm_model.clone();
                                let key = st.cfg.google_maps_api_key.clone();
                                let (tx, rx) = std::sync::mpsc::channel();
                                std::thread::spawn(move || {
                                    let payload: Result<Vec<(f64, f64, String)>, String> = match recommend::recommend(&cmd, &model, &dir) {
                                        Ok(recs) => {
                                            let mut verified: Vec<(f64, f64, String)> = Vec::new();
                                            for r in recs.iter().take(8) {
                                                let q = if r.area.is_empty() { r.name.clone() } else { format!("{} {}", r.area, r.name) };
                                                if let Ok((la, lo)) = geocode(&q, Some((lat, lon)), &key) {
                                                    verified.push((la, lo, r.name.clone()));
                                                }
                                            }
                                            Ok(verified)
                                        }
                                        Err(e) => Err(e),
                                    };
                                    let _ = tx.send(payload);
                                });
                                st.recommend_job = Some(rx);
                                st.focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                            }
                        }
                        KeyCode::Esc => { st.snd.play("back"); }
                        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::Recommend(buf); }
                    },
                    Focus::SpotList => match k.code { // cur_cat のスポット一覧
                        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.sp_sel = st.sp_sel.saturating_sub(1); st.focus = Focus::SpotList; }
                        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); let n = st.spots.iter().filter(|s| s.cat == st.cur_cat).count(); if st.sp_sel + 1 < n { st.sp_sel += 1; } st.focus = Focus::SpotList; }
                        KeyCode::Char('n') => { st.input_cur = 0; st.focus = Focus::SpotForm { name: String::new(), url: String::new(), field: 0 }; } // 新規スポット登録フォーム
                        KeyCode::Char('[') => { // 選択スポットを同カテゴリ内で上へ
                            let idxs: Vec<usize> = st.spots.iter().enumerate().filter(|(_, s)| s.cat == st.cur_cat).map(|(i, _)| i).collect();
                            if st.sp_sel > 0 && st.sp_sel < idxs.len() { st.spots.swap(idxs[st.sp_sel], idxs[st.sp_sel - 1]); st.sp_sel -= 1; let _ = save_all_spots(&st.spots); }
                            st.focus = Focus::SpotList;
                        }
                        KeyCode::Char(']') => { // 選択スポットを同カテゴリ内で下へ
                            let idxs: Vec<usize> = st.spots.iter().enumerate().filter(|(_, s)| s.cat == st.cur_cat).map(|(i, _)| i).collect();
                            if st.sp_sel + 1 < idxs.len() { st.spots.swap(idxs[st.sp_sel], idxs[st.sp_sel + 1]); st.sp_sel += 1; let _ = save_all_spots(&st.spots); }
                            st.focus = Focus::SpotList;
                        }
                        KeyCode::Char('r') => { // 選択スポットを改名
                            let idxs: Vec<usize> = st.spots.iter().enumerate().filter(|(_, s)| s.cat == st.cur_cat).map(|(i, _)| i).collect();
                            match idxs.get(st.sp_sel) { Some(&gi) => { st.input_cur = st.spots[gi].name.chars().count(); st.focus = Focus::SpotEditName(st.spots[gi].name.clone(), gi); } None => st.focus = Focus::SpotList }
                        }
                        KeyCode::Char('m') => { // 選択スポットを現在の中心へ移動(破壊的なので確認待ちにするだけ)
                            let idxs: Vec<usize> = st.spots.iter().enumerate().filter(|(_, s)| s.cat == st.cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(st.sp_sel) { st.spot_move_confirm = Some(gi); }
                            st.focus = Focus::SpotList;
                        }
                        KeyCode::Enter => {
                            let idxs: Vec<usize> = st.spots.iter().enumerate().filter(|(_, s)| s.cat == st.cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(st.sp_sel) { let (nx, ny) = deg_to_pixel(st.spots[gi].lat, st.spots[gi].lon, st.z); st.cx = nx; st.cy = ny; }
                            st.focus = Focus::SpotList;
                        }
                        KeyCode::Char('x') => {
                            let idxs: Vec<usize> = st.spots.iter().enumerate().filter(|(_, s)| s.cat == st.cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(st.sp_sel) {
                                st.spots.remove(gi);
                                if st.sp_sel > 0 && st.sp_sel >= idxs.len() - 1 { st.sp_sel -= 1; }
                                let _ = save_all_spots(&st.spots);
                                apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots);
                            }
                            st.focus = Focus::SpotList;
                        }
                        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::SpotCatList; }
                        _ => st.focus = Focus::SpotList,
                    },
                    Focus::SpotEditName(mut buf, gi) => match k.code { // スポット改名
                        KeyCode::Enter => {
                            st.snd.play("confirm");
                            let new = spot_clean(buf.trim());
                            if let Some(s) = st.spots.get_mut(gi) { s.name = new; }
                            let _ = save_all_spots(&st.spots);
                            apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots);
                            st.focus = Focus::SpotList;
                        }
                        KeyCode::Esc => st.focus = Focus::SpotList,
                        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::SpotEditName(buf, gi); }
                    },
                    Focus::NewCat(mut buf) => match k.code {
                        KeyCode::Enter => { let name = buf.trim().to_string(); if !name.is_empty() { st.snd.play("confirm"); let _ = ensure_spot_cat(&name, &mut st.spot_cats); } st.focus = Focus::SpotCatList; }
                        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::SpotCatList; }
                        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::NewCat(buf); }
                    },
                    Focus::SpotRename(mut buf, idx) => match k.code {
                        KeyCode::Enter => {
                            let new = spot_clean(buf.trim());
                            if !new.is_empty() {
                                if let Some(old) = st.spot_cats.get(idx).map(|(n, _, _)| n.clone()) {
                                    for s in st.spots.iter_mut() { if s.cat == old { s.cat = new.clone(); } }
                                    if let Some(e) = st.spot_cats.get_mut(idx) { e.0 = new; }
                                    let _ = save_all_spots(&st.spots);
                                    let _ = save_all_cats(&st.spot_cats);
                                    apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots);
                                }
                            }
                            st.focus = Focus::SpotCatList;
                        }
                        KeyCode::Esc => st.focus = Focus::SpotCatList,
                        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::SpotRename(buf, idx); }
                    },
                    Focus::SpotForm { mut name, mut url, mut field } => match k.code { // 新規スポット登録フォーム
                        KeyCode::Up | KeyCode::BackTab => { field = (field + 3) % 4; st.input_cur = form_cur(&name, &url, field); st.focus = Focus::SpotForm { name, url, field }; }
                        KeyCode::Down | KeyCode::Tab => { field = (field + 1) % 4; st.input_cur = form_cur(&name, &url, field); st.focus = Focus::SpotForm { name, url, field }; }
                        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::SpotList; } // 取消
                        KeyCode::Enter => match field {
                            0 => { field = 1; st.input_cur = url.chars().count(); st.focus = Focus::SpotForm { name, url, field }; } // 次のフィールドへ
                            1 => { field = 2; st.input_cur = 0; st.focus = Focus::SpotForm { name, url, field }; }
                            3 => st.focus = Focus::SpotList, // [戻る]
                            _ => { // 2 = [送信]
                                let u = url.trim();
                                let name_in = spot_clean(name.trim()); // 名称buf(整形済)
                                // URL非空: parse_gmaps_placeで(lat,lon,店名)。空: 現在地(中心)+名称。両方空: 何もしない
                                enum Act { Save(f64, f64, String), Err(String), Nop }
                                let act = if u.is_empty() && name_in.is_empty() { Act::Nop }
                                    else if u.is_empty() { Act::Save(lat, lon, if name_in.is_empty() { "(無名)".into() } else { name_in.clone() }) }
                                    else if u.contains("goo.gl") || u.contains("maps.app") { Act::Err("短縮URLは不可。Googleマップの通常URL(…/@…/!3d…!4d…)を貼って".into()) }
                                    else if let Some((la, lo, nm)) = parse_gmaps_place(u) {
                                        let nm = spot_clean(&nm); // URLの名前
                                        let final_name = if !name_in.is_empty() { name_in.clone() } // 名称buf優先
                                            else if !nm.is_empty() { nm } else { "(無名)".into() };
                                        Act::Save(la, lo, final_name)
                                    } else { Act::Err("URLから位置を取得できません(GoogleマップのURLか確認)".into()) };
                                match act {
                                    Act::Save(la, lo, nm) => {
                                        st.snd.play("confirm");
                                        let s = Spot { lat: la, lon: lo, cat: st.cur_cat.clone(), name: nm };
                                        let _ = ensure_spot_cat(&s.cat, &mut st.spot_cats);
                                        st.addr = match append_spot(&s) { Ok(_) => format!("スポット保存: {}", s.name), Err(e) => format!("({e})") };
                                        st.spots.push(s); st.show_spots = true; apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots);
                                        st.focus = Focus::SpotList;
                                    }
                                    Act::Err(msg) => { st.addr = msg; st.focus = Focus::SpotForm { name, url, field }; }
                                    Act::Nop => st.focus = Focus::SpotForm { name, url, field },
                                }
                            }
                        },
                        other => { // ←→/文字/BS/Del/Home/End は選択中フィールドを編集(ボタン欄では無視)
                            if field == 0 { edit_line(&mut name, &mut st.input_cur, other); }
                            else if field == 1 { edit_line(&mut url, &mut st.input_cur, other); }
                            st.focus = Focus::SpotForm { name, url, field };
                        }
                    },
                    Focus::PoiKindForm { mut label, mut tag, mut field } => match k.code { // 目的地カテゴリの新規追加フォーム
                        KeyCode::Up | KeyCode::BackTab => { field = (field + 3) % 4; st.input_cur = form_cur(&label, &tag, field); st.focus = Focus::PoiKindForm { label, tag, field }; }
                        KeyCode::Down | KeyCode::Tab => { field = (field + 1) % 4; st.input_cur = form_cur(&label, &tag, field); st.focus = Focus::PoiKindForm { label, tag, field }; }
                        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::PoiMenu; }
                        KeyCode::Enter => match field {
                            0 => { field = 1; st.input_cur = tag.chars().count(); st.focus = Focus::PoiKindForm { label, tag, field }; }
                            1 => { field = 2; st.input_cur = 0; st.focus = Focus::PoiKindForm { label, tag, field }; }
                            3 => st.focus = Focus::PoiMenu, // [戻る]
                            _ => { // 2 = [追加]
                                let label_in = poi_kind_clean(label.trim());
                                let t = tag.trim();
                                let parts: Vec<&str> = t.splitn(2, '=').collect();
                                let bad_char = |s: &str| s.contains('"') || s.contains('\\') || s.contains('\n');
                                if label_in.is_empty() { st.addr = "表示名を入力してください".into(); st.focus = Focus::PoiKindForm { label, tag, field }; }
                                else if parts.len() != 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() || bad_char(t) {
                                    st.addr = "OSMタグは key=value 形式(例: shop=bakery)".into();
                                    st.focus = Focus::PoiKindForm { label, tag, field };
                                } else {
                                    let (tk, tv) = (parts[0].trim(), parts[1].trim());
                                    let key = next_free_key(&st.poi_kinds);
                                    let kind = PoiKind { key, label: label_in.clone(), filter: format!("nwr[\"{tk}\"=\"{tv}\"]"), cat: PoiCat::Other };
                                    st.poi_kinds.push(kind);
                                    let _ = save_poi_kinds(&st.poi_kinds);
                                    st.snd.play("confirm");
                                    st.addr = format!("カテゴリ追加: {label_in} ({key})");
                                    st.focus = Focus::PoiMenu;
                                }
                            }
                        },
                        other => {
                            if field == 0 { edit_line(&mut label, &mut st.input_cur, other); }
                            else if field == 1 { edit_line(&mut tag, &mut st.input_cur, other); }
                            st.focus = Focus::PoiKindForm { label, tag, field };
                        }
                    },
                    Focus::WanderForm { mut dist_km } => match k.code { // おまかせ周回: 距離ゲージ
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
                    },
                    Focus::NearSearch(mut buf) => match k.code {
                        KeyCode::Enter => {
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                // Overpass(遅い)を別スレッドへ。viewbox境界を先に確定して渡す。★マージは結果適用側で行う。
                                let (vt, vl) = pixel_to_deg(st.cx - ow as f64 * 1.25, st.cy - oh as f64 * 1.25, st.z);
                                let (vb, vr) = pixel_to_deg(st.cx + ow as f64 * 1.25, st.cy + oh as f64 * 1.25, st.z);
                                let rlat = 2.0 / 111.0;
                                let rlon = 2.0 / (111.0 * lat.to_radians().cos().abs().max(0.1));
                                let (south, west) = (vb.min(lat - rlat), vl.min(lon - rlon));
                                let (north, east) = (vt.max(lat + rlat), vr.max(lon + rlon));
                                let q2 = q.clone();
                                let (tx, rx) = std::sync::mpsc::channel();
                                std::thread::spawn(move || {
                                    let v = search_nearby(&q2, south, west, north, east);
                                    let _ = tx.send((q2, v));
                                });
                                st.near_job = Some(rx);
                                st.focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                            }
                        }
                        KeyCode::Esc => { st.snd.play("back"); }
                        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::NearSearch(buf); }
                    },
                    Focus::PoiMenu => match k.code {
                        KeyCode::Esc => {}
                        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.poimenu_sel = st.poimenu_sel.saturating_sub(1); st.focus = Focus::PoiMenu; }
                        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); if st.poimenu_sel + 1 <= st.poi_kinds.len() { st.poimenu_sel += 1; } st.focus = Focus::PoiMenu; }
                        KeyCode::Char('/') => { st.input_cur = 0; st.focus = Focus::NearSearch(String::new()); }
                        KeyCode::Char('n') => { st.input_cur = 0; st.focus = Focus::PoiKindForm { label: String::new(), tag: String::new(), field: 0 }; } // 新規カテゴリ追加
                        KeyCode::Char('[') if st.poimenu_sel > 0 && st.poimenu_sel < st.poi_kinds.len() => {
                            st.poi_kinds.swap(st.poimenu_sel, st.poimenu_sel - 1); st.poimenu_sel -= 1;
                            let _ = save_poi_kinds(&st.poi_kinds);
                            st.focus = Focus::PoiMenu;
                        }
                        KeyCode::Char(']') if st.poimenu_sel + 1 < st.poi_kinds.len() => {
                            st.poi_kinds.swap(st.poimenu_sel, st.poimenu_sel + 1); st.poimenu_sel += 1;
                            let _ = save_poi_kinds(&st.poi_kinds);
                            st.focus = Focus::PoiMenu;
                        }
                        KeyCode::Char('x') if st.poimenu_sel < st.poi_kinds.len() => {
                            let removed = st.poi_kinds.remove(st.poimenu_sel);
                            if st.poimenu_sel >= st.poi_kinds.len() && st.poimenu_sel > 0 { st.poimenu_sel -= 1; }
                            let _ = save_poi_kinds(&st.poi_kinds);
                            st.addr = format!("カテゴリ削除: {}", removed.label);
                            st.focus = Focus::PoiMenu;
                        }
                        KeyCode::Enter | KeyCode::Char(_) => {
                            // Enter=選択行 / キー1文字=対応カテゴリ。最終行(=poi_kinds.len())はキーワード周辺検索。
                            let idx = if let KeyCode::Char(c) = k.code { st.poi_kinds.iter().position(|kk| kk.key == c) } else { Some(st.poimenu_sel) };
                            match idx {
                                Some(i) if i >= st.poi_kinds.len() => { st.input_cur = 0; st.focus = Focus::NearSearch(String::new()); }
                                Some(i) => {
                                    let kind = st.poi_kinds[i].clone();
                                    let label = kind.label.clone();
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    std::thread::spawn(move || {
                                        let r = poi_search(&kind, st.cx, st.cy, st.z, ow, oh, lat, lon);
                                        let _ = tx.send((label, r));
                                    });
                                    st.catpoi_job = Some(rx);
                                    st.focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                                }
                                None => st.focus = Focus::PoiMenu,
                            }
                        }
                        _ => st.focus = Focus::PoiMenu,
                    },
                    Focus::PoiList => match k.code {
                        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.poi_sel = st.poi_sel.saturating_sub(1); if let Some(p) = st.pois.get(st.poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, st.z); st.cx = nx; st.cy = ny; } st.focus = Focus::PoiList; } // 選択に地図追従
                        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); if st.poi_sel + 1 < st.pois.len() { st.poi_sel += 1; } if let Some(p) = st.pois.get(st.poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, st.z); st.cx = nx; st.cy = ny; } st.focus = Focus::PoiList; }
                        KeyCode::Left | KeyCode::Char('a') => { st.cx -= (oh as f64 / 8.0).max(1.0); st.focus = Focus::PoiList; } // ←→/hjklで地図を微パン(一覧選択は動かさない)
                        KeyCode::Right | KeyCode::Char('d') => { st.cx += (oh as f64 / 8.0).max(1.0); st.focus = Focus::PoiList; }
                        KeyCode::Char('h') => { st.cx -= (oh as f64 / 8.0).max(1.0); st.focus = Focus::PoiList; }
                        KeyCode::Char('l') => { st.cx += (oh as f64 / 8.0).max(1.0); st.focus = Focus::PoiList; }
                        KeyCode::Char('k') => { st.cy -= (oh as f64 / 8.0).max(1.0); st.focus = Focus::PoiList; }
                        KeyCode::Char('j') => { st.cy += (oh as f64 / 8.0).max(1.0); st.focus = Focus::PoiList; }
                        KeyCode::Char('+') | KeyCode::Char('=') => { if st.z < 19 { st.z += 1; st.cx *= 2.0; st.cy *= 2.0; st.restart_prefetch_on_zoom(); } st.focus = Focus::PoiList; } // +/-でズーム
                        KeyCode::Char('-') | KeyCode::Char('_') => { if st.z > 2 { st.z -= 1; st.cx /= 2.0; st.cy /= 2.0; st.restart_prefetch_on_zoom(); } st.focus = Focus::PoiList; }
                        KeyCode::Enter => { // 選択地点へ移動(明示)
                            if let Some(p) = st.pois.get(st.poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, st.z); st.cx = nx; st.cy = ny; }
                            st.focus = Focus::PoiList;
                        }
                        KeyCode::Char('v') => { // 選択地点をルートに追加(末尾)
                            if let Some(p) = st.pois.get(st.poi_sel) {
                                st.snd.play("pop");
                                wp_add(&mut st.wps, (p.0, p.1));
                                let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &route_nogos); st.route_note = n_; st.route_job = j_;
                                st.addr = format!("地点を追加 #{}", st.wps.len());
                            }
                            st.focus = Focus::PoiList;
                        }
                        KeyCode::Char('f') => st.focus = Focus::PoiMenu,
                        KeyCode::Char('P') => { // 選択結果をお気に入りスポットに登録(カテゴリを選ばせる)
                            if let Some(p) = st.pois.get(st.poi_sel) {
                                if st.spot_cats.is_empty() { let _ = ensure_spot_cat("お気に入り", &mut st.spot_cats); }
                                st.pending_spot = Some((p.0, p.1, p.2.clone()));
                                st.cat_sel = 0;
                                st.focus = Focus::SpotCatList;
                            } else { st.focus = Focus::PoiList; }
                        }
                        KeyCode::Esc => { st.pois.clear(); set_markers(&mut st.spec, &st.wps, &st.pois); }
                        _ => st.focus = Focus::PoiList,
                    },
                    Focus::SaveName(mut buf) => match k.code {
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
                    },
                    Focus::RouteFavMenu { sel } => match k.code { // お気に入りルート: 保存/呼び出しの小メニュー(Sキー)
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
                    },
                    Focus::RouteList => match k.code {
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
                    },
                    Focus::RoadList => match k.code { // 道路の塊の一覧(個別削除)
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
                    },
                    // 並べ替えビュー: ↑↓で選択(地図が追従)、Spaceで掴む↔置く、掴み中は↑↓で地点を移動
                    Focus::WaypointList => match k.code {
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
                    },
                    // Space メニュー・トップ(カテゴリ選択)。文字キーは全カテゴリ横断で直接実行できる。
                    Focus::Menu(MenuLevel::Categories) => match k.code {
                        KeyCode::Up | KeyCode::Char('w') => { st.snd.play("click"); st.menu_cat_sel = st.menu_cat_sel.saturating_sub(1); st.focus = Focus::Menu(MenuLevel::Categories); }
                        KeyCode::Down | KeyCode::Char('s') => { st.snd.play("click"); if st.menu_cat_sel + 1 < MENU_CATEGORIES.len() { st.menu_cat_sel += 1; } st.focus = Focus::Menu(MenuLevel::Categories); }
                        KeyCode::Enter => { st.snd.play("click"); st.menu_item_sel = 0; st.focus = Focus::Menu(MenuLevel::Items(st.menu_cat_sel)); }
                        // メニューを閉じる → Map。左袖(カテゴリ一覧)はマップとは別の列に描かれており、
                        // 通常のマップ再描画では上書きされない列が残ることがあるため、全消去してから
                        // 次フレームで確実に再構築させる(Resize時の扱いと同じ)。
                        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Map; let _ = write!(out, "\x1b[2J"); st.force_reemit = true; }
                        KeyCode::Char(c) => match menu_action_for_key(c) {
                            Some(act) => run_action!(act, lat, lon, cols, tr, &route_nogos),
                            None => st.focus = Focus::Menu(MenuLevel::Categories),
                        },
                        _ => st.focus = Focus::Menu(MenuLevel::Categories),
                    },
                    // Space メニュー・展開(項目選択)。キーはそのカテゴリ内だけ有効(スコープ限定)。
                    Focus::Menu(MenuLevel::Items(ci)) => {
                        let items = MENU_CATEGORIES[ci].items;
                        match k.code {
                            KeyCode::Up | KeyCode::Char('w') if !items.iter().any(|it| it.key == 'w') => { st.snd.play("click"); st.menu_item_sel = st.menu_item_sel.saturating_sub(1); st.focus = Focus::Menu(MenuLevel::Items(ci)); }
                            KeyCode::Down | KeyCode::Char('s') if !items.iter().any(|it| it.key == 's') => { st.snd.play("click"); if st.menu_item_sel + 1 < items.len() { st.menu_item_sel += 1; } st.focus = Focus::Menu(MenuLevel::Items(ci)); }
                            KeyCode::Enter => run_action!(items[st.menu_item_sel].action, lat, lon, cols, tr, &route_nogos),
                            KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::Menu(MenuLevel::Categories); } // 上位カテゴリへ戻る
                            KeyCode::Char(c) => match items.iter().find(|it| it.key == c) {
                                Some(it) => run_action!(it.action, lat, lon, cols, tr, &route_nogos),
                                None => st.focus = Focus::Menu(MenuLevel::Items(ci)),
                            },
                            _ => st.focus = Focus::Menu(MenuLevel::Items(ci)),
                        }
                    }
                    // 色ピッカー: ←→でパレット選択、Enterで確定
                    Focus::ColorPick { cat } => {
                        let n = SPOT_PALETTE.len() as u8;
                        match k.code {
                            KeyCode::Left => { st.color_sel = (st.color_sel + n - 1) % n; st.focus = Focus::ColorPick { cat }; }
                            KeyCode::Right => { st.color_sel = (st.color_sel + 1) % n; st.focus = Focus::ColorPick { cat }; }
                            KeyCode::Enter => {
                                if let Some(e) = st.spot_cats.get_mut(cat) { e.1 = st.color_sel; let _ = save_all_cats(&st.spot_cats); apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots); }
                                st.focus = Focus::SpotCatList;
                            }
                            KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::SpotCatList; }
                            _ => st.focus = Focus::ColorPick { cat },
                        }
                    }
                    Focus::ShapePick { cat } => { // 形状ピッカー(色とは独立に形を選ぶ)
                        let n = NUM_MARKER_SHAPES;
                        match k.code {
                            KeyCode::Left => { st.shape_sel = (st.shape_sel + n - 1) % n; st.focus = Focus::ShapePick { cat }; }
                            KeyCode::Right => { st.shape_sel = (st.shape_sel + 1) % n; st.focus = Focus::ShapePick { cat }; }
                            KeyCode::Enter => {
                                if let Some(e) = st.spot_cats.get_mut(cat) { e.2 = st.shape_sel; let _ = save_all_cats(&st.spot_cats); apply_spots(&mut st.spec, &st.spots, &st.spot_cats, st.show_spots); }
                                st.focus = Focus::SpotCatList;
                            }
                            KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::SpotCatList; }
                            _ => st.focus = Focus::ShapePick { cat },
                        }
                    }
                    // 設定画面の一覧ピッカー: 地図種別/既定ルート/AIモデル/画像解像度/中心十字の色を↑↓/w・sで選びEnterで確定
                    Focus::SettingsPick(idx) => {
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
                    // ルート一覧にフォーカス中: ↑↓で点/操作行を選択、Enterで実行。矢印はパンでなく選択。
                    Focus::RoutePanel => {
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
                                    if ai < ROUTE_ACTS.len() { let act = ROUTE_ACTS[ai].1; run_action!(act, lat, lon, cols, tr, &route_nogos); }
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
                    Focus::Map => {
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
                                run_action!(act, lat, lon, cols, tr, &route_nogos);
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
                            KeyCode::Char('A') => run_action!(MenuAction::PlayRoute, lat, lon, cols, tr, &route_nogos),
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
                            KeyCode::Char('N') => run_action!(MenuAction::ViewCamera, lat, lon, cols, tr, &route_nogos),
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
                            KeyCode::Char('c') => run_action!(MenuAction::ClearRoute, lat, lon, cols, tr, &route_nogos),
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
                        if quit { break; }
                        let n = (TILE as f64) * 2f64.powi(st.z as i32);
                        if st.cx < 0.0 { st.cx += n; } else if st.cx >= n { st.cx -= n; }
                        st.cy = st.cy.clamp(0.0, n - 1.0);
                    }
                }
            }
            // web/touch-overlay.js が window.term.paste() で送ってくる、ブラウザの
            // Geolocation APIによるライブ現在地。SOH(\u{1})区切りの専用マーカーにしているのは、
            // 普通に貼り付けられるURL/テキストと衝突しない制御文字だから。マーカーに一致しない
            // 通常のペーストは下の既存分岐(検索欄への入力等)へ素通しする。
            Some(Event::Paste(s)) if s.starts_with("\u{1}GPS_STOP\u{1}") => {
                st.web_gps_active = false;
                st.addr = "ライブ現在地(スマホ): OFF".into();
            }
            Some(Event::Paste(s)) if s.starts_with("\u{1}GPS\u{1}") => {
                let rest = &s["\u{1}GPS\u{1}".len()..];
                let mut parts = rest.splitn(2, '\u{1}');
                if let (Some(la_s), Some(lo_s)) = (parts.next(), parts.next()) {
                    if let (Ok(la), Ok(lo)) = (la_s.parse::<f64>(), lo_s.parse::<f64>()) {
                        if la.is_finite() && lo.is_finite() && (-90.0..=90.0).contains(&la) && (-180.0..=180.0).contains(&lo) {
                            if !st.web_gps_active { st.gps_trail.clear(); st.addr = "ライブ現在地(スマホ): ON".into(); }
                            st.web_gps_active = true;
                            st.gps_pos = Some((la, lo));
                            st.gps_trail.push((la, lo));
                            if st.gps_trail.len() > 300 { st.gps_trail.remove(0); }
                            maybe_speak_turn(&st.cfg, &st.spec, &st.turn_points, &mut st.voice_guide, (la, lo));
                        }
                    }
                }
            }
            // 軸モードの再送要求(#87 設計書 §5.3)。ブラウザを再読み込みするとJS側の状態は
            // 消えるが termmap 側の Focus は変わらないので通知が飛ばない。ここでは印を立てる
            // だけで、実際の送出は次フレーム末の1か所に任せる。
            Some(Event::Paste(s)) if s.starts_with(dragmode::DRAG_MODE_REQUEST) => {
                st.drag_mode_req_pending = true;
            }
            // パン量マーカーは上の合算ブロックで消費済みなので、通常ここへは来ない。念のための
            // 保険(合算ブロックを通らない経路が将来増えても、マーカーが検索欄へ文字として
            // 入らないようにする)。
            Some(Event::Paste(s)) if s.starts_with(dragmode::PAN_MARKER) => {}
            Some(Event::Paste(s)) => { match &mut st.focus {
                Focus::Search(buf) | Focus::SaveName(buf) | Focus::NearSearch(buf) | Focus::NewCat(buf) | Focus::RoadSearch(buf) | Focus::Recommend(buf) => insert_str_at(buf, &mut st.input_cur, &s),
                Focus::SpotForm { name, url, field } => { if *field == 0 { insert_str_at(name, &mut st.input_cur, &s); } else if *field == 1 { insert_str_at(url, &mut st.input_cur, &s); } }
                Focus::SpotRename(buf, _) | Focus::SpotEditName(buf, _) => insert_str_at(buf, &mut st.input_cur, &s),
                Focus::SettingsEdit(idx, buf) => {
                    let filtered: String = if *idx == 6 {
                        s.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect()
                    } else {
                        s.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').collect()
                    };
                    insert_str_at(buf, &mut st.input_cur, &filtered);
                }
                Focus::Settings if st.set_sel == 17 => { st.cfg.google_maps_api_key = s.trim().to_string(); let _ = config::save_config(&st.cfg); st.addr = "APIキー設定(自動保存)".into(); }
                _ => {}
            } }
            Some(Event::Resize(..)) => { let _ = write!(out, "\x1b[2J"); st.force_reemit = true; } // 端末サイズ変更: 全消去して次フレームで再描画(インライン画像の残像防止)
            _ => {}
        }
    }
    // 雨雲の背景ポーラーは drop でスレッドを join する。終了時にちょうど取得中だと、その分
    // (HTTPは最大20秒)終了が固まって見えるので join を別スレッドへ逃がす(プロセス終了で消える)。
    if let Some(rc) = st.radar_clock.take() { std::thread::spawn(move || drop(rc)); }
    persist_full_state(st.cx, st.cy, st.z, &st.opts, &st.wps, &st.mode, &mut st.cfg, st.radar_on, st.show_spots);
    Ok(())
}


