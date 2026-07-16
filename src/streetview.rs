//! Google Street View Static API から実写画像を取得する。
//! config の [streetview] api_key を使う。画像取得(課金)の前に、無料の metadata で被覆を確認する。

use image::RgbImage;
use std::io::Read;

const BASE: &str = "https://maps.googleapis.com/maps/api/streetview";
const MAX_SIZE: u32 = 640; // Street View Static の最大サイズ

/// キーが設定されているか。
pub fn available(key: &str) -> bool {
    !key.trim().is_empty()
}

/// 指定地点の被覆確認(無料)。Ok(true)=画像あり / Ok(false)=無し / Err=通信・キー・API有効化等の問題。
pub fn metadata_ok(lat: f64, lon: f64, key: &str) -> Result<bool, String> {
    let url = format!("{BASE}/metadata?location={lat},{lon}&key={key}");
    let body = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .map_err(|e| format!("metadata: {e}"))?
        .into_string()
        .map_err(|e| format!("metadata read: {e}"))?;
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("\"status\":\"OK\"") {
        Ok(true)
    } else if compact.contains("\"status\":\"ZERO_RESULTS\"") || compact.contains("\"status\":\"NOT_FOUND\"") {
        Ok(false)
    } else {
        Err(status_of(&compact))
    }
}

/// compact JSON から status 値を拾ってメッセージ化(キー無効/API未有効化などの切り分け用)。
fn status_of(compact: &str) -> String {
    if let Some(i) = compact.find("\"status\":\"") {
        let rest = &compact[i + 10..];
        if let Some(j) = rest.find('"') {
            return format!("status={}", &rest[..j]);
        }
    }
    "取得エラー".to_string()
}

/// (lat,lon) から heading 方向へ dist_m メートル進んだ地点を返す。
/// Street View は隣接パノラマのグラフを持たないので、少し前進した点で再取得すると
/// metadata が最寄りパノラマにスナップする＝実質「前へ進む」挙動になる。
pub fn step(lat: f64, lon: f64, heading_deg: f64, dist_m: f64) -> (f64, f64) {
    let r = 6_371_000.0_f64; // 地球半径(m)
    let br = heading_deg.to_radians();
    let dr = dist_m / r;
    let (lat1, lon1) = (lat.to_radians(), lon.to_radians());
    let lat2 = (lat1.sin() * dr.cos() + lat1.cos() * dr.sin() * br.cos()).asin();
    let lon2 = lon1 + (br.sin() * dr.sin() * lat1.cos()).atan2(dr.cos() - lat1.sin() * lat2.sin());
    (lat2.to_degrees(), lon2.to_degrees())
}

/// 実写画像を RgbImage(w x h、最大 640) で返す。被覆が無ければ Err("この地点に画像なし")。
/// fov(画角・度)は小さいほどズームイン。Google Street View Static APIの有効範囲(10-120)にクランプ。
pub fn fetch(lat: f64, lon: f64, heading: i32, w: u32, h: u32, fov: f64, key: &str) -> Result<RgbImage, String> {
    if !available(key) {
        return Err("APIキー未設定".to_string());
    }
    match metadata_ok(lat, lon, key)? {
        true => {}
        false => return Err("この地点に画像なし".to_string()),
    }
    let (w, h) = (w.clamp(16, MAX_SIZE), h.clamp(16, MAX_SIZE));
    let hd = ((heading % 360) + 360) % 360;
    let fov = fov.clamp(10.0, 120.0);
    let url = format!("{BASE}?size={w}x{h}&location={lat},{lon}&heading={hd}&pitch=0&fov={fov}&key={key}");
    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|e| format!("streetview: {e}"))?;
    let mut buf = Vec::new();
    resp.into_reader()
        .read_to_end(&mut buf)
        .map_err(|e| format!("streetview read: {e}"))?;
    let img = image::load_from_memory(&buf)
        .map_err(|e| format!("画像デコード: {e}"))?
        .to_rgb8();
    Ok(img)
}
