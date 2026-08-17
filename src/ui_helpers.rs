// ui.rs の interactive() の外にあった小さなヘルパー群(状態を持たない/引数だけで完結するもの)。
// ループ本体と混ざっていると読みにくいのでここへ集約した。

use crate::*;
use crate::geo::*;
use crate::render::*;
use image::RgbImage;

// 初回起動オンボーディングの既読マーカー(~/.config/termmap/onboarded)。存在すれば以後は出さない。
pub(crate) fn onboarded_marker() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config/termmap/onboarded"))
}

// スマホ共有QRの表示内容。Text=既定のDense1x2文字描画(全端末で動作)/Image=iTerm2インライン画像
// (見た目のセルサイズをモジュール数と切り離して小さくできるが、image_capable()な端末限定)。
pub(crate) enum QrView { Text(String), Image(RgbImage) }

// cfg.qr_style に応じてQrViewを組み立てる。"image"指定でも非対応端末ならTextへ自動フォールバックする。
pub(crate) fn build_qr_view(c: &qrcode::QrCode, style: &str) -> QrView {
    if style == "image" && image_capable() {
        let w = c.width();
        let dark: Vec<bool> = c.to_colors().iter().map(|col| *col == qrcode::Color::Dark).collect();
        QrView::Image(render_qr_image(&dark, w, 8, 4))
    } else {
        QrView::Text(c.render::<qrcode::render::unicode::Dense1x2>().quiet_zone(false).build())
    }
}

// 雨雲レーダーの不透明度(0.0..=1.0)。1.0にしないのは地図が消えたら地図アプリとして機能しないため。
// 設定 [radar] opacity の3択を実際の値へ読み替える(薄い=地図優先 / 標準 / 濃い=雨優先)。
// 未知の値(configを手書きで壊した場合)は標準扱いにして必ず描ける値を返す。
pub(crate) fn radar_opacity_value(cfg: &config::Config) -> f64 {
    match cfg.radar_opacity.as_str() { "light" => 0.35, "strong" => 0.75, _ => 0.55 }
}
// 人口メッシュの不透明度(0.0..=1.0)。雨雲と同じ3択・同じ値。面を塗る唯一のレイヤなので、
// ここを1.0にすると道路も経路も完全に消えて地図が読めなくなる。
// 実際に塗られる濃さは、この値に階級ごとのアルファ(薄い階級=40 / 都心=230)が掛かる。
pub(crate) fn population_opacity_value(cfg: &config::Config) -> f64 {
    match cfg.population_opacity.as_str() { "light" => 0.35, "strong" => 0.75, _ => 0.55 }
}

// targetTimes(フレーム時刻一覧)の再取得間隔(秒)の既定。ナウキャスト自体が5分更新なので、
// これより短くしても新しい情報は無い。設定 [radar] refresh_sec で変えられる。
pub(crate) const RADAR_REFRESH_SECS: u64 = 300;
// 無操作が続いた時の状態保存(#69)の間隔。強制終了/クラッシュ対策なので長すぎず短すぎず。
pub(crate) const IDLE_SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

// 設定の再取得間隔(秒・f64)を RadarClock に渡す u64 へ。壊れた値なら既定値へ落として必ず動かす。
pub(crate) fn radar_refresh_secs(cfg: &config::Config) -> u64 {
    let s = cfg.radar_refresh_sec;
    if s.is_finite() && s >= 1.0 { s as u64 } else { RADAR_REFRESH_SECS }
}

// GPS位置(Mac本体のGキー経由/スマホの📍ボタン経由のどちらでもよい)を1件取り込むたびに呼ぶ。
// 曲がり角の残り距離が閾値を切っていれば読み上げる。ルート未確定/音声案内OFF/曲がり角取得前
// (turn_job待ち)は何もしない(=呼び出し側で毎回呼んでも無害)。
pub(crate) fn maybe_speak_turn(cfg: &config::Config, spec: &render::OverlaySpec, turn_points: &[route::TurnPoint], voice_guide: &mut Option<voice::VoiceGuide>, pos: (f64, f64)) {
    if !cfg.voice_guide_enabled || turn_points.is_empty() {
        return;
    }
    let Some(guide) = voice_guide else { return };
    if !guide.matches_len(turn_points) {
        return; // ルート更新直後の一時的なズレ。turn_job完了でvoice_guideが作り直されるまで待つ
    }
    let Some(pts) = spec.routes.last().map(|rt| &rt.pts) else { return };
    let Some(progress_m) = route::progress_along_route(pos, pts) else { return };
    if let Some(phrase) = guide.tick(turn_points, progress_m) {
        voice::speak(&phrase, cfg.voice_speak_local, &cfg.voice_name);
    }
}

// 位置/ルート(last.txt)と直接キーで変えたcfg項目をまとめて保存。終了時とアイドル時の両方から呼ぶ。
pub(crate) fn persist_full_state(cx: f64, cy: f64, z: u32, opts: &Args, wps: &[(f64, f64)], mode: &str, cfg: &mut config::Config, radar_on: bool, show_spots: bool) {
    let (lat, lon) = pixel_to_deg(cx, cy, z);
    save_state(lat, lon, z, &opts.style, wps, mode);
    cfg.braille = opts.braille; cfg.classify = opts.classify; cfg.edge = opts.edge; cfg.mono = opts.mono; cfg.style = opts.style.clone();
    cfg.radar_enabled = radar_on;
    cfg.show_spots = show_spots;
    let _ = config::save_config(cfg);
}

// ---- サブピクセル描画と再描画判定(docs/web-pan-smoothness-design.md §5.1/§5.2 対策A・B) ----

// 1出力ピクセルを何段に割るか(設計 §5.1 対策A・§5.2)。
// 対策A で切り出しが小数位置になったため、描画に効く粒度が「整数ピクセル」から
// 「1/SUBPIXEL_STEPS ピクセル」へ細かくなった。設計は 8 か 16 を想定と書いている。
// 8 を選んだのは、halfblock の横1出力ピクセル(=1文字セル・iPhone の xterm.js でおおむね
// 9 CSS px)を約1.1 CSS px 刻みで動かせて、指の動きの粒度としてはこれで十分細かい一方、
// 16 にすると同じ絵に見える差でも再構築が走る回数が倍になり、対策B(同じ絵なら送らない)で
// 削ったバイト数を食い潰すため。偶数なので、窓の左上(中心 - rw/2.0)も必ず同じ格子に載る。
pub(crate) const SUBPIXEL_STEPS: f64 = 8.0;

// 描画へ渡す中心座標をサブピクセル格子へ吸着させる。
//
// 描画に使う位置と map_sig の位置がずれていると、絵が変わったのにシグネチャが変わらない
// 取りこぼし(=地図が止まって見える)が起きる。両者を必ず同じ値から導くため、丸めは
// ここ1箇所に閉じてある(設計 §5.2)。論理座標 cx/cy は連続のまま保持し、丸めるのは
// 描画へ渡す直前だけにする。
//
// steps=1.0 なら整数ピクセルへの吸着になり、対策A を使わない描画モード(braille/edge)で
// そのまま使える。
pub(crate) fn snap_center_to_grid(rcx: f64, rcy: f64, steps: f64) -> (f64, f64) {
    let snap = |v: f64| if v.is_finite() { (v * steps).round() / steps } else { v };
    (snap(rcx), snap(rcy))
}

// サブピクセル切り出しを使うか(設計 §5.1 の注意・§11 のリスク)。
//
// braille / edge は輝度の閾値でドットの on/off を決める(render.rs)ので、バイリニアで作った
// 中間色が閾値をまたぐ瞬間にドットが入れ替わる。実質ディザになるため、階段は消える代わりに
// ちらつきが増える可能性がある。halfblock と実画像では素直に効く。
//
// 設計は「braille は実機で確認して、悪ければ braille だけ従来の整数切り出しに戻せるように
// しておく」としている。既定は全モードで有効にし、環境変数 TERMMAP_SUBPIXEL で上書きできる
// ようにした(0/false=全モードで従来の整数切り出し、1/true=全モードで有効)。
// 実機で braille がちらつくと分かったら、ここの既定を「braille/edge では false」へ変える。
pub(crate) fn use_subpixel_window(braille: bool, edge: bool, env: Option<&str>) -> bool {
    match env.map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) if v == "0" || v == "false" || v == "off" || v == "no" => false,
        Some(v) if v == "1" || v == "true" || v == "on" || v == "yes" => true,
        _ => {
            let _ = (braille, edge); // 既定は描画モードによらず有効(上のコメントの判断)
            true
        }
    }
}

// map_sig に混ぜる中心座標の値。実際に描画へ効く粒度へ丸める。
//
// 生の f64(to_bits)を混ぜると、1出力ピクセルの1/100しか動かないパンでもシグネチャが変わり、
// 絵が1ピクセルも変わらないのに全画面(halfblock 94x23 で 85.6KB)を再送してしまう
// (設計 §2.3 の実測)。ゆっくり指を動かしているときほどこの無駄の割合が上がる。
//
// 丸めの基準は中心そのものではなく「窓の左上」にしてある。切り出しは tiles.rs で
// left = rcx - rw/2.0 を基準に行われ、rw が奇数(左袖なし・halfblock で端末幅が奇数のとき等)
// だと rcx の丸めが同じでも left の丸めが変わる = 実際の絵が変わる。設計と依頼は rcx を
// 直接丸めると書いているが、それだとこの場合に再構築を取りこぼして地図が動かなくなる。
// rw/rh 自体は map_sig 側で別途ハッシュしているので、中心の代わりに左上を混ぜても情報は落ちない。
//
// steps は描画側の粒度と必ず揃える。対策A(サブピクセル切り出し)を使う描画モードでは
// SUBPIXEL_STEPS、従来の整数切り出しでは 1.0 を渡す(設計 §5.2 の「対策A を入れる場合」)。
pub(crate) fn map_center_sig_key(rcx: f64, rcy: f64, rw: u32, rh: u32, steps: f64) -> (i64, i64) {
    (
        ((rcx - rw as f64 / 2.0) * steps).floor() as i64,
        ((rcy - rh as f64 / 2.0) * steps).floor() as i64,
    )
}

// ---- 規制原因アイコン(#規制原因アイコン、docs/regulation-cause-icons-design.md) ----

// 規制ラインの中点(アイコンを置く座標)。空ならNone、1点のみならその点。
// regulation.rsはcrate::に依存しない方針のため、roadtrace側を使うここに置く。
pub(crate) fn closure_icon_position(line: &[(f64, f64)]) -> Option<(f64, f64)> {
    if line.is_empty() { return None; }
    if line.len() == 1 { return Some(line[0]); }
    let total = roadtrace::polyline_len(line);
    Some(roadtrace::point_at(line, total / 2.0))
}

// 表示中のClosedイベントのうち、まだcauseキャッシュに無い最初の1件のdetail_idを返す
// (無ければNone=今フレームは新規フェッチしない)。detail_id空文字は対象外。
// 同時に1件だけフェッチする(呼び出し側でcause_jobが空の時だけ呼ぶ)ためのレート制限。
pub(crate) fn next_closure_to_categorize<'a>(
    visible: &[&'a regulation::ClosureEvent],
    cached: &std::collections::HashMap<String, regulation::CauseCategory>,
) -> Option<&'a str> {
    visible.iter()
        .map(|e| e.detail_id.as_str())
        .find(|id| !id.is_empty() && !cached.contains_key(*id))
}

// 端末状態を RAII で復元する。パニック/早期return でも Drop で raw mode と代替スクリーンを必ず戻す。
pub(crate) struct TermGuard;
impl TermGuard {
    pub(crate) fn enter() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide, crossterm::event::EnableBracketedPaste)?;
        Ok(Self)
    }
}
impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste,
            crossterm::cursor::Show, crossterm::terminal::LeaveAlternateScreen);
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regulation::{ClosureEvent, RegulationKind};

    #[test]
    fn closure_icon_position_empty_is_none() {
        assert_eq!(closure_icon_position(&[]), None);
    }

    #[test]
    fn closure_icon_position_single_point_is_that_point() {
        assert_eq!(closure_icon_position(&[(35.0, 139.0)]), Some((35.0, 139.0)));
    }

    #[test]
    fn closure_icon_position_is_the_midpoint_of_a_straight_line() {
        // 経線に沿った直線なので、中点は緯度の単純平均に近い。
        let pos = closure_icon_position(&[(35.0, 139.0), (35.02, 139.0)]).unwrap();
        assert!((pos.0 - 35.01).abs() < 1e-3, "{pos:?}");
        assert!((pos.1 - 139.0).abs() < 1e-6, "{pos:?}");
    }

    fn ev(id: &str) -> ClosureEvent {
        ClosureEvent { line: vec![(35.0, 139.0), (35.01, 139.0)], kind: RegulationKind::Closed, detail_id: id.to_string(), active: true }
    }

    #[test]
    fn next_closure_to_categorize_returns_first_uncached() {
        let a = ev("a"); let b = ev("b");
        let visible = vec![&a, &b];
        let mut cached = std::collections::HashMap::new();
        cached.insert("a".to_string(), regulation::CauseCategory::Other);
        assert_eq!(next_closure_to_categorize(&visible, &cached), Some("b"));
    }

    #[test]
    fn next_closure_to_categorize_none_when_all_cached() {
        let a = ev("a");
        let visible = vec![&a];
        let mut cached = std::collections::HashMap::new();
        cached.insert("a".to_string(), regulation::CauseCategory::Construction);
        assert_eq!(next_closure_to_categorize(&visible, &cached), None);
    }

    #[test]
    fn next_closure_to_categorize_skips_empty_detail_id() {
        let a = ev(""); let b = ev("b");
        let visible = vec![&a, &b];
        let cached = std::collections::HashMap::new();
        assert_eq!(next_closure_to_categorize(&visible, &cached), Some("b"));
    }

    #[test]
    fn next_closure_to_categorize_prefers_visible_order() {
        let a = ev("a"); let b = ev("b");
        let visible = vec![&a, &b];
        let cached = std::collections::HashMap::new();
        assert_eq!(next_closure_to_categorize(&visible, &cached), Some("a"));
    }

    // ---- map_center_sig_key(再描画判定の中心座標項・設計 §5.2 対策B) ----

    // map_sig と同じ形でキーをハッシュしたもの。値が変わらない = need_build が立たない。
    // 従来の整数切り出し(対策A なし)相当の粒度。
    fn center_sig(rcx: f64, rcy: f64, rw: u32, rh: u32) -> u64 {
        center_sig_steps(rcx, rcy, rw, rh, 1.0)
    }
    fn center_sig_steps(rcx: f64, rcy: f64, rw: u32, rh: u32, steps: f64) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        map_center_sig_key(rcx, rcy, rw, rh, steps).hash(&mut h);
        h.finish()
    }
    // ui.rs と同じ手順(格子へ吸着 → キー化)。描画とシグネチャで同じ値を使う経路の検証。
    fn snapped_sig(rcx: f64, rcy: f64, rw: u32, rh: u32, steps: f64) -> u64 {
        let (sx, sy) = snap_center_to_grid(rcx, rcy, steps);
        center_sig_steps(sx, sy, rw, rh, steps)
    }

    // 同じ丸め値になる2つの中心座標では need_build が立たない(全画面の無駄な再送が消える)。
    #[test]
    fn map_center_sig_key_ignores_subpixel_moves() {
        let (rw, rh) = (94u32, 44u32);
        // 1出力ピクセルの1/100しか動かないパン(設計 §2.3 の再現条件)。
        assert_eq!(center_sig(1000.0, 500.0, rw, rh), center_sig(1000.0001, 500.0001, rw, rh));
        // 同じ整数ピクセル内であれば、小数部がどれだけ違っても同じ。
        assert_eq!(center_sig(1000.2, 500.9, rw, rh), center_sig(1000.8, 500.1, rw, rh));
    }

    // 丸め値が1段変われば need_build が立つ(動くべきときに動かなくなる退行を防ぐ)。
    #[test]
    fn map_center_sig_key_changes_when_the_drawn_pixel_changes() {
        let (rw, rh) = (94u32, 44u32);
        assert_ne!(center_sig(1000.0, 500.0, rw, rh), center_sig(1001.0, 500.0, rw, rh));
        assert_ne!(center_sig(1000.0, 500.0, rw, rh), center_sig(1000.0, 501.0, rw, rh));
        // 整数の境界をまたぐケース(0.9 → 1.1)。
        assert_ne!(center_sig(1000.9, 500.0, rw, rh), center_sig(1001.1, 500.0, rw, rh));
    }

    // 出力幅が奇数のときは left = rcx - rw/2.0 に .5 が乗る。中心を floor する実装だと
    // この2つが同じキーになり、実際には1px違う絵なのに再構築されず地図が止まって見える。
    #[test]
    fn map_center_sig_key_follows_the_window_origin_for_odd_widths() {
        let (rw, rh) = (93u32, 44u32); // 左袖なし・halfblock で端末幅が奇数のとき
        assert_eq!(map_center_sig_key(1000.2, 500.0, rw, rh, 1.0).0, 953); // 1000.2 - 46.5 = 953.7
        assert_eq!(map_center_sig_key(1000.8, 500.0, rw, rh, 1.0).0, 954); // 1000.8 - 46.5 = 954.3
        assert_ne!(center_sig(1000.2, 500.0, rw, rh), center_sig(1000.8, 500.0, rw, rh));
    }

    // 出力寸法が変われば当然キーも変わる(map_sig 側でも rw/rh を混ぜているが二重に効かせる)。
    #[test]
    fn map_center_sig_key_depends_on_the_output_size() {
        assert_ne!(map_center_sig_key(1000.0, 500.0, 94, 44, 1.0), map_center_sig_key(1000.0, 500.0, 96, 44, 1.0));
        assert_ne!(map_center_sig_key(1000.0, 500.0, 94, 44, 1.0), map_center_sig_key(1000.0, 500.0, 94, 48, 1.0));
    }

    #[test]
    fn map_center_sig_key_handles_negative_origin_and_broken_values() {
        // 世界の西端付近では窓の左上が負になる。floor は負側でも下方向へ丸まる。
        assert_eq!(map_center_sig_key(10.0, 500.0, 94, 44, 1.0).0, -37);
        assert_eq!(map_center_sig_key(-0.5, 500.0, 94, 44, 1.0).0, -48);
        // 壊れた値が来ても panic しない(as キャストは飽和し、NaN は 0 になる)。
        let (kx, ky) = map_center_sig_key(f64::NAN, f64::INFINITY, 94, 44, 1.0);
        assert_eq!(kx, 0);
        assert_eq!(ky, i64::MAX);
    }

    // ---- サブピクセル刻みの丸め(設計 §5.2 の「対策A を入れる場合」) ----

    // 同じ刻みに入る2つの中心では need_build が立たない。対策A で粒度は細かくなるが、
    // 「同じ絵なら送らない」は 1/SUBPIXEL_STEPS 単位で引き続き効く。
    #[test]
    fn subpixel_sig_ignores_moves_inside_one_step() {
        let (rw, rh) = (94u32, 44u32);
        let step = 1.0 / SUBPIXEL_STEPS; // 0.125
        // 1出力ピクセルの1/100しか動かないパン(設計 §2.3 の再現条件)は刻みの中に収まる。
        assert_eq!(snapped_sig(1000.0, 500.0, rw, rh, SUBPIXEL_STEPS),
                   snapped_sig(1000.0001, 500.0001, rw, rh, SUBPIXEL_STEPS));
        // 刻みの 1/10 ずつずらしても同じキーのまま。
        assert_eq!(snapped_sig(1000.0, 500.0, rw, rh, SUBPIXEL_STEPS),
                   snapped_sig(1000.0 + step * 0.1, 500.0 + step * 0.1, rw, rh, SUBPIXEL_STEPS));
    }

    // 刻みが1段変われば need_build が立つ(サブピクセルの動きが描画へ反映される)。
    #[test]
    fn subpixel_sig_changes_when_the_step_changes() {
        let (rw, rh) = (94u32, 44u32);
        let step = 1.0 / SUBPIXEL_STEPS;
        assert_ne!(snapped_sig(1000.0, 500.0, rw, rh, SUBPIXEL_STEPS),
                   snapped_sig(1000.0 + step, 500.0, rw, rh, SUBPIXEL_STEPS));
        assert_ne!(snapped_sig(1000.0, 500.0, rw, rh, SUBPIXEL_STEPS),
                   snapped_sig(1000.0, 500.0 + step, rw, rh, SUBPIXEL_STEPS));
    }

    // 整数切り出しでは捨てられていた 1/8 ピクセルの動きが、対策A では別フレームになる。
    // (これが「ゆっくり動かすと 9px 飛ぶ」が消える理由。設計 §3.1)
    #[test]
    fn subpixel_sig_is_finer_than_the_integer_one() {
        let (rw, rh) = (94u32, 44u32);
        let step = 1.0 / SUBPIXEL_STEPS;
        assert_eq!(snapped_sig(1000.0, 500.0, rw, rh, 1.0),
                   snapped_sig(1000.0 + step, 500.0, rw, rh, 1.0)); // 従来は同じ絵
        assert_ne!(snapped_sig(1000.0, 500.0, rw, rh, SUBPIXEL_STEPS),
                   snapped_sig(1000.0 + step, 500.0, rw, rh, SUBPIXEL_STEPS)); // 対策A では変わる
    }

    // 吸着後の中心から作ったキーは、境界上でも揺れない(1/8 は2進で正確に表せるので、
    // floor が期待どおりの整数になる)。描画とシグネチャがずれない前提そのものの検証。
    #[test]
    fn snap_center_to_grid_lands_exactly_on_the_grid() {
        let (sx, sy) = snap_center_to_grid(1000.06, 500.19, SUBPIXEL_STEPS);
        assert_eq!(sx, 1000.0 + 0.0 / 8.0); // 0.06 → 0/8(最寄りは 0.0)
        assert_eq!(sy, 500.0 + 2.0 / 8.0);  // 0.19 → 2/8 = 0.25
        // 奇数幅でも左上は格子に載る(rw/2.0 の .5 は 1/8 の倍数)。
        assert_eq!(map_center_sig_key(sx, sy, 93, 44, SUBPIXEL_STEPS).0,
                   ((1000.0 - 46.5) * 8.0) as i64);
    }

    #[test]
    fn snap_center_to_grid_passes_broken_values_through() {
        let (sx, sy) = snap_center_to_grid(f64::NAN, f64::INFINITY, SUBPIXEL_STEPS);
        assert!(sx.is_nan());
        assert!(sy.is_infinite());
    }

    // ---- use_subpixel_window(braille/edge を従来方式へ戻せる逃げ道・設計 §5.1 の注意) ----

    #[test]
    fn use_subpixel_window_defaults_to_on_for_every_mode() {
        assert!(use_subpixel_window(false, false, None)); // halfblock
        assert!(use_subpixel_window(true, false, None));  // braille
        assert!(use_subpixel_window(false, true, None));  // edge
    }

    #[test]
    fn use_subpixel_window_can_be_turned_off_by_env() {
        for v in ["0", "false", "off", "no", " OFF "] {
            assert!(!use_subpixel_window(false, false, Some(v)), "{v}");
            assert!(!use_subpixel_window(true, false, Some(v)), "{v}");
        }
    }

    #[test]
    fn use_subpixel_window_can_be_forced_on_by_env() {
        for v in ["1", "true", "on", "yes", " ON "] {
            assert!(use_subpixel_window(true, true, Some(v)), "{v}");
        }
    }

    // 綴り間違い等の未知の値は既定へ落とす(起動しなくなる/黙って挙動が変わるのを避ける)。
    #[test]
    fn use_subpixel_window_ignores_unknown_env_values() {
        assert!(use_subpixel_window(false, false, Some("maybe")));
        assert!(use_subpixel_window(false, false, Some("")));
    }
}
