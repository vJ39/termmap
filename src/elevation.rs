// ルート標高プロファイルの端末描画。std のみ・単体コンパイル可能(crate:: 参照なし)。
//
// elevation_chart: 標高列を width 列にビン化し、height 行のブロック文字グラフに変換する。
// elevation_stats: 標高列から (min, max, total_ascent) を計算する。

/// 1/8刻みのブロック文字(下から積む用)。index 1..=7 が ▁..▇、8 は呼び出し側で █ を使う。
const EIGHTHS: [char; 7] = ['\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}'];
const FULL_BLOCK: char = '\u{2588}';

/// ele を width 列へビン化する(各列は対応区間の平均値。区間が空なら最近傍1点)。
fn bin_values(ele: &[f64], width: usize) -> Vec<f64> {
    let n = ele.len();
    let mut out = Vec::with_capacity(width);
    for col in 0..width {
        let start = col * n / width;
        let mut end = (col + 1) * n / width;
        if end <= start {
            end = start + 1;
        }
        let end = end.min(n);
        if start >= end {
            // n=0 はこの関数の呼び出し元で既に排除済みだが、念のため最近傍(末尾)にフォールバック。
            let idx = n.saturating_sub(1);
            out.push(*ele.get(idx).unwrap_or(&0.0));
        } else {
            let slice = &ele[start..end];
            let avg = slice.iter().sum::<f64>() / slice.len() as f64;
            out.push(avg);
        }
    }
    out
}

/// ルート標高プロファイルを height 行 × width 幅(chars換算)のブロック文字グラフにする。
/// ele が空、または width/height が 0 の場合は空の Vec を返す(安全側)。
pub fn elevation_chart(ele: &[f64], width: usize, height: usize) -> Vec<String> {
    if ele.is_empty() || width == 0 || height == 0 {
        return Vec::new();
    }

    let cols = bin_values(ele, width);

    let min = cols.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = cols.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;

    // 各列の合計レベル(0..=height*8): 高さ方向の 1/8 刻み精度。
    let levels: Vec<i64> = cols
        .iter()
        .map(|&v| {
            let h = if range > 0.0 { (v - min) / range } else { 0.5 };
            let max_level = (height as i64) * 8;
            let lvl = (h * max_level as f64).round() as i64;
            lvl.clamp(0, max_level)
        })
        .collect();

    let mut rows: Vec<String> = Vec::with_capacity(height);
    for row_idx in 0..height {
        // row_idx=0 が最上段。row_from_bottom は下から数えた段位置(0-indexed)。
        let row_from_bottom = (height - 1 - row_idx) as i64;
        let mut line = String::with_capacity(width);
        for &lvl in &levels {
            let row_level = (lvl - row_from_bottom * 8).clamp(0, 8);
            let ch = match row_level {
                0 => ' ',
                8 => FULL_BLOCK,
                n => EIGHTHS[(n - 1) as usize],
            };
            line.push(ch);
        }
        rows.push(line);
    }
    rows
}

/// (min, max, total_ascent) を返す。total_ascent は連続する正の差分の合計。
/// ele が空なら (0.0, 0.0, 0.0)。
pub fn elevation_stats(ele: &[f64]) -> (f64, f64, f64) {
    if ele.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let min = ele.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = ele.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mut ascent = 0.0;
    for i in 1..ele.len() {
        let d = ele[i] - ele[i - 1];
        if d > 0.0 {
            ascent += d;
        }
    }
    (min, max, ascent)
}

/// n_points 個の経路点のうち index 番目が、幅 width のプロファイル上で占める列(0..width-1)。
/// 標高帯に「現在地カーソル」を出すための位置変換(純粋・テスト対象)。
pub fn profile_col(n_points: usize, index: usize, width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    if n_points <= 1 {
        return 0;
    }
    (index * (width - 1) / (n_points - 1)).min(width - 1)
}

/// 標高プロファイル左端の高さ目盛りラベル。row(0-indexed、0が最上段)がheight行中の
/// 最上段なら最高値、最下段なら最低値、中間段(heightが4以上のときのみ、height/2行目)
/// なら中間値を返す。それ以外はNone(その行はラベル無し)。
pub fn axis_label(row: u32, height: u32, min: f64, max: f64) -> Option<String> {
    if height == 0 || row >= height {
        return None;
    }
    if row == 0 {
        Some(format!("{max:>5.0}m"))
    } else if row == height - 1 {
        Some(format!("{min:>5.0}m"))
    } else if height >= 4 && row == height / 2 {
        Some(format!("{:>5.0}m", min + (max - min) / 2.0))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_known_ascending_series() {
        // 100 -> 150(+50) -> 120(-30) -> 180(+60) -> 180(+0) -> 90(-90)
        let ele = [100.0, 150.0, 120.0, 180.0, 180.0, 90.0];
        let (min, max, ascent) = elevation_stats(&ele);
        assert!((min - 90.0).abs() < 1e-9, "min={min}");
        assert!((max - 180.0).abs() < 1e-9, "max={max}");
        assert!((ascent - 110.0).abs() < 1e-9, "ascent={ascent}");
    }

    #[test]
    fn stats_flat_series_has_zero_ascent() {
        let ele = [50.0, 50.0, 50.0];
        let (min, max, ascent) = elevation_stats(&ele);
        assert!((min - 50.0).abs() < 1e-9);
        assert!((max - 50.0).abs() < 1e-9);
        assert!((ascent - 0.0).abs() < 1e-9);
    }

    #[test]
    fn stats_empty_is_safe() {
        let ele: [f64; 0] = [];
        assert_eq!(elevation_stats(&ele), (0.0, 0.0, 0.0));
    }

    #[test]
    fn chart_has_height_rows_and_width_chars() {
        // 単調増加の標高列(20点)を width=10, height=5 のグラフへ。
        let ele: Vec<f64> = (0..20).map(|i| i as f64 * 10.0).collect();
        let width = 10;
        let height = 5;
        let rows = elevation_chart(&ele, width, height);
        assert_eq!(rows.len(), height, "行数が height と一致しない");
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.chars().count(), width, "row {i} の幅が width と一致しない: {row:?}");
        }
    }

    #[test]
    fn chart_bottom_row_denser_than_top_for_ascending_profile() {
        // 単調増加なら、右に行くほど高くなる(最上段に埋まる文字が増える=空白でない列が増える)。
        let ele: Vec<f64> = (0..30).map(|i| i as f64).collect();
        let rows = elevation_chart(&ele, 15, 6);
        let top = &rows[0];
        let non_space_top = top.chars().filter(|&c| c != ' ').count();
        // 単調増加の左端(標高最小)は最上段まで届かないはずなので、全列が埋まることはない。
        assert!(non_space_top < 15, "top row unexpectedly full: {top:?}");
        // 最終列(最大標高)は最上段まで達しているはず。
        assert_ne!(top.chars().last().unwrap(), ' ');
    }

    #[test]
    fn chart_multibyte_safe_width_by_chars_count() {
        // ブロック文字(▁..█)は3バイトUTF-8だが、幅判定は必ず chars().count() で行うこと。
        // byte長とchar数が食い違うケース(=マルチバイト文字を含む行)がテストに含まれることを確認する。
        let ele = vec![1.0, 5.0, 3.0, 8.0, 2.0, 9.0, 4.0];
        let rows = elevation_chart(&ele, 7, 3);
        let mut saw_multibyte_row = false;
        for row in &rows {
            assert_eq!(row.chars().count(), 7, "row={row:?}");
            if row.len() != row.chars().count() {
                saw_multibyte_row = true;
            }
        }
        assert!(saw_multibyte_row, "no row contained multibyte block chars: {rows:?}");
    }

    #[test]
    fn chart_empty_input_is_safe() {
        let ele: [f64; 0] = [];
        assert!(elevation_chart(&ele, 10, 5).is_empty());
    }

    #[test]
    fn chart_zero_width_or_height_is_safe() {
        let ele = [1.0, 2.0, 3.0];
        assert!(elevation_chart(&ele, 0, 5).is_empty());
        assert!(elevation_chart(&ele, 5, 0).is_empty());
    }

    #[test]
    fn chart_width_larger_than_samples_is_safe() {
        // width > ele.len() でもビン化(最近傍フォールバック)で落ちない。
        let ele = [10.0, 20.0];
        let rows = elevation_chart(&ele, 8, 4);
        assert_eq!(rows.len(), 4);
        for row in &rows {
            assert_eq!(row.chars().count(), 8);
        }
    }

    #[test]
    fn profile_col_maps_endpoints_and_clamps() {
        assert_eq!(profile_col(2, 0, 10), 0); // 始点=左端
        assert_eq!(profile_col(2, 1, 10), 9); // 終点=右端
        assert_eq!(profile_col(5, 2, 100), 49); // 中間(2/4*99=49.5→49、整数除算)
        assert_eq!(profile_col(1, 0, 10), 0); // 点1つ
        assert_eq!(profile_col(10, 5, 1), 0); // 幅1
        assert_eq!(profile_col(10, 0, 0), 0); // 幅0安全
    }

    #[test]
    fn axis_label_top_and_bottom_rows_show_max_and_min() {
        assert_eq!(axis_label(0, 8, -66.0, 527.0).as_deref(), Some("  527m"));
        assert_eq!(axis_label(7, 8, -66.0, 527.0).as_deref(), Some("  -66m"));
    }

    #[test]
    fn axis_label_mid_row_only_when_height_at_least_4() {
        // height=8: 中間行(4行目)に (min+max)/2 = 230.5 → 230m(Rustの{:.0}は偶数丸め)
        assert_eq!(axis_label(4, 8, -66.0, 527.0).as_deref(), Some("  230m"));
        // height=3: 4未満なので中間ラベルは出さない(row=1が中間相当だがNone)
        assert_eq!(axis_label(1, 3, 0.0, 100.0), None);
    }

    #[test]
    fn axis_label_other_rows_are_none() {
        assert_eq!(axis_label(1, 8, -66.0, 527.0), None);
        assert_eq!(axis_label(2, 8, -66.0, 527.0), None);
        assert_eq!(axis_label(6, 8, -66.0, 527.0), None);
    }

    #[test]
    fn axis_label_height_one_shows_only_top_which_is_also_bottom() {
        // height=1: row=0はtop条件に先にマッチしmaxを返す(bottom分岐には届かない)
        assert_eq!(axis_label(0, 1, 10.0, 20.0).as_deref(), Some("   20m"));
    }

    #[test]
    fn axis_label_zero_height_or_out_of_range_row_is_none() {
        assert_eq!(axis_label(0, 0, 0.0, 100.0), None);
        assert_eq!(axis_label(5, 5, 0.0, 100.0), None); // row == height(範囲外)
    }

    #[test]
    fn axis_label_flat_elevation_min_equals_max() {
        assert_eq!(axis_label(0, 4, 50.0, 50.0).as_deref(), Some("   50m"));
        assert_eq!(axis_label(2, 4, 50.0, 50.0).as_deref(), Some("   50m")); // 中間値も同じ
    }
}
