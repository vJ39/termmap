// 座標変換 (Web Mercator) と距離・方位などの地理計算

pub const TILE: u32 = 256;

// ---- 座標変換 (Web Mercator, グローバルピクセル) ----
pub fn deg_to_pixel(lat: f64, lon: f64, z: u32) -> (f64, f64) {
    let latr = lat.to_radians();
    let n = (TILE as f64) * 2f64.powi(z as i32);
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - (latr.tan() + 1.0 / latr.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}
pub fn pixel_to_deg(px: f64, py: f64, z: u32) -> (f64, f64) {
    let n = (TILE as f64) * 2f64.powi(z as i32);
    let lon = px / n * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * py / n)).sinh().atan().to_degrees();
    (lat, lon)
}

// 緯度latズームzでの m/px (Web Mercator)
pub fn meters_per_pixel(lat: f64, z: u32) -> f64 {
    156543.033_92 * lat.to_radians().cos() / 2f64.powi(z as i32)
}

pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let r = 6371.0;
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let (dlat, dlon) = ((b.0 - a.0).to_radians(), (b.1 - a.1).to_radians());
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
}
pub fn bearing(from: (f64, f64), to: (f64, f64)) -> f64 {
    let (la1, la2) = (from.0.to_radians(), to.0.to_radians());
    let dlon = (to.1 - from.1).to_radians();
    let y = dlon.sin() * la2.cos();
    let x = la1.cos() * la2.sin() - la1.sin() * la2.cos() * dlon.cos();
    y.atan2(x).to_degrees().rem_euclid(360.0)
}
pub fn angdiff(a: f64, b: f64) -> f64 { let d = (a - b).abs() % 360.0; d.min(360.0 - d) }

/// 方位(度、北=0・時計回り)を8方向の矢印記号に変換する(実写のPegman風「方位くるくる」表示用)。
/// 北=↑ / 北東=↗ / 東=→ / 南東=↘ / 南=↓ / 南西=↙ / 西=← / 北西=↖ の45°刻みで最も近い方向を返す。
pub fn heading_arrow(heading_deg: f64) -> char {
    const ARROWS: [char; 8] = ['↑', '↗', '→', '↘', '↓', '↙', '←', '↖'];
    let h = heading_deg.rem_euclid(360.0);
    let idx = ((h / 45.0).round() as usize) % 8;
    ARROWS[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_deg_roundtrip() {
        for &(lat, lon, z) in &[(35.68, 139.76, 14u32), (0.0, 0.0, 5), (35.99, 139.08, 11)] {
            let (px, py) = deg_to_pixel(lat, lon, z);
            let (la, lo) = pixel_to_deg(px, py, z);
            assert!((la - lat).abs() < 1e-6 && (lo - lon).abs() < 1e-6);
        }
    }

    #[test]
    fn haversine_known() {
        let d = haversine_km((35.0, 139.0), (36.0, 139.0)); // 緯度1度 ≈ 111km
        assert!((d - 111.2).abs() < 1.0, "{d}");
    }

    #[test]
    fn bearing_cardinal() {
        assert!(angdiff(bearing((35.0, 139.0), (36.0, 139.0)), 0.0) < 1.0);  // 北
        assert!(angdiff(bearing((35.0, 139.0), (35.0, 140.0)), 90.0) < 1.0); // 東
        assert!((angdiff(350.0, 10.0) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn meters_per_pixel_halves_per_zoom() {
        let a = meters_per_pixel(35.0, 12);
        let b = meters_per_pixel(35.0, 13);
        assert!((a / b - 2.0).abs() < 1e-6); // ズーム+1で半分
    }

    #[test]
    fn heading_arrow_cardinal_directions() {
        assert_eq!(heading_arrow(0.0), '↑');
        assert_eq!(heading_arrow(90.0), '→');
        assert_eq!(heading_arrow(180.0), '↓');
        assert_eq!(heading_arrow(270.0), '←');
    }

    #[test]
    fn heading_arrow_intercardinal_directions() {
        assert_eq!(heading_arrow(45.0), '↗');
        assert_eq!(heading_arrow(135.0), '↘');
        assert_eq!(heading_arrow(225.0), '↙');
        assert_eq!(heading_arrow(315.0), '↖');
    }

    #[test]
    fn heading_arrow_wraps_around_360() {
        assert_eq!(heading_arrow(360.0), '↑'); // 360=0扱い
        assert_eq!(heading_arrow(-45.0), '↖'); // 負の角度もrem_euclidで正規化
        assert_eq!(heading_arrow(720.0 + 90.0), '→'); // 2周分足しても同じ
    }

    #[test]
    fn heading_arrow_rounds_to_nearest_45deg_bucket() {
        assert_eq!(heading_arrow(22.0), '↑');  // 0に近い
        assert_eq!(heading_arrow(23.0), '↗');  // 45に近い(22.5が境界)
        assert_eq!(heading_arrow(350.0), '↑'); // 360(=0)に近い(315との差35 vs 360との差10)
    }
}
