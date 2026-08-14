// 気象庁ナウキャスト(降水)のフレーム時刻管理。タイル画像の取得そのものは tiles.rs が行う。
// gpslive.rs と同じ方針で std + ureq + serde_json のみに依存し、crate:: を参照しない
// (このモジュール単体でコンパイル/テストできる)。
//
// 非公式エンドポイント(開発者向けAPIとして文書化されていない)を使うため、URL は
// このファイルの定数と tiles.rs の TileSource::url() の2箇所だけに閉じる。
// 出典表示「出典: 気象庁ナウキャスト」は呼び出し側(ステータス行・ヘルプ・README)の責務。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

// 実況(過去〜現在)の時刻一覧。basetime == validtime。5分刻みで数時間分(実測37件)。
pub const TARGET_TIMES_N1_URL: &str =
    "https://www.jma.go.jp/bosai/jmatile/data/nowc/targetTimes_N1.json";
// 予報(未来)の時刻一覧。basetime は最新の実況時刻で固定、validtime が5分刻みに進む(実測12件=+60分)。
pub const TARGET_TIMES_N2_URL: &str =
    "https://www.jma.go.jp/bosai/jmatile/data/nowc/targetTimes_N2.json";

// 既存のタイル取得(tiles.rs)と同じ User-Agent を使う。
const USER_AGENT: &str = "termmap/0.1 (personal experiment)";
const HTTP_TIMEOUT_SECS: u64 = 20;

// targetTimes の再取得間隔の下限。ナウキャスト自体が5分更新なので、これより短くしても
// 新しい情報は無い。config で 0 等の異常値を書かれても公共サービスを叩き続けないための歯止め。
const MIN_REFRESH_SECS: u64 = 60;

// ナウキャストの提供範囲(日本)のラフな外接矩形。厳密な境界は不要で、
// 「この矩形に全くかからないなら1枚もリクエストしない」という無駄打ち防止のためだけに使う。
const JP_LAT_MIN: f64 = 20.0;
const JP_LAT_MAX: f64 = 50.0;
const JP_LON_MIN: f64 = 120.0;
const JP_LON_MAX: f64 = 150.0;

// フレームの種別。実況(過去〜現在)と予報(未来)。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameKind {
    Observed,
    Forecast,
}

// 1コマ分の時刻。文字列は JMA が返す "YYYYMMDDHHMMSS"(14桁・区切りなし・UTC)を
// そのまま保持する。タイルURLの構築にこの文字列をそのまま使うので、日時型へ変換して往復させない。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub basetime: String,
    pub validtime: String,
    pub kind: FrameKind,
}

// フレーム一覧と「現在」の位置。ui.rs が保持する表示状態の入れ物。
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Timeline {
    pub frames: Vec<Frame>,
    // 実況の最後尾 = 「現在」。追従判定と表示ラベルの相対時刻の基準に使う。
    pub now_idx: usize,
}

impl Timeline {
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn get(&self, idx: usize) -> Option<&Frame> {
        self.frames.get(idx)
    }

    // 新しい frames を受け取ったときの再アンカー。
    // targetTimes は5分ごとに更新され basetime が動くため、index を素朴に保持していると
    // 表示時刻が勝手にずれる/消えたフレームを指す。同一性の基準は index ではなく validtime 文字列。
    //
    // 戻り値は (新しい index, 新しい follow, 利用者に伝える調整メッセージ)。
    //   follow == true  … 常に最新の実況(now_idx)へ追従する
    //   follow == false … prev_validtime と同じ時刻を新しい frames から探す。
    //                     消えていれば最も近い時刻へクランプし、その旨のメッセージを返す。
    pub fn reanchor(
        &self,
        prev_validtime: Option<&str>,
        prev_follow: bool,
    ) -> (usize, bool, Option<String>) {
        // 空リストでもパニックしない。モード(follow)だけ保って何も言わない。
        if self.frames.is_empty() {
            return (0, prev_follow, None);
        }
        // 追従モードは無条件で「今」へ。
        if prev_follow {
            return (self.clamp_idx(self.now_idx), true, None);
        }
        // スクラブ済みだが直前の表示時刻が分からない場合は、安全側に倒して「今」へ戻す。
        let Some(prev) = prev_validtime else {
            return (self.clamp_idx(self.now_idx), true, None);
        };
        // 同じ validtime が残っていればそこへ。follow はユーザーの意思(false)を尊重して変えない
        // (追従へ戻すのは `>` で now_idx ちょうどに送り返したときだけ、という操作系にする)。
        if let Some(i) = self.frames.iter().position(|f| f.validtime == prev) {
            return (i, false, None);
        }
        // 消えていた場合は最も近い時刻へクランプする。
        let i = self.nearest_idx(prev);
        (i, false, Some("表示時刻を調整しました".to_string()))
    }

    fn clamp_idx(&self, idx: usize) -> usize {
        idx.min(self.frames.len().saturating_sub(1))
    }

    // target に時刻が最も近いフレームの index。frames が空でないことは呼び出し側で保証する。
    fn nearest_idx(&self, target: &str) -> usize {
        if let Some(t) = epoch_minutes(target) {
            let mut best = 0usize;
            let mut best_d = i64::MAX;
            for (i, f) in self.frames.iter().enumerate() {
                // 時刻としてパースできないフレームは距離を測れないので候補から外す。
                let Some(v) = epoch_minutes(&f.validtime) else { continue };
                let d = (v - t).abs();
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            return best;
        }
        // 時刻としてパースできない文字列が来た場合の保険。frames は validtime 昇順なので、
        // 文字列順での挿入位置(=最初の「target 以上」の要素)へ寄せる。
        let pos = self.frames.partition_point(|f| f.validtime.as_str() < target);
        self.clamp_idx(pos)
    }
}

// ---- 取得 ----

// targetTimes_N1.json / N2.json を取得してマージした Timeline を返す。
// どちらか片方でも取れれば成功扱いにする(実況だけでも表示価値があるため)。
// 両方失敗、あるいは両方取れても1件もパースできなかった場合は Err を返し、
// 呼び出し側はステータスに「時刻取得できず」を出して次の周期で再試行する(地図は落とさない)。
pub fn fetch_timeline() -> Result<Timeline, String> {
    let obs_body = fetch_body(TARGET_TIMES_N1_URL);
    let fcst_body = fetch_body(TARGET_TIMES_N2_URL);

    if let (Err(e1), Err(e2)) = (&obs_body, &fcst_body) {
        return Err(format!("targetTimes 取得失敗: 実況={e1} / 予報={e2}"));
    }

    let observed = obs_body
        .map(|b| parse_target_times(&b, FrameKind::Observed))
        .unwrap_or_default();
    let forecast = fcst_body
        .map(|b| parse_target_times(&b, FrameKind::Forecast))
        .unwrap_or_default();

    let tl = merge_timeline(observed, forecast);
    if tl.is_empty() {
        // HTTP は成功したが中身が想定と違う(JSON形式変更など)。取得失敗と同じ扱いにする。
        return Err("targetTimes にフレームが1件も含まれていません".to_string());
    }
    Ok(tl)
}

fn fetch_body(url: &str) -> Result<String, String> {
    ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

// JSON本文 → Vec<Frame>。ネットワークに触れない純関数。
//
// 想定する本文(実測。ISO8601風のT区切り/Z終端ではなく14桁の数字列):
//   [ {"basetime":"20260814125500","validtime":"20260814125500","elements":[...]}, ... ]
//
// 方針:
//  - パース不能・配列でない・フィールド欠如は「その要素を捨てる」だけでパニックしない。
//    JSON形式が変わっても空 Vec が返るだけで、地図表示には一切影響しない。
//  - basetime/validtime は URL のパス要素にそのまま埋め込むため、ASCII数字のみを受け付ける。
//    想定外の応答に "../" 等が混ざってもURLを組み替えられないようにするための入力検証。
//    桁数は固定しない(将来 JMA が桁を変えても、時刻表示が諦められるだけで取得は動く)。
//  - kind は kind_hint を既定にしつつ、validtime > basetime のものは無条件に予報とみなす
//    (どちらのファイルから来たかより、時刻の前後関係の方が確かな情報のため)。
pub fn parse_target_times(body: &str, kind_hint: FrameKind) -> Vec<Frame> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let (Some(basetime), Some(validtime)) = (
            item.get("basetime").and_then(|x| x.as_str()),
            item.get("validtime").and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        if !is_time_token(basetime) || !is_time_token(validtime) {
            continue;
        }
        let kind = if validtime > basetime { FrameKind::Forecast } else { kind_hint };
        out.push(Frame {
            basetime: basetime.to_string(),
            validtime: validtime.to_string(),
            kind,
        });
    }
    out
}

// URL のパス要素として安全に使える時刻トークンか(空でなく、ASCII数字のみ)。
fn is_time_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

// N1(実況) と N2(予報) を validtime 昇順にマージし、同一 validtime は実況を優先して重複排除する。
// now_idx は実況の最後尾(=「現在」)。実況が1件も無い場合は先頭(0)にフォールバックする。純関数。
pub fn merge_timeline(observed: Vec<Frame>, forecast: Vec<Frame>) -> Timeline {
    let mut frames: Vec<Frame> = observed;
    frames.extend(forecast);
    // validtime 昇順。同一 validtime では実況を先に置き、直後の dedup で実況が残るようにする。
    frames.sort_by(|a, b| {
        a.validtime
            .cmp(&b.validtime)
            .then_with(|| kind_rank(a.kind).cmp(&kind_rank(b.kind)))
    });
    // dedup_by は「同じ」と判定した後続要素を落とし、先頭(=実況)を残す。
    frames.dedup_by(|a, b| a.validtime == b.validtime);

    let now_idx = frames
        .iter()
        .rposition(|f| f.kind == FrameKind::Observed)
        .unwrap_or(0);
    Timeline { frames, now_idx }
}

// 同一 validtime の並び順を決める重み。実況(0)が予報(1)より前に来る。
fn kind_rank(k: FrameKind) -> u8 {
    match k {
        FrameKind::Observed => 0,
        FrameKind::Forecast => 1,
    }
}

// ---- 表示用の時刻整形(日時crate非依存・純関数) ----

// "20260814060000"(14桁・UTC) → "15:00"(JST・時分のみ)。
// JMA が返すのは UTC(実測: UTC 12:56 時点の最新 basetime が 20260814125500 と一致し、
// JST 21:56 とは一致しなかった)。+9時間して JST の時分だけを返す。
// 日付跨ぎは (hh + 9) % 24 で正しく出る。時分しか出さないので日付の繰り上がり表示は不要。
// 桁数・範囲が想定外の文字列は None を返す(呼び出し側は生の文字列等へフォールバックする)。
pub fn jst_hhmm(utc_compact: &str) -> Option<String> {
    let (_, _, _, hh, mn) = split_compact(utc_compact)?;
    Some(format!("{:02}:{:02}", (hh + 9) % 24, mn))
}

// "YYYYMMDDHHMMSS" を (年, 月, 日, 時, 分) へ分解する。秒は使わないので数字であることだけ見る。
// 14桁ASCII数字、かつ月1〜12・日1〜31・時0〜23・分0〜59でなければ None。
fn split_compact(s: &str) -> Option<(i64, i64, i64, i64, i64)> {
    let b = s.as_bytes();
    if b.len() != 14 || !b.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    // 14桁ASCII数字であることを確認済みなので、以降のスライスは境界内かつUTF-8境界上。
    let num = |from: usize, to: usize| -> i64 { s[from..to].parse::<i64>().unwrap_or(-1) };
    let (y, mo, d, hh, mn) = (num(0, 4), num(4, 6), num(6, 8), num(8, 10), num(10, 12));
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || !(0..=23).contains(&hh) || !(0..=59).contains(&mn) {
        return None;
    }
    Some((y, mo, d, hh, mn))
}

// UTC の "YYYYMMDDHHMMSS" を epoch(1970-01-01 00:00 UTC)からの経過分に変換する。
// フレーム間の差分(「+30分」等)を日付跨ぎでも正しく出すために使う。
fn epoch_minutes(utc_compact: &str) -> Option<i64> {
    let (y, mo, d, hh, mn) = split_compact(utc_compact)?;
    Some(days_from_civil(y, mo, d) * 1440 + hh * 60 + mn)
}

// 暦日 → epoch からの日数(Howard Hinnant の days_from_civil)。うるう年・世紀の例外を正しく扱う。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // 3月を0とした月番号 [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // その年(3月始まり)の通日 [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // era 内の通日 [0, 146096]
    era * 146097 + doe - 719468
}

// 表示中フレームの人間向けラベル。
//   "15:00 実況"          … 「現在」ちょうど
//   "15:30 予報 +30分"    … 未来
//   "14:40 実況 -20分"    … 過去へスクラブ中
// 絶対時刻だけだと今からの距離が読み取りにくいので、now_idx との差を併記する(§8.8)。
pub fn frame_label(tl: &Timeline, idx: usize) -> String {
    let Some(f) = tl.get(idx) else {
        return "時刻不明".to_string();
    };
    // 時刻としてパースできない場合は生の文字列を出す(壊れ方が分かるように黙って消さない)。
    let hhmm = jst_hhmm(&f.validtime).unwrap_or_else(|| f.validtime.clone());
    let kind = match f.kind {
        FrameKind::Observed => "実況",
        FrameKind::Forecast => "予報",
    };
    let rel = tl
        .get(tl.now_idx)
        .and_then(|now| Some(epoch_minutes(&f.validtime)? - epoch_minutes(&now.validtime)?))
        .filter(|d| *d != 0)
        .map(|d| if d > 0 { format!(" +{d}分") } else { format!(" -{}分", -d) })
        .unwrap_or_default();
    format!("{hhmm} {kind}{rel}")
}

// ---- 圏域判定 ----

// 表示中の範囲が日本(ナウキャストの提供範囲)にかかっているか。
// 範囲外なら1枚もリクエストしない(海外で開いたときに公共サービスへ無駄な404を量産しない)。
// 判定はラフな矩形の交差のみ。min/max が逆に渡されても入れ替えて評価し、NaN は false になる。
pub fn covers_japan(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> bool {
    let (lat_lo, lat_hi) = if lat_min > lat_max { (lat_max, lat_min) } else { (lat_min, lat_max) };
    let (lon_lo, lon_hi) = if lon_min > lon_max { (lon_max, lon_min) } else { (lon_min, lon_max) };
    lat_hi >= JP_LAT_MIN && lat_lo <= JP_LAT_MAX && lon_hi >= JP_LON_MIN && lon_lo <= JP_LON_MAX
}

// ---- 背景ポーラー(gpslive::GpsPoller と同型) ----

// targetTimes を定期取得する背景スレッド。drop すると停止フラグを立ててスレッドを join し、
// 取得が失敗し続けて send に到達しないケースでもスレッドを確実に止める。
pub struct RadarClock {
    pub rx: Receiver<Timeline>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for RadarClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// interval_secs ごとに fetch_timeline() し、成功したものだけ channel へ送る背景スレッドを起動する。
// 起動直後に1回取得するので、ONにしてから最初の interval を待たずに時刻一覧が届く。
// 停止契機は2つ: ①受信側 drop で send 失敗 ②RadarClock drop で stop フラグ。
// 取得失敗が続き send に到達しない場合でも、stop を小刻み(200ms)に確認するので確実に終わる。
pub fn start_clock(interval_secs: u64) -> RadarClock {
    let (tx, rx) = mpsc::channel::<Timeline>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let interval = interval_secs.max(MIN_REFRESH_SECS);
    let handle = thread::spawn(move || {
        while !stop_thread.load(Ordering::Relaxed) {
            // 失敗は握り潰す。地図は落とさず、次の周期でまた試す(§8.1)。
            if let Ok(tl) = fetch_timeline() {
                if tx.send(tl).is_err() {
                    return; // 受信側drop→終了
                }
            }
            // interval 分を 200ms 刻みで待ち、その都度 stop を確認する(join を素早く返すため)。
            for _ in 0..(interval.saturating_mul(5)) {
                if stop_thread.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(Duration::from_millis(200));
            }
        }
    });
    RadarClock { rx, stop, handle: Some(handle) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 実際の targetTimes_N1.json の応答形(2026/08/14 実測)。
    const N1_SAMPLE: &str = r#"[
  {"basetime": "20260814125500", "validtime": "20260814125500", "elements": ["hrpns", "hrpns_nd"]}
 ,{"basetime": "20260814125000", "validtime": "20260814125000", "elements": ["hrpns", "hrpns_nd"]}
 ,{"basetime": "20260814124500", "validtime": "20260814124500", "elements": ["hrpns", "hrpns_nd"]}
]"#;

    // 実際の targetTimes_N2.json の応答形(2026/08/14 実測)。basetime は最新実況で固定。
    const N2_SAMPLE: &str = r#"[
  {"basetime": "20260814125500", "validtime": "20260814131000", "elements": ["hrpns", "hrpns_nd"]}
 ,{"basetime": "20260814125500", "validtime": "20260814130500", "elements": ["hrpns", "hrpns_nd"]}
 ,{"basetime": "20260814125500", "validtime": "20260814130000", "elements": ["hrpns", "hrpns_nd"]}
]"#;

    fn f(basetime: &str, validtime: &str, kind: FrameKind) -> Frame {
        Frame { basetime: basetime.to_string(), validtime: validtime.to_string(), kind }
    }

    // ---- parse_target_times ----

    #[test]
    fn parse_n1_sample() {
        let got = parse_target_times(N1_SAMPLE, FrameKind::Observed);
        assert_eq!(got.len(), 3);
        // 応答の並び(新しい順)をそのまま保つ。並べ替えは merge_timeline の責務。
        assert_eq!(got[0], f("20260814125500", "20260814125500", FrameKind::Observed));
        assert!(got.iter().all(|x| x.kind == FrameKind::Observed));
    }

    #[test]
    fn parse_n2_sample_is_forecast() {
        let got = parse_target_times(N2_SAMPLE, FrameKind::Forecast);
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|x| x.kind == FrameKind::Forecast));
        // basetime は全件共通、validtime だけが進む。
        assert!(got.iter().all(|x| x.basetime == "20260814125500"));
        assert!(got.iter().all(|x| x.validtime > x.basetime));
    }

    #[test]
    fn parse_promotes_to_forecast_when_validtime_ahead() {
        // hint が実況でも validtime > basetime なら予報として扱う(時刻の前後関係を優先)。
        let body = r#"[{"basetime":"20260814125500","validtime":"20260814130000"}]"#;
        let got = parse_target_times(body, FrameKind::Observed);
        assert_eq!(got[0].kind, FrameKind::Forecast);
    }

    #[test]
    fn parse_empty_array() {
        assert!(parse_target_times("[]", FrameKind::Observed).is_empty());
    }

    #[test]
    fn parse_broken_json_does_not_panic() {
        assert!(parse_target_times("", FrameKind::Observed).is_empty());
        assert!(parse_target_times("{", FrameKind::Observed).is_empty());
        assert!(parse_target_times("not json at all", FrameKind::Observed).is_empty());
        // 配列でないJSON(オブジェクト/スカラー)も空。
        assert!(parse_target_times(r#"{"basetime":"20260814125500"}"#, FrameKind::Observed).is_empty());
        assert!(parse_target_times("123", FrameKind::Observed).is_empty());
        assert!(parse_target_times("null", FrameKind::Observed).is_empty());
    }

    #[test]
    fn parse_skips_entries_with_missing_or_wrong_typed_fields() {
        let body = r#"[
          {"basetime":"20260814125500"},
          {"validtime":"20260814125000"},
          {"basetime":20260814124500,"validtime":"20260814124500"},
          {"basetime":"20260814124000","validtime":"20260814124000"},
          "junk",
          null
        ]"#;
        let got = parse_target_times(body, FrameKind::Observed);
        // 揃っていて型も正しい1件だけが残る。
        assert_eq!(got, vec![f("20260814124000", "20260814124000", FrameKind::Observed)]);
    }

    #[test]
    fn parse_ignores_unknown_fields() {
        let body = r#"[{"basetime":"20260814125500","validtime":"20260814125500",
                        "elements":["hrpns"],"future_field":{"nested":true}}]"#;
        assert_eq!(parse_target_times(body, FrameKind::Observed).len(), 1);
    }

    #[test]
    fn parse_rejects_non_digit_time_tokens() {
        // URLのパス要素にそのまま埋めるので、数字以外を含むものは採用しない。
        let body = r#"[
          {"basetime":"../../etc","validtime":"20260814125500"},
          {"basetime":"20260814125500","validtime":"2026-08-14T12:55:00Z"},
          {"basetime":"","validtime":""},
          {"basetime":"20260814125500","validtime":"20260814125500"}
        ]"#;
        let got = parse_target_times(body, FrameKind::Observed);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].validtime, "20260814125500");
    }

    // ---- merge_timeline ----

    #[test]
    fn merge_sorts_ascending_and_sets_now_idx() {
        let obs = parse_target_times(N1_SAMPLE, FrameKind::Observed);
        let fcst = parse_target_times(N2_SAMPLE, FrameKind::Forecast);
        let tl = merge_timeline(obs, fcst);

        let times: Vec<&str> = tl.frames.iter().map(|x| x.validtime.as_str()).collect();
        assert_eq!(
            times,
            vec![
                "20260814124500",
                "20260814125000",
                "20260814125500",
                "20260814130000",
                "20260814130500",
                "20260814131000",
            ]
        );
        // 「現在」= 実況の最後尾。
        assert_eq!(tl.now_idx, 2);
        assert_eq!(tl.frames[tl.now_idx].kind, FrameKind::Observed);
        assert!(tl.frames[3..].iter().all(|x| x.kind == FrameKind::Forecast));
    }

    #[test]
    fn merge_prefers_observed_on_duplicate_validtime() {
        let obs = vec![f("20260814125500", "20260814125500", FrameKind::Observed)];
        // 同一 validtime の予報(basetimeが違う)が来ても実況が勝つ。
        let fcst = vec![f("20260814125000", "20260814125500", FrameKind::Forecast)];
        let tl = merge_timeline(obs, fcst);
        assert_eq!(tl.frames.len(), 1);
        assert_eq!(tl.frames[0].kind, FrameKind::Observed);
        assert_eq!(tl.frames[0].basetime, "20260814125500");
        assert_eq!(tl.now_idx, 0);
    }

    #[test]
    fn merge_with_only_observed() {
        let obs = parse_target_times(N1_SAMPLE, FrameKind::Observed);
        let tl = merge_timeline(obs, Vec::new());
        assert_eq!(tl.frames.len(), 3);
        assert_eq!(tl.now_idx, 2); // 実況しか無ければ最後尾が「現在」
    }

    #[test]
    fn merge_with_only_forecast_falls_back_to_head() {
        // 実況が1件も無い異常系。now_idx は先頭へフォールバックし、範囲外を指さない。
        let fcst = parse_target_times(N2_SAMPLE, FrameKind::Forecast);
        let tl = merge_timeline(Vec::new(), fcst);
        assert_eq!(tl.frames.len(), 3);
        assert_eq!(tl.now_idx, 0);
        assert!(tl.get(tl.now_idx).is_some());
    }

    #[test]
    fn merge_empty_is_empty() {
        let tl = merge_timeline(Vec::new(), Vec::new());
        assert!(tl.is_empty());
        assert_eq!(tl.now_idx, 0);
        assert!(tl.get(0).is_none());
    }

    // ---- jst_hhmm ----

    #[test]
    fn jst_adds_nine_hours() {
        assert_eq!(jst_hhmm("20260814060000").as_deref(), Some("15:00"));
        // 実測データ: UTC 12:55 → JST 21:55
        assert_eq!(jst_hhmm("20260814125500").as_deref(), Some("21:55"));
    }

    #[test]
    fn jst_wraps_across_midnight() {
        assert_eq!(jst_hhmm("20260814150000").as_deref(), Some("00:00"));
        assert_eq!(jst_hhmm("20260814235500").as_deref(), Some("08:55"));
    }

    #[test]
    fn jst_rejects_malformed() {
        assert_eq!(jst_hhmm(""), None);
        assert_eq!(jst_hhmm("2026081412550"), None); // 13桁
        assert_eq!(jst_hhmm("202608141255000"), None); // 15桁
        assert_eq!(jst_hhmm("20260814T060000Z"), None); // 設計書が想定していたISO風は来ない
        assert_eq!(jst_hhmm("2026081412550x"), None); // 非数字混入
        assert_eq!(jst_hhmm("20260814995500"), None); // 時が範囲外
        assert_eq!(jst_hhmm("20260814129900"), None); // 分が範囲外
        assert_eq!(jst_hhmm("20261314125500"), None); // 月が範囲外
        assert_eq!(jst_hhmm("20260800125500"), None); // 日が範囲外
    }

    // ---- frame_label ----

    #[test]
    fn label_now_has_no_relative_part() {
        let tl = merge_timeline(
            parse_target_times(N1_SAMPLE, FrameKind::Observed),
            parse_target_times(N2_SAMPLE, FrameKind::Forecast),
        );
        assert_eq!(frame_label(&tl, tl.now_idx), "21:55 実況");
    }

    #[test]
    fn label_forecast_shows_plus_minutes() {
        let tl = merge_timeline(
            parse_target_times(N1_SAMPLE, FrameKind::Observed),
            parse_target_times(N2_SAMPLE, FrameKind::Forecast),
        );
        assert_eq!(frame_label(&tl, 5), "22:10 予報 +15分");
    }

    #[test]
    fn label_past_shows_minus_minutes() {
        let tl = merge_timeline(
            parse_target_times(N1_SAMPLE, FrameKind::Observed),
            parse_target_times(N2_SAMPLE, FrameKind::Forecast),
        );
        assert_eq!(frame_label(&tl, 0), "21:45 実況 -10分");
    }

    #[test]
    fn label_spans_date_boundary() {
        // 23:55Z(JST 08:55) → 翌00:10Z(JST 09:10) の差が +15分として出る(日跨ぎ)。
        let tl = merge_timeline(
            vec![f("20260814235500", "20260814235500", FrameKind::Observed)],
            vec![f("20260814235500", "20260815001000", FrameKind::Forecast)],
        );
        assert_eq!(frame_label(&tl, 0), "08:55 実況");
        assert_eq!(frame_label(&tl, 1), "09:10 予報 +15分");
    }

    #[test]
    fn label_out_of_range_does_not_panic() {
        let tl = Timeline::default();
        assert_eq!(frame_label(&tl, 0), "時刻不明");
        assert_eq!(frame_label(&tl, 999), "時刻不明");
    }

    // ---- reanchor ----

    fn tl_for_reanchor() -> Timeline {
        merge_timeline(
            parse_target_times(N1_SAMPLE, FrameKind::Observed),
            parse_target_times(N2_SAMPLE, FrameKind::Forecast),
        )
    }

    #[test]
    fn reanchor_following_jumps_to_now() {
        let tl = tl_for_reanchor();
        // 追従中は直前の表示時刻に関係なく now_idx へ。
        let (idx, follow, msg) = tl.reanchor(Some("20260814124500"), true);
        assert_eq!(idx, tl.now_idx);
        assert!(follow);
        assert!(msg.is_none());
    }

    #[test]
    fn reanchor_keeps_same_validtime_when_scrubbed() {
        let tl = tl_for_reanchor();
        let (idx, follow, msg) = tl.reanchor(Some("20260814130500"), false);
        assert_eq!(tl.frames[idx].validtime, "20260814130500");
        assert!(!follow); // ユーザーのスクラブ状態を維持する
        assert!(msg.is_none());
    }

    #[test]
    fn reanchor_clamps_to_nearest_when_frame_disappeared() {
        let tl = tl_for_reanchor();
        // 期限切れで消えた古い時刻 → 最も近い先頭(12:45)へクランプ。
        let (idx, follow, msg) = tl.reanchor(Some("20260814120000"), false);
        assert_eq!(tl.frames[idx].validtime, "20260814124500");
        assert!(!follow);
        assert_eq!(msg.as_deref(), Some("表示時刻を調整しました"));
    }

    #[test]
    fn reanchor_clamps_forward_to_nearest() {
        let tl = tl_for_reanchor();
        // 一覧の末尾より未来 → 最後尾へクランプ。
        let (idx, _follow, msg) = tl.reanchor(Some("20260814140000"), false);
        assert_eq!(tl.frames[idx].validtime, "20260814131000");
        assert!(msg.is_some());
    }

    #[test]
    fn reanchor_picks_the_closer_of_two_neighbors() {
        let tl = tl_for_reanchor();
        // 12:57 は 12:55(2分前) と 13:00(3分後) の間 → 近い方の 12:55 を選ぶ。
        let (idx, _, _) = tl.reanchor(Some("20260814125700"), false);
        assert_eq!(tl.frames[idx].validtime, "20260814125500");
    }

    #[test]
    fn reanchor_nearest_across_date_boundary() {
        // 文字列比較ではなく時刻として近い方を選ぶ(日跨ぎでも壊れない)。
        let tl = merge_timeline(
            vec![
                f("20260814234500", "20260814234500", FrameKind::Observed),
                f("20260815000500", "20260815000500", FrameKind::Observed),
            ],
            Vec::new(),
        );
        // 00:02 は 23:45(17分前) より 00:05(3分後) に近い。
        let (idx, _, msg) = tl.reanchor(Some("20260815000200"), false);
        assert_eq!(tl.frames[idx].validtime, "20260815000500");
        assert!(msg.is_some());
    }

    #[test]
    fn reanchor_without_prev_validtime_returns_to_now() {
        let tl = tl_for_reanchor();
        let (idx, follow, msg) = tl.reanchor(None, false);
        assert_eq!(idx, tl.now_idx);
        assert!(follow);
        assert!(msg.is_none());
    }

    #[test]
    fn reanchor_on_empty_timeline_does_not_panic() {
        let tl = Timeline::default();
        assert_eq!(tl.reanchor(Some("20260814125500"), false), (0, false, None));
        assert_eq!(tl.reanchor(None, true), (0, true, None));
    }

    #[test]
    fn reanchor_with_unparsable_prev_falls_back_to_string_order() {
        let tl = tl_for_reanchor();
        // 時刻としてパースできない文字列でもパニックせず、範囲内の index を返す。
        let (idx, follow, msg) = tl.reanchor(Some("garbage"), false);
        assert!(idx < tl.frames.len());
        assert!(!follow);
        assert!(msg.is_some());
    }

    // ---- covers_japan ----

    #[test]
    fn covers_japan_tokyo() {
        // 東京周辺の小さな窓。
        assert!(covers_japan(35.6, 139.7, 35.8, 139.9));
    }

    #[test]
    fn covers_japan_hawaii_is_false() {
        // ハワイ(lat 21, lon -158)。緯度は範囲内だが経度が全く違う。
        assert!(!covers_japan(21.2, -158.0, 21.4, -157.8));
    }

    #[test]
    fn covers_japan_wide_view_true() {
        // 日本を含む広域(アジア〜太平洋)。
        assert!(covers_japan(-10.0, 90.0, 60.0, 180.0));
    }

    #[test]
    fn covers_japan_boundaries() {
        // 矩形の角でちょうど接する場合は含むとみなす(取りこぼしより無駄打ちの方が軽い)。
        assert!(covers_japan(10.0, 110.0, JP_LAT_MIN, JP_LON_MIN));
        assert!(covers_japan(JP_LAT_MAX, JP_LON_MAX, 60.0, 160.0));
        // わずかに外れたら false。
        assert!(!covers_japan(10.0, 110.0, JP_LAT_MIN - 0.001, JP_LON_MIN - 0.001));
        assert!(!covers_japan(JP_LAT_MAX + 0.001, JP_LON_MAX + 0.001, 60.0, 160.0));
    }

    #[test]
    fn covers_japan_other_far_places() {
        assert!(!covers_japan(51.4, -0.2, 51.6, 0.0)); // ロンドン
        assert!(!covers_japan(-34.0, 151.1, -33.8, 151.3)); // シドニー(経度は日本域だが緯度が南)
    }

    #[test]
    fn covers_japan_handles_swapped_and_nan() {
        // min/max が逆でも同じ判定になる。
        assert!(covers_japan(35.8, 139.9, 35.6, 139.7));
        // NaN は false(判定不能なので取りに行かない)。
        assert!(!covers_japan(f64::NAN, 139.7, 35.8, 139.9));
        assert!(!covers_japan(35.6, f64::NAN, 35.8, f64::NAN));
    }

    // ---- 内部ヘルパー ----

    #[test]
    fn epoch_minutes_diffs_are_exact() {
        let a = epoch_minutes("20260814235500").unwrap();
        let b = epoch_minutes("20260815001000").unwrap();
        assert_eq!(b - a, 15);
        // うるう年の2/28→3/1(2028年はうるう年なので間に2/29が入る)。
        let c = epoch_minutes("20280228000000").unwrap();
        let d = epoch_minutes("20280301000000").unwrap();
        assert_eq!(d - c, 2 * 1440);
        // 平年(2026年)は1日。
        let e = epoch_minutes("20260228000000").unwrap();
        let g = epoch_minutes("20260301000000").unwrap();
        assert_eq!(g - e, 1440);
    }

    #[test]
    fn days_from_civil_matches_known_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2026, 8, 14), 20679);
    }

}
