// 探して候補を出す画面のキー処理。ui_keys.rs の Focus 分岐から関心ごとに切り出した1つ。
// 地名検索(/)・道路名検索・AIおすすめ・目的地カテゴリ(f)・キーワード周辺検索・その結果一覧。
// どれも「入力 → 別スレッドで問い合わせ → 結果は ui_jobs 側で受け取る」形で、
// 問い合わせを投げたら画面は Map へ戻す(待っている間も地図を動かせるようにするため)。
//
// 引数は「そのフレームの値」のうち各画面が実際に使うものだけを受け取る(何に依存しているかを
// 引数で見えるようにするため。3つ以上必要になる画面は KeyCtx をまとめて受け取る)。

use crate::focus::Focus;
use crate::geo::*;
use crate::poi::*;
use crate::route::*;
use crate::spots::ensure_spot_cat;
use crate::textedit::{edit_line, form_cur};
use crate::ui_keys::KeyCtx;
use crate::uistate::UiState;
use crate::*;
use crossterm::event::{KeyCode, KeyEvent};

pub(crate) fn search(st: &mut UiState, k: KeyEvent, mut buf: String, lat: f64, lon: f64) {
    match k.code {
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
    }
}

pub(crate) fn road_search(st: &mut UiState, k: KeyEvent, mut buf: String, ow: u32, oh: u32) {
    match k.code { // 道路名/ref で現在view内をルート化
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
    }
}

pub(crate) fn recommend(st: &mut UiState, k: KeyEvent, mut buf: String, lat: f64, lon: f64) {
    match k.code { // おすすめ: 方向性→claude -p→実在確認→候補一覧
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
    }
}

pub(crate) fn poi_kind_form(st: &mut UiState, k: KeyEvent, mut label: String, mut tag: String, mut field: usize) {
    match k.code { // 目的地カテゴリの新規追加フォーム
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
    }
}

pub(crate) fn near_search(st: &mut UiState, k: KeyEvent, mut buf: String, kx: &KeyCtx) {
    let KeyCtx { lat, lon, ow, oh, .. } = *kx;
    match k.code {
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
    }
}

pub(crate) fn poi_menu(st: &mut UiState, k: KeyEvent, kx: &KeyCtx) {
    let KeyCtx { lat, lon, ow, oh, .. } = *kx;
    match k.code {
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
                    // 中心・ズームは先に取り出す(&mut UiState 越しだと move クロージャが
                    // st ごと持って行こうとするため。読む値も読む時点も変わらない)。
                    let (mcx, mcy, mz) = (st.cx, st.cy, st.z);
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let r = poi_search(&kind, mcx, mcy, mz, ow, oh, lat, lon);
                        let _ = tx.send((label, r));
                    });
                    st.catpoi_job = Some(rx);
                    st.focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                }
                None => st.focus = Focus::PoiMenu,
            }
        }
        _ => st.focus = Focus::PoiMenu,
    }
}

pub(crate) fn poi_list(st: &mut UiState, k: KeyEvent, oh: u32, route_nogos: &str) {
    match k.code {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::{Cache, TileLoader};
    use crate::uistate::testing::*;
    use crossterm::event::KeyModifiers;

    // TileLoader はワーカースレッドを起こすのでテスト全体で1つだけ使い回す。
    fn shared_loader() -> &'static TileLoader {
        static L: std::sync::OnceLock<TileLoader> = std::sync::OnceLock::new();
        L.get_or_init(|| TileLoader::start(std::sync::Arc::new(std::sync::Mutex::new(Cache::new()))))
    }

    // KeyCtx は起動時引数を借用するので、こちらも1つだけ作って使い回す。
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

    // ui_keys::dispatch は focus を Map へ倒してから呼ぶので、テストも同じ前提で始める
    // (「画面を出したままにする」分岐だけが focus を書き戻す)。
    //
    // 問い合わせを実際に投げる分岐(通信スレッドを起こす)と、目的地カテゴリの並べ替え/削除/追加
    // ($HOME/.config/termmap/poi-kinds.txt を書く)はテストから触らない。ここで確かめるのは
    // 入力の受け付け方・カーソル移動・画面遷移・キャッシュヒット時の適用だけ。
    fn base() -> UiState {
        let mut st = test_state();
        st.focus = Focus::Map;
        st.poi_kinds = vec![
            PoiKind { key: '1', label: "ガソスタ".into(), filter: "nwr[\"amenity\"=\"fuel\"]".into(), cat: PoiCat::Fuel },
            PoiKind { key: '2', label: "カフェ".into(), filter: "nwr[\"amenity\"=\"cafe\"]".into(), cat: PoiCat::Food },
        ];
        st
    }

    // 検索結果一覧を出した状態(候補2件・1件目を選択)。
    fn listed() -> UiState {
        let mut st = base();
        st.pois = vec![
            (35.1, 139.1, "箱根".to_string(), PoiCat::Waypoint),
            (36.2, 137.2, "白骨".to_string(), PoiCat::Other),
        ];
        st.poi_sel = 0;
        st.spot_cats = vec![("温泉".to_string(), 3, 2)];
        st
    }

    // 地名検索のキャッシュを1件だけ仕込む(ヒット時は通信しないので、ここだけ Enter を試せる)。
    fn seed_cache(st: &mut UiState, q: &str, lat: f64, lon: f64, results: Vec<(f64, f64, String)>) -> String {
        let ckey = searchcache::make_key("n", "ja", q, lat, lon);
        st.scache.insert(ckey.clone(), searchcache::CacheEntry { results, created_at: 100, last_used_at: 0 });
        ckey
    }

    #[test]
    fn a_cached_search_shows_the_hits_in_the_result_list() {
        let mut st = base();
        st.cfg.google_maps_api_key = String::new(); // キー無し=Nominatim("n")のキャッシュを引く
        let ckey = seed_cache(&mut st, "箱根", 35.2, 139.0, vec![
            (35.1, 139.1, "箱根湯本".to_string()),
            (35.2, 139.0, "箱根峠".to_string()),
        ]);
        search(&mut st, code(KeyCode::Enter), "箱根".to_string(), 35.2, 139.0);
        assert!(matches!(st.focus, Focus::PoiList), "ヒットしたら結果一覧へ");
        assert_eq!(st.pois.len(), 2);
        assert_eq!(st.pois[0].2, "箱根湯本");
        assert_eq!(st.poi_sel, 0, "選択は先頭から");
        assert_eq!(st.poi_label, "検索:箱根");
        assert_eq!(st.spec.pois.len(), 2, "地図のマーカーにも反映する");
        assert!(st.search_job.is_none(), "キャッシュヒットなら通信しない");
        assert!(st.scache[&ckey].last_used_at > 0, "使った時刻を更新する(LRU破棄の基準)");
    }

    #[test]
    fn a_cached_search_with_no_hits_reports_it_and_closes() {
        let mut st = base();
        st.cfg.google_maps_api_key = String::new();
        seed_cache(&mut st, "存在しない地名", 35.2, 139.0, Vec::new());
        search(&mut st, code(KeyCode::Enter), "存在しない地名".to_string(), 35.2, 139.0);
        assert_eq!(st.addr, "見つからない: 存在しない地名");
        assert!(st.pois.is_empty());
        assert!(matches!(st.focus, Focus::Map), "0件は入力欄を閉じてメッセージだけ出す");
        assert!(st.search_job.is_none());
    }

    #[test]
    fn the_search_box_ignores_an_empty_query_and_closes_on_esc() {
        let mut st = base();
        search(&mut st, code(KeyCode::Enter), "   ".to_string(), 35.2, 139.0);
        assert!(st.search_job.is_none(), "空欄のEnterでは何も起きない");
        assert!(matches!(st.focus, Focus::Map));

        st.focus = Focus::Map;
        search(&mut st, code(KeyCode::Esc), "箱根".to_string(), 35.2, 139.0);
        assert!(matches!(st.focus, Focus::Map), "Escは入力を捨てて閉じる");
    }

    #[test]
    fn typing_keeps_the_search_box_open() {
        let mut st = base();
        search(&mut st, ch('箱'), String::new(), 35.2, 139.0);
        match &st.focus {
            Focus::Search(buf) => assert_eq!(buf, "箱"),
            _ => panic!("文字入力中は入力欄のまま"),
        }
        assert_eq!(st.input_cur, 1, "カーソルは1文字分進む");
    }

    #[test]
    fn the_road_search_needs_a_name() {
        let mut st = base();
        road_search(&mut st, code(KeyCode::Enter), "  ".to_string(), 640, 400);
        assert!(st.road_job.is_none(), "空欄のEnterでは問い合わせない");
        assert!(matches!(st.focus, Focus::Map));

        st.focus = Focus::Map;
        road_search(&mut st, ch('国'), String::new(), 640, 400);
        match &st.focus {
            Focus::RoadSearch(buf) => assert_eq!(buf, "国"),
            _ => panic!("文字入力中は入力欄のまま"),
        }

        st.focus = Focus::Map;
        road_search(&mut st, code(KeyCode::Esc), "国道1号".to_string(), 640, 400);
        assert!(matches!(st.focus, Focus::Map));
        assert!(st.road_job.is_none());
    }

    #[test]
    fn the_recommend_box_needs_a_direction() {
        let mut st = base();
        recommend(&mut st, code(KeyCode::Enter), String::new(), 35.2, 139.0);
        assert!(st.recommend_job.is_none(), "空欄のEnterではAIを呼ばない");
        assert!(matches!(st.focus, Focus::Map));

        st.focus = Focus::Map;
        recommend(&mut st, ch('海'), String::new(), 35.2, 139.0);
        match &st.focus {
            Focus::Recommend(buf) => assert_eq!(buf, "海"),
            _ => panic!("文字入力中は入力欄のまま"),
        }

        st.focus = Focus::Map;
        recommend(&mut st, code(KeyCode::Esc), "海沿い".to_string(), 35.2, 139.0);
        assert!(matches!(st.focus, Focus::Map));
        assert!(st.recommend_job.is_none());
    }

    #[test]
    fn the_nearby_search_needs_a_keyword() {
        let mut st = base();
        let kx = kctx();
        near_search(&mut st, code(KeyCode::Enter), "\t".to_string(), &kx);
        assert!(st.near_job.is_none(), "空欄のEnterでは問い合わせない");
        assert!(matches!(st.focus, Focus::Map));

        st.focus = Focus::Map;
        near_search(&mut st, ch('湯'), String::new(), &kx);
        match &st.focus {
            Focus::NearSearch(buf) => assert_eq!(buf, "湯"),
            _ => panic!("文字入力中は入力欄のまま"),
        }

        st.focus = Focus::Map;
        near_search(&mut st, code(KeyCode::Esc), "湯".to_string(), &kx);
        assert!(matches!(st.focus, Focus::Map));
        assert!(st.near_job.is_none());
    }

    #[test]
    fn the_kind_form_walks_the_fields_and_esc_returns_to_the_menu() {
        let mut st = base();
        poi_kind_form(&mut st, code(KeyCode::Down), "パン屋".to_string(), "shop=bakery".to_string(), 0);
        match &st.focus {
            Focus::PoiKindForm { field, .. } => assert_eq!(*field, 1, "下でOSMタグ欄へ"),
            _ => panic!("フォームのまま"),
        }
        assert_eq!(st.input_cur, 11, "カーソルはタグの末尾");

        st.focus = Focus::Map;
        poi_kind_form(&mut st, code(KeyCode::Up), "パン屋".to_string(), "shop=bakery".to_string(), 0);
        match &st.focus {
            Focus::PoiKindForm { field, .. } => assert_eq!(*field, 3, "上へは末尾([戻る])へ回り込む"),
            _ => panic!("フォームのまま"),
        }

        st.focus = Focus::Map;
        poi_kind_form(&mut st, code(KeyCode::Enter), "パン屋".to_string(), "shop=bakery".to_string(), 3);
        assert!(matches!(st.focus, Focus::PoiMenu), "[戻る]のEnterはカテゴリ一覧へ");

        st.focus = Focus::Map;
        poi_kind_form(&mut st, code(KeyCode::Esc), "パン屋".to_string(), "shop=bakery".to_string(), 1);
        assert!(matches!(st.focus, Focus::PoiMenu), "Escもカテゴリ一覧へ");
    }

    #[test]
    fn the_kind_form_refuses_an_empty_label_or_a_broken_tag() {
        let mut st = base();
        poi_kind_form(&mut st, code(KeyCode::Enter), "  ".to_string(), "shop=bakery".to_string(), 2);
        assert_eq!(st.addr, "表示名を入力してください");
        assert!(matches!(st.focus, Focus::PoiKindForm { .. }), "直せるようフォームに留まる");
        assert_eq!(st.poi_kinds.len(), 2, "カテゴリは増えない");

        st.focus = Focus::Map;
        poi_kind_form(&mut st, code(KeyCode::Enter), "パン屋".to_string(), "shop".to_string(), 2);
        assert_eq!(st.addr, "OSMタグは key=value 形式(例: shop=bakery)");
        assert!(matches!(st.focus, Focus::PoiKindForm { .. }));
        assert_eq!(st.poi_kinds.len(), 2);

        st.focus = Focus::Map;
        poi_kind_form(&mut st, code(KeyCode::Enter), "パン屋".to_string(), "shop=\"x\"".to_string(), 2);
        assert_eq!(st.addr, "OSMタグは key=value 形式(例: shop=bakery)", "引用符などは受け付けない");
        assert_eq!(st.poi_kinds.len(), 2);
    }

    #[test]
    fn the_kind_form_types_into_the_selected_field() {
        let mut st = base();
        st.input_cur = 0;
        poi_kind_form(&mut st, ch('パ'), String::new(), String::new(), 0);
        match &st.focus {
            Focus::PoiKindForm { label, tag, .. } => { assert_eq!(label, "パ"); assert_eq!(tag, ""); }
            _ => panic!("フォームのまま"),
        }

        st.focus = Focus::Map;
        st.input_cur = 0;
        poi_kind_form(&mut st, ch('s'), "パ".to_string(), String::new(), 1);
        match &st.focus {
            Focus::PoiKindForm { label, tag, .. } => { assert_eq!(label, "パ"); assert_eq!(tag, "s"); }
            _ => panic!("フォームのまま"),
        }

        st.focus = Focus::Map;
        poi_kind_form(&mut st, ch('z'), "パ".to_string(), "s".to_string(), 2);
        match &st.focus {
            Focus::PoiKindForm { label, tag, .. } => { assert_eq!(label, "パ"); assert_eq!(tag, "s", "ボタン欄では文字を拾わない"); }
            _ => panic!("フォームのまま"),
        }
    }

    #[test]
    fn the_kind_menu_cursor_stops_at_the_top_and_at_the_search_row() {
        let mut st = base();
        let kx = kctx();
        poi_menu(&mut st, code(KeyCode::Up), &kx);
        assert_eq!(st.poimenu_sel, 0, "先頭より上へは行かない");

        st.focus = Focus::Map;
        poi_menu(&mut st, code(KeyCode::Down), &kx);
        st.focus = Focus::Map;
        poi_menu(&mut st, ch('s'), &kx);
        assert_eq!(st.poimenu_sel, 2, "カテゴリ2件の下にキーワード周辺検索の行がある");

        st.focus = Focus::Map;
        poi_menu(&mut st, code(KeyCode::Down), &kx);
        assert_eq!(st.poimenu_sel, 2, "最終行より下へは行かない");
        assert!(matches!(st.focus, Focus::PoiMenu), "移動だけならメニューのまま");
    }

    #[test]
    fn the_kind_menu_opens_the_keyword_search_and_the_new_kind_form() {
        let mut st = base();
        let kx = kctx();
        st.input_cur = 5;
        poi_menu(&mut st, ch('/'), &kx);
        match &st.focus {
            Focus::NearSearch(buf) => assert!(buf.is_empty(), "空欄から入力を始める"),
            _ => panic!("/ はキーワード周辺検索へ"),
        }
        assert_eq!(st.input_cur, 0);

        st.focus = Focus::Map;
        poi_menu(&mut st, ch('n'), &kx);
        match &st.focus {
            Focus::PoiKindForm { label, tag, field } => { assert!(label.is_empty()); assert!(tag.is_empty()); assert_eq!(*field, 0); }
            _ => panic!("n は新規カテゴリのフォームへ"),
        }
    }

    #[test]
    fn enter_on_the_last_row_of_the_kind_menu_opens_the_keyword_search() {
        let mut st = base();
        let kx = kctx();
        st.poimenu_sel = st.poi_kinds.len();
        poi_menu(&mut st, code(KeyCode::Enter), &kx);
        assert!(matches!(st.focus, Focus::NearSearch(_)), "最終行はキーワード周辺検索");
        assert!(st.catpoi_job.is_none(), "カテゴリ検索は投げない");
    }

    #[test]
    fn an_unknown_letter_keeps_the_kind_menu_and_esc_closes_it() {
        let mut st = base();
        let kx = kctx();
        poi_menu(&mut st, ch('Z'), &kx);
        assert!(matches!(st.focus, Focus::PoiMenu), "どのカテゴリのキーでもなければ何もしない");
        assert!(st.catpoi_job.is_none());

        st.focus = Focus::Map;
        poi_menu(&mut st, code(KeyCode::Esc), &kx);
        assert!(matches!(st.focus, Focus::Map), "Escで地図へ戻る");
    }

    #[test]
    fn the_result_list_follows_the_selection_on_the_map() {
        let mut st = listed();
        poi_list(&mut st, code(KeyCode::Down), 400, "");
        assert_eq!(st.poi_sel, 1);
        let (ex, ey) = deg_to_pixel(36.2, 137.2, st.z);
        assert_eq!((st.cx, st.cy), (ex, ey), "選択に地図が追従する");
        assert!(matches!(st.focus, Focus::PoiList));

        st.focus = Focus::Map;
        poi_list(&mut st, code(KeyCode::Down), 400, "");
        assert_eq!(st.poi_sel, 1, "末尾より下へは行かない");

        st.focus = Focus::Map;
        poi_list(&mut st, ch('w'), 400, "");
        assert_eq!(st.poi_sel, 0);
        let (ex, ey) = deg_to_pixel(35.1, 139.1, st.z);
        assert_eq!((st.cx, st.cy), (ex, ey));
    }

    #[test]
    fn enter_centers_the_map_on_the_selection() {
        let mut st = listed();
        st.poi_sel = 1;
        st.cx = 0.0;
        st.cy = 0.0;
        poi_list(&mut st, code(KeyCode::Enter), 400, "");
        let (ex, ey) = deg_to_pixel(36.2, 137.2, st.z);
        assert_eq!((st.cx, st.cy), (ex, ey));
        assert!(matches!(st.focus, Focus::PoiList), "一覧は出したまま");
    }

    #[test]
    fn the_arrows_pan_the_map_without_moving_the_selection() {
        let mut st = listed();
        st.cx = 1000.0;
        st.cy = 2000.0;
        poi_list(&mut st, code(KeyCode::Left), 400, "");
        assert_eq!((st.cx, st.cy), (950.0, 2000.0), "地図の高さの1/8だけ動く");
        assert_eq!(st.poi_sel, 0, "一覧の選択は動かさない");

        st.focus = Focus::Map;
        poi_list(&mut st, ch('j'), 400, "");
        assert_eq!((st.cx, st.cy), (950.0, 2050.0));
        assert!(matches!(st.focus, Focus::PoiList));
    }

    #[test]
    fn the_zoom_keys_scale_the_center_and_keep_the_list() {
        let mut st = listed();
        st.cx = 1000.0;
        st.cy = 2000.0;
        poi_list(&mut st, ch('+'), 400, "");
        assert_eq!((st.z, st.cx, st.cy), (15, 2000.0, 4000.0));

        st.focus = Focus::Map;
        poi_list(&mut st, ch('-'), 400, "");
        assert_eq!((st.z, st.cx, st.cy), (14, 1000.0, 2000.0));
        assert!(matches!(st.focus, Focus::PoiList));
    }

    #[test]
    fn v_adds_the_selected_point_as_a_waypoint() {
        let mut st = listed();
        assert!(st.wps.is_empty());
        poi_list(&mut st, ch('v'), 400, "");
        assert_eq!(st.wps, vec![(35.1, 139.1)]);
        assert_eq!(st.addr, "地点を追加 #1");
        assert!(st.route_job.is_none(), "1点だけならまだ経路は引かない");
        assert!(matches!(st.focus, Focus::PoiList));
    }

    #[test]
    fn capital_p_hands_the_selection_to_the_category_list() {
        let mut st = listed();
        st.cat_sel = 3;
        poi_list(&mut st, ch('P'), 400, "");
        assert_eq!(st.pending_spot, Some((35.1, 139.1, "箱根".to_string())));
        assert_eq!(st.cat_sel, 0, "カテゴリ選択は先頭から");
        assert!(matches!(st.focus, Focus::SpotCatList));
        assert_eq!(st.spot_cats.len(), 1, "既にカテゴリがあれば作らない");
    }

    #[test]
    fn esc_clears_the_results_and_the_markers() {
        let mut st = listed();
        set_markers(&mut st.spec, &st.wps, &st.pois);
        assert_eq!(st.spec.pois.len(), 2);
        poi_list(&mut st, code(KeyCode::Esc), 400, "");
        assert!(st.pois.is_empty());
        assert!(st.spec.pois.is_empty(), "地図のマーカーも消す");
        assert!(matches!(st.focus, Focus::Map));
    }

    #[test]
    fn f_returns_to_the_kind_menu() {
        let mut st = listed();
        poi_list(&mut st, ch('f'), 400, "");
        assert!(matches!(st.focus, Focus::PoiMenu));
    }
}
