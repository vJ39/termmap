// 道路名検索(r)で追加した道路の塊(RoadSeg)まわり。ui.rs から機械的に切り出したもの(挙動は不変)。
// RoadSeg 自体は name/color/pts だけを持つ純粋なデータで、対話ループのローカル状態には依存しない。

// 道路の塊(RoadSeg)ごとの表示色。BRouterルートの cyan [0,220,255] と被らない色を len で循環。
pub(crate) const ROAD_PALETTE: &[[u8; 3]] = &[
    [180, 80, 255],  // 紫
    [255, 140, 0],   // 橙
    [0, 200, 120],   // 緑
    [255, 80, 180],  // 桃
    [230, 200, 0],   // 黄
];

// 道路名検索(r)で追加した道路1本ぶんの塊。個別に色を持ち、一覧から個別削除できる。
pub(crate) struct RoadSeg { pub(crate) name: String, pub(crate) color: [u8; 3], pub(crate) pts: Vec<(f64, f64)> }

// 新しい RoadSeg に割り当てる色を、既存本数(road_segs.len())から ROAD_PALETTE を循環して決める。
pub(crate) fn road_color_for(existing_count: usize) -> [u8; 3] {
    ROAD_PALETTE[existing_count % ROAD_PALETTE.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_through_palette() {
        for i in 0..ROAD_PALETTE.len() {
            assert_eq!(road_color_for(i), ROAD_PALETTE[i]);
        }
        // 本数がパレット数を超えたら先頭へ循環する
        assert_eq!(road_color_for(ROAD_PALETTE.len()), ROAD_PALETTE[0]);
        assert_eq!(road_color_for(ROAD_PALETTE.len() + 2), ROAD_PALETTE[2]);
    }
}
