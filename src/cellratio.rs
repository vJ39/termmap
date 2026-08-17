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
//!   2. 写真をセル矩形と同じ縦横比で中央から切り出す(cover方式)寸法計算(設計書 §7.6 から変更。
//!      レターボックス(余白)案は写真が小さく見える影響が大きいとしてクロップ(見切れ)案へ変更)
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

/// 写真をセル矩形の縦横比で中央から切り出す(cover方式)ときの、切り出し矩形。
/// 出力画素の縦横比 = セル矩形の物理的な縦横比になるように作るので、
/// `preserveAspectRatio=0` でセル矩形へ強制フィットさせても歪まない。
/// 元の写真より大きくなることはないので、余白(黒帯)は生じない代わりに、
/// セル矩形からはみ出す方向の端は表示されない(見切れる)。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PhotoCrop {
    /// 切り出し矩形の左上X位置[px](元画像内、中央寄せ)
    pub crop_x: u32,
    /// 切り出し矩形の左上Y位置[px](元画像内、中央寄せ)
    pub crop_y: u32,
    /// 切り出し矩形の幅[px](= 出力画像の幅)
    pub crop_w: u32,
    /// 切り出し矩形の高さ[px](= 出力画像の高さ)
    pub crop_h: u32,
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

/// 写真(`img_w` x `img_h`)から `cols` x `rows` セルの矩形と同じ縦横比の領域を、
/// 中央を基準に切り出す(cover方式、設計書 §7.6 のレターボックス案から変更)。
/// セル1個の物理寸法は 幅1 : 高さ r なので、矩形の物理的な縦横比は `cols : rows*r`。
///
/// 写真は拡大しない(切り出すだけ)。矩形の方が横長なら上下を、縦長なら左右を切り落とす。
///
/// 計算できない入力(0寸法・非有限/非正の r)では写真全体(切り出しなし)を返す。
/// 呼び出し側はその場合これまでどおりの1枚を出すことになり、挙動が悪化しない。
pub(crate) fn crop_photo_to_cells(img_w: u32, img_h: u32, cols: u32, rows: u32, r: f64) -> PhotoCrop {
    let as_is = PhotoCrop { crop_x: 0, crop_y: 0, crop_w: img_w.max(1), crop_h: img_h.max(1) };
    if img_w == 0 || img_h == 0 || cols == 0 || rows == 0 || !r.is_finite() || r <= 0.0 {
        return as_is;
    }
    let rect = cols as f64 / (rows as f64 * r); // セル矩形の物理的な 幅/高さ
    let photo = img_w as f64 / img_h as f64; // 写真の 幅/高さ(640x480 なら 4/3)
    let (crop_w, crop_h) = if rect >= photo {
        // 矩形の方が横長 → 写真の幅をいっぱいに使い、高さを縮める(上下を切り落とす)
        (img_w as f64, img_w as f64 / rect)
    } else {
        // 矩形の方が縦長(iPhone 縦持ちはこちら) → 写真の高さをいっぱいに使い、
        // 幅を縮める(左右を切り落とす)
        (img_h as f64 * rect, img_h as f64)
    };
    let crop_w = round_at_least_one(crop_w).min(img_w);
    let crop_h = round_at_least_one(crop_h).min(img_h);
    PhotoCrop {
        crop_x: (img_w - crop_w) / 2,
        crop_y: (img_h - crop_h) / 2,
        crop_w,
        crop_h,
    }
}

/// `crop_photo_to_cells` の結果どおりに、写真の中央を切り出した1枚を返す。
/// 切り出しが要らない(セル矩形が写真と同じ形)ときは複製だけ返す。
pub(crate) fn crop_photo(img: &RgbImage, cols: u32, rows: u32, r: f64) -> RgbImage {
    let c = crop_photo_to_cells(img.width(), img.height(), cols, rows, r);
    if c.crop_w == img.width() && c.crop_h == img.height() {
        return img.clone();
    }
    image::imageops::crop_imm(img, c.crop_x, c.crop_y, c.crop_w, c.crop_h).to_image()
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

    // ── クロップの寸法計算(設計書 §7.6・§9、cover方式) ──────────────────────

    /// クロップ後の画像が画面上で持つ縦横比。出力画素の縦横比 = セル矩形の物理的な縦横比
    /// (cols : rows*r)になっている(強制フィットされても正方形画素になる)ことを見る。
    fn displayed_aspect(crop: &PhotoCrop, cols: u32, rows: u32, r: f64) -> f64 {
        let _ = (cols, rows, r);
        crop.crop_w as f64 / crop.crop_h as f64
    }

    /// セル矩形の物理的な 幅/高さ。
    fn cell_rect_aspect(cols: u32, rows: u32, r: f64) -> f64 {
        cols as f64 / (rows as f64 * r)
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
    fn crop_matches_the_cell_rect_aspect_ratio_for_every_shape() {
        for (name, cols, rows, r) in photo_cases() {
            let crop = crop_photo_to_cells(640, 480, cols, rows, r);
            let got = displayed_aspect(&crop, cols, rows, r);
            let want = cell_rect_aspect(cols, rows, r);
            // 整数丸めぶんの誤差は許す(短辺が数十pxまで小さくなる極端な形では
            // 1px の丸めが数%の相対誤差になるため、レターボックス版より少し広げてある)。
            assert!(
                (got / want - 1.0).abs() < 0.03,
                "{name}: 出力の縦横比がセル矩形比から外れた。want {want:.4} got {got:.4} crop {crop:?}"
            );
        }
    }

    #[test]
    fn crop_never_exceeds_the_photo_bounds() {
        // 切り出し矩形は必ず元画像の範囲内(cover方式なので拡大はしない)。
        for (name, cols, rows, r) in photo_cases() {
            let crop = crop_photo_to_cells(640, 480, cols, rows, r);
            assert!(crop.crop_w <= 640, "{name}: 幅が元画像を超える {crop:?}");
            assert!(crop.crop_h <= 480, "{name}: 高さが元画像を超える {crop:?}");
            assert!(crop.crop_x + crop.crop_w <= 640, "{name}: 右へはみ出す {crop:?}");
            assert!(crop.crop_y + crop.crop_h <= 480, "{name}: 下へはみ出す {crop:?}");
            assert!(crop.crop_w >= 1 && crop.crop_h >= 1, "{name}: 0px の画像になった {crop:?}");
        }
    }

    #[test]
    fn crop_trims_horizontally_on_a_portrait_terminal() {
        // 今回の主症状(設計書 §4.1)。iPhone 縦持ちではセル矩形が写真よりずっと縦長なので、
        // 写真の高さをいっぱいに使い、幅の左右を切り落とす(cover方式は「領域が縦長なほど
        // 写真の左右が大きく削れる」向きになる。レターボックス案の「幅いっぱい・上下に余白」
        // とは逆軸になる点に注意)。セル比が 2.0 でも 2.2174 でも向きは同じ。
        for r in [2.0, 2.2174] {
            let crop = crop_photo_to_cells(640, 480, 50, 49, r);
            assert_eq!(crop.crop_y, 0, "r={r}: 上下は切り落とさない {crop:?}");
            assert_eq!(crop.crop_h, 480, "r={r}: 写真は高さいっぱい {crop:?}");
            assert!(crop.crop_x > 0, "r={r}: 左右が切り落とされること {crop:?}");
            assert!(crop.crop_w < 640, "r={r}: 幅は元画像より縮む {crop:?}");
        }
    }

    #[test]
    fn crop_trims_vertically_on_a_wide_terminal() {
        // 横長端末(ネイティブ端末の横長ウィンドウ)では、写真の幅をいっぱいに使い、
        // 上下を切り落とす(領域が横長なほど写真の上下が大きく削れる向き)。
        let crop = crop_photo_to_cells(640, 480, 200, 40, 2.0);
        assert_eq!(crop.crop_x, 0, "左右は切り落とさない {crop:?}");
        assert_eq!(crop.crop_w, 640, "写真は幅いっぱい {crop:?}");
        assert!(crop.crop_y > 0, "上下が切り落とされること {crop:?}");
        assert!(crop.crop_h < 480, "高さは元画像より縮む {crop:?}");
    }

    #[test]
    fn crop_adds_no_trim_when_the_rect_already_matches() {
        // セル矩形の物理形状がちょうど 4:3 のとき(cols:rows*r = 8:6)は切り落としなし。
        // 余計な再合成をしない回帰確認も兼ねる。
        let crop = crop_photo_to_cells(640, 480, 8, 3, 2.0);
        assert_eq!(crop, PhotoCrop { crop_x: 0, crop_y: 0, crop_w: 640, crop_h: 480 });
    }

    #[test]
    fn crop_centers_the_crop_region() {
        for (name, cols, rows, r) in photo_cases() {
            let crop = crop_photo_to_cells(640, 480, cols, rows, r);
            let left = crop.crop_x;
            let right = 640 - crop.crop_w - crop.crop_x;
            let top = crop.crop_y;
            let bottom = 480 - crop.crop_h - crop.crop_y;
            assert!(left.abs_diff(right) <= 1, "{name}: 左右の切り落としが偏っている {crop:?}");
            assert!(top.abs_diff(bottom) <= 1, "{name}: 上下の切り落としが偏っている {crop:?}");
        }
    }

    #[test]
    fn crop_returns_the_photo_as_is_for_degenerate_input() {
        // 計算できない入力では写真そのまま(切り出しなし)。落ちない・0pxを作らない。
        let deg = PhotoCrop { crop_x: 0, crop_y: 0, crop_w: 640, crop_h: 480 };
        assert_eq!(crop_photo_to_cells(640, 480, 0, 40, 2.0), deg);
        assert_eq!(crop_photo_to_cells(640, 480, 100, 0, 2.0), deg);
        assert_eq!(crop_photo_to_cells(640, 480, 100, 40, 0.0), deg);
        assert_eq!(crop_photo_to_cells(640, 480, 100, 40, -2.0), deg);
        assert_eq!(crop_photo_to_cells(640, 480, 100, 40, f64::NAN), deg);
        assert_eq!(crop_photo_to_cells(640, 480, 100, 40, f64::INFINITY), deg);
        // 0px の写真でも 0px の切り出し矩形を作らない。
        let z = crop_photo_to_cells(0, 0, 100, 40, 2.0);
        assert!(z.crop_w >= 1 && z.crop_h >= 1, "{z:?}");
    }

    #[test]
    fn crop_handles_non_4_by_3_sources() {
        // 道路ライブカメラは提供元によって縦横比が違う場合がある。4:3 決め打ちにしていないこと。
        for (iw, ih) in [(1920u32, 1080u32), (480, 640), (300, 300)] {
            for (name, cols, rows, r) in photo_cases() {
                let crop = crop_photo_to_cells(iw, ih, cols, rows, r);
                let got = displayed_aspect(&crop, cols, rows, r);
                let want = cell_rect_aspect(cols, rows, r);
                assert!(
                    (got / want - 1.0).abs() < 0.02,
                    "{name} {iw}x{ih}: want {want:.4} got {got:.4} crop {crop:?}"
                );
                assert!(crop.crop_w <= iw && crop.crop_h <= ih, "{name} {iw}x{ih}: 元画像を超える {crop:?}");
            }
        }
    }

    // ── 実際の合成(切り出し済みの1枚) ─────────────────────────

    /// 中心付近(±3px の小さな正方形)が縁と違う色の写真。中心を基準に切り出す実装なので、
    /// 1px の丸めで判定がぶれないよう、点ではなく小さな範囲を塗る。
    fn photo_with_distinct_center(w: u32, h: u32) -> RgbImage {
        let mut img = RgbImage::from_pixel(w, h, image::Rgb([30, 30, 200]));
        let (cx, cy) = (w / 2, h / 2);
        for dy in -3i32..=3 {
            for dx in -3i32..=3 {
                let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
                    img.put_pixel(x as u32, y as u32, image::Rgb([200, 30, 30]));
                }
            }
        }
        img
    }

    #[test]
    fn crop_photo_matches_the_computed_crop() {
        let img = photo_with_distinct_center(640, 480);
        for (name, cols, rows, r) in photo_cases() {
            let crop = crop_photo_to_cells(640, 480, cols, rows, r);
            let out = crop_photo(&img, cols, rows, r);
            assert_eq!((out.width(), out.height()), (crop.crop_w, crop.crop_h), "{name}");
        }
    }

    /// 出力画像の中心付近(±4px)のどこかに赤(中心マーカー)があるか。
    /// crop_x/crop_y の整数丸めで中心が1〜2px ずれることがあるため範囲で見る。
    fn has_red_near_center(img: &RgbImage) -> bool {
        let (cx, cy) = (img.width() / 2, img.height() / 2);
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height()
                    && *img.get_pixel(x as u32, y as u32) == image::Rgb([200, 30, 30])
                {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn crop_photo_keeps_the_center_of_the_photo() {
        let img = photo_with_distinct_center(640, 480);
        // 縦長端末: 左右を切り落としても中心(=写真の中心)は残る。
        let out = crop_photo(&img, 50, 49, 2.2174);
        assert!(has_red_near_center(&out), "中心が残ること {}x{}", out.width(), out.height());
        // 横長端末: 上下を切り落としても中心は残る。
        let out2 = crop_photo(&img, 200, 40, 2.0);
        assert!(has_red_near_center(&out2), "中心が残ること {}x{}", out2.width(), out2.height());
    }

    #[test]
    fn crop_photo_returns_the_original_when_no_trim_is_needed() {
        let img = photo_with_distinct_center(640, 480);
        let out = crop_photo(&img, 8, 3, 2.0);
        assert_eq!((out.width(), out.height()), (640, 480));
        assert_eq!(out.as_raw(), img.as_raw(), "無変換で返すこと");
    }

    #[test]
    fn emitted_image_has_square_pixels_on_the_cell_rect() {
        // ui.rs の実写/道路カメラ経路と同じ順序(クロップ → emit_iterm2_image)を通し、
        // 出力の形まで見る。ユニットテストでは配線ミス(呼び出し忘れ・引数の取り違え)を
        // 検出できないという過去の指摘(feedback_settings-toggle-wiring-gap)への備え。
        let img = photo_with_distinct_center(640, 480);
        for (name, cols, rows, r) in photo_cases() {
            let shown = crop_photo(&img, cols, rows, r);
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
            let rect = cell_rect_aspect(cols, rows, r);
            assert!(
                (px / rect - 1.0).abs() < 0.03,
                "{name}: 画素比 {px:.4} とセル矩形比 {rect:.4} が一致しない({}x{})",
                shown.width(), shown.height()
            );
        }
    }

    #[test]
    fn crop_photo_survives_degenerate_input() {
        let img = photo_with_distinct_center(640, 480);
        for (cols, rows, r) in [(0u32, 40u32, 2.0f64), (100, 0, 2.0), (100, 40, f64::NAN)] {
            let out = crop_photo(&img, cols, rows, r);
            assert_eq!((out.width(), out.height()), (640, 480));
        }
    }
}
