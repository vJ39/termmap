// マイスポット (カテゴリ別に色分けして保存・重畳)
use crate::render::OverlaySpec;

pub const SPOT_PALETTE: [[u8; 3]; 10] = [
    [255, 64, 64], [255, 140, 0], [255, 215, 0], [120, 255, 120], [80, 200, 255],
    [180, 120, 255], [255, 80, 200], [0, 220, 180], [200, 160, 90], [180, 180, 180],
];
pub struct Spot { pub lat: f64, pub lon: f64, pub cat: String, pub name: String }
fn spots_path() -> Option<std::path::PathBuf> { Some(std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".config/termmap/spots.txt")) }
fn spot_cats_path() -> Option<std::path::PathBuf> { Some(std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".config/termmap/spot-categories.txt")) }
// カテゴリ/名前にカンマ・制御文字(改行・タブ・ESC等)を入れない。保存形式(カンマ/タブ区切り・1行1件)と画面表示を壊すため。
pub fn spot_clean(s: &str) -> String { s.trim().chars().map(|c| if c == ',' || c.is_control() { ' ' } else { c }).collect() }
pub fn load_spots() -> Vec<Spot> {
    let mut v = Vec::new();
    if let Some(s) = spots_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        for l in s.lines() {
            let mut it = l.splitn(4, ',');
            if let (Some(la), Some(lo), Some(cat), Some(name)) = (it.next(), it.next(), it.next(), it.next()) {
                if let (Ok(la), Ok(lo)) = (la.trim().parse(), lo.trim().parse()) {
                    v.push(Spot { lat: la, lon: lo, cat: cat.trim().to_string(), name: name.trim().to_string() });
                }
            }
        }
    }
    v
}
pub fn append_spot(s: &Spot) -> Result<(), String> {
    use std::io::Write;
    let p = spots_path().ok_or("HOME不明")?;
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p).map_err(|e| e.to_string())?;
    writeln!(f, "{},{},{},{}", s.lat, s.lon, spot_clean(&s.cat), spot_clean(&s.name)).map_err(|e| e.to_string())
}
// カテゴリは (名前, 色index, 形状index)。形状は色とは独立に選べる(M で形状ピッカー)。
pub fn load_spot_cats() -> Vec<(String, u8, u8)> {
    let mut v = Vec::new();
    if let Some(s) = spot_cats_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        for l in s.lines() {
            let mut it = l.splitn(3, '\t');
            if let (Some(n), Some(i)) = (it.next(), it.next()) {
                if let Ok(idx) = i.trim().parse::<u8>() {
                    // 3列目(形状)は後方互換で欠落時 0=四角
                    let shape = it.next().and_then(|s| s.trim().parse::<u8>().ok()).unwrap_or(0);
                    v.push((n.to_string(), idx, shape));
                }
            }
        }
    }
    v
}
pub fn ensure_spot_cat(name: &str, cats: &mut Vec<(String, u8, u8)>) -> u8 {
    use std::io::Write;
    let name = spot_clean(name);
    if let Some((_, c, _)) = cats.iter().find(|(n, _, _)| *n == name) { return *c; }
    let idx = (cats.len() % SPOT_PALETTE.len()) as u8;
    let shape = 0u8; // 新規カテゴリの既定形状=四角(M で変更)
    cats.push((name.clone(), idx, shape));
    if let Some(p) = spot_cats_path() {
        if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) { let _ = writeln!(f, "{name}\t{idx}\t{shape}"); }
    }
    idx
}
fn spot_color_of(cat: &str, cats: &[(String, u8, u8)]) -> [u8; 3] {
    let idx = cats.iter().find(|(n, _, _)| n == cat).map(|(_, c, _)| *c).unwrap_or(9);
    SPOT_PALETTE[(idx as usize) % SPOT_PALETTE.len()]
}
// カテゴリに保存された形状indexを返す(見つからなければ 0=四角)。描画側で範囲外は四角にフォールバックする。
fn spot_shape_of(cat: &str, cats: &[(String, u8, u8)]) -> u8 {
    cats.iter().find(|(n, _, _)| n == cat).map(|(_, _, s)| *s).unwrap_or(0)
}
pub fn apply_spots(spec: &mut OverlaySpec, spots: &[Spot], cats: &[(String, u8, u8)], show: bool) {
    spec.spots.clear();
    if show { for s in spots { spec.spots.push((s.lat, s.lon, spot_color_of(&s.cat, cats), spot_shape_of(&s.cat, cats))); } }
}
pub fn save_all_spots(spots: &[Spot]) -> Result<(), String> {
    let p = spots_path().ok_or("HOME不明")?;
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let s: String = spots.iter().map(|s| format!("{},{},{},{}\n", s.lat, s.lon, spot_clean(&s.cat), spot_clean(&s.name))).collect();
    std::fs::write(p, s).map_err(|e| e.to_string())
}
pub fn save_all_cats(cats: &[(String, u8, u8)]) -> Result<(), String> {
    let p = spot_cats_path().ok_or("HOME不明")?;
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let s: String = cats.iter().map(|(n, i, sh)| format!("{n}\t{i}\t{sh}\n")).collect();
    std::fs::write(p, s).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testing::reexec_in_temp_home;

    fn spot(lat: f64, lon: f64, cat: &str, name: &str) -> Spot {
        Spot { lat, lon, cat: cat.to_string(), name: name.to_string() }
    }

    fn empty_spec() -> OverlaySpec {
        OverlaySpec { pois: Vec::new(), routes: Vec::new(), expressway_segments: Vec::new(), roads: Vec::new(),
                      traffic_segments: Vec::new(), warning_segments: Vec::new(), rings: Vec::new(), spots: Vec::new() }
    }

    #[test]
    fn spot_clean_trims_and_replaces_commas_and_newlines() {
        assert_eq!(spot_clean("  ラーメン  "), "ラーメン");
        assert_eq!(spot_clean("a,b\nc"), "a b c", "保存形式(カンマ区切り・1行1件)を壊さない");
        assert_eq!(spot_clean(""), "");
    }

    // 貼り付けは制御文字を素通しで入れてくる。タブはカテゴリの保存形式(タブ区切り)を、
    // ESC等は画面表示を壊すので空白にする。
    #[test]
    fn spot_clean_replaces_tabs_and_other_control_characters() {
        assert_eq!(spot_clean("峠\t道"), "峠 道");
        assert_eq!(spot_clean("a\rb\u{1b}[31mc"), "a b [31mc");
    }

    #[test]
    fn a_category_name_with_a_tab_survives_a_restart() {
        if !reexec_in_temp_home(module_path!(), "a_category_name_with_a_tab_survives_a_restart", &[]) { return; }
        let mut cats = Vec::new();
        ensure_spot_cat("峠\t道", &mut cats);
        assert_eq!(cats[0].0, "峠 道");
        assert_eq!(load_spot_cats(), cats, "再読込で同じ一覧に戻る");
    }

    #[test]
    fn spot_color_and_shape_come_from_the_category() {
        let cats = vec![("温泉".to_string(), 3u8, 2u8), ("峠".to_string(), 12u8, 5u8)];
        assert_eq!(spot_color_of("温泉", &cats), SPOT_PALETTE[3]);
        assert_eq!(spot_shape_of("温泉", &cats), 2);
        assert_eq!(spot_color_of("峠", &cats), SPOT_PALETTE[2], "色indexがパレット数を超えても循環する");
        // 未登録カテゴリは灰(パレット末尾)・四角
        assert_eq!(spot_color_of("不明", &cats), SPOT_PALETTE[9]);
        assert_eq!(spot_shape_of("不明", &cats), 0);
    }

    #[test]
    fn apply_spots_rebuilds_the_markers_and_clears_them_when_hidden() {
        let cats = vec![("温泉".to_string(), 1u8, 3u8)];
        let spots = vec![spot(35.0, 139.0, "温泉", "A湯"), spot(36.0, 140.0, "未分類", "B")];
        let mut spec = empty_spec();
        spec.spots.push((0.0, 0.0, [0, 0, 0], 0)); // 前回の残り
        apply_spots(&mut spec, &spots, &cats, true);
        assert_eq!(spec.spots, vec![(35.0, 139.0, SPOT_PALETTE[1], 3), (36.0, 140.0, SPOT_PALETTE[9], 0)]);
        apply_spots(&mut spec, &spots, &cats, false);
        assert!(spec.spots.is_empty(), "非表示なら残さない");
    }

    // 以下は HOME を一時ディレクトリにした子プロセスで実行する(実HOMEには触れない)。

    #[test]
    fn spots_survive_append_save_and_reload() {
        if !reexec_in_temp_home(module_path!(), "spots_survive_append_save_and_reload", &[]) { return; }
        assert!(load_spots().is_empty(), "ファイルが無ければ空");
        append_spot(&spot(35.5, 139.5, "ラーメン", "満州軒,本店\n2階")).unwrap();
        append_spot(&spot(36.0, 140.0, " 温泉 ", "A湯")).unwrap();
        let got = load_spots();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].lat, got[0].lon), (35.5, 139.5));
        assert_eq!(got[0].name, "満州軒 本店 2階", "カンマ・改行は空白にして保存する");
        assert_eq!(got[1].cat, "温泉");
        save_all_spots(&got[1..]).unwrap();
        let again = load_spots();
        assert_eq!(again.len(), 1, "全保存は追記ではなく置き換え");
        assert_eq!(again[0].name, "A湯");
    }

    // 手で壊した行は読み飛ばす。4列目以降のカンマは名前の一部として残る。
    #[test]
    fn broken_spot_lines_are_skipped_on_load() {
        if !reexec_in_temp_home(module_path!(), "broken_spot_lines_are_skipped_on_load", &[]) { return; }
        let p = spots_path().unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "壊れた行\n35.0,139.0,3列だけ\nx,y,温泉,座標が数値でない\n 35.1 , 139.1 , 温泉 , A湯,別館 \n").unwrap();
        let got = load_spots();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].lat, got[0].lon), (35.1, 139.1));
        assert_eq!(got[0].cat, "温泉");
        assert_eq!(got[0].name, "A湯,別館");
    }

    // 全保存と読込の往復・形状列の無い旧形式は四角扱い・壊れた行は読み飛ばす。
    #[test]
    fn categories_survive_save_and_reload() {
        if !reexec_in_temp_home(module_path!(), "categories_survive_save_and_reload", &[]) { return; }
        assert!(load_spot_cats().is_empty(), "ファイルが無ければ空");
        let cats = vec![("温泉".to_string(), 1u8, 2u8), ("峠".to_string(), 9u8, 6u8)];
        save_all_cats(&cats).unwrap();
        assert_eq!(load_spot_cats(), cats);
        let p = spot_cats_path().unwrap();
        std::fs::write(&p, "旧形式\t4\n色が数値でない\tx\t0\n色が範囲外\t300\t0\n形状が壊れている\t5\tz\n").unwrap();
        assert_eq!(load_spot_cats(), vec![("旧形式".to_string(), 4, 0), ("形状が壊れている".to_string(), 5, 0)]);
    }

    // 既存カテゴリは同じ色を返して書き込まない。新規は登録数で色を循環させて追記する。
    #[test]
    fn ensure_spot_cat_reuses_known_names_and_appends_new_ones() {
        if !reexec_in_temp_home(module_path!(), "ensure_spot_cat_reuses_known_names_and_appends_new_ones", &[]) { return; }
        let mut cats = vec![("温泉".to_string(), 7u8, 1u8)];
        assert_eq!(ensure_spot_cat(" 温泉 ", &mut cats), 7, "前後の空白は名前に含めない");
        assert_eq!(cats.len(), 1, "既存なら増やさない");
        assert!(load_spot_cats().is_empty(), "既存なら書き込まない");
        assert_eq!(ensure_spot_cat("峠,道", &mut cats), 1);
        assert_eq!(cats[1], ("峠 道".to_string(), 1, 0), "カンマは空白へ・形状は四角");
        assert_eq!(load_spot_cats(), vec![("峠 道".to_string(), 1, 0)]);
    }
}
