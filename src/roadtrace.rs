// roadtrace: 道路断片(RoadFrag)の連結・区間切り出し・等間隔サンプリング。
// std のみ・外部crate禁止・crate:: 参照禁止(単体コンパイル可能)。
// 座標は (f64, f64) = (lat, lon)。

/// 1本の道路断片(OSM way等の分割単位)。
#[derive(Debug, Clone, PartialEq)]
pub struct RoadFrag {
    pub pts: Vec<(f64, f64)>,
    pub oneway: bool,
}

/// 座標の実質同一判定(結合点の重複除去用)。度単位の極小許容誤差。
fn approx_eq(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
}

/// 2点間の距離(メートル)。Haversine公式・地球半径6371km。
fn haversine_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lon1) = a;
    let (lat2, lon2) = b;
    let r = 6_371_000.0_f64;

    let phi1 = lat1.to_radians();
    let phi2 = lat2.to_radians();
    let d_phi = (lat2 - lat1).to_radians();
    let d_lambda = (lon2 - lon1).to_radians();

    let sin_d_phi = (d_phi / 2.0).sin();
    let sin_d_lambda = (d_lambda / 2.0).sin();

    let h = sin_d_phi * sin_d_phi + phi1.cos() * phi2.cos() * sin_d_lambda * sin_d_lambda;
    let h = h.clamp(0.0, 1.0);
    let c = 2.0 * h.sqrt().atan2((1.0 - h).sqrt());

    r * c
}

/// chain の末尾に new_pts を連結する。結合点で座標が重複する場合は先頭を1点除去する。
fn append_points<I: Iterator<Item = (f64, f64)>>(chain: &mut Vec<(f64, f64)>, mut new_pts: I) {
    let mut first = true;
    for p in new_pts.by_ref() {
        if first {
            first = false;
            if let Some(&last) = chain.last() {
                if approx_eq(last, p) {
                    continue;
                }
            }
        }
        chain.push(p);
    }
}

/// chain の先頭に new_pts (chain の前に続く順序で渡す)を連結する。
/// new_pts の最後の点が chain の先頭と重複する場合はその点を除去する。
fn prepend_points<I: Iterator<Item = (f64, f64)>>(chain: &mut Vec<(f64, f64)>, new_pts: I) {
    let mut prefix: Vec<(f64, f64)> = new_pts.collect();
    if let (Some(&last_new), Some(&first_chain)) = (prefix.last(), chain.first()) {
        if approx_eq(last_new, first_chain) {
            prefix.pop();
        }
    }
    prefix.extend(chain.drain(..));
    *chain = prefix;
}

/// 断片群を端点の最短接続で貪欲に1本の順序付きポリラインへ連結する。
/// 接続のため断片を反転することがある。連結点の重複座標は除去する。
pub fn assemble_polyline(frags: &[RoadFrag]) -> Vec<(f64, f64)> {
    let valid: Vec<&RoadFrag> = frags.iter().filter(|f| !f.pts.is_empty()).collect();
    if valid.is_empty() {
        return Vec::new();
    }

    let n = valid.len();
    let mut used = vec![false; n];
    let mut chain: Vec<(f64, f64)> = valid[0].pts.clone();
    used[0] = true;
    let mut remaining = n - 1;

    // mode: 0=chain末尾に順方向で追加, 1=chain末尾に逆方向で追加,
    //       2=chain先頭に順方向で連結(断片は前に来る), 3=chain先頭に逆方向で連結
    while remaining > 0 {
        let front = chain[0];
        let back = *chain.last().unwrap();

        let mut best: Option<(f64, usize, u8)> = None;

        for (i, f) in valid.iter().enumerate() {
            if used[i] {
                continue;
            }
            let fstart = f.pts[0];
            let fend = *f.pts.last().unwrap();

            let candidates = [
                (haversine_m(back, fstart), 0u8),
                (haversine_m(back, fend), 1u8),
                (haversine_m(fend, front), 2u8),
                (haversine_m(fstart, front), 3u8),
            ];

            for (d, mode) in candidates {
                let better = match best {
                    None => true,
                    Some((bd, _, _)) => d < bd,
                };
                if better {
                    best = Some((d, i, mode));
                }
            }
        }

        let (_, idx, mode) = best.expect("remaining fragments but no candidate found");
        used[idx] = true;
        remaining -= 1;

        let pts = &valid[idx].pts;
        match mode {
            0 => append_points(&mut chain, pts.iter().copied()),
            1 => append_points(&mut chain, pts.iter().rev().copied()),
            2 => prepend_points(&mut chain, pts.iter().copied()),
            3 => prepend_points(&mut chain, pts.iter().rev().copied()),
            _ => unreachable!(),
        }
    }

    chain
}

/// poly の中で p に最も近い点のindexを返す。poly は空でないこと。
fn nearest_index(poly: &[(f64, f64)], p: (f64, f64)) -> usize {
    let mut best_i = 0;
    let mut best_d = f64::INFINITY;
    for (i, &q) in poly.iter().enumerate() {
        let d = haversine_m(p, q);
        if d < best_d {
            best_d = d;
            best_i = i;
        }
    }
    best_i
}

/// from に最も近いindexと to に最も近いindexを求め、その間を進行順(from->to、
/// 必要なら反転)で内包スライスとして返す。
pub fn cut_segment(poly: &[(f64, f64)], from: (f64, f64), to: (f64, f64)) -> Vec<(f64, f64)> {
    if poly.is_empty() {
        return Vec::new();
    }

    let i_from = nearest_index(poly, from);
    let i_to = nearest_index(poly, to);

    if i_from <= i_to {
        poly[i_from..=i_to].to_vec()
    } else {
        let mut seg = poly[i_to..=i_from].to_vec();
        seg.reverse();
        seg
    }
}

/// 累積haversine距離でポリラインを歩き、約meters間隔で点を出す。
/// 先頭と末尾は必ず含める。
pub fn sample_every(poly: &[(f64, f64)], meters: f64) -> Vec<(f64, f64)> {
    if poly.len() < 2 {
        return poly.to_vec();
    }
    if !(meters > 0.0) {
        // 不正な間隔(0以下・NaN)は無限ループ回避のため先頭+末尾のみ返す
        return vec![poly[0], *poly.last().unwrap()];
    }

    let mut cum = vec![0.0_f64; poly.len()];
    for i in 1..poly.len() {
        cum[i] = cum[i - 1] + haversine_m(poly[i - 1], poly[i]);
    }
    let total = *cum.last().unwrap();

    let mut result = vec![poly[0]];

    if total > 0.0 {
        let mut seg_idx = 0usize;
        let mut target = meters;
        while target < total {
            while seg_idx < poly.len() - 2 && cum[seg_idx + 1] < target {
                seg_idx += 1;
            }
            let seg_len = cum[seg_idx + 1] - cum[seg_idx];
            let t = if seg_len > 0.0 {
                (target - cum[seg_idx]) / seg_len
            } else {
                0.0
            };
            let p0 = poly[seg_idx];
            let p1 = poly[seg_idx + 1];
            let interp = (p0.0 + (p1.0 - p0.0) * t, p0.1 + (p1.1 - p0.1) * t);
            result.push(interp);
            target += meters;
        }
    }

    result.push(*poly.last().unwrap());
    result
}

/// ポリラインの総距離(メートル)。
pub fn polyline_len(poly: &[(f64, f64)]) -> f64 {
    poly.windows(2).map(|w| haversine_m(w[0], w[1])).sum()
}

/// ルート再生(プレビュー走行)の1フレーム分の進行距離(メートル)。想定巡航速度(km/h)×再生
/// 倍率×経過秒数。倍率は0.05未満に張り付かせて実質的な停止(0除算的な張り付き)を避ける。
pub fn play_step_distance_m(speed_kmh: f64, multiplier: f64, dt_secs: f64) -> f64 {
    speed_kmh * 1000.0 / 3600.0 * multiplier.max(0.05) * dt_secs
}

/// ポリライン先頭から dist_m メートル進んだ地点を線形補間で返す。範囲外は端にクランプ。
pub fn point_at(poly: &[(f64, f64)], dist_m: f64) -> (f64, f64) {
    if poly.is_empty() {
        return (0.0, 0.0);
    }
    if poly.len() == 1 || dist_m <= 0.0 {
        return poly[0];
    }
    let mut acc = 0.0;
    for w in poly.windows(2) {
        let seg = haversine_m(w[0], w[1]);
        if acc + seg >= dist_m {
            let t = if seg > 0.0 { (dist_m - acc) / seg } else { 0.0 };
            return (w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t);
        }
        acc += seg;
    }
    *poly.last().unwrap()
}

/// poly を「連続点間距離 > gap_m」で分割し、center に最も近い点を含むサブセグメントだけを返す。
/// assemble_polyline は散らばった断片も貪欲連結で1本にするため大ジャンプが混じることがある。
/// これを gap_m でぶった切り、view中心に一番近い連結成分(=いま見ている道)だけを塊として残す。
/// poly が空なら空Vecを返す。
pub fn nearest_segment(poly: &[(f64, f64)], center: (f64, f64), gap_m: f64) -> Vec<(f64, f64)> {
    if poly.is_empty() {
        return Vec::new();
    }
    // 連続点間距離が gap_m を超えた所で新しいサブセグメントに分ける
    let mut segments: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut cur: Vec<(f64, f64)> = vec![poly[0]];
    for i in 1..poly.len() {
        if haversine_m(poly[i - 1], poly[i]) > gap_m {
            segments.push(std::mem::take(&mut cur));
        }
        cur.push(poly[i]);
    }
    segments.push(cur);

    // center に最も近い点を含むサブセグメントを選ぶ
    let mut best_seg = 0usize;
    let mut best_d = f64::INFINITY;
    for (si, seg) in segments.iter().enumerate() {
        for &p in seg {
            let d = haversine_m(center, p);
            if d < best_d {
                best_d = d;
                best_seg = si;
            }
        }
    }
    segments.into_iter().nth(best_seg).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_polyline_handles_empty_input() {
        let frags: Vec<RoadFrag> = Vec::new();
        assert_eq!(assemble_polyline(&frags), Vec::new());
    }

    #[test]
    fn assemble_polyline_handles_single_fragment() {
        let pts = vec![(35.0, 139.0), (35.001, 139.0)];
        let frags = vec![RoadFrag {
            pts: pts.clone(),
            oneway: false,
        }];
        assert_eq!(assemble_polyline(&frags), pts);
    }

    #[test]
    fn assemble_reassembles_shuffled_reversed_line() {
        // 既知の直線 p0..p8 (等間隔・南北方向)
        let base: Vec<(f64, f64)> = (0..=8)
            .map(|i| (35.0 + i as f64 * 0.001, 139.0))
            .collect();

        let frag_a = RoadFrag {
            pts: base[0..=3].to_vec(), // p0,p1,p2,p3
            oneway: false,
        };
        let frag_b = RoadFrag {
            pts: base[3..=6].to_vec(), // p3,p4,p5,p6
            oneway: true,
        };
        let frag_c = RoadFrag {
            pts: base[6..=8].to_vec(), // p6,p7,p8
            oneway: false,
        };

        // 反転 + シャッフルした状態で渡す
        let frag_b_rev = RoadFrag {
            pts: frag_b.pts.into_iter().rev().collect(),
            oneway: true,
        };
        let frag_c_rev = RoadFrag {
            pts: frag_c.pts.into_iter().rev().collect(),
            oneway: false,
        };

        let frags = vec![frag_c_rev, frag_a, frag_b_rev];
        let result = assemble_polyline(&frags);

        let forward = base.clone();
        let mut backward = base.clone();
        backward.reverse();

        assert!(
            result == forward || result == backward,
            "unexpected order: {:?}",
            result
        );
        // 結合点(p3, p6)の重複が除去され、元の点数と一致すること
        assert_eq!(result.len(), base.len());
    }

    #[test]
    fn cut_segment_handles_empty_and_single_point() {
        let empty: Vec<(f64, f64)> = Vec::new();
        assert_eq!(cut_segment(&empty, (0.0, 0.0), (1.0, 1.0)), Vec::new());

        let single = vec![(35.0, 139.0)];
        assert_eq!(cut_segment(&single, (0.0, 0.0), (1.0, 1.0)), single);
    }

    #[test]
    fn cut_segment_returns_correct_range_and_direction() {
        let poly: Vec<(f64, f64)> = (0..=8)
            .map(|i| (35.0 + i as f64 * 0.001, 139.0))
            .collect();

        // ぴったり点上でなく、僅かにずらした座標でも最近傍indexが解決できること
        let from = (poly[2].0 + 0.00001, poly[2].1);
        let to = (poly[6].0 - 0.00001, poly[6].1);

        let forward = cut_segment(&poly, from, to);
        assert_eq!(forward, poly[2..=6].to_vec());

        let backward = cut_segment(&poly, to, from);
        let mut expected_backward = poly[2..=6].to_vec();
        expected_backward.reverse();
        assert_eq!(backward, expected_backward);
    }

    #[test]
    fn sample_every_handles_empty_and_single_point() {
        let empty: Vec<(f64, f64)> = Vec::new();
        assert_eq!(sample_every(&empty, 100.0), Vec::new());

        let single = vec![(35.0, 139.0)];
        assert_eq!(sample_every(&single, 100.0), single);
    }

    #[test]
    fn sample_every_spaces_points_along_meridian() {
        // 経度固定・緯線に沿った直線なら緯度差とメートル距離が正確に対応するため、
        // 補間点の間隔を厳密に検証できる。
        let r = 6_371_000.0_f64;
        let total_m = 10_000.0_f64;
        let dlat_deg = (total_m / r).to_degrees();
        let lat0 = 35.0;
        let lon0 = 139.0;
        let poly = vec![(lat0, lon0), (lat0 + dlat_deg, lon0)];

        let sampled = sample_every(&poly, 1000.0);

        assert_eq!(sampled.first(), Some(&poly[0]));
        assert_eq!(sampled.last(), Some(&poly[1]));
        // 10km / 1km 間隔 => 先頭+9個の内部点+末尾 = 11点
        assert_eq!(sampled.len(), 11);

        for w in sampled.windows(2) {
            let d = haversine_m(w[0], w[1]);
            assert!((d - 1000.0).abs() < 1.0, "unexpected spacing: {d}");
        }
    }

    #[test]
    fn sample_every_handles_non_positive_meters_safely() {
        let poly = vec![(35.0, 139.0), (35.001, 139.0), (35.002, 139.0)];
        let result = sample_every(&poly, 0.0);
        assert_eq!(result.first(), Some(&poly[0]));
        assert_eq!(result.last(), Some(&poly[poly.len() - 1]));
    }

    #[test]
    fn play_step_distance_scales_with_speed_and_time() {
        // 40km/h = 約11.111...m/s。等倍1.0x・1秒でその分だけ進む。
        let d = play_step_distance_m(40.0, 1.0, 1.0);
        assert!((d - 11.111_111_111).abs() < 1e-6, "d={d}");
    }

    #[test]
    fn play_step_distance_scales_with_multiplier() {
        let base = play_step_distance_m(40.0, 1.0, 1.0);
        let doubled = play_step_distance_m(40.0, 2.0, 1.0);
        assert!((doubled - base * 2.0).abs() < 1e-9);
    }

    #[test]
    fn play_step_distance_zero_dt_is_zero() {
        assert_eq!(play_step_distance_m(40.0, 1.0, 0.0), 0.0);
    }

    #[test]
    fn play_step_distance_clamps_multiplier_floor() {
        // 倍率が0や負でも0.05に張り付き、進行が完全に止まらない(0除算的な停止を避ける)。
        let d_zero = play_step_distance_m(40.0, 0.0, 1.0);
        let d_floor = play_step_distance_m(40.0, 0.05, 1.0);
        assert!((d_zero - d_floor).abs() < 1e-9);
        assert!(d_zero > 0.0);
        let d_neg = play_step_distance_m(40.0, -5.0, 1.0);
        assert!((d_neg - d_floor).abs() < 1e-9);
    }

    #[test]
    fn point_at_interpolates_and_clamps() {
        // 経線上の約1.11km区間(0.01度)。中点付近を補間で拾えること。
        let poly = vec![(35.0, 139.0), (35.01, 139.0)];
        let total = polyline_len(&poly);
        assert!(total > 1000.0 && total < 1200.0);
        let mid = point_at(&poly, total / 2.0);
        assert!((mid.0 - 35.005).abs() < 1e-4 && (mid.1 - 139.0).abs() < 1e-9);
        assert_eq!(point_at(&poly, 0.0), poly[0]); // 先頭
        assert_eq!(point_at(&poly, total * 2.0), poly[1]); // 範囲外→末尾
    }

    #[test]
    fn nearest_segment_handles_empty_and_single() {
        let empty: Vec<(f64, f64)> = Vec::new();
        assert!(nearest_segment(&empty, (35.0, 139.0), 500.0).is_empty());

        let single = vec![(35.0, 139.0)];
        assert_eq!(nearest_segment(&single, (0.0, 0.0), 500.0), single);
    }

    #[test]
    fn nearest_segment_keeps_center_cluster() {
        // 2つの塊を大ジャンプ(gap超え)で繋いだポリライン。
        // クラスタA(35.0付近)とクラスタB(36.0付近)は約111km離れており、
        // 各塊内の連続点間は約111m(gap 500m未満)で繋がっている。
        let poly = vec![
            (35.000, 139.0), (35.001, 139.0), (35.002, 139.0), // A
            (36.000, 139.0), (36.001, 139.0), (36.002, 139.0), // B(Aから大ジャンプ)
        ];
        let cluster_a = vec![(35.000, 139.0), (35.001, 139.0), (35.002, 139.0)];
        let cluster_b = vec![(36.000, 139.0), (36.001, 139.0), (36.002, 139.0)];

        // 中心をA側に置く → A の塊だけ残る
        assert_eq!(nearest_segment(&poly, (35.001, 139.0), 500.0), cluster_a);
        // 中心をB側に置く → B の塊だけ残る
        assert_eq!(nearest_segment(&poly, (36.001, 139.0), 500.0), cluster_b);
    }
}
