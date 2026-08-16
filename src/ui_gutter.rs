// 左袖リスト(メニュー/ルート/スポット/設定/目的地 等)の各行の組み立て。
// ui.rs の描画ループから機械的に切り出したもの(挙動は不変)。表示中のリストは focus に応じて
// 常に1つだけで、書き換えるループ状態はスクロール位置(list_offset)のみ。

use crate::Args;
use crate::config::Config;
use crate::focus::Focus;
use crate::fit_cells;
use crate::geo::haversine_km;
use crate::listview::ensure_visible;
use crate::menu::{MENU_CATEGORIES, MenuLevel, menu_row, ROUTE_ACTS};
use crate::poi::PoiKind;
use crate::render::PoiCat;
use crate::roadseg::RoadSeg;
use crate::settings;
use crate::spots::Spot;
use crate::ui_helpers::onboarded_marker;

// build_gutter_lines が読む描画ループ側の状態(書き換えるのは list_offset だけなので別引数)。
pub(crate) struct GutterCtx<'a> {
    pub gut: u32,
    pub map_rows: u32,
    pub focus: &'a Focus,
    pub show_menu: bool,
    pub show_route: bool,
    pub show_wps: bool,
    pub show_splist: bool,
    pub show_catlist: bool,
    pub show_settings: bool,
    pub show_poimenu: bool,
    pub show_routes: bool,
    pub show_favmenu: bool,
    pub show_roadlist: bool,
    pub menu_cat_sel: usize,
    pub menu_item_sel: usize,
    pub wps: &'a [(f64, f64)],
    pub route_sel: usize,
    pub grab: bool,
    pub wp_sel: usize,
    pub spots: &'a [Spot],
    pub cur_cat: &'a str,
    pub sp_sel: usize,
    pub lat: f64,
    pub lon: f64,
    pub spot_cats: &'a [(String, u8, u8)],
    pub cat_sel: usize,
    pub opts: &'a Args,
    pub cfg: &'a Config,
    pub set_sel: usize,
    pub set_pick_sel: usize,
    pub poi_kinds: &'a [PoiKind],
    pub poimenu_sel: usize,
    pub route_names: &'a [String],
    pub rn_sel: usize,
    pub road_segs: &'a [RoadSeg],
    pub road_sel: usize,
    pub pois: &'a [(f64, f64, String, PoiCat)],
    pub poi_label: &'a str,
    pub poi_sel: usize,
}

pub(crate) fn build_gutter_lines(c: &GutterCtx, list_offset: &mut usize) -> Vec<String> {
    if c.gut == 0 { return Vec::new(); }
    let gw = c.gut as usize;
    let (header, items, sel): (String, Vec<String>, usize) = if c.show_menu {
        match c.focus {
            // トップ: カテゴリだけ(キー列なし)。文字キー直打ちも効く旨は下部に出す。
            Focus::Menu(MenuLevel::Categories) => {
                let its = MENU_CATEGORIES.iter().map(|cat| format!("  {}", cat.label)).collect();
                ("メニュー".to_string(), its, c.menu_cat_sel)
            }
            // 展開: 選んだカテゴリの項目のみ。ラベル左・キー右端揃え。
            Focus::Menu(MenuLevel::Items(ci)) => {
                let cat = &MENU_CATEGORIES[*ci];
                let its = cat.items.iter().map(|it| menu_row(it.label, it.key, gw.saturating_sub(1))).collect();
                (format!("← {}", cat.label), its, c.menu_item_sel)
            }
            _ => ("メニュー".to_string(), Vec::new(), 0),
        }
    } else if c.show_route {
        // Map左袖: 点(#1..#n) + 操作行(保存/GPX/QR/再生/標高/代替/消去)。Tabで縦断・Enterで実行。
        let n = c.wps.len();
        let mut its: Vec<String> = c.wps.iter().enumerate().map(|(i, (la, lo))| {
            let role = if i == 0 { "始点" } else if i + 1 == n { "終点" } else { "経由" };
            format!("#{} {} {:.3},{:.3}", i + 1, role, la, lo)
        }).collect();
        for (label, _) in ROUTE_ACTS.iter() { its.push((*label).to_string()); }
        let sel = c.route_sel.min(its.len().saturating_sub(1));
        let hdr = if matches!(c.focus, Focus::RoutePanel) { "ルート ↑↓選択".to_string() } else { "ルート(w/s上下)".to_string() };
        (hdr, its, sel)
    } else if c.show_wps {
        let n = c.wps.len();
        let its = c.wps.iter().enumerate().map(|(i, (la, lo))| {
            let role = if i == 0 { "始点" } else if i + 1 == n { "終点" } else { "経由" };
            format!("#{} {} {:.3},{:.3}", i + 1, role, la, lo)
        }).collect();
        let hdr = if c.grab { "並べ替え:掴".to_string() } else { "並べ替え".to_string() };
        (hdr, its, c.wp_sel)
    } else if c.show_splist {
        let its = c.spots.iter().filter(|s| s.cat == c.cur_cat).map(|s| {
            let nm = if s.name.is_empty() { "(無名)" } else { s.name.as_str() };
            let d = haversine_km((c.lat, c.lon), (s.lat, s.lon)); // 現在地(中心)からの距離
            format!("{} {:.1}k", nm, d)
        }).collect();
        (format!("{}", c.cur_cat), its, c.sp_sel)
    } else if c.show_catlist {
        let its = c.spot_cats.iter().map(|(n, _, _)| n.clone()).collect(); // 色は c、形は M で選ぶ(番号表示はやめた)
        ("カテゴリ".to_string(), its, c.cat_sel)
    } else if c.show_settings {
        // 項目一覧(its)の組み立ては settings.rs::settings_rows へ切り出し済み(opts/cfg/引数の値だけを
        // 見る純関数)。onboarded_marker()(ファイルIO)とset_sel/set_pick_sel(ローカル選択状態)は
        // ここで評価してから渡す。
        let picking = if let Focus::SettingsPick(idx) = c.focus { Some(*idx) } else { None };
        let onboarded_done = onboarded_marker().map_or(false, |p| p.exists());
        settings::settings_rows(c.opts, c.cfg, picking, onboarded_done, c.set_sel, c.set_pick_sel)
    } else if c.show_poimenu {
        let mut its: Vec<String> = c.poi_kinds.iter().map(|k| format!("{} {}", k.key, k.label)).collect();
        its.push("キーワードで周辺検索".to_string());
        ("目的地(n新規 x削除 [ ]並替)".to_string(), its, c.poimenu_sel)
    } else if c.show_routes {
        ("← お気に入りルート".to_string(), c.route_names.to_vec(), c.rn_sel)
    } else if c.show_favmenu {
        let sel = if let Focus::RouteFavMenu { sel } = c.focus { *sel } else { 0 };
        ("お気に入りルート".to_string(), vec!["保存".to_string(), "呼び出し".to_string()], sel)
    } else if c.show_roadlist {
        // 各行を塊マーカー │ + 道路名で。色はマップ側の別色で区別(gutterはfit_cells制約でANSI不可)
        let its = c.road_segs.iter().map(|r| format!("│ {}", if r.name.is_empty() { "(無名)" } else { r.name.as_str() })).collect();
        ("道路".to_string(), its, c.road_sel)
    } else {
        let its = c.pois.iter().map(|(la, lo, nm, _)| {
            // OSMにnameタグが無いPOI(駐車場等に多い)は「(無名)」の連発でなく検索カテゴリ名で埋める
            let d = haversine_km((c.lat, c.lon), (*la, *lo));
            format!("{} {:.1}k", if nm.is_empty() { c.poi_label } else { nm.as_str() }, d)
        }).collect();
        (c.poi_label.to_string(), its, c.poi_sel)
    };
    // 見出し1行を除いた表示可能行数ぶんだけ、選択に追従してウィンドウ表示する
    let sel = sel.min(items.len().saturating_sub(1)); // sel が範囲外でも位置表示/添字を破綻させない
    let vh = (c.map_rows as usize).saturating_sub(1).max(1);
    ensure_visible(list_offset, sel, items.len(), vh);
    let end = (*list_offset + vh).min(items.len());
    let (more_up, more_dn) = (*list_offset > 0, end < items.len());
    let mut gl = Vec::with_capacity(c.map_rows as usize);
    let hdr = if items.len() > vh {
        // 画面に収まらない時は 位置(sel+1/総数) と上下の続き矢印を出す
        format!("[{} {}/{}]{}{}", header, sel + 1, items.len(), if more_up { " ↑" } else { "" }, if more_dn { "↓" } else { "" })
    } else {
        format!("[{} {}]", header, items.len())
    };
    gl.push(fit_cells(&hdr, gw));
    for idx in *list_offset..end {
        let cell = fit_cells(&format!("{}{}", if idx == sel { ">" } else { " " }, items[idx]), gw);
        gl.push(if idx == sel { format!("\x1b[7m{cell}\x1b[0m") } else { cell });
    }
    gl
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::MenuLevel;

    // build_gutter_lines() を呼ぶテスト用の Args(既定値。parse_args() の初期値と同じ)。
    fn test_args() -> Args {
        Args { lat: None, lon: None, place: None, zoom: 14, width: None, win_px: 640,
               style: "osm".to_string(), braille: false, mono: false, classify: false,
               edge: false, here: false, threshold: None,
               range: Vec::new(), home: None, route: None, route_mode: "surface".to_string(),
               gpx: None, load_route: None, save_route: None, list_routes: false, share: false,
               wander: false, dist: None, shape: "loop".to_string(), image: None, png: None }
    }

    // どのリストも出していない(全 show_* が false)状態。各テストで必要なフラグだけ立てる。
    fn base_ctx<'a>(focus: &'a Focus, opts: &'a Args, cfg: &'a Config) -> GutterCtx<'a> {
        GutterCtx {
            gut: 28, map_rows: 6, focus,
            show_menu: false, show_route: false, show_wps: false, show_splist: false,
            show_catlist: false, show_settings: false, show_poimenu: false, show_routes: false,
            show_favmenu: false, show_roadlist: false,
            menu_cat_sel: 0, menu_item_sel: 0,
            wps: &[], route_sel: 0, grab: false, wp_sel: 0,
            spots: &[], cur_cat: "", sp_sel: 0, lat: 35.0, lon: 139.0,
            spot_cats: &[], cat_sel: 0,
            opts, cfg, set_sel: 0, set_pick_sel: 0,
            poi_kinds: &[], poimenu_sel: 0,
            route_names: &[], rn_sel: 0,
            road_segs: &[], road_sel: 0,
            pois: &[], poi_label: "", poi_sel: 0,
        }
    }

    // 選択行に付く反転エスケープを剥がして中身だけ見る(表示幅の検証もできるように)。
    fn plain(line: &str) -> String {
        line.trim_start_matches("\x1b[7m").trim_end_matches("\x1b[0m").to_string()
    }

    fn width(s: &str) -> usize {
        s.chars().map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)).sum()
    }

    #[test]
    fn gutter_is_empty_when_width_is_zero() {
        let (focus, opts, cfg) = (Focus::Map, test_args(), Config::default());
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.gut = 0;
        c.show_roadlist = true;
        let mut off = 3usize;
        assert!(build_gutter_lines(&c, &mut off).is_empty());
        assert_eq!(off, 3, "袖を出さないフレームはスクロール位置に触らない");
    }

    #[test]
    fn header_shows_total_and_rows_are_padded_to_gutter_width() {
        let (focus, opts, cfg) = (Focus::RoadList, test_args(), Config::default());
        let segs = vec![
            RoadSeg { name: "国道1号".to_string(), color: [0, 0, 0], pts: Vec::new() },
            RoadSeg { name: String::new(), color: [0, 0, 0], pts: Vec::new() },
        ];
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_roadlist = true;
        c.road_segs = &segs;
        c.road_sel = 1;
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert_eq!(gl.len(), 3); // 見出し + 2行
        assert!(plain(&gl[0]).starts_with("[道路 2]"));
        assert!(plain(&gl[1]).starts_with(" │ 国道1号"));
        assert!(plain(&gl[2]).starts_with(">│ (無名)"), "name空は(無名)で埋める");
        for l in &gl { assert_eq!(width(&plain(l)), 28); }
        // 選択行だけ反転エスケープで包む
        assert!(!gl[1].starts_with("\x1b[7m"));
        assert!(gl[2].starts_with("\x1b[7m") && gl[2].ends_with("\x1b[0m"));
    }

    #[test]
    fn empty_list_renders_header_only() {
        let (focus, opts, cfg) = (Focus::RoadList, test_args(), Config::default());
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_roadlist = true;
        c.road_sel = 4; // 空リストで範囲外の選択が残っていても破綻しない
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert_eq!(gl.len(), 1);
        assert!(plain(&gl[0]).starts_with("[道路 0]"));
    }

    #[test]
    fn long_list_scrolls_to_follow_selection() {
        let (focus, opts, cfg) = (Focus::RoadList, test_args(), Config::default());
        let segs: Vec<RoadSeg> = (0..20).map(|i| RoadSeg { name: format!("道{i}"), color: [0, 0, 0], pts: Vec::new() }).collect();
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_roadlist = true;
        c.road_segs = &segs;
        c.map_rows = 6; // 表示可能行数 vh = 5
        c.road_sel = 12;
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert_eq!(off, 8, "選択(12)が末尾に来るまでスクロールする");
        assert_eq!(gl.len(), 6); // 見出し + vh
        assert!(plain(&gl[0]).starts_with("[道路 13/20] ↑"), "上に続きがある: {}", plain(&gl[0]));
        assert!(plain(&gl[0]).contains('↓'), "下にも続きがある");
        assert!(plain(&gl[1]).starts_with(" │ 道8"));
        assert!(plain(&gl[5]).starts_with(">│ 道12"));
    }

    #[test]
    fn selection_beyond_end_is_clamped_to_last_row() {
        let (focus, opts, cfg) = (Focus::RoadList, test_args(), Config::default());
        let segs: Vec<RoadSeg> = (0..3).map(|i| RoadSeg { name: format!("道{i}"), color: [0, 0, 0], pts: Vec::new() }).collect();
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_roadlist = true;
        c.road_segs = &segs;
        c.road_sel = 99;
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert_eq!(gl.len(), 4);
        assert!(plain(&gl[3]).starts_with(">│ 道2"));
    }

    #[test]
    fn poi_list_is_the_fallback_and_fills_unnamed_with_the_search_label() {
        let (focus, opts, cfg) = (Focus::PoiList, test_args(), Config::default());
        let pois = vec![
            (35.0, 139.0, "セブン".to_string(), crate::render::PoiCat::Shop),
            (35.1, 139.0, String::new(), crate::render::PoiCat::Shop),
        ];
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.pois = &pois;
        c.poi_label = "コンビニ";
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert!(plain(&gl[0]).starts_with("[コンビニ 2]"));
        assert!(plain(&gl[1]).starts_with(">セブン 0.0k"), "中心と同座標は0.0k");
        assert!(plain(&gl[2]).starts_with(" コンビニ 11.1k"), "無名は検索カテゴリ名で埋める: {}", plain(&gl[2]));
    }

    #[test]
    fn spot_list_shows_only_the_current_category() {
        let (focus, opts, cfg) = (Focus::SpotList, test_args(), Config::default());
        let spots = vec![
            Spot { lat: 35.0, lon: 139.0, cat: "温泉".to_string(), name: "A湯".to_string() },
            Spot { lat: 35.0, lon: 139.0, cat: "峠".to_string(), name: "B峠".to_string() },
            Spot { lat: 35.0, lon: 139.0, cat: "温泉".to_string(), name: String::new() },
        ];
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_splist = true;
        c.spots = &spots;
        c.cur_cat = "温泉";
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert_eq!(gl.len(), 3); // 見出し + 温泉2件(峠は除外)
        assert!(plain(&gl[0]).starts_with("[温泉 2]"));
        assert!(plain(&gl[1]).starts_with(">A湯 0.0k"));
        assert!(plain(&gl[2]).starts_with(" (無名) 0.0k"));
    }

    #[test]
    fn route_panel_lists_waypoints_then_action_rows() {
        let (focus, opts, cfg) = (Focus::RoutePanel, test_args(), Config::default());
        let wps = vec![(35.0, 139.0), (35.5, 139.5), (36.0, 140.0)];
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_route = true;
        c.wps = &wps;
        c.map_rows = 20;
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert_eq!(gl.len(), 1 + 3 + ROUTE_ACTS.len());
        assert!(plain(&gl[0]).starts_with("[ルート ↑↓選択 10]"));
        assert!(plain(&gl[1]).starts_with(">#1 始点 35.000,139.000"));
        assert!(plain(&gl[2]).starts_with(" #2 経由"));
        assert!(plain(&gl[3]).starts_with(" #3 終点"));
        assert!(plain(&gl[4]).starts_with(" ▶ 保存"));
    }

    #[test]
    fn waypoint_reorder_header_reflects_grab_state() {
        let (focus, opts, cfg) = (Focus::WaypointList, test_args(), Config::default());
        let wps = vec![(35.0, 139.0), (36.0, 140.0)];
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_wps = true;
        c.wps = &wps;
        let mut off = 0usize;
        assert!(plain(&build_gutter_lines(&c, &mut off)[0]).starts_with("[並べ替え 2]"));
        c.grab = true;
        let mut off = 0usize;
        assert!(plain(&build_gutter_lines(&c, &mut off)[0]).starts_with("[並べ替え:掴 2]"));
    }

    #[test]
    fn menu_shows_categories_at_top_and_items_when_expanded() {
        let opts = test_args();
        let cfg = Config::default();
        let focus = Focus::Menu(MenuLevel::Categories);
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_menu = true;
        c.map_rows = 20;
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert!(plain(&gl[0]).starts_with(&format!("[メニュー {}]", MENU_CATEGORIES.len())));
        assert!(plain(&gl[1]).starts_with(&format!(">  {}", MENU_CATEGORIES[0].label)));

        let focus = Focus::Menu(MenuLevel::Items(0));
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_menu = true;
        c.map_rows = 20;
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert!(plain(&gl[0]).starts_with(&format!("[← {} {}]", MENU_CATEGORIES[0].label, MENU_CATEGORIES[0].items.len())));
        assert!(plain(&gl[1]).contains(MENU_CATEGORIES[0].items[0].label));
    }

    #[test]
    fn fav_menu_selection_comes_from_focus() {
        let opts = test_args();
        let cfg = Config::default();
        let focus = Focus::RouteFavMenu { sel: 1 };
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_favmenu = true;
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert_eq!(gl.len(), 3);
        assert!(plain(&gl[1]).starts_with(" 保存"));
        assert!(plain(&gl[2]).starts_with(">呼び出し"));
    }

    // show_* の優先順位(先に判定される方が勝つ)は元のif-elseチェーンのまま。
    #[test]
    fn menu_takes_precedence_over_other_lists() {
        let opts = test_args();
        let cfg = Config::default();
        let focus = Focus::Menu(MenuLevel::Categories);
        let segs = vec![RoadSeg { name: "国道1号".to_string(), color: [0, 0, 0], pts: Vec::new() }];
        let mut c = base_ctx(&focus, &opts, &cfg);
        c.show_menu = true;
        c.show_roadlist = true;
        c.road_segs = &segs;
        c.map_rows = 20;
        let mut off = 0usize;
        let gl = build_gutter_lines(&c, &mut off);
        assert!(plain(&gl[0]).starts_with("[メニュー "));
    }
}
