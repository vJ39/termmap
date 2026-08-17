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
/// 必要セル数が MAX_CODES を超える場合も空を返す(交通量・規制・道路の安全弁。
/// 過去災害/境界はこの安全弁を持たない `primary_codes_unbounded` を使う)。
pub fn primary_codes(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> Vec<u32> {
    primary_codes_impl(lat_min, lon_min, lat_max, lon_max, Some(MAX_CODES))
}

/// primary_codes と同じ列挙だが、MAX_CODES の安全弁を持たない。過去災害/境界は
/// plotlayer::PlotLayer 側で1回のジョブのセル数上限を外しているため(設計
/// docs/disaster-choropleth-unlimited-zoom-design.md §3.1)、こちら側の列挙でも
/// 安全弁に当たって黙って0件を返す状態を作らない。日本のメッシュ空間(p,uが0..=99)の
/// 外は変わらず除外するので、実際に返る件数は最大10,000(100×100)に収まる。
pub fn primary_codes_unbounded(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> Vec<u32> {
    primary_codes_impl(lat_min, lon_min, lat_max, lon_max, None)
}

fn primary_codes_impl(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64, max_codes: Option<usize>) -> Vec<u32> {
    if !(lat_min <= lat_max && lon_min <= lon_max) {
        return Vec::new();
    }
    let p0 = (lat_min * 1.5).floor() as i64;
    let p1 = (lat_max * 1.5).floor() as i64;
    let u0 = lon_min.floor() as i64 - 100;
    let u1 = lon_max.floor() as i64 - 100;
    if let Some(cap) = max_codes {
        if (p1 - p0 + 1).saturating_mul(u1 - u0 + 1) > cap as i64 {
            return Vec::new();
        }
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

// ---- 2分の1地域メッシュ(500mメッシュ・9桁) ----
// 3次メッシュ(約1km)を4分割したもの。国土数値情報の500mメッシュ別将来推計人口が使う単位で、
// 桁の意味は次のとおり(docs/population-mesh-overlay-design.md §2.4)。
//
//   MESH_ID = p p u u  r c  r3 c3  q      例: 523351132
//             └1次┘   └2次┘└3次┘ └500m┘
//   q: 1=南西 2=南東 3=北西 4=北東
//   高さ 1/240 度(緯度15秒) / 幅 1/160 度(経度22.5秒)
//
// 500mメッシュの格子は「緯度を1/240度・経度を1/160度で刻んだ整数格子」そのものなので、
// 下の2関数はどちらもその格子番号を経由して計算する(secondary_codes が 1/12・1/8 の格子を
// 経由しているのと同じ流儀)。1次メッシュ1枚は緯度160×経度160区画に分かれる。

// 1次メッシュ1枚に入る500mメッシュの数(緯度・経度とも)。2/3度 ÷ 1/240度 = 1度 ÷ 1/160度 = 160。
const HALF_PER_PRIMARY: i64 = 160;

/// 9桁の2分の1地域メッシュコード → (lat_min, lon_min, lat_max, lon_max)。
/// 桁が壊れている(q が 1..=4 の外など)場合も矩形は返す(呼び出し側で弾く必要が無いように、
/// 各桁を取り出す位置だけを見て素直に計算する)。
pub fn half_mesh_bbox(code: u32) -> (f64, f64, f64, f64) {
    let q = code % 10; // 500m区画(1=南西 2=南東 3=北西 4=北東)
    let c3 = (code / 10) % 10; // 3次メッシュ 経度側
    let r3 = (code / 100) % 10; // 3次メッシュ 緯度側
    let c = (code / 1000) % 10; // 2次メッシュ 経度側
    let r = (code / 10000) % 10; // 2次メッシュ 緯度側
    let u = (code / 100_000) % 100; // 1次メッシュ 経度側
    let p = code / 10_000_000; // 1次メッシュ 緯度側
    let qi = q.saturating_sub(1).min(3); // 0..=3 へ丸める(0 と 1 はどちらも南西)
    let lat_min = p as f64 / 1.5 + r as f64 / 12.0 + r3 as f64 / 120.0 + (qi / 2) as f64 / 240.0;
    let lon_min = 100.0 + u as f64 + c as f64 / 8.0 + c3 as f64 / 80.0 + (qi % 2) as f64 / 160.0;
    (lat_min, lon_min, lat_min + 1.0 / 240.0, lon_min + 1.0 / 160.0)
}

/// 緯度経度 → その点を含む2分の1地域メッシュコード(9桁)。half_mesh_bbox の逆。
/// 日本のメッシュ空間(p,u が 0..=99)の外なら None。
/// 中心のクロスヘアが指すメッシュの実数値を出す(設計 §7.6)ために使う。
pub fn half_mesh_code(lat: f64, lon: f64) -> Option<u32> {
    if !lat.is_finite() || !lon.is_finite() {
        return None;
    }
    // 500mメッシュの整数格子番号(南西端を含む・北東端は次の区画)。
    let ga = (lat * 240.0).floor() as i64;
    let gb = (lon * 160.0).floor() as i64;
    let p = ga.div_euclid(HALF_PER_PRIMARY);
    let u = gb.div_euclid(HALF_PER_PRIMARY) - 100;
    if !(0..100).contains(&p) || !(0..100).contains(&u) {
        return None;
    }
    let ia = ga.rem_euclid(HALF_PER_PRIMARY); // 1次メッシュ内の緯度側 0..160
    let ib = gb.rem_euclid(HALF_PER_PRIMARY); // 同 経度側 0..160
    let (r, r3, half_r) = (ia / 20, (ia % 20) / 2, ia % 2);
    let (c, c3, half_c) = (ib / 20, (ib % 20) / 2, ib % 2);
    let q = half_r * 2 + half_c + 1; // 1=南西 2=南東 3=北西 4=北東
    Some((p * 10_000_000 + u * 100_000 + r * 10_000 + c * 1_000 + r3 * 100 + c3 * 10 + q) as u32)
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

    // primary_codes_unbounded は MAX_CODES の安全弁を持たない(過去災害/境界だけが使う。
    // 設計 docs/disaster-choropleth-unlimited-zoom-design.md)。同じ入力で通常版と結果が
    // 一致することを、安全弁に当たらない範囲でまず確認する。
    #[test]
    fn primary_codes_unbounded_matches_primary_codes_when_under_the_cap() {
        let cases = [
            (35.68, 139.77, 35.68, 139.77),
            (35.0, 136.5, 36.0, 141.0),
            (36.0, 139.0, 35.0, 140.0), // 範囲逆転(空になるはず)
            (-10.0, 139.0, -9.0, 140.0), // メッシュ空間の外
        ];
        for (a, b, c, d) in cases {
            assert_eq!(primary_codes_unbounded(a, b, c, d), primary_codes(a, b, c, d), "({a},{b},{c},{d})");
        }
    }

    // 通常版が安全弁で空を返す(=256セル超)入力でも、unbounded版はセルを返す。
    #[test]
    fn primary_codes_unbounded_does_not_give_up_on_absurdly_wide_areas() {
        assert!(primary_codes(-85.0, -180.0, 85.0, 180.0).is_empty(), "前提: 通常版は空");
        // 世界全体は日本のメッシュ空間(p,u が 0..=99)の外がほとんどなので、実際に返る件数は
        // 上限10,000(100×100)以下に収まる。0件ではなく、かつ現実的な件数であることを見る。
        let n = primary_codes_unbounded(-85.0, -180.0, 85.0, 180.0).len();
        assert!(n > 0, "unbounded版は空を返してはいけない");
        assert!(n <= 10_000, "日本のメッシュ空間(100×100)を超えるのはおかしい: {n}");
    }

    #[test]
    fn primary_codes_unbounded_still_rejects_inverted_ranges() {
        assert!(primary_codes_unbounded(36.0, 139.0, 35.0, 140.0).is_empty());
        assert!(primary_codes_unbounded(35.0, 140.0, 36.0, 139.0).is_empty());
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

    // ---- 2分の1地域メッシュ(500mメッシュ) ----

    // 設計 §2.4 / §12 の既知値。実データ(鳥取)の feature と一致することを確認済みの値。
    #[test]
    fn half_mesh_bbox_matches_the_known_value_from_the_design() {
        let (s, w, n, e) = half_mesh_bbox(523351132);
        assert!((s - 35.0916666).abs() < 1e-6, "s={s}");
        assert!((w - 133.16875).abs() < 1e-9, "w={w}");
        assert!((n - 35.0958333).abs() < 1e-6, "n={n}");
        assert!((e - 133.175).abs() < 1e-9, "e={e}");
    }

    // 1枚の大きさは緯度15秒(1/240度)×経度22.5秒(1/160度)で、コードによらず一定。
    #[test]
    fn half_mesh_bbox_is_always_fifteen_by_twenty_two_point_five_seconds() {
        for code in [523351132u32, 533945764, 644142001, 362317894] {
            let (s, w, n, e) = half_mesh_bbox(code);
            assert!((n - s - 1.0 / 240.0).abs() < 1e-12, "code={code}");
            assert!((e - w - 1.0 / 160.0).abs() < 1e-12, "code={code}");
        }
    }

    // q=1..4 が 南西/南東/北西/北東 の順に並ぶ(3次メッシュ1枚を4分割している)。
    #[test]
    fn the_four_quadrants_tile_their_tertiary_mesh() {
        let base = 52335113; // 3次メッシュ(8桁)
        let sw = half_mesh_bbox(base * 10 + 1);
        let se = half_mesh_bbox(base * 10 + 2);
        let nw = half_mesh_bbox(base * 10 + 3);
        let ne = half_mesh_bbox(base * 10 + 4);
        assert!((sw.0 - se.0).abs() < 1e-12, "南の2枚は緯度が同じ");
        assert!((nw.0 - ne.0).abs() < 1e-12, "北の2枚は緯度が同じ");
        assert!(sw.0 < nw.0, "北の方が緯度が高い");
        assert!((sw.1 - nw.1).abs() < 1e-12, "西の2枚は経度が同じ");
        assert!(sw.1 < se.1, "東の方が経度が高い");
        // 4枚を合わせると3次メッシュ1枚(緯度1/120度・経度1/80度)をちょうど覆う。
        assert!((ne.2 - sw.0 - 1.0 / 120.0).abs() < 1e-12);
        assert!((ne.3 - sw.1 - 1.0 / 80.0).abs() < 1e-12);
        // 南西の北東角が、北東の南西角になる(隙間も重なりも無い)。
        assert!((sw.2 - nw.0).abs() < 1e-12);
        assert!((sw.3 - se.1).abs() < 1e-12);
    }

    // 500mメッシュ4枚×4枚…が上位のメッシュに入れ子で収まる(1次/2次との整合)。
    #[test]
    fn half_mesh_nests_inside_its_primary_and_secondary_mesh() {
        let code = 533946123;
        let (s, w, n, e) = half_mesh_bbox(code);
        let (ps, pw, pn, pe) = primary_bbox(code / 100_000); // 上4桁 = 1次メッシュ
        assert!(ps <= s && n <= pn && pw <= w && e <= pe, "1次からはみ出している");
        let (ss, sw_, sn, se_) = secondary_bbox(code / 1_000); // 上6桁 = 2次メッシュ
        assert!(ss <= s && n <= sn && sw_ <= w && e <= se_, "2次からはみ出している");
    }

    #[test]
    fn half_mesh_code_is_the_inverse_of_half_mesh_bbox() {
        for code in [523351132u32, 533946123, 644142001, 362317894, 533945764] {
            let (s, w, n, e) = half_mesh_bbox(code);
            let (mid_lat, mid_lon) = ((s + n) / 2.0, (w + e) / 2.0);
            assert_eq!(half_mesh_code(mid_lat, mid_lon), Some(code), "code={code}");
            // 南西角のすぐ内側も同じコードになる(境界ちょうどは浮動小数の丸めでどちらに転ぶか
            // 決まらないので、区画の内側であることが分かる最小の量だけずらして見る)。
            assert_eq!(half_mesh_code(s + 1e-9, w + 1e-9), Some(code), "南西角 code={code}");
        }
    }

    #[test]
    fn half_mesh_code_uses_all_four_quadrant_digits() {
        let base = half_mesh_bbox(523351131); // 南西
        let (s, w) = (base.0, base.1);
        let (dy, dx) = (1.0 / 480.0, 1.0 / 320.0); // 500m区画の半分
        assert_eq!(half_mesh_code(s + dy, w + dx), Some(523351131), "南西");
        assert_eq!(half_mesh_code(s + dy, w + dx + 1.0 / 160.0), Some(523351132), "南東");
        assert_eq!(half_mesh_code(s + dy + 1.0 / 240.0, w + dx), Some(523351133), "北西");
        assert_eq!(half_mesh_code(s + dy + 1.0 / 240.0, w + dx + 1.0 / 160.0), Some(523351134), "北東");
    }

    #[test]
    fn half_mesh_code_is_none_outside_the_japanese_mesh_space() {
        assert!(half_mesh_code(48.85, 2.35).is_none()); // パリ
        assert!(half_mesh_code(-33.87, 151.21).is_none()); // シドニー(南半球)
        assert!(half_mesh_code(f64::NAN, 139.0).is_none());
        assert!(half_mesh_code(35.0, f64::INFINITY).is_none());
    }

    // 東京駅は 2次メッシュ 533946 の中。上6桁が secondary_codes の結果と一致する。
    #[test]
    fn half_mesh_code_agrees_with_the_secondary_mesh_of_the_same_point() {
        let code = half_mesh_code(35.68, 139.77).expect("東京駅");
        assert_eq!(code / 1_000, 533946);
        assert_eq!(secondary_codes(35.68, 139.77, 35.68, 139.77), vec![code / 1_000]);
    }

}
