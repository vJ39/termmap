// マイスポット(P キー)のキー処理。ui_keys.rs の Focus 分岐から関心ごとに切り出した1つ。
// 画面は8つ: カテゴリ一覧・スポット一覧・改名(スポット/カテゴリ)・新規カテゴリ・
// 新規スポット登録フォーム・色ピッカー・形状ピッカー。
//
// 引数は「そのフレームの値」のうち各画面が実際に使うものだけを受け取る(何に依存しているかを
// 引数で見えるようにするため)。

use crate::focus::Focus;
use crate::geo::deg_to_pixel;
use crate::render::NUM_MARKER_SHAPES;
use crate::share::parse_gmaps_place;
use crate::spots::*;
use crate::textedit::{edit_line, form_cur};
use crate::uistate::UiState;
use crossterm::event::{KeyCode, KeyEvent};
use std::io::Write;

pub(crate) fn spot_cat_list(st: &mut UiState, k: KeyEvent, out: &mut dyn Write) {
    match k.code { // カテゴリ一覧(P)
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
    }
}

pub(crate) fn spot_list(st: &mut UiState, k: KeyEvent) {
    match k.code { // cur_cat のスポット一覧
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
    }
}

pub(crate) fn spot_edit_name(st: &mut UiState, k: KeyEvent, mut buf: String, gi: usize) {
    match k.code { // スポット改名
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
    }
}

pub(crate) fn new_cat(st: &mut UiState, k: KeyEvent, mut buf: String) {
    match k.code {
        KeyCode::Enter => { let name = buf.trim().to_string(); if !name.is_empty() { st.snd.play("confirm"); let _ = ensure_spot_cat(&name, &mut st.spot_cats); } st.focus = Focus::SpotCatList; }
        KeyCode::Esc => { st.snd.play("back"); st.focus = Focus::SpotCatList; }
        other => { edit_line(&mut buf, &mut st.input_cur, other); st.focus = Focus::NewCat(buf); }
    }
}

pub(crate) fn spot_rename(st: &mut UiState, k: KeyEvent, mut buf: String, idx: usize) {
    match k.code {
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
    }
}

pub(crate) fn spot_form(st: &mut UiState, k: KeyEvent, mut name: String, mut url: String, mut field: usize, lat: f64, lon: f64) {
    match k.code { // 新規スポット登録フォーム
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
    }
}

// 色ピッカー: ←→でパレット選択、Enterで確定
pub(crate) fn color_pick(st: &mut UiState, k: KeyEvent, cat: usize) {
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

pub(crate) fn shape_pick(st: &mut UiState, k: KeyEvent, cat: usize) { // 形状ピッカー(色とは独立に形を選ぶ)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uistate::testing::*;
    use crossterm::event::KeyModifiers;

    fn ch(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }
    fn code(c: KeyCode) -> KeyEvent { KeyEvent::new(c, KeyModifiers::NONE) }

    // ui_keys::dispatch は focus を Map へ倒してから呼ぶので、テストも同じ前提で始める
    // (「画面を出したままにする」分岐だけが focus を書き戻す)。
    // ★保存を伴う分岐($HOME/.config/termmap/spots.txt・spot-categories.txt を上書きする
    //   [ ] x Enter など)はテストから触らない。ここで確かめるのは選択・画面遷移・入力の扱い。
    fn base() -> UiState {
        let mut st = test_state();
        st.focus = Focus::Map;
        st.spot_cats = vec![("温泉".to_string(), 3, 2), ("峠".to_string(), 5, 1)];
        st.cur_cat = "温泉".to_string();
        // 意図的にカテゴリ順と並びをずらしておく(一覧は cur_cat で絞るので、
        // 全体の添字と一覧上の位置がずれることを確かめたい)。
        st.spots = vec![
            Spot { lat: 36.0, lon: 138.0, cat: "峠".into(), name: "碓氷".into() },
            Spot { lat: 35.1, lon: 139.1, cat: "温泉".into(), name: "箱根".into() },
            Spot { lat: 34.2, lon: 137.2, cat: "温泉".into(), name: "下呂".into() },
        ];
        st
    }

    #[test]
    fn the_category_list_moves_the_cursor_and_stops_at_both_ends() {
        let mut st = base();
        let mut out: Vec<u8> = Vec::new();
        spot_cat_list(&mut st, code(KeyCode::Up), &mut out);
        assert_eq!(st.cat_sel, 0, "先頭より上へは行かない");
        spot_cat_list(&mut st, ch('s'), &mut out);
        assert_eq!(st.cat_sel, 1);
        spot_cat_list(&mut st, ch('s'), &mut out);
        assert_eq!(st.cat_sel, 1, "カテゴリ数を超えない");
        assert!(matches!(st.focus, Focus::SpotCatList), "移動だけなら一覧のまま");

        spot_cat_list(&mut st, ch('Z'), &mut out);
        assert!(matches!(st.focus, Focus::SpotCatList), "知らないキーは無視して一覧のまま");
    }

    #[test]
    fn the_category_list_opens_the_new_rename_color_and_shape_screens() {
        let mut out: Vec<u8> = Vec::new();

        let mut st = base();
        spot_cat_list(&mut st, ch('n'), &mut out);
        assert!(matches!(&st.focus, Focus::NewCat(b) if b.is_empty()));
        assert_eq!(st.input_cur, 0);

        let mut st = base();
        st.cat_sel = 1;
        spot_cat_list(&mut st, ch('r'), &mut out);
        match &st.focus {
            Focus::SpotRename(b, idx) => { assert_eq!(b, "峠"); assert_eq!(*idx, 1); }
            _ => panic!("r は改名画面へ"),
        }
        assert_eq!(st.input_cur, 1, "カーソルは名前の末尾");

        let mut st = base();
        st.cat_sel = 1;
        spot_cat_list(&mut st, ch('c'), &mut out);
        assert!(matches!(st.focus, Focus::ColorPick { cat: 1 }));
        assert_eq!(st.color_sel, 5, "いま保存されている色から始める");

        let mut st = base();
        st.cat_sel = 1;
        spot_cat_list(&mut st, ch('M'), &mut out);
        assert!(matches!(st.focus, Focus::ShapePick { cat: 1 }));
        assert_eq!(st.shape_sel, 1, "いま保存されている形から始める");
    }

    #[test]
    fn enter_on_a_category_opens_its_spot_list() {
        let mut st = base();
        let mut out: Vec<u8> = Vec::new();
        st.cat_sel = 1;
        st.sp_sel = 3;
        spot_cat_list(&mut st, code(KeyCode::Enter), &mut out);
        assert_eq!(st.cur_cat, "峠");
        assert_eq!(st.sp_sel, 0, "一覧の先頭から見せる");
        assert!(matches!(st.focus, Focus::SpotList));
    }

    #[test]
    fn esc_on_the_category_list_drops_the_pending_registration() {
        let mut st = base();
        let mut out: Vec<u8> = Vec::new();
        st.pending_spot = Some((35.0, 139.0, "検索結果".into()));
        spot_cat_list(&mut st, code(KeyCode::Esc), &mut out);
        assert!(st.pending_spot.is_none(), "登録の保留は消す");
        assert!(matches!(st.focus, Focus::Map));
        assert!(String::from_utf8_lossy(&out).contains("\x1b[2J"), "左袖の残像を消す");
        assert!(st.force_reemit);
        assert_eq!(st.spots.len(), 3, "Escでは保存しない");
    }

    #[test]
    fn the_spot_list_counts_only_the_current_category() {
        let mut st = base(); // 温泉=2件
        spot_list(&mut st, ch('s'));
        assert_eq!(st.sp_sel, 1);
        spot_list(&mut st, ch('s'));
        assert_eq!(st.sp_sel, 1, "カテゴリ内の件数を超えない");
        spot_list(&mut st, code(KeyCode::Up));
        assert_eq!(st.sp_sel, 0);
        assert!(matches!(st.focus, Focus::SpotList));
    }

    #[test]
    fn n_on_the_spot_list_opens_an_empty_registration_form() {
        let mut st = base();
        st.input_cur = 7;
        spot_list(&mut st, ch('n'));
        match &st.focus {
            Focus::SpotForm { name, url, field } => { assert!(name.is_empty()); assert!(url.is_empty()); assert_eq!(*field, 0); }
            _ => panic!("n は登録フォームへ"),
        }
        assert_eq!(st.input_cur, 0);
    }

    #[test]
    fn renaming_from_the_spot_list_uses_the_index_in_the_whole_list() {
        let mut st = base();
        st.sp_sel = 1; // 温泉の2件目=全体では添字2
        spot_list(&mut st, ch('r'));
        match &st.focus {
            Focus::SpotEditName(b, gi) => { assert_eq!(b, "下呂"); assert_eq!(*gi, 2); }
            _ => panic!("r は改名画面へ"),
        }
        assert_eq!(st.input_cur, 2);
    }

    #[test]
    fn m_only_asks_before_moving_the_spot() {
        let mut st = base();
        st.sp_sel = 1;
        spot_list(&mut st, ch('m'));
        assert_eq!(st.spot_move_confirm, Some(2), "確認待ちにするだけ");
        assert_eq!(st.spots[2].lat, 34.2, "この時点では動かさない");
        assert!(matches!(st.focus, Focus::SpotList));
    }

    #[test]
    fn enter_on_the_spot_list_centers_the_map_on_it() {
        let mut st = base();
        st.sp_sel = 0; // 温泉の1件目=全体では添字1(箱根)
        spot_list(&mut st, code(KeyCode::Enter));
        let (nx, ny) = deg_to_pixel(35.1, 139.1, st.z);
        assert_eq!((st.cx, st.cy), (nx, ny));
        assert!(matches!(st.focus, Focus::SpotList), "一覧は開いたまま");
    }

    #[test]
    fn esc_walks_back_from_the_spots_to_the_categories() {
        let mut st = base();
        spot_list(&mut st, code(KeyCode::Esc));
        assert!(matches!(st.focus, Focus::SpotCatList));
    }

    #[test]
    fn renaming_a_spot_keeps_editing_and_esc_discards_it() {
        let mut st = base();
        st.input_cur = 0;
        spot_edit_name(&mut st, ch('新'), String::new(), 2);
        match &st.focus {
            Focus::SpotEditName(b, 2) => assert_eq!(b, "新"),
            _ => panic!("入力中は改名画面のまま"),
        }

        let mut st = base();
        spot_edit_name(&mut st, code(KeyCode::Esc), "べつの名前".into(), 2);
        assert!(matches!(st.focus, Focus::SpotList));
        assert_eq!(st.spots[2].name, "下呂", "破棄したので元の名前のまま");
    }

    #[test]
    fn an_empty_category_name_is_not_registered() {
        let mut st = base();
        new_cat(&mut st, code(KeyCode::Enter), "   ".into());
        assert_eq!(st.spot_cats.len(), 2, "空名は作らない");
        assert!(matches!(st.focus, Focus::SpotCatList));

        let mut st = base();
        st.input_cur = 0;
        new_cat(&mut st, ch('滝'), String::new());
        assert!(matches!(&st.focus, Focus::NewCat(b) if b == "滝"), "入力中は入力画面のまま");
    }

    #[test]
    fn an_empty_rename_leaves_the_category_untouched() {
        let mut st = base();
        spot_rename(&mut st, code(KeyCode::Enter), "  ".into(), 0);
        assert_eq!(st.spot_cats[0].0, "温泉", "空名では書き換えない");
        assert!(matches!(st.focus, Focus::SpotCatList));

        let mut st = base();
        spot_rename(&mut st, code(KeyCode::Esc), "湯".into(), 0);
        assert_eq!(st.spot_cats[0].0, "温泉", "破棄したので元の名前のまま");
        assert!(matches!(st.focus, Focus::SpotCatList));
    }

    #[test]
    fn the_form_cycles_four_fields_and_places_the_cursor() {
        let mut st = base();
        spot_form(&mut st, code(KeyCode::Tab), "名".into(), "URL".into(), 0, 35.0, 139.0);
        match &st.focus {
            Focus::SpotForm { field, .. } => assert_eq!(*field, 1),
            _ => panic!("Tabはフォームのまま"),
        }
        assert_eq!(st.input_cur, 3, "URL欄へ移ったらその末尾");

        let mut st = base();
        spot_form(&mut st, code(KeyCode::BackTab), "名".into(), "URL".into(), 0, 35.0, 139.0);
        match &st.focus {
            Focus::SpotForm { field, .. } => assert_eq!(*field, 3, "先頭で戻ると最後のボタンへ"),
            _ => panic!("BackTabはフォームのまま"),
        }
    }

    #[test]
    fn enter_walks_the_form_and_the_back_button_closes_it() {
        let mut st = base();
        spot_form(&mut st, code(KeyCode::Enter), "名".into(), "URL".into(), 0, 35.0, 139.0);
        assert!(matches!(&st.focus, Focus::SpotForm { field: 1, .. }));
        assert_eq!(st.input_cur, 3);

        let mut st = base();
        spot_form(&mut st, code(KeyCode::Enter), "名".into(), "URL".into(), 1, 35.0, 139.0);
        assert!(matches!(&st.focus, Focus::SpotForm { field: 2, .. }));
        assert_eq!(st.input_cur, 0, "ボタン欄はカーソルを持たない");

        let mut st = base();
        spot_form(&mut st, code(KeyCode::Enter), "名".into(), "URL".into(), 3, 35.0, 139.0);
        assert!(matches!(st.focus, Focus::SpotList), "[戻る]で一覧へ");

        let mut st = base();
        spot_form(&mut st, code(KeyCode::Esc), "名".into(), "URL".into(), 0, 35.0, 139.0);
        assert!(matches!(st.focus, Focus::SpotList), "Escで取消");
    }

    #[test]
    fn submitting_an_empty_form_changes_nothing() {
        let mut st = base();
        st.addr = "そのまま".into();
        spot_form(&mut st, code(KeyCode::Enter), String::new(), String::new(), 2, 35.0, 139.0);
        assert!(matches!(&st.focus, Focus::SpotForm { field: 2, .. }), "フォームは開いたまま");
        assert_eq!(st.addr, "そのまま", "何も言わない");
        assert_eq!(st.spots.len(), 3, "保存しない");
    }

    #[test]
    fn a_url_that_has_no_position_is_refused_with_a_reason() {
        let mut st = base();
        spot_form(&mut st, code(KeyCode::Enter), String::new(), "https://maps.app.goo.gl/abc".into(), 2, 35.0, 139.0);
        assert!(st.addr.contains("短縮URL"), "短縮URLはその旨を出す: {}", st.addr);
        assert!(matches!(&st.focus, Focus::SpotForm { field: 2, .. }));
        assert_eq!(st.spots.len(), 3);

        let mut st = base();
        spot_form(&mut st, code(KeyCode::Enter), String::new(), "https://example.com/".into(), 2, 35.0, 139.0);
        assert!(st.addr.contains("位置を取得できません"), "座標が読めない旨を出す: {}", st.addr);
        assert_eq!(st.spots.len(), 3);
    }

    #[test]
    fn typing_in_the_form_edits_only_the_selected_field() {
        let mut st = base();
        st.input_cur = 0;
        spot_form(&mut st, ch('A'), "名".into(), String::new(), 1, 35.0, 139.0);
        match &st.focus {
            Focus::SpotForm { name, url, field } => { assert_eq!(name, "名"); assert_eq!(url, "A"); assert_eq!(*field, 1); }
            _ => panic!("入力中はフォームのまま"),
        }

        let mut st = base();
        st.input_cur = 0;
        spot_form(&mut st, ch('B'), String::new(), "URL".into(), 2, 35.0, 139.0);
        match &st.focus {
            Focus::SpotForm { name, url, .. } => { assert!(name.is_empty(), "ボタン欄では文字を入れない"); assert_eq!(url, "URL"); }
            _ => panic!("入力中はフォームのまま"),
        }
    }

    #[test]
    fn the_color_picker_wraps_and_esc_keeps_the_saved_color() {
        let mut st = base();
        st.color_sel = 0;
        color_pick(&mut st, code(KeyCode::Left), 0);
        assert_eq!(st.color_sel as usize, SPOT_PALETTE.len() - 1, "先頭で←は末尾へ回り込む");
        color_pick(&mut st, code(KeyCode::Right), 0);
        assert_eq!(st.color_sel, 0, "末尾で→は先頭へ戻る");
        assert!(matches!(st.focus, Focus::ColorPick { cat: 0 }));

        color_pick(&mut st, code(KeyCode::Esc), 0);
        assert!(matches!(st.focus, Focus::SpotCatList));
        assert_eq!(st.spot_cats[0].1, 3, "確定していないので色は変えない");
    }

    #[test]
    fn the_shape_picker_wraps_and_esc_keeps_the_saved_shape() {
        let mut st = base();
        st.shape_sel = 0;
        shape_pick(&mut st, code(KeyCode::Left), 0);
        assert_eq!(st.shape_sel, NUM_MARKER_SHAPES - 1, "先頭で←は末尾へ回り込む");
        shape_pick(&mut st, code(KeyCode::Right), 0);
        assert_eq!(st.shape_sel, 0);
        assert!(matches!(st.focus, Focus::ShapePick { cat: 0 }));

        shape_pick(&mut st, code(KeyCode::Esc), 0);
        assert!(matches!(st.focus, Focus::SpotCatList));
        assert_eq!(st.spot_cats[0].2, 2, "確定していないので形は変えない");
    }
}
