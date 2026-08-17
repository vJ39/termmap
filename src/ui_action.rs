// Spaceメニューの項目・ルートパネルの操作行・地図の直接キーのどこから呼ばれても同じ処理を
// 走らせる実行部。もとは ui.rs の interactive() 内の run_action! マクロで、状態を UiState へ
// 集約したことで普通の関数にできた(マクロだったのは、裸のローカル変数40個以上を参照する
// 関数を書けなかったという消極的な理由による)。
//
// lat/lon(画面中心の緯度経度)と nogos(通行止め回避の指定)は毎フレーム計算し直す値なので
// 引数で受け取る。a は起動時のCLI引数で、実行中に変わる描画設定 st.opts とは別物。

use crate::focus::Focus;
use crate::menu::MenuAction;
use crate::route::*;
use crate::share::*;
use crate::spots::*;
use crate::ui_helpers::*;
use crate::uistate::UiState;
use crate::*;

pub(crate) fn run_action(st: &mut UiState, a: &Args, act: MenuAction, lat: f64, lon: f64, nogos: &str) {
    match act {
        MenuAction::SearchPlace => { st.input_cur = 0; st.focus = Focus::Search(String::new()); }
        MenuAction::SearchPoi => { st.focus = Focus::PoiMenu; }
        MenuAction::ShowAddress => { st.addr = reverse_geocode(lat, lon).unwrap_or_else(|e| format!("({e})")); }
        MenuAction::Recommend => {
            if !st.cfg.llm_recommend_enabled { st.snd.play("error"); st.addr = "おすすめ: 設定でOFF(,でON)".into(); }
            else if !recommend::claude_available(&st.cfg.llm_command) { st.snd.play("error"); st.addr = "おすすめ: claudeが無い(設定のLLM/コマンド確認)".into(); }
            else { st.input_cur = 0; st.focus = Focus::Recommend(String::new()); }
        }
        MenuAction::RouteForm => { if st.wps.is_empty() { st.addr = "先に v で地点を置いてね".into(); } else { st.wp_sel = 0; st.grab = false; st.focus = Focus::WaypointList; } }
        MenuAction::AddVia => { st.snd.play("pop"); wp_add(&mut st.wps, (lat, lon)); let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, nogos); st.route_note = n_; st.route_job = j_; st.addr = format!("地点を追加 #{}", st.wps.len()); }
        MenuAction::RoadRoute => { st.input_cur = 0; st.focus = Focus::RoadSearch(String::new()); }
        MenuAction::Wander => { st.focus = Focus::WanderForm { dist_km: a.dist.unwrap_or(40.0) }; } // 距離ゲージを開く(Enterで検索開始)
        MenuAction::CycleMode => { st.mode = match mode_label(&st.mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, nogos); st.route_note = n_; st.route_job = j_; }
        MenuAction::AltRoute => {
            if st.wps.len() >= 2 {
                st.route_alt = (st.route_alt + 1) % 4;
                let (nn, jj) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, st.route_alt, &st.cfg.google_maps_api_key, nogos);
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
                        let da = (a.lat - lat).powi(2) + (a.lon - lon).powi(2);
                        let db = (b.lat - lat).powi(2) + (b.lon - lon).powi(2);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roadseg::RoadSeg;
    use crate::uistate::testing::*;

    // 通信・ディスク・外部コマンドに触らない範囲だけを対象にする。地点が2個未満なら
    // trigger_route はスレッドを起こさないので、ルート絡みの分岐もここで確かめられる。
    fn run(st: &mut UiState, act: MenuAction) {
        let a = test_args();
        run_action(st, &a, act, 35.0, 139.0, "");
    }

    #[test]
    fn search_place_opens_an_empty_input() {
        let mut st = test_state();
        st.input_cur = 7;
        run(&mut st, MenuAction::SearchPlace);
        assert!(matches!(&st.focus, Focus::Search(q) if q.is_empty()));
        assert_eq!(st.input_cur, 0, "空欄なのでカーソルは先頭");
    }

    #[test]
    fn search_poi_opens_the_category_menu() {
        let mut st = test_state();
        run(&mut st, MenuAction::SearchPoi);
        assert!(matches!(st.focus, Focus::PoiMenu));
    }

    #[test]
    fn recommend_reports_when_turned_off() {
        let mut st = test_state();
        st.cfg.llm_recommend_enabled = false;
        run(&mut st, MenuAction::Recommend);
        assert!(st.addr.contains("設定でOFF"));
        assert!(matches!(st.focus, Focus::Map), "画面は移動しない");
    }

    #[test]
    fn route_form_needs_a_waypoint_first() {
        let mut st = test_state();
        run(&mut st, MenuAction::RouteForm);
        assert_eq!(st.addr, "先に v で地点を置いてね");
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn route_form_opens_the_list_from_the_top() {
        let mut st = test_state();
        st.wps = vec![(35.0, 139.0), (35.1, 139.1)];
        st.wp_sel = 1;
        st.grab = true;
        run(&mut st, MenuAction::RouteForm);
        assert!(matches!(st.focus, Focus::WaypointList));
        assert_eq!(st.wp_sel, 0);
        assert!(!st.grab, "掴んだ状態は持ち込まない");
    }

    #[test]
    fn add_via_appends_the_center_without_starting_a_route() {
        let mut st = test_state();
        run(&mut st, MenuAction::AddVia);
        assert_eq!(st.wps, vec![(35.0, 139.0)]);
        assert_eq!(st.addr, "地点を追加 #1");
        assert!(st.route_job.is_none(), "1点だけならルート計算は始めない");
    }

    #[test]
    fn cycle_mode_walks_surface_highway_short() {
        let mut st = test_state();
        assert_eq!(st.mode, "surface");
        run(&mut st, MenuAction::CycleMode);
        assert_eq!(st.mode, "highway");
        run(&mut st, MenuAction::CycleMode);
        assert_eq!(st.mode, "short");
        run(&mut st, MenuAction::CycleMode);
        assert_eq!(st.mode, "surface", "3種を巡回して戻る");
    }

    #[test]
    fn alt_route_needs_two_waypoints() {
        let mut st = test_state();
        st.wps = vec![(35.0, 139.0)];
        run(&mut st, MenuAction::AltRoute);
        assert_eq!(st.addr, "ルート未確定");
        assert_eq!(st.route_alt, 0, "代替ルートの番号は進めない");
    }

    #[test]
    fn clear_route_asks_before_wiping() {
        let mut st = test_state();
        run(&mut st, MenuAction::ClearRoute);
        assert!(!st.clear_route_confirm, "消すものが無ければ確認も出さない");
        st.wps = vec![(35.0, 139.0)];
        run(&mut st, MenuAction::ClearRoute);
        assert!(st.clear_route_confirm);
    }

    #[test]
    fn manage_roads_reports_when_there_is_nothing_to_manage() {
        let mut st = test_state();
        run(&mut st, MenuAction::ManageRoads);
        assert!(st.addr.contains("道路の塊がまだ無い"));
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn manage_roads_opens_the_list_from_the_top() {
        let mut st = test_state();
        st.road_segs = vec![RoadSeg { name: "国道1号".into(), pts: vec![(35.0, 139.0)], color: [1, 2, 3] }];
        st.road_sel = 5;
        run(&mut st, MenuAction::ManageRoads);
        assert!(matches!(st.focus, Focus::RoadList));
        assert_eq!(st.road_sel, 0);
    }

    #[test]
    fn manage_spots_opens_the_category_list() {
        let mut st = test_state();
        st.cat_sel = 3;
        run(&mut st, MenuAction::ManageSpots);
        assert!(matches!(st.focus, Focus::SpotCatList));
        assert_eq!(st.cat_sel, 0);
    }

    #[test]
    fn toggle_spots_flips_the_flag_and_the_message() {
        let mut st = test_state();
        st.show_spots = true;
        run(&mut st, MenuAction::ToggleSpots);
        assert!(!st.show_spots);
        assert_eq!(st.addr, "マイスポット非表示");
        run(&mut st, MenuAction::ToggleSpots);
        assert!(st.show_spots);
        assert_eq!(st.addr, "マイスポット表示");
    }

    #[test]
    fn toggle_elevation_warns_until_a_route_exists() {
        let mut st = test_state();
        run(&mut st, MenuAction::ToggleElevation);
        assert!(st.show_elev, "表示自体はONにする");
        assert_eq!(st.addr, "標高: ルート確定後に表示");
        st.addr.clear();
        run(&mut st, MenuAction::ToggleElevation);
        assert!(!st.show_elev);
        assert!(st.addr.is_empty(), "OFFにするときは何も言わない");
    }

    #[test]
    fn street_view_reports_a_missing_api_key() {
        let mut st = test_state();
        assert!(st.cfg.google_maps_api_key.is_empty());
        run(&mut st, MenuAction::StreetView);
        assert!(st.addr.contains("APIキー未設定"));
        assert!(st.street_job.is_none());
    }

    #[test]
    fn play_route_reports_when_there_is_no_route() {
        let mut st = test_state();
        run(&mut st, MenuAction::PlayRoute);
        assert_eq!(st.addr, "再生: ルート未確定");
        assert!(st.play.is_none());
    }

    #[test]
    fn view_camera_reports_when_the_layer_is_off() {
        let mut st = test_state();
        assert!(!st.cfg.camera_enabled);
        run(&mut st, MenuAction::ViewCamera);
        assert!(st.addr.contains("OFF"));
        assert!(st.cam_job.is_none());
    }

    #[test]
    fn save_route_prefills_the_last_used_name() {
        let mut st = test_state();
        st.route_name_hint = "箱根".to_string();
        run(&mut st, MenuAction::SaveRoute);
        assert!(matches!(&st.focus, Focus::SaveName(n) if n == "箱根"));
        assert_eq!(st.input_cur, 2, "カーソルは名前の末尾(文字単位)");
    }

    #[test]
    fn save_gpx_reports_when_there_is_no_route() {
        let mut st = test_state();
        run(&mut st, MenuAction::SaveGpx);
        assert_eq!(st.addr, "ルート未確定");
    }

    #[test]
    fn share_qr_needs_a_route() {
        let mut st = test_state();
        st.wps = vec![(35.0, 139.0)];
        run(&mut st, MenuAction::ShareQr);
        assert_eq!(st.addr, "ルート未確定");
        assert!(st.qr_view.is_none());
    }

    #[test]
    fn wander_opens_the_distance_gauge() {
        let mut st = test_state();
        let mut a = test_args();
        a.dist = Some(80.0);
        run_action(&mut st, &a, MenuAction::Wander, 35.0, 139.0, "");
        assert!(matches!(st.focus, Focus::WanderForm { dist_km } if dist_km == 80.0));
        // CLIで距離を指定していなければ既定の40km
        let mut st = test_state();
        run(&mut st, MenuAction::Wander);
        assert!(matches!(st.focus, Focus::WanderForm { dist_km } if dist_km == 40.0));
    }

    #[test]
    fn help_always_opens_at_the_first_page() {
        let mut st = test_state();
        st.help_page = 3;
        run(&mut st, MenuAction::Help);
        assert!(st.help);
        assert_eq!(st.help_page, 0);
    }
}
