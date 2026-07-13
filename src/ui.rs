// 対話UIループ。main.rs から機械的に切り出したもの(挙動は不変)。
// show_busy / HELP / TermGuard / interactive を収める。fit_cells 等はクレートルート(main.rs)側に残す。

use crate::*;
use crate::geo::*;
use crate::tiles::*;
use crate::render::*;
use crate::route::*;
use crate::poi::*;
use crate::spots::*;
use crate::share::*;
use std::io::Write;
use image::{RgbImage, imageops::FilterType};

// ---- テキスト1行編集ヘルパ(全テキスト入力欄で共有) ----
// cur は「文字単位」のカーソル位置(0..=文字数)。byte offset は char_indices で都度求めるのでマルチバイト安全。

// 文字位置 char_idx の byte offset を返す(末尾なら文字列長)。
fn char_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

// cur 位置に文字列 s を挿入し、cur を挿入文字数ぶん進める(ペースト用)。
fn insert_str_at(buf: &mut String, cur: &mut usize, s: &str) {
    let at = char_byte(buf, *cur);
    buf.insert_str(at, s);
    *cur += s.chars().count();
}

// SpotForm のフィールド切替時、移動先フィールドのバッファ文字数(末尾)を返す。ボタン欄は0。
fn form_cur(name: &str, url: &str, field: usize) -> usize {
    match field { 0 => name.chars().count(), 1 => url.chars().count(), _ => 0 }
}

// 1行入力の編集。対象キー(←→ Home/End 文字入力 Backspace Delete)を処理したら true、非対象は false。
fn edit_line(buf: &mut String, cur: &mut usize, code: crossterm::event::KeyCode) -> bool {
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
fn render_with_cursor(buf: &str, cur: usize) -> String {
    let chars: Vec<char> = buf.chars().collect();
    let cur = cur.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let after: String = chars[cur..].iter().collect();
    format!("{before}\u{2588}{after}")
}

// 同期API待ちの間、中央に「通信中…」を出す(呼び出し直前にflushして表示)。
// 同期処理なのでアニメーションはしないが、待ちが起きていることを示す。
fn show_busy<W: std::io::Write>(out: &mut W, cols: u32, rows: u16, msg: &str) {
    let text = format!("  ⏳ {}  ", msg);
    let w = text.chars().count();
    let c0 = ((cols as usize).saturating_sub(w) / 2).max(1);
    let r0 = (rows / 2).max(1);
    let pad = " ".repeat(w);
    let _ = write!(out, "\x1b[{};{}H\x1b[7m{}\x1b[0m", r0, c0, pad);
    let _ = write!(out, "\x1b[{};{}H\x1b[7m{}\x1b[0m", r0 + 1, c0, text);
    let _ = write!(out, "\x1b[{};{}H\x1b[7m{}\x1b[0m", r0 + 2, c0, pad);
    let _ = out.flush();
}

// 単一テキスト欄の中央入力パネル(底面バーでなく地図中央に重畳。SpotFormと同じ手法)。
// title=見出し / hint=下部の操作説明 / buf=入力中の文字列 / cur=カーソル文字位置。
fn draw_input_panel<W: std::io::Write>(out: &mut W, cols: u32, map_rows: u32, title: &str, hint: &str, buf: &str, cur: usize) {
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

// 対話モードの操作マニュアル(? で表示)
const HELP: &[&str] = &[
    " termmap 対話モード ─ 操作マニュアル",
    "",
    " [移動]",
    "   ←↑↓→        パン (既定で速い / Shift+矢印で微調整)",
    "   + / -          ズーム",
    "   /              住所・地名で検索して移動",
    "   a              中心の住所を表示",
    "   Enter          中心付近の最寄りお気に入りにスナップ＋名前表示",
    "   Space          メニュー(全操作をキー無しで選べる)",
    "",
    " [ルートを作る]  中心の十字(黄)が置く位置",
    "   v              中心に地点を置く (並び順で 始点→…→終点 が自動)",
    "   Tab            並べ替えビューへ (↑↓選択・Space掴んで↑↓で移動)",
    "   [ / ]          選択点を 前 / 後ろ へ並べ替え",
    "   x              選択点を削除     c  ルート全消去",
    "   m              モード切替  下道 → 高速 → 最短",
    "   n              代替ルート候補を巡回(BRouterの案 1〜4)",
    "   r              道路名/refで道路を1本の塊として追加(例: 国道16号 / E20)。別色で表示",
    "   D              道路の塊を一覧(個別に x で削除・c で全消去)",
    "   @              おすすめ: 方向性を入力→AI(claude)が提案→実在確認して候補表示(設定でON要)",
    "   W              走りまくり: 峠/展望を巡る周回を自動生成(連打で別案)",
    "",
    " [目的地・お気に入り]",
    "   f              カテゴリ検索 1ｶﾞｿ 2ｶﾌｪ 3ｺﾝﾋﾞﾆ 4道の駅 5展望 6公園 7峠道",
    "                   / でキーワード周辺検索(現在範囲) → リスト",
    "                   → リスト: ↑↓選択(地図追従) v=地点追加 Enter移動 f再検索 Esc閉",
    "   S / L          ルートを お気に入り保存 / 呼び出し",
    "   g              ルートを GPX 保存 (termmap-route.gpx)",
    "   E              標高プロファイル 表示/非表示 (ルート確定後・下部に折れ線)",
    "   A              ルート再生 開始/停止 (プレビュー走行・全体を約20秒で自動パン)",
    "   G              ライブ現在地 ON/OFF (CoreLocationCLIを5秒毎・自位置と軌跡を表示)",
    "",
    " [マイスポット] (ラーメン等をカテゴリ別に色分け保存)",
    "   P              カテゴリ一覧を開く",
    "                   カテゴリ: ↑↓ Enter=中へ n新規 r改名 c色 x削除(空のみ)",
    "                   スポット: ↑↓ Enter=移動 n新規(現在地) r改名 m位置を中心へ移動 x削除 Esc戻る",
    "   V              マイスポットの表示 / 非表示",
    "   o              スマホ共有(GoogleマップのQRをポップアップ表示)",
    "",
    " [実写]",
    "   i              中心地点の実写(Street View)を全画面表示  ←→向き ↑↓前後移動 Esc/q戻る",
    "                   要 config.toml [streetview] api_key",
    "",
    " [実画像表示] (iTerm2 / WezTerm のみ)",
    "   I              地図・実写をAA↔実画像でトグル (, の設定でも切替)",
    "",
    " [起動オプション]  --range KM,.. 航続リング / --route / --load-route 名前",
    "",
    "",
    " [設定]  , で設定画面 (braille/classify/edge/mono/style を実行中に切替・sで保存)",
    "         config.toml で既定を指定可 ([display]/[streetview])",
    "",
    "   ?  ヘルプ   q  終了   Esc  サブモード取消   Ctrl+C  計算の中断(終了はq)",
    "",
    "   (任意のキーで閉じる)",
];

// 道路の塊(RoadSeg)ごとの表示色。BRouterルートの cyan [0,220,255] と被らない色を len で循環。
const ROAD_PALETTE: &[[u8; 3]] = &[
    [180, 80, 255],  // 紫
    [255, 140, 0],   // 橙
    [0, 200, 120],   // 緑
    [255, 80, 180],  // 桃
    [230, 200, 0],   // 黄
];

// 道路名検索(r)で追加した道路1本ぶんの塊。個別に色を持ち、一覧から個別削除できる。
struct RoadSeg { name: String, color: [u8; 3], pts: Vec<(f64, f64)> }

// Space メニュー。2階層(カテゴリ→項目)。項目は「操作として読める動詞ラベル」+ 単キー。
// 実処理は run_action! マクロ(interactive 内)に集約し、各キーの直接操作と共通化している。
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    SearchPlace, SearchPoi, ShowAddress, Recommend,                                    // 検索・移動
    RouteForm, AddVia, RoadRoute, ManageRoads, Wander, CycleMode, AltRoute, ClearRoute, // ルート作成(RouteForm=並べ替えを開く / AddVia=中心に地点を置く / ManageRoads=道路の塊を管理)
    ManageSpots, ToggleSpots,                                                          // スポット
    ToggleElevation, StreetView, PlayRoute, ToggleGps,                                 // ナビ・表示
    SaveRoute, LoadRoute, SaveGpx, ShareQr,                                            // 保存・共有
    Settings, Help,                                                                    // 設定・ヘルプ
}
struct MenuItem { label: &'static str, key: char, action: MenuAction }
struct MenuCategory { label: &'static str, items: &'static [MenuItem] }

const MENU_CATEGORIES: &[MenuCategory] = &[
    MenuCategory { label: "検索・移動", items: &[
        MenuItem { label: "地名を検索",        key: '/', action: MenuAction::SearchPlace },
        MenuItem { label: "目的地を探す",      key: 'f', action: MenuAction::SearchPoi },
        MenuItem { label: "中心の住所を見る",  key: 'a', action: MenuAction::ShowAddress },
        MenuItem { label: "おすすめを出す",    key: '@', action: MenuAction::Recommend },
    ]},
    MenuCategory { label: "ルート作成", items: &[
        MenuItem { label: "地点を置く(中心)",  key: 'v', action: MenuAction::AddVia },
        MenuItem { label: "並べ替え・編集",    key: 'R', action: MenuAction::RouteForm },
        MenuItem { label: "道路名から追加",    key: 'r', action: MenuAction::RoadRoute },
        MenuItem { label: "道路の塊を管理",    key: 'D', action: MenuAction::ManageRoads },
        MenuItem { label: "おまかせ周回",      key: 'W', action: MenuAction::Wander },
        MenuItem { label: "移動モード切替",    key: 'm', action: MenuAction::CycleMode },
        MenuItem { label: "別ルートを検索",    key: 'n', action: MenuAction::AltRoute },
        MenuItem { label: "ルートを消去",      key: 'c', action: MenuAction::ClearRoute },
    ]},
    MenuCategory { label: "スポット", items: &[
        MenuItem { label: "マイスポットを開く", key: 'P', action: MenuAction::ManageSpots },
        MenuItem { label: "スポット表示を切替", key: 'V', action: MenuAction::ToggleSpots },
    ]},
    MenuCategory { label: "ナビ・表示", items: &[
        MenuItem { label: "標高プロファイル",  key: 'E', action: MenuAction::ToggleElevation },
        MenuItem { label: "実写を見る",        key: 'i', action: MenuAction::StreetView },
        MenuItem { label: "ルートを再生",      key: 'A', action: MenuAction::PlayRoute },
        MenuItem { label: "ライブ現在地",      key: 'G', action: MenuAction::ToggleGps },
    ]},
    MenuCategory { label: "保存・共有", items: &[
        MenuItem { label: "ルートを保存",      key: 'S', action: MenuAction::SaveRoute },
        MenuItem { label: "保存ルートを開く",  key: 'L', action: MenuAction::LoadRoute },
        MenuItem { label: "GPXを書き出す",     key: 'g', action: MenuAction::SaveGpx },
        MenuItem { label: "QRで共有",          key: 'o', action: MenuAction::ShareQr },
    ]},
    MenuCategory { label: "設定・ヘルプ", items: &[
        MenuItem { label: "設定を開く",        key: ',', action: MenuAction::Settings },
        MenuItem { label: "ヘルプ",            key: '?', action: MenuAction::Help },
    ]},
];

// メニューの階層。Categories=トップ(カテゴリ選択) / Items(cat)=そのカテゴリの項目選択。
#[derive(Clone, Copy)]
enum MenuLevel { Categories, Items(usize) }

// トップメニューで押された文字キーを全カテゴリ横断で対応するアクションに引く(熟練者の直打ち用)。
fn menu_action_for_key(c: char) -> Option<MenuAction> {
    MENU_CATEGORIES.iter().flat_map(|cat| cat.items.iter()).find(|it| it.key == c).map(|it| it.action)
}

// 表示セル幅(fit_cells と同じ規則: ASCII=1 / 非ASCII=2)。
fn disp_width(s: &str) -> usize { unicode_width::UnicodeWidthStr::width(s) }

// メニュー項目1行。ラベルは左、キーは右端に揃える(幅 w セル内。行頭カーソル prefix の1セルは呼び出し側が足す)。
fn menu_row(label: &str, key: char, w: usize) -> String {
    let mut ks = [0u8; 4];
    let key_s = key.encode_utf8(&mut ks);
    let pad = w.saturating_sub(2 + disp_width(label) + disp_width(key_s));
    format!("  {label}{}{key_s}", " ".repeat(pad))
}

// 左袖リストの表示開始位置(offset)を、選択(sel)が viewport 内に入るよう最小移動で更新する。
// 項目数(count)が viewport を超えたときにスクロール追従させ、選択が画面外に消えないようにする。
fn ensure_visible(offset: &mut usize, sel: usize, count: usize, viewport: usize) {
    if viewport == 0 {
        *offset = 0;
        return;
    }
    if sel < *offset {
        *offset = sel; // 上へはみ出た → 選択を先頭に
    } else if sel >= *offset + viewport {
        *offset = sel + 1 - viewport; // 下へはみ出た → 選択を末尾に
    }
    *offset = (*offset).min(count.saturating_sub(viewport)); // 末尾側の空きを詰める
}

// 初回起動オンボーディングの既読マーカー(~/.config/termmap/onboarded)。存在すれば以後は出さない。
fn onboarded_marker() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config/termmap/onboarded"))
}


// ---- 対話モード (crossterm) ----
// 端末状態を RAII で復元する。パニック/早期return でも Drop で raw mode と代替スクリーンを必ず戻す。
struct TermGuard;
impl TermGuard {
    fn enter() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide, crossterm::event::EnableBracketedPaste)?;
        Ok(Self)
    }
}
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste,
            crossterm::cursor::Show, crossterm::terminal::LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub(crate) fn interactive(mut cx: f64, mut cy: f64, mut z: u32, a: &Args) -> std::io::Result<()> {
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    enum Focus { Map, Menu(MenuLevel), Search(String), SaveName(String), NearSearch(String), PoiMenu, PoiList, RouteList, WaypointList, RoadList,
                 NewCat(String), SpotForm { name: String, url: String, field: usize }, SpotList, SpotCatList, SpotRename(String, usize), Settings, RoadSearch(String), SpotEditName(String, usize), Recommend(String), ColorPick { cat: usize } }
    let _guard = TermGuard::enter()?; // Drop で必ず端末復元
    let mut cache: Cache = Cache::new();
    let mut out = std::io::stdout();
    let mut addr = String::new();          // 'a' 住所 / 一時メッセージ
    let mut focus = Focus::Map;
    let mut cfg = config::load_config();   // 設定(streetview key / 描画既定 等・設定画面で書き換え)
    let mut opts = a.clone();              // 実行中に変えられる描画設定(Argsのコピー)
    // config を既定として適用(CLIフラグは ON 方向で優先。style は CLI が既定osmなら config 採用)
    opts.braille = opts.braille || cfg.braille;
    opts.classify = opts.classify || cfg.classify;
    opts.edge = opts.edge || cfg.edge;
    opts.mono = opts.mono || cfg.mono;
    if opts.style == "osm" { opts.style = cfg.style.clone(); }
    let mut set_sel: usize = 0;            // 設定画面の選択行
    let mut input_cur: usize = 0;          // テキスト入力欄のカーソル位置(文字単位)。テキストFocus開始時に該当バッファ末尾へ
    let mut menu_cat_sel: usize = 0;       // Space メニュー: トップのカテゴリ選択
    let mut menu_item_sel: usize = 0;      // Space メニュー: 展開後の項目選択
    let mut poimenu_sel: usize = 0;        // 目的地カテゴリの選択行
    let mut street: Option<(RgbImage, i32, f64, f64)> = None; // 実写(画像, heading, lat, lon)

    let (home_lat, home_lon) = pixel_to_deg(cx, cy, z);
    let mut spec = build_spec(a, home_lat, home_lon); // --range のリングは保持

    let mut wps: Vec<(f64, f64)> = a.route.clone().unwrap_or_default(); // 始点..終点
    let mut wp_sel: usize = 0;             // Tab で巡回する選択 waypoint
    let mut road_segs: Vec<RoadSeg> = Vec::new(); // 道路名検索(r)で追加した道路の塊(別色レイヤ・spec.roadsへ同期)
    let mut road_sel: usize = 0;           // 道路一覧(RoadList)の選択行
    let mut grab = false;                  // 並べ替えビューで地点を「掴んで」移動中か
    let mut mode = a.route_mode.clone();
    let mut pois: Vec<(f64, f64, String, PoiCat)> = Vec::new(); // 目的地検索結果
    let mut poi_sel: usize = 0;
    let mut poi_label = String::new();
    let mut route_names: Vec<String> = Vec::new(); // お気に入り一覧(L)
    let mut rn_sel: usize = 0;
    let mut help = false; // ? でヘルプ表示
    let mut qr_view: Option<String> = None; // o でGoogleマップQRをポップアップ表示
    let mut route_alt: u32 = 0; // n で BRouter の代替ルート(0..=3)を巡回
    let mut route_ele: Vec<f64> = Vec::new(); // 直近ルートの標高列(pts と同数)
    let mut route_ascend: f64 = 0.0;          // 直近ルートの累積登り(m)
    let mut show_elev = false;                // E で標高プロファイル表示
    let mut gps_rx: Option<gpslive::GpsPoller> = None; // G ライブ現在地(drop で停止)
    let mut gps_pos: Option<(f64, f64)> = None; // 最新の自位置
    let mut gps_trail: Vec<(f64, f64)> = Vec::new(); // 通過ブレッドクラム
    let mut play: Option<f64> = None; // A ルート再生(先頭からの距離m。Noneで停止)
    let mut scache = searchcache::load(); // 検索結果キャッシュ(キーワード+位置→結果。API節約)
    let mut popup: Option<String> = None; // 中央に出す一時ポップアップ(スポット名等・任意キーで閉じる)
    // ルート計算のバックグラウンド受信(マーカーは即時、ルート線は別スレッド)
    let (mut route_note, mut route_job) = {
        let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0);
        (n_, j_)
    };
    // ルート計算と同じ非同期パターンで、検索/周辺/実写/おすすめの通信もバックグラウンド化する。
    // 新規spawn時に古いrxはdropされる=最新のみ採用(generation ID不要)。
    let mut search_job: Option<std::sync::mpsc::Receiver<(String, String, Result<Vec<(f64, f64, String)>, String>)>> = None; // (ckey, query, geocode結果)
    let mut near_job: Option<std::sync::mpsc::Receiver<(String, Vec<(f64, f64, String)>)>> = None; // (query, search_nearbyのosm結果)
    let mut street_job: Option<std::sync::mpsc::Receiver<(f64, f64, i32, Result<image::RgbImage, String>)>> = None; // (lat, lon, heading, 実写画像)
    let mut recommend_job: Option<std::sync::mpsc::Receiver<Result<Vec<(f64, f64, String)>, String>>> = None; // 実在確認済みスポット列
    let mut spin: usize = 0; // 通信中スピナーのフレーム(毎ループ+1)
    let mut spots = load_spots();          // マイスポット
    let mut spot_cats = load_spot_cats();
    let mut show_spots = true;
    let mut sp_sel: usize = 0;
    let mut cat_sel: usize = 0;
    let mut cur_cat = String::new(); // スポット一覧で表示中のカテゴリ
    let mut pending_spot: Option<(f64, f64, String)> = None; // 検索結果からお気に入り登録する際の保留(座標+名前)。カテゴリ選択待ち
    let mut list_offset: usize = 0; // 左袖リストのスクロール開始位置(表示中の1リストで共有・ensure_visibleで追従)
    let mut color_sel: u8 = 0; // 色ピッカーで選択中のパレットindex
    let mut onboard = onboarded_marker().map_or(false, |p| !p.exists()); // 初回起動なら操作案内を出す
    let mut spot_move_confirm: Option<usize> = None; // m(中心へ移動)の確認待ち。上書きは破壊的なのでy/nを挟む
    apply_spots(&mut spec, &spots, &spot_cats, show_spots);
    // 操作UI効果音(macOS afplay)。設定OFF/非macOS/afplay不在なら no-op。設定トグルで作り直す。
    let mut snd = sound::Sound::new(cfg.sound_enabled);

    // メニュー項目/直接キー どちらからでも同じ処理を走らせる。
    // lat/lon/cols/tr は各ループで再計算されるフレーム値。マクロ衛生性のため引数で受け取る。
    macro_rules! run_action { ($act:expr, $lat:expr, $lon:expr, $cols:expr, $tr:expr) => {{
        match $act {
            MenuAction::SearchPlace => { input_cur = 0; focus = Focus::Search(String::new()); }
            MenuAction::SearchPoi => { focus = Focus::PoiMenu; }
            MenuAction::ShowAddress => { addr = reverse_geocode($lat, $lon).unwrap_or_else(|e| format!("({e})")); }
            MenuAction::Recommend => {
                if !cfg.llm_recommend_enabled { snd.play("error"); addr = "おすすめ: 設定でOFF(,でON)".into(); }
                else if !recommend::claude_available(&cfg.llm_command) { snd.play("error"); addr = "おすすめ: claudeが無い(設定のLLM/コマンド確認)".into(); }
                else { input_cur = 0; focus = Focus::Recommend(String::new()); }
            }
            MenuAction::RouteForm => { if wps.is_empty() { addr = "先に v で地点を置いてね".into(); } else { wp_sel = 0; grab = false; focus = Focus::WaypointList; } }
            MenuAction::AddVia => { snd.play("pop"); wp_add(&mut wps, ($lat, $lon)); let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; addr = format!("地点を追加 #{}", wps.len()); }
            MenuAction::RoadRoute => { input_cur = 0; focus = Focus::RoadSearch(String::new()); }
            MenuAction::Wander => {
                let dist = a.dist.unwrap_or(40.0);
                match wander_route(($lat, $lon), dist, &a.shape) {
                    Ok(w) => { wps = w; wp_sel = 0; let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = nn; route_job = jj; }
                    Err(e) => addr = format!("({e})"),
                }
            }
            MenuAction::CycleMode => { mode = match mode_label(&mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
            MenuAction::AltRoute => {
                if wps.len() >= 2 {
                    route_alt = (route_alt + 1) % 4;
                    let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, route_alt);
                    route_note = nn; route_job = jj;
                } else { snd.play("error"); addr = "ルート未確定".into(); }
            }
            MenuAction::ClearRoute => { wps.clear(); wp_sel = 0; road_segs.clear(); spec.roads.clear(); let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
            MenuAction::ManageRoads => { if road_segs.is_empty() { snd.play("error"); addr = "道路の塊がまだ無い(rで道路を追加)".into(); } else { road_sel = 0; focus = Focus::RoadList; } }
            MenuAction::ManageSpots => { cat_sel = 0; focus = Focus::SpotCatList; }
            MenuAction::ToggleSpots => { show_spots = !show_spots; apply_spots(&mut spec, &spots, &spot_cats, show_spots); addr = if show_spots { "マイスポット表示".into() } else { "マイスポット非表示".into() }; }
            MenuAction::ToggleElevation => {
                show_elev = !show_elev;
                if show_elev && (spec.routes.is_empty() || !route_ele.iter().any(|&z| z != 0.0)) { addr = "標高: ルート確定後に表示".into(); }
            }
            MenuAction::StreetView => {
                if !streetview::available(&cfg.google_maps_api_key) { snd.play("error"); addr = "実写: APIキー未設定(config.toml [streetview])".into(); }
                else {
                    // 実写取得を別スレッドへ。focus は Map のまま(メニューは既に閉じている)でスピナーが回る。
                    let (la, lo) = ($lat, $lon);
                    let key = cfg.google_maps_api_key.clone();
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let r = streetview::fetch(la, lo, 0, 640, 480, &key);
                        let _ = tx.send((la, lo, 0, r));
                    });
                    street_job = Some(rx);
                }
            }
            MenuAction::PlayRoute => {
                if spec.routes.last().map_or(false, |r| r.pts.len() >= 2) {
                    if play.is_some() { play = None; addr = "再生: 停止".into(); }
                    else { play = Some(0.0); addr = "再生: 開始(Aで停止)".into(); }
                } else { snd.play("error"); addr = "再生: ルート未確定".into(); }
            }
            MenuAction::ToggleGps => {
                if gps_rx.is_some() { gps_rx = None; addr = "ライブ現在地: OFF".into(); }
                else {
                    let bin = if std::path::Path::new("/opt/homebrew/bin/CoreLocationCLI").exists() { "/opt/homebrew/bin/CoreLocationCLI" } else { "CoreLocationCLI" };
                    if gpslive::available(bin) { gps_rx = Some(gpslive::start_poller(bin.to_string(), 5)); gps_trail.clear(); gps_pos = None; addr = "ライブ現在地: ON(5秒ごと)".into(); }
                    else { addr = "ライブ: CoreLocationCLI無し(brew install corelocationcli)".into(); }
                }
            }
            MenuAction::SaveRoute => { input_cur = 0; focus = Focus::SaveName(String::new()); }
            MenuAction::LoadRoute => { route_names = list_named_routes(); rn_sel = 0; if route_names.is_empty() { addr = "お気に入り無し".into(); } else { focus = Focus::RouteList; } }
            MenuAction::SaveGpx => match spec.routes.last() {
                Some(rt) => addr = match write_gpx("termmap-route.gpx", &rt.pts) { Ok(_) => "GPX保存: termmap-route.gpx".into(), Err(e) => format!("({e})") },
                None => { snd.play("error"); addr = "ルート未確定".into(); }
            },
            MenuAction::ShareQr => {
                if wps.len() >= 2 {
                    let (url, _) = gmaps_url(&wps);
                    match qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L) {
                        Ok(c) => qr_view = Some(c.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(false).build()),
                        Err(_) => addr = "QR生成失敗".into(),
                    }
                } else { snd.play("error"); addr = "ルート未確定".into(); }
            }
            MenuAction::Settings => { set_sel = 0; focus = Focus::Settings; }
            MenuAction::Help => { help = true; }
        }
    }};}

    // road_segs の変更後に描画用の spec.roads を作り直す(trigger_route等では消えない別レイヤ)。
    macro_rules! sync_roads { () => {
        spec.roads = road_segs.iter().map(|r| Route { pts: r.pts.clone(), color: r.color, thickness: 2 }).collect();
    };}

    let _ = write!(out, "\x1b[2J");
    loop {
        spin = spin.wrapping_add(1); // 通信中スピナーのアニメ用(毎フレーム進める)
        let (tc, tr) = crossterm::terminal::size().unwrap_or((100, 40));
        let cols = tc.max(20) as u32;
        let map_rows = (tr.max(3) - 1) as u32;
        if help { // ヘルプ全画面。任意キーで閉じる
            let _ = write!(out, "\x1b[2J\x1b[H");
            for (i, l) in HELP.iter().enumerate().take(map_rows as usize) {
                let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + 1, l);
            }
            let _ = write!(out, "\x1b[{};1H\x1b[7m 任意のキーで閉じる \x1b[0m\x1b[K", tr);
            let _ = out.flush();
            if let Event::Key(_) = event::read()? { help = false; }
            continue;
        }
        if street.is_some() { // 実写(Street View)全画面。←→で向き、Esc/qで戻る
            { // 描画(不変借用のスコープ)
                let (img, heading, slat, slon) = street.as_ref().unwrap();
                if cfg.image_mode && image_capable() {
                    // 実画像モード: 実写を全幅×map_rows のインライン画像で表示
                    let _ = write!(out, "\x1b[H");
                    let _ = emit_iterm2_image(&mut out, img, cols, map_rows);
                } else {
                    let rs = image::imageops::resize(img, cols.max(10), map_rows * 2, FilterType::Triangle);
                    let art = render_halfblock(&rs);
                    let sv_lines: Vec<&str> = art.split("\r\n").collect();
                    let _ = write!(out, "\x1b[H");
                    for i in 0..map_rows as usize {
                        let ln = sv_lines.get(i).copied().unwrap_or("");
                        let _ = write!(out, "\x1b[{};1H{}\x1b[K", i + 1, ln);
                    }
                }
                let hd = ((heading % 360) + 360) % 360;
                let st = fit_cells(&format!(" 実写 h{hd}°  ←→向き ↑↓移動  Esc/q戻る  {slat:.4},{slon:.4} "), cols as usize);
                let _ = write!(out, "\x1b[{};1H\x1b[7m{st}\x1b[0m\x1b[K", tr);
                let _ = out.flush();
            }
            let (hd_c, slat_c, slon_c) = { let (_, h, la, lo) = street.as_ref().unwrap(); (*h, *la, *lo) };
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => {
                        // ←→=向き回転 / ↑↓=向き方向に前後移動(隣パノラマへスナップ)
                        let (nlat, nlon, nhd) = match k.code {
                            KeyCode::Left => (slat_c, slon_c, hd_c - 45),
                            KeyCode::Right => (slat_c, slon_c, hd_c + 45),
                            KeyCode::Up => { let (a, b) = streetview::step(slat_c, slon_c, hd_c as f64, 20.0); (a, b, hd_c) }
                            _ => { let (a, b) = streetview::step(slat_c, slon_c, hd_c as f64 + 180.0, 20.0); (a, b, hd_c) }
                        };
                        if let Ok(im) = streetview::fetch(nlat, nlon, nhd, 640, 480, &cfg.google_maps_api_key) {
                            street = Some((im, nhd, nlat, nlon)); // Err時は現状維持(行き止まり等)
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => street = None,
                    _ => {}
                }
            }
            continue;
        }
        // 標高プロファイル帯を出すぶん地図の行数を減らす(E)
        let elev_on = show_elev && !spec.routes.is_empty() && route_ele.len() >= 2 && route_ele.iter().any(|&z| z != 0.0);
        let elev_h: u32 = if elev_on { (map_rows / 3).clamp(4, 12) } else { 0 };
        let map_rows = if elev_h > 0 { map_rows.saturating_sub(elev_h + 1).max(3) } else { map_rows };
        let show_routes = matches!(focus, Focus::RouteList);
        let show_wps = matches!(focus, Focus::WaypointList);
        let show_route = matches!(focus, Focus::Map) && !wps.is_empty(); // Map中も地点一覧を左袖に常時表示
        let show_splist = matches!(focus, Focus::SpotList);
        let show_catlist = matches!(focus, Focus::SpotCatList);
        let show_settings = matches!(focus, Focus::Settings);
        let show_menu = matches!(focus, Focus::Menu(_));
        let show_poimenu = matches!(focus, Focus::PoiMenu);
        let show_roadlist = matches!(focus, Focus::RoadList);
        let gut: u32 = if !pois.is_empty() || show_routes || show_wps || show_route || show_splist || show_catlist || show_settings || show_menu || show_poimenu || show_roadlist { 28 } else { 0 };
        let map_cols = cols.saturating_sub(gut).max(10);
        let (ow, oh) = if opts.braille || opts.edge { (map_cols * 2, map_rows * 4) } else { (map_cols, map_rows * 2) };
        if let Some(p) = &gps_rx { // ライブ現在地を取り込み、自位置に追従
            while let Ok((la, lo)) = p.rx.try_recv() {
                gps_pos = Some((la, lo));
                gps_trail.push((la, lo));
                if gps_trail.len() > 300 { gps_trail.remove(0); }
                let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny;
            }
        }
        if play.is_some() { // ルート再生: 位置を進めて自動パン(全体を約20秒で走破)
            if let Some(rt) = spec.routes.last().map(|r| r.pts.clone()) {
                if rt.len() >= 2 {
                    let total = roadtrace::polyline_len(&rt);
                    let d = play.unwrap() + (total / 250.0).max(1.0);
                    if d >= total { play = None; addr = "再生: 終了".into(); }
                    else {
                        play = Some(d);
                        let (pla, plo) = roadtrace::point_at(&rt, d);
                        let (nx, ny) = deg_to_pixel(pla, plo, z); cx = nx; cy = ny;
                    }
                } else { play = None; }
            } else { play = None; }
        }
        let (lat, lon) = pixel_to_deg(cx, cy, z);
        let img_inline = cfg.image_mode && image_capable(); // 実画像モード(iTerm2系端末のみ)

        let mut map_img: Option<RgbImage> = None; // 実画像モードで描く overlay 合成済み画像
        let body = match build_window(cx, cy, z, ow, oh, &opts.style, &mut cache) {
            Ok(img) => {
                let mut ov = build_overlay(&spec, cx, cy, z, ow, oh, 1.0, 1.0, ow, oh);
                let (mx, my) = (ow as i32 / 2, oh as i32 / 2); // 中心クロスヘア(黄)
                draw_line(&mut ov, mx - 6, my, mx + 6, my, [255, 255, 0], 1);
                draw_line(&mut ov, mx, my - 6, mx, my + 6, [255, 255, 0], 1);
                if gps_pos.is_some() { // ライブ現在地: トレイル(薄青)+自位置(赤)
                    for (tla, tlo) in &gps_trail {
                        let (gx, gy) = deg_to_pixel(*tla, *tlo, z);
                        let ix = (gx - (cx - ow as f64 / 2.0)).floor() as i32;
                        let iy = (gy - (cy - oh as f64 / 2.0)).floor() as i32;
                        draw_ring(&mut ov, ix, iy, 1, [80, 160, 255], 1);
                    }
                    if let Some((gla, glo)) = gps_pos {
                        let (gx, gy) = deg_to_pixel(gla, glo, z);
                        let ix = (gx - (cx - ow as f64 / 2.0)).floor() as i32;
                        let iy = (gy - (cy - oh as f64 / 2.0)).floor() as i32;
                        draw_ring(&mut ov, ix, iy, 4, [255, 60, 60], 2);
                    }
                }
                if !wps.is_empty() { // 選択中(Tab)の waypoint を白丸で強調
                    let s = wp_sel.min(wps.len() - 1);
                    let (gx, gy) = deg_to_pixel(wps[s].0, wps[s].1, z);
                    let ix = (gx - (cx - ow as f64 / 2.0)).floor() as i32;
                    let iy = (gy - (cy - oh as f64 / 2.0)).floor() as i32;
                    draw_ring(&mut ov, ix, iy, 3, [255, 255, 255], 1);
                }
                if img_inline {
                    // 実画像モード: renderに渡すのと同じ画像に overlay を焼き込んで保持。AA文字列は空。
                    let mut c = img.clone();
                    composite(&mut c, &ov);
                    map_img = Some(c);
                    String::new()
                } else {
                    render(&img, &opts, Some(&ov))
                }
            }
            Err(e) => format!("取得失敗: {e}\r\n"),
        };

        // 左袖リスト(POI か お気に入り)の各行を組む
        let glines: Vec<String> = if gut > 0 {
            let gw = gut as usize;
            let (header, items, sel): (String, Vec<String>, usize) = if show_menu {
                match &focus {
                    // トップ: カテゴリだけ(キー列なし)。文字キー直打ちも効く旨は下部に出す。
                    Focus::Menu(MenuLevel::Categories) => {
                        let its = MENU_CATEGORIES.iter().map(|c| format!("  {}", c.label)).collect();
                        ("メニュー".to_string(), its, menu_cat_sel)
                    }
                    // 展開: 選んだカテゴリの項目のみ。ラベル左・キー右端揃え。
                    Focus::Menu(MenuLevel::Items(ci)) => {
                        let cat = &MENU_CATEGORIES[*ci];
                        let its = cat.items.iter().map(|it| menu_row(it.label, it.key, gw.saturating_sub(1))).collect();
                        (format!("← {}", cat.label), its, menu_item_sel)
                    }
                    _ => ("メニュー".to_string(), Vec::new(), 0),
                }
            } else if show_wps || show_route {
                let n = wps.len();
                let its = wps.iter().enumerate().map(|(i, (la, lo))| {
                    let role = if i == 0 { "始点" } else if i + 1 == n { "終点" } else { "経由" };
                    format!("#{} {} {:.3},{:.3}", i + 1, role, la, lo)
                }).collect();
                let hdr = if show_wps && grab { "並べ替え:掴".to_string() } else if show_wps { "並べ替え".to_string() } else { "ルート".to_string() };
                (hdr, its, wp_sel)
            } else if show_splist {
                let its = spots.iter().filter(|s| s.cat == cur_cat).map(|s| {
                    let nm = if s.name.is_empty() { "(無名)" } else { s.name.as_str() };
                    let d = haversine_km((lat, lon), (s.lat, s.lon)); // 現在地(中心)からの距離
                    format!("{} {:.1}k", nm, d)
                }).collect();
                (format!("{cur_cat}"), its, sp_sel)
            } else if show_catlist {
                let its = spot_cats.iter().map(|(n, _)| n.clone()).collect(); // 色は c で実色スウォッチから選ぶ(番号表示はやめた)
                ("カテゴリ".to_string(), its, cat_sel)
            } else if show_settings {
                let onoff = |b: bool| if b { "ON" } else { "OFF" };
                let keyset = if cfg.google_maps_api_key.trim().is_empty() { "未設定" } else { "設定済" };
                let mode_ja = match cfg.route_profile.as_str() { "car-fast" => "高速", "moped" => "下道", "shortest" => "最短", o => o };
                let model_ja = match cfg.llm_model.as_str() { "claude-sonnet-5" => "sonnet", "claude-haiku-4-5" => "haiku", "claude-opus-4-8" => "opus", o => o };
                let its = vec![
                    format!("点字ドット {}", onoff(opts.braille)),
                    format!("地物色分け {}", onoff(opts.classify)),
                    format!("輪郭抽出 {}", onoff(opts.edge)),
                    format!("単色 {}", onoff(opts.mono)),
                    format!("地図種別 {}", opts.style),
                    format!("既定ルート {}", mode_ja),
                    format!("道路の点間隔 {}m", cfg.sample_interval_m as i64),
                    format!("スポット既定表示 {}", onoff(cfg.show_spots)),
                    format!("おすすめ {}", onoff(cfg.llm_recommend_enabled)),
                    format!("提案AIモデル {}", model_ja),
                    format!("実写(StreetView) {}", onoff(cfg.streetview_enabled)),
                    format!("画像表示(iTerm2) {}", onoff(cfg.image_mode)),
                    format!("サウンド {}", onoff(cfg.sound_enabled)),
                    format!("Google APIキー {}", keyset),
                ];
                ("設定".to_string(), its, set_sel)
            } else if show_poimenu {
                let mut its: Vec<String> = POI_KINDS.iter().map(|k| k.label.to_string()).collect();
                its.push("キーワードで周辺検索".to_string());
                ("目的地".to_string(), its, poimenu_sel)
            } else if show_routes {
                ("お気に入り".to_string(), route_names.clone(), rn_sel)
            } else if show_roadlist {
                // 各行を塊マーカー │ + 道路名で。色はマップ側の別色で区別(gutterはfit_cells制約でANSI不可)
                let its = road_segs.iter().map(|r| format!("│ {}", if r.name.is_empty() { "(無名)" } else { r.name.as_str() })).collect();
                ("道路".to_string(), its, road_sel)
            } else {
                let its = pois.iter().map(|(la, lo, nm, _)| {
                    let d = haversine_km((lat, lon), (*la, *lo));
                    format!("{} {:.1}k", if nm.is_empty() { "(無名)" } else { nm }, d)
                }).collect();
                (poi_label.clone(), its, poi_sel)
            };
            // 見出し1行を除いた表示可能行数ぶんだけ、選択に追従してウィンドウ表示する
            let sel = sel.min(items.len().saturating_sub(1)); // sel が範囲外でも位置表示/添字を破綻させない
            let vh = (map_rows as usize).saturating_sub(1).max(1);
            ensure_visible(&mut list_offset, sel, items.len(), vh);
            let end = (list_offset + vh).min(items.len());
            let (more_up, more_dn) = (list_offset > 0, end < items.len());
            let mut gl = Vec::with_capacity(map_rows as usize);
            let hdr = if items.len() > vh {
                // 画面に収まらない時は 位置(sel+1/総数) と上下の続き矢印を出す
                format!("[{} {}/{}]{}{}", header, sel + 1, items.len(), if more_up { " ↑" } else { "" }, if more_dn { "↓" } else { "" })
            } else {
                format!("[{} {}]", header, items.len())
            };
            gl.push(fit_cells(&hdr, gw));
            for idx in list_offset..end {
                let cell = fit_cells(&format!("{}{}", if idx == sel { ">" } else { " " }, items[idx]), gw);
                gl.push(if idx == sel { format!("\x1b[7m{cell}\x1b[0m") } else { cell });
            }
            gl
        } else { Vec::new() };

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
        }
        if elev_h > 0 { // 標高プロファイル帯(地図の下・ステータスの上)
            let (mn, mx, _asc) = elevation::elevation_stats(&route_ele);
            let label = fit_cells(&format!(" 標高 ↑{route_ascend:.0}m  最高{mx:.0}m 最低{mn:.0}m  (Eで消す) "), cols as usize);
            let _ = write!(out, "\x1b[{};1H\x1b[7m{label}\x1b[0m\x1b[K", map_rows + 1);
            let chart = elevation::elevation_chart(&route_ele, cols as usize, elev_h as usize);
            for (i, line) in chart.iter().enumerate() {
                let _ = write!(out, "\x1b[{};1H{}\x1b[K", map_rows + 2 + i as u32, line);
            }
            // 地図中心が経路上のどこかを示す縦カーソル(パン/再生で動く)
            if let Some(rt) = spec.routes.last() {
                if rt.pts.len() >= 2 {
                    let (mut bi, mut bd) = (0usize, f64::MAX);
                    for (i, p) in rt.pts.iter().enumerate() {
                        let d = (p.0 - lat).powi(2) + (p.1 - lon).powi(2);
                        if d < bd { bd = d; bi = i; }
                    }
                    let col = elevation::profile_col(rt.pts.len(), bi, cols as usize);
                    for i in 0..elev_h as usize {
                        let _ = write!(out, "\x1b[{};{}H\x1b[1;31m|\x1b[0m", map_rows + 2 + i as u32, col + 1);
                    }
                }
            }
        }
        let status = match &focus {
            Focus::Search(_) => " 中央フォームに入力中 ".to_string(),
            Focus::SaveName(_) => " 中央フォームに入力中 ".to_string(),
            Focus::NearSearch(_) => " 中央フォームに入力中 ".to_string(),
            Focus::NewCat(_) => " 中央フォームに入力中 ".to_string(),
            Focus::SpotForm { .. } => " 新規スポット: ↑↓/Tab移動 入力/貼付 Enter=次/送信 Esc=取消 ".to_string(),
            Focus::SpotList if spot_move_confirm.is_some() => {
                let nm = spot_move_confirm.and_then(|gi| spots.get(gi)).map(|s| if s.name.is_empty() { "(無名)" } else { s.name.as_str() }).unwrap_or("");
                format!(" 「{nm}」をこの地図中心の位置へ移動する？ y=はい / 他キー=取消 ")
            }
            Focus::SpotList => format!(" [{cur_cat}] ↑↓ Enter移動 [ ]並替 n新規 r改名 m中心へ x削除 Esc戻る "),
            Focus::SpotEditName(_, _) => " 中央フォームに入力中 ".to_string(),
            Focus::SpotCatList if pending_spot.is_some() => " 登録先カテゴリを選択: ↑↓ Enter=ここに登録 n新規 Esc取消 ".to_string(),
            Focus::SpotCatList => " カテゴリ: ↑↓選択 [ ]並替 Enter=中へ n新規 r改名 c色 x削除(空のみ) Esc=閉 ".to_string(),
            Focus::Settings => {
                let desc = match set_sel {
                    0 => "braille: 点字ドットで高精細描画(色は淡め)。OFFはハーフブロック",
                    1 => "classify: 地物を色分け(水域/緑地/道路/建物)。地形が見やすい",
                    2 => "edge: 輪郭抽出表示(線画風)",
                    3 => "mono: 単色描画(色を使わない)",
                    4 => "style: タイル種別を循環(osm=標準/voyager/dark=暗/light=淡)",
                    5 => "既定mode: 起動時のルート種別。car-fast=高速優先 / moped=下道(高速回避) / shortest=最短距離",
                    6 => "道路の点間隔: rの道路名ルートで、その道を何mおきの点でなぞるか(小=忠実で点多/大=粗い)。←→で調整",
                    7 => "spot既定: 起動時にお気に入りスポットを表示するか",
                    8 => "おすすめ: claude -p でツーリングスポットを提案する機能のON/OFF(未実装)",
                    9 => "LLM: おすすめに使うモデルを循環(claude-sonnet-5/haiku/opus)",
                    10 => "実写: iで中心地点のStreet Viewを開く機能のON/OFF(要Google APIキー)",
                    11 => if image_capable() { "画像表示: 地図と実写をiTerm2インライン画像で実画像表示(AAでなく実画像)。Iキーでも切替" } else { "画像表示: この端末は画像非対応(iTerm2/WezTermで有効)" },
                    12 => "サウンド: 操作音のON/OFF(macOSのafplayで再生)。切替は次回起動から確実に反映",
                    _ => "Google APIキー: 検索(Geocoding)とStreet View共通。この行でCmd+V貼付→設定、sで保存。環境変数TERMMAP_GOOGLE_API_KEYでも可",
                };
                format!(" ▶ {desc}   [↑↓選択 Enter切替 s保存 Esc閉]")
            }
            Focus::RoadSearch(_) => " 中央フォームに入力中 ".to_string(),
            Focus::Recommend(_) => " 中央フォームに入力中 ".to_string(),
            Focus::SpotRename(_, _) => " 中央フォームに入力中 ".to_string(),
            Focus::PoiMenu => " 目的地カテゴリ: ↑↓選択 Enter=検索 (数字1-7も可 / キーワードは最終行かEnter) Esc=取消 ".to_string(),
            Focus::PoiList => format!(" [{}] ↑↓選択(地図追従) ←→地図 v=地点追加 Enter移動 P登録 f再検索 Esc閉 ", poi_label),
            Focus::RouteList => " お気に入り: ↑↓選択 Enter=読込 Esc=閉 ".to_string(),
            Focus::RoadList => " 道路: ↑↓選択 x削除 Esc戻る ".to_string(),
            Focus::WaypointList => " 並べ替え: ↑↓/ws選択(地図追従)  Space掴む↔置く(掴み中↑↓/wsで移動)  x削除  +/-拡縮  Esc閉 ".to_string(),
            Focus::ColorPick { .. } => " 色を選択: ←→ Enter=決定 Esc=取消 ".to_string(),
            Focus::Menu(MenuLevel::Categories) => " ↑↓カテゴリ Enter展開 / 文字キーで直接実行 Esc閉 ".to_string(),
            Focus::Menu(MenuLevel::Items(_)) => " ↑↓選択 Enter実行 / 右端キーでも実行 Esc戻る ".to_string(),
            Focus::Map => {
                // 通信中(いずれかのジョブがSome)はスピナー1文字＋案内を先頭に出す
                let jobs_active = route_job.is_some() || search_job.is_some() || near_job.is_some() || street_job.is_some() || recommend_job.is_some();
                let spinner = if jobs_active {
                    const FR: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                    format!("{} 通信中…(Escで中断) ", FR[spin % FR.len()])
                } else { String::new() };
                let live = if gps_rx.is_some() { "●LIVE(Gで解除) " } else { "" };
                let playing = if play.is_some() { "▶再生中(Aで停止) " } else { "" };
                // 一時メッセージが無い時は底面にロゴを常時表示。メッセージ発生時はそちらを優先。
                let msg = if addr.is_empty() { "◉╌╌╌► termmap · terminal touring map   ".to_string() } else { format!("» {addr} « ") };
                // 下部バーは細く。全操作は Space メニューから選べる
                let route_hint = if wps.is_empty() { "v=地点を置く".to_string() } else { format!("{}点 v足す Tab/ws選択 [ ]動 x消", wps.len()) };
                let base = format!(" {spinner}{msg}{live}{playing}z{z} {lat:.4},{lon:.4} ｜ {route_hint} ｜ Space:メニュー ?ヘルプ q終了");
                match &route_note { Some(rn) => format!("{base} | {rn} "), None => base }
            }
        };
        let status = fit_cells(&status, cols as usize);
        write!(out, "\x1b[{};1H\x1b[7m{status}\x1b[0m", tr)?;

        if let Some(msg) = &popup { // 中央に名前ポップアップ(任意キーで閉じる)
            let text = format!("  {}  ", msg);
            let w = text.chars().count();
            let c0 = ((cols as usize).saturating_sub(w) / 2).max(1);
            let r0 = (map_rows / 2).max(1);
            let pad = " ".repeat(w);
            let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0, c0, pad);
            let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0 + 1, c0, text);
            let _ = write!(out, "\x1b[{};{}H\x1b[30;47m{}\x1b[0m", r0 + 2, c0, pad);
        }

        // QR共有ポップアップ(地図の上に白地で重ねる。白地×黒でどのテーマでもスキャン可)
        if let Some(q) = &qr_view {
            let lines: Vec<&str> = q.lines().collect();
            let qw = lines.iter().map(|l| l.chars().count()).max().unwrap_or(21);
            let padx = 2usize; // 左右の白余白(quiet zone)
            let bw = qw + padx * 2;
            let c0 = ((cols as usize).saturating_sub(bw) / 2).max(1) as u32;
            // 行構成: ラベル / 上白余白×2 / QR / 下白余白×2
            let total = lines.len() + 5;
            let r0 = ((map_rows as usize).saturating_sub(total) / 2).max(1) as u32;
            let hpad = " ".repeat(bw);
            let side = " ".repeat(padx);
            // ラベルを箱幅で中央寄せ
            let label = "スマホでスキャン → Googleマップ";
            let lw: usize = label.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum();
            let lc = c0 + (bw.saturating_sub(lw) / 2) as u32;
            let _ = write!(out, "\x1b[{r0};{lc}H\x1b[1m{label}\x1b[0m");
            // 純白の箱(bright white 107 + black 30)。上下2行の白余白でquiet zone確保
            for k in 0..2 { let _ = write!(out, "\x1b[{};{c0}H\x1b[30;107m{hpad}\x1b[0m", r0 + 1 + k); }
            for (i, l) in lines.iter().enumerate() {
                let _ = write!(out, "\x1b[{};{c0}H\x1b[30;107m{side}{l:<qw$}{side}\x1b[0m", r0 + 3 + i as u32, qw = qw);
            }
            for k in 0..2 { let _ = write!(out, "\x1b[{};{c0}H\x1b[30;107m{hpad}\x1b[0m", r0 + 3 + lines.len() as u32 + k); }
            let _ = write!(out, "\x1b[{};1H\x1b[7m 任意のキーで閉じる \x1b[0m\x1b[K", tr);
        }

        // 新規スポット登録フォーム(中央ボックス。qr_view/popup と同じ中央重畳手法)
        if let Focus::SpotForm { name, url, field } = &focus {
            const BG: &str = "\x1b[30;47m";   // 黒字・白地(ボックス地)
            const SEL: &str = "\x1b[97;40m";  // 白字・黒地(選択中フィールドを反転表示)
            const RST: &str = "\x1b[0m";
            let iw = (cols as usize).saturating_sub(6).clamp(24, 60); // ボックス内容幅
            // 選択中の入力欄は cur 位置にカーソルを出す。非選択欄はそのまま表示。
            let name_disp = if *field == 0 { render_with_cursor(name, input_cur) } else { name.clone() };
            let url_disp = if *field == 1 { render_with_cursor(url, input_cur) } else { url.clone() };
            let header = format!("  新規スポット [{cur_cat}]");
            let name_line = format!("  名称: {}", name_disp);
            let url_line = format!("  GoogleマップURL(任意): {}", url_disp);
            let blank = " ".repeat(iw);
            // 行の並び(内容, その行が選択中フィールドか)
            let rows: [(String, bool); 6] = [
                (blank.clone(), false),
                (fit_cells(&header, iw), false),
                (fit_cells(&name_line, iw), *field == 0),
                (fit_cells(&url_line, iw), *field == 1),
                (blank.clone(), false),
                (blank.clone(), false),
            ];
            // ボタン行([送信]/[戻る] を明示セグメントで組む。各6セル+前後余白)
            let mut btn = String::new();
            btn.push_str(BG); btn.push_str("  ");
            btn.push_str(if *field == 2 { SEL } else { BG }); btn.push_str("[送信]");
            btn.push_str(BG); btn.push_str("  ");
            btn.push_str(if *field == 3 { SEL } else { BG }); btn.push_str("[戻る]");
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
        // 単一テキスト入力は地図中央のフォームで受ける(底面バーで完結させない)
        match &focus {
            Focus::Search(b) => draw_input_panel(&mut out, cols, map_rows, "地名・住所で検索", "Enter=検索  Esc=取消  (住所も入力OK)", b, input_cur),
            Focus::SaveName(b) => draw_input_panel(&mut out, cols, map_rows, "ルートに名前を付けて保存", "Enter=保存  Esc=取消", b, input_cur),
            Focus::NearSearch(b) => draw_input_panel(&mut out, cols, map_rows, "このあたりでキーワード検索", "Enter=検索  Esc=取消", b, input_cur),
            Focus::NewCat(b) => draw_input_panel(&mut out, cols, map_rows, "新しいカテゴリ名", "Enter=作成  Esc=取消", b, input_cur),
            Focus::RoadSearch(b) => draw_input_panel(&mut out, cols, map_rows, "道路名・国道番号でルートに追加", "Enter=view内を追加(複数可)  Esc=取消", b, input_cur),
            Focus::Recommend(b) => draw_input_panel(&mut out, cols, map_rows, "おすすめの方向性 (例: 海沿い / 峠)", "Enter=提案(数秒)  Esc=取消", b, input_cur),
            Focus::SpotRename(b, _) => draw_input_panel(&mut out, cols, map_rows, "カテゴリ名を変更", "Enter=確定  Esc=取消", b, input_cur),
            Focus::SpotEditName(b, _) => draw_input_panel(&mut out, cols, map_rows, "スポット名を変更", "Enter=確定  Esc=取消", b, input_cur),
            _ => {}
        }
        // 色ピッカー(中央パネル・実色スウォッチ)。選択中は [ ] で囲む
        if let Focus::ColorPick { .. } = &focus {
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
        // 初回起動の操作案内(中央パネル・何かキーで消える)
        if onboard {
            const BG: &str = "\x1b[97;44m"; // 白字・青地(目立たせる)
            const RST: &str = "\x1b[0m";
            let iw = 34usize;
            let lines = [
                "",
                "   ╺┳╸┏━╸┏━┓┏┳┓┏┳┓┏━┓┏━┓",
                "    ┃ ┣╸ ┣┳┛┃┃┃┃┃┃┣━┫┣━┛",
                "    ╹ ┗━╸╹┗╸╹╹╹╹╹╹╹ ╹╹",
                "   terminal touring map",
                "",
                "  Space   メニューを開く",
                "  ?       ヘルプ",
                "  q       終了",
                "",
                "  何かキーを押して開始",
            ];
            let r0 = ((map_rows as usize).saturating_sub(lines.len()) / 2).max(1) as u32;
            let c0 = ((cols as usize).saturating_sub(iw) / 2).max(1) as u32;
            for (i, ln) in lines.iter().enumerate() {
                let _ = write!(out, "\x1b[{};{}H{}{}{}", r0 + i as u32, c0, BG, fit_cells(ln, iw), RST);
            }
        }
        out.flush()?;

        // バックグラウンドジョブの結果を毎フレーム取り込む(route/search/near/street/recommend)。
        // Ok=適用しjob=None / Empty=保持 / Disconnected=None。結果を適用したフレームはブロックせず即再描画する。
        use std::sync::mpsc::TryRecvError;
        let mut got_result = false;
        if route_job.is_some() {
            match route_job.as_ref().unwrap().try_recv() {
                Ok(Ok(r)) => {
                    spec.routes.clear();
                    route_note = Some(route_summary(&mode, &r));
                    route_ele = r.ele;
                    route_ascend = r.ascend_m;
                    spec.routes.push(Route { pts: r.pts, color: [0, 220, 255], thickness: 2 });
                    route_job = None; got_result = true;
                }
                Ok(Err(e)) => { route_note = Some(format!("({e})")); route_job = None; got_result = true; }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { route_job = None; got_result = true; }
            }
        }
        if search_job.is_some() {
            match search_job.as_ref().unwrap().try_recv() {
                Ok((ckey, q, res)) => {
                    match res {
                        Err(e) => { snd.play("error"); addr = format!("検索できません（{e}）"); }
                        Ok(v) if v.is_empty() => { snd.play("error"); addr = format!("見つからない: {q}"); }
                        Ok(v) => {
                            let now = searchcache::now_secs();
                            scache.insert(ckey, searchcache::CacheEntry { results: v.clone(), created_at: now, last_used_at: now });
                            let _ = searchcache::save(&scache);
                            pois = v.into_iter().take(8).map(|(la, lo, nm)| (la, lo, nm, PoiCat::Waypoint)).collect();
                            poi_sel = 0;
                            poi_label = format!("検索:{q}");
                            set_markers(&mut spec, &wps, &pois);
                            if matches!(focus, Focus::Map) { focus = Focus::PoiList; } // 別画面へ移っていたら奪わない
                        }
                    }
                    search_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { search_job = None; got_result = true; }
            }
        }
        if near_job.is_some() {
            match near_job.as_ref().unwrap().try_recv() {
                Ok((q, osm)) => {
                    // ローカルの★スポット一致(距離順)を先頭、Overpass結果(距離順)を後ろにマージ
                    let ql = q.to_lowercase();
                    let mut mine: Vec<(f64, f64, String, PoiCat)> = spots.iter()
                        .filter(|s| s.name.to_lowercase().contains(&ql))
                        .map(|s| (s.lat, s.lon, format!("★{}", s.name), PoiCat::Home)).collect();
                    mine.sort_by(|p, r| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (r.0, r.1))).unwrap_or(std::cmp::Ordering::Equal));
                    let mut got: Vec<(f64, f64, String, PoiCat)> = osm.into_iter().map(|(a, b, nm)| (a, b, nm, PoiCat::Other)).collect();
                    got.sort_by(|p, r| haversine_km((lat, lon), (p.0, p.1)).partial_cmp(&haversine_km((lat, lon), (r.0, r.1))).unwrap_or(std::cmp::Ordering::Equal));
                    mine.extend(got);
                    if mine.is_empty() { snd.play("error"); addr = format!("周辺に無し: {q}"); }
                    else {
                        pois = mine; poi_sel = 0; poi_label = format!("周辺:{q}");
                        set_markers(&mut spec, &wps, &pois);
                        if matches!(focus, Focus::Map) { focus = Focus::PoiList; }
                    }
                    near_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { near_job = None; got_result = true; }
            }
        }
        if street_job.is_some() {
            match street_job.as_ref().unwrap().try_recv() {
                Ok((la, lo, hd, res)) => {
                    match res {
                        Ok(img) => { street = Some((img, hd, la, lo)); addr.clear(); }
                        Err(e) => addr = format!("実写: {e}"),
                    }
                    street_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { street_job = None; got_result = true; }
            }
        }
        if recommend_job.is_some() {
            match recommend_job.as_ref().unwrap().try_recv() {
                Ok(res) => {
                    match res {
                        Ok(v) if v.is_empty() => addr = "おすすめ: 実在確認できる地点なし".into(),
                        Ok(v) => {
                            pois = v.into_iter().map(|(la, lo, nm)| (la, lo, nm, PoiCat::Home)).collect();
                            poi_sel = 0; poi_label = "おすすめ".into();
                            set_markers(&mut spec, &wps, &pois);
                            if matches!(focus, Focus::Map) { focus = Focus::PoiList; }
                        }
                        Err(e) => addr = format!("おすすめ: {e}"),
                    }
                    recommend_job = None; got_result = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => { recommend_job = None; got_result = true; }
            }
        }

        // 入力待ち。結果適用直後は即再描画(None)。ジョブ/GPS/再生いずれか進行中はポーリング。
        let polling = route_job.is_some() || search_job.is_some() || near_job.is_some() || street_job.is_some() || recommend_job.is_some() || gps_rx.is_some() || play.is_some();
        let ev: Option<Event> = if got_result {
            None
        } else if polling {
            if event::poll(std::time::Duration::from_millis(80))? { Some(event::read()?) } else { None }
        } else {
            Some(event::read()?)
        };
        match ev {
            None => {} // 再描画のみ(計算待ち)
            Some(Event::Key(k)) if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl-C: 進行中の全ジョブを中断(アプリは終了しない)
                let any = route_job.is_some() || search_job.is_some() || near_job.is_some() || street_job.is_some() || recommend_job.is_some();
                if any {
                    if route_job.is_some() { route_note = Some("中断".to_string()); }
                    route_job = None; search_job = None; near_job = None; street_job = None; recommend_job = None;
                    addr = "中断".into();
                }
            }
            Some(Event::Key(_)) if onboard => { // 初回案内を最初のキーで閉じ、既読マーカーを書く(以後出さない)
                onboard = false;
                if let Some(p) = onboarded_marker() { let _ = crate::fsutil::write_atomic(&p, b"1", None); }
            }
            Some(Event::Key(_)) if qr_view.is_some() => qr_view = None, // ポップアップを閉じる
            Some(Event::Key(_)) if popup.is_some() => popup = None, // 名前ポップアップを閉じる
            Some(Event::Key(k)) if spot_move_confirm.is_some() => { // 「中心へ移動」の確認(y=実行/他=取消)
                let gi = spot_move_confirm.take().unwrap();
                if let KeyCode::Char('y') = k.code {
                    snd.play("confirm");
                    if let Some(s) = spots.get_mut(gi) { s.lat = lat; s.lon = lon; }
                    let _ = save_all_spots(&spots); apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                    addr = "スポット位置を中心へ移動".into();
                } else { addr = "移動を取消".into(); }
            }
            // Map表示中のEscは進行中ジョブの中断に使う(サブ画面のEscは各Focusの取消のまま)
            Some(Event::Key(k)) if k.code == KeyCode::Esc && matches!(focus, Focus::Map)
                && (route_job.is_some() || search_job.is_some() || near_job.is_some() || street_job.is_some() || recommend_job.is_some()) => {
                if route_job.is_some() { route_note = Some("中断".to_string()); }
                route_job = None; search_job = None; near_job = None; street_job = None; recommend_job = None;
                addr = "中断".into();
            }
            Some(Event::Key(k)) => {
                let cur = std::mem::replace(&mut focus, Focus::Map);
                match cur {
                    Focus::Search(mut buf) => match k.code {
                        KeyCode::Enter => { // 候補を一覧表示(左袖)。Enterで移動/s e vで経路点
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                // provider は Google キーの有無で分ける(キーあり=Google優先"g"/無し=Nominatim"n")。言語は ja 固定。
                                let provider = if cfg.google_maps_api_key.trim().is_empty() { "n" } else { "g" };
                                let ckey = searchcache::make_key(provider, "ja", &q, lat, lon);
                                // キャッシュヒットは即適用(同期)。ミス時のみ別スレッドで検索(通信/サーバ障害は0件と区別)。
                                // ヒット時は last_used を更新(LRU破棄の基準。次回 save 時に永続化される)。
                                let hit = scache.get_mut(&ckey).map(|e| { e.last_used_at = searchcache::now_secs(); e.results.clone() });
                                if let Some(v) = hit {
                                    if v.is_empty() { snd.play("error"); addr = format!("見つからない: {q}"); }
                                    else {
                                        pois = v.into_iter().take(8).map(|(la, lo, nm)| (la, lo, nm, PoiCat::Waypoint)).collect();
                                        poi_sel = 0;
                                        poi_label = format!("検索:{q}");
                                        set_markers(&mut spec, &wps, &pois);
                                        focus = Focus::PoiList;
                                    }
                                } else {
                                    let q2 = q.clone(); let ckey2 = ckey.clone();
                                    let key = cfg.google_maps_api_key.clone();
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    std::thread::spawn(move || {
                                        let r = geocode_list(&q2, Some((lat, lon)), &key).map_err(|e| e.to_string());
                                        let _ = tx.send((ckey2, q2, r));
                                    });
                                    search_job = Some(rx);
                                    focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                                }
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::Search(buf); } // ←→/文字/BS/Del/Home/End
                    },
                    Focus::SpotCatList => match k.code { // カテゴリ一覧(P)
                        KeyCode::Up => { cat_sel = cat_sel.saturating_sub(1); focus = Focus::SpotCatList; }
                        KeyCode::Down => { if cat_sel + 1 < spot_cats.len() { cat_sel += 1; } focus = Focus::SpotCatList; }
                        KeyCode::Char('n') => { input_cur = 0; focus = Focus::NewCat(String::new()); }
                        KeyCode::Char('[') => { // 選択カテゴリを上へ
                            if cat_sel > 0 && cat_sel < spot_cats.len() { spot_cats.swap(cat_sel, cat_sel - 1); cat_sel -= 1; let _ = save_all_cats(&spot_cats); }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Char(']') => { // 選択カテゴリを下へ
                            if cat_sel + 1 < spot_cats.len() { spot_cats.swap(cat_sel, cat_sel + 1); cat_sel += 1; let _ = save_all_cats(&spot_cats); }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Char('r') => { if let Some((n, _)) = spot_cats.get(cat_sel) { input_cur = n.chars().count(); focus = Focus::SpotRename(n.clone(), cat_sel); } else { focus = Focus::SpotCatList; } }
                        KeyCode::Char('c') => {
                            match spot_cats.get(cat_sel) {
                                Some((_, ci)) => { color_sel = *ci; focus = Focus::ColorPick { cat: cat_sel }; }
                                None => focus = Focus::SpotCatList,
                            }
                        }
                        KeyCode::Char('x') => {
                            if let Some((name, _)) = spot_cats.get(cat_sel).cloned() {
                                if spots.iter().any(|s| s.cat == name) { addr = format!("使用中: {name}(先に空に)"); }
                                else { spot_cats.remove(cat_sel); if cat_sel >= spot_cats.len() && cat_sel > 0 { cat_sel -= 1; } let _ = save_all_cats(&spot_cats); }
                            }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Enter => {
                            let cat = spot_cats.get(cat_sel).map(|(c, _)| c.clone());
                            if let Some((la, lo, nm)) = pending_spot.take() {
                                // 検索結果からの登録: 選択カテゴリに新規スポットとして保存
                                if let Some(cat) = cat {
                                    snd.play("pop");
                                    let s = Spot { lat: la, lon: lo, cat: cat.clone(), name: spot_clean(&nm) };
                                    let _ = append_spot(&s);
                                    spots.push(s);
                                    show_spots = true;
                                    apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                                    addr = format!("★登録: {} [{}]", if nm.is_empty() { "(無名)" } else { nm.as_str() }, cat);
                                }
                                focus = Focus::Map;
                            } else if let Some(cat) = cat {
                                cur_cat = cat; sp_sel = 0; focus = Focus::SpotList;
                            } else { focus = Focus::SpotCatList; }
                        }
                        KeyCode::Esc => { snd.play("back"); pending_spot = None; } // 登録キャンセル時も保留を消す→Mapへ
                        _ => focus = Focus::SpotCatList,
                    },
                    Focus::Settings => { let mut stay = true; match k.code { // 設定画面
                        KeyCode::Up => { set_sel = set_sel.saturating_sub(1); }
                        KeyCode::Down => { if set_sel + 1 < 14 { set_sel += 1; } }
                        KeyCode::Left | KeyCode::Right => {
                            if set_sel == 6 { let d = if k.code == KeyCode::Left { -100.0 } else { 100.0 }; cfg.sample_interval_m = (cfg.sample_interval_m + d).clamp(100.0, 5000.0); }
                        }
                        KeyCode::Enter | KeyCode::Char(' ') => match set_sel {
                            0 => opts.braille = !opts.braille,
                            1 => opts.classify = !opts.classify,
                            2 => opts.edge = !opts.edge,
                            3 => opts.mono = !opts.mono,
                            4 => { opts.style = match opts.style.as_str() { "osm" => "voyager", "voyager" => "dark", "dark" => "light", _ => "osm" }.to_string(); cache.clear(); }
                            5 => cfg.route_profile = match cfg.route_profile.as_str() { "car-fast" => "moped", "moped" => "shortest", _ => "car-fast" }.to_string(),
                            6 => {} // ←→で調整
                            7 => { cfg.show_spots = !cfg.show_spots; show_spots = cfg.show_spots; apply_spots(&mut spec, &spots, &spot_cats, show_spots); }
                            8 => cfg.llm_recommend_enabled = !cfg.llm_recommend_enabled,
                            9 => cfg.llm_model = match cfg.llm_model.as_str() { "claude-sonnet-5" => "claude-haiku-4-5", "claude-haiku-4-5" => "claude-opus-4-8", _ => "claude-sonnet-5" }.to_string(),
                            10 => cfg.streetview_enabled = !cfg.streetview_enabled,
                            11 => cfg.image_mode = !cfg.image_mode,
                            12 => { cfg.sound_enabled = !cfg.sound_enabled; snd = sound::Sound::new(cfg.sound_enabled); snd.play("confirm"); }
                            _ => addr = "APIキー: この行で貼り付け(Cmd+V)して設定".into(),
                        },
                        KeyCode::Char('s') => {
                            cfg.braille = opts.braille; cfg.classify = opts.classify; cfg.edge = opts.edge; cfg.mono = opts.mono; cfg.style = opts.style.clone();
                            addr = match config::save_config(&cfg) { Ok(_) => "設定を保存(config.toml)".into(), Err(e) => format!("保存失敗: {e}") };
                        }
                        KeyCode::Esc => { snd.play("back"); stay = false; }
                        _ => {}
                    } if stay { focus = Focus::Settings; } },
                    Focus::RoadSearch(mut buf) => match k.code { // 道路名/ref で現在view内をルート化
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            if !name.is_empty() {
                                let (n_lat, w_lon) = pixel_to_deg(cx - ow as f64 / 2.0, cy - oh as f64 / 2.0, z);
                                let (s_lat, e_lon) = pixel_to_deg(cx + ow as f64 / 2.0, cy + oh as f64 / 2.0, z);
                                show_busy(&mut out, cols, tr, "道路検索中…");
                                match roadsearch::fetch(&name, s_lat, w_lon, n_lat, e_lon) {
                                    Ok(frags) if !frags.is_empty() => {
                                        let rf: Vec<roadtrace::RoadFrag> = frags.into_iter().map(|(pts, oneway)| roadtrace::RoadFrag { pts, oneway }).collect();
                                        let poly = roadtrace::assemble_polyline(&rf);
                                        // view中心に近い連結成分だけを塊として残す(大ジャンプで繋がった飛び地を捨てる)
                                        let seg = roadtrace::nearest_segment(&poly, (lat, lon), 500.0);
                                        if seg.len() >= 2 {
                                            // BRouterでは縫わず(wpsには入れない)、別色レイヤの塊として保持する
                                            let color = ROAD_PALETTE[road_segs.len() % ROAD_PALETTE.len()];
                                            road_segs.push(RoadSeg { name: name.clone(), color, pts: seg });
                                            sync_roads!();
                                            addr = format!("道路: {name} を塊で追加(計{}本)", road_segs.len());
                                        } else { addr = "道路: 点が足りない(拡大/移動して再検索)".into(); }
                                    }
                                    Ok(_) => addr = format!("道路が見つからない: {name}(view内に無い)"),
                                    Err(e) => addr = format!("道路: {e}"),
                                }
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::RoadSearch(buf); }
                    },
                    Focus::Recommend(mut buf) => match k.code { // おすすめ: 方向性→claude -p→実在確認→候補一覧
                        KeyCode::Enter => {
                            let dir = buf.trim().to_string();
                            if !dir.is_empty() {
                                // AI提案→実在確認(geocode)ループを別スレッドで回し、検証済みスポット列を返す。
                                let cmd = cfg.llm_command.clone();
                                let model = cfg.llm_model.clone();
                                let key = cfg.google_maps_api_key.clone();
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
                                recommend_job = Some(rx);
                                focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::Recommend(buf); }
                    },
                    Focus::SpotList => match k.code { // cur_cat のスポット一覧
                        KeyCode::Up => { sp_sel = sp_sel.saturating_sub(1); focus = Focus::SpotList; }
                        KeyCode::Down => { let n = spots.iter().filter(|s| s.cat == cur_cat).count(); if sp_sel + 1 < n { sp_sel += 1; } focus = Focus::SpotList; }
                        KeyCode::Char('n') => { input_cur = 0; focus = Focus::SpotForm { name: String::new(), url: String::new(), field: 0 }; } // 新規スポット登録フォーム
                        KeyCode::Char('[') => { // 選択スポットを同カテゴリ内で上へ
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if sp_sel > 0 && sp_sel < idxs.len() { spots.swap(idxs[sp_sel], idxs[sp_sel - 1]); sp_sel -= 1; let _ = save_all_spots(&spots); }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Char(']') => { // 選択スポットを同カテゴリ内で下へ
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if sp_sel + 1 < idxs.len() { spots.swap(idxs[sp_sel], idxs[sp_sel + 1]); sp_sel += 1; let _ = save_all_spots(&spots); }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Char('r') => { // 選択スポットを改名
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            match idxs.get(sp_sel) { Some(&gi) => { input_cur = spots[gi].name.chars().count(); focus = Focus::SpotEditName(spots[gi].name.clone(), gi); } None => focus = Focus::SpotList }
                        }
                        KeyCode::Char('m') => { // 選択スポットを現在の中心へ移動(破壊的なので確認待ちにするだけ)
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(sp_sel) { spot_move_confirm = Some(gi); }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Enter => {
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(sp_sel) { let (nx, ny) = deg_to_pixel(spots[gi].lat, spots[gi].lon, z); cx = nx; cy = ny; }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Char('x') => {
                            let idxs: Vec<usize> = spots.iter().enumerate().filter(|(_, s)| s.cat == cur_cat).map(|(i, _)| i).collect();
                            if let Some(&gi) = idxs.get(sp_sel) {
                                spots.remove(gi);
                                if sp_sel > 0 && sp_sel >= idxs.len() - 1 { sp_sel -= 1; }
                                let _ = save_all_spots(&spots);
                                apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                            }
                            focus = Focus::SpotList;
                        }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::SpotCatList; }
                        _ => focus = Focus::SpotList,
                    },
                    Focus::SpotEditName(mut buf, gi) => match k.code { // スポット改名
                        KeyCode::Enter => {
                            snd.play("confirm");
                            let new = spot_clean(buf.trim());
                            if let Some(s) = spots.get_mut(gi) { s.name = new; }
                            let _ = save_all_spots(&spots);
                            apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                            focus = Focus::SpotList;
                        }
                        KeyCode::Esc => focus = Focus::SpotList,
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::SpotEditName(buf, gi); }
                    },
                    Focus::NewCat(mut buf) => match k.code {
                        KeyCode::Enter => { let name = buf.trim().to_string(); if !name.is_empty() { snd.play("confirm"); let _ = ensure_spot_cat(&name, &mut spot_cats); } focus = Focus::SpotCatList; }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::SpotCatList; }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::NewCat(buf); }
                    },
                    Focus::SpotRename(mut buf, idx) => match k.code {
                        KeyCode::Enter => {
                            let new = spot_clean(buf.trim());
                            if !new.is_empty() {
                                if let Some(old) = spot_cats.get(idx).map(|(n, _)| n.clone()) {
                                    for s in spots.iter_mut() { if s.cat == old { s.cat = new.clone(); } }
                                    if let Some(e) = spot_cats.get_mut(idx) { e.0 = new; }
                                    let _ = save_all_spots(&spots);
                                    let _ = save_all_cats(&spot_cats);
                                    apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                                }
                            }
                            focus = Focus::SpotCatList;
                        }
                        KeyCode::Esc => focus = Focus::SpotCatList,
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::SpotRename(buf, idx); }
                    },
                    Focus::SpotForm { mut name, mut url, mut field } => match k.code { // 新規スポット登録フォーム
                        KeyCode::Up | KeyCode::BackTab => { field = (field + 3) % 4; input_cur = form_cur(&name, &url, field); focus = Focus::SpotForm { name, url, field }; }
                        KeyCode::Down | KeyCode::Tab => { field = (field + 1) % 4; input_cur = form_cur(&name, &url, field); focus = Focus::SpotForm { name, url, field }; }
                        KeyCode::Esc => { snd.play("back"); focus = Focus::SpotList; } // 取消
                        KeyCode::Enter => match field {
                            0 => { field = 1; input_cur = url.chars().count(); focus = Focus::SpotForm { name, url, field }; } // 次のフィールドへ
                            1 => { field = 2; input_cur = 0; focus = Focus::SpotForm { name, url, field }; }
                            3 => focus = Focus::SpotList, // [戻る]
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
                                        snd.play("confirm");
                                        let s = Spot { lat: la, lon: lo, cat: cur_cat.clone(), name: nm };
                                        let _ = ensure_spot_cat(&s.cat, &mut spot_cats);
                                        addr = match append_spot(&s) { Ok(_) => format!("スポット保存: {}", s.name), Err(e) => format!("({e})") };
                                        spots.push(s); show_spots = true; apply_spots(&mut spec, &spots, &spot_cats, show_spots);
                                        focus = Focus::SpotList;
                                    }
                                    Act::Err(msg) => { addr = msg; focus = Focus::SpotForm { name, url, field }; }
                                    Act::Nop => focus = Focus::SpotForm { name, url, field },
                                }
                            }
                        },
                        other => { // ←→/文字/BS/Del/Home/End は選択中フィールドを編集(ボタン欄では無視)
                            if field == 0 { edit_line(&mut name, &mut input_cur, other); }
                            else if field == 1 { edit_line(&mut url, &mut input_cur, other); }
                            focus = Focus::SpotForm { name, url, field };
                        }
                    },
                    Focus::NearSearch(mut buf) => match k.code {
                        KeyCode::Enter => {
                            let q = buf.trim().to_string();
                            if !q.is_empty() {
                                // Overpass(遅い)を別スレッドへ。viewbox境界を先に確定して渡す。★マージは結果適用側で行う。
                                let (vt, vl) = pixel_to_deg(cx - ow as f64 * 1.25, cy - oh as f64 * 1.25, z);
                                let (vb, vr) = pixel_to_deg(cx + ow as f64 * 1.25, cy + oh as f64 * 1.25, z);
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
                                near_job = Some(rx);
                                focus = Focus::Map; // UIは生きたまま(スピナー表示・Escで中断)
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::NearSearch(buf); }
                    },
                    Focus::PoiMenu => match k.code {
                        KeyCode::Esc => {}
                        KeyCode::Up => { poimenu_sel = poimenu_sel.saturating_sub(1); focus = Focus::PoiMenu; }
                        KeyCode::Down => { if poimenu_sel + 1 <= POI_KINDS.len() { poimenu_sel += 1; } focus = Focus::PoiMenu; }
                        KeyCode::Char('/') => { input_cur = 0; focus = Focus::NearSearch(String::new()); }
                        KeyCode::Enter | KeyCode::Char(_) => {
                            // Enter=選択行 / 数字キー1-7=対応カテゴリ。最終行(=POI_KINDS.len())はキーワード周辺検索。
                            let idx = if let KeyCode::Char(c) = k.code { POI_KINDS.iter().position(|kk| kk.key == c) } else { Some(poimenu_sel) };
                            match idx {
                                Some(i) if i >= POI_KINDS.len() => { input_cur = 0; focus = Focus::NearSearch(String::new()); }
                                Some(i) => {
                                    show_busy(&mut out, cols, tr, "検索中…");
                                    match poi_search(&POI_KINDS[i], cx, cy, z, ow, oh, lat, lon) {
                                        Ok(items) if !items.is_empty() => { pois = items; poi_sel = 0; poi_label = POI_KINDS[i].label.to_string(); set_markers(&mut spec, &wps, &pois); focus = Focus::PoiList; }
                                        Ok(_) => { snd.play("error"); addr = format!("周辺2kmに{}無し", POI_KINDS[i].label); focus = Focus::PoiMenu; }
                                        Err(e) => { addr = format!("({e})"); focus = Focus::PoiMenu; }
                                    }
                                }
                                None => focus = Focus::PoiMenu,
                            }
                        }
                        _ => focus = Focus::PoiMenu,
                    },
                    Focus::PoiList => match k.code {
                        KeyCode::Up => { poi_sel = poi_sel.saturating_sub(1); if let Some(p) = pois.get(poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, z); cx = nx; cy = ny; } focus = Focus::PoiList; } // 選択に地図追従
                        KeyCode::Down => { if poi_sel + 1 < pois.len() { poi_sel += 1; } if let Some(p) = pois.get(poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, z); cx = nx; cy = ny; } focus = Focus::PoiList; }
                        KeyCode::Left => { cx -= (oh as f64 / 8.0).max(1.0); focus = Focus::PoiList; } // ←→で地図を微パン
                        KeyCode::Right => { cx += (oh as f64 / 8.0).max(1.0); focus = Focus::PoiList; }
                        KeyCode::Enter => { // 選択地点へ移動(明示)
                            if let Some(p) = pois.get(poi_sel) { let (nx, ny) = deg_to_pixel(p.0, p.1, z); cx = nx; cy = ny; }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('v') => { // 選択地点をルートに追加(末尾)
                            if let Some(p) = pois.get(poi_sel) {
                                snd.play("pop");
                                wp_add(&mut wps, (p.0, p.1));
                                let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_;
                                addr = format!("地点を追加 #{}", wps.len());
                            }
                            focus = Focus::PoiList;
                        }
                        KeyCode::Char('f') => focus = Focus::PoiMenu,
                        KeyCode::Char('P') => { // 選択結果をお気に入りスポットに登録(カテゴリを選ばせる)
                            if let Some(p) = pois.get(poi_sel) {
                                if spot_cats.is_empty() { let _ = ensure_spot_cat("お気に入り", &mut spot_cats); }
                                pending_spot = Some((p.0, p.1, p.2.clone()));
                                cat_sel = 0;
                                focus = Focus::SpotCatList;
                            } else { focus = Focus::PoiList; }
                        }
                        KeyCode::Esc => { pois.clear(); set_markers(&mut spec, &wps, &pois); }
                        _ => focus = Focus::PoiList,
                    },
                    Focus::SaveName(mut buf) => match k.code {
                        KeyCode::Enter => {
                            let name = buf.trim().to_string();
                            if !name.is_empty() {
                                addr = match save_named_route(&name, &mode, &wps) { Ok(_) => { snd.play("confirm"); format!("保存: {name}") }, Err(e) => format!("({e})") };
                            }
                        }
                        KeyCode::Esc => { snd.play("back"); }
                        other => { edit_line(&mut buf, &mut input_cur, other); focus = Focus::SaveName(buf); }
                    },
                    Focus::RouteList => match k.code {
                        KeyCode::Up => { rn_sel = rn_sel.saturating_sub(1); focus = Focus::RouteList; }
                        KeyCode::Down => { if rn_sel + 1 < route_names.len() { rn_sel += 1; } focus = Focus::RouteList; }
                        KeyCode::Enter => {
                            if let Some(name) = route_names.get(rn_sel) {
                                if let Some((w, m)) = load_named_route(name) {
                                    let (nx, ny) = deg_to_pixel(w[0].0, w[0].1, z); cx = nx; cy = ny;
                                    wps = w; mode = m; wp_sel = 0;
                                    { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
                                }
                            }
                        }
                        KeyCode::Esc => {}
                        _ => focus = Focus::RouteList,
                    },
                    Focus::RoadList => match k.code { // 道路の塊の一覧(個別削除)
                        KeyCode::Up => { road_sel = road_sel.saturating_sub(1); focus = Focus::RoadList; }
                        KeyCode::Down => { if road_sel + 1 < road_segs.len() { road_sel += 1; } focus = Focus::RoadList; }
                        KeyCode::Char('x') => { // 選択した道路の塊を削除
                            if road_sel < road_segs.len() {
                                road_segs.remove(road_sel);
                                if road_sel >= road_segs.len() && road_sel > 0 { road_sel -= 1; }
                                sync_roads!();
                            }
                            if road_segs.is_empty() { addr = "道路を全削除".into(); } // 空になったら閉じる(focusはMapのまま)
                            else { focus = Focus::RoadList; }
                        }
                        KeyCode::Esc => { snd.play("back"); } // 閉じる → Map
                        _ => focus = Focus::RoadList,
                    },
                    // 並べ替えビュー: ↑↓で選択(地図が追従)、Spaceで掴む↔置く、掴み中は↑↓で地点を移動
                    Focus::WaypointList => match k.code {
                        KeyCode::Up | KeyCode::BackTab | KeyCode::Char('w') => {
                            if !wps.is_empty() {
                                if grab && wp_sel > 0 { wps.swap(wp_sel, wp_sel - 1); wp_sel -= 1; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
                                else { wp_sel = (wp_sel + wps.len() - 1) % wps.len(); }
                                if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; }
                            }
                            focus = Focus::WaypointList;
                        }
                        KeyCode::Down | KeyCode::Tab | KeyCode::Char('s') => {
                            if !wps.is_empty() {
                                if grab && wp_sel + 1 < wps.len() { wps.swap(wp_sel, wp_sel + 1); wp_sel += 1; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
                                else { wp_sel = (wp_sel + 1) % wps.len(); }
                                if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; }
                            }
                            focus = Focus::WaypointList;
                        }
                        KeyCode::Char(' ') => { if !wps.is_empty() { grab = !grab; snd.play(if grab { "blip" } else { "pop" }); } focus = Focus::WaypointList; }
                        KeyCode::Char('+') | KeyCode::Char('=') => { if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; } focus = Focus::WaypointList; }
                        KeyCode::Char('-') | KeyCode::Char('_') => { if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; } focus = Focus::WaypointList; }
                        KeyCode::Char('[') => { if wp_sel > 0 && wp_sel < wps.len() { wps.swap(wp_sel, wp_sel - 1); wp_sel -= 1; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; } } focus = Focus::WaypointList; }
                        KeyCode::Char(']') => { if wp_sel + 1 < wps.len() { wps.swap(wp_sel, wp_sel + 1); wp_sel += 1; let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; } } focus = Focus::WaypointList; }
                        KeyCode::Char('x') => {
                            if !wps.is_empty() { let i = wp_sel.min(wps.len() - 1); wps.remove(i); if wp_sel >= wps.len() && wp_sel > 0 { wp_sel -= 1; } let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; }
                            grab = false;
                            if !wps.is_empty() { if let Some(&(la, lo)) = wps.get(wp_sel) { let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; } focus = Focus::WaypointList; } // 空になったら閉じる
                        }
                        KeyCode::Char('v') => { // 中心に地点を追加し、追加した点を選択(リストは wps から即再生成される)
                            snd.play("pop");
                            wp_add(&mut wps, (lat, lon));
                            wp_sel = wps.len().saturating_sub(1);
                            grab = false;
                            let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_;
                            addr = format!("地点を追加 #{}", wps.len());
                            focus = Focus::WaypointList;
                        }
                        KeyCode::Esc | KeyCode::Enter => { grab = false; } // 閉じる → Map
                        _ => focus = Focus::WaypointList,
                    },
                    // Space メニュー・トップ(カテゴリ選択)。文字キーは全カテゴリ横断で直接実行できる。
                    Focus::Menu(MenuLevel::Categories) => match k.code {
                        KeyCode::Up => { menu_cat_sel = menu_cat_sel.saturating_sub(1); focus = Focus::Menu(MenuLevel::Categories); }
                        KeyCode::Down => { if menu_cat_sel + 1 < MENU_CATEGORIES.len() { menu_cat_sel += 1; } focus = Focus::Menu(MenuLevel::Categories); }
                        KeyCode::Enter => { menu_item_sel = 0; focus = Focus::Menu(MenuLevel::Items(menu_cat_sel)); }
                        KeyCode::Esc => {} // メニューを閉じる → Map(音は鳴らさない)
                        KeyCode::Char(c) => match menu_action_for_key(c) {
                            Some(act) => run_action!(act, lat, lon, cols, tr),
                            None => focus = Focus::Menu(MenuLevel::Categories),
                        },
                        _ => focus = Focus::Menu(MenuLevel::Categories),
                    },
                    // Space メニュー・展開(項目選択)。キーはそのカテゴリ内だけ有効(スコープ限定)。
                    Focus::Menu(MenuLevel::Items(ci)) => {
                        let items = MENU_CATEGORIES[ci].items;
                        match k.code {
                            KeyCode::Up => { menu_item_sel = menu_item_sel.saturating_sub(1); focus = Focus::Menu(MenuLevel::Items(ci)); }
                            KeyCode::Down => { if menu_item_sel + 1 < items.len() { menu_item_sel += 1; } focus = Focus::Menu(MenuLevel::Items(ci)); }
                            KeyCode::Enter => run_action!(items[menu_item_sel].action, lat, lon, cols, tr),
                            KeyCode::Esc => { snd.play("back"); focus = Focus::Menu(MenuLevel::Categories); } // 上位カテゴリへ戻る
                            KeyCode::Char(c) => match items.iter().find(|it| it.key == c) {
                                Some(it) => run_action!(it.action, lat, lon, cols, tr),
                                None => focus = Focus::Menu(MenuLevel::Items(ci)),
                            },
                            _ => focus = Focus::Menu(MenuLevel::Items(ci)),
                        }
                    }
                    // 色ピッカー: ←→でパレット選択、Enterで確定
                    Focus::ColorPick { cat } => {
                        let n = SPOT_PALETTE.len() as u8;
                        match k.code {
                            KeyCode::Left => { color_sel = (color_sel + n - 1) % n; focus = Focus::ColorPick { cat }; }
                            KeyCode::Right => { color_sel = (color_sel + 1) % n; focus = Focus::ColorPick { cat }; }
                            KeyCode::Enter => {
                                if let Some(e) = spot_cats.get_mut(cat) { e.1 = color_sel; let _ = save_all_cats(&spot_cats); apply_spots(&mut spec, &spots, &spot_cats, show_spots); }
                                focus = Focus::SpotCatList;
                            }
                            KeyCode::Esc => { snd.play("back"); focus = Focus::SpotCatList; }
                            _ => focus = Focus::ColorPick { cat },
                        }
                    }
                    Focus::Map => {
                        // 既定を速く(大きく)・Shiftで微調整(ちょびちょび)に反転
                        let frac = if k.modifiers.contains(KeyModifiers::SHIFT) { 32.0 } else { 4.0 };
                        let step = (oh as f64 / frac).max(1.0);
                        let mut quit = false;
                        match k.code {
                            KeyCode::Left => { cx -= step; addr.clear(); }
                            KeyCode::Right => { cx += step; addr.clear(); }
                            KeyCode::Up => { cy -= step; addr.clear(); }
                            KeyCode::Down => { cy += step; addr.clear(); }
                            KeyCode::Char('+') | KeyCode::Char('=') => if z < 19 { z += 1; cx *= 2.0; cy *= 2.0; addr.clear(); },
                            KeyCode::Char('-') | KeyCode::Char('_') => if z > 2 { z -= 1; cx /= 2.0; cy /= 2.0; addr.clear(); },
                            KeyCode::Enter => { // 中心付近の最寄りお気に入りにスナップ＋名前表示
                                let mut best: Option<(f64, usize)> = None;
                                for (i, s) in spots.iter().enumerate() {
                                    let (gx, gy) = deg_to_pixel(s.lat, s.lon, z);
                                    let dpx = ((gx - cx).powi(2) + (gy - cy).powi(2)).sqrt();
                                    if best.map_or(true, |(bd, _)| dpx < bd) { best = Some((dpx, i)); }
                                }
                                match best {
                                    Some((dpx, i)) if dpx <= (ow.min(oh) as f64) * 0.25 => {
                                        let s = &spots[i];
                                        let (nx, ny) = deg_to_pixel(s.lat, s.lon, z); cx = nx; cy = ny;
                                        popup = Some(if s.name.is_empty() { "★ (無名スポット)".into() } else { format!("★ {} [{}]", s.name, s.cat) });
                                    }
                                    Some(_) => addr = "近くにお気に入り無し".into(),
                                    None => addr = "お気に入り未登録".into(),
                                }
                            }
                            KeyCode::Char('a') => addr = reverse_geocode(lat, lon).unwrap_or_else(|e| format!("({e})")),
                            KeyCode::Char('/') => { input_cur = 0; focus = Focus::Search(String::new()); }
                            KeyCode::Char('f') => focus = Focus::PoiMenu,
                            KeyCode::Char('S') => { input_cur = 0; focus = Focus::SaveName(String::new()); }
                            KeyCode::Char('L') => { route_names = list_named_routes(); rn_sel = 0; if route_names.is_empty() { addr = "お気に入り無し".into(); } else { focus = Focus::RouteList; } }
                            KeyCode::Char('v') => { // 地図中心に地点を追加(末尾)。役割は並び順で自動(先頭=始点/末尾=終点)
                                snd.play("pop"); wp_add(&mut wps, (lat, lon));
                                let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_;
                                addr = format!("地点を追加 #{}", wps.len());
                            }
                            KeyCode::Tab | KeyCode::Char('s') => { if !wps.is_empty() { wp_sel = (wp_sel + 1) % wps.len(); let (la, lo) = wps[wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; } } // 一覧の選択を回す(選択点へ寄る)。s=次
                            KeyCode::BackTab | KeyCode::Char('w') => { if !wps.is_empty() { wp_sel = (wp_sel + wps.len() - 1) % wps.len(); let (la, lo) = wps[wp_sel]; let (nx, ny) = deg_to_pixel(la, lo, z); cx = nx; cy = ny; } } // w=前
                            KeyCode::Char(' ') => { snd.play("blip"); menu_cat_sel = 0; focus = Focus::Menu(MenuLevel::Categories); } // Space=メニュー(カテゴリ→展開の2階層)
                            KeyCode::Char('?') => help = true,
                            KeyCode::Char('P') => { cat_sel = 0; focus = Focus::SpotCatList; } // マイスポット(カテゴリ一覧)
                            KeyCode::Char(',') => { set_sel = 0; focus = Focus::Settings; } // 設定画面
                            KeyCode::Char('r') => { input_cur = 0; focus = Focus::RoadSearch(String::new()); } // 道路名でルート(現在view内)
                            KeyCode::Char('@') => { // おすすめツーリングスポット提案(claude -p)
                                if !cfg.llm_recommend_enabled { snd.play("error"); addr = "おすすめ: 設定でOFF(,でON)".into(); }
                                else if !recommend::claude_available(&cfg.llm_command) { snd.play("error"); addr = "おすすめ: claudeが無い(設定のLLM/コマンド確認)".into(); }
                                else { input_cur = 0; focus = Focus::Recommend(String::new()); }
                            }
                            KeyCode::Char('V') => { show_spots = !show_spots; apply_spots(&mut spec, &spots, &spot_cats, show_spots); addr = if show_spots { "マイスポット表示".into() } else { "マイスポット非表示".into() }; }
                            KeyCode::Char('E') => { // 標高プロファイルの表示/非表示
                                show_elev = !show_elev;
                                if show_elev && (spec.routes.is_empty() || !route_ele.iter().any(|&z| z != 0.0)) { addr = "標高: ルート確定後に表示".into(); }
                            }
                            KeyCode::Char('A') => { // ルート再生(プレビュー走行)の開始/停止
                                if spec.routes.last().map_or(false, |r| r.pts.len() >= 2) {
                                    if play.is_some() { play = None; addr = "再生: 停止".into(); }
                                    else { play = Some(0.0); addr = "再生: 開始(Aで停止)".into(); }
                                } else { snd.play("error"); addr = "再生: ルート未確定".into(); }
                            }
                            KeyCode::Char('G') => { // ライブ現在地(ブレッドクラム)の ON/OFF
                                if gps_rx.is_some() { gps_rx = None; addr = "ライブ現在地: OFF".into(); }
                                else {
                                    let bin = if std::path::Path::new("/opt/homebrew/bin/CoreLocationCLI").exists() { "/opt/homebrew/bin/CoreLocationCLI" } else { "CoreLocationCLI" };
                                    if gpslive::available(bin) { gps_rx = Some(gpslive::start_poller(bin.to_string(), 5)); gps_trail.clear(); gps_pos = None; addr = "ライブ現在地: ON(5秒ごと)".into(); }
                                    else { addr = "ライブ: CoreLocationCLI無し(brew install corelocationcli)".into(); }
                                }
                            }
                            KeyCode::Char('i') => { // 実写(Street View)を中心地点で開く
                                if !cfg.streetview_enabled { snd.play("error"); addr = "実写: OFF(設定で有効化)".into(); }
                                else if !streetview::available(&cfg.google_maps_api_key) { snd.play("error"); addr = "実写: Google APIキー未設定([google] maps_api_key)".into(); }
                                else {
                                    // 実写取得を別スレッドへ(focusはMapのまま=スピナーが回る)
                                    let (la, lo) = (lat, lon);
                                    let key = cfg.google_maps_api_key.clone();
                                    let (tx, rx) = std::sync::mpsc::channel();
                                    std::thread::spawn(move || {
                                        let r = streetview::fetch(la, lo, 0, 640, 480, &key);
                                        let _ = tx.send((la, lo, 0, r));
                                    });
                                    street_job = Some(rx);
                                }
                            }
                            KeyCode::Char('I') => { // 実画像モード(iTerm2インライン画像)の ON/OFF
                                cfg.image_mode = !cfg.image_mode;
                                addr = if cfg.image_mode {
                                    if image_capable() { "実画像モード: ON".into() } else { "実画像モード: ON(この端末は非対応・AA継続)".into() }
                                } else { "実画像モード: OFF".into() };
                            }
                            KeyCode::Char('n') => { // BRouter の代替ルート候補を巡回
                                if wps.len() >= 2 {
                                    route_alt = (route_alt + 1) % 4;
                                    let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, route_alt);
                                    route_note = nn; route_job = jj;
                                } else { snd.play("error"); addr = "ルート未確定".into(); }
                            }
                            KeyCode::Char('W') => { // 走りまくり(峠/展望の周回)を生成。連打で別案
                                let dist = a.dist.unwrap_or(40.0);
                                match wander_route((lat, lon), dist, &a.shape) {
                                    Ok(w) => { wps = w; wp_sel = 0; let (nn, jj) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = nn; route_job = jj; }
                                    Err(e) => addr = format!("({e})"),
                                }
                            }
                            KeyCode::Char('o') => { // スマホ共有(GoogleマップQR)
                                if wps.len() >= 2 {
                                    let (url, _) = gmaps_url(&wps);
                                    match qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::L) {
                                        Ok(c) => qr_view = Some(c.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(false).build()),
                                        Err(_) => addr = "QR生成失敗".into(),
                                    }
                                } else { snd.play("error"); addr = "ルート未確定".into(); }
                            }
                            KeyCode::Char('x') => { wp_remove(&mut wps, &mut wp_sel); { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char('[') => { wp_swap(&mut wps, &mut wp_sel, true); { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char(']') => { wp_swap(&mut wps, &mut wp_sel, false); { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char('m') => { mode = match mode_label(&mode) { "下道" => "highway", "高速" => "short", _ => "surface" }.to_string(); { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char('c') => { wps.clear(); wp_sel = 0; road_segs.clear(); spec.roads.clear(); { let (n_, j_) = trigger_route(&mut spec, &wps, &pois, &mode, 0); route_note = n_; route_job = j_; } }
                            KeyCode::Char('g') => match spec.routes.last() {
                                Some(rt) => addr = match write_gpx("termmap-route.gpx", &rt.pts) { Ok(_) => "GPX保存: termmap-route.gpx".into(), Err(e) => format!("({e})") },
                                None => { snd.play("error"); addr = "ルート未確定".into(); }
                            },
                            KeyCode::Char('q') => quit = true, // Esc はサブモードの取消専用(Mapでは終了しない)
                            _ => {}
                        }
                        if quit { break; }
                        let n = (TILE as f64) * 2f64.powi(z as i32);
                        if cx < 0.0 { cx += n; } else if cx >= n { cx -= n; }
                        cy = cy.clamp(0.0, n - 1.0);
                    }
                }
            }
            Some(Event::Paste(s)) => { match &mut focus {
                Focus::Search(buf) | Focus::SaveName(buf) | Focus::NearSearch(buf) | Focus::NewCat(buf) | Focus::RoadSearch(buf) | Focus::Recommend(buf) => insert_str_at(buf, &mut input_cur, &s),
                Focus::SpotForm { name, url, field } => { if *field == 0 { insert_str_at(name, &mut input_cur, &s); } else if *field == 1 { insert_str_at(url, &mut input_cur, &s); } }
                Focus::SpotRename(buf, _) | Focus::SpotEditName(buf, _) => insert_str_at(buf, &mut input_cur, &s),
                Focus::Settings if set_sel == 13 => { cfg.google_maps_api_key = s.trim().to_string(); addr = "APIキー設定(sで保存)".into(); }
                _ => {}
            } }
            _ => {}
        }
    }
    let (lat, lon) = pixel_to_deg(cx, cy, z);
    save_state(lat, lon, z, &opts.style, &wps, &mode); // 終了時の位置とルートを --resume 用に保存
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    // 左袖リストのスクロール追従
    #[test]
    fn ensure_visible_follows_selection() {
        let vh = 5; // 表示5行
        // 収まる場合は offset=0 のまま
        let mut o = 0;
        ensure_visible(&mut o, 3, 4, vh);
        assert_eq!(o, 0);
        // 下へはみ出す: 20件・選択10 → 選択が末尾に来る位置(10+1-5=6)
        let mut o = 0;
        ensure_visible(&mut o, 10, 20, vh);
        assert_eq!(o, 6);
        assert!(10 >= o && 10 < o + vh, "選択が窓内");
        // そこから上へ戻る: 選択2 → 先頭に
        ensure_visible(&mut o, 2, 20, vh);
        assert_eq!(o, 2);
        // 末尾選択は末尾側の空きが詰まる(offset=count-vh)
        let mut o = 0;
        ensure_visible(&mut o, 19, 20, vh);
        assert_eq!(o, 15);
        // viewport=0 は安全に0
        let mut o = 7;
        ensure_visible(&mut o, 3, 20, 0);
        assert_eq!(o, 0);
    }

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

