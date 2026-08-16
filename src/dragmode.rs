//! web版(ブラウザ)のドラッグ操作で、X軸/Y軸それぞれが「地図パン」「カーソル移動」「無効」の
//! どれなのかを Focus から決める。docs/web-touch-drag-design.md §3.2 の表がそのまま axes()。
//!
//! ブラウザ側(web/touch-overlay.js)に Focus の対応表を二重に持たせないため、判定は Rust 側へ
//! 集約し、結果だけを OSC 9997 で通知する(sound.rs の 9999=効果音・voice.rs の 9998=音声案内と
//! 同じ仕組み。認識しない端末では無害に無視される)。

use crate::focus::Focus;

/// OSC 9997 のデータ部先頭に置くフォーマット版数。将来フィールドを足すときに上げる。
const OSC_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Axis {
    /// ビューポート(地図)が動く軸。ブラウザ側は極性を反転して送る(指と同じ向きに地図が流れる)。
    Pan,
    /// カーソル/値が動く軸。ブラウザ側は極性そのままで矢印キーを送る。
    Cursor,
    /// その軸の操作は無効。ブラウザ側は何も送らない。
    None,
}

impl Axis {
    /// OSC 9997 のデータ部に載せる1文字表現。
    fn code(self) -> char {
        match self {
            Axis::Pan => 'p',
            Axis::Cursor => 'c',
            Axis::None => 'n',
        }
    }
}

/// Focus から (X軸, Y軸) の意味を決める。
///
/// ワイルドカード(`_`)を使わない網羅マッチにしてある。Focus に variant が増えると、ここが
/// コンパイルエラーになって設計書 §3.2 の表への追記漏れに気づける(テストの網羅性チェックも同様)。
pub(crate) fn axes(focus: &Focus) -> (Axis, Axis) {
    use Axis::{Cursor, None as Nothing, Pan};
    match focus {
        // 地図: 両軸ともパン
        Focus::Map => (Pan, Pan),
        // 周辺検索の一覧: 横は地図の微パン・縦は一覧カーソル(選択地点へ地図が追従)。
        // X軸とY軸で意味が違う代表例なので、Focus単位でなく軸単位で判定している。
        Focus::PoiList => (Pan, Cursor),
        // 一覧/メニュー系: 横は無効・縦は一覧カーソル
        Focus::RoutePanel
        | Focus::WaypointList
        | Focus::RoadList
        | Focus::RouteList
        | Focus::SpotList
        | Focus::SpotCatList
        | Focus::PoiMenu
        | Focus::Menu(_)
        | Focus::RouteFavMenu { .. }
        | Focus::SettingsPick(_) => (Nothing, Cursor),
        // 設定一覧: 横は値の増減・縦は行カーソル
        Focus::Settings => (Cursor, Cursor),
        // 候補ピッカーと距離ゲージ: 横のみ有効
        Focus::ColorPick { .. } | Focus::ShapePick { .. } | Focus::WanderForm { .. } => {
            (Cursor, Nothing)
        }
        // 1行テキスト入力: 横は文字カーソル・縦は無効
        Focus::Search(_)
        | Focus::SaveName(_)
        | Focus::NearSearch(_)
        | Focus::NewCat(_)
        | Focus::RoadSearch(_)
        | Focus::Recommend(_)
        | Focus::SpotRename(..)
        | Focus::SpotEditName(..)
        | Focus::SettingsEdit(..) => (Cursor, Nothing),
        // 複数項目フォーム: 横は文字カーソル・縦は項目移動
        Focus::SpotForm { .. } | Focus::PoiKindForm { .. } => (Cursor, Cursor),
    }
}

/// OSC 9997 の通知文字列を組み立てる。ESC ] 9997 ; <version> ; <xy> BEL の形。
/// <xy> はX軸・Y軸の順に 'p'(Pan)/'c'(Cursor)/'n'(None) を1文字ずつ並べたもの。
fn drag_mode_seq(axes: (Axis, Axis)) -> String {
    format!("\x1b]9997;{OSC_VERSION};{}{}\x07", axes.0.code(), axes.1.code())
}

/// ブラウザ(web/touch-overlay.js)へ現在の軸モードを通知する。
/// sound.rs::emit_web_sound_signal と同じ書き方(print! + flush)。
pub(crate) fn emit_web_drag_mode(axes: (Axis, Axis)) {
    use std::io::Write;
    print!("{}", drag_mode_seq(axes));
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::Axis::{Cursor, None as Nothing, Pan};
    use super::*;
    use crate::menu::MenuLevel;
    use std::collections::BTreeSet;

    /// Focus の variant 総数(src/focus.rs の宣言数)。variant を足したときは
    /// variant_id() にアームを足す(足さないとコンパイルエラー)→ この定数を +1 →
    /// design_table() にその variant の期待値を足す、の順で直す。
    const FOCUS_VARIANT_COUNT: usize = 27;

    /// variant ごとの通し番号と名前。網羅マッチ(`_` なし)なので Focus が増えると
    /// ここがコンパイルエラーになり、テストの網羅性が崩れたことに気づける。
    fn variant_id(f: &Focus) -> (usize, &'static str) {
        match f {
            Focus::Map => (0, "Map"),
            Focus::RoutePanel => (1, "RoutePanel"),
            Focus::Menu(_) => (2, "Menu"),
            Focus::Search(_) => (3, "Search"),
            Focus::SaveName(_) => (4, "SaveName"),
            Focus::NearSearch(_) => (5, "NearSearch"),
            Focus::PoiMenu => (6, "PoiMenu"),
            Focus::PoiList => (7, "PoiList"),
            Focus::RouteList => (8, "RouteList"),
            Focus::WaypointList => (9, "WaypointList"),
            Focus::RoadList => (10, "RoadList"),
            Focus::NewCat(_) => (11, "NewCat"),
            Focus::SpotForm { .. } => (12, "SpotForm"),
            Focus::SpotList => (13, "SpotList"),
            Focus::SpotCatList => (14, "SpotCatList"),
            Focus::SpotRename(..) => (15, "SpotRename"),
            Focus::Settings => (16, "Settings"),
            Focus::SettingsEdit(..) => (17, "SettingsEdit"),
            Focus::SettingsPick(_) => (18, "SettingsPick"),
            Focus::RoadSearch(_) => (19, "RoadSearch"),
            Focus::SpotEditName(..) => (20, "SpotEditName"),
            Focus::Recommend(_) => (21, "Recommend"),
            Focus::ColorPick { .. } => (22, "ColorPick"),
            Focus::ShapePick { .. } => (23, "ShapePick"),
            Focus::PoiKindForm { .. } => (24, "PoiKindForm"),
            Focus::WanderForm { .. } => (25, "WanderForm"),
            Focus::RouteFavMenu { .. } => (26, "RouteFavMenu"),
        }
    }

    /// 設計書 §3.2 の表そのもの。左が Focus、右が期待する (X軸, Y軸)。
    /// フィールドを持つ variant は代表値を入れる(値によって軸モードは変わらない)。
    fn design_table() -> Vec<(Focus, (Axis, Axis))> {
        vec![
            (Focus::Map, (Pan, Pan)),
            (Focus::PoiList, (Pan, Cursor)),
            (Focus::RoutePanel, (Nothing, Cursor)),
            (Focus::WaypointList, (Nothing, Cursor)),
            (Focus::RoadList, (Nothing, Cursor)),
            (Focus::RouteList, (Nothing, Cursor)),
            (Focus::SpotList, (Nothing, Cursor)),
            (Focus::SpotCatList, (Nothing, Cursor)),
            (Focus::PoiMenu, (Nothing, Cursor)),
            (Focus::Menu(MenuLevel::Categories), (Nothing, Cursor)),
            (Focus::Menu(MenuLevel::Items(2)), (Nothing, Cursor)),
            (Focus::RouteFavMenu { sel: 0 }, (Nothing, Cursor)),
            (Focus::SettingsPick(3), (Nothing, Cursor)),
            (Focus::Settings, (Cursor, Cursor)),
            (Focus::ColorPick { cat: 1 }, (Cursor, Nothing)),
            (Focus::ShapePick { cat: 1 }, (Cursor, Nothing)),
            (Focus::WanderForm { dist_km: 5.0 }, (Cursor, Nothing)),
            (Focus::Search("東京".into()), (Cursor, Nothing)),
            (Focus::SaveName("ルート1".into()), (Cursor, Nothing)),
            (Focus::NearSearch("コンビニ".into()), (Cursor, Nothing)),
            (Focus::NewCat("旅先".into()), (Cursor, Nothing)),
            (Focus::RoadSearch("国道1号".into()), (Cursor, Nothing)),
            (Focus::Recommend("温泉".into()), (Cursor, Nothing)),
            (Focus::SpotRename("名前".into(), 0), (Cursor, Nothing)),
            (Focus::SpotEditName("名前".into(), 0), (Cursor, Nothing)),
            (Focus::SettingsEdit(6, "12".into()), (Cursor, Nothing)),
            (
                Focus::SpotForm { name: "店".into(), url: "https://example.com".into(), field: 0 },
                (Cursor, Cursor),
            ),
            (
                Focus::PoiKindForm { label: "銭湯".into(), tag: "amenity=public_bath".into(), field: 1 },
                (Cursor, Cursor),
            ),
        ]
    }

    #[test]
    fn axes_matches_design_table() {
        for (focus, want) in design_table() {
            let (_, name) = variant_id(&focus);
            assert_eq!(axes(&focus), want, "Focus::{name} の軸モードが設計書 §3.2 の表と違う");
        }
    }

    #[test]
    fn design_table_covers_every_focus_variant() {
        let seen: BTreeSet<usize> = design_table().iter().map(|(f, _)| variant_id(f).0).collect();
        let want: BTreeSet<usize> = (0..FOCUS_VARIANT_COUNT).collect();
        let missing: Vec<usize> = want.difference(&seen).copied().collect();
        assert!(
            missing.is_empty(),
            "未対応のFocusがある: variant_id {missing:?}。設計書 §3.2 の表と design_table() に追記すること"
        );
        assert_eq!(
            seen.len(),
            FOCUS_VARIANT_COUNT,
            "Focus の variant 数が変わっている。FOCUS_VARIANT_COUNT を更新すること"
        );
    }

    #[test]
    fn poi_list_is_pan_on_x_and_cursor_on_y() {
        // 設計書が強調している、X軸とY軸で意味が違うケース。
        assert_eq!(axes(&Focus::PoiList), (Pan, Cursor));
    }

    #[test]
    fn wander_form_is_cursor_on_x_and_none_on_y() {
        // 距離ゲージ。横ドラッグで距離増減・縦は無効。距離の値では変わらない。
        assert_eq!(axes(&Focus::WanderForm { dist_km: 5.0 }), (Cursor, Nothing));
        assert_eq!(axes(&Focus::WanderForm { dist_km: 120.0 }), (Cursor, Nothing));
    }

    #[test]
    fn map_is_pan_on_both_axes() {
        assert_eq!(axes(&Focus::Map), (Pan, Pan));
    }

    #[test]
    fn drag_mode_seq_builds_osc9997() {
        assert_eq!(drag_mode_seq((Pan, Pan)), "\x1b]9997;1;pp\x07");
        assert_eq!(drag_mode_seq((Nothing, Cursor)), "\x1b]9997;1;nc\x07");
        assert_eq!(drag_mode_seq((Cursor, Nothing)), "\x1b]9997;1;cn\x07");
        assert_eq!(drag_mode_seq((Cursor, Cursor)), "\x1b]9997;1;cc\x07");
        assert_eq!(drag_mode_seq((Pan, Cursor)), "\x1b]9997;1;pc\x07");
    }

    #[test]
    fn drag_mode_seq_has_expected_shape_for_all_combinations() {
        let all = [Pan, Cursor, Nothing];
        for x in all {
            for y in all {
                let s = drag_mode_seq((x, y));
                let body = s
                    .strip_prefix("\x1b]9997;")
                    .and_then(|r| r.strip_suffix('\x07'))
                    .expect("ESC ] 9997 ; ... BEL で囲まれていること");
                let (ver, xy) = body.split_once(';').expect("version と xy が ; 区切りであること");
                assert_eq!(ver, "1");
                assert_eq!(xy.chars().count(), 2, "xy は必ず2文字");
                assert!(xy.chars().all(|c| matches!(c, 'p' | 'c' | 'n')));
                // 軸の順序が (X, Y) であること。入れ替わると左右と上下が逆に効く。
                assert_eq!(xy.chars().next().unwrap(), x.code());
                assert_eq!(xy.chars().nth(1).unwrap(), y.code());
            }
        }
    }

    #[test]
    fn emit_does_not_panic() {
        // 標準出力へ書くだけ。sound.rs の play() テストと同じく panic しないことだけ確認する。
        emit_web_drag_mode((Pan, Pan));
        emit_web_drag_mode((Nothing, Nothing));
    }
}
