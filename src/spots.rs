// マイスポット (カテゴリ別に色分けして保存・重畳)
use crate::render::OverlaySpec;

pub const SPOT_PALETTE: [[u8; 3]; 10] = [
    [255, 64, 64], [255, 140, 0], [255, 215, 0], [120, 255, 120], [80, 200, 255],
    [180, 120, 255], [255, 80, 200], [0, 220, 180], [200, 160, 90], [180, 180, 180],
];
pub struct Spot { pub lat: f64, pub lon: f64, pub cat: String, pub name: String }
fn spots_path() -> Option<std::path::PathBuf> { Some(std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".config/termmap/spots.txt")) }
fn spot_cats_path() -> Option<std::path::PathBuf> { Some(std::path::PathBuf::from(std::env::var("HOME").ok()?).join(".config/termmap/spot-categories.txt")) }
pub fn spot_clean(s: &str) -> String { s.trim().replace(['\n', ','], " ") } // カテゴリ/名前にカンマ・改行を入れない
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
