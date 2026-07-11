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
pub fn load_spot_cats() -> Vec<(String, u8)> {
    let mut v = Vec::new();
    if let Some(s) = spot_cats_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        for l in s.lines() {
            let mut it = l.splitn(2, '\t');
            if let (Some(n), Some(i)) = (it.next(), it.next()) {
                if let Ok(idx) = i.trim().parse::<u8>() { v.push((n.to_string(), idx)); }
            }
        }
    }
    v
}
pub fn ensure_spot_cat(name: &str, cats: &mut Vec<(String, u8)>) -> u8 {
    use std::io::Write;
    let name = spot_clean(name);
    if let Some((_, c)) = cats.iter().find(|(n, _)| *n == name) { return *c; }
    let idx = (cats.len() % SPOT_PALETTE.len()) as u8;
    cats.push((name.clone(), idx));
    if let Some(p) = spot_cats_path() {
        if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) { let _ = writeln!(f, "{name}\t{idx}"); }
    }
    idx
}
fn spot_color_of(cat: &str, cats: &[(String, u8)]) -> [u8; 3] {
    let idx = cats.iter().find(|(n, _)| n == cat).map(|(_, c)| *c).unwrap_or(9);
    SPOT_PALETTE[(idx as usize) % SPOT_PALETTE.len()]
}
pub fn apply_spots(spec: &mut OverlaySpec, spots: &[Spot], cats: &[(String, u8)], show: bool) {
    spec.spots.clear();
    if show { for s in spots { spec.spots.push((s.lat, s.lon, spot_color_of(&s.cat, cats))); } }
}
pub fn save_all_spots(spots: &[Spot]) -> Result<(), String> {
    let p = spots_path().ok_or("HOME不明")?;
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let s: String = spots.iter().map(|s| format!("{},{},{},{}\n", s.lat, s.lon, spot_clean(&s.cat), spot_clean(&s.name))).collect();
    std::fs::write(p, s).map_err(|e| e.to_string())
}
pub fn save_all_cats(cats: &[(String, u8)]) -> Result<(), String> {
    let p = spot_cats_path().ok_or("HOME不明")?;
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let s: String = cats.iter().map(|(n, i)| format!("{n}\t{i}\n")).collect();
    std::fs::write(p, s).map_err(|e| e.to_string())
}
