// 地図の上に重ねる中央パネル/ポップアップ類と、地図下の標高プロファイル帯の描画。
// いずれも「状態を読んで端末へ書く」だけで、ui.rs のループ状態を書き換えない部分をここへ集約した。

use crate::*;
use crate::render::*;
use crate::spots::SPOT_PALETTE;
use crate::textedit::{draw_input_panel, render_with_cursor};
use crate::focus::Focus;
use image::RgbImage;
use std::io::Write;

// 中央に終了確認(y=終了/他=取消)
pub(crate) fn draw_quit_confirm<W: Write>(out: &mut W, cols: u32, map_rows: u32) {
    let text = "  termmapを終了しますか？ (y/n)  ";
    let w = text.chars().count();
    let c0 = ((cols as usize).saturating_sub(w) / 2).max(1);
    let r0 = (map_rows / 2).max(1);
    let pad = " ".repeat(w);
    let _ = write!(out, "\x1b[{};{}H\x1b[30;43m{}\x1b[0m", r0, c0, pad);
    let _ = write!(out, "\x1b[{};{}H\x1b[30;43m{}\x1b[0m", r0 + 1, c0, text);
    let _ = write!(out, "\x1b[{};{}H\x1b[30;43m{}\x1b[0m", r0 + 2, c0, pad);
}

// 中央に名前ポップアップ(任意キーで閉じる)
pub(crate) fn draw_popup<W: Write>(out: &mut W, cols: u32, map_rows: u32, msg: &str) {
    let text = format!("  {}  ", msg);
    let w = text.chars().count();
    let c0 = ((cols as usize).saturating_sub(w) / 2).max(1);
    let r0 = (map_rows / 2).max(1);
    let pad = " ".repeat(w);
    let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0, c0, pad);
    let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0 + 1, c0, text);
    let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0 + 2, c0, pad);
}

// QR共有ポップアップ(地図の上に白地で重ねる。白地×黒でどのテーマでもスキャン可。
// QRだけで用途は自明なため案内ラベルは出さない)。
pub(crate) fn draw_qr_text<W: Write>(out: &mut W, cols: u32, map_rows: u32, tr: u16, q: &str) {
    let lines: Vec<&str> = q.lines().collect();
    let qw = lines.iter().map(|l| l.chars().count()).max().unwrap_or(21);
    let padx = 2usize; // 左右の白余白(quiet zone)
    let bw = qw + padx * 2;
    let c0 = ((cols as usize).saturating_sub(bw) / 2).max(1) as u32;
    // 行構成: 上白余白×2 / QR / 下白余白×2
    let total = lines.len() + 4;
    let r0 = ((map_rows as usize).saturating_sub(total) / 2).max(1) as u32;
    let hpad = " ".repeat(bw);
    let side = " ".repeat(padx);
    // 純白の箱(bright white 107 + black 30)。上下2行の白余白でquiet zone確保
    for k in 0..2 { let _ = write!(out, "\x1b[{};{c0}H\x1b[30;107m{hpad}\x1b[0m", r0 + k); }
    for (i, l) in lines.iter().enumerate() {
        let _ = write!(out, "\x1b[{};{c0}H\x1b[30;107m{side}{l:<qw$}{side}\x1b[0m", r0 + 2 + i as u32, qw = qw);
    }
    for k in 0..2 { let _ = write!(out, "\x1b[{};{c0}H\x1b[30;107m{hpad}\x1b[0m", r0 + 2 + lines.len() as u32 + k); }
    let _ = write!(out, "\x1b[{};1H\x1b[7m 任意のキーで閉じる \x1b[0m\x1b[K", tr);
}

// QR共有ポップアップ(画像モード): インライン画像は文字セル密度の制約を受けないため、
// モジュール数(=QRの複雑さ)に関係なく常に一定の小さいセル数で表示できる。
pub(crate) fn draw_qr_image<W: Write>(out: &mut W, cols: u32, map_rows: u32, tr: u16, img: &RgbImage) {
    let cell_w: u32 = 20; // 端末フォントの縦横比(概ね横1:縦2)を踏まえ、正方形に見えるよう縦の2倍を確保
    let cell_h: u32 = 10;
    let c0 = ((cols as usize).saturating_sub(cell_w as usize) / 2).max(1) as u32;
    let r0 = ((map_rows as usize).saturating_sub(cell_h as usize) / 2).max(1) as u32;
    let _ = write!(out, "\x1b[{};{c0}H", r0);
    let _ = emit_iterm2_image(out, img, cell_w, cell_h);
    let _ = write!(out, "\x1b[{};1H\x1b[7m 任意のキーで閉じる \x1b[0m\x1b[K", tr);
}

// 新規スポット登録フォーム(中央ボックス。qr_view/popup と同じ中央重畳手法)
pub(crate) fn draw_spot_form<W: Write>(out: &mut W, cols: u32, map_rows: u32, name: &str, url: &str, field: usize, input_cur: usize, cur_cat: &str) {
    const BG: &str = "\x1b[30;47m";   // 黒字・白地(ボックス地)
    const SEL: &str = "\x1b[97;40m";  // 白字・黒地(選択中フィールドを反転表示)
    const RST: &str = "\x1b[0m";
    let iw = (cols as usize).saturating_sub(6).clamp(24, 60); // ボックス内容幅
    // 選択中の入力欄は cur 位置にカーソルを出す。非選択欄はそのまま表示。
    let name_disp = if field == 0 { render_with_cursor(name, input_cur) } else { name.to_string() };
    let url_disp = if field == 1 { render_with_cursor(url, input_cur) } else { url.to_string() };
    let header = format!("  新規スポット [{cur_cat}]");
    let name_line = format!("  名称: {}", name_disp);
    let url_line = format!("  GoogleマップURL(任意): {}", url_disp);
    let blank = " ".repeat(iw);
    // 行の並び(内容, その行が選択中フィールドか)
    let rows: [(String, bool); 6] = [
        (blank.clone(), false),
        (fit_cells(&header, iw), false),
        (fit_cells(&name_line, iw), field == 0),
        (fit_cells(&url_line, iw), field == 1),
        (blank.clone(), false),
        (blank.clone(), false),
    ];
    // ボタン行([送信]/[戻る] を明示セグメントで組む。各6セル+前後余白)
    let mut btn = String::new();
    btn.push_str(BG); btn.push_str("  ");
    btn.push_str(if field == 2 { SEL } else { BG }); btn.push_str("[送信]");
    btn.push_str(BG); btn.push_str("  ");
    btn.push_str(if field == 3 { SEL } else { BG }); btn.push_str("[戻る]");
    btn.push_str(BG);
    btn.push_str(&" ".repeat(iw.saturating_sub(2 + 6 + 2 + 6)));
    btn.push_str(RST);
    let total = rows.len() + 2; // + ボタン行 + 下余白
    let r0 = ((map_rows as usize).saturating_sub(total) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    for (i, (line, sel)) in rows.iter().enumerate() {
        let style = if *sel { SEL } else { BG };
        let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, style, line, RST);
    }
    let _ = write!(out, "\x1b[{};{}H{}", r0 + rows.len() as u32, c0, btn);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + rows.len() as u32 + 1, c0, BG, blank, RST);
}

// 目的地カテゴリの新規追加フォーム
pub(crate) fn draw_poi_kind_form<W: Write>(out: &mut W, cols: u32, map_rows: u32, label: &str, tag: &str, field: usize, input_cur: usize) {
    const BG: &str = "\x1b[30;47m";
    const SEL: &str = "\x1b[97;40m";
    const RST: &str = "\x1b[0m";
    let iw = (cols as usize).saturating_sub(6).clamp(24, 60);
    let label_disp = if field == 0 { render_with_cursor(label, input_cur) } else { label.to_string() };
    let tag_disp = if field == 1 { render_with_cursor(tag, input_cur) } else { tag.to_string() };
    let header = "  新しい目的地カテゴリ";
    let label_line = format!("  表示名: {}", label_disp);
    let tag_line = format!("  OSMタグ(key=value 例 shop=bakery): {}", tag_disp);
    let blank = " ".repeat(iw);
    let rows: [(String, bool); 6] = [
        (blank.clone(), false),
        (fit_cells(header, iw), false),
        (fit_cells(&label_line, iw), field == 0),
        (fit_cells(&tag_line, iw), field == 1),
        (blank.clone(), false),
        (blank.clone(), false),
    ];
    let mut btn = String::new();
    btn.push_str(BG); btn.push_str("  ");
    btn.push_str(if field == 2 { SEL } else { BG }); btn.push_str("[追加]");
    btn.push_str(BG); btn.push_str("  ");
    btn.push_str(if field == 3 { SEL } else { BG }); btn.push_str("[戻る]");
    btn.push_str(BG);
    btn.push_str(&" ".repeat(iw.saturating_sub(2 + 6 + 2 + 6)));
    btn.push_str(RST);
    let total = rows.len() + 2;
    let r0 = ((map_rows as usize).saturating_sub(total) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    for (i, (line, sel)) in rows.iter().enumerate() {
        let style = if *sel { SEL } else { BG };
        let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, style, line, RST);
    }
    let _ = write!(out, "\x1b[{};{}H{}", r0 + rows.len() as u32, c0, btn);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + rows.len() as u32 + 1, c0, BG, blank, RST);
}

// おまかせ周回: 距離をゲージで選ぶ
pub(crate) fn draw_wander_form<W: Write>(out: &mut W, cols: u32, map_rows: u32, dist_km: f64) {
    const BG: &str = "\x1b[30;47m";
    const FILL: &str = "\x1b[42;30m";  // 緑地(埋まった部分)
    const RST: &str = "\x1b[0m";
    let iw = (cols as usize).saturating_sub(6).clamp(24, 60);
    let gw = iw.saturating_sub(4).max(10); // ゲージ本体の幅(セル。█/░は等幅1セルなのでfit_cells不要)
    let (lo, hi) = (10.0, 200.0);
    let frac = ((dist_km - lo) / (hi - lo)).clamp(0.0, 1.0);
    let filled = ((gw as f64 * frac).round() as usize).min(gw);
    let header = "  おまかせ周回: 距離を選択";
    let dist_line = format!("  {:.0}km  (←→=5km Shift=20km  範囲{:.0}〜{:.0}km)", dist_km, lo, hi);
    let blank = " ".repeat(iw);
    let rows: [String; 6] = [
        blank.clone(),
        fit_cells(header, iw),
        blank.clone(), // ゲージ本体はこの行にループ後で個別に上書き
        fit_cells(&dist_line, iw),
        blank.clone(),
        fit_cells("  Enter=検索開始(バックグラウンド)  Esc=取消", iw),
    ];
    let r0 = ((map_rows as usize).saturating_sub(rows.len() + 1) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    for (i, line) in rows.iter().enumerate() {
        let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, BG, line, RST);
    }
    // ゲージ本体(行index2)を色付きで上書き。前後の余白は地の色(BG)のまま。
    let gauge_row = r0 + 2;
    let _ = write!(out, "\x1b[{};{}H{}  {}{}{}{}{}", gauge_row, c0, BG,
        FILL, "█".repeat(filled), BG, "░".repeat(gw.saturating_sub(filled)), RST);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + rows.len() as u32, c0, BG, blank, RST);
}

// 単一テキスト入力は地図中央のフォームで受ける(底面バーで完結させない)
pub(crate) fn draw_text_input<W: Write>(out: &mut W, cols: u32, map_rows: u32, focus: &Focus, input_cur: usize) {
    match focus {
        Focus::Search(b) => draw_input_panel(out, cols, map_rows, "地名・住所で検索", "Enter=検索  Esc=取消  (住所も入力OK)", b, input_cur),
        Focus::SaveName(b) => draw_input_panel(out, cols, map_rows, "ルートに名前を付けて保存", "Enter=保存  Esc=取消", b, input_cur),
        Focus::NearSearch(b) => draw_input_panel(out, cols, map_rows, "このあたりでキーワード検索", "Enter=検索  Esc=取消", b, input_cur),
        Focus::NewCat(b) => draw_input_panel(out, cols, map_rows, "新しいカテゴリ名", "Enter=作成  Esc=取消", b, input_cur),
        Focus::RoadSearch(b) => draw_input_panel(out, cols, map_rows, "道路名・国道番号でルートに追加", "Enter=view内を追加(複数可)  Esc=取消", b, input_cur),
        Focus::Recommend(b) => draw_input_panel(out, cols, map_rows, "おすすめの方向性 (例: 海沿い / 峠)", "Enter=提案(数秒)  Esc=取消", b, input_cur),
        Focus::SpotRename(b, _) => draw_input_panel(out, cols, map_rows, "カテゴリ名を変更", "Enter=確定  Esc=取消", b, input_cur),
        Focus::SpotEditName(b, _) => draw_input_panel(out, cols, map_rows, "スポット名を変更", "Enter=確定  Esc=取消", b, input_cur),
        Focus::SettingsEdit(idx, b) => {
            let (title, hint) = if *idx == 6 { ("道路の点間隔(m)", "数字のみ・100〜5000にクランプ  Enter=確定(自動保存)  Esc=取消") }
                else { ("Google APIキー", "印字可能ASCIIのみ(制御文字/改行不可)  Enter=確定(自動保存)  Esc=取消") };
            draw_input_panel(out, cols, map_rows, title, hint, b, input_cur);
        }
        _ => {}
    }
}

// 色ピッカー(中央パネル・実色スウォッチ)。選択中は [ ] で囲む
pub(crate) fn draw_color_pick<W: Write>(out: &mut W, cols: u32, map_rows: u32, color_sel: u8) {
    const BG: &str = "\x1b[30;47m";
    const RST: &str = "\x1b[0m";
    let iw = SPOT_PALETTE.len() * 4 + 2; // 各色4セル(枠含む)+左余白2
    let blank = " ".repeat(iw);
    let mut sw = String::from(BG);
    sw.push_str("  ");
    for (i, c) in SPOT_PALETTE.iter().enumerate() {
        let s = i as u8 == color_sel;
        sw.push_str(BG);
        sw.push(if s { '[' } else { ' ' });
        sw.push_str(&format!("\x1b[48;2;{};{};{}m  ", c[0], c[1], c[2]));
        sw.push_str(BG);
        sw.push(if s { ']' } else { ' ' });
    }
    sw.push_str(RST);
    let title = fit_cells("  色を選択", iw);
    let hint = fit_cells("  ←→ 選択   Enter 決定   Esc 取消", iw);
    let r0 = ((map_rows as usize).saturating_sub(6) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0, c0, BG, blank, RST);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + 1, c0, BG, title, RST);
    let _ = write!(out, "\x1b[{};{}H{}", r0 + 2, c0, sw);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + 3, c0, BG, blank, RST);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + 4, c0, BG, hint, RST);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + 5, c0, BG, blank, RST);
}

// 形状ピッカー(色とは独立に形を選ぶ)。選択中は [ ] で囲む
pub(crate) fn draw_shape_pick<W: Write>(out: &mut W, cols: u32, map_rows: u32, shape_sel: u8) {
    const BG: &str = "\x1b[30;47m";
    const RST: &str = "\x1b[0m";
    // 形状index順のグリフ(0四角 1三角 2丸 3菱形 4十字 5星)。描画実体は render の marker_inside。
    const GLYPHS: [&str; NUM_MARKER_SHAPES as usize] = ["■", "▲", "●", "◆", "＋", "✦"];
    let iw = NUM_MARKER_SHAPES as usize * 4 + 2; // 各形4セル(枠含む)+左余白2
    let blank = " ".repeat(iw);
    let mut sw = String::from(BG);
    sw.push_str("  ");
    for (i, g) in GLYPHS.iter().enumerate() {
        let s = i as u8 == shape_sel;
        sw.push(if s { '[' } else { ' ' });
        sw.push_str(g);
        sw.push(if s { ']' } else { ' ' });
    }
    sw.push_str(RST);
    let title = fit_cells("  形を選択", iw);
    let hint = fit_cells("  ←→ 選択   Enter 決定   Esc 取消", iw);
    let r0 = ((map_rows as usize).saturating_sub(6) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0, c0, BG, blank, RST);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + 1, c0, BG, title, RST);
    let _ = write!(out, "\x1b[{};{}H{}", r0 + 2, c0, sw);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + 3, c0, BG, blank, RST);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + 4, c0, BG, hint, RST);
    let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + 5, c0, BG, blank, RST);
}

// 過去災害の事例一覧(中央パネル・Bキーで開く。何かキーで消える)。
// draw_popup が1行専用なので、draw_onboarding と同じ組み方の複数行パネルとして別に持つ。
// 地点は市区町村の代表点なので「災害が起きた場所そのもの」ではない。それを取り違えられないよう、
// 出典と一緒に「市区町村単位の記録」であることを常時1行で出す。
pub(crate) fn draw_disaster_panel<W: Write>(
    out: &mut W,
    cols: u32,
    map_rows: u32,
    title: &str,
    lines: &[String],
    truncated: bool,
) {
    const BG: &str = "\x1b[30;47m";
    const RST: &str = "\x1b[0m";
    let iw = (cols as usize).saturating_sub(6).clamp(24, 96);
    // 枠(空行・見出し・区切り2本・脚注・操作案内)を除いた残りが本文に使える行数。
    const CHROME_ROWS: usize = 9;
    let max_body = (map_rows as usize).saturating_sub(CHROME_ROWS).max(1);
    let shown = lines.len().min(max_body);
    let rule = format!(" {}", "─".repeat(iw.saturating_sub(2)));
    let mut rows: Vec<String> = vec![String::new(), format!(" {title}"), rule.clone()];
    for l in lines.iter().take(shown) {
        rows.push(format!(" {l}"));
    }
    if lines.len() > shown {
        rows.push(format!(" …ほか{}件(画面に収まらない)", lines.len() - shown));
    }
    rows.push(rule);
    if truncated {
        // 集計が上限で打ち切られると件数が黙って過少になる。黙って過少にしない。
        rows.push(" ※取得上限で打ち切られた集計がある(件数は下限)".to_string());
    }
    rows.push(" 市区町村単位の記録  出典: 防災科学技術研究所 災害事例データベース".to_string());
    rows.push(" 任意のキー(Esc/q)で閉じる".to_string());
    rows.push(String::new());
    let r0 = ((map_rows as usize).saturating_sub(rows.len()) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    for (i, ln) in rows.iter().enumerate() {
        let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, BG, fit_cells(ln, iw), RST);
    }
}

// 初回起動の操作案内(中央パネル・何かキーで消える)
pub(crate) fn draw_onboarding<W: Write>(out: &mut W, cols: u32, map_rows: u32) {
    const RST: &str = "\x1b[0m";
    let iw = 40usize;
    // 緑グラデのワードマーク(端末=Term を意識)。背景は塗らない。(太字か, RGB, 文字)
    // SGRはtruecolor_safe()を見て組み立てる(truecolor不安定端末では256色にフォールバック)。
    let rows: [(Option<(bool, (u8, u8, u8))>, &str); 11] = [
        (None, ""),
        (Some((true, (130, 255, 150))), "   ╺┳╸┏━╸┏━┓┏┳┓┏┳┓┏━┓┏━┓"),
        (Some((true, (80, 220, 110))),  "    ┃ ┣╸ ┣┳┛┃┃┃┃┃┃┣━┫┣━┛"),
        (Some((true, (40, 175, 80))),   "    ╹ ┗━╸╹┗╸╹╹╹╹╹╹╹ ╹╹"),
        (Some((false, (110, 170, 120))), "   terminal touring map"),
        (None, ""),
        (Some((false, (190, 235, 200))), "  Space  メニュー   ?  ヘルプ   q  終了"),
        (None, ""),
        (Some((false, (150, 205, 160))), "  何かキーを押して開始"),
        (Some((false, (110, 150, 120))), "  d = 次回から表示しない (設定で再表示)"),
        (None, ""),
    ];
    let r0 = ((map_rows as usize).saturating_sub(rows.len()) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    let tc_ok = truecolor_safe();
    for (i, (spec, ln)) in rows.iter().enumerate() {
        let col = match spec {
            Some((bold, (r, g, b))) => format!("{}{}", if *bold { "\x1b[1m" } else { "" }, sgr_fg(*r, *g, *b, tc_ok)),
            None => String::new(),
        };
        let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, col, fit_cells(ln, iw), RST);
    }
}

// 標高プロファイル帯(地図の下・ステータスの上)
pub(crate) fn draw_elevation_band<W: Write>(out: &mut W, cols: u32, map_rows: u32, elev_h: u32,
    route_ele: &[f64], route_ascend: f64, spec: &render::OverlaySpec, lat: f64, lon: f64) {
    let (mn, mx, _asc) = elevation::elevation_stats(route_ele);
    let label = fit_cells(&format!(" 標高 ↑{route_ascend:.0}m  最高{mx:.0}m 最低{mn:.0}m  (Eで消す) "), cols as usize);
    let _ = write!(out, "\x1b[{};1H\x1b[7m{label}\x1b[0m\x1b[K", map_rows + 1);
    // 左端に高さの目盛り(6桁分「1234m」+区切り1マス=7列)を出す。最上段=最高/最下段=最低/
    // 中間段(4行以上の時)=中間値(elevation::axis_label、テスト済み)。グラフ本体はその分幅を削る。
    const AXIS_W: usize = 7;
    let chart_w = (cols as usize).saturating_sub(AXIS_W).max(1);
    // ルート点は間隔が不均一(直線区間は疎・複雑な区間は密)なため、単純に点のインデックスで
    // ビン化すると横軸が実距離とズレる(グラフの形も現在地カーソルも)。累積距離で一度
    // chart_w点に均等リサンプルしてからelevation_chartへ渡すことで、その後の単純な
    // インデックスビン化(bin_values)が距離一様として正しく機能するようにする。
    let cum_dist = spec.routes.last().map(|rt| elevation::cumulative_distances(&rt.pts));
    let route_ele_by_dist = match &cum_dist {
        Some(cd) if cd.len() == route_ele.len() && !route_ele.is_empty() =>
            elevation::resample_by_distance(route_ele, cd, chart_w),
        _ => route_ele.to_vec(),
    };
    let chart = elevation::elevation_chart(&route_ele_by_dist, chart_w, elev_h as usize);
    for (i, line) in chart.iter().enumerate() {
        let axis = elevation::axis_label(i as u32, elev_h, mn, mx).unwrap_or_else(|| "     ".to_string());
        let _ = write!(out, "\x1b[{};1H\x1b[2m{axis}\x1b[0m {}\x1b[K", map_rows + 2 + i as u32, line);
    }
    // 地図中心が経路上のどこかを示す縦カーソル(パン/再生で動く)。実距離基準で位置を出す
    // (点のインデックス基準だとグラフ側と同じ理由でズレる)。
    if let Some(rt) = spec.routes.last() {
        if rt.pts.len() >= 2 {
            let (mut bi, mut bd) = (0usize, f64::MAX);
            for (i, p) in rt.pts.iter().enumerate() {
                let d = (p.0 - lat).powi(2) + (p.1 - lon).powi(2);
                if d < bd { bd = d; bi = i; }
            }
            let col = match &cum_dist {
                Some(cd) if cd.len() == rt.pts.len() => {
                    let total = *cd.last().unwrap_or(&0.0);
                    elevation::profile_col_by_distance(cd[bi], total, chart_w)
                }
                _ => elevation::profile_col(rt.pts.len(), bi, chart_w),
            };
            for i in 0..elev_h as usize {
                let _ = write!(out, "\x1b[{};{}H\x1b[1;31m|\x1b[0m", map_rows + 2 + i as u32, col + 1 + AXIS_W);
            }
        }
    }
}
