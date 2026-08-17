// 多角形の純粋な幾何(内外判定と外接矩形)。std だけに依存し crate:: を参照しない。
// 設計は docs/disaster-choropleth-design.md §5.2。
//
// muni.rs(気象庁 class20s の取得)から分けてあるのは、#79(気象警報)が class10s の区域に
// 対して同じ内外判定を必要とするため。先に分けておけば #79 はこのモジュールをそのまま使える。
//
// 座標系は持たない。呼び出し側が (第1成分, 第2成分) の意味を決める。termmap では
// (緯度, 経度) の組で呼ぶので、rings_bbox の戻り値は plotlayer::Bbox と同じ
// (lat_min, lon_min, lat_max, lon_max) の並びになる。
//
// リング列は「外周・穴・離島」を区別せず1つの配列に平らに並べたものを受ける。even-odd 規則
// なら区別せずに1回の走査で正しく判定できるため(GeoJSON の Polygon/MultiPolygon のリングを
// そのまま並べるだけでよい)。class20s は実測でリングの巻き方向が不統一(CW/CCW が混在)なので、
// 向きに依存する nonzero winding は使えない。

/// 点がリング列の内側かを even-odd(レイキャスティング)で判定する。
///
/// 辺の採否は `(b_i > pb) != (b_j > pb)` の半開区間規則にする。頂点をちょうど通る走査線で
/// 交点を二重に数えて内外が裏返るのを防ぐため。この規則の結果として、境界線上の点は
/// 「第1成分が小さい側の辺は内側・大きい側の辺は外側」という一貫した扱いになる
/// (隣り合う区域で同じ点が二重に当たらない)。
///
/// 頂点が2つ以下のリング(線・点に潰れたもの)は面を持たないので無視する。空配列なら false。
pub fn point_in_rings(rings: &[Vec<(f64, f64)>], pt: (f64, f64)) -> bool {
    let (pa, pb) = pt;
    if !pa.is_finite() || !pb.is_finite() {
        return false;
    }
    let mut inside = false;
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let mut j = ring.len() - 1;
        for i in 0..ring.len() {
            let (ai, bi) = ring[i];
            let (aj, bj) = ring[j];
            if (bi > pb) != (bj > pb) {
                // 分母は 0 にならない(上の条件が bi != bj を含意する)。
                let t = (pb - bi) / (bj - bi);
                if ai + t * (aj - ai) > pa {
                    inside = !inside;
                }
            }
            j = i;
        }
    }
    inside
}

/// リング列の外接矩形 `(a_min, b_min, a_max, b_max)`。頂点が1つも無ければ None。
/// 非有限な座標は無かったものとして飛ばす(壊れたデータで矩形全体が NaN にならないように)。
pub fn rings_bbox(rings: &[Vec<(f64, f64)>]) -> Option<(f64, f64, f64, f64)> {
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let mut seen = false;
    for ring in rings {
        for &(a, c) in ring {
            if !a.is_finite() || !c.is_finite() {
                continue;
            }
            b.0 = b.0.min(a);
            b.1 = b.1.min(c);
            b.2 = b.2.max(a);
            b.3 = b.3.max(c);
            seen = true;
        }
    }
    seen.then_some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 一辺10の正方形(反時計回り)。
    fn square() -> Vec<Vec<(f64, f64)>> {
        vec![vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]]
    }

    // 凹多角形(L字)。(0,0)-(10,0)-(10,4)-(4,4)-(4,10)-(0,10)
    fn ell() -> Vec<Vec<(f64, f64)>> {
        vec![vec![(0.0, 0.0), (10.0, 0.0), (10.0, 4.0), (4.0, 4.0), (4.0, 10.0), (0.0, 10.0)]]
    }

    // 外周(0..10)の中に穴(3..7)。even-odd なので外周と穴を区別せず並べるだけでよい。
    fn with_hole() -> Vec<Vec<(f64, f64)>> {
        vec![
            vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
            vec![(3.0, 3.0), (7.0, 3.0), (7.0, 7.0), (3.0, 7.0)],
        ]
    }

    // 離島(多重ポリゴン)。離れた2つの正方形。
    fn two_islands() -> Vec<Vec<(f64, f64)>> {
        vec![
            vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)],
            vec![(50.0, 50.0), (52.0, 50.0), (52.0, 52.0), (50.0, 52.0)],
        ]
    }

    #[test]
    fn a_point_inside_a_simple_rectangle_is_inside() {
        assert!(point_in_rings(&square(), (5.0, 5.0)));
        assert!(point_in_rings(&square(), (0.1, 0.1)));
        assert!(point_in_rings(&square(), (9.9, 9.9)));
    }

    #[test]
    fn a_point_outside_a_simple_rectangle_is_outside() {
        for p in [(-1.0, 5.0), (11.0, 5.0), (5.0, -1.0), (5.0, 11.0), (100.0, 100.0)] {
            assert!(!point_in_rings(&square(), p), "{p:?}");
        }
    }

    #[test]
    fn the_notch_of_a_concave_polygon_is_outside() {
        assert!(point_in_rings(&ell(), (2.0, 8.0)), "縦棒の中");
        assert!(point_in_rings(&ell(), (8.0, 2.0)), "横棒の中");
        assert!(!point_in_rings(&ell(), (8.0, 8.0)), "L字のへこみは外");
    }

    #[test]
    fn a_hole_reads_as_outside() {
        assert!(point_in_rings(&with_hole(), (1.0, 5.0)), "穴の外側のドーナツ部分");
        assert!(!point_in_rings(&with_hole(), (5.0, 5.0)), "穴の中は外");
        assert!(point_in_rings(&with_hole(), (8.5, 5.0)), "穴の反対側のドーナツ部分");
    }

    #[test]
    fn every_island_of_a_multipolygon_counts() {
        assert!(point_in_rings(&two_islands(), (1.0, 1.0)));
        assert!(point_in_rings(&two_islands(), (51.0, 51.0)));
        assert!(!point_in_rings(&two_islands(), (25.0, 25.0)), "島と島の間の海は外");
    }

    // 頂点をちょうど通る走査線。半開区間規則が効いていないと、頂点で交点を2回数えて
    // その走査線だけ内外が裏返る。
    #[test]
    fn a_scanline_through_a_vertex_does_not_flip_the_result() {
        // 菱形。b=5 の走査線が左右の頂点(0,5)/(10,5)をちょうど通る。
        let diamond = vec![vec![(5.0, 0.0), (10.0, 5.0), (5.0, 10.0), (0.0, 5.0)]];
        assert!(point_in_rings(&diamond, (5.0, 5.0)), "菱形の中心");
        assert!(!point_in_rings(&diamond, (-1.0, 5.0)), "頂点の外側(左)");
        assert!(!point_in_rings(&diamond, (11.0, 5.0)), "頂点の外側(右)");
        // 三角形の頂点を通る走査線(b=0 が底辺と重なる)でも裏返らない。
        let tri = vec![vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)]];
        assert!(point_in_rings(&tri, (5.0, 1.0)));
        assert!(!point_in_rings(&tri, (5.0, -0.1)));
    }

    // 境界線上の点。隣り合う区域で同じ点が二重に当たらないよう、片側の辺だけを内側と数える。
    #[test]
    fn a_point_on_the_boundary_is_counted_on_exactly_one_side() {
        assert!(point_in_rings(&square(), (0.0, 5.0)), "第1成分が小さい側の辺は内側");
        assert!(!point_in_rings(&square(), (10.0, 5.0)), "大きい側の辺は外側");
        // 隣接する2区域(0..10 と 10..20)の境界に立つ点は、どちらか一方にだけ入る。
        let right = vec![vec![(10.0, 0.0), (20.0, 0.0), (20.0, 10.0), (10.0, 10.0)]];
        let hits = [point_in_rings(&square(), (10.0, 5.0)), point_in_rings(&right, (10.0, 5.0))];
        assert_eq!(hits.iter().filter(|h| **h).count(), 1, "{hits:?}");
    }

    #[test]
    fn degenerate_rings_do_not_panic() {
        assert!(!point_in_rings(&[], (1.0, 1.0)));
        assert!(!point_in_rings(&[Vec::new()], (1.0, 1.0)));
        assert!(!point_in_rings(&[vec![(0.0, 0.0)]], (0.0, 0.0)), "点は面を持たない");
        assert!(!point_in_rings(&[vec![(0.0, 0.0), (10.0, 10.0)]], (5.0, 5.0)), "線分も面を持たない");
        // 潰れたリングが混ざっていても、まともなリングの判定は変わらない。
        let mut mixed = square();
        mixed.push(vec![(1.0, 1.0), (2.0, 2.0)]);
        assert!(point_in_rings(&mixed, (5.0, 5.0)));
    }

    #[test]
    fn non_finite_input_is_refused_rather_than_guessed() {
        assert!(!point_in_rings(&square(), (f64::NAN, 5.0)));
        assert!(!point_in_rings(&square(), (5.0, f64::INFINITY)));
    }

    #[test]
    fn rings_bbox_covers_every_ring() {
        assert_eq!(rings_bbox(&square()), Some((0.0, 0.0, 10.0, 10.0)));
        assert_eq!(rings_bbox(&two_islands()), Some((0.0, 0.0, 52.0, 52.0)), "離島まで含む");
        assert_eq!(rings_bbox(&with_hole()), Some((0.0, 0.0, 10.0, 10.0)), "穴は外接矩形を変えない");
    }

    #[test]
    fn rings_bbox_is_none_when_there_are_no_vertices() {
        assert_eq!(rings_bbox(&[]), None);
        assert_eq!(rings_bbox(&[Vec::new(), Vec::new()]), None);
    }

    #[test]
    fn rings_bbox_skips_non_finite_vertices() {
        let r = vec![vec![(0.0, 0.0), (f64::NAN, 5.0), (10.0, 10.0), (5.0, f64::INFINITY)]];
        assert_eq!(rings_bbox(&r), Some((0.0, 0.0, 10.0, 10.0)));
        // 全部が非有限なら「頂点が無い」と同じ扱い。
        assert_eq!(rings_bbox(&[vec![(f64::NAN, f64::NAN)]]), None);
    }
}
