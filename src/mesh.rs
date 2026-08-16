// JIS X 0410「地域メッシュ」の1次/2次メッシュ計算。プロットデータ(交通量・主要道路・
// 通行規制)のディスクキャッシュを、緯度経度bboxではなく取得元が持つ自然な単位で区切るために使う。
// bboxを生のままキーにすると1pxパンするたびに別キーになりキャッシュがヒットしないため、
// 地理的に固定されたメッシュへ丸めてからキーにする(docs/plot-data-disk-cache-design.md §4)。
//
// 定義(実測ではなく規格そのもの):
//   1次メッシュ … 緯度40分(=2/3度)×経度1度。コード4桁 = p*100+u
//                  p = floor(lat*1.5) / u = floor(lon)-100
//   2次メッシュ … 1次を8×8に分割(緯度5分=1/12度 × 経度7分30秒=1/8度)。コード6桁 =
//                  1次コード*100 + row*10 + col (row=南から0..7 / col=西から0..7)
// std のみに依存し、ネットワークにもcrateにも触れない(単体テストで完結できるようにするため)。

// 1回の被覆計算で返すコードの上限。ズーム下限(plotlayer側)より広域では呼ばれない想定だが、
// 万一世界全体のbboxが渡っても数万件を列挙して1フレームを潰さないための安全弁。
// 上限に達したら「広すぎる」として空を返す(部分的な列挙を返すと取りこぼしに気づけないため)。
const MAX_CODES: usize = 256;

/// bbox(lat_min, lon_min, lat_max, lon_max)を覆う1次メッシュコードを全て列挙する。
/// 範囲が逆転している/日本のメッシュ空間(p,u が 0..=99)から外れる場合は空を返す。
pub fn primary_codes(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> Vec<u32> {
    if !(lat_min <= lat_max && lon_min <= lon_max) {
        return Vec::new();
    }
    let p0 = (lat_min * 1.5).floor() as i64;
    let p1 = (lat_max * 1.5).floor() as i64;
    let u0 = lon_min.floor() as i64 - 100;
    let u1 = lon_max.floor() as i64 - 100;
    if (p1 - p0 + 1).saturating_mul(u1 - u0 + 1) > MAX_CODES as i64 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for p in p0..=p1 {
        for u in u0..=u1 {
            if (0..100).contains(&p) && (0..100).contains(&u) {
                out.push((p * 100 + u) as u32);
            }
        }
    }
    out
}

/// 1次メッシュコード → (lat_min, lon_min, lat_max, lon_max)。
pub fn primary_bbox(code: u32) -> (f64, f64, f64, f64) {
    let p = (code / 100) as f64;
    let u = (code % 100) as f64;
    (p / 1.5, 100.0 + u, (p + 1.0) / 1.5, 101.0 + u)
}

/// bboxを覆う2次メッシュコード(6桁)を全て列挙する。制約は primary_codes と同じ。
pub fn secondary_codes(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> Vec<u32> {
    if !(lat_min <= lat_max && lon_min <= lon_max) {
        return Vec::new();
    }
    // 2次メッシュの格子は緯度1/12度・経度1/8度なので、その整数格子の上で数える。
    let q0 = (lat_min * 12.0).floor() as i64;
    let q1 = (lat_max * 12.0).floor() as i64;
    let r0 = (lon_min * 8.0).floor() as i64;
    let r1 = (lon_max * 8.0).floor() as i64;
    if (q1 - q0 + 1).saturating_mul(r1 - r0 + 1) > MAX_CODES as i64 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for q in q0..=q1 {
        for r in r0..=r1 {
            let p = q.div_euclid(8);
            let row = q.rem_euclid(8);
            let u = r.div_euclid(8) - 100;
            let col = r.rem_euclid(8);
            if (0..100).contains(&p) && (0..100).contains(&u) {
                out.push(((p * 100 + u) * 100 + row * 10 + col) as u32);
            }
        }
    }
    out
}

/// 2次メッシュコード → (lat_min, lon_min, lat_max, lon_max)。
pub fn secondary_bbox(code: u32) -> (f64, f64, f64, f64) {
    let (p_lat_min, p_lon_min, _, _) = primary_bbox(code / 100);
    let row = ((code % 100) / 10) as f64;
    let col = (code % 10) as f64;
    let lat_min = p_lat_min + row / 12.0;
    let lon_min = p_lon_min + col / 8.0;
    (lat_min, lon_min, lat_min + 1.0 / 12.0, lon_min + 1.0 / 8.0)
}

/// メッシュ矩形の各辺をわずかに内側へ寄せた矩形。取得元へ「このメッシュぶんだけ」を頼むとき、
/// 境界がちょうど次のメッシュの下端と一致して隣まで巻き込まれるのを防ぐ
/// (取得元は floor でメッシュへ割り戻すため、上端をそのまま渡すと隣のコードが混ざる)。
pub fn shrink(b: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    const EPS: f64 = 1e-6;
    (b.0 + EPS, b.1 + EPS, b.2 - EPS, b.3 - EPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_codes_single_point_is_tokyo_5339() {
        assert_eq!(primary_codes(35.68, 139.77, 35.68, 139.77), vec![5339]);
    }

    #[test]
    fn primary_codes_spans_multiple_cells() {
        let codes = primary_codes(35.0, 139.0, 36.5, 141.0);
        assert!(codes.len() > 1);
        assert!(codes.contains(&5339));
    }

    #[test]
    fn primary_codes_empty_on_inverted_range() {
        assert!(primary_codes(36.0, 139.0, 35.0, 140.0).is_empty()); // lat_min > lat_max
        assert!(primary_codes(35.0, 140.0, 36.0, 139.0).is_empty()); // lon_min > lon_max
    }

    #[test]
    fn primary_codes_skips_cells_outside_the_mesh_space() {
        // 南半球/西経はメッシュ空間(p,u が 0..=99)の外なので1件も出さない。
        assert!(primary_codes(-10.0, 139.0, -9.0, 140.0).is_empty());
        assert!(primary_codes(35.0, -140.0, 36.0, -139.0).is_empty());
    }

    #[test]
    fn primary_codes_returns_empty_when_the_area_is_absurdly_wide() {
        // 世界全体。列挙すると数万件になるので空(=取得しない)を返す。
        assert!(primary_codes(-85.0, -180.0, 85.0, 180.0).is_empty());
    }

    #[test]
    fn primary_bbox_roundtrips_with_primary_codes() {
        for code in [5339u32, 6441, 3624, 5235] {
            let (s, w, n, e) = primary_bbox(code);
            let mid_lat = (s + n) / 2.0;
            let mid_lon = (w + e) / 2.0;
            assert_eq!(primary_codes(mid_lat, mid_lon, mid_lat, mid_lon), vec![code], "code={code}");
        }
    }

    #[test]
    fn primary_bbox_has_the_regulation_sized_cell() {
        let (s, w, n, e) = primary_bbox(5339);
        assert!((s - 35.3333333).abs() < 1e-5, "s={s}");
        assert!((n - s - 2.0 / 3.0).abs() < 1e-9, "緯度幅は40分");
        assert!((w - 139.0).abs() < 1e-9);
        assert!((e - w - 1.0).abs() < 1e-9, "経度幅は1度");
    }

    #[test]
    fn secondary_codes_single_point_is_six_digits_inside_its_primary() {
        // 東京(35.68,139.77)は 1次5339 の row=4(南から5番目)・col=6(西から7番目)。
        assert_eq!(secondary_codes(35.68, 139.77, 35.68, 139.77), vec![533946]);
    }

    #[test]
    fn secondary_bbox_roundtrips_with_secondary_codes() {
        for code in [533946u32, 533900, 644177, 523504] {
            let (s, w, n, e) = secondary_bbox(code);
            let mid_lat = (s + n) / 2.0;
            let mid_lon = (w + e) / 2.0;
            assert_eq!(secondary_codes(mid_lat, mid_lon, mid_lat, mid_lon), vec![code], "code={code}");
        }
    }

    #[test]
    fn secondary_bbox_is_one_eighth_of_its_primary_in_both_axes() {
        let (ps, pw, pn, pe) = primary_bbox(5339);
        let (s, w, n, e) = secondary_bbox(533900); // row=0/col=0 なので1次の南西角と一致する
        assert!((s - ps).abs() < 1e-9);
        assert!((w - pw).abs() < 1e-9);
        assert!(((n - s) - (pn - ps) / 8.0).abs() < 1e-9);
        assert!(((e - w) - (pe - pw) / 8.0).abs() < 1e-9);
    }

    #[test]
    fn secondary_codes_cover_a_small_area_with_a_handful_of_cells() {
        // 約14km四方(z14の視野相当)。2次メッシュは約10km四方なので数枚で収まる。
        let codes = secondary_codes(35.60, 139.70, 35.72, 139.85);
        assert!(!codes.is_empty());
        assert!(codes.len() <= 9, "z14相当で{}枚は多すぎる", codes.len());
        assert!(codes.contains(&533946));
    }

    #[test]
    fn secondary_codes_returns_empty_when_the_area_is_absurdly_wide() {
        assert!(secondary_codes(20.0, 120.0, 46.0, 150.0).is_empty());
    }

    #[test]
    fn secondary_codes_empty_on_inverted_range() {
        assert!(secondary_codes(36.0, 139.0, 35.0, 140.0).is_empty());
    }

    // regulation.rs は元々自前で同じ1次メッシュ計算を持っていた(bboxを受け取って自分で
    // メッシュへ割る取得元仕様のため)。ここへ集約した際に値が変わっていないことを、
    // 当時のテストが固定していた既知値で担保する。
    #[test]
    fn primary_codes_matches_the_values_the_regulation_module_used_to_assert() {
        assert_eq!(primary_codes(35.68, 139.77, 35.68, 139.77), vec![5339]);
        let codes = primary_codes(35.0, 139.0, 36.5, 141.0);
        assert!(codes.contains(&5339));
        assert!(primary_codes(36.0, 139.0, 35.0, 140.0).is_empty());
    }

    #[test]
    fn shrink_pulls_every_edge_inward() {
        let (s, w, n, e) = shrink((35.0, 139.0, 36.0, 140.0));
        assert!(s > 35.0 && w > 139.0 && n < 36.0 && e < 140.0);
        // 内側へ寄せた矩形が覆うメッシュは、元の矩形が覆うメッシュの部分集合になる
        // (境界にちょうど乗っていたぶんだけが落ちる)。
        let before = primary_codes(35.0, 139.0, 36.0, 140.0);
        let after = primary_codes(s, w, n, e);
        assert!(!after.is_empty());
        assert!(after.iter().all(|c| before.contains(c)), "before={before:?} after={after:?}");
        assert!(after.len() < before.len(), "境界ぶんが落ちるはず: {after:?}");
    }

    // 1次メッシュの上端をそのまま渡すと隣のメッシュまで拾ってしまうこと、
    // shrink すればそれが起きないことを固定する(取得範囲がセル1枚に収まる保証)。
    #[test]
    fn shrink_keeps_a_primary_cell_from_leaking_into_its_neighbour() {
        let b = primary_bbox(5339);
        assert!(primary_codes(b.0, b.1, b.2, b.3).len() > 1, "素のbboxは境界で隣を拾う");
        let s = shrink(b);
        assert_eq!(primary_codes(s.0, s.1, s.2, s.3), vec![5339]);
    }

    #[test]
    fn shrink_keeps_a_secondary_cell_from_leaking_into_its_neighbour() {
        let b = secondary_bbox(533946);
        let s = shrink(b);
        assert_eq!(secondary_codes(s.0, s.1, s.2, s.3), vec![533946]);
    }
}
