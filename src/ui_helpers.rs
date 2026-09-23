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
// 無操作が続いた時の状態保存の間隔。強制終了/クラッシュ対策なので長すぎず短すぎず。
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

// 気象警報(ルートベース)。ルートのポリラインを2km間隔でサンプリングし、通過する
// class10s領域(geoarea.rs)の気象台コードを重複無しで列挙する。2kmはclass10s領域が概ね
// 数十km規模のため、粒度を上げても得られる精度は限定的という判断(粗くても良い)。
pub(crate) fn route_warning_office_codes(pts: &[(f64, f64)]) -> Vec<String> {
    let sampled = roadtrace::sample_every(pts, 2000.0);
    let mut codes: Vec<String> = Vec::new();
    for &p in &sampled {
        if let Some(region) = geoarea::region_at(p) {
            if !codes.iter().any(|c| c == &region.office_code) {
                codes.push(region.office_code.clone());
            }
        }
    }
    codes
}

// ルートのポリラインを、警報の有無/severityで塗り分けたRoute群へ変換する。警報が無い
// 区間はRouteを作らない(既存のroutes/traffic_segments等の下地色がそのまま見える)。
// 連続する点が同じ判定(同じseverityの色、または「警報なし」)である間は1本のRouteへまとめる。
pub(crate) fn build_warning_segments(pts: &[(f64, f64)], warnings: &[warning::ActiveWarning]) -> Vec<render::Route> {
    let color_at = |p: (f64, f64)| -> Option<[u8; 3]> {
        let region = geoarea::region_at(p)?;
        warnings
            .iter()
            .filter(|w| w.area_code == region.code)
            .map(|w| w.severity)
            .max_by_key(severity_rank)
            .map(|s| s.color())
    };
    if pts.len() < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur: Vec<(f64, f64)> = vec![pts[0]];
    let mut cur_color = color_at(pts[0]);
    for &p in &pts[1..] {
        let color = color_at(p);
        if color == cur_color {
            cur.push(p);
        } else {
            if let Some(c) = cur_color {
                if cur.len() >= 2 {
                    out.push(render::Route { pts: cur, color: c, thickness: 3 });
                }
            }
            cur = vec![p];
            cur_color = color;
        }
    }
    if let Some(c) = cur_color {
        if cur.len() >= 2 {
            out.push(render::Route { pts: cur, color: c, thickness: 3 });
        }
    }
    out
}

// severityの深刻度を大小比較できる数値へ(max_by_keyで最も深刻なものを選ぶため)。
// 特別警報 > 警報 > 注意報 > その他、の順。
fn severity_rank(s: &warning::Severity) -> u8 {
    match s {
        warning::Severity::Special => 3,
        warning::Severity::Warning => 2,
        warning::Severity::Advisory => 1,
        warning::Severity::Other => 0,
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

// 1出力ピクセルを何段に割るか(設計 §5.1 対策A・§5.2。設計の想定は 8 か 16)。8 なら halfblock の
// 横1出力ピクセル(iPhone の xterm.js でおおむね 9 CSS px)を約1.1 CSS px 刻みで動かせて十分細かい。
// 16 だと同じ絵に見える差でも再構築が倍走り、対策B(同じ絵なら送らない)で削ったバイト数を食い潰す。
// 偶数なので、窓の左上(中心 - rw/2.0)も必ず同じ格子に載る。
pub(crate) const SUBPIXEL_STEPS: f64 = 8.0;

// 描画へ渡す中心座標をサブピクセル格子へ吸着させる。描画の位置と map_sig の位置がずれると、
// 絵が変わったのにシグネチャが変わらず地図が止まって見えるので、丸めはここ1箇所に閉じてある
// (設計 §5.2)。論理座標 cx/cy は連続のまま保持し、丸めるのは描画へ渡す直前だけにする。
// steps=1.0 なら整数ピクセルへの吸着になり、対策A を使わない描画モード(braille/edge)でそのまま使える。
pub(crate) fn snap_center_to_grid(rcx: f64, rcy: f64, steps: f64) -> (f64, f64) {
    let snap = |v: f64| if v.is_finite() { (v * steps).round() / steps } else { v };
    (snap(rcx), snap(rcy))
}

// サブピクセル切り出しを使うか(設計 §5.1 の注意・§11 のリスク)。braille / edge は輝度の閾値で
// ドットの on/off を決める(render.rs)ので、バイリニアの中間色が閾値をまたぐとドットが入れ替わり、
// 階段は消える代わりにちらつきが増える可能性がある。既定は全モードで有効。環境変数 TERMMAP_SUBPIXEL で
// 上書きでき、halfblock / no-braille なら braille/edge だけ従来の整数切り出しにする。
pub(crate) fn use_subpixel_window(braille: bool, edge: bool, env: Option<&str>) -> bool {
    let dots = braille || edge; // 輝度の閾値でドットの on/off を決めるモード
    match env.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("0") | Some("false") | Some("off") | Some("no") => false,
        Some("1") | Some("true") | Some("on") | Some("yes") => true,
        Some("halfblock") | Some("no-braille") | Some("nobraille") => !dots,
        _ => !(SUBPIXEL_EXCEPT_BRAILLE && dots),
    }
}

// 既定で braille/edge を対策A から外すか。実機で braille のちらつきが許容できないと分かったら、
// ここを true にするだけで既定が「braille/edge は従来の整数切り出し」になる(halfblock と実画像は
// 対策A のまま)。PTY 実測の数字では有効のままでよさそうだが、ちらつきの見え方は実機でしか判断
// できないので false のままにしてある。
pub(crate) const SUBPIXEL_EXCEPT_BRAILLE: bool = false;

// map_sig に混ぜる中心座標の値。生の f64 では絵が変わらない微小なパンでも全画面を再送するので
// (設計 §2.3)、描画に効く粒度へ丸める。基準は中心でなく窓の左上(tiles.rs の left = rcx - rw/2.0)。
// rw が奇数だと rcx の丸めが同じでも絵が変わり、設計どおり rcx を丸めると再構築を取りこぼす(rw/rh は
// 別途ハッシュ済み)。steps は描画側と必ず揃える(対策A は SUBPIXEL_STEPS、従来は 1.0。設計 §5.2)。
pub(crate) fn map_center_sig_key(rcx: f64, rcy: f64, rw: u32, rh: u32, steps: f64) -> (i64, i64) {
    (
        ((rcx - rw as f64 / 2.0) * steps).floor() as i64,
        ((rcy - rh as f64 / 2.0) * steps).floor() as i64,
    )
}

// ---- 規制原因アイコン(docs/regulation-cause-icons-design.md) ----

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
// マウス報告は設定を読んだ後に enable_mouse() で有効化し、有効化したときだけ Drop で戻す。
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";
pub(crate) struct TermGuard { mouse: bool }
impl TermGuard {
    pub(crate) fn enter() -> std::io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen,
            crossterm::cursor::Hide, crossterm::event::EnableBracketedPaste)?;
        Ok(Self { mouse: false })
    }
    // crossterm の EnableMouseCapture は全移動報告(?1003)まで有効にし、ボタンを押していない
    // カーソル移動のたびにイベントが来るので使わない。押下/ドラッグ(?1000/?1002)と SGR 形式(?1006)だけ。
    pub(crate) fn enable_mouse(&mut self) {
        use std::io::Write;
        let mut o = std::io::stdout();
        if !self.mouse && o.write_all(MOUSE_ON.as_bytes()).and_then(|_| o.flush()).is_ok() {
            self.mouse = true;
        }
    }
}
impl Drop for TermGuard {
    fn drop(&mut self) {
        if self.mouse {
            use std::io::Write;
            let _ = std::io::stdout().write_all(MOUSE_OFF.as_bytes());
        }
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

    // 新宿駅付近(東京地方=130010・気象台コード130000)を通る短い直線ルート。geoarea.rsの
    // 実データ(assets/jma-class10s.json)を使うテストなので、座標は実在の地点にしている。
    fn tokyo_route() -> Vec<(f64, f64)> {
        vec![(35.6896, 139.7006), (35.69, 139.701), (35.6905, 139.7015)]
    }

    #[test]
    fn route_warning_office_codes_resolves_tokyo() {
        let codes = route_warning_office_codes(&tokyo_route());
        assert_eq!(codes, vec!["130000".to_string()]);
    }

    #[test]
    fn route_warning_office_codes_empty_route_is_empty() {
        assert!(route_warning_office_codes(&[]).is_empty());
    }

    #[test]
    fn build_warning_segments_colors_the_route_when_area_has_an_active_warning() {
        let pts = tokyo_route();
        let warnings = vec![warning::ActiveWarning {
            area_code: "130010".to_string(),
            name: "濃霧注意報".to_string(),
            severity: warning::Severity::Advisory,
        }];
        let segs = build_warning_segments(&pts, &warnings);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].color, warning::Severity::Advisory.color());
        assert_eq!(segs[0].pts, pts);
    }

    #[test]
    fn build_warning_segments_empty_when_no_warning_matches_the_area() {
        let pts = tokyo_route();
        let warnings = vec![warning::ActiveWarning {
            area_code: "999999".to_string(), // どこにも該当しないコード
            name: "大雨警報".to_string(),
            severity: warning::Severity::Warning,
        }];
        assert!(build_warning_segments(&pts, &warnings).is_empty());
    }

    #[test]
    fn build_warning_segments_empty_when_no_warnings_at_all() {
        assert!(build_warning_segments(&tokyo_route(), &[]).is_empty());
    }

    #[test]
    fn build_warning_segments_picks_the_most_severe_when_multiple_warnings_overlap() {
        let pts = tokyo_route();
        let warnings = vec![
            warning::ActiveWarning { area_code: "130010".to_string(), name: "大雨注意報".to_string(), severity: warning::Severity::Advisory },
            warning::ActiveWarning { area_code: "130010".to_string(), name: "大雨特別警報".to_string(), severity: warning::Severity::Special },
        ];
        let segs = build_warning_segments(&pts, &warnings);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].color, warning::Severity::Special.color());
    }

    #[test]
    fn build_warning_segments_handles_short_routes_without_panicking() {
        assert!(build_warning_segments(&[], &[]).is_empty());
        assert!(build_warning_segments(&[(35.0, 139.0)], &[]).is_empty());
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

    // 設計 §5.1/§11 が求める「braille だけ従来の整数切り出しへ戻す」逃げ道。
    // 全モード一括の 0/1 とは別に、モード単位で切り替えられること。
    #[test]
    fn use_subpixel_window_can_exclude_only_the_dot_modes() {
        for v in ["halfblock", "no-braille", "nobraille", " HalfBlock "] {
            assert!(use_subpixel_window(false, false, Some(v)), "halfblock は対策A のまま: {v}");
            assert!(!use_subpixel_window(true, false, Some(v)), "braille は整数切り出しへ: {v}");
            assert!(!use_subpixel_window(false, true, Some(v)), "edge は整数切り出しへ: {v}");
        }
    }

    // 既定の分岐は SUBPIXEL_EXCEPT_BRAILLE 1箇所で決まる(実機で braille がちらついたら
    // ここを true にするだけで既定が変わる)。定数の現在値と挙動が食い違わないことを見る。
    #[test]
    fn use_subpixel_window_default_follows_the_except_braille_constant() {
        assert!(use_subpixel_window(false, false, None), "halfblock は常に対策A");
        assert_eq!(use_subpixel_window(true, false, None), !SUBPIXEL_EXCEPT_BRAILLE);
        assert_eq!(use_subpixel_window(false, true, None), !SUBPIXEL_EXCEPT_BRAILLE);
    }

    // 綴り間違い等の未知の値は既定へ落とす(起動しなくなる/黙って挙動が変わるのを避ける)。
    #[test]
    fn use_subpixel_window_ignores_unknown_env_values() {
        assert!(use_subpixel_window(false, false, Some("maybe")));
        assert!(use_subpixel_window(false, false, Some("")));
    }

    #[test]
    fn opacity_settings_map_to_their_values_and_unknown_is_standard() {
        let mut cfg = config::Config::default();
        for (name, want) in [("light", 0.35), ("mid", 0.55), ("strong", 0.75), ("壊れた値", 0.55)] {
            cfg.radar_opacity = name.to_string();
            cfg.population_opacity = name.to_string();
            assert_eq!(radar_opacity_value(&cfg), want, "雨雲 {name}");
            assert_eq!(population_opacity_value(&cfg), want, "人口 {name}");
        }
    }

    #[test]
    fn radar_refresh_secs_falls_back_to_the_default_for_broken_values() {
        let with = |s: f64| radar_refresh_secs(&config::Config { radar_refresh_sec: s, ..config::Config::default() });
        assert_eq!(with(600.0), 600);
        assert_eq!(with(1.0), 1);
        assert_eq!(with(90.9), 90, "小数は切り捨て");
        for bad in [0.0, 0.5, -30.0, f64::NAN, f64::INFINITY] {
            assert_eq!(with(bad), RADAR_REFRESH_SECS, "{bad}");
        }
    }

    #[test]
    fn qr_view_draws_text_unless_the_image_style_is_supported() {
        let code = qrcode::QrCode::new(b"https://www.google.com/maps/dir/35.0,139.0/35.1,139.1").unwrap();
        match build_qr_view(&code, "dense") {
            QrView::Text(t) => assert!(t.lines().count() >= code.width() / 2, "上下2モジュールを1行に詰める"),
            QrView::Image(_) => panic!("dense なら文字描画"),
        }
        match build_qr_view(&code, "image") {
            QrView::Image(img) => {
                assert!(image_capable());
                assert_eq!(img.width(), (code.width() as u32 + 8) * 8, "静穏領域4モジュール×2・1モジュール8px");
            }
            QrView::Text(_) => assert!(!image_capable(), "非対応端末では文字描画へ戻す"),
        }
    }

    // 東京地方(tokyo_route)とは別の一次細分区域(神奈川県東部)を通る2点。
    fn yokohama() -> Vec<(f64, f64)> {
        vec![(35.4437, 139.6380), (35.4450, 139.6400)]
    }

    fn alert(area: &str, severity: warning::Severity) -> warning::ActiveWarning {
        warning::ActiveWarning { area_code: area.to_string(), name: "テスト".to_string(), severity }
    }

    fn area_of(p: (f64, f64)) -> String {
        geoarea::region_at(p).expect("実データの区域内の点").code.clone()
    }

    // 区域をまたぐルートは、区域ごとの警報の色で区間を分ける。警報の無い区域は上塗りしない。
    #[test]
    fn build_warning_segments_splits_the_route_where_the_area_changes() {
        let (tokyo, kanagawa) = (area_of(tokyo_route()[0]), area_of(yokohama()[0]));
        assert_ne!(tokyo, kanagawa, "前提: 別の区域の点");
        let pts: Vec<(f64, f64)> = tokyo_route().into_iter().chain(yokohama()).collect();
        let both = [alert(&tokyo, warning::Severity::Warning), alert(&kanagawa, warning::Severity::Advisory)];
        let segs = build_warning_segments(&pts, &both);
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[0].color, segs[0].pts.clone()), (warning::Severity::Warning.color(), tokyo_route()));
        assert_eq!((segs[1].color, segs[1].pts.clone()), (warning::Severity::Advisory.color(), yokohama()));
        let segs = build_warning_segments(&pts, &[alert(&tokyo, warning::Severity::Warning)]);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].pts, tokyo_route());
    }

    // 警報のある区域を1点しか通らない区間は線にならないので作らない。
    #[test]
    fn build_warning_segments_drops_a_run_of_a_single_point() {
        let tokyo = area_of(tokyo_route()[0]);
        let pts = vec![tokyo_route()[0], yokohama()[0], yokohama()[1]];
        assert!(build_warning_segments(&pts, &[alert(&tokyo, warning::Severity::Special)]).is_empty());
    }

    #[test]
    fn build_warning_segments_prefers_an_advisory_over_other_notices() {
        let tokyo = area_of(tokyo_route()[0]);
        let ws = [alert(&tokyo, warning::Severity::Other), alert(&tokyo, warning::Severity::Advisory)];
        assert_eq!(build_warning_segments(&tokyo_route(), &ws)[0].color, warning::Severity::Advisory.color());
    }

    // 終了時・無操作時の保存。位置とルートは last.txt、直接キーで変えた表示設定は config.toml へ。
    // HOME を一時ディレクトリにした子プロセスで実行する(実HOMEには触れない)。
    #[test]
    fn persist_full_state_saves_the_position_and_the_display_settings() {
        let test = "persist_full_state_saves_the_position_and_the_display_settings";
        if !crate::fsutil::testing::reexec_in_temp_home(module_path!(), test, &[("TERMMAP_GOOGLE_API_KEY", "")]) { return; }
        let mut opts = crate::uistate::testing::test_args();
        opts.braille = true;
        opts.style = "dark".to_string();
        let mut cfg = crate::uistate::testing::test_cfg();
        let (cx, cy) = deg_to_pixel(35.5, 139.5, 12);
        let wps = [(35.0, 139.0), (35.1, 139.1)];
        persist_full_state(cx, cy, 12, &opts, &wps, "highway", &mut cfg, true, false);
        assert!(cfg.braille && cfg.style == "dark" && cfg.radar_enabled && !cfg.show_spots, "cfg へ反映する");
        let (lat, lon, z, style) = crate::load_state().expect("last.txt");
        assert!((lat - 35.5).abs() < 1e-9 && (lon - 139.5).abs() < 1e-9, "{lat},{lon}");
        assert_eq!((z, style.as_str()), (12, "dark"));
        assert_eq!(crate::load_route(), Some((wps.to_vec(), "highway".to_string())));
        assert_eq!(config::load_config(), cfg, "保存した設定がそのまま読み戻せる");
    }
}
