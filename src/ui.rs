// 対話UIループ。main.rs から機械的に切り出したもの(挙動は不変)。
// HELP / TermGuard / interactive を収める。fit_cells 等はクレートルート(main.rs)側に残す。

use crate::*;
use crate::geo::*;
use crate::tiles::*;
use crate::render::*;
use crate::route::*;
use crate::spots::*;
use std::io::Write;
use image::{RgbImage, RgbaImage, imageops::FilterType};
// 単一行テキスト入力欄の共通編集ロジック(char_byte/insert_str_at/form_cur/edit_line/
// render_with_cursor/draw_input_panel)は textedit.rs へ切り出し済み。
// 貼り付け(Paste)の取り込みだけこのファイルに残っているので insert_str_at のみ使う。
use crate::textedit::insert_str_at;
// 左袖リストのスクロール追従(ensure_visible)は listview.rs、行の組み立ては ui_gutter.rs へ切り出し済み。

// PALETTE_NAMES(中心十字の色名。SPOT_PALETTEと同じ並び)・その利用箇所は settings.rs に移設。
// 緑グラデのワードマーク(LOGO・ヘルプ画面で使用)は keymap.rs へ移設済み。

use crate::keymap::{HELP, LOGO};

// Space メニュー(MenuAction/MenuItem/MenuCategory/MENU_CATEGORIES/MenuLevel/menu_action_for_key/
// disp_width/menu_row/ROUTE_ACTS)は menu.rs へ、メニューのキー処理は ui_keys.rs へ切り出し済み
// (このファイルからは使わない)。

// 道路名検索(r)で追加した道路の塊(RoadSeg)・その表示色選択(road_color_for)は roadseg.rs へ、
// それを組み立てる検索結果の取り込みは ui_jobs.rs へ切り出し済み(このファイルからは使わない)。

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

    let _ = write!(out, "\x1b[2J");
    loop {
        st.spin = st.spin.wrapping_add(1); // 通信中スピナーのアニメ用(毎フレーム進める)
        let (tc, tr) = crossterm::terminal::size().unwrap_or((100, 40));
        let cols = tc.max(20) as u32;
        let map_rows = (tr.max(3) - 1) as u32;
        // このフレームで使う端末セル比。ネイティブ端末(window_size)→ブラウザ通知(CELLマーカー)→
        // 既定 2.0 の順(設計書 §7.2)。フォントサイズ変更や画面回転に毎フレーム追随させたいので
        // キャッシュせず都度引く(window_size は terminal::size と同じ ioctl 1回で、上の size()
        // 呼び出しと同程度のコスト)。
        let cell_ratio = cellratio::resolve_ratio(cellratio::detect_native_ratio(), st.cell_ratio_web);
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
                // 実写は 640x480(4:3)の写真で、地図と違って端末の形に合わせて生成していない。
                // 端末全体のセル矩形へそのまま強制フィットすると、iPhone 縦持ちのような縦長端末
                // では縦に約2.6倍伸びる(設計書 §4.1)。写真の縦横比を保ったまま黒帯付きの1枚へ
                // 合成してから出す(設計書 §7.6)。実画像モードもAAフォールバックも同じ考え方。
                let shown = cellratio::crop_photo(img, cols.max(10), map_rows, cell_ratio);
                if st.cfg.image_mode && image_capable() {
                    // 実画像モード: 実写を全幅×map_rows のインライン画像で表示
                    let _ = write!(out, "\x1b[H");
                    let _ = emit_iterm2_image(&mut out, &shown, cols, map_rows);
                } else {
                    let rs = image::imageops::resize(&shown, cols.max(10), map_rows * 2, FilterType::Triangle);
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
            // この画面は下の match まで進まず continue するので、セル比の通知はここで取り込む。
            // 取りこぼすと、写真を開いたまま文字サイズを変えたときだけ古い比のまま固定される
            // (JS 側は同じ値を再送しないため、地図へ戻っても直らない)。
            if let Event::Paste(s) = &ev {
                if let Some(r) = cellratio::parse_cell_marker(s) { st.cell_ratio_web = Some(r); }
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
                // 実写と同じく、写真の縦横比を保ったまま黒帯付きの1枚へ合成してから出す
                // (設計書 §7.6)。カメラ画像は提供元により縦横比が違うので 4:3 決め打ちにしない。
                let shown = cellratio::crop_photo(img, cols.max(10), map_rows, cell_ratio);
                if st.cfg.image_mode && image_capable() {
                    let _ = write!(out, "\x1b[H");
                    let _ = emit_iterm2_image(&mut out, &shown, cols, map_rows);
                } else {
                    let rs = image::imageops::resize(&shown, cols.max(10), map_rows * 2, FilterType::Triangle);
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
            let ev = event::read()?;
            // 実写画面と同じ理由でセル比の通知をここで取り込む(下の match まで進まないため)。
            if let Event::Paste(s) = &ev {
                if let Some(r) = cellratio::parse_cell_marker(s) { st.cell_ratio_web = Some(r); }
            }
            if let Event::Key(k) = ev {
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
        let muni_areas = st.boundary_layer.items(plot_view);
        // 過去災害の見せ方をズームで切り替える(設計 §3.4 / 無制限ズーム版 §4.1)。塗りは画面に
        // 複数の市区町村が入るズーム帯でだけ意味を持つ。z14以上は画面幅が1km前後になり
        // 「全面が同じ色」に退化するので、そこは従来の代表点マーカーへ譲る。判定に使うのは
        // 実描画ズーム(rz)ではなく地図ズーム(z): 実画像モードは rz>z でも写る地理範囲は同じため。
        // 下限は無い(choropleth::fill_visible_at_zoom参照)。
        let fill_on = st.cfg.disaster_enabled && st.cfg.disaster_fill;
        let choropleth_fill = fill_on && choropleth::fill_visible_at_zoom(st.z);
        // 件数リングは塗りが件数を担っている間は出さない(中心の小さな塊は常に残すので、
        // B キーが何を指しているかは画面から分かる)。
        let disaster_rings = !choropleth_fill;
        // 代表点マーカーは z11 以上だけ。市区町村あたり1点なので、z9 の braille では画面に
        // 最大96個が撒かれて塗りの上のノイズになる(広域版 §3.3)。広域で読みたいのは面の模様。
        // B キー(中心に最も近い地点の事例一覧)はマーカーが無くても従来どおり動く。
        let disaster_markers = z >= 11;
        // 塗りが出るときだけ z9 まで取りに行く。塗りOFFならマーカーの下限(z11)に据え置く
        // (塗りを使わない人に広域セルの通信をさせない)。
        let disaster_wanted = st.cfg.disaster_enabled && (z >= 11 || st.cfg.disaster_fill);
        let population_meshes =
            if st.cfg.population_enabled { st.population_layer.items(plot_view) } else { Vec::new() };
        // 表示する年次(設定)。configを手書きで壊されても必ず描ける索引へ落とす。
        let population_year_idx = population::year_index(st.cfg.population_year)
            .unwrap_or_else(|| population::year_index(config::DEFAULT_POPULATION_YEAR).unwrap_or(1));
        // 中心のクロスヘアが指すメッシュの人口密度(ステータス行に実数値で出す・設計 §7.6)。
        // 階級の幅(1,000〜4,000等)しか読めない色分けを、1つの数字で補う。
        // 中心は必ず視野の中にあるので、切り出し済みの population_meshes から拾う
        // (layer.items() をもう一度呼ぶと全セル(最大16万件)をもう1周することになる)。
        let population_here = mesh::half_mesh_code(lat, lon).and_then(|code| {
            population_meshes
                .iter()
                .find(|m| m.mesh == code)
                .and_then(|m| population::density(m, population_year_idx))
        });

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
        // 移動中に落とす先は config::IMAGE_SETTLE_DELTA_CAP(設計 §5.3 C-3 の見直し。
        // 判断の根拠は定数側のコメントに書いてある)。
        let delta = if !img_inline { 0 }
            else if settling { base_delta.min(config::IMAGE_SETTLE_DELTA_CAP).min(18u32.saturating_sub(st.z)) }
            else { base_delta.min(18u32.saturating_sub(st.z)) };
        let scale = 1u32 << delta;
        let (rw, rh, rz, rcx, rcy) = if img_inline {
            (map_cols * scale, map_rows * 2 * scale, st.z + delta, st.cx * scale as f64, st.cy * scale as f64)
        } else {
            (ow, oh, st.z, st.cx, st.cy)
        };
        // サブピクセル描画(設計 §5.1 対策A)。窓の切り出しで left/top の小数部を捨てないので、
        // 1出力ピクセル未満の動きも色の遷移として見え、斜めドラッグの階段が消える。
        // braille/edge は閾値でドットの on/off が決まるためちらつく可能性があり、
        // use_subpixel_window() で切り替えられるようにしてある。
        let subpixel = use_subpixel_window(st.opts.braille, st.opts.edge, st.subpixel_env.as_deref());
        let sub_steps = if subpixel { SUBPIXEL_STEPS } else { 1.0 };
        // 描画へ渡す中心は格子へ吸着させる。再描画判定(map_sig)と描画で同じ値を使わないと、
        // 絵が変わったのにシグネチャが変わらない取りこぼしが起きる(設計 §5.2)。
        // 論理座標 cx/cy は連続のまま保持してあるので、指の移動量は 1:1 のまま失われない。
        let (rcx, rcy) = snap_center_to_grid(rcx, rcy, sub_steps);
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
            // 中心座標は生の f64 ではなく、実際に描画へ効く粒度へ丸めて混ぜる(設計 §5.2 対策B)。
            // これより細かい差はどうせ同じ絵になる。粒度は描画側と揃える必要があるので、
            // rcx/rcy を吸着させたときと同じ sub_steps を渡す(サブピクセル切り出しなら
            // 1/SUBPIXEL_STEPS ピクセル・従来の整数切り出しなら1ピクセル)。
            // rcx/rcy は既に同じ格子へ吸着済みなので、描画とシグネチャの位置は必ず一致する。
            map_center_sig_key(rcx, rcy, rw, rh, sub_steps).hash(&mut h);
            // 切り出し方が変わると同じ中心でも絵が変わるので、モードそのものも混ぜる。
            subpixel.hash(&mut h);
            rz.hash(&mut h); rw.hash(&mut h); rh.hash(&mut h);
            gut.hash(&mut h); map_cols.hash(&mut h); map_rows.hash(&mut h);
            st.opts.style.hash(&mut h);
            // 裏取得でタイルが1枚届くたびに世代が変わる→sigが変わり次フレームで再構築され、
            // グレーのプレースホルダーが実タイルへ順次置き換わる。
            loader_gen_snapshot.hash(&mut h);
            st.spec.routes.len().hash(&mut h);
            st.spec.expressway_segments.len().hash(&mut h);
            st.spec.roads.len().hash(&mut h);
            st.spec.traffic_segments.len().hash(&mut h);
            st.spec.warning_segments.len().hash(&mut h);
            for rt in st.spec.routes.iter().chain(st.spec.expressway_segments.iter()).chain(st.spec.roads.iter()).chain(st.spec.traffic_segments.iter()).chain(st.spec.warning_segments.iter()) {
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
            // 過去災害の塗り: 境界セルが届くたび、また塗りの出し分け/ブラー半径が変わるたびに
            // ラスタライズし直す(逆に、パンもズームもしていないフレームでは1回も走らない)。
            // ブラー半径は地図ズーム(st.z)依存で、choropleth_fillがtrueのままでもz9→z10のように
            // 変わることがあるため、rzだけでは足りず個別にhashする。
            st.boundary_layer.generation().hash(&mut h);
            choropleth_fill.hash(&mut h);
            choropleth::blur_radius_for_zoom(st.z).hash(&mut h);
            disaster_markers.hash(&mut h); // 代表点マーカーの出し分け(z11未満では出さない)
            st.population_layer.generation().hash(&mut h);
            // 人口メッシュ: ON/OFF・年次・濃さを変えたら描き直す(セル表は動かないため generation
            // だけでは変化を拾えない)。
            st.cfg.population_enabled.hash(&mut h);
            if st.cfg.population_enabled {
                population_year_idx.hash(&mut h);
                population_opacity_value(&st.cfg).to_bits().hash(&mut h);
            }
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
                None => build_window_nowait(rcx, rcy, rz, rw, rh, &st.opts.style, subpixel, &loader),
            };
            match built {
                Ok(mut img) => {
                    // 雨雲レーダーの降水レイヤ。未取得タイルは透明のまま返る(グレー箱もLOADING
                    // 透かしも出さない)。視野が日本国外/広域すぎる場合は None = 何も重ねない。
                    let radar_layer: Option<RgbaImage> = if st.radar_on {
                        st.radar_tl.get(st.radar_idx)
                            .and_then(|f| build_radar_window_nowait(rcx, rcy, rz, rw, rh, f, &loader))
                    } else { None };
                    // 過去災害のコロプレス層(市区町村を記録の多さで塗り分けたRGBA)。
                    // 雨雲と同じ合成経路に「雨雲より後ろ」として乗せる(雨は今の話・災害履歴は
                    // 土地の話なので、今の情報が上に来るのが正しい)。
                    let choro_shading = choropleth::Shading {
                        // 濃さは層の中身に影響しない(合成時に掛ける)ので、ここで控えておいて
                        // 3経路それぞれへ同じ値を渡す。設定行にするのは Stage2。
                        opacity: choropleth::DEFAULT_OPACITY,
                        fill: choropleth_fill,
                        blur_radius: choropleth::blur_radius_for_zoom(st.z),
                    };
                    let choro_opacity = choro_shading.opacity;
                    let choro_layer: Option<RgbaImage> = if choropleth_fill {
                        choropleth::build_layer(&disaster_sites, &muni_areas, rcx, rcy, rz, rw, rh, choro_shading)
                    } else { None };
                    // 500mメッシュ人口。人口なし=透明の面レイヤを作る(設計 §7.1)。
                    // 雨雲より背面に置く(人口は数年変わらない下地・雨雲は現況)。
                    let pop_layer: Option<RgbaImage> = if st.cfg.population_enabled && !population_meshes.is_empty() {
                        Some(build_population_layer(&population_meshes, population_year_idx,
                                                    population::Metric::Density, rcx, rcy, rz, rw, rh))
                    } else { None };
                    // 実画像モードはここで地図へ直接アルファ合成する(オーバーレイはこの後に焼くので
                    // 経路/POI/中心十字は常に雨雲・人口より前面に残る)。
                    // 下から コロプレス → 人口 → 雨雲 の順で重ねる(土地の話が下・今の話が上)。
                    if img_inline {
                        if let Some(l) = &choro_layer { blend_rgba_over(&mut img, l, choro_opacity); }
                        if let Some(l) = &pop_layer { blend_rgba_over(&mut img, l, population_opacity_value(&st.cfg)); }
                        if let Some(l) = &radar_layer { blend_rgba_over(&mut img, l, radar_opacity_value(&st.cfg)); }
                    }
                    // braille/edge は OverlayLayer へインクとして焼く(build_overlay の先頭で最背面に入る)。
                    // 配列の順序がそのまま重ね順(先頭が最背面)なので、上と同じ順で積む。
                    // コロプレスの面塗りだけは雨雲・人口のディザではなく疎な点描で間引く
                    // (Bayerだと braille のセルが全部塗り色に化けるため)。
                    let mut inks: Vec<InkLayer> = Vec::new();
                    if radar_ink {
                        if let Some(l) = &choro_layer {
                            inks.push(InkLayer::Stipple { layer: l, base: choropleth::STIPPLE_SPACING });
                        }
                        if let Some(l) = &pop_layer {
                            inks.push(InkLayer::Dither { layer: l, density: population_opacity_value(&st.cfg) });
                        }
                        if let Some(l) = &radar_layer {
                            inks.push(InkLayer::Dither { layer: l, density: radar_opacity_value(&st.cfg) });
                        }
                    }
                    let mut ov = build_overlay(&st.spec, rcx, rcy, rz, rw, rh, 1.0, 1.0, rw, rh, &inks);
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
                    if st.cfg.disaster_enabled && disaster_markers { // 過去災害(Bでその地点の事例一覧)
                        // 座標が市区町村の代表点で1点に何十件も重なるため、事例1件=1マーカーには
                        // しない。1座標=1マーカーにして、件数を外周リングの半径、最も多い種別を
                        // 色で表す。外周を細くするのは地図と他レイヤを覆い隠さないため
                        // (中心の塊があるので細くても位置は読める)。
                        // 塗り(コロプレス)が出ているズーム帯では件数の役目が塗りへ移るので、
                        // 外周リングは出さず中心の小さな塊だけを残す(Bキーの対象を示すため)。
                        for s in &disaster_sites {
                            let (gx, gy) = deg_to_pixel(s.lat, s.lon, rz);
                            let ix = (gx - (rcx - rw as f64 / 2.0)).floor() as i32;
                            let iy = (gy - (rcy - rh as f64 / 2.0)).floor() as i32;
                            let color = s.dominant().color();
                            draw_ring(&mut ov, ix, iy, 1, color, 2);
                            if disaster_rings {
                                draw_ring(&mut ov, ix, iy, disaster::marker_radius(s.total()), color, 1);
                            }
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
                        // 配列の順序がそのまま重ね順。コロプレス → 人口 → 雨雲 の順で地図へ
                        // アルファ合成する(braille/edge は上で ov のインクに入れ済みなので渡さない)。
                        let mut blends: Vec<(&RgbaImage, f64)> = Vec::new();
                        if !radar_ink {
                            if let Some(l) = &choro_layer { blends.push((l, choro_opacity)); }
                            if let Some(l) = &pop_layer { blends.push((l, population_opacity_value(&st.cfg))); }
                            if let Some(l) = &radar_layer { blends.push((l, radar_opacity_value(&st.cfg))); }
                        }
                        render(&img, &st.opts, Some(&ov), &blends)
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
        let jobs_active = st.jobs_active();
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
                area: None,
                count: traffic_points.len(),
                job_active: st.traffic_layer.job_active() || st.roads_layer.job_active(),
                stale_age_secs: st.traffic_layer.stale_age_secs(plot_now),
                wide_area: st.traffic_layer.suppressed(),
            },
            camera: ui_status::PlotStatus {
                area: None,
                count: camera_points.len(),
                job_active: st.camera_layer.job_active(),
                stale_age_secs: st.camera_layer.stale_age_secs(plot_now),
                wide_area: st.camera_layer.suppressed(),
            },
            regulation: ui_status::PlotStatus {
                area: None,
                count: regulation_events.len(),
                job_active: st.regulation_layer.job_active(),
                stale_age_secs: st.regulation_layer.stale_age_secs(plot_now),
                wide_area: st.regulation_layer.suppressed(),
            },
            // 過去災害は事例数でなく地点数を出す(1地点に最大166件が重なるため)。
            // 事例一覧(Bキー)の取得中もスピナーではなくこのレイヤの表示で分かるようにする。
            population: ui_status::PopulationStatus {
                job_active: st.population_layer.job_active(),
                // 取得中のセルキーは都道府県コード2桁。名前に直して「北海道を取得中…」と出す。
                fetching: st.population_layer
                    .fetching_key()
                    .and_then(|k| k.parse::<u8>().ok())
                    .map(|p| population::pref_name(p).to_string())
                    .filter(|n| !n.is_empty()),
                wide_area: st.population_layer.suppressed(),
                density: population_here,
            },
            disaster: ui_status::PlotStatus {
                count: disaster_sites.len(),
                job_active: st.disaster_layer.job_active() || st.disaster_job.is_some() || st.boundary_layer.job_active(),
                stale_age_secs: st.disaster_layer.stale_age_secs(plot_now),
                // 塗りOFF・z9〜z10 では enabled を落として取りに行かないので suppressed が
                // 立たない。それだと「なぜ何も出ないのか」が画面から分からなくなるため、
                // こちらの条件でも「広域では非表示」を出す(広域版 §3.1)。
                wide_area: st.disaster_layer.suppressed() || (st.cfg.disaster_enabled && !disaster_wanted),
                // 塗りが出ているときだけ「いまいる市区町村と、その町の記録件数」を出す
                // (凡例を置く幅が無いので、代わりに読み手が知りたいことへ直接答える)。
                area: choropleth_fill
                    .then(|| choropleth::area_summary(&disaster_sites, &muni_areas, lat, lon))
                    .flatten(),
            },
            weather_warning_count: st.route_warnings.len(),
            weather_warning_top_name: st.route_warnings.first().map(|w| w.name.as_str()),
            weather_warning_job_active: st.route_warning_job.is_some(),
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

        // 走らせておいたジョブの結果の取り込みは ui_jobs.rs へ切り出し済み。
        // 何か適用できたフレームは入力待ちでブロックせず即座に描き直す。
        let got_result = ui_jobs::poll(&mut st, &loader, lat, lon, &route_nogos, route_nogos_truncated);

        // 入力待ち。結果適用直後は即再描画(None)。ジョブ/GPS/再生/移動settling中はポーリング。
        // settling中は短間隔(60ms)で見に行き、動きが止まったフレームで高解像度に上げ直す。
        // ローダーがまだ未取得タイルを抱えている間もポーリング側に倒す(read()でブロックすると
        // 無入力時に届いたタイルが画面へ反映されないため)。
        // is_busy()に加えgenerationのスナップショット比較も見る(#53): このフレームの再構築後、
        // is_busy()を読むまでの間に最後の1枚がちょうど着地しinflightが空になっていた場合、
        // is_busy()だけではその1枚の反映漏れを検知できずread()でブロックしてしまうため。
        let polling = st.jobs_active() || st.voice_preview_job.is_some()
            || st.gps_rx.is_some() || st.play.is_some() || settling || loader.is_busy() || loader.generation() != loader_gen_snapshot
            || st.radar_clock.is_some() // 雨雲: 背景ポーラーからの時刻一覧を取りこぼさない
            // 道路交通量/主要道路/ライブカメラ/通行規制の背景取得完了を、キー入力無しでも
            // 取りこぼさない(結果が最大60秒(IDLE_SAVE_INTERVAL)反映されない事故を防ぐ)。
            // 主要道路は以前この条件から漏れていたが、4レイヤとも同じ扱いにする。
            // 人口メッシュは1セルの取得に数十秒かかるため、ここから漏れると
            // 「取得中…」の表示すら出ないまま画面が固まって見える(PTY実機で確認済み)。
            || st.traffic_layer.job_active() || st.roads_layer.job_active()
            || st.camera_layer.job_active() || st.regulation_layer.job_active() || st.disaster_layer.job_active()
            || st.boundary_layer.job_active() || st.population_layer.job_active()
            || st.route_warning_job.is_some();
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
            // 実画像モードでは実際に描かれる解像度は ow/oh ではなく rw/rh(zoom rz のピクセル)。
            // 表示している地理範囲は zoom z 換算で常に横 map_cols・縦 map_rows*2 ピクセルなので、
            // rw/scale・rh/scale へ戻して渡す(設計 §5.5 対策E)。AA 用に計算された ow/oh を
            // そのまま渡すと、braille(または --edge)と実画像を同時に有効にしたときだけ
            // ow=map_cols*2 / oh=map_rows*4 となり、両軸とも指の2倍地図が動く(§2.5 の実測)。
            let (pan_ow, pan_oh) = if img_inline { (rw / scale, rh / scale) } else { (ow, oh) };
            let lay = dragmode::Layout { cols, rows: tr as u32, map_cols, map_rows, ow: pan_ow, oh: pan_oh };
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
                if st.jobs_active() { st.cancel_jobs(); }
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
            Some(Event::Key(k)) if k.code == KeyCode::Esc && matches!(st.focus, Focus::Map) && st.jobs_active() => {
                st.cancel_jobs();
            }
            Some(Event::Key(k)) => {
                // Focus ごとのキー処理は ui_keys.rs へ切り出し済み。
                // 終了(q)はループを抜ける必要があるので戻り値で受け取る。
                let kx = ui_keys::KeyCtx { a, loader: &loader, lat, lon, nogos: &route_nogos, ow, oh };
                if ui_keys::dispatch(&mut st, k, &kx, &mut out) { break; }
            }
            // web/touch-overlay.js が window.term.paste() で送ってくる、ブラウザの
            // Geolocation APIによるライブ現在地。SOH(\u{1})区切りの専用マーカーにしているのは、
            // 普通に貼り付けられるURL/テキストと衝突しない制御文字だから。マーカーに一致しない
            // 通常のペーストは下の既存分岐(検索欄への入力等)へ素通しする。
            Some(Event::Paste(s)) if s.starts_with("\u{1}GPS_STOP\u{1}") => {
                st.web_gps_active = false;
                st.addr = "ライブ現在地(スマホ): OFF".into();
            }
            // スマホ側のGeolocation APIが失敗した時の理由(web/touch-overlay.js::gpsErrorReason)。
            // 「ボタンを押しても権限ダイアログすら出ない」といった切り分けをステータス行で
            // できるようにする診断用。成功時は既存のGPSマーカー分岐がaddrを上書きする。
            Some(Event::Paste(s)) if s.starts_with("\u{1}GPS_ERR\u{1}") => {
                let reason = &s["\u{1}GPS_ERR\u{1}".len()..];
                let msg = match reason {
                    "denied" => "権限拒否(Safariのサイト設定/位置情報サービスを確認)",
                    "unavailable" => "位置情報を取得できません",
                    "timeout" => "取得がタイムアウトしました",
                    "unsupported" => "この接続では使えません(HTTPS化を確認)",
                    _ => "原因不明のエラー",
                };
                st.addr = format!("ライブ現在地(スマホ): 失敗 - {msg}");
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
            // ブラウザが実測したセル寸法(設計書 §7.2 の経路2)。ttyd は pty の ws_xpixel/ws_ypixel
            // を埋めないため、web版ではここが唯一のセル比の入手経路になる。壊れた値・非現実的な
            // 比は parse 側が捨て、その場合は既定値 2.0 のまま(=修正前と同じ)動く。
            Some(Event::Paste(s)) if s.starts_with(cellratio::CELL_MARKER) => {
                if let Some(r) = cellratio::parse_cell_marker(&s) {
                    if st.cell_ratio_web != Some(r) {
                        st.cell_ratio_web = Some(r);
                        // 比が変わった=いま出ている画像の形が古い。次フレームで1枚描き直す。
                        st.force_reemit = true;
                        st.last_map_sig = None; // sig一致による再構築スキップに巻き込まれないように
                    }
                }
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


