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

// 画面表示用: 現在地から見て次に案内すべき曲がり角までの残り距離(m)と案内文。
// tick()と違い読み上げ済みかの状態は持たない(呼んでも状態を変えない・何度呼んでも同じ結果)。
// 単に「まだ通過していない最初の曲がり角」を返すだけなので、Near/Imminentの区別なく常に出せる。
pub fn next_turn_display(turns: &[crate::route::TurnPoint], progress_m: f64) -> Option<(f64, &'static str)> {
    turns.iter().find_map(|t| {
        let remaining = t.dist_from_start_m - progress_m;
        if remaining < PASSED_M { None } else { Some((remaining.max(0.0), turn_phrase(&t.turn))) }
    })
}

// 距離を50m刻みに丸める(「287メートル先」より「300メートル先」の方が聞き取りやすい)。
pub(crate) fn round_to_50(m: f64) -> i64 {
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
        // 「分岐」は漢字だとmacOSのsay(Kyoko)が「うんき」等に誤読することがあるため、
        // 読み間違えようのないひらがな表記にする(実機で確認済みの回避策)。
        "KL" => "ぶんきを左です",
        "KR" => "ぶんきを右です",
        "TU" => "この先Uターンです",
        "C" => "この先道なりです",
        "ARRIVE" => "まもなく到着です",
        s if s.starts_with("RNDB") || s.starts_with("RNAB") => "この先ロータリーです",
        _ => "この先進行方向に注意してください",
    }
}

// 読み上げ実行。sound.rs::Sound::play と同じ方針で、対応する経路だけが実際に鳴る。
// native_local: この端末(macOSのsay)でも鳴らすか。falseならブラウザ側(speak_web)だけが鳴る。
// web版で見ている間、手元のMac本体が同時に喋るのを避けたい場合にfalseで呼ぶ。
// voice: sayに渡す音声名(空=OS既定、cfg.voice_nameを渡す)。
pub fn speak(text: &str, native_local: bool, voice: &str) {
    if native_local {
        speak_local_with(voice, text);
    }
    speak_web(text);
}

// sayの引数を組み立てる純関数(macOS以外でも単体テストできるよう分離)。
// voiceが空なら-vを付けずOS既定の声に任せる。
pub(crate) fn say_args(voice: &str, text: &str) -> Vec<String> {
    if voice.is_empty() {
        vec![text.to_string()]
    } else {
        vec!["-v".to_string(), voice.to_string(), text.to_string()]
    }
}

#[cfg(target_os = "macos")]
fn speak_local_with(voice: &str, text: &str) {
    // 起動して待たない(Sound::playや効果音と同じ非ブロッキング方針)。指定した声が無い環境では
    // sayコマンド自体は失敗するが、spawnの戻り値は握りつぶすだけで実害はない。
    // stdout/stderrは破棄する(存在しない音声名だと`Voice 'X' not found.`をstderrへ出し、
    // 代替スクリーンで描画中のTUIへそのまま文字列が流れ込むため)。
    let _ = std::process::Command::new("say")
        .args(say_args(voice, text))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
#[cfg(not(target_os = "macos"))]
fn speak_local_with(_voice: &str, _text: &str) {}

// 試聴(設定画面でEnter確定時)。アンインストール済み等で実際には鳴らない場合を拾うため、
// spawnして捨てるのではなく終了コードを別スレッドで待って結果を返す
// (route::trigger_turn_points と同じ、mpsc受信側を返してUIループでポーリングする形)。
#[cfg(target_os = "macos")]
pub(crate) fn preview_voice(voice: &str, text: &str) -> std::sync::mpsc::Receiver<Result<(), String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let voice = voice.to_string();
    let text = text.to_string();
    std::thread::spawn(move || {
        let result = std::process::Command::new("say")
            .args(say_args(&voice, &text))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = tx.send(match result {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err(format!("この声は再生できませんでした: {voice}")),
            Err(e) => Err(format!("この声は再生できませんでした: {voice} ({e})")),
        });
    });
    rx
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn preview_voice(_voice: &str, _text: &str) -> std::sync::mpsc::Receiver<Result<(), String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(Ok(()));
    rx
}

// `say -v '?'`で列挙したja始まりロケールの音声名一覧(OnceLockで1回だけ実行し使い回す)。
// 非macOSでは常に空を返す。
static JA_VOICES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

pub(crate) fn japanese_voices() -> &'static [String] {
    JA_VOICES.get_or_init(fetch_japanese_voices)
}

// Focus::Settingsに入った時点で呼ぶ。japanese_voices()を叩くだけのスレッドを1本投げて
// 事前にOnceLockを温める(設定画面を開いてから声の行までカーソルを下げる間に完了させる)。
pub(crate) fn warm_voice_list() {
    std::thread::spawn(|| { japanese_voices(); });
}

#[cfg(target_os = "macos")]
fn fetch_japanese_voices() -> Vec<String> {
    let out = std::process::Command::new("say").arg("-v").arg("?").output();
    match out {
        Ok(o) => parse_say_voices(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Vec::new(),
    }
}
#[cfg(not(target_os = "macos"))]
fn fetch_japanese_voices() -> Vec<String> {
    Vec::new()
}

// `say -v '?'`の出力("Kyoko               ja_JP    # こんにちは!..."形式)からja始まりロケールの
// 音声名だけを取り出す純関数(ネットワーク/プロセス起動に触れないのでmacOS以外でもテスト可能)。
// 名前は空白を含みうるため列位置での切り出しはしない: 最初の'#'より左を見て、末尾の
// 空白区切りトークンをロケールとし、残りをtrimしたものを名前とする。同名は先勝ちで畳む。
pub(crate) fn parse_say_voices(stdout: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let left = line.split('#').next().unwrap_or("");
        let left = left.trim_end();
        let Some((name_part, locale)) = left.rsplit_once(char::is_whitespace) else { continue };
        let name = name_part.trim();
        if name.is_empty() {
            continue;
        }
        let locale_norm = locale.replace('-', "_");
        if locale_norm != "ja" && !locale_norm.starts_with("ja_") {
            continue; // "java_XX"等、"ja"を含むが別ロケールの語を拾わない
        }
        if !out.iter().any(|n: &String| n == name) {
            out.push(name.to_string());
        }
    }
    out
}

// 一覧表示専用: 末尾の括弧グループ(ロケール表記)を落として短くする。保存する値は常に
// 生の名前のままで、これは表示だけに使う。
pub(crate) fn display_voice_name(raw: &str) -> &str {
    if raw.ends_with(')') {
        if let Some(i) = raw.rfind(" (") {
            if i > 0 {
                return &raw[..i];
            }
        }
    }
    raw
}

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
    fn next_turn_display_returns_nearest_unpassed_turn() {
        let turns = vec![tp("TL", 1000.0), tp("TR", 5000.0)];
        let (remaining, phrase) = next_turn_display(&turns, 700.0).unwrap();
        assert_eq!(remaining, 300.0);
        assert_eq!(phrase, "左折です");
    }

    #[test]
    fn next_turn_display_skips_passed_turns() {
        // 1つ目(100m地点)は既に通過済み(500m地点にいる)なので、2つ目(900m地点)を返す。
        let turns = vec![tp("TL", 100.0), tp("ARRIVE", 900.0)];
        let (remaining, phrase) = next_turn_display(&turns, 500.0).unwrap();
        assert_eq!(remaining, 400.0);
        assert_eq!(phrase, "まもなく到着です");
    }

    #[test]
    fn next_turn_display_none_when_all_turns_passed_or_empty() {
        assert!(next_turn_display(&[], 0.0).is_none());
        let turns = vec![tp("TL", 100.0)];
        assert!(next_turn_display(&turns, 1000.0).is_none());
    }

    #[test]
    fn next_turn_display_does_not_consume_state_repeated_calls_match() {
        // tick()と違い、同じ引数で何度呼んでも同じ結果(状態を変えない)。
        let turns = vec![tp("TR", 1000.0)];
        let a = next_turn_display(&turns, 800.0);
        let b = next_turn_display(&turns, 800.0);
        assert_eq!(a, b);
    }

    #[test]
    fn turn_phrase_covers_known_codes_and_falls_back() {
        assert_eq!(turn_phrase("TL"), "左折です");
        assert_eq!(turn_phrase("TR"), "右折です");
        assert_eq!(turn_phrase("ARRIVE"), "まもなく到着です");
        assert_eq!(turn_phrase("RNDB2"), "この先ロータリーです");
        assert_eq!(turn_phrase("謎コード"), "この先進行方向に注意してください");
    }

    #[test]
    fn say_args_omits_dash_v_when_voice_is_empty() {
        assert_eq!(say_args("", "300メートル先、左折です"), vec!["300メートル先、左折です".to_string()]);
    }

    #[test]
    fn say_args_adds_dash_v_when_voice_is_set() {
        assert_eq!(say_args("Kyoko", "hello"), vec!["-v".to_string(), "Kyoko".to_string(), "hello".to_string()]);
    }

    #[test]
    fn say_args_keeps_a_space_containing_name_as_one_element() {
        let got = say_args("Eddy (英語（アメリカ）)", "hi");
        assert_eq!(got, vec!["-v".to_string(), "Eddy (英語（アメリカ）)".to_string(), "hi".to_string()]);
    }

    #[test]
    fn parse_say_voices_extracts_ja_names_only() {
        let stdout = "Kyoko               ja_JP    # こんにちは! 私の名前はKyokoです。\n\
                       Eddy (英語（アメリカ）)     en_US    # Hello! My name is Eddy.\n\
                       Otoya               ja_JP    # こんにちは! 私の名前はOtoyaです。\n";
        assert_eq!(parse_say_voices(stdout), vec!["Kyoko".to_string(), "Otoya".to_string()]);
    }

    #[test]
    fn parse_say_voices_accepts_hyphenated_locale_form() {
        let stdout = "Kyoko               ja-JP    # sample\n";
        assert_eq!(parse_say_voices(stdout), vec!["Kyoko".to_string()]);
    }

    #[test]
    fn parse_say_voices_does_not_match_locales_that_merely_start_with_ja_like_java() {
        let stdout = "Weird               java_XX    # sample\n";
        assert!(parse_say_voices(stdout).is_empty());
    }

    #[test]
    fn parse_say_voices_handles_names_with_parens_and_sample_text_containing_hash() {
        let stdout = "Eddy (日本語（日本）)     ja_JP    # コメント中に # が入っていても平気\n";
        assert_eq!(parse_say_voices(stdout), vec!["Eddy (日本語（日本）)".to_string()]);
    }

    #[test]
    fn parse_say_voices_handles_empty_and_blank_input_without_panicking() {
        assert!(parse_say_voices("").is_empty());
        assert!(parse_say_voices("\n\n").is_empty());
        assert!(parse_say_voices("   \n").is_empty());
    }

    #[test]
    fn parse_say_voices_dedupes_by_name_keeping_first_occurrence_and_order() {
        let stdout = "Kyoko               ja_JP    # 1回目\n\
                       Otoya               ja_JP    # 別の声\n\
                       Kyoko               ja_JP    # 2回目(重複)\n";
        assert_eq!(parse_say_voices(stdout), vec!["Kyoko".to_string(), "Otoya".to_string()]);
    }

    #[test]
    fn display_voice_name_drops_trailing_parenthetical_locale() {
        assert_eq!(display_voice_name("Eddy (日本語（日本）)"), "Eddy");
        assert_eq!(display_voice_name("Kyoko"), "Kyoko");
    }

    #[test]
    fn display_voice_name_keeps_the_raw_name_when_stripping_would_empty_it() {
        assert_eq!(display_voice_name("(日本語（日本）)"), "(日本語（日本）)");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn japanese_voices_is_empty_on_non_macos() {
        assert!(japanese_voices().is_empty());
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn preview_voice_succeeds_immediately_on_non_macos() {
        let rx = preview_voice("Kyoko", "test");
        assert_eq!(rx.recv().unwrap(), Ok(()));
    }
}
