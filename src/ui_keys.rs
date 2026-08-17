// Focus(いまどの画面を触っているか)ごとのキー処理。もとは ui.rs の interactive() 内に
// べた書きされていた28分岐の match で、状態を UiState へ集約したことでそのまま関数へ移せた。
//
// 端末ハンドル out とタイルローダー loader は UiState に持たせていない(uistate.rs を通信も
// ディスクも触らない素のデータに保つため)ので、引数で受け取る。
// 戻り値は「対話ループを抜けるか(=アプリ終了)」。q キーだけが true を返す。ループを持って
// いるのは ui.rs 側なので、break を関数の中に隠さずここから返す。

use crate::focus::Focus;
use crate::geo::*;
use crate::menu::{MenuAction, MENU_CATEGORIES, MenuLevel, menu_action_for_key, ROUTE_ACTS};
use crate::poi::*;
use crate::route::*;
use crate::share::*;
use crate::spots::*;
use crate::textedit::{edit_line, form_cur};
use crate::tiles::TileLoader;
use crate::ui_helpers::*;
use crate::uistate::UiState;
use crate::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    // 分岐の中身は ui.rs から動かしていないので、フレームの値はもとと同じ名前で受け取る。
    let KeyCtx { a, lat, lon, nogos: route_nogos, ow, oh, .. } = *cx;
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
        Focus::Settings => ui_keys_settings::settings(st, k, cx.nogos, out),
        Focus::SettingsEdit(idx, buf) => ui_keys_settings::settings_edit(st, k, idx, buf),
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
                Some(act) => ui_action::run_action(st, a, act, lat, lon, &route_nogos),
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
        Focus::SettingsPick(idx) => ui_keys_settings::settings_pick(st, k, idx, cx.loader),
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
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::Cache;
    use crate::uistate::testing::*;

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
