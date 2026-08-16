ルート音声案内の読み上げ音声(TTS voice)を選べるようにする(#78)の設計。macOSローカル側は
`say` の音声名を設定値にし、web版はブラウザが持つ日本語ボイスを自動で選んで `u.voice` に
明示指定する。web側の変更は #86(iPhoneで音声案内が聞こえない)の対策候補F をそのまま満たす。

前提調査: `docs/tts-voice-selection-investigation.md`(#78)、`docs/ios-voice-guide-investigation.md`(#86)。

対象コード: `src/voice.rs` の `speak_local` / `src/config.rs` / `src/settings.rs` /
`src/ui.rs` の `Focus::Settings`・`Focus::SettingsPick` 分岐 / `src/ui_status.rs` の
`status_hint` / `web/touch-overlay.js` の `speakVoiceGuide`・`bindVoiceGuideOsc`。

## 1. 決定事項

設計判断として問われた3点の結論を先に置く。根拠は各章。

| # | 論点 | 結論 | 根拠 |
|---|---|---|---|
| 1 | macOSの候補一覧 | **実行時に `say -v '?'` を1回だけ実行して動的に列挙**(固定候補は採らない) | §3.2 |
| 2 | web版の選択方式 | **自動選択のみ。選択UIは作らない**(ただし優先順は決め打ちにせず規則化し、コンソールから上書きできる逃げ道を用意する) | §4.2 / §4.6 |
| 3 | 両者の設定統合 | **統合しない。設定項目はmacOS側だけに持つ**。webは設定を持たず自動 | §5 |

あわせて #86 の対策候補F(voiceschangedを待ってja-JPボイスを明示指定する)は §4 で実装する。
「ユーザーが声を選べるようにする」ためにブラウザのボイス一覧を確定させる処理が必ず要るので、
web側は選択UIを作らなくても F の対策そのものになる。

## 2. 実装状況

| 機能 | 状態 |
|---|---|
| macOS: 音声名を設定で持つ | 未実装(この設計の対象) |
| macOS: 候補一覧の動的列挙 | 未実装(この設計の対象) |
| macOS: 設定画面の一覧選択 + サンプル再生 | 未実装(この設計の対象) |
| web: ja ボイスの確定と `u.voice` 明示指定 | 未実装(この設計の対象・#86 対策5と同一) |
| web: 声の選択UI | **作らない**(§4.6) |
| Rust設定 → ブラウザへの声の受け渡し | **今回はやらない**(§5.2 に将来案だけ残す) |

## 3. macOSローカル側

### 3.1 設定値

`Config` に文字列フィールドを1つ足す。既存の音声関連2項目と同じ `[route]` セクションに置く。

```
[route]
voice_guide_enabled = false
voice_speak_local = true
voice_name = "Kyoko"        # 追加
```

| 項目 | 内容 |
|---|---|
| フィールド | `pub voice_name: String`(`src/config.rs` の `voice_guide_enabled` / `voice_speak_local` の直後) |
| 既定値 | `"Kyoko"` |
| 空文字の意味 | `say` に `-v` を付けない = OSの既定音声を使う(実測: `say -o out.aiff "テスト"` は `-v` 無しで exit 0) |
| キーが無い場合 | 既定値のまま(既存の全キーと同じ挙動。後方互換は自動的に取れる) |

既定を `"Kyoko"` にするのは現行の埋め込み値と同じにするため。`""`(OS既定)を既定にすると、
`turn_phrase()` の読み上げ調整(`src/voice.rs:95-98` の「ぶんき」ひらがな回避)がKyokoで確認した
ものである以上、既存ユーザーの聞こえ方が黙って変わる。

読み込み時の検証(`load_config_from` の match に1行追加):

```
("route", "voice_name") => { if let Some(s) = parse_string(value) { if valid_voice_name(&s) { cfg.voice_name = s; } } }
```

`valid_voice_name` は次を弾く。弾いた場合は既定値のまま(他のキーと同じく黙って無視)。

| 条件 | 理由 |
|---|---|
| `-` で始まる | `Command::new("say").arg("-v").arg(name)` の `name` が `say` のオプションとして解釈される。シェルを介さないので注入は起きないが、`-o` 等を入れられると挙動が変わる |
| `"` を含む | `save_config_to` は `voice_name = "{}"` と素の `format!` で書き出す。引用符が混ざると次回のパースが壊れる(既存の `google_maps_api_key` と同じ弱点なので、新規キーでは最初から塞ぐ) |
| 制御文字(`\n` 等)を含む | 同上。1行1キーの前提が崩れる |

空文字自体は有効値として通す(`parse_string("\"\"")` は `Some("")` を返すのでキー欠落と区別できる)。

保存側は `[route]` ブロックに `voice_name = "{}"` を1行、`voice_speak_local` の次に足す。

`src/config.rs` は「crate内の他モジュールを参照しない・std だけで単体コンパイルできる」方針
(ファイル冒頭のコメント)なので、候補一覧の列挙(`say` の起動)はここには置かない。config が持つのは
文字列1つだけで、値がその端末に実在するかの判定は一切しない。

### 3.2 候補一覧: 動的列挙を採る

実行時に `say -v '?'` を実行して `ja` 始まりのロケールの音声だけを拾う。固定候補(Kyoko/Otoya)は
採らない。

| 観点 | 動的列挙 | 固定候補 |
|---|---|---|
| 実態との一致 | インストール済みのものだけが出る | 端末に無いものが選べてしまう |
| 選べなかった時の症状 | 起こらない | **無音**。`say` は exit 1 で失敗するが `spawn` の戻り値を捨てているので何も起きない(§3.5) |
| 新しい音声への追随 | 自動 | 追随しない |
| コスト | `say -v '?'` 1回(実測 0.48秒 / 143音声)+パース | ゼロ |

決め手は2つ。

- 固定候補の失敗モードが無音であること。無音は「機能が壊れている」としか見えないので、
  今回の変更でむしろ体験を悪くする。
- 現行のmacOSが配っている音声名は `Kyoko` のような単純な名前だけではない。この開発環境の
  実出力には `Eddy (ドイツ語（ドイツ）)` のように**空白・半角括弧・全角括弧を含む名前**が
  79件ある。日本語ロケールにも同じ系統の名前(`Eddy (日本語（日本）)` 等)が入りうる。
  固定リストで書き当てられる形ではない。

実測で確認済みの事実:

| 確認 | 結果 |
|---|---|
| `say -v '?'` の所要時間 | 0.48秒(user 0.24s / 143行) |
| この環境の `ja_JP` | `Kyoko` のみ |
| 空白・括弧入りの名前を1引数で渡せるか | 渡せる(`say -v "Eddy (英語（アメリカ）)" -o /tmp/t.aiff hello` が exit 0・45KBの音声を生成) |
| 存在しない音声名 | stderr に ``Voice `NoSuchVoice' not found.`` を出して **exit 1**(§3.5 で扱う) |
| `-v` 無し | exit 0(OS既定音声) |

#### 3.2.1 取得のタイミング

`say` の起動を毎フレームや毎回やらせない。`OnceLock` で1回だけ実行して以後は使い回す。

```
static JA_VOICES: OnceLock<Vec<String>> = OnceLock::new();

pub(crate) fn japanese_voices() -> &'static [String];  // get_or_init。初回だけ say を起動する
pub(crate) fn warm_voice_list();                       // japanese_voices() を叩くだけのスレッドを1本投げる
```

`warm_voice_list()` は `Focus::Settings` に入った時点(`src/ui.rs` の `MenuAction::Settings` と
`,` キーの2箇所)で呼ぶ。設定画面を開いてから声の行(最終行)までカーソルを下ろすのに 0.48秒を
下回ることはまずないので、実際には `japanese_voices()` が待たされる場面はほぼ発生しない。
最悪でも一度だけ 0.5秒 描画が止まるだけで、以降はキャッシュを返す。

設定画面を開かない実行では `say` を起動しない(音声案内をONにしていても、読み上げ自体は
`cfg.voice_name` を渡すだけで一覧は要らない)。

非macOSでは `japanese_voices()` は常に空を返す(`speak_local` と同じ `#[cfg(target_os = "macos")]`
で分ける)。

#### 3.2.2 パース規則

`say -v '?'` の1行は次の形。

```
Kyoko               ja_JP    # こんにちは! 私の名前はKyokoです。
Eddy (英語（アメリカ）)     en_US    # Hello! My name is Eddy.
```

名前は空白を含みうるので、列位置での切り出しはしない。次の手順で分ける。

1. 行を最初の `#` で切り、左側だけを見る(サンプル文に `#` が入っていても影響しないよう、
   分割は最初の1個のみ)。
2. 左側を末尾から見て、最後の空白区切りトークンをロケールとする(`rsplit_once(char::is_whitespace)`)。
3. 残りを `trim()` したものが音声名。空になった行は捨てる。
4. ロケールの `-` を `_` に正規化したうえで `ja` 始まりなら採用(`ja_JP` / `ja-JP` の両方に耐える)。
5. 名前が重複したら先勝ちで1つに畳む。並びは `say` の出力順(アルファベット順)のまま。

この処理は `pub(crate) fn parse_say_voices(stdout: &str) -> Vec<String>` という純関数に切り出す。
`say` を起動できない環境でも単体テストできるようにするため。

#### 3.2.3 表示名

一覧の表示は左袖(幅28セル・`src/ui.rs:545`)に出る。`fit_cells`(`src/main.rs:392`)は溢れた分を
黙って切るので、`Eddy (日本語（日本）)` のような名前はそのままだと読めない位置で切れる。

表示のときだけ、末尾の括弧グループを落とす。

```
pub(crate) fn display_voice_name(raw: &str) -> &str
```

- 末尾が `)` で、対応する ` (` が名前の先頭以外にあるなら、そこから手前を返す(`Eddy (日本語（日本）)` → `Eddy`)。
- 括弧を落とした結果が空になる場合は落とさない。
- 括弧が無ければそのまま。

一覧は全件が日本語ボイスなので、括弧内のロケール表記は情報を持たない。設定に保存する値は
必ず**加工前の生の名前**で、表示だけを短くする。

### 3.3 設定画面への項目追加

既存の選択式UI(`Focus::SettingsPick` によるアコーディオン展開)にそのまま乗せる。
`ColorPick` / `ShapePick` は地図に重ねる別レイヤの独立ピッカーで、設定画面の中で使う仕組みでは
ないため踏襲しない。設定画面の3択以上は `SettingsPick` に統一されている(`src/settings.rs:27-34`)。

| 変更点 | 内容 |
|---|---|
| 行番号 | **27**(末尾に追加)。`SETTINGS_ROW_COUNT` を 27 → **28** |
| ラベル | `format!("{} 読み上げの声 {}", arrow(27), display_voice_name(&cfg.voice_name))`。`voice_name` が空なら `システム既定` |
| 説明文 | `setting_description(27)` に専用の arm を足す(§3.3.2) |
| `is_pickable` | 27 を true にする |

末尾に足すのは `src/settings.rs:26` のコメント(「項目を増やすときは必ず末尾に足す」)に従うため。
音声関連の3項目(21 ルート音声案内 / 23 音声をこの端末でも再生 / 27 読み上げの声)が離れて並ぶ
ことになるが、既存も 21 と 23 の間に 22(道路交通量)が入っており、この画面はもともと機能別に
並んでいない。並べ替えは `src/ui.rs` の生の数値比較(`set_sel == 6` / `== 17` と match の各 arm)を
全部触ることになるので、この変更ではやらない(§9 に残す)。

#### 3.3.1 候補リストの組み立て

他の項目と違い、候補が実行環境と現在値の両方に依存する。`CHOICES` の静的テーブルには載せず、
`idx == 16`(中心十字の色)と同じく特別扱いの分岐で組み立てる。

`voice_choices(cfg) -> Vec<(String /*保存値*/, String /*表示*/)>`:

| 位置 | 値 | 表示 | 条件 |
|---|---|---|---|
| 0 | `""` | `システム既定` | 常に |
| 1.. | 生の音声名 | `display_voice_name` | `japanese_voices()` の各件 |
| 末尾 | `cfg.voice_name` | `<表示名> (未検出)` | `cfg.voice_name` が空でなく、かつ列挙結果に無く、**かつ列挙結果が空でない**場合 |

末尾の1件は、音声をアンインストールした/config を手で書いた場合に**現在値が一覧から消えて
黙って別の声に置き換わる**のを防ぐためのもの。選択位置が求まらない値を 0 に丸めると、
Enterを押していないのに設定が変わったように見える。

「列挙結果が空でない場合」という条件を付けるのは、非macOS や `say` の起動失敗のときに
`(未検出)` と出すと嘘になるため。列挙できていない状態と、列挙できたうえで見つからない状態は
区別する。

#### 3.3.2 `pick_labels` の戻り値型を変える

現行は `pick_labels(idx: usize) -> Vec<&'static str>` で、静的テーブルからしかラベルを作れない。
声の候補は実行時に決まるので、次のように変える。

```
pub(crate) fn pick_labels(idx: usize, cfg: &Config) -> Vec<String>
```

呼び出し側の影響は2箇所だけで、どちらもコストは無い。

| 箇所 | 現在 | 変更後 |
|---|---|---|
| `src/settings.rs` `settings_rows` | `labels.iter().map(\|l\| format!("    {l}"))` | そのまま動く |
| `src/ui.rs:2099` | `settings::pick_labels(idx).len()` | 引数に `&cfg` を足すだけ |

`OnceLock` から `&'static str` を取り出して既存シグネチャを保つ手もあるが、`(未検出)` 付きの
ラベルが `cfg` に依存するため結局は所有文字列が要る。型を素直にする方を採る。

`pick_current` / `apply_pick` は既に `cfg` を受け取っているので、内部で `voice_choices` を組み立て
れば足りる(`apply_pick` は `&mut Config` を持つので、リストを作ってから代入する順にする)。

なお `pick_labels` はアコーディオン展開中は毎フレーム呼ばれる。`japanese_voices()` はキャッシュ
済みのスライスを返すだけなので、毎フレームのコストは高々10件程度の `String` 生成で、既存の
`settings_rows` が毎フレームやっている `format!` と同じ水準に収まる。

#### 3.3.3 説明文

```
27 => "読み上げの声: ルート音声案内をこの端末(macOSのsay)で読み上げるときの声。Enterで一覧を開いて選択(Spaceで試聴)。インストール済みの日本語音声だけが並ぶ。web版(ブラウザ)の声はブラウザ側が自動で選ぶのでここでは変わらない",
```

`setting_description` の `_ =>` は Google APIキー(17)へのフォールバックなので、**catch-all より前に
arm を足す**。足し忘れると27行目にGoogle APIキーの説明が出る。

非macOSでは末尾を「この端末(macOS以外)では読み上げ自体が動かないため効果は無い」に差し替える。
`idx == 11`(画像表示)が端末対応の有無で文言を出し分けている前例と同じ形にする。行そのものを
隠す案は採らない。行数が環境で変わると `SETTINGS_ROW_COUNT` が定数で持てなくなる。

### 3.4 試聴(サンプル再生)

声は名前を見ても判断できない。ヘルメットを被って走行中に聞くものなので、静かな部屋で名前を
選んで終わりにすると、実際に必要な「風切り音の中で聞き取れるか」が全く確認できないまま設定が
確定する。設定画面で一度は音を出せるようにする。

| 操作 | 動作 |
|---|---|
| `SettingsPick(27)` で `Space` | カーソル位置の声で試聴する。選択は確定せず、一覧も閉じない |
| `SettingsPick(27)` で `Enter` | 確定・保存・一覧を閉じたうえで、確定した声で1回鳴らす |

`Space` は現在 `SettingsPick` では未使用(`src/ui.rs:2100-2115` の match は Up/Down/Enter/Esc のみで、
残りは `_ =>` で何もしない)。`Focus::Settings` 側の `Enter | Char(' ')` とは別のフォーカスなので
衝突しない。

- サンプル文は実際の案内文と同じ調子にする: `"300メートル先、左折です"`。声によって
  読み間違いの傾向が変わるため(§8)、本番と同じ語彙で確認できる方がよい。
- 再生は**ローカル固定**。`voice::speak()` ではなく `speak_local_with(name, text)` を直接呼ぶ。
  設定しているのは `say` の声なので、OSC経由でブラウザに流す意味が無い。
- そのため web版(ttyd)越しに設定している場合、音はMac本体から出て手元では聞こえない。
  ステータス行に `試聴: <名前>(この端末で再生)` と出して、鳴っていないのではなく別の場所で
  鳴っていることが分かるようにする。
- `cfg.voice_speak_local` が OFF でも試聴は鳴らす。試聴は「その声が使えるか」の確認であって、
  案内の再生経路の設定とは別物。

`Enter` で確定した声が実際には使えない(アンインストール済み等)場合を拾うため、確定時の再生だけは
`spawn` して捨てるのではなく、終了コードを別スレッドで待って結果を返す。

```
pub(crate) fn preview_voice(voice: &str) -> std::sync::mpsc::Receiver<Result<(), String>>
```

`route::trigger_turn_points`(`src/route.rs:207-213`)と同じ、mpsc の受信側を返してUIループで
ポーリングする形にする。exit 1 が返ったら `この声は再生できませんでした: <名前>` をステータス行に
出す。標準エラーは捨てる(§3.5)。

### 3.5 読み上げ実行の変更

現行(`src/voice.rs:117-122`):

```
let _ = std::process::Command::new("say").arg("-v").arg("Kyoko").arg(text).spawn();
```

変更点は2つ。

**(a) 音声名を設定から取る。空なら `-v` ごと省く。**

引数の組み立てを純関数に切り出して、macOS以外でも単体テストできるようにする。

```
pub(crate) fn say_args(voice: &str, text: &str) -> Vec<String>
// voice が空          -> [text]
// voice が空でない     -> ["-v", voice, text]
```

`speak(text, native_local)` は `cfg` を持っていないので、`speak(text, native_local, voice: &str)` に
引数を1つ足す。呼び出しは `src/ui_helpers.rs:61` の `maybe_speak_turn` 1箇所だけなので、
`voice::speak(&phrase, cfg.voice_speak_local, &cfg.voice_name)` に変える。

**(b) 子プロセスの stderr を捨てる。**

存在しない音声名を渡すと `say` は stderr に ``Voice `X' not found.`` を出す(実測)。現行は
`Stdio` を指定していないので親の stderr を継承する = **代替スクリーンで描画中のTUIに
そのまま文字列が流れ込む**。音声名が固定だったこれまでは起きなかったが、設定できるように
した以上は必ず起きうる。`.stdout(Stdio::null()).stderr(Stdio::null())` を付ける。

(なお `spawn` して `wait` しない現行方式は子プロセスがゾンビとして残る。走行中に案内が続くと
その分溜まる。この設計の範囲外だが、`say_args` 周りを触るついでに気付いた点として記録しておく。)

## 4. web版側

### 4.1 ボイス一覧の確定

`web/touch-overlay.js` にボイス解決を1組足す。`speechSynthesis.getVoices()` は初回に空配列を
返す実装(iOS Safari含む)があるため、`voiceschanged` とリトライの両方で確定させる。

```
var jaVoices = null;      // 確定済みの ja ボイス配列(null = 未確定)
var chosenVoice = null;   // 実際に u.voice へ入れるもの(null = 見つからない/未確定)

function resolveVoices()   // getVoices() を引いて ja を絞り、chosenVoice を決める。成否を返す
function bindVoiceList()   // init から1回。下の順で確定を試みる
```

`bindVoiceList()` の確定手順:

1. その場で `resolveVoices()`。取れたら終わり。
2. `speechSynthesis.addEventListener('voiceschanged', resolveVoices)` を張る。
   (`onvoiceschanged` への直接代入は他スクリプトの上書き事故があるので `addEventListener` を使う)
3. `[100, 300, 700, 1500, 3000]` ms のタイマーでも `resolveVoices()` を試す。
   `voiceschanged` を一度も発火させないWebKitビルドがあるため、イベント待ちだけにしない。
   リトライ配列で粘る形は `bindSoundOsc` / `bindVoiceGuideOsc`(`web/touch-overlay.js:156,208`)と
   同じ書き方に揃える。
4. どの経路でも確定できなければ `chosenVoice = null` のまま。**現状の挙動(lang のみ)へ落ちる**。

`getVoices()` が空配列を返した場合は「ボイス無し」と確定させない(未確定のまま次の機会を待つ)。
空を確定扱いにすると、ページ読み込み直後の1回で `ja ボイスは無い` と決めつけてしまう。

### 4.2 選択規則

`speechSynthesis.getVoices()` の並び順は仕様で決まっていない。「最初に見つかったもの」だと
端末やブラウザのバージョンで結果が変わり、低品質なcompactボイスを引くこともある。優先順を
明示する。

ja 判定は `/^ja(-|_|$)/i.test(v.lang || '')`(`ja-JP` / `ja_JP` / `ja` を通す)。その中で:

| 順 | 条件 | 理由 |
|---|---|---|
| 1 | `localStorage['termmap.voiceName']` と名前が一致(完全一致 → 前方一致、大小無視) | 明示指定の逃げ道(§4.5) |
| 2 | `v.default === true` | 端末で日本語の既定に設定されている声。ユーザーが普段聞いている声 |
| 3 | `v.localService === true` | 端末内で合成できる。remote ボイスは通信に依存し、圏外や不安定な回線で無音になりうる。走行中に使うものなので優先度は高い |
| 4 | 配列の先頭 | 最後の手段 |

ja ボイスが1件も無ければ `chosenVoice = null`。

### 4.3 `speakVoiceGuide` の変更と #86 対策候補F の解決

現行(`web/touch-overlay.js:192-206`)は `u.lang = 'ja-JP'` だけで `u.voice` を設定していない。
iOSは日本語ボイスをオンデマンドで追加する仕組みなので、lang だけの指定では合成器が選べず
無音で終わることがある。これが #86 の候補F。

```
var u = new SpeechSynthesisUtterance(text);
u.lang = 'ja-JP';                       // 従来どおり残す(voice が無い時のフォールバック)
if (!chosenVoice) { resolveVoices(); }  // 発話時にもう一度だけ引く(遅延確定の取りこぼし対策)
if (chosenVoice) { u.voice = chosenVoice; }
window.speechSynthesis.speak(u);
```

守ること:

- **ボイス確定を待って発話を遅らせない。** 曲がり角の案内は遅れて出たら価値が無い。
  未確定なら未確定のまま lang だけで投げる。今より悪くなる経路を作らない。
- `u.lang` は `u.voice` を入れた場合も残す。両方入れて害は無く、`voice` の代入に失敗する実装が
  あっても従来の挙動に落ちる。
- OSC 9998 のペイロード形式(base64のUTF-8文字列)は変えない。Rust側は無改造で、
  古いtermmap + 新しいoverlay / 新しいtermmap + 古いoverlay のどちらの組み合わせでも壊れない。

#86 の他の未実装対策(3: touchend/click での解錠、4: 解錠用Utteranceの実体化、6: onstart/onerror の
診断表示)はこの設計では扱わない。あちらは実機での切り分け結果を見てから判断する項目で、
声の選択とは独立している。ただし対策4を後で入れるときは、解錠用のUtteranceにも `chosenVoice` を
使うこと(声を指定した発話で解錠しないと、解錠できても本番の発話で別の合成器が起きる可能性が残る)。

### 4.4 診断入口

#86 の実機切り分け(あちらの §4 手順4)は Safari の Web インスペクタから `window.__termmapTouch` を
叩く手順になっている。同じ場所からボイスの状態を見られるようにする。

```
__termmapTouch.getVoiceInfo()
// { resolved, chosen: {name, lang, localService, default}|null,
//   ja: [{name, lang, localService, default}...], total: <getVoices().length>,
//   override: localStorage['termmap.voiceName'] || null }
__termmapTouch.setVoiceName('Kyoko')   // localStorage に保存して即再解決。戻り値は getVoiceInfo()
__termmapTouch.setVoiceName(null)      // 解除
```

これで実機に接続したとき「そもそもjaボイスが0件なのか」「選んでいるが鳴らないのか」を
1コマンドで切り分けられる。#86 の残課題の確認コストがそのまま下がる。

### 4.5 明示指定の逃げ道

選択UIは作らないが、上書きはできるようにする。`localStorage['termmap.voiceName']` を
§4.2 の優先順1として読む。UIもボタンも増えず、`setVoiceName()` から設定できる。

これで足りると判断する理由: 端末に入っている日本語ボイスは1〜2件のことが多く、自動選択が
外れる場面自体が稀。稀な場面のためにUIを常設するより、外れたときに直せる手段がある方が
釣り合う。

### 4.6 採らなかった案

| 案 | 採らない理由 |
|---|---|
| ボタンバー(`buildBar`)に声の選択ボタンを足す | バーは既に14個で、幅390pxの端末で1個あたり約28px。ソース内のコメント(`web/touch-overlay.js` のCSS定義部)にも「これ以上ボタンが増えるなら2段組みへの変更が要る」と書いてある。走行中に触るものではない設定に、常時表示の1枠と2段組み化を払う価値は無い |
| モーダルの設定画面をJS側に新設する | 得られるのは「候補1〜2件から選ぶ」だけ。実装量(モーダル・永続化・端末領域とのイベント競合の再調整)に見合わない |
| Rust の設定値をOSCでブラウザへ渡す | §5.2 |

## 5. macOSローカルとweb版の関係

### 5.1 設定を統合しない

2つのTTSは名前空間が違う。

| | macOSローカル | web版 |
|---|---|---|
| エンジン | `say` コマンド | ブラウザの Web Speech API |
| 名前の例 | `Kyoko`, `Eddy (日本語（日本）)` | `Kyoko`, `O-ren`, `Hattori`(端末・ブラウザで異なる) |
| 一覧の取得元 | `say -v '?'`(Rustから取れる) | `speechSynthesis.getVoices()`(ブラウザ内でしか取れない) |
| 誰の端末の話か | termmap を動かしているMac | 画面を見ているスマホ/PC |

重なるのは偶然一致する名前だけで、共通の「声」という概念は無い。同じ設定項目に見せると
「Macで設定したのにiPhoneで変わらない」という説明のつかない状態になる。

したがって:

- 設定画面(`,`)の「読み上げの声」は **macOSの `say` 専用**。説明文にそう明記する(§3.3.2)。
- web版は設定を持たず自動選択。ユーザーに選ばせる操作は存在しない。

「音声をこの端末でも再生」(行23)が既に「この端末 = Mac本体」の意味で使われているので、
「読み上げの声」も同じ土俵の設定として読める。

### 5.2 将来やるなら(今回はやらない)

web の声も明示指定したくなった場合の形だけ決めておく。実装はしない。

- 設定は共有せず、**別キー** `[route] web_voice_name`(既定 `""` = 自動)を新設する。
  `voice_name` を流用しない。名前空間が違うものを1つの値に押し込むと、どちらかで必ず外れる。
- 受け渡しは OSC 9996(9997=軸モード / 9998=音声 / 9999=効果音 の隣で未使用)。
  ペイロードは `base64(UTF-8の名前)`。ブラウザ側は §4.2 の優先順1(localStorage)と同じ扱いにする。
- 前提として、ブラウザ側の候補一覧をRustが知る手段が要る(ブラウザ → Rust はマーカーペースト
  経由。`sendMarkerPaste` と `src/dragmode.rs` の受信側と同じ仕組みで実装可能)。ここまでやって
  初めて「Macの設定画面でスマホの声を選ぶ」が成立する。相応の量になるので、実際に不便だと
  分かってから着手する。

## 6. 影響範囲

| ファイル | 変更 |
|---|---|
| `src/config.rs` | `voice_name` フィールド追加 / 既定値 / パース + `valid_voice_name` / 保存 / テスト |
| `src/voice.rs` | `say_args` / `speak_local_with` / `speak` の引数追加 / stderr破棄 / `japanese_voices` / `warm_voice_list` / `parse_say_voices` / `display_voice_name` / `preview_voice` / テスト |
| `src/settings.rs` | `SETTINGS_ROW_COUNT` 27→28 / 行27のラベル / `is_pickable` / `pick_labels` のシグネチャ変更 / `pick_current` / `apply_pick` / `setting_description(27)` / `voice_choices` / テスト |
| `src/ui.rs` | `Focus::Settings` 進入時に `warm_voice_list()`(2箇所) / `pick_labels` 呼び出しの引数追加(`:2099`) / `SettingsPick` に `Space`=試聴の分岐 / `Enter` 確定後の試聴と結果ポーリング |
| `src/ui_helpers.rs` | `maybe_speak_turn` の `voice::speak` 呼び出しに `&cfg.voice_name` を渡す |
| `src/ui_status.rs` | `Focus::SettingsPick(27)` のヒント文を分ける(`Space=試聴` を出す) |
| `web/touch-overlay.js` | ボイス解決一式 / `speakVoiceGuide` の `u.voice` 指定 / `init` から `bindVoiceList()` / `__termmapTouch` に `getVoiceInfo`・`setVoiceName` / `OVERLAY_VERSION` を上げる |
| `docs/MANUAL.md` | 設定項目の一覧(232行目・239行目付近)に「読み上げの声」を追記 |
| `README.md` | `[route] voice_name` を設定キー一覧に追記 / ルート音声案内の説明に声の選択を1行 |

変更不要と確認したもの:

- `src/dragmode.rs` … `Focus::SettingsPick(_)` は既にワイルドカードで `(Nothing, Cursor)`。
  行27でも同じ挙動でよい。
- `src/ui_gutter.rs` … `settings_rows` を呼ぶだけで、行数・内容には触れていない。
- OSC 9998 の形式 … 変えない(§4.3)。
- `scripts/build-web-index.sh` … `touch-overlay.js` を埋め込むだけ。

## 7. テスト

### 7.1 Rust

`parse_say_voices`(純関数・macOS以外でも走る):

- 実出力を模した文字列から `ja_JP` の行だけを拾う
- 名前に空白・半角括弧・全角括弧が入っていても壊れない(`Eddy (日本語（日本）)`)
- サンプル文に `#` が含まれる行でも名前を誤らない
- `ja-JP` 表記も拾う / `java_XX` のような紛らわしい語を拾わない
- 空行・末尾改行なし・空文字入力で panic しない
- 同名の重複を1件に畳む・出力順を保つ

`display_voice_name`:

- `Eddy (日本語（日本）)` → `Eddy` / `Kyoko` → `Kyoko`
- 先頭が括弧の異常な名前で空文字を返さない

`say_args`:

- 空の音声名 → `["300メートル先、左折です"]`(`-v` が付かない)
- `Kyoko` → `["-v", "Kyoko", "..."]`
- 空白入りの名前が1要素のまま(分割されない)

`config`:

- `voice_name` の save → load 往復
- キーが無い設定ファイルで既定 `"Kyoko"` になる(後方互換)
- `voice_name = ""` が空文字として保持される(キー欠落と区別される)
- `-` 始まり / `"` を含む / 制御文字を含む値は無視して既定のまま
- 既存の `config_roundtrip` 系テストのフィクスチャに `voice_name` を追加

`settings`:

- `is_pickable(27)` が true(既存テストの pickable 配列に 27 を足す)
- `SETTINGS_ROW_COUNT == 28` / `settings_rows` の行数が一致(既存テストがそのまま効く)
- 行27のラベルが `▸ 読み上げの声 ...` で、22/24/25/26 の並びが動いていない(既存の回帰テストを流用)
- `pick_labels(27, cfg)` の先頭が `システム既定`
- `cfg.voice_name` が列挙結果に無い かつ 列挙結果が空でない → 末尾に `(未検出)` 付きで並ぶ
- 列挙結果が空のときは `(未検出)` を出さない
- `pick_current` → `apply_pick` の往復 / 範囲外 `sel` を無視する
- `setting_description(27)` が 17(Google APIキー)のフォールバックと違う文字列を返す

補足: 既存の `setting_description_covers_every_known_row_distinctly` は 0〜16 と 18〜20 しか
見ていない。21〜27 まで広げると今回追加分の取りこぼしも拾えるようになる(21〜26 は既に
それぞれ別の説明文を持っているので、広げても既存は通る)。

`japanese_voices` / `preview_voice` は実際に `say` を起動するので、CIや非macOSで落ちない形にする。
実行環境依存の assert は書かず、「非macOSで空を返す」ことだけを確認する。

### 7.2 web

自動テストの枠組みが無いので手順で確認する(既存の web 側変更と同じ扱い)。

1. `node --check web/touch-overlay.js`(構文)
2. Mac のブラウザで開き、コンソールで `__termmapTouch.getVoiceInfo()` … `resolved: true` と
   `chosen.name` が出ること
3. `__termmapTouch.speakVoiceGuide(btoa(unescape(encodeURIComponent('300メートル先、左折です'))))`
   で選ばれた声で鳴ること
4. `__termmapTouch.setVoiceName('<別のja音声名>')` → 3 を再実行して声が変わること
5. iPhone を Mac に繋いで Web インスペクタから 2〜4 を実施。`ja: []` なら端末側に日本語ボイスが
   無い(#86 の候補F が当たり)と確定できる
6. `speechSynthesis` を持たない環境の擬似確認(コンソールで一時的に隠す等)で例外が出ないこと

## 8. 制約・既知のリスク

- **声を変えると読み間違いの傾向が変わる。** `turn_phrase()` の「ぶんきを左です」というひらがな
  表記(`src/voice.rs:95-98`)は Kyoko が「分岐」を誤読するのを避けるための調整で、Kyoko で実機
  確認したもの。別の声では別の語が誤読される可能性がある。事前には潰せないので、試聴を
  実際の案内文で行う(§3.4)ことで気付けるようにするに留める。
- **選んだ声が後から消えると走行中は無音になる。** 設定画面を開けば `(未検出)` で気付けるが、
  走っている最中に検知して代替する仕組みは入れない。案内のたびに終了コードを待つと読み上げ
  経路がブロッキングになるため(`sound.rs` と同じ非ブロッキング方針を崩さない)。
- **`say -v '?'` は 0.48秒かかる。** 設定画面を開いた時に1回だけ背景で走らせる(§3.2.1)。
  最悪の場合、開いた直後に最終行まで飛んで Enter を押すと一度だけ描画が止まる。
- **web は端末に日本語ボイスが無ければ改善しない。** iOSはオンデマンド追加なので、
  設定アプリ → アクセシビリティ → 読み上げコンテンツ → 声 での追加が要る場合がある
  (#86 の §3-G に手順あり)。コード側からは追加できない。
- **web の自動選択はユーザーには見えない。** どの声が選ばれたかを画面に出す手段が無い
  (出すならバーかステータス表示の新設が要る)。`getVoiceInfo()` で確認できることに留める。
- 非macOSでは設定行は出るが効果は無い(説明文で明示する)。

## 9. 実装の順序

先に進めた分だけで動作確認できる並びにする。

| 段 | 内容 | ここまでで確認できること |
|---|---|---|
| 1 | `config.rs`(フィールド・パース・保存・テスト) | config.toml の往復 |
| 2 | `voice.rs`(`say_args` / stderr破棄 / `speak` 引数追加 / `ui_helpers` 側の呼び出し) | config に書いた声で読み上げが変わる(設定画面はまだ無い) |
| 3 | `voice.rs`(`parse_say_voices` / `japanese_voices` / `display_voice_name`) | 一覧の列挙とパース(単体テスト) |
| 4 | `settings.rs` + `ui.rs`(行追加・ピッカー・`warm_voice_list`) | 設定画面から選べる |
| 5 | `preview_voice` + `ui_status.rs`(試聴と失敗表示) | 試聴・失敗の通知 |
| 6 | `web/touch-overlay.js`(ボイス解決・`u.voice`・診断) | #86 対策候補F |
| 7 | `docs/MANUAL.md` / `README.md` | — |

6 は 1〜5 と独立しているので、先に出しても構わない(#86 の実機確認を早く回したい場合)。

## 10. この設計でやらないこと

- web版の声の選択UI(§4.6)
- Rust設定とブラウザの声の連携(§5.2)
- 日本語以外の音声を選べるようにすること。候補は `ja` に絞る。ただし `config.toml` を手で書けば
  任意の名前を入れられ、その値は `(未検出)` 扱いにならず一覧に出る(列挙にある限り)ので、
  実質の逃げ道はある
- 設定画面の行番号を生の数値で持っている構造の是正(`src/ui.rs` の `set_sel == 6` / `== 17` と
  `settings.rs` の `idx` テーブルが手作業で同期されている)。今回は末尾追加で済むので触らない。
  中間に項目を挿したくなった時点で、行を enum 化する作業を別に立てる
- `speak_local` が子プロセスを `wait` せずゾンビを残す点(§3.5 の補足)
- #86 の対策3・4・6(実機確認の結果を待つ)
