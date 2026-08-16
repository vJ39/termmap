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
