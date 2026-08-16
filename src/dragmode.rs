//! web版(ブラウザ)のドラッグ操作で、X軸/Y軸それぞれが「地図パン」「カーソル移動」「無効」の
//! どれなのかを Focus から決める。docs/web-touch-drag-design.md §3.2 の表がそのまま axes()。
//!
//! ブラウザ側(web/touch-overlay.js)に Focus の対応表を二重に持たせないため、判定は Rust 側へ
//! 集約し、結果だけを OSC 9997 で通知する(sound.rs の 9999=効果音・voice.rs の 9998=音声案内と
//! 同じ仕組み。認識しない端末では無害に無視される)。

use crate::focus::Focus;

/// OSC 9997 のデータ部先頭に置くフォーマット版数。将来フィールドを足すときに上げる。
const OSC_VERSION: u32 = 1;

/// ブラウザ → termmap のパン量マーカーの先頭(設計書 §6.1)。
/// 形式は `\u{1}PAN\u{1}<fx>\u{1}<fy>\u{1}`。fx/fy は指の移動量を端末ビューポートの
/// 幅/高さで割った比で、符号は指の移動方向そのもの(右/下が正)。ライブ現在地の
/// `\u{1}GPS\u{1}...` と同じ SOH 区切りの専用マーカーにしてあり、通常のペースト
/// (検索欄への貼り付け等)とは衝突しない。
pub(crate) const PAN_MARKER: &str = "\u{1}PAN\u{1}";

/// ブラウザが軸モードの再送を要求するマーカー(設計書 §5.3)。ページを再読み込みすると
/// JS 側の状態は消えるが termmap 側の Focus は変わらないため、OSC 9997 が飛ばない。
/// JS の初期化時と visibilitychange での復帰時にこれを送ってもらい、次フレームで送り直す。
pub(crate) const DRAG_MODE_REQUEST: &str = "\u{1}DRAGMODE?\u{1}";

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

/// パン量マーカー(`\u{1}PAN\u{1}<fx>\u{1}<fy>\u{1}`)を (fx, fy) に解く。
///
/// 壊れた入力は「無視」する(None を返す)方針で、GPS マーカーの受け口
/// (`src/ui.rs` の `\u{1}GPS\u{1}` 分岐)が `is_finite()` と範囲チェックで弾いているのと
/// 同じ扱い。範囲外(|比| > 1)は、指が1回の touchmove で端末1画面分を超えて動いたことに
/// なり実機ではありえないため、クランプして一部を活かすのではなくマーカーごと捨てる
/// (壊れた値を丸めて画面が飛ぶより、その1回分を落とす方が被害が小さい)。
pub(crate) fn parse_pan_marker(s: &str) -> Option<(f64, f64)> {
    let rest = s.strip_prefix(PAN_MARKER)?;
    let mut parts = rest.split('\u{1}');
    let fx: f64 = parts.next()?.trim().parse().ok()?;
    let fy: f64 = parts.next()?.trim().parse().ok()?;
    if !fx.is_finite() || !fy.is_finite() {
        return None;
    }
    if !(-1.0..=1.0).contains(&fx) || !(-1.0..=1.0).contains(&fy) {
        return None;
    }
    Some((fx, fy))
}

/// 指の移動比 → 地図の移動量[出力ピクセル]の換算(設計書 §6.2)。
///
/// 「指が端末の1/4を横切ったら地図も表示範囲の1/4動く」を式で保証する。
/// 端末の桁数/行数(`cols`/`rows`)と地図領域(`map_cols`/`map_rows` と出力ピクセル `ow`/`oh`)の
/// 両方を Rust が持っているので、左袖やステータス行のぶんもここで正確に吸収される
/// (JS 側にゲイン調整の定数を置かなくて済む)。
///
/// 返すのは符号を反転していない生の移動量。指と同じ向きに地図を流すための反転
/// (`cx -= dx`)は呼び出し側で行う。
pub(crate) fn pan_ratio_to_px(fx: f64, fy: f64, lay: &Layout) -> (f64, f64) {
    // 地図領域が0セルなら換算不能。実際の呼び出し元では下限が効いている(map_cols>=10 /
    // map_rows>=3)が、純関数として0除算でNaNを返さないようにしておく。
    if lay.map_cols == 0 || lay.map_rows == 0 {
        return (0.0, 0.0);
    }
    let dx = fx * lay.cols as f64 * (lay.ow as f64 / lay.map_cols as f64);
    let dy = fy * lay.rows as f64 * (lay.oh as f64 / lay.map_rows as f64);
    (dx, dy)
}

/// 合算したパン量(比)を地図中心へ適用する。設計書 §6.2 の「適用条件」をここに閉じてある:
///
/// - 該当軸が `Axis::Pan` のときだけ動かす。X軸とY軸は独立に判定する(`PoiList` は X だけ)。
/// - 指と同じ向きに地図が流れるよう、中心は逆向きに動かす(設計書 §3.1)。
/// - 世界ピクセル座標へ正規化する。X は `rem_euclid` で巻き、Y は端でクランプする。
///   合算値は1マーカーの上限(±1)を超えうるので、キー経路のような1回ぶんの加減算では
///   足りない(何周ぶん動いても確実に範囲へ収まる形にしてある)。
///
/// 戻り値は (新しいcx, 新しいcy, 実際に動かしたか)。動かさなかった場合は入力をそのまま返す。
pub(crate) fn apply_pan(
    cx: f64,
    cy: f64,
    z: u32,
    axes: (Axis, Axis),
    fx: f64,
    fy: f64,
    lay: &Layout,
) -> (f64, f64, bool) {
    let moved_x = axes.0 == Axis::Pan && fx != 0.0;
    let moved_y = axes.1 == Axis::Pan && fy != 0.0;
    if !moved_x && !moved_y {
        return (cx, cy, false);
    }
    let (dx, dy) = pan_ratio_to_px(fx, fy, lay);
    let mut ncx = cx;
    let mut ncy = cy;
    if moved_x {
        ncx -= dx;
    }
    if moved_y {
        ncy -= dy;
    }
    let n = (crate::geo::TILE as f64) * 2f64.powi(z as i32);
    ncx = ncx.rem_euclid(n);
    ncy = ncy.clamp(0.0, n - 1.0);
    (ncx, ncy, true)
}

/// pan_ratio_to_px() に渡す画面レイアウトの寸法。同じ型(u32)の値が6つ並ぶので、
/// 位置引数で渡すと cols と rows、ow と oh の取り違えが静かに通ってしまう。
/// 名前付きで束ねて取り違えを防ぐ。
pub(crate) struct Layout {
    /// 端末の桁数
    pub cols: u32,
    /// 端末の行数(ステータス行を含む)
    pub rows: u32,
    /// 地図領域の桁数(左袖を除いた分)
    pub map_cols: u32,
    /// 地図領域の行数(標高帯・ステータス行を除いた分)
    pub map_rows: u32,
    /// 地図領域の出力ピクセル幅(braille なら map_cols*2)
    pub ow: u32,
    /// 地図領域の出力ピクセル高(braille なら map_rows*4)
    pub oh: u32,
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

    // ── パン量マーカーのパース(設計書 §11) ────────────────────────────

    /// テスト用にマーカー文字列を組み立てる。JS 側(sendPanPaste)が送る形と同じ。
    fn pan_marker(fx: &str, fy: &str) -> String {
        format!("{PAN_MARKER}{fx}\u{1}{fy}\u{1}")
    }

    #[test]
    fn parse_pan_marker_accepts_normal_values() {
        let got = parse_pan_marker(&pan_marker("0.0132", "-0.0041")).expect("正常値はパースできる");
        assert!((got.0 - 0.0132).abs() < 1e-12);
        assert!((got.1 + 0.0041).abs() < 1e-12);
    }

    #[test]
    fn parse_pan_marker_accepts_range_boundaries() {
        // ±1(端末1画面ぶん)はちょうど許容範囲の端なので通す。
        assert_eq!(parse_pan_marker(&pan_marker("1", "-1")), Some((1.0, -1.0)));
        assert_eq!(parse_pan_marker(&pan_marker("0", "0")), Some((0.0, 0.0)));
    }

    #[test]
    fn parse_pan_marker_works_without_trailing_separator() {
        // 末尾の区切りが無い形(GPSマーカーと同じ形)でも読めること。JS 側の実装が変わっても
        // 受け口が壊れないようにしておく。
        assert_eq!(parse_pan_marker("\u{1}PAN\u{1}0.5\u{1}0.25"), Some((0.5, 0.25)));
    }

    #[test]
    fn parse_pan_marker_rejects_out_of_range() {
        // |比| > 1 は1回の touchmove で端末1画面分を超えた計算になる。実機ではありえないので捨てる。
        assert_eq!(parse_pan_marker(&pan_marker("1.5", "0.1")), None);
        assert_eq!(parse_pan_marker(&pan_marker("0.1", "-1.5")), None);
        assert_eq!(parse_pan_marker(&pan_marker("1e9", "0")), None);
    }

    #[test]
    fn parse_pan_marker_rejects_nan_and_infinity() {
        assert_eq!(parse_pan_marker(&pan_marker("NaN", "0.1")), None);
        assert_eq!(parse_pan_marker(&pan_marker("0.1", "NaN")), None);
        assert_eq!(parse_pan_marker(&pan_marker("inf", "0.1")), None);
        assert_eq!(parse_pan_marker(&pan_marker("-inf", "0.1")), None);
    }

    #[test]
    fn parse_pan_marker_rejects_non_numeric() {
        assert_eq!(parse_pan_marker(&pan_marker("abc", "0.1")), None);
        assert_eq!(parse_pan_marker(&pan_marker("0.1", "")), None);
        assert_eq!(parse_pan_marker(&pan_marker("0.1,0.2", "0.3")), None);
    }

    #[test]
    fn parse_pan_marker_rejects_missing_fields() {
        assert_eq!(parse_pan_marker("\u{1}PAN\u{1}0.5"), None); // fy が無い
        assert_eq!(parse_pan_marker("\u{1}PAN\u{1}"), None); // マーカーだけ
        assert_eq!(parse_pan_marker("\u{1}PAN\u{1}\u{1}"), None); // 区切りだけで中身が無い
    }

    #[test]
    fn parse_pan_marker_ignores_extra_fields() {
        // 3個目以降のフィールドは黙って無視する(将来 JS 側がフィールドを足しても、
        // 古い termmap が fx/fy だけ読んで動き続けられるようにするため)。
        assert_eq!(parse_pan_marker("\u{1}PAN\u{1}0.1\u{1}0.2\u{1}0.3\u{1}"), Some((0.1, 0.2)));
    }

    #[test]
    fn parse_pan_marker_rejects_other_pastes() {
        // 通常のペーストや他のマーカーを取り違えない(検索欄への貼り付けを食わない)。
        assert_eq!(parse_pan_marker("\u{1}GPS\u{1}35.0\u{1}139.0"), None);
        assert_eq!(parse_pan_marker("PAN\u{1}0.1\u{1}0.1\u{1}"), None);
        assert_eq!(parse_pan_marker("https://example.com/"), None);
        assert_eq!(parse_pan_marker(""), None);
    }

    #[test]
    fn parse_pan_marker_reads_what_the_overlay_actually_sends() {
        // web/touch-overlay.js の flushPan() が組み立てる文字列そのもの
        // (`sendMarkerPaste('PAN' + sx + '' + sy + '')` = 先頭SOH + 小数4桁)。
        // JS 側の組み立てと Rust 側の受け口がズレると、パンが全く効かない/検索欄へ
        // マーカーが文字として入る、という形で壊れるのでここで固定しておく。
        assert_eq!(parse_pan_marker("\u{1}PAN\u{1}0.3000\u{1}0.0000\u{1}"), Some((0.3, 0.0)));
        assert_eq!(parse_pan_marker("\u{1}PAN\u{1}0.2500\u{1}-0.1000\u{1}"), Some((0.25, -0.1)));
        // 軸モードの再送要求も同様(JS: sendMarkerPaste('DRAGMODE?'))。
        assert!("\u{1}DRAGMODE?\u{1}".starts_with(DRAG_MODE_REQUEST));
        // 2つのマーカーは互いに取り違えない。
        assert!(!DRAG_MODE_REQUEST.starts_with(PAN_MARKER));
        assert!(!PAN_MARKER.starts_with(DRAG_MODE_REQUEST));
    }

    // ── 比 → 出力ピクセルの換算(設計書 §6.2・§11) ────────────────────

    /// テスト用にレイアウトを組み立てる(引数順は Layout の宣言順と同じ)。
    fn lay(cols: u32, rows: u32, map_cols: u32, map_rows: u32, ow: u32, oh: u32) -> Layout {
        Layout { cols, rows, map_cols, map_rows, ow, oh }
    }

    #[test]
    fn pan_ratio_to_px_is_one_to_one_without_gutter() {
        // 左袖なし(map_cols == cols)・ステータス行1行ぶんだけ地図より小さい端末。
        // 指が端末幅の1/4を横切ったら、地図も出力幅 ow の1/4動く。
        let (dx, dy) = pan_ratio_to_px(0.25, 0.0, &lay(100, 40, 100, 39, 200, 156));
        assert!((dx - 50.0).abs() < 1e-9, "ow=200 の1/4=50 になること (got {dx})");
        assert_eq!(dy, 0.0);
    }

    #[test]
    fn pan_ratio_to_px_compensates_gutter_and_status_row() {
        // 左袖28桁ぶんは地図ではないので、指が端末幅の1/4動いても地図が動くのは
        // 「端末幅の1/4に相当するセル数 × 1セルあたりの出力px」= 25 * (144/72) = 50 出力px。
        let (dx, _) = pan_ratio_to_px(0.25, 0.0, &lay(100, 40, 72, 39, 144, 156));
        assert!((dx - 50.0).abs() < 1e-9, "got {dx}");
        // 縦も同じ考え方。ステータス行1行ぶんは地図でないので、端末高の1/2 = 20行 ×
        // 1行あたり4出力px(braille) = 80 出力px。
        let (_, dy) = pan_ratio_to_px(0.0, 0.5, &lay(100, 40, 72, 39, 144, 156));
        assert!((dy - 80.0).abs() < 1e-9, "got {dy}");
    }

    #[test]
    fn pan_ratio_to_px_keeps_sign_and_is_linear() {
        let (dx1, dy1) = pan_ratio_to_px(0.1, -0.2, &lay(100, 40, 72, 39, 144, 156));
        let (dx2, dy2) = pan_ratio_to_px(0.2, -0.4, &lay(100, 40, 72, 39, 144, 156));
        assert!(dx1 > 0.0 && dy1 < 0.0, "符号は指の移動方向のまま(反転は呼び出し側)");
        assert!((dx2 - dx1 * 2.0).abs() < 1e-9);
        assert!((dy2 - dy1 * 2.0).abs() < 1e-9);
    }

    #[test]
    fn pan_ratio_to_px_zero_ratio_is_zero() {
        assert_eq!(pan_ratio_to_px(0.0, 0.0, &lay(100, 40, 72, 39, 144, 156)), (0.0, 0.0));
    }

    #[test]
    fn pan_ratio_to_px_guards_zero_sized_map() {
        // 0除算でNaNを返さない(NaNがcx/cyへ入ると以降の描画が全部壊れる)。
        assert_eq!(pan_ratio_to_px(0.5, 0.5, &lay(100, 40, 0, 39, 144, 156)), (0.0, 0.0));
        assert_eq!(pan_ratio_to_px(0.5, 0.5, &lay(100, 40, 72, 0, 144, 156)), (0.0, 0.0));
    }

    // ── パン量の適用(設計書 §6.2 の適用条件) ────────────────────────

    /// z=10 のときの世界ピクセル幅。テストの期待値を書くのに使う。
    fn world_px(z: u32) -> f64 {
        (crate::geo::TILE as f64) * 2f64.powi(z as i32)
    }

    #[test]
    fn apply_pan_moves_map_opposite_to_finger() {
        // 指を右下へ→中心は左上へ動く(=地図の絵が指と同じ右下へ流れる)。
        let l = lay(100, 40, 100, 39, 200, 156);
        let (cx, cy, moved) = apply_pan(50_000.0, 50_000.0, 10, (Pan, Pan), 0.25, 0.25, &l);
        assert!(moved);
        assert!(cx < 50_000.0, "指を右へ→中心は西(cxが減る)。got {cx}");
        assert!(cy < 50_000.0, "指を下へ→中心は北(cyが減る)。got {cy}");
        // 移動量は pan_ratio_to_px と一致する。
        let (dx, dy) = pan_ratio_to_px(0.25, 0.25, &l);
        assert!((cx - (50_000.0 - dx)).abs() < 1e-9);
        assert!((cy - (50_000.0 - dy)).abs() < 1e-9);
    }

    #[test]
    fn apply_pan_applies_only_the_pan_axis() {
        // PoiList 相当 (Pan, Cursor): Xだけ動き、Yの値は捨てられる。
        let l = lay(100, 40, 72, 39, 144, 156);
        let (cx, cy, moved) = apply_pan(50_000.0, 50_000.0, 10, (Pan, Cursor), 0.2, 0.2, &l);
        assert!(moved);
        assert!(cx != 50_000.0, "X軸(Pan)は動く");
        assert_eq!(cy, 50_000.0, "Y軸(Cursor)は動かない");
    }

    #[test]
    fn apply_pan_ignores_non_pan_axes_entirely() {
        let l = lay(100, 40, 72, 39, 144, 156);
        // 一覧/メニュー相当・設定相当・入力欄相当: どの軸もPanでないので一切動かない。
        for axes in [(Nothing, Cursor), (Cursor, Cursor), (Cursor, Nothing), (Nothing, Nothing)] {
            let (cx, cy, moved) = apply_pan(50_000.0, 40_000.0, 10, axes, 0.5, 0.5, &l);
            assert!(!moved, "{axes:?} では動かないこと");
            assert_eq!((cx, cy), (50_000.0, 40_000.0));
        }
    }

    #[test]
    fn apply_pan_reports_not_moved_for_zero_delta() {
        let l = lay(100, 40, 72, 39, 144, 156);
        let (cx, cy, moved) = apply_pan(1_000.0, 2_000.0, 10, (Pan, Pan), 0.0, 0.0, &l);
        assert!(!moved, "移動量0なら「動いた」と報告しない(pan_streakを無駄にリセットしない)");
        assert_eq!((cx, cy), (1_000.0, 2_000.0));
    }

    #[test]
    fn apply_pan_wraps_x_around_the_world() {
        let l = lay(100, 40, 100, 39, 200, 156);
        let n = world_px(10);
        // 世界の西端(cx≈0)で指を右へ払う = 中心はさらに西へ動く → 東端へ回り込む
        // (地図は経度方向に連続しているので、ここで止まると世界一周できなくなる)。
        let (cx, _, moved) = apply_pan(10.0, 1_000.0, 10, (Pan, Pan), 0.5, 0.0, &l);
        assert!(moved);
        assert!((0.0..n).contains(&cx), "範囲内に収まること。got {cx} (n={n})");
        assert!(cx > n / 2.0, "西端を越えて東端側へ回り込むこと。got {cx}");
        // 逆向き(指を左へ)は中心が東へ動く。東端を越えれば西端へ回り込む。
        let (cx2, _, _) = apply_pan(n - 10.0, 1_000.0, 10, (Pan, Pan), -0.5, 0.0, &l);
        assert!((0.0..n).contains(&cx2), "範囲内に収まること。got {cx2}");
        assert!(cx2 < n / 2.0, "東端を越えて西端側へ回り込むこと。got {cx2}");
    }

    #[test]
    fn apply_pan_wraps_even_when_the_sum_exceeds_one_screen() {
        // 合算値は1マーカーの上限(±1)を超えうる。何周ぶんでも範囲内に収まること
        // (キー経路のような1回だけの ±n 加減算では収まらないケース)。
        let l = lay(100, 40, 100, 39, 200, 156);
        let n = world_px(4); // 小さい世界(4096px)で、1回のパン量が複数周ぶんになる状況を作る
        for fx in [5.0, -5.0, 50.0, -50.0] {
            let (cx, _, moved) = apply_pan(100.0, 100.0, 4, (Pan, Pan), fx, 0.0, &l);
            assert!(moved);
            assert!(cx.is_finite(), "NaN/Infにならない。fx={fx}");
            assert!((0.0..n).contains(&cx), "fx={fx} で範囲外: {cx} (n={n})");
        }
    }

    #[test]
    fn apply_pan_clamps_y_at_the_poles() {
        let l = lay(100, 40, 100, 39, 200, 156);
        let n = world_px(10);
        // 北端を越えて上へ払っても 0 で止まる(Yは巻かない)。
        let (_, cy, _) = apply_pan(1_000.0, 5.0, 10, (Pan, Pan), 0.0, 1.0, &l);
        assert_eq!(cy, 0.0, "北端でクランプ");
        // 南端も同様。
        let (_, cy2, _) = apply_pan(1_000.0, n - 5.0, 10, (Pan, Pan), 0.0, -1.0, &l);
        assert_eq!(cy2, n - 1.0, "南端でクランプ");
    }

    #[test]
    fn apply_pan_never_produces_nan() {
        // 地図領域が0セルという壊れたレイアウトでも、cx/cy に NaN を入れない
        // (NaN が入ると以降のタイル計算・描画が全部壊れる)。
        let broken = lay(100, 40, 0, 0, 144, 156);
        let (cx, cy, _) = apply_pan(1_000.0, 2_000.0, 10, (Pan, Pan), 0.5, 0.5, &broken);
        assert!(cx.is_finite() && cy.is_finite());
        assert_eq!((cx, cy), (1_000.0, 2_000.0), "換算が0なら位置は変わらない");
    }

    #[test]
    fn pan_ratio_to_px_output_stays_finite_at_limits() {
        // パース側が通す最大値(±1)でも有限で、地図の出力寸法をそれほど超えない。
        let (dx, dy) = pan_ratio_to_px(1.0, 1.0, &lay(100, 40, 72, 39, 144, 156));
        assert!(dx.is_finite() && dy.is_finite());
        assert!(dx > 0.0 && dy > 0.0);
    }
}
