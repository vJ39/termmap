//! 端末セル比(r = セル高 / セル幅)の取得と、写真を歪ませずにセル矩形へ収める当てはめ計算。
//!
//! 背景は docs/web-image-aspect-ratio-design.md。実画像モード(OSC 1337)は
//! `preserveAspectRatio=0` で「指定したセル矩形へ強制フィットする」指定なので、画像の縦横比と
//! セル矩形の物理的な縦横比が食い違っているとそのぶん歪む。640x480 の実写(Street View)/
//! 道路ライブカメラの写真を端末全体のセル矩形へそのまま流し込んでいた箇所(設計書 §4.1)は、
//! iPhone 縦持ちで縦に約2.6倍伸びていた。
//!
//! ここには次の2つを閉じてある。どちらも純関数にしてあり、UI を起こさずに検証できる。
//!   1. セル比 r の取得(ネイティブ端末 = `window_size()` / web = ブラウザからの CELL マーカー)
//!   2. 写真をセル矩形へ内接させるレターボックスの寸法計算(設計書 §7.6)
//!
//! 地図タイル側の歪み(設計書 §2・最大1割強)はここでは扱わない。実画像モードの地図の
//! 生成解像度 `rh` は `src/ui.rs` で `map_rows * 2 * scale` のままにしてある。r を掛ける形に
//! 変えると、地図の地理的な縦幅だけが変わってドラッグのパン量換算
//! (`dragmode::pan_ratio_to_px` が使う `oh` は `map_rows * 2` のまま)とズレるため、
//! そちらは `Layout` の見直しとセットで別途行う。

use image::RgbImage;

/// ブラウザ → termmap のセル寸法マーカーの先頭(設計書 §7.2 の経路2)。
/// 形式は `\u{1}CELL\u{1}<cw>\u{1}<ch>\u{1}`。cw/ch はセル幅・セル高で、使うのは比だけなので
/// 単位系(CSS px / device px)は問わない。`\u{1}GPS\u{1}` / `\u{1}PAN\u{1}` と同じ SOH 区切りの
/// 専用マーカーにしてあり、検索欄への貼り付け等の通常のペーストとは衝突しない。
pub(crate) const CELL_MARKER: &str = "\u{1}CELL\u{1}";

/// セル比が取れないときの既定値。従来コードが暗黙に置いていた「1セル = 縦横比 1:2」と同じ値で、
/// 既定へ落ちた場合の見え方は修正前と変わらない。
pub(crate) const DEFAULT_CELL_RATIO: f64 = 2.0;

/// 受け入れるセル比の下限・上限。等幅フォントの実測値は概ね 1.6〜2.6
/// (設計書 §3 の Menlo 13px で 1.93〜2.22)に収まる。ここを大きく外れる値は計測ミスや
/// 壊れた通知とみなして捨て、既定値へ落とす。片側に十分な余裕を持たせた 1.0〜4.0 を採る。
pub(crate) const MIN_CELL_RATIO: f64 = 1.0;
pub(crate) const MAX_CELL_RATIO: f64 = 4.0;

/// レターボックス後のキャンバスの最大辺[px]。縦長端末では余白のぶんキャンバスが写真より
/// はるかに大きくなる(iPhone 縦持ちで 640x1379 程度)ため、極端に細長い端末で PNG 符号化が
/// 重くならないよう上限を置く。表示先の端末は高々数千 device px なので、ここで頭打ちに
/// しても見た目は落ちない。
const MAX_CANVAS_PX: f64 = 2048.0;

/// セル幅・セル高から比 r = ch / cw を作る。壊れた値・非現実的な比は None を返して捨てる
/// (`dragmode::parse_pan_marker` と同じく「丸めて一部を活かす」ことはしない。おかしな比で
/// 描くくらいなら既定値 2.0 で描いた方が被害が小さい)。
pub(crate) fn ratio_from_cell_size(cw: f64, ch: f64) -> Option<f64> {
    if !cw.is_finite() || !ch.is_finite() || cw <= 0.0 || ch <= 0.0 {
        return None;
    }
    let r = ch / cw;
    if !(MIN_CELL_RATIO..=MAX_CELL_RATIO).contains(&r) {
        return None;
    }
    Some(r)
}

/// セル寸法マーカー(`\u{1}CELL\u{1}<cw>\u{1}<ch>\u{1}`)を比 r に解く。
/// 3個目以降のフィールドは黙って無視する(将来 JS 側がフィールドを足しても古い termmap が
/// cw/ch だけ読んで動き続けられるようにするため。PAN マーカーと同じ方針)。
pub(crate) fn parse_cell_marker(s: &str) -> Option<f64> {
    let rest = s.strip_prefix(CELL_MARKER)?;
    let mut parts = rest.split('\u{1}');
    let cw: f64 = parts.next()?.trim().parse().ok()?;
    let ch: f64 = parts.next()?.trim().parse().ok()?;
    ratio_from_cell_size(cw, ch)
}

/// TIOCGWINSZ 由来の画素サイズ・文字数からセル比を出す(設計書 §7.2 の経路1)。
/// ttyd のように画素サイズを埋めない端末では 0 が来るので、その場合は None を返して
/// 呼び出し側を次の経路へ落とす。
pub(crate) fn ratio_from_window_size(width_px: u16, height_px: u16, cols: u16, rows: u16) -> Option<f64> {
    if width_px == 0 || height_px == 0 || cols == 0 || rows == 0 {
        return None;
    }
    ratio_from_cell_size(width_px as f64 / cols as f64, height_px as f64 / rows as f64)
}

/// ネイティブ端末(iTerm2 / WezTerm 等)からセル比を取る。取れなければ None。
pub(crate) fn detect_native_ratio() -> Option<f64> {
    let ws = crossterm::terminal::window_size().ok()?;
    ratio_from_window_size(ws.width, ws.height, ws.columns, ws.rows)
}

/// 取得経路の優先順(設計書 §7.2): ネイティブの `window_size()` → ブラウザの CELL マーカー →
/// 既定値 2.0。どちらの経路も無い端末では既定値になり、修正前と同じ挙動になる。
pub(crate) fn resolve_ratio(native: Option<f64>, web: Option<f64>) -> f64 {
    native.or(web).unwrap_or(DEFAULT_CELL_RATIO)
}

/// 写真をセル矩形へ内接させたときの、余白込みキャンバスと写真の配置。
/// キャンバスは `preserveAspectRatio=0` でセル矩形へ強制フィットされる前提なので、
/// キャンバスの画素の縦横比 = セル矩形の物理的な縦横比 になるように作る。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PhotoFit {
    /// 余白込みキャンバスの幅[px]
    pub canvas_w: u32,
    /// 余白込みキャンバスの高さ[px]
    pub canvas_h: u32,
    /// キャンバス内に置く写真の幅[px]
    pub photo_w: u32,
    /// キャンバス内に置く写真の高さ[px]
    pub photo_h: u32,
    /// 写真の左上のX位置[px](中央寄せ)
    pub off_x: u32,
    /// 写真の左上のY位置[px](中央寄せ)
    pub off_y: u32,
}

/// 1未満・非有限へ落ちないように丸める。0px の画像は作れないため。
fn round_at_least_one(v: f64) -> u32 {
    let r = v.round();
    if !r.is_finite() || r < 1.0 {
        1
    } else {
        r.min(u32::MAX as f64) as u32
    }
}

/// 写真(`img_w` x `img_h`)を `cols` x `rows` セルの矩形へ、縦横比を保ったまま内接させる
/// (設計書 §7.6)。セル1個の物理寸法は 幅1 : 高さ r なので、矩形の物理的な縦横比は
/// `cols : rows*r`。この形のキャンバスを作って写真を中央に置けば、キャンバスを矩形へ
/// 強制フィットさせても写真部分は歪まない。
///
/// 写真は拡大しない(余白を足すだけ)。ただしキャンバスが `MAX_CANVAS_PX` を超えるほど
/// 細長くなる場合だけ、全体を等倍で縮めて収める。
///
/// 計算できない入力(0寸法・非有限/非正の r)では写真そのままの寸法を返す。呼び出し側は
/// その場合これまでどおりの1枚を出すことになり、挙動が修正前より悪化しない。
pub(crate) fn fit_photo_into_cells(img_w: u32, img_h: u32, cols: u32, rows: u32, r: f64) -> PhotoFit {
    let as_is = PhotoFit {
        canvas_w: img_w.max(1),
        canvas_h: img_h.max(1),
        photo_w: img_w.max(1),
        photo_h: img_h.max(1),
        off_x: 0,
        off_y: 0,
    };
    if img_w == 0 || img_h == 0 || cols == 0 || rows == 0 || !r.is_finite() || r <= 0.0 {
        return as_is;
    }
    let rect = cols as f64 / (rows as f64 * r); // セル矩形の物理的な 幅/高さ
    let photo = img_w as f64 / img_h as f64; // 写真の 幅/高さ(640x480 なら 4/3)
    let (mut canvas_w, mut canvas_h, mut photo_w, mut photo_h) = if rect >= photo {
        // 矩形の方が横長 → 写真は高さいっぱいに置き、左右へ余白を足す
        (img_h as f64 * rect, img_h as f64, img_w as f64, img_h as f64)
    } else {
        // 矩形の方が縦長(iPhone 縦持ちはこちら) → 写真は幅いっぱいに置き、上下へ余白を足す
        (img_w as f64, img_w as f64 / rect, img_w as f64, img_h as f64)
    };
    let longest = canvas_w.max(canvas_h);
    if longest > MAX_CANVAS_PX {
        let k = MAX_CANVAS_PX / longest; // 写真も同じ倍率で縮めるので縦横比は保たれる
        canvas_w *= k;
        canvas_h *= k;
        photo_w *= k;
        photo_h *= k;
    }
    let canvas_w = round_at_least_one(canvas_w);
    let canvas_h = round_at_least_one(canvas_h);
    let photo_w = round_at_least_one(photo_w).min(canvas_w);
    let photo_h = round_at_least_one(photo_h).min(canvas_h);
    PhotoFit {
        canvas_w,
        canvas_h,
        photo_w,
        photo_h,
        off_x: (canvas_w - photo_w) / 2,
        off_y: (canvas_h - photo_h) / 2,
    }
}

/// `fit_photo_into_cells` の結果どおりに、余白(黒)込みの1枚へ合成する。
/// 余白も縮小も要らない(セル矩形が写真と同じ形)ときは複製だけ返す。
pub(crate) fn letterbox_photo(img: &RgbImage, cols: u32, rows: u32, r: f64) -> RgbImage {
    let fit = fit_photo_into_cells(img.width(), img.height(), cols, rows, r);
    let same_photo = fit.photo_w == img.width() && fit.photo_h == img.height();
    if same_photo && fit.canvas_w == img.width() && fit.canvas_h == img.height() {
        return img.clone();
    }
    let scaled = if same_photo {
        None
    } else {
        Some(image::imageops::resize(
            img,
            fit.photo_w,
            fit.photo_h,
            image::imageops::FilterType::Triangle,
        ))
    };
    let src = scaled.as_ref().unwrap_or(img);
    let mut canvas = RgbImage::from_pixel(fit.canvas_w, fit.canvas_h, image::Rgb([0, 0, 0]));
    image::imageops::replace(&mut canvas, src, fit.off_x as i64, fit.off_y as i64);
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── セル比の取得(設計書 §7.2・§9) ────────────────────────────────

    /// テスト用にマーカー文字列を組み立てる。JS 側(sendCellSize)が送る形と同じ。
    fn cell_marker(cw: &str, ch: &str) -> String {
        format!("{CELL_MARKER}{cw}\u{1}{ch}\u{1}")
    }

    #[test]
    fn parse_cell_marker_accepts_normal_values() {
        // 設計書 §3 の iPhone 実測見積り(Menlo 13px / DPR=3 で最も不利に転んだ場合)。
        let r = parse_cell_marker(&cell_marker("7.6667", "17.0000")).expect("正常値はパースできる");
        assert!((r - 17.0 / 7.6667).abs() < 1e-9, "got {r}");
        // 有利に転んだ場合はちょうど 2.0。
        let r2 = parse_cell_marker(&cell_marker("7.6667", "15.3333")).expect("正常値はパースできる");
        assert!((r2 - 2.0).abs() < 1e-3, "got {r2}");
    }

    #[test]
    fn parse_cell_marker_works_without_trailing_separator() {
        // 末尾の区切りが無い形(GPSマーカーと同じ形)でも読めること。
        assert_eq!(parse_cell_marker("\u{1}CELL\u{1}8\u{1}16"), Some(2.0));
    }

    #[test]
    fn parse_cell_marker_ignores_extra_fields() {
        // 3個目以降は黙って無視する(将来 JS がフィールドを足しても古い termmap が動く)。
        assert_eq!(parse_cell_marker("\u{1}CELL\u{1}8\u{1}16\u{1}3\u{1}"), Some(2.0));
    }

    #[test]
    fn parse_cell_marker_rejects_missing_fields() {
        assert_eq!(parse_cell_marker("\u{1}CELL\u{1}8"), None); // ch が無い
        assert_eq!(parse_cell_marker("\u{1}CELL\u{1}"), None); // マーカーだけ
        assert_eq!(parse_cell_marker("\u{1}CELL\u{1}\u{1}"), None); // 区切りだけで中身が無い
    }

    #[test]
    fn parse_cell_marker_rejects_non_numeric() {
        assert_eq!(parse_cell_marker(&cell_marker("abc", "16")), None);
        assert_eq!(parse_cell_marker(&cell_marker("8", "")), None);
        assert_eq!(parse_cell_marker(&cell_marker("8,16", "16")), None);
    }

    #[test]
    fn parse_cell_marker_rejects_zero_and_negative() {
        // 0 は 0除算/Inf、負は上下反転した画像になる。どちらも捨てて既定値へ落とす。
        assert_eq!(parse_cell_marker(&cell_marker("0", "16")), None);
        assert_eq!(parse_cell_marker(&cell_marker("8", "0")), None);
        assert_eq!(parse_cell_marker(&cell_marker("-8", "16")), None);
        assert_eq!(parse_cell_marker(&cell_marker("8", "-16")), None);
    }

    #[test]
    fn parse_cell_marker_rejects_nan_and_infinity() {
        assert_eq!(parse_cell_marker(&cell_marker("NaN", "16")), None);
        assert_eq!(parse_cell_marker(&cell_marker("8", "NaN")), None);
        assert_eq!(parse_cell_marker(&cell_marker("inf", "16")), None);
        assert_eq!(parse_cell_marker(&cell_marker("8", "inf")), None);
    }

    #[test]
    fn parse_cell_marker_rejects_unrealistic_ratios() {
        // 等幅フォントではありえない比。計測ミス(まだ 0 セルの矩形を読んだ等)とみなして捨てる。
        assert_eq!(parse_cell_marker(&cell_marker("1", "100")), None); // 100:1
        assert_eq!(parse_cell_marker(&cell_marker("100", "1")), None); // 1:100
        // 境界はちょうど通す。
        assert_eq!(parse_cell_marker(&cell_marker("1", "1")), Some(MIN_CELL_RATIO));
        assert_eq!(parse_cell_marker(&cell_marker("1", "4")), Some(MAX_CELL_RATIO));
        assert_eq!(parse_cell_marker(&cell_marker("1", "4.001")), None);
    }

    #[test]
    fn parse_cell_marker_rejects_other_markers_and_pastes() {
        // 他のマーカーや通常のペーストを取り違えない(検索欄への貼り付けを食わない)。
        assert_eq!(parse_cell_marker("\u{1}GPS\u{1}35.0\u{1}139.0"), None);
        assert_eq!(parse_cell_marker("\u{1}PAN\u{1}0.1\u{1}0.1\u{1}"), None);
        assert_eq!(parse_cell_marker("\u{1}DRAGMODE?\u{1}"), None);
        assert_eq!(parse_cell_marker("CELL\u{1}8\u{1}16\u{1}"), None); // 先頭SOHが無い
        assert_eq!(parse_cell_marker("https://example.com/"), None);
        assert_eq!(parse_cell_marker(""), None);
    }

    #[test]
    fn parse_cell_marker_does_not_collide_with_other_markers() {
        assert!(!CELL_MARKER.starts_with(crate::dragmode::PAN_MARKER));
        assert!(!crate::dragmode::PAN_MARKER.starts_with(CELL_MARKER));
        assert!(!CELL_MARKER.starts_with(crate::dragmode::DRAG_MODE_REQUEST));
        assert!(!"\u{1}GPS\u{1}".starts_with(CELL_MARKER));
    }

    #[test]
    fn parse_cell_marker_reads_what_the_overlay_actually_sends() {
        // web/touch-overlay.js の sendCellSize() が組み立てる文字列そのもの
        // (`sendMarkerPaste('CELL' + SOH + cw.toFixed(4) + SOH + ch.toFixed(4) + SOH)`
        //  = 先頭SOH + 小数4桁)。JS 側の組み立てと Rust 側の受け口がズレると、セル比が
        // 一生届かず既定値のまま(=写真が歪んだまま)という形で静かに壊れるので固定しておく。
        // 値は設計書 §4.1 の iPhone 縦持ち実測(383x750 px / 50桁 x 49行)から。
        let r = parse_cell_marker("\u{1}CELL\u{1}7.6600\u{1}15.3061\u{1}").expect("overlay の形が読めること");
        assert!((r - 1.9982).abs() < 1e-3, "got {r}");
        // 回転して横持ちになった場合(750x383 px / 98桁 x 25行)も同じ形で届く。
        let r2 = parse_cell_marker("\u{1}CELL\u{1}7.6531\u{1}15.3200\u{1}").expect("overlay の形が読めること");
        assert!((r2 - 2.0018).abs() < 1e-3, "got {r2}");
    }

    #[test]
    fn ratio_from_window_size_uses_pixels_per_cell() {
        // iTerm2 のような端末: 800x960 px を 100桁x40行 → cw=8, ch=24 → r=3.0
        assert_eq!(ratio_from_window_size(800, 960, 100, 40), Some(3.0));
        // 一般的な 1:2
        assert_eq!(ratio_from_window_size(800, 640, 100, 40), Some(2.0));
    }

    #[test]
    fn ratio_from_window_size_returns_none_when_pixels_are_zero() {
        // ttyd は cols/rows しか埋めない見込み(設計書 §10)。0 が来たら次の経路へ落とす。
        assert_eq!(ratio_from_window_size(0, 0, 100, 40), None);
        assert_eq!(ratio_from_window_size(800, 0, 100, 40), None);
        assert_eq!(ratio_from_window_size(0, 640, 100, 40), None);
        // 文字数側が 0 でも 0除算しない。
        assert_eq!(ratio_from_window_size(800, 640, 0, 40), None);
        assert_eq!(ratio_from_window_size(800, 640, 100, 0), None);
    }

    #[test]
    fn resolve_ratio_follows_the_designed_priority() {
        // 設計書 §7.2: window_size() → CELL マーカー → 既定 2.0
        assert_eq!(resolve_ratio(Some(2.4), Some(2.2)), 2.4, "ネイティブが最優先");
        assert_eq!(resolve_ratio(None, Some(2.2)), 2.2, "ネイティブが取れなければ web の通知");
        assert_eq!(resolve_ratio(Some(1.8), None), 1.8);
        assert_eq!(resolve_ratio(None, None), DEFAULT_CELL_RATIO, "どちらも無ければ 2.0");
        assert_eq!(DEFAULT_CELL_RATIO, 2.0, "既定は従来コードの暗黙値と同じ");
    }

    #[test]
    fn detect_native_ratio_does_not_panic() {
        // 端末に繋がっていない CI でも Err になるだけで落ちないこと。
        let _ = detect_native_ratio();
    }

    // ── レターボックスの寸法計算(設計書 §7.6・§9) ──────────────────────

    /// 写真部分が画面上で持つ縦横比。キャンバスはセル矩形へ強制フィットされるので、
    /// 写真部分の物理的な 幅:高さ は (photo_w/canvas_w)*cols : (photo_h/canvas_h)*rows*r。
    fn displayed_aspect(fit: &PhotoFit, cols: u32, rows: u32, r: f64) -> f64 {
        let w = fit.photo_w as f64 / fit.canvas_w as f64 * cols as f64;
        let h = fit.photo_h as f64 / fit.canvas_h as f64 * rows as f64 * r;
        w / h
    }

    /// 640x480(4:3)の写真を、いろいろな端末形状・セル比へ当てはめる。
    fn photo_cases() -> Vec<(&'static str, u32, u32, f64)> {
        vec![
            // (名前, cols, rows, r)
            ("正方形セル・横長端末", 100, 40, 1.0),
            ("正方形セル・縦長端末", 40, 100, 1.0),
            ("標準セル(1:2)・横長端末", 100, 40, 2.0),
            ("標準セル(1:2)・正方形に近い端末", 80, 40, 2.0),
            ("iPhone縦持ち(実測見積りの上限)", 50, 49, 2.2174),
            ("iPhone縦持ち(実測見積りの下限)", 50, 49, 2.0),
            ("iPhone横持ち", 100, 22, 2.2174),
            ("縦長セル(行間広め)", 100, 40, 3.5),
            ("極端に横長な端末", 400, 12, 2.0),
            ("極端に細い端末", 20, 60, 2.2),
            ("最小クランプ相当(10桁)", 10, 3, 2.0),
        ]
    }

    #[test]
    fn fit_preserves_photo_aspect_ratio_for_every_shape() {
        let want = 640.0 / 480.0;
        for (name, cols, rows, r) in photo_cases() {
            let fit = fit_photo_into_cells(640, 480, cols, rows, r);
            let got = displayed_aspect(&fit, cols, rows, r);
            // 整数丸めぶんの誤差は許す(キャンバスの短辺が数百pxあるので実害は無い)。
            assert!(
                (got / want - 1.0).abs() < 0.01,
                "{name}: 表示上の縦横比が 4:3 から外れた。want {want:.4} got {got:.4} fit {fit:?}"
            );
        }
    }

    #[test]
    fn fit_never_crops_the_photo() {
        // 写真は必ずキャンバスに収まる(切り落とさない=レターボックスであってクロップではない)。
        for (name, cols, rows, r) in photo_cases() {
            let fit = fit_photo_into_cells(640, 480, cols, rows, r);
            assert!(fit.photo_w <= fit.canvas_w, "{name}: 横がはみ出す {fit:?}");
            assert!(fit.photo_h <= fit.canvas_h, "{name}: 縦がはみ出す {fit:?}");
            assert!(fit.off_x + fit.photo_w <= fit.canvas_w, "{name}: 右へはみ出す {fit:?}");
            assert!(fit.off_y + fit.photo_h <= fit.canvas_h, "{name}: 下へはみ出す {fit:?}");
            assert!(fit.photo_w >= 1 && fit.photo_h >= 1, "{name}: 0px の画像になった {fit:?}");
        }
    }

    #[test]
    fn fit_pads_vertically_on_a_portrait_terminal() {
        // 今回の主症状(設計書 §4.1)。iPhone 縦持ちではセル矩形が写真よりずっと縦長なので、
        // 上下に余白が入り左右には入らない。セル比が 2.0 でも 2.2174 でも向きは同じ。
        for r in [2.0, 2.2174] {
            let fit = fit_photo_into_cells(640, 480, 50, 49, r);
            assert_eq!(fit.off_x, 0, "r={r}: 左右には余白を入れない {fit:?}");
            assert!(fit.off_y > 0, "r={r}: 上下へ余白が入ること {fit:?}");
            assert_eq!(fit.photo_w, fit.canvas_w, "r={r}: 写真は幅いっぱい {fit:?}");
            assert!(fit.canvas_h > fit.photo_h, "r={r}: キャンバスは写真より縦長 {fit:?}");
            // 修正前は写真がこの矩形へ強制フィットされて縦に伸びていた。余白が入ったぶん、
            // 写真の占める高さの割合はその伸び率の逆数まで下がる。
            let stretch_before: f64 = (49.0 * r) / 50.0 / (480.0 / 640.0);
            let occupancy = fit.photo_h as f64 / fit.canvas_h as f64;
            assert!((occupancy - 1.0 / stretch_before).abs() < 0.01, "r={r}: got {occupancy}");
        }
        // 設計書 §4.1 の「縦に約2.6倍」は、実測した表示矩形 383x750 px(= 50桁x49行なら
        // cw≈7.66 / ch≈15.31 でセル比 2.0)から出した値。同じ前提で再計算して一致を確認する。
        let stretch_at_r2: f64 = (49.0 * 2.0) / 50.0 / (480.0 / 640.0);
        assert!((stretch_at_r2 - 2.6).abs() < 0.05, "設計書の見積り(約2.6倍)と一致 {stretch_at_r2}");
        // セル比がもっと縦長(2.2174)に転ぶと歪みはさらに大きくなる。
        let stretch_at_r22: f64 = (49.0 * 2.2174) / 50.0 / (480.0 / 640.0);
        assert!(stretch_at_r22 > stretch_at_r2, "{stretch_at_r22} > {stretch_at_r2}");
    }

    #[test]
    fn fit_pads_horizontally_on_a_wide_terminal() {
        // 横長端末では左右に余白が入る(ネイティブ端末の横長ウィンドウ。設計書 §4.1 の「3割程度」)。
        let fit = fit_photo_into_cells(640, 480, 200, 40, 2.0);
        assert_eq!(fit.off_y, 0, "上下には余白を入れない {fit:?}");
        assert!(fit.off_x > 0, "左右へ余白が入ること {fit:?}");
        assert_eq!(fit.photo_h, fit.canvas_h, "写真は高さいっぱい {fit:?}");
    }

    #[test]
    fn fit_adds_no_padding_when_the_rect_already_matches() {
        // セル矩形の物理形状がちょうど 4:3 のとき(cols:rows*r = 8:6)は余白なし。
        // 余計な再合成をしない回帰確認も兼ねる。
        let fit = fit_photo_into_cells(640, 480, 8, 3, 2.0);
        assert_eq!(
            fit,
            PhotoFit { canvas_w: 640, canvas_h: 480, photo_w: 640, photo_h: 480, off_x: 0, off_y: 0 }
        );
    }

    #[test]
    fn fit_centers_the_photo() {
        for (name, cols, rows, r) in photo_cases() {
            let fit = fit_photo_into_cells(640, 480, cols, rows, r);
            let left = fit.off_x;
            let right = fit.canvas_w - fit.photo_w - fit.off_x;
            let top = fit.off_y;
            let bottom = fit.canvas_h - fit.photo_h - fit.off_y;
            assert!(left.abs_diff(right) <= 1, "{name}: 左右の余白が偏っている {fit:?}");
            assert!(top.abs_diff(bottom) <= 1, "{name}: 上下の余白が偏っている {fit:?}");
        }
    }

    #[test]
    fn fit_caps_the_canvas_size() {
        // 極端に細長い端末でもキャンバスが青天井にならない(PNG符号化のコスト上限)。
        let fit = fit_photo_into_cells(640, 480, 10, 200, 2.2);
        assert!(fit.canvas_h as f64 <= MAX_CANVAS_PX, "縦の上限 {fit:?}");
        assert!(fit.canvas_w as f64 <= MAX_CANVAS_PX, "横の上限 {fit:?}");
        // 上限で縮めても縦横比は保たれる。
        let got = displayed_aspect(&fit, 10, 200, 2.2);
        assert!((got / (640.0 / 480.0) - 1.0).abs() < 0.02, "got {got} fit {fit:?}");
        // 逆方向(極端に横長)も同じ。
        let wide = fit_photo_into_cells(640, 480, 600, 4, 2.0);
        assert!(wide.canvas_w as f64 <= MAX_CANVAS_PX, "横の上限 {wide:?}");
    }

    #[test]
    fn fit_returns_the_photo_as_is_for_degenerate_input() {
        // 計算できない入力では写真そのまま = 修正前と同じ挙動。落ちない・0pxを作らない。
        let deg = PhotoFit { canvas_w: 640, canvas_h: 480, photo_w: 640, photo_h: 480, off_x: 0, off_y: 0 };
        assert_eq!(fit_photo_into_cells(640, 480, 0, 40, 2.0), deg);
        assert_eq!(fit_photo_into_cells(640, 480, 100, 0, 2.0), deg);
        assert_eq!(fit_photo_into_cells(640, 480, 100, 40, 0.0), deg);
        assert_eq!(fit_photo_into_cells(640, 480, 100, 40, -2.0), deg);
        assert_eq!(fit_photo_into_cells(640, 480, 100, 40, f64::NAN), deg);
        assert_eq!(fit_photo_into_cells(640, 480, 100, 40, f64::INFINITY), deg);
        // 0px の写真でも 0px のキャンバスを作らない。
        let z = fit_photo_into_cells(0, 0, 100, 40, 2.0);
        assert!(z.canvas_w >= 1 && z.canvas_h >= 1 && z.photo_w >= 1 && z.photo_h >= 1, "{z:?}");
    }

    #[test]
    fn fit_handles_non_4_by_3_sources() {
        // 道路ライブカメラは提供元によって縦横比が違う場合がある。4:3 決め打ちにしていないこと。
        for (iw, ih) in [(1920u32, 1080u32), (480, 640), (300, 300)] {
            let want = iw as f64 / ih as f64;
            for (name, cols, rows, r) in photo_cases() {
                let fit = fit_photo_into_cells(iw, ih, cols, rows, r);
                let got = displayed_aspect(&fit, cols, rows, r);
                assert!(
                    (got / want - 1.0).abs() < 0.02,
                    "{name} {iw}x{ih}: want {want:.4} got {got:.4} fit {fit:?}"
                );
            }
        }
    }

    // ── 実際の合成(レターボックス済みの1枚) ─────────────────────────

    /// 全面が同じ色の写真。余白(黒)と写真部分を区別できるよう黒以外にする。
    fn solid_photo(w: u32, h: u32) -> RgbImage {
        RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30]))
    }

    #[test]
    fn letterbox_photo_matches_the_computed_fit() {
        let img = solid_photo(640, 480);
        for (name, cols, rows, r) in photo_cases() {
            let fit = fit_photo_into_cells(640, 480, cols, rows, r);
            let out = letterbox_photo(&img, cols, rows, r);
            assert_eq!((out.width(), out.height()), (fit.canvas_w, fit.canvas_h), "{name}");
        }
    }

    #[test]
    fn letterbox_photo_puts_the_photo_in_the_middle_and_black_in_the_margins() {
        let img = solid_photo(640, 480);
        // 縦長端末: 上下が黒・中央が写真。
        let out = letterbox_photo(&img, 50, 49, 2.2174);
        let (w, h) = (out.width(), out.height());
        assert_eq!(*out.get_pixel(w / 2, 0), image::Rgb([0, 0, 0]), "上端は余白");
        assert_eq!(*out.get_pixel(w / 2, h - 1), image::Rgb([0, 0, 0]), "下端は余白");
        assert_eq!(*out.get_pixel(w / 2, h / 2), image::Rgb([200, 30, 30]), "中央は写真");
        // 横長端末: 左右が黒・中央が写真。
        let out2 = letterbox_photo(&img, 200, 40, 2.0);
        let (w2, h2) = (out2.width(), out2.height());
        assert_eq!(*out2.get_pixel(0, h2 / 2), image::Rgb([0, 0, 0]), "左端は余白");
        assert_eq!(*out2.get_pixel(w2 - 1, h2 / 2), image::Rgb([0, 0, 0]), "右端は余白");
        assert_eq!(*out2.get_pixel(w2 / 2, h2 / 2), image::Rgb([200, 30, 30]), "中央は写真");
    }

    #[test]
    fn letterbox_photo_returns_the_original_when_no_padding_is_needed() {
        let img = solid_photo(640, 480);
        let out = letterbox_photo(&img, 8, 3, 2.0);
        assert_eq!((out.width(), out.height()), (640, 480));
        assert_eq!(out.as_raw(), img.as_raw(), "無変換で返すこと");
    }

    #[test]
    fn emitted_image_has_square_pixels_on_the_cell_rect() {
        // ui.rs の実写/道路カメラ経路と同じ順序(レターボックス → emit_iterm2_image)を通し、
        // 出力の形まで見る。ユニットテストでは配線ミス(呼び出し忘れ・引数の取り違え)を
        // 検出できないという過去の指摘(feedback_settings-toggle-wiring-gap)への備え。
        let img = solid_photo(640, 480);
        for (name, cols, rows, r) in photo_cases() {
            let shown = letterbox_photo(&img, cols, rows, r);
            let mut out: Vec<u8> = Vec::new();
            crate::render::emit_iterm2_image(&mut out, &shown, cols, rows).expect("書き出せること");
            let s = String::from_utf8(out).expect("ヘッダはASCII・本体はbase64");
            assert!(s.starts_with("\u{1b}]1337;File=inline=1;"), "{name}: iTerm2 インライン画像の形でない");
            assert!(s.ends_with('\u{7}'), "{name}: BEL で終端していない");
            assert!(s.contains(&format!(";width={cols};height={rows};")), "{name}: セル矩形の指定が違う");
            // preserveAspectRatio は 0(強制フィット)のまま。設計書 §7.4 の理由で 1 にはしない。
            assert!(s.contains("preserveAspectRatio=0"), "{name}: 強制フィット指定でない");
            // size=0 だとブラウザ側アドオンが本体を1バイトも読まず真っ黒になる(render.rs のコメント)。
            let size: usize = s
                .split(";size=").nth(1).and_then(|t| t.split(';').next()).and_then(|t| t.parse().ok())
                .unwrap_or(0);
            assert!(size > 0, "{name}: size が入っていない");
            let body = s.split(':').next_back().unwrap_or("").trim_end_matches('\u{7}');
            assert!(!body.is_empty(), "{name}: base64 本体が空");
            // 本題: 画像の画素比が「セル矩形の物理比」と一致すること。強制フィットされても
            // 画面上の1画素が正方形になり、中の写真が歪まない。
            let px = shown.width() as f64 / shown.height() as f64;
            let rect = cols as f64 / (rows as f64 * r);
            assert!(
                (px / rect - 1.0).abs() < 0.01,
                "{name}: 画素比 {px:.4} とセル矩形比 {rect:.4} が一致しない({}x{})",
                shown.width(), shown.height()
            );
        }
    }

    #[test]
    fn letterbox_photo_survives_degenerate_input() {
        let img = solid_photo(640, 480);
        for (cols, rows, r) in [(0u32, 40u32, 2.0f64), (100, 0, 2.0), (100, 40, f64::NAN)] {
            let out = letterbox_photo(&img, cols, rows, r);
            assert_eq!((out.width(), out.height()), (640, 480));
        }
    }
}
