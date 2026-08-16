//! ルート音声案内。route::TurnPoint(BRouterの曲がり角情報)と現在地の進捗距離から、
//! 残り距離が閾値を切ったタイミングで1回だけ読み上げ内容を返す。読み上げの実体は
//! sound.rs の Sound::play と同じ方針で「常に両方試す」: ローカル実行時はmacOSの`say`、
//! web実行時はブラウザのWeb Speech API(既存の効果音と同じOSC経由の合図)。どちらの
//! 経路も、対応していない側では無害に無視される。

// 案内の距離閾値。300m手前で1段階目、30m手前(直前)で2段階目を読み上げる。
pub const NEAR_M: f64 = 300.0;
pub const IMMINENT_M: f64 = 30.0;
// 「もう通過した」とみなす閾値(マイナス側)。GPSのブレで多少行き過ぎても誤って
// 案内し直さないよう、通過直後の再読み上げを防ぐ。
const PASSED_M: f64 = -20.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Pending,
    Near,
    Imminent,
    Done,
}

// 曲がり角ごとの案内進捗。route::TurnPointの一覧と同じ並び・同じ長さで保持する。
pub struct VoiceGuide {
    stages: Vec<Stage>,
}

impl VoiceGuide {
    pub fn new(turns: &[crate::route::TurnPoint]) -> Self {
        VoiceGuide { stages: vec![Stage::Pending; turns.len()] }
    }

    // turnsの要素数が変わった(ルート再計算等)場合は作り直しが必要。呼び出し側で
    // 「ルートが変わったらVoiceGuide::newし直す」運用にするための判定。
    pub fn matches_len(&self, turns: &[crate::route::TurnPoint]) -> bool {
        self.stages.len() == turns.len()
    }

    // 現在地のルート上進捗(起点からの累積距離, メートル)を渡し、今読み上げるべき
    // フレーズがあれば返す(無ければNone)。1回のtickで最大1件だけ返す
    // (turnsはルート順=dist_from_start_m昇順の前提。手前の曲がり角から順に処理する)。
    pub fn tick(&mut self, turns: &[crate::route::TurnPoint], progress_m: f64) -> Option<String> {
        for (i, t) in turns.iter().enumerate() {
            let stage = self.stages.get_mut(i)?;
            if *stage == Stage::Done {
                continue;
            }
            let remaining = t.dist_from_start_m - progress_m;
            if remaining < PASSED_M {
                *stage = Stage::Done; // 案内し損ねたが、通過済みなので黙って次へ
                continue;
            }
            // ここに到達したら「まだ案内すべき最も近い曲がり角」。判定してすぐ返す
            // (先の曲がり角を先読みして案内しない=手前から順番に案内する)。
            return match *stage {
                Stage::Pending if remaining <= NEAR_M => {
                    *stage = Stage::Near;
                    Some(format!("{}メートル先、{}", round_to_50(remaining), turn_phrase(&t.turn)))
                }
                Stage::Near if remaining <= IMMINENT_M => {
                    *stage = Stage::Imminent;
                    Some(turn_phrase(&t.turn).to_string())
                }
                _ => None,
            };
        }
        None
    }
}

// 距離を50m刻みに丸める(「287メートル先」より「300メートル先」の方が聞き取りやすい)。
fn round_to_50(m: f64) -> i64 {
    ((m.max(0.0) / 50.0).round() as i64) * 50
}

// BRouterの曲がり角コード → 日本語の案内文。未知のコードは無難な注意喚起にフォールバックする
// (turnInstructionModeの詳細な分類が増えても黙って落ちないように)。
fn turn_phrase(code: &str) -> &'static str {
    match code {
        "TL" => "左折です",
        "TR" => "右折です",
        "TSLL" => "緩やかに左です",
        "TSLR" => "緩やかに右です",
        "TSHL" => "急に左です",
        "TSHR" => "急に右です",
        "KL" => "分岐を左です",
        "KR" => "分岐を右です",
        "TU" => "この先Uターンです",
        "C" => "この先道なりです",
        "ARRIVE" => "まもなく到着です",
        s if s.starts_with("RNDB") || s.starts_with("RNAB") => "この先ロータリーです",
        _ => "この先進行方向に注意してください",
    }
}

// 読み上げ実行。sound.rs::Sound::play と同じ方針で、対応する経路だけが実際に鳴る。
pub fn speak(text: &str) {
    speak_local(text);
    speak_web(text);
}

#[cfg(target_os = "macos")]
fn speak_local(text: &str) {
    // 起動して待たない(Sound::playや効果音と同じ非ブロッキング方針)。Kyokoが無い環境では
    // sayコマンド自体は失敗するが、spawnの戻り値は握りつぶすだけで実害はない。
    let _ = std::process::Command::new("say").arg("-v").arg("Kyoko").arg(text).spawn();
}
#[cfg(not(target_os = "macos"))]
fn speak_local(_text: &str) {}

// ブラウザ(web/touch-overlay.js)が xterm.js の OSC ハンドラでフックする合図。
// ESC ] 9998 ; <base64> BEL の形。9998 はsound.rsの9999(効果音)の隣で、他用途と
// 衝突しないよう選んだ私的な番号。日本語(UTF-8マルチバイト)をそのまま埋め込むより、
// base64にした方が制御文字・ターミネータとの衝突を気にしなくてよい(1337の画像と同じ考え方)。
fn speak_web(text: &str) {
    use std::io::Write;
    let b64 = crate::render::base64_encode(text.as_bytes());
    print!("\x1b]9998;{b64}\x07");
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::TurnPoint;

    fn tp(turn: &str, dist_from_start_m: f64) -> TurnPoint {
        TurnPoint { lat: 0.0, lon: 0.0, turn: turn.to_string(), dist_from_start_m }
    }

    #[test]
    fn round_to_50_rounds_to_nearest_multiple() {
        assert_eq!(round_to_50(287.0), 300);
        assert_eq!(round_to_50(24.0), 0);
        assert_eq!(round_to_50(-5.0), 0);
    }

    #[test]
    fn tick_announces_near_then_imminent_once_each() {
        let turns = vec![tp("TL", 1000.0)];
        let mut g = VoiceGuide::new(&turns);
        assert!(g.tick(&turns, 0.0).is_none(), "遠いうちは何も言わない");
        let near = g.tick(&turns, 1000.0 - 300.0).unwrap();
        assert!(near.contains("メートル先"));
        assert!(near.contains("左折です"));
        assert!(g.tick(&turns, 1000.0 - 200.0).is_none(), "Near後、Imminent閾値前は再度言わない");
        let imminent = g.tick(&turns, 1000.0 - 30.0).unwrap();
        assert_eq!(imminent, "左折です");
        assert!(g.tick(&turns, 1000.0 - 10.0).is_none(), "Imminent後は同じ曲がり角を再度言わない");
        assert!(g.tick(&turns, 1000.0 + 50.0).is_none(), "通過後も言わない");
    }

    #[test]
    fn tick_processes_turns_in_order_one_at_a_time() {
        let turns = vec![tp("TL", 1000.0), tp("TR", 5000.0)];
        let mut g = VoiceGuide::new(&turns);
        // 1つ目がNearに入るまで、2つ目(はるか先)がフライングして案内されない。
        assert!(g.tick(&turns, 0.0).is_none());
        let first = g.tick(&turns, 1000.0 - 300.0).unwrap();
        assert!(first.contains("左折です"));
        let _ = g.tick(&turns, 1000.0 - 30.0); // 1つ目のImminent
        // 1つ目が完全にDoneになった後、2つ目の案内に進めること。
        let _ = g.tick(&turns, 1000.0 + 50.0); // 1つ目をDoneへ
        let second = g.tick(&turns, 5000.0 - 300.0).unwrap();
        assert!(second.contains("右折です"));
    }

    #[test]
    fn tick_skips_already_passed_turn_without_announcing() {
        // ガイド開始時点で既に通過済みの曲がり角は、案内せずDone扱いにする。
        let turns = vec![tp("TL", 100.0), tp("ARRIVE", 900.0)];
        let mut g = VoiceGuide::new(&turns);
        let got = g.tick(&turns, 500.0); // 1つ目(100m地点)は既に400m通過済み
        assert!(got.is_none() || got.unwrap().contains("到着"), "通過済みのTLを案内してはいけない");
    }

    #[test]
    fn matches_len_detects_route_change() {
        let turns = vec![tp("TL", 100.0)];
        let g = VoiceGuide::new(&turns);
        assert!(g.matches_len(&turns));
        let other = vec![tp("TL", 100.0), tp("TR", 200.0)];
        assert!(!g.matches_len(&other));
    }

    #[test]
    fn turn_phrase_covers_known_codes_and_falls_back() {
        assert_eq!(turn_phrase("TL"), "左折です");
        assert_eq!(turn_phrase("TR"), "右折です");
        assert_eq!(turn_phrase("ARRIVE"), "まもなく到着です");
        assert_eq!(turn_phrase("RNDB2"), "この先ロータリーです");
        assert_eq!(turn_phrase("謎コード"), "この先進行方向に注意してください");
    }
}
