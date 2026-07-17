// 単一行テキスト入力欄の共通編集ロジック。ui.rs から機械的に切り出したもの(挙動は不変)。
// Search/SaveName/NearSearch/NewCat/RoadSearch/Recommend/SpotRename/SpotEditName/
// SettingsEdit/SpotForm/PoiKindForm など、全テキスト系 Focus から呼ばれる。

use crate::*;

// ---- テキスト1行編集ヘルパ(全テキスト入力欄で共有) ----
// cur は「文字単位」のカーソル位置(0..=文字数)。byte offset は char_indices で都度求めるのでマルチバイト安全。

// 文字位置 char_idx の byte offset を返す(末尾なら文字列長)。
pub(crate) fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

// cur 位置に文字列 s を挿入し、cur を挿入文字数ぶん進める(ペースト用)。
pub(crate) fn insert_str_at(buf: &mut String, cur: &mut usize, s: &str) {
    let at = char_byte(buf, *cur);
    buf.insert_str(at, s);
    *cur += s.chars().count();
}

// SpotForm のフィールド切替時、移動先フィールドのバッファ文字数(末尾)を返す。ボタン欄は0。
pub(crate) fn form_cur(name: &str, url: &str, field: usize) -> usize {
    match field { 0 => name.chars().count(), 1 => url.chars().count(), _ => 0 }
}

// 1行入力の編集。対象キー(←→ Home/End 文字入力 Backspace Delete)を処理したら true、非対象は false。
pub(crate) fn edit_line(buf: &mut String, cur: &mut usize, code: crossterm::event::KeyCode) -> bool {
    use crossterm::event::KeyCode;
    let n = buf.chars().count();
    if *cur > n { *cur = n; } // 念のため範囲に丸める
    match code {
        KeyCode::Left  => { *cur = cur.saturating_sub(1); true }
        KeyCode::Right => { *cur = (*cur + 1).min(n); true }
        KeyCode::Home  => { *cur = 0; true }
        KeyCode::End   => { *cur = n; true }
        KeyCode::Char(c) => { let at = char_byte(buf, *cur); buf.insert(at, c); *cur += 1; true } // cur の文字位置に挿入
        KeyCode::Backspace => {
            if *cur > 0 { // cur-1 の1文字を削除
                let s = char_byte(buf, *cur - 1);
                let e = char_byte(buf, *cur);
                buf.replace_range(s..e, "");
                *cur -= 1;
            }
            true
        }
        KeyCode::Delete => {
            if *cur < n { // cur 位置の1文字を削除(cur据え置き)
                let s = char_byte(buf, *cur);
                let e = char_byte(buf, *cur + 1);
                buf.replace_range(s..e, "");
            }
            true
        }
        _ => false,
    }
}

// cur 位置にブロックカーソル █ を挟んで表示(末尾なら末尾に付く)。
// ANSI を含めない(表示は fit_cells が幅計算するため、エスケープを入れると桁がずれる)。
pub(crate) fn render_with_cursor(buf: &str, cur: usize) -> String {
    let chars: Vec<char> = buf.chars().collect();
    let cur = cur.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let after: String = chars[cur..].iter().collect();
    format!("{before}\u{2588}{after}")
}

// 単一テキスト欄の中央入力パネル(底面バーでなく地図中央に重畳。SpotFormと同じ手法)。
// title=見出し / hint=下部の操作説明 / buf=入力中の文字列 / cur=カーソル文字位置。
pub(crate) fn draw_input_panel<W: std::io::Write>(out: &mut W, cols: u32, map_rows: u32, title: &str, hint: &str, buf: &str, cur: usize) {
    const BG: &str = "\x1b[30;47m";  // 黒字・白地
    const RST: &str = "\x1b[0m";
    let iw = (cols as usize).saturating_sub(6).clamp(24, 64); // ボックス内容幅
    let input_line = format!("  ▸ {}", render_with_cursor(buf, cur));
    let blank = " ".repeat(iw);
    let rows: [String; 6] = [
        blank.clone(),
        fit_cells(&format!("  {title}"), iw),
        blank.clone(),
        fit_cells(&input_line, iw),
        blank.clone(),
        fit_cells(&format!("  {hint}"), iw),
    ];
    let r0 = ((map_rows as usize).saturating_sub(rows.len() + 1) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    for (i, line) in rows.iter().enumerate() {
        let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, BG, line, RST);
    }
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + rows.len() as u32, c0, BG, blank, RST);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    // 文字位置→byte offset(マルチバイト含む)
    #[test]
    fn char_byte_multibyte() {
        assert_eq!(char_byte("abc", 0), 0);
        assert_eq!(char_byte("abc", 2), 2);
        assert_eq!(char_byte("abc", 3), 3);   // 末尾
        assert_eq!(char_byte("あい", 0), 0);
        assert_eq!(char_byte("あい", 1), 3);  // 'あ'=3byte
        assert_eq!(char_byte("あい", 2), 6);  // 末尾
        assert_eq!(char_byte("あい", 9), 6);  // 範囲外は末尾扱い
    }

    // 途中挿入(ASCII)
    #[test]
    fn edit_insert_middle_ascii() {
        let mut b = "ac".to_string();
        let mut c = 1;
        assert!(edit_line(&mut b, &mut c, KeyCode::Char('b')));
        assert_eq!(b, "abc");
        assert_eq!(c, 2);
    }

    // 途中挿入(マルチバイト)。byte offset ずれで壊れないこと
    #[test]
    fn edit_insert_middle_multibyte() {
        let mut b = "あう".to_string();
        let mut c = 1; // 'あ'の後ろ
        assert!(edit_line(&mut b, &mut c, KeyCode::Char('い')));
        assert_eq!(b, "あいう");
        assert_eq!(c, 2);
    }

    // 左右移動とクランプ
    #[test]
    fn edit_left_right_clamp() {
        let mut b = "abc".to_string();
        let mut c = 0;
        assert!(edit_line(&mut b, &mut c, KeyCode::Left)); // 0で止まる
        assert_eq!(c, 0);
        edit_line(&mut b, &mut c, KeyCode::Right);
        edit_line(&mut b, &mut c, KeyCode::Right);
        edit_line(&mut b, &mut c, KeyCode::Right);
        edit_line(&mut b, &mut c, KeyCode::Right); // 文字数3で止まる
        assert_eq!(c, 3);
    }

    // Home/End
    #[test]
    fn edit_home_end() {
        let mut b = "あいう".to_string();
        let mut c = 1;
        assert!(edit_line(&mut b, &mut c, KeyCode::End));
        assert_eq!(c, 3);
        assert!(edit_line(&mut b, &mut c, KeyCode::Home));
        assert_eq!(c, 0);
    }

    // Backspace は cur-1 の文字を消す(マルチバイト)
    #[test]
    fn edit_backspace_multibyte() {
        let mut b = "あいう".to_string();
        let mut c = 2; // 'い'の後ろ
        assert!(edit_line(&mut b, &mut c, KeyCode::Backspace));
        assert_eq!(b, "あう");
        assert_eq!(c, 1);
        // cur=0 では何もしない
        let mut c0 = 0;
        let mut b0 = "x".to_string();
        edit_line(&mut b0, &mut c0, KeyCode::Backspace);
        assert_eq!(b0, "x");
        assert_eq!(c0, 0);
    }

    // Delete は cur 位置の文字を消す(cur据え置き)
    #[test]
    fn edit_delete_multibyte() {
        let mut b = "あいう".to_string();
        let mut c = 1; // 'い'を消す
        assert!(edit_line(&mut b, &mut c, KeyCode::Delete));
        assert_eq!(b, "あう");
        assert_eq!(c, 1);
        // 末尾では何もしない
        let mut cend = 2;
        edit_line(&mut b, &mut cend, KeyCode::Delete);
        assert_eq!(b, "あう");
    }

    // 非対象キーは false
    #[test]
    fn edit_ignores_other_keys() {
        let mut b = "ab".to_string();
        let mut c = 1;
        assert!(!edit_line(&mut b, &mut c, KeyCode::Enter));
        assert!(!edit_line(&mut b, &mut c, KeyCode::Tab));
        assert!(!edit_line(&mut b, &mut c, KeyCode::Up));
        assert_eq!(b, "ab"); // 変化なし
        assert_eq!(c, 1);
    }

    // ペースト挿入
    #[test]
    fn insert_str_at_middle() {
        let mut b = "あZ".to_string();
        let mut c = 1;
        insert_str_at(&mut b, &mut c, "XY");
        assert_eq!(b, "あXYZ");
        assert_eq!(c, 3);
    }

    // 表示: cur 位置にブロック █
    #[test]
    fn render_cursor_positions() {
        assert_eq!(render_with_cursor("abc", 0), "\u{2588}abc");
        assert_eq!(render_with_cursor("abc", 1), "a\u{2588}bc");
        assert_eq!(render_with_cursor("abc", 3), "abc\u{2588}"); // 末尾
        assert_eq!(render_with_cursor("あい", 1), "あ\u{2588}い");
        assert_eq!(render_with_cursor("ab", 9), "ab\u{2588}"); // 範囲外は末尾
    }

    // SpotForm フィールド切替時のカーソル位置
    #[test]
    fn form_cur_by_field() {
        assert_eq!(form_cur("あい", "http://x", 0), 2); // 名称の文字数
        assert_eq!(form_cur("あい", "http://x", 1), 8); // URLの文字数
        assert_eq!(form_cur("あい", "http://x", 2), 0); // ボタン欄
        assert_eq!(form_cur("あい", "http://x", 3), 0);
    }
}
