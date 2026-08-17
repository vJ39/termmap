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
}
