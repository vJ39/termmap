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
    // 形状index順のグリフ(0四角 1三角 2丸 3菱形 4十字 5星 6✕)。描画実体は render の marker_inside。
    const GLYPHS: [&str; NUM_MARKER_SHAPES as usize] = ["■", "▲", "●", "◆", "＋", "✦", "✕"];
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

fn disp_width(s: &str) -> usize { unicode_width::UnicodeWidthStr::width(s) }

// 凡例1項目のプレーン表示幅を測るための文字列(色は含まない)。実際の描画は legend_row_colored が
// 色を被せて組み立てる(fit_cells は SGR エスケープを解さず幅計算が壊れるため、幅の計算だけは
// 常にこちらのプレーン版で行う)。
fn legend_item_plain(kind: disaster::DisasterKind) -> String {
    format!("■{}", kind.label())
}

// 過去災害の色凡例(6種)を iw 幅に収まる行へ貪欲に折り返す。1行に入り切らない幅でも、
// 1項目だけの行にはする(空行は作らない)。
fn wrap_legend(iw: usize) -> Vec<Vec<disaster::DisasterKind>> {
    let mut rows: Vec<Vec<disaster::DisasterKind>> = vec![Vec::new()];
    let mut w = 0usize;
    for kind in disaster::DisasterKind::legend_kinds() {
        let item_w = disp_width(&legend_item_plain(kind));
        let need = if w == 0 { item_w } else { w + 1 + item_w };
        if need > iw && w > 0 {
            rows.push(Vec::new());
            w = 0;
        }
        let row = rows.last_mut().expect("直前に必ず1行積んである");
        if !row.is_empty() { w += 1; }
        row.push(kind);
        w += item_w;
    }
    rows
}

// 凡例1行ぶんの色付き文字列。パネルの背景(黒文字/白背景)を保つため、色を変えたあとは
// "\x1b[0m" ではなく "\x1b[30m"(黒文字のみ)へ戻す(0mだと背景色も消えてしまう)。
// 幅の計算は legend_item_plain のプレーン版を使い、iw ちょうどまで空白でパディングする。
fn legend_row_colored(kinds: &[disaster::DisasterKind], iw: usize, truecolor: bool) -> String {
    let mut s = String::new();
    let mut w = 0usize;
    for (i, kind) in kinds.iter().enumerate() {
        if i > 0 { s.push(' '); w += 1; }
        let [r, g, b] = kind.color();
        s.push_str(&sgr_fg(r, g, b, truecolor));
        s.push('■');
        s.push_str("\x1b[30m");
        s.push_str(kind.label());
        w += disp_width(&legend_item_plain(*kind));
    }
    while w < iw { s.push(' '); w += 1; }
    s
}

// 過去災害の事例一覧(中央パネル・Bキーで開く。何かキーで消える)。draw_popup は1行専用なので
// 複数行パネルとして別に持つ。地点は市区町村の代表点で災害の起きた場所そのものではないため、
// 取り違えられないよう出典と一緒に「市区町村単位の記録」であることを常時1行で出す。脚注の上には
// 種別ごとの色凡例(コロプレスの塗り・マーカーと共通)を添える。
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
    let legend_rows = wrap_legend(iw.saturating_sub(1));
    // 枠(空行・見出し・区切り2本・凡例・脚注・操作案内)を除いた残りが本文に使える行数。
    let chrome_rows = 9 + legend_rows.len();
    let max_body = (map_rows as usize).saturating_sub(chrome_rows).max(1);
    let shown = lines.len().min(max_body);
    let rule = format!(" {}", "─".repeat(iw.saturating_sub(2)));
    let tc_ok = truecolor_safe();
    // fit_cells は SGR エスケープを解さないため、色付き行(Styled)はそのまま書き出し、
    // それ以外(Plain)だけ fit_cells でパディング・切り詰めを行う。
    enum Row { Plain(String), Styled(String) }
    let mut rows: Vec<Row> = vec![Row::Plain(String::new()), Row::Plain(format!(" {title}")), Row::Plain(rule.clone())];
    for l in lines.iter().take(shown) {
        rows.push(Row::Plain(format!(" {l}")));
    }
    if lines.len() > shown {
        rows.push(Row::Plain(format!(" …ほか{}件(画面に収まらない)", lines.len() - shown)));
    }
    rows.push(Row::Plain(rule));
    for kinds in &legend_rows {
        rows.push(Row::Styled(format!(" {}", legend_row_colored(kinds, iw.saturating_sub(1), tc_ok))));
    }
    if truncated {
        // 集計が上限で打ち切られると件数が黙って過少になる。黙って過少にしない。
        rows.push(Row::Plain(" ※取得上限で打ち切られた集計がある(件数は下限)".to_string()));
    }
    rows.push(Row::Plain(" 市区町村単位の記録  出典: 防災科学技術研究所 災害事例データベース".to_string()));
    rows.push(Row::Plain(" 任意のキー(Esc/q)で閉じる".to_string()));
    rows.push(Row::Plain(String::new()));
    let r0 = ((map_rows as usize).saturating_sub(rows.len()) / 2).max(1) as u32;
    let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
    for (i, row) in rows.iter().enumerate() {
        match row {
            Row::Plain(ln) => { let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, BG, fit_cells(ln, iw), RST); }
            Row::Styled(ln) => { let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, BG, ln, RST); }
        }
    }
}

// 通行規制の詳細(中央パネル・Tキーで開く。何かキーで消える)。
// draw_disaster_panel と同じ組み方だが、脚注が「なぜ通れないか」用に固定文言なので別関数にする。
pub(crate) fn draw_regulation_detail_panel<W: Write>(
    out: &mut W,
    cols: u32,
    map_rows: u32,
    title: &str,
    lines: &[String],
) {
    const BG: &str = "\x1b[30;47m";
    const RST: &str = "\x1b[0m";
    let iw = (cols as usize).saturating_sub(6).clamp(24, 96);
    const CHROME_ROWS: usize = 7;
    let max_body = (map_rows as usize).saturating_sub(CHROME_ROWS).max(1);
    let shown = lines.len().min(max_body);
    let rule = format!(" {}", "─".repeat(iw.saturating_sub(2)));
    let mut rows: Vec<String> = vec![String::new(), format!(" {title}"), rule.clone()];
    for l in lines.iter().take(shown) {
        rows.push(format!(" {l}"));
    }
    if lines.len() > shown {
        rows.push(format!(" …ほか{}行(画面に収まらない)", lines.len() - shown));
    }
    rows.push(rule);
    rows.push(" 出典: 国土交通省 道路情報提供システム".to_string());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disaster::DisasterKind;

    // SGRエスケープ(\x1b[...m)を取り除いた可視文字列。幅・文字内容の検証に使う。
    fn visible_text(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for n in chars.by_ref() {
                    if n == 'm' { break; }
                }
                continue;
            }
            out.push(c);
        }
        out
    }

    #[test]
    fn wrap_legend_fits_all_six_kinds_on_one_row_when_wide_enough() {
        let rows = wrap_legend(96);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], DisasterKind::legend_kinds().to_vec());
    }

    #[test]
    fn wrap_legend_splits_into_multiple_rows_when_narrow_and_never_exceeds_the_width() {
        let rows = wrap_legend(24);
        assert!(rows.len() > 1, "24幅では6種が1行に収まらないはず");
        for row in &rows {
            assert!(!row.is_empty(), "空行を作らない");
            let w: usize = row.iter().map(|k| disp_width(&legend_item_plain(*k))).sum::<usize>()
                + row.len().saturating_sub(1);
            assert!(w <= 24, "row width {w} exceeds 24: {row:?}");
        }
        let total: usize = rows.iter().map(|r| r.len()).sum();
        assert_eq!(total, 6, "6種が過不足なく分配される");
    }

    #[test]
    fn legend_row_colored_contains_every_kind_colour_and_resets_to_black_between_items() {
        let kinds = DisasterKind::legend_kinds();
        let row = legend_row_colored(&kinds, 96, true);
        for k in kinds {
            let [r, g, b] = k.color();
            assert!(row.contains(&sgr_fg(r, g, b, true)), "{k:?} の色が含まれる");
        }
        assert_eq!(row.matches("\x1b[30m").count(), kinds.len(), "各項目のあとに黒文字へ戻す(背景色は保つ)");
    }

    #[test]
    fn legend_row_colored_visible_text_matches_plain_items_and_pads_to_width() {
        let kinds = DisasterKind::legend_kinds();
        let row = legend_row_colored(&kinds, 96, true);
        let visible = visible_text(&row);
        assert_eq!(disp_width(&visible), 96, "iw幅ちょうどにパディングされる(背景色を保つため)");
        let expected_prefix: String =
            kinds.iter().map(|k| legend_item_plain(*k)).collect::<Vec<_>>().join(" ");
        assert!(visible.starts_with(&expected_prefix), "visible={visible:?}");
    }

    #[test]
    fn legend_row_colored_falls_back_to_256_colour_when_truecolor_is_off() {
        let kinds = [DisasterKind::Earthquake];
        let row = legend_row_colored(&kinds, 20, false);
        let [r, g, b] = DisasterKind::Earthquake.color();
        assert!(row.contains(&sgr_fg(r, g, b, false)));
        assert!(!row.contains("38;2;"), "truecolor無効なら24bitコードを使わない");
    }

    // ---- 各パネルの描画(小さい端末・長い文字列でも落ちないこと + 見た目の要点) ----

    // 書き出しを (行, 列, 見える文字列) に分ける。カーソル移動(\x1b[行;列H)ごとに1要素。
    // それ以外のCSI(色・行末消去)は取り除き、OSC(\x1b]...\x07 = インライン画像)は丸ごと捨てる。
    fn screen(out: &[u8]) -> Vec<(u32, u32, String)> {
        let s = String::from_utf8_lossy(out);
        let mut rows: Vec<(u32, u32, String)> = Vec::new();
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c != '\u{1b}' {
                if let Some(last) = rows.last_mut() { last.2.push(c); }
                continue;
            }
            match it.next() {
                Some('[') => {
                    let mut params = String::new();
                    for n in it.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            if n == 'H' {
                                let mut p = params.split(';').map(|x| x.parse::<u32>().unwrap_or(1));
                                let r = p.next().unwrap_or(1);
                                rows.push((r, p.next().unwrap_or(1), String::new()));
                            }
                            break;
                        }
                        params.push(n);
                    }
                }
                Some(']') => { for n in it.by_ref() { if n == '\u{7}' { break; } } }
                _ => {}
            }
        }
        rows
    }

    fn texts(out: &[u8]) -> Vec<String> {
        screen(out).into_iter().map(|r| r.2).collect()
    }

    fn at_least_top_left(out: &[u8]) -> bool {
        screen(out).iter().all(|&(r, c, _)| r >= 1 && c >= 1)
    }

    fn empty_spec() -> OverlaySpec {
        OverlaySpec { pois: Vec::new(), routes: Vec::new(), expressway_segments: Vec::new(), roads: Vec::new(),
                      traffic_segments: Vec::new(), warning_segments: Vec::new(), rings: Vec::new(), spots: Vec::new() }
    }

    #[test]
    fn quit_confirm_and_popup_are_centered_and_never_leave_the_screen() {
        let mut out = Vec::new();
        draw_quit_confirm(&mut out, 80, 24);
        let rows = screen(&out);
        assert_eq!(rows.len(), 3);
        assert!(rows[1].2.contains("termmapを終了しますか？ (y/n)"));
        assert_eq!(rows[0].0, 12, "縦中央");
        assert_eq!(rows[1].1, (80 - rows[1].2.chars().count() as u32) / 2, "横中央(文字数基準)");
        let mut tiny = Vec::new();
        draw_quit_confirm(&mut tiny, 5, 0);
        assert!(at_least_top_left(&tiny), "狭い端末でも1行1列より外へ出さない");
        let mut pop = Vec::new();
        draw_popup(&mut pop, 10, 1, &"長い名前".repeat(20));
        assert!(texts(&pop)[1].contains("長い名前長い名前"));
        assert!(at_least_top_left(&pop));
    }

    #[test]
    fn qr_text_is_wrapped_in_a_quiet_zone_with_the_close_hint_at_the_bottom() {
        let mut out = Vec::new();
        draw_qr_text(&mut out, 40, 20, 21, "▀▄▀\n▄▀▄");
        let rows = screen(&out);
        assert_eq!(rows.len(), 2 + 2 + 2 + 1, "上下2行の余白 + QR2行 + 案内");
        assert_eq!(rows[2].2, "  ▀▄▀  ", "左右2セルの余白");
        let hint = rows.last().unwrap();
        assert!(hint.2.contains("任意のキーで閉じる"));
        assert_eq!(hint.0, 21, "案内は最下段");
        let mut empty = Vec::new();
        draw_qr_text(&mut empty, 0, 0, 1, "");
        assert!(at_least_top_left(&empty));
    }

    #[test]
    fn qr_image_emits_one_inline_image_and_the_close_hint() {
        let mut out = Vec::new();
        draw_qr_image(&mut out, 80, 24, 24, &RgbImage::from_pixel(8, 8, image::Rgb([255, 255, 255])));
        let s = String::from_utf8_lossy(&out);
        assert_eq!(s.matches("\x1b]1337;File=inline=1").count(), 1);
        assert!(s.contains("width=20;height=10"), "モジュール数に関係なく一定のセル数");
        assert!(s.contains("任意のキーで閉じる"));
        let mut tiny = Vec::new();
        draw_qr_image(&mut tiny, 3, 2, 2, &RgbImage::new(0, 0)); // 空画像・狭い端末でも落ちない
        assert!(at_least_top_left(&tiny));
    }

    #[test]
    fn spot_form_highlights_only_the_selected_field() {
        const SEL: &str = "\x1b[97;40m";
        for field in 0..4usize {
            let mut out = Vec::new();
            draw_spot_form(&mut out, 80, 24, "満州軒", "https://g.co/x", field, 1, "ラーメン");
            let raw = String::from_utf8_lossy(&out).into_owned();
            let rows = texts(&out);
            assert_eq!(rows.len(), 8);
            assert!(rows.iter().all(|t| disp_width(t) == 60), "全行が箱の幅(60)に揃う: {rows:?}");
            assert!(rows[1].contains("新規スポット [ラーメン]"));
            let highlighted = [format!("{SEL}  名称: 満\u{2588}州軒"), format!("{SEL}  GoogleマップURL(任意): h\u{2588}ttps"),
                               format!("{SEL}[送信]"), format!("{SEL}[戻る]")];
            for (i, h) in highlighted.iter().enumerate() {
                assert_eq!(raw.contains(h.as_str()), i == field, "field={field} で {h:?}");
            }
        }
    }

    #[test]
    fn forms_keep_their_minimum_width_on_a_tiny_terminal_with_long_input() {
        let long = "とても長い店名".repeat(30);
        let mut out = Vec::new();
        draw_spot_form(&mut out, 10, 3, &long, &long, 1, 999, "");
        assert!(texts(&out).iter().all(|t| disp_width(t) == 24), "箱は最小幅24で切り詰める");
        assert!(at_least_top_left(&out));
        let mut out = Vec::new();
        draw_poi_kind_form(&mut out, 10, 3, &long, &long, 0, 999);
        assert!(texts(&out).iter().all(|t| disp_width(t) == 24));
        assert!(at_least_top_left(&out));
    }

    #[test]
    fn poi_kind_form_highlights_the_add_button() {
        let mut out = Vec::new();
        draw_poi_kind_form(&mut out, 80, 24, "パン屋", "shop=bakery", 2, 0);
        let raw = String::from_utf8_lossy(&out).into_owned();
        assert!(raw.contains("\x1b[97;40m[追加]"));
        assert!(!raw.contains('\u{2588}'), "ボタン選択中は入力欄にカーソルを出さない");
        assert!(texts(&out)[1].contains("新しい目的地カテゴリ"));
    }

    #[test]
    fn wander_form_gauge_fills_in_proportion_to_the_distance() {
        let gauge = |d: f64| {
            let mut out = Vec::new();
            draw_wander_form(&mut out, 80, 24, d);
            let rows = texts(&out);
            (rows[6].matches('█').count(), rows[6].matches('░').count()) // 本文6行の後にゲージを上書きする
        };
        assert_eq!(gauge(10.0), (0, 56), "下限10kmは空");
        assert_eq!(gauge(105.0), (28, 28), "中間は半分");
        assert_eq!(gauge(200.0), (56, 0), "上限200kmは満杯");
        assert_eq!(gauge(-50.0), (0, 56), "範囲外は端へ寄せる");
        assert_eq!(gauge(999.0), (56, 0));
        assert_eq!(gauge(f64::NAN).0 + gauge(f64::NAN).1, 56, "壊れた値でも落ちない");
    }

    #[test]
    fn text_input_panel_is_drawn_only_for_text_focuses() {
        let draw = |f: &Focus| {
            let mut out = Vec::new();
            draw_text_input(&mut out, 80, 24, f, 1);
            String::from_utf8_lossy(&out).into_owned()
        };
        assert!(draw(&Focus::Map).is_empty(), "入力欄の無い画面では何も書かない");
        let s = draw(&Focus::Search("東京".to_string()));
        assert!(s.contains("地名・住所で検索") && s.contains("東\u{2588}京"));
        assert!(draw(&Focus::SettingsEdit(6, "800".into())).contains("道路の点間隔(m)"));
        assert!(draw(&Focus::SettingsEdit(17, String::new())).contains("Google APIキー"));
        for f in [Focus::SaveName(String::new()), Focus::NearSearch(String::new()), Focus::NewCat(String::new()),
                  Focus::RoadSearch(String::new()), Focus::Recommend(String::new()),
                  Focus::SpotRename(String::new(), 0), Focus::SpotEditName(String::new(), 0)] {
            assert!(!draw(&f).is_empty());
        }
    }

    #[test]
    fn pickers_bracket_only_the_selected_item() {
        for sel in [0u8, 9] {
            let mut out = Vec::new();
            draw_color_pick(&mut out, 80, 24, sel);
            let sw = &texts(&out)[2];
            assert_eq!((sw.matches('[').count(), sw.matches(']').count()), (1, 1), "{sw:?}");
            assert_eq!(sw.find('['), Some(2 + 4 * sel as usize), "左余白2 + 1色4セル");
        }
        let mut out = Vec::new();
        draw_color_pick(&mut out, 80, 24, 99);
        assert!(!texts(&out)[2].contains('['), "範囲外の選択は囲まない");

        let mut out = Vec::new();
        draw_shape_pick(&mut out, 80, 24, 6);
        let sw = &texts(&out)[2];
        assert_eq!((sw.matches('[').count(), sw.matches(']').count()), (1, 1));
        assert!(sw.contains("[✕]"));
        for g in ["■", "▲", "●", "◆", "＋", "✦", "✕"] {
            assert!(sw.contains(g), "{g} が並んでいない");
        }
        let mut tiny = Vec::new();
        draw_shape_pick(&mut tiny, 1, 1, 0);
        assert!(at_least_top_left(&tiny));
    }

    #[test]
    fn disaster_panel_folds_the_overflow_and_notes_a_truncated_count() {
        let lines: Vec<String> = (0..50).map(|i| format!("{i}件目")).collect();
        let (cols, map_rows) = (80u32, 20u32);
        let iw = 74; // (80-6).clamp(24, 96)
        let shown = map_rows as usize - (9 + wrap_legend(iw - 1).len());
        let mut out = Vec::new();
        draw_disaster_panel(&mut out, cols, map_rows, "野田市 ─ 記録 50件", &lines, true);
        let rows = texts(&out);
        assert!(rows.iter().any(|t| t.contains(&format!("…ほか{}件(画面に収まらない)", 50 - shown))), "{rows:?}");
        assert!(rows.iter().any(|t| t.contains("※取得上限で打ち切られた集計がある")));
        assert!(rows.iter().all(|t| disp_width(t) == iw), "凡例の色付き行も含めて幅が揃う");
        let mut out = Vec::new();
        draw_disaster_panel(&mut out, cols, map_rows, "t", &lines[..2], false);
        let rows = texts(&out);
        assert!(!rows.iter().any(|t| t.contains("…ほか") || t.contains("※取得上限")), "収まれば畳まない・打ち切り無しなら注記しない");
        let mut tiny = Vec::new();
        draw_disaster_panel(&mut tiny, 0, 0, &"長".repeat(200), &lines, false);
        assert!(texts(&tiny).iter().any(|t| t.contains("0件目")), "狭くても本文を最低1行出す");
        assert!(at_least_top_left(&tiny));
    }

    #[test]
    fn regulation_panel_folds_lines_that_do_not_fit() {
        let lines: Vec<String> = (0..30).map(|i| format!("行{i}")).collect();
        let mut out = Vec::new();
        draw_regulation_detail_panel(&mut out, 80, 17, "国道418号", &lines);
        let rows = texts(&out);
        assert!(rows.iter().any(|t| t.contains("…ほか20行(画面に収まらない)")), "本文は 17-7=10 行まで: {rows:?}");
        assert!(rows.iter().any(|t| t.contains("国土交通省")));
        assert!(rows.iter().all(|t| disp_width(t) == 74));
        let mut tiny = Vec::new();
        draw_regulation_detail_panel(&mut tiny, 0, 0, "", &[]);
        assert!(at_least_top_left(&tiny));
    }

    // オンボーディングのワードマークはヘルプ画面(keymap::LOGO)と同じ見た目を保つ(別々に持っているため)。
    #[test]
    fn onboarding_wordmark_matches_the_help_screen_logo() {
        let mut out = Vec::new();
        draw_onboarding(&mut out, 80, 24);
        let raw = String::from_utf8_lossy(&out).into_owned();
        for (bold, (r, g, b), ln) in crate::keymap::LOGO {
            let style = format!("{}{}", if bold { "\x1b[1m" } else { "" }, sgr_fg(r, g, b, truecolor_safe()));
            assert!(raw.contains(&format!("{style}{ln}")), "{ln:?} の文字か色がヘルプと違う");
        }
        let mut tiny = Vec::new();
        draw_onboarding(&mut tiny, 0, 0);
        assert!(at_least_top_left(&tiny));
    }

    #[test]
    fn elevation_band_draws_the_label_the_chart_and_the_position_cursor() {
        let mut spec = empty_spec();
        spec.routes.push(Route { pts: vec![(35.0, 139.0), (35.01, 139.0), (35.02, 139.0)], color: [0, 220, 255], thickness: 2 });
        let mut out = Vec::new();
        draw_elevation_band(&mut out, 40, 20, 4, &[100.0, 300.0, 200.0], 200.0, &spec, 35.01, 139.0);
        let rows = screen(&out);
        assert_eq!(rows[0].0, 21, "ラベルは地図の直下");
        assert!(rows[0].2.contains("標高 ↑200m  最高300m 最低100m"), "{:?}", rows[0]);
        assert!(rows[1].2.contains("300m"), "最上段の目盛りは最高値");
        let cursors: Vec<_> = rows.iter().filter(|r| r.2 == "|").collect();
        assert_eq!(cursors.len(), 4, "標高帯の行数ぶんの縦カーソル");
        // 真ん中の点にいる → グラフ幅33列の中央(16)+1 + 目盛り7列
        assert!(cursors.iter().all(|c| c.1 == 24), "{cursors:?}");
    }

    #[test]
    fn elevation_band_without_a_route_or_rows_draws_only_the_label() {
        let mut out = Vec::new();
        draw_elevation_band(&mut out, 0, 0, 0, &[], 0.0, &empty_spec(), 0.0, 0.0);
        assert_eq!(screen(&out).len(), 1);
        let mut out = Vec::new();
        draw_elevation_band(&mut out, 40, 20, 3, &[], 0.0, &empty_spec(), 0.0, 0.0); // 標高データ無し
        assert!(!screen(&out).iter().any(|r| r.2 == "|"), "ルートが無ければカーソルを出さない");
    }
}
