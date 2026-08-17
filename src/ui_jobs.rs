// 走らせておいたバックグラウンドジョブの結果を毎フレーム取り込む部分。
// もとは ui.rs の interactive() 内にべた書きされていた約280行で、UiState へ状態を集約した
// ことでそのまま関数へ移せた。1フレームに1回だけ呼ぶ。
//
// try_recv の扱いは全ジョブ共通で Ok=結果を適用して job=None / Empty=次フレームへ持ち越し /
// Disconnected=None(送信側が落ちた)。戻り値は「このフレームで何か適用したか」で、true なら
// 呼び出し側は入力待ちでブロックせず即座に描き直す。
//
// lat/lon(画面中心)と nogos/nogos_truncated(通行止め回避の指定と件数上限で溢れたか)は
// 毎フレーム計算し直す値なので引数で受け取る。loader はタイル取得の常駐スレッドで、
// ルート確定時の周辺タイル先読みと、消えた雨雲コマの破棄で使う。

use crate::focus::Focus;
use crate::geo::*;
use crate::roadseg::{road_color_for, RoadSeg};
use crate::route::*;
use crate::tiles::TileLoader;
use crate::uistate::UiState;
use crate::*;

pub(crate) fn poll(st: &mut UiState, loader: &TileLoader, lat: f64, lon: f64, nogos: &str, nogos_truncated: bool) -> bool {
    use std::sync::mpsc::TryRecvError;
    let mut got_result = false;
    if st.route_job.is_some() {
        match st.route_job.as_ref().unwrap().try_recv() {
            Ok(Ok(r)) => {
                st.spec.routes.clear();
                st.spec.traffic_segments.clear(); // 古いルートの色分けを引き継がない
                st.spec.expressway_segments.clear(); // 古いルートの高速区間も同様
                st.spec.warning_segments.clear(); // 古いルートの気象警報も同様
                st.route_warnings.clear(); st.route_warning_job = None; // 新ルート確定でturn_jobを待ち直すため一旦クリア
                st.route_note = Some(route_summary(&st.mode, &r));
                // 通行止め回避が件数上限で一部反映できなかった場合、黙って進めると
                // 「回避できた」と誤解されるのでひとこと添える。
                if nogos_truncated {
                    // take() で取り出してから入れ直す(&mut 越しなのでフィールドから直接
                    // move できない。すぐ代入するので中身は変わらない)。
                    st.route_note = st.route_note.take().map(|n| format!("{n} (通行止めの一部は回避対象外)"));
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
                    st.turn_job = Some(trigger_turn_points(&st.wps, &st.mode, 0, &r.pts, &nogos));
                }
                // 高速区間(#高速区間)の点列は r.pts をムーブする前に作る。ルート結果と
                // 同時に確定するので、渋滞の色分けのような非同期の受け取り口は要らない。
                st.spec.expressway_segments = route::expressway_polylines(&r.pts, &r.hw_segments)
                    .into_iter()
                    .map(|pts| Route { pts, color: route::EXPRESSWAY_COLOR, thickness: 2 })
                    .collect();
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
            Ok(v) => {
                st.turn_points = v; st.voice_guide = Some(voice::VoiceGuide::new(&st.turn_points)); st.turn_job = None;
                // 気象警報(#79・ルートベース)。voice_guide作り直しと同じ「ルート確定時」フックで
                // ルート沿いの気象台コードを列挙し、まとめて背景取得する。
                if st.cfg.weather_warning_enabled {
                    if let Some(pts) = st.spec.routes.last().map(|rt| rt.pts.clone()) {
                        let office_codes = ui_helpers::route_warning_office_codes(&pts);
                        if !office_codes.is_empty() {
                            let (tx, rx) = std::sync::mpsc::channel();
                            std::thread::spawn(move || {
                                let mut all = Vec::new();
                                for code in office_codes {
                                    if let Ok(ws) = warning::fetch_warnings(&code) { all.extend(ws); }
                                }
                                let _ = tx.send(all);
                            });
                            st.route_warning_job = Some(rx);
                        }
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => { st.turn_job = None; }
        }
    }
    if let Some(job) = &st.route_warning_job {
        match job.try_recv() {
            Ok(ws) => {
                st.route_warnings = ws;
                if let Some(pts) = st.spec.routes.last().map(|rt| rt.pts.clone()) {
                    st.spec.warning_segments = ui_helpers::build_warning_segments(&pts, &st.route_warnings);
                }
                st.route_warning_job = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => { st.route_warning_job = None; }
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
    // 境界は塗りに使うときだけ取りに行く(塗りをOFFにしている人に通信させない)。
    got_result |= st.boundary_layer.tick(st.cx, st.cy, st.z, st.cfg.disaster_enabled && st.cfg.disaster_fill);
    got_result |= st.population_layer.tick(st.cx, st.cy, st.z, st.cfg.population_enabled);
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
                    st.route_note = st.route_note.take().map(|n| format!("{n} (渋滞あり: 黄/赤)"));
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
                    Ok(w) => { st.wps = w; st.wp_sel = 0; st.route_sel = 0; let (n_, j_) = trigger_route(&mut st.spec, &st.wps, &st.pois, &st.mode, 0, &st.cfg.google_maps_api_key, &nogos); st.route_note = n_; st.route_job = j_; }
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
    got_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poi::ApiError;
    use crate::spots::Spot;
    use crate::tiles::Cache;
    use crate::uistate::testing::*;

    // TileLoader はワーカースレッドを起こすのでテスト全体で1つだけ使い回す(ui_status.rs と同じ)。
    // ここで通すのはタイル先読みも雨雲の時刻一覧も伴わない経路だけなので、実際には触られない。
    fn shared_loader() -> &'static TileLoader {
        static L: std::sync::OnceLock<TileLoader> = std::sync::OnceLock::new();
        L.get_or_init(|| TileLoader::start(std::sync::Arc::new(std::sync::Mutex::new(Cache::new()))))
    }

    // 画面中心を (35.0, 139.0) 固定にして1フレーム分だけ回す。通行止め回避は指定なし。
    fn poll1(st: &mut UiState) -> bool {
        poll(st, shared_loader(), 35.0, 139.0, "", false)
    }

    // 1件送ってから閉じたチャネルを作る。ジョブの完了1回分を再現するための道具。
    fn sent<T>(v: T) -> std::sync::mpsc::Receiver<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(v).unwrap();
        rx
    }

    #[test]
    fn nothing_to_receive_leaves_every_job_in_place() {
        // 送信側を生かしたまま空のチャネルを置く(=まだ計算中)。畳んだり消したりしない。
        let mut st = test_state();
        let (tx_s, rx_s) = std::sync::mpsc::channel();
        let (tx_n, rx_n) = std::sync::mpsc::channel();
        st.search_job = Some(rx_s);
        st.near_job = Some(rx_n);
        assert!(!poll1(&mut st), "適用したものは無い");
        assert!(st.search_job.is_some());
        assert!(st.near_job.is_some());
        assert!(st.addr.is_empty());
        drop((tx_s, tx_n));
    }

    #[test]
    fn a_dropped_sender_clears_the_job() {
        // 送信側が結果を送らずに落ちた(スレッドがpanic等)。ジョブを畳んで前へ進む。
        let mut st = test_state();
        let (tx, rx) = std::sync::mpsc::channel::<(String, String, Result<Vec<(f64, f64, String)>, String>)>();
        drop(tx);
        st.search_job = Some(rx);
        assert!(poll1(&mut st));
        assert!(st.search_job.is_none());
    }

    #[test]
    fn route_failure_shows_the_reason_in_the_route_note() {
        let mut st = test_state();
        st.route_job = Some(sent(Err("BRouterに繋がらない".to_string())));
        assert!(poll1(&mut st));
        assert_eq!(st.route_note.as_deref(), Some("(BRouterに繋がらない)"));
        assert!(st.route_job.is_none());
    }

    #[test]
    fn turn_points_arrival_builds_the_voice_guide() {
        let mut st = test_state();
        st.turn_job = Some(sent(vec![TurnPoint { lat: 35.0, lon: 139.0, turn: "TL".into(), dist_from_start_m: 100.0 }]));
        // 曲がり角はルート線の付随情報なので、届いても即再描画はしない(got_resultを立てない)。
        assert!(!poll1(&mut st));
        assert_eq!(st.turn_points.len(), 1);
        assert!(st.voice_guide.is_some());
        assert!(st.turn_job.is_none());
    }

    // ---- 気象警報(#79・ルートベース)の配線 ----
    //
    // 以下は取得そのもの(warning::fetch_warnings)を走らせない経路だけを通す。
    // 「設定ON かつ ルートあり かつ 気象台コードが1つ以上」が揃うとスレッドを起こして
    // 気象庁へ問い合わせるので、その組み合わせは意図的に作らない。
    // 塗り分けの計算そのもの(build_warning_segments)は ui_helpers.rs 側で試している。

    // 新宿駅付近(東京地方=130010)を通る短いルート。geoarea.rs の実データを引くので実在の座標。
    fn tokyo_pts() -> Vec<(f64, f64)> {
        vec![(35.6896, 139.7006), (35.69, 139.701), (35.6905, 139.7015)]
    }

    fn warned(code: &str, severity: warning::Severity) -> warning::ActiveWarning {
        warning::ActiveWarning { area_code: code.to_string(), name: "大雨警報".to_string(), severity }
    }

    #[test]
    fn route_warnings_arrival_paints_the_route() {
        let mut st = test_state();
        let pts = tokyo_pts();
        st.spec.routes.push(Route { pts: pts.clone(), color: [0, 220, 255], thickness: 2 });
        st.route_warning_job = Some(sent(vec![warned("130010", warning::Severity::Advisory)]));
        // 警報はルート線の付随情報なので、届いても即再描画はしない(曲がり角と同じ扱い)。
        assert!(!poll1(&mut st));
        assert_eq!(st.route_warnings.len(), 1);
        assert_eq!(st.spec.warning_segments.len(), 1, "ルートを塗り分ける");
        assert_eq!(st.spec.warning_segments[0].pts, pts);
        assert!(st.route_warning_job.is_none());
    }

    #[test]
    fn route_warnings_that_match_no_area_leave_the_route_plain() {
        let mut st = test_state();
        st.spec.routes.push(Route { pts: tokyo_pts(), color: [0, 220, 255], thickness: 2 });
        st.route_warning_job = Some(sent(vec![warned("999999", warning::Severity::Warning)]));
        assert!(!poll1(&mut st));
        assert_eq!(st.route_warnings.len(), 1, "件数はステータス行に出すので保持する");
        assert!(st.spec.warning_segments.is_empty());
    }

    #[test]
    fn route_warnings_without_a_route_are_kept_but_paint_nothing() {
        // ルートを消した直後に結果が届く場合。塗る対象が無いだけで、件数は捨てない。
        let mut st = test_state();
        st.route_warning_job = Some(sent(vec![warned("130010", warning::Severity::Warning)]));
        assert!(!poll1(&mut st));
        assert_eq!(st.route_warnings.len(), 1);
        assert!(st.spec.warning_segments.is_empty());
        assert!(st.route_warning_job.is_none());
    }

    #[test]
    fn a_dropped_warning_sender_clears_the_job() {
        let mut st = test_state();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<warning::ActiveWarning>>();
        drop(tx);
        st.route_warning_job = Some(rx);
        assert!(!poll1(&mut st));
        assert!(st.route_warning_job.is_none());
        assert!(st.route_warnings.is_empty());
    }

    #[test]
    fn turn_points_do_not_ask_for_warnings_while_the_setting_is_off() {
        // 既定OFF。ONにした人だけが気象庁へ追加問い合わせする。
        let mut st = test_state();
        assert!(!st.cfg.weather_warning_enabled, "既定はOFF");
        st.spec.routes.push(Route { pts: tokyo_pts(), color: [0, 220, 255], thickness: 2 });
        st.turn_job = Some(sent(Vec::new()));
        assert!(!poll1(&mut st));
        assert!(st.route_warning_job.is_none());
    }

    #[test]
    fn turn_points_do_not_ask_for_warnings_without_a_route() {
        // ONでもルートが無ければ問い合わせ先が決まらない。
        let mut st = test_state();
        st.cfg.weather_warning_enabled = true;
        st.turn_job = Some(sent(Vec::new()));
        assert!(!poll1(&mut st));
        assert!(st.route_warning_job.is_none());
    }

    #[test]
    fn turn_points_do_not_ask_for_warnings_when_the_route_yields_no_office_code() {
        // ONでルートもあるが、そのルートから気象台コードが1つも出ない場合は空振りの
        // 通信をしない(点が無いルートで再現)。
        let mut st = test_state();
        st.cfg.weather_warning_enabled = true;
        st.spec.routes.push(Route { pts: Vec::new(), color: [0, 220, 255], thickness: 2 });
        st.turn_job = Some(sent(Vec::new()));
        assert!(!poll1(&mut st));
        assert!(st.route_warning_job.is_none());
    }

    #[test]
    fn a_new_route_clears_what_belonged_to_the_old_one() {
        // 高速区間(#高速区間)と気象警報(#79)はどちらもルートに紐づく付随情報なので、
        // 新しいルートが確定した時点で古い分を捨てる(でないと別のルートの上に前の色が残る)。
        // pts を空にしているのは周辺タイルの先読み(loader経由の通信)を起こさないため。
        let mut st = test_state();
        st.spec.expressway_segments = vec![Route { pts: tokyo_pts(), color: [1, 2, 3], thickness: 2 }];
        st.spec.warning_segments = vec![Route { pts: tokyo_pts(), color: [4, 5, 6], thickness: 3 }];
        st.route_warnings = vec![warned("130010", warning::Severity::Warning)];
        let (tx, rx) = std::sync::mpsc::channel::<Vec<warning::ActiveWarning>>();
        st.route_warning_job = Some(rx);
        st.route_job = Some(sent(Ok(RouteResult {
            pts: Vec::new(), ele: Vec::new(), dist_m: 1000.0, time_s: 600.0,
            hw_m: 0.0, hw_segments: Vec::new(), ascend_m: 0.0, via_google: false,
        })));
        assert!(poll1(&mut st));
        assert!(st.spec.expressway_segments.is_empty());
        assert!(st.spec.warning_segments.is_empty());
        assert!(st.route_warnings.is_empty());
        assert!(st.route_warning_job.is_none(), "turn_job の完了を待って取り直すので一旦畳む");
        assert!(st.route_job.is_none());
        drop(tx);
    }

    #[test]
    fn the_background_layers_stay_idle_while_their_settings_are_off() {
        // 市区町村境界は「過去災害ON かつ 塗りON」の両方が揃ったときだけ取りに行く
        // (既定は disaster_enabled=false / disaster_fill=true なので取りに行かない)。
        // 人口メッシュも既定OFF(1都道府県が最大31MBあるため)。
        let mut st = test_state();
        assert!(!st.cfg.disaster_enabled && st.cfg.disaster_fill, "既定の組み合わせ");
        assert!(!st.cfg.population_enabled);
        assert!(!poll1(&mut st));
        assert!(!st.boundary_layer.job_active());
        assert!(!st.population_layer.job_active());
    }

    #[test]
    fn search_failure_reports_and_keeps_the_map() {
        let mut st = test_state();
        st.search_job = Some(sent(("k".to_string(), "東京駅".to_string(), Err("timeout".to_string()))));
        assert!(poll1(&mut st));
        assert!(st.addr.contains("検索できません"));
        assert!(matches!(st.focus, Focus::Map));
        assert!(st.search_job.is_none());
    }

    #[test]
    fn search_with_no_hit_says_so() {
        let mut st = test_state();
        st.search_job = Some(sent(("k".to_string(), "存在しない地名".to_string(), Ok(vec![]))));
        assert!(poll1(&mut st));
        assert_eq!(st.addr, "見つからない: 存在しない地名");
        assert!(st.pois.is_empty(), "前の候補を消さない(空の結果で上書きしない)");
    }

    #[test]
    fn nearby_failure_still_shows_matching_local_spots() {
        let mut st = test_state();
        st.spots = vec![Spot { name: "秘湯".into(), lat: 35.01, lon: 139.01, cat: "温泉".into() }];
        st.near_job = Some(sent(("秘湯".to_string(), Err(ApiError::Http(503)))));
        assert!(poll1(&mut st));
        assert!(st.addr.starts_with("★のみ表示"), "障害でも★は出す: {}", st.addr);
        assert_eq!(st.pois.len(), 1);
        assert_eq!(st.pois[0].2, "★秘湯");
        assert!(matches!(st.focus, Focus::PoiList));
    }

    #[test]
    fn nearby_failure_without_local_spots_reports_the_error() {
        let mut st = test_state();
        st.near_job = Some(sent(("コンビニ".to_string(), Err(ApiError::Http(503)))));
        assert!(poll1(&mut st));
        assert!(st.addr.starts_with("周辺検索:"), "該当なしと障害を混同しない: {}", st.addr);
        assert!(st.pois.is_empty());
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn nearby_result_does_not_steal_another_screen() {
        // 結果が届くまでに別画面へ移っていたら焦点は奪わない(既定の Map のときだけ一覧を開く)。
        let mut st = test_state();
        st.focus = Focus::Settings;
        st.near_job = Some(sent(("コンビニ".to_string(), Ok(vec![(35.001, 139.001, "コンビニA".to_string())]))));
        assert!(poll1(&mut st));
        assert_eq!(st.pois.len(), 1);
        assert!(matches!(st.focus, Focus::Settings));
    }

    #[test]
    fn road_result_adds_one_segment_and_syncs_the_drawing() {
        let mut st = test_state();
        let pts = vec![(35.0, 139.0), (35.0, 139.001), (35.0, 139.002)];
        st.road_job = Some(sent(("国道1号".to_string(), Ok(vec![(pts, false)]))));
        assert!(poll1(&mut st));
        assert_eq!(st.road_segs.len(), 1);
        assert_eq!(st.road_segs[0].name, "国道1号");
        assert_eq!(st.spec.roads.len(), 1, "描画用のレイヤも作り直す");
        assert!(st.addr.contains("計1本"));
    }

    #[test]
    fn road_not_found_reports_without_touching_the_segments() {
        let mut st = test_state();
        st.road_job = Some(sent(("酷道".to_string(), Ok(vec![]))));
        assert!(poll1(&mut st));
        assert!(st.addr.contains("道路が見つからない"));
        assert!(st.road_segs.is_empty());
        assert!(st.spec.roads.is_empty());
    }

    #[test]
    fn category_poi_result_opens_the_list() {
        let mut st = test_state();
        let items = vec![(35.001, 139.001, "道の駅A".to_string(), PoiCat::Other)];
        st.catpoi_job = Some(sent(("道の駅".to_string(), Ok(items))));
        assert!(poll1(&mut st));
        assert_eq!(st.poi_label, "道の駅");
        assert_eq!(st.poi_sel, 0);
        assert!(matches!(st.focus, Focus::PoiList));
    }

    #[test]
    fn category_poi_with_no_hit_goes_back_to_the_menu() {
        let mut st = test_state();
        st.catpoi_job = Some(sent(("道の駅".to_string(), Ok(vec![]))));
        assert!(poll1(&mut st));
        assert_eq!(st.addr, "周辺2kmに道の駅無し");
        assert!(matches!(st.focus, Focus::PoiMenu), "選び直せるようカテゴリ一覧へ戻す");
    }

    #[test]
    fn recommend_result_opens_the_list() {
        let mut st = test_state();
        st.recommend_job = Some(sent(Ok(vec![(35.001, 139.001, "峠の茶屋".to_string())])));
        assert!(poll1(&mut st));
        assert_eq!(st.poi_label, "おすすめ");
        assert_eq!(st.pois.len(), 1);
        assert!(matches!(st.focus, Focus::PoiList));
    }

    #[test]
    fn recommend_with_no_verified_place_reports() {
        let mut st = test_state();
        st.recommend_job = Some(sent(Ok(vec![])));
        assert!(poll1(&mut st));
        assert_eq!(st.addr, "おすすめ: 実在確認できる地点なし");
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn wander_failure_reports() {
        let mut st = test_state();
        st.wander_job = Some(sent(Err("周回路を作れない".to_string())));
        assert!(poll1(&mut st));
        assert_eq!(st.addr, "(周回路を作れない)");
        assert!(st.wps.is_empty());
        assert!(st.wander_job.is_none());
    }

    #[test]
    fn street_view_failure_reports() {
        let mut st = test_state();
        st.street_job = Some(sent((35.0, 139.0, 0, Err("画像なし".to_string()))));
        assert!(poll1(&mut st));
        assert_eq!(st.addr, "実写: 画像なし");
        assert!(st.street.is_none());
    }

    #[test]
    fn traffic_coloring_appends_a_note_to_the_route() {
        let mut st = test_state();
        st.route_note = Some("40.0km".into());
        st.traffic_color_job = Some(sent(vec![([255, 0, 0], vec![(35.0, 139.0), (35.0, 139.001)])]));
        assert!(poll1(&mut st));
        assert_eq!(st.spec.traffic_segments.len(), 1);
        assert_eq!(st.route_note.as_deref(), Some("40.0km (渋滞あり: 黄/赤)"));
    }

    #[test]
    fn traffic_coloring_with_no_segment_keeps_the_plain_line() {
        let mut st = test_state();
        st.route_note = Some("40.0km".into());
        st.traffic_color_job = Some(sent(vec![]));
        assert!(poll1(&mut st));
        assert!(st.spec.traffic_segments.is_empty());
        assert_eq!(st.route_note.as_deref(), Some("40.0km"), "静かに諦める");
        assert!(st.traffic_color_job.is_none());
    }

    #[test]
    fn cause_failure_is_cached_as_other() {
        // 失敗もキャッシュに残す。残さないと同じ1件を毎フレーム引き直してしまう。
        let mut st = test_state();
        st.cause_job = Some(sent(("12345".to_string(), Err("取れない".to_string()))));
        assert!(poll1(&mut st));
        assert_eq!(st.cause_cache.get("12345"), Some(&regulation::CauseCategory::Other));
        assert!(st.cause_job.is_none());
    }

    #[test]
    fn disaster_list_arrival_opens_the_panel() {
        let mut st = test_state();
        st.disaster_job = Some(sent(Ok(("1959年 伊勢湾台風".to_string(), vec!["行1".to_string()]))));
        assert!(poll1(&mut st));
        let (title, lines) = st.disaster_view.as_ref().expect("パネルが開く");
        assert_eq!(title, "1959年 伊勢湾台風");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn disaster_failure_reports_without_opening_the_panel() {
        let mut st = test_state();
        st.disaster_job = Some(sent(Err("取得失敗".to_string())));
        assert!(poll1(&mut st));
        assert_eq!(st.addr, "災害事例: 取得失敗");
        assert!(st.disaster_view.is_none());
    }

    #[test]
    fn regulation_detail_failure_reports() {
        let mut st = test_state();
        st.regulation_detail_job = Some(sent(Err("詳細ページなし".to_string())));
        assert!(poll1(&mut st));
        assert_eq!(st.addr, "通行規制: 詳細ページなし");
        assert!(st.regulation_detail_view.is_none());
    }

    #[test]
    fn voice_preview_failure_reports() {
        let mut st = test_state();
        st.voice_preview_job = Some(sent(Err("その声は無い".to_string())));
        assert!(poll1(&mut st));
        assert_eq!(st.addr, "その声は無い");
        assert!(st.voice_preview_job.is_none());
    }

    #[test]
    fn camera_image_failure_reports() {
        let mut st = test_state();
        let cam = camera::RoadCamera {
            id: "1".into(), name: "国道1号".into(), lat: 35.0, lon: 139.0,
            thumb_url: None, full_url: None, taken_at: String::new(),
        };
        st.cam_job = Some(sent((cam, Err("画像URLを取得できない".to_string()))));
        // カメラは届いても即再描画しない(既存挙動)。次のフレームで反映される。
        assert!(!poll1(&mut st));
        assert!(st.addr.contains("カメラ画像取得失敗"));
        assert!(st.cam_view.is_none());
        assert!(st.cam_job.is_none());
    }
}
