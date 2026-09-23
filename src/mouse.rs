//! ネイティブ端末のマウス操作(左ドラッグでパン・左クリックで中心移動・ホイールでズーム)。
//! 設計 docs/mouse-click-drag-design.md。端末や UiState に依存しない純関数にしてある。

use crate::dragmode::Layout;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

/// ズームの範囲(キー操作の +/- と揃える)。
const MIN_Z: u32 = 2;
const MAX_Z: u32 = 19;

/// マウス報告を有効にするか。設定で OFF、または web 版(serve-web.sh が TERMMAP_NO_MOUSE を渡す。
/// ブラウザ側が自前でパンを送るので二重になる)なら有効にしない。
pub(crate) fn capture_wanted(cfg_mouse: bool, no_mouse_env: bool) -> bool {
    cfg_mouse && !no_mouse_env
}

/// マウス1イベントを解釈した結果。どれを実行するかは Focus を見て呼び出し側が決める。
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum MouseAction {
    None,
    /// 前回位置からの移動量(端末の桁数/行数に対する比。dragmode::apply_pan にそのまま渡せる)。
    Pan { fx: f64, fy: f64 },
    /// 地図領域内で押して動かさずに離した。座標は押した位置。
    Click { col: u16, row: u16 },
    /// 地図領域内でのホイール。true=ズームイン。
    Zoom { zoom_in: bool, col: u16, row: u16 },
}

/// 左ボタンの押下から離すまでの状態。
#[derive(Default, Debug)]
pub(crate) struct MouseTracker {
    last: Option<(u16, u16)>,
    press: Option<(u16, u16)>,
    dragged: bool,
}

/// (col,row) が地図領域(列 gut..gut+map_cols・行 0..map_rows)に入っているか。
pub(crate) fn in_map(col: u16, row: u16, gut: u32, lay: &Layout) -> bool {
    let (c, r) = (col as u32, row as u32);
    c >= gut && c < gut + lay.map_cols && r < lay.map_rows
}

impl MouseTracker {
    pub(crate) fn feed(&mut self, ev: &MouseEvent, gut: u32, lay: &Layout) -> MouseAction {
        let (col, row) = (ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // 地図の外で押したドラッグは追わない(左袖などを掴んで地図が動かないように)。
                if in_map(col, row, gut, lay) {
                    self.press = Some((col, row));
                    self.last = Some((col, row));
                } else {
                    self.press = None;
                    self.last = None;
                }
                self.dragged = false;
                MouseAction::None
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some((lc, lr)) = self.last else { return MouseAction::None };
                if (lc, lr) == (col, row) {
                    return MouseAction::None;
                }
                self.last = Some((col, row));
                self.dragged = true;
                if lay.cols == 0 || lay.rows == 0 {
                    return MouseAction::None;
                }
                let fx = (col as f64 - lc as f64) / lay.cols as f64;
                let fy = (row as f64 - lr as f64) / lay.rows as f64;
                MouseAction::Pan { fx, fy }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let press = self.press.take();
                let dragged = std::mem::take(&mut self.dragged);
                self.last = None;
                match press {
                    Some((pc, pr)) if !dragged => MouseAction::Click { col: pc, row: pr },
                    _ => MouseAction::None,
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if in_map(col, row, gut, lay) => {
                MouseAction::Zoom { zoom_in: ev.kind == MouseEventKind::ScrollUp, col, row }
            }
            _ => MouseAction::None,
        }
    }
}

/// セル (col,row) の中心が、地図中心から何ピクセル(zoom z の世界ピクセル)離れているか。
pub(crate) fn cell_offset_px(col: u16, row: u16, gut: u32, lay: &Layout) -> (f64, f64) {
    if lay.map_cols == 0 || lay.map_rows == 0 {
        return (0.0, 0.0);
    }
    let dc = col as f64 + 0.5 - gut as f64 - lay.map_cols as f64 / 2.0;
    let dr = row as f64 + 0.5 - lay.map_rows as f64 / 2.0;
    (dc * lay.ow as f64 / lay.map_cols as f64, dr * lay.oh as f64 / lay.map_rows as f64)
}

/// 世界ピクセル座標へ正規化する(X は巻く・Y は端でクランプ。dragmode::apply_pan と同じ)。
fn normalize(cx: f64, cy: f64, z: u32) -> (f64, f64) {
    let n = crate::geo::TILE as f64 * 2f64.powi(z as i32);
    (cx.rem_euclid(n), cy.clamp(0.0, n - 1.0))
}

/// クリックしたセルを地図の中心にした新しい中心。
pub(crate) fn recenter(cx: f64, cy: f64, z: u32, col: u16, row: u16, gut: u32, lay: &Layout) -> (f64, f64) {
    let (dx, dy) = cell_offset_px(col, row, gut, lay);
    normalize(cx + dx, cy + dy, z)
}

/// カーソル下の地点を画面上の同じ位置に保ったままズームする。範囲外なら None。
pub(crate) fn zoom_at(
    cx: f64,
    cy: f64,
    z: u32,
    zoom_in: bool,
    col: u16,
    row: u16,
    gut: u32,
    lay: &Layout,
) -> Option<(f64, f64, u32)> {
    let (dx, dy) = cell_offset_px(col, row, gut, lay);
    if zoom_in {
        if z >= MAX_Z {
            return None;
        }
        let (nx, ny) = normalize(2.0 * cx + dx, 2.0 * cy + dy, z + 1);
        Some((nx, ny, z + 1))
    } else {
        if z <= MIN_Z {
            return None;
        }
        let (nx, ny) = normalize((cx + dx) / 2.0 - dx, (cy + dy) / 2.0 - dy, z - 1);
        Some((nx, ny, z - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{deg_to_pixel, pixel_to_deg};
    use crossterm::event::KeyModifiers;

    // 端末100x41・左袖28・地図72x40・AA(1セル=横1px/縦2px)。
    fn lay() -> Layout {
        Layout { cols: 100, rows: 41, map_cols: 72, map_rows: 40, ow: 72, oh: 80 }
    }
    const GUT: u32 = 28;

    fn ev(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE }
    }
    fn down(c: u16, r: u16) -> MouseEvent { ev(MouseEventKind::Down(MouseButton::Left), c, r) }
    fn drag(c: u16, r: u16) -> MouseEvent { ev(MouseEventKind::Drag(MouseButton::Left), c, r) }
    fn up(c: u16, r: u16) -> MouseEvent { ev(MouseEventKind::Up(MouseButton::Left), c, r) }

    #[test]
    fn capture_is_off_by_config_or_under_the_web_launcher() {
        assert!(capture_wanted(true, false));
        assert!(!capture_wanted(false, false), "設定でOFF");
        assert!(!capture_wanted(true, true), "web版");
    }

    #[test]
    fn in_map_respects_gutter_and_bottom_rows() {
        let l = lay();
        assert!(!in_map(27, 5, GUT, &l), "左袖");
        assert!(in_map(28, 0, GUT, &l));
        assert!(in_map(99, 39, GUT, &l));
        assert!(!in_map(100, 5, GUT, &l), "右端の外");
        assert!(!in_map(50, 40, GUT, &l), "ステータス行");
        assert!(in_map(0, 0, 0, &l), "左袖なし");
    }

    #[test]
    fn press_and_release_without_moving_is_a_click_at_the_press_position() {
        let mut t = MouseTracker::default();
        assert_eq!(t.feed(&down(40, 10), GUT, &lay()), MouseAction::None);
        assert_eq!(t.feed(&up(40, 10), GUT, &lay()), MouseAction::Click { col: 40, row: 10 });
    }

    #[test]
    fn drag_to_the_same_cell_is_still_a_click() {
        let mut t = MouseTracker::default();
        t.feed(&down(40, 10), GUT, &lay());
        assert_eq!(t.feed(&drag(40, 10), GUT, &lay()), MouseAction::None);
        assert_eq!(t.feed(&up(40, 10), GUT, &lay()), MouseAction::Click { col: 40, row: 10 });
    }

    #[test]
    fn drag_reports_the_delta_since_the_previous_event_as_a_ratio() {
        let l = lay();
        let mut t = MouseTracker::default();
        t.feed(&down(40, 10), GUT, &l);
        assert_eq!(t.feed(&drag(45, 12), GUT, &l), MouseAction::Pan { fx: 5.0 / 100.0, fy: 2.0 / 41.0 });
        // 次は前回位置(45,12)からの差分。
        assert_eq!(t.feed(&drag(43, 12), GUT, &l), MouseAction::Pan { fx: -2.0 / 100.0, fy: 0.0 });
        // 動いた後に離してもクリックにならない。
        assert_eq!(t.feed(&up(43, 12), GUT, &l), MouseAction::None);
    }

    #[test]
    fn drag_keeps_following_outside_the_map_once_started_inside() {
        let mut t = MouseTracker::default();
        t.feed(&down(30, 10), GUT, &lay());
        assert!(matches!(t.feed(&drag(10, 10), GUT, &lay()), MouseAction::Pan { .. }));
    }

    #[test]
    fn drag_started_outside_the_map_is_ignored() {
        let mut t = MouseTracker::default();
        t.feed(&down(5, 10), GUT, &lay());
        assert_eq!(t.feed(&drag(40, 10), GUT, &lay()), MouseAction::None);
        assert_eq!(t.feed(&up(40, 10), GUT, &lay()), MouseAction::None, "左袖で押して離してもクリックにしない");
    }

    #[test]
    fn drag_or_release_without_a_press_is_ignored() {
        let mut t = MouseTracker::default();
        assert_eq!(t.feed(&drag(40, 10), GUT, &lay()), MouseAction::None);
        assert_eq!(t.feed(&up(40, 10), GUT, &lay()), MouseAction::None);
    }

    #[test]
    fn a_new_press_resets_the_dragged_state() {
        let mut t = MouseTracker::default();
        t.feed(&down(40, 10), GUT, &lay());
        t.feed(&drag(50, 10), GUT, &lay());
        t.feed(&down(60, 20), GUT, &lay()); // Up を取りこぼした後の押下
        assert_eq!(t.feed(&up(60, 20), GUT, &lay()), MouseAction::Click { col: 60, row: 20 });
    }

    #[test]
    fn right_button_and_plain_moves_are_ignored() {
        let mut t = MouseTracker::default();
        assert_eq!(t.feed(&ev(MouseEventKind::Down(MouseButton::Right), 40, 10), GUT, &lay()), MouseAction::None);
        assert_eq!(t.feed(&ev(MouseEventKind::Moved, 41, 10), GUT, &lay()), MouseAction::None);
        assert_eq!(t.feed(&ev(MouseEventKind::Up(MouseButton::Right), 40, 10), GUT, &lay()), MouseAction::None);
    }

    #[test]
    fn wheel_zooms_only_inside_the_map() {
        let mut t = MouseTracker::default();
        assert_eq!(t.feed(&ev(MouseEventKind::ScrollUp, 40, 10), GUT, &lay()), MouseAction::Zoom { zoom_in: true, col: 40, row: 10 });
        assert_eq!(t.feed(&ev(MouseEventKind::ScrollDown, 40, 10), GUT, &lay()), MouseAction::Zoom { zoom_in: false, col: 40, row: 10 });
        assert_eq!(t.feed(&ev(MouseEventKind::ScrollUp, 5, 10), GUT, &lay()), MouseAction::None, "左袖");
    }

    // ドラッグの比を apply_pan に渡すと、カーソルの移動セル数×1セルのピクセル数だけ地図が動く(1:1)。
    #[test]
    fn drag_ratio_through_apply_pan_moves_the_map_one_to_one() {
        use crate::dragmode::{apply_pan, Axis};
        let l = Layout { cols: 100, rows: 41, map_cols: 72, map_rows: 40, ow: 144, oh: 160 }; // braille
        let (cx, cy) = deg_to_pixel(35.68, 139.77, 12);
        let (nx, ny, moved) = apply_pan(cx, cy, 12, (Axis::Pan, Axis::Pan), 5.0 / 100.0, 2.0 / 41.0, &l);
        assert!(moved);
        assert!((cx - nx - 5.0 * 2.0).abs() < 1e-9, "横5セル=10px 逆向き");
        assert!((cy - ny - 2.0 * 4.0).abs() < 1e-9, "縦2セル=8px 逆向き");
    }

    #[test]
    fn cell_offset_of_the_center_cell_is_about_zero() {
        let l = lay();
        // 地図は列28..100(中央64)・行0..40(中央20)。セル(64,20)の中心は(64.5,20.5)。
        let (dx, dy) = cell_offset_px(64, 20, GUT, &l);
        assert_eq!((dx, dy), (0.5, 1.0));
        let (dx, dy) = cell_offset_px(28, 0, GUT, &l);
        assert_eq!((dx, dy), (-35.5, -39.0));
    }

    #[test]
    fn cell_offset_is_zero_for_an_empty_map() {
        let l = Layout { cols: 0, rows: 0, map_cols: 0, map_rows: 0, ow: 0, oh: 0 };
        assert_eq!(cell_offset_px(3, 3, 0, &l), (0.0, 0.0));
    }

    #[test]
    fn recenter_moves_the_center_by_the_cell_offset() {
        let l = lay();
        let (cx, cy) = deg_to_pixel(35.68, 139.77, 12);
        let (nx, ny) = recenter(cx, cy, 12, 74, 5, GUT, &l);
        let (dx, dy) = cell_offset_px(74, 5, GUT, &l);
        assert!((nx - (cx + dx)).abs() < 1e-9 && (ny - (cy + dy)).abs() < 1e-9);
        assert!(nx > cx && ny < cy, "右上をクリックしたら中心は右上へ");
    }

    #[test]
    fn recenter_wraps_x_and_clamps_y() {
        let l = lay();
        let n = 256.0 * 4.0; // z2
        let (nx, ny) = recenter(0.0, 0.0, 2, 28, 0, GUT, &l);
        assert!((0.0..n).contains(&nx), "Xは巻く: {nx}");
        assert_eq!(ny, 0.0, "Yは上端でクランプ");
    }

    // ホイールのズームでカーソル下の地点(緯度経度)が変わらないこと。
    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let l = lay();
        let (cx, cy) = deg_to_pixel(35.68, 139.77, 12);
        let (col, row) = (80u16, 7u16);
        let under = |cx: f64, cy: f64, z: u32| {
            let (dx, dy) = cell_offset_px(col, row, GUT, &l);
            pixel_to_deg(cx + dx, cy + dy, z)
        };
        let before = under(cx, cy, 12);
        for zoom_in in [true, false] {
            let (nx, ny, nz) = zoom_at(cx, cy, 12, zoom_in, col, row, GUT, &l).unwrap();
            assert_eq!(nz, if zoom_in { 13 } else { 11 });
            let after = under(nx, ny, nz);
            assert!((before.0 - after.0).abs() < 1e-9 && (before.1 - after.1).abs() < 1e-9,
                "zoom_in={zoom_in}: {before:?} → {after:?}");
        }
    }

    #[test]
    fn zoom_at_the_center_cell_matches_the_key_zoom_closely() {
        // 中心セルでのズームはキーの +/-(cx*2, cx/2)とほぼ同じ(セル中心の0.5セル分だけずれる)。
        let l = lay();
        let (cx, cy) = deg_to_pixel(35.68, 139.77, 12);
        let (nx, _, _) = zoom_at(cx, cy, 12, true, 64, 20, GUT, &l).unwrap();
        assert!((nx - cx * 2.0).abs() <= 1.0);
    }

    #[test]
    fn zoom_stops_at_the_limits() {
        let l = lay();
        assert!(zoom_at(100.0, 100.0, MAX_Z, true, 40, 10, GUT, &l).is_none());
        assert!(zoom_at(100.0, 100.0, MIN_Z, false, 40, 10, GUT, &l).is_none());
        assert!(zoom_at(100.0, 100.0, MAX_Z, false, 40, 10, GUT, &l).is_some());
        assert!(zoom_at(100.0, 100.0, MIN_Z, true, 40, 10, GUT, &l).is_some());
    }
}
