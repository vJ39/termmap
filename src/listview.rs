// 左袖リストのスクロール追従ロジック。ui.rs から機械的に切り出したもの(挙動は不変)。
// POI一覧/スポット一覧/道路一覧/設定一覧/メニュー等、あらゆる左袖リストから共通利用される。

// 左袖リストの表示開始位置(offset)を、選択(sel)が viewport 内に入るよう最小移動で更新する。
// 項目数(count)が viewport を超えたときにスクロール追従させ、選択が画面外に消えないようにする。
pub(crate) fn ensure_visible(offset: &mut usize, sel: usize, count: usize, viewport: usize) {
    if viewport == 0 {
        *offset = 0;
        return;
    }
    if sel < *offset {
        *offset = sel; // 上へはみ出た → 選択を先頭に
    } else if sel >= *offset + viewport {
        *offset = sel + 1 - viewport; // 下へはみ出た → 選択を末尾に
    }
    *offset = (*offset).min(count.saturating_sub(viewport)); // 末尾側の空きを詰める
}

#[cfg(test)]
mod tests {
    use super::*;

    // 左袖リストのスクロール追従
    #[test]
    fn ensure_visible_follows_selection() {
        let vh = 5; // 表示5行
        // 収まる場合は offset=0 のまま
        let mut o = 0;
        ensure_visible(&mut o, 3, 4, vh);
        assert_eq!(o, 0);
        // 下へはみ出す: 20件・選択10 → 選択が末尾に来る位置(10+1-5=6)
        let mut o = 0;
        ensure_visible(&mut o, 10, 20, vh);
        assert_eq!(o, 6);
        assert!(10 >= o && 10 < o + vh, "選択が窓内");
        // そこから上へ戻る: 選択2 → 先頭に
        ensure_visible(&mut o, 2, 20, vh);
        assert_eq!(o, 2);
        // 末尾選択は末尾側の空きが詰まる(offset=count-vh)
        let mut o = 0;
        ensure_visible(&mut o, 19, 20, vh);
        assert_eq!(o, 15);
        // viewport=0 は安全に0
        let mut o = 7;
        ensure_visible(&mut o, 3, 20, 0);
        assert_eq!(o, 0);
    }
}
