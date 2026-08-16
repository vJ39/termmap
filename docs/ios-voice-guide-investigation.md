web版(ttyd + xterm.js)のルート音声案内が、Macのブラウザでは聞こえるのに iPhone のブラウザでは
聞こえない件の原因調査。タスク#80で `web/touch-overlay.js` に入れた対策(commit `58d6737`:
最初のタッチでの解錠 + Chrome 15秒バグ対策の keep-alive)だけでは iPhone が直らなかったため、
iOS 固有の追加原因を洗い出す。実装修正はこの文書の範囲外。

対象コード: `web/touch-overlay.js` の `unlockSpeechSynthesisOnce` / `speakVoiceGuide` /
`bindVoiceGuideOsc` / `bindGestures` / `buildBar` / `inTerminal`、`src/voice.rs` の `speak_web`。

## 1. 切り分け済みの範囲

Macのブラウザで聞こえている事実から、以下は正常と確定できる。iPhone でも同じコードが動くため、
残る原因はすべてブラウザ(WebKit)側の挙動に絞られる。

| 経路 | 判定 | 根拠 |
|---|---|---|
| `voice.rs` の案内文生成・閾値判定 | 正常 | 端末非依存。Macブラウザで案内が出ている |
| OSC 9998 の送出 | 正常 | `speak_web()` は `native_local` に関係なく常に `print!` する(`src/voice.rs:130-135`) |
| xterm.js の `registerOscHandler(9998, ...)` 登録 | 正常 | 同じ ttyd セッション・同じスクリプト |
| base64 → UTF-8 デコード | 正常 | `speakVoiceGuide` 冒頭(`web/touch-overlay.js:165-171`)は端末非依存 |
| HTTPS / Secure Context | 影響なし | §5 参照 |

`speak_web()` は設定の「端末別ON/OFF」に関係なく無条件で OSC を書く。ON/OFF が効くのは
macOS ローカルの `say`(`native_local`)だけなので、web側が設定で黙る経路は無い。

## 2. 原因候補の一覧

| # | 候補 | 確度 | コードで確定できるか |
|---|---|---|---|
| A | keep-alive の `pause()`/`resume()` を iOS にも適用している | 高 | 実装は確定。iOS での破壊的挙動は要実機確認 |
| B | ボタンバーのタップでは解錠関数が一度も呼ばれない | 高 | コードで確定 |
| C | 端末のサイレントスイッチ / 着信音量 | 高 | 実機のみ |
| D | 解錠を `touchstart` にしか結び付けていない | 中 | コードで確定 |
| E | 解錠用 Utterance が `volume=0` かつ空白文字のみ | 中 | コードで確定。iOS での成否は要実機確認 |
| F | `voice` 未指定で `lang='ja-JP'` のみ | 中〜低 | コードで確定。日本語ボイスの有無は実機依存 |
| G | iOS Safari のサイト別「自動再生」設定 | 低 | 実機のみ |
| H | ページの非アクティブ化 / 自動ロック | 低 | 実機のみ |
| I | OSC ハンドラ登録リトライの取りこぼし | 低 | 効果音が鳴るなら除外できる |

## 3. 候補ごとの詳細

### A. keep-alive の pause()/resume() を iOS にも適用している(確度: 高)

`bindVoiceGuideOsc()` の末尾(`web/touch-overlay.js:191-198`)。

```js
setInterval(function () {
  if (window.speechSynthesis.speaking) {
    window.speechSynthesis.pause();
    window.speechSynthesis.resume();
  }
}, 5000);
```

これは Chrome が約15秒で内部停止する既知バグ向けの回避策で、一般には Chrome 判定で囲って
適用する。ここにはブラウザ判定が無く、iOS Safari でも同じように 5秒ごとに走る。

WebKit 側で知られている挙動が2つ噛み合うと、iPhone だけ全く鳴らない状態になる。

- iOS の `speechSynthesis.pause()` / `resume()` は実装が不安定で、`pause()` を挟むと
  発話中の Utterance が再開されずキューごと止まることがある。
- iOS では発話完了後も `speechSynthesis.speaking` が `true` のまま戻らないケースが知られている。

`setInterval` は `init()`(`web/touch-overlay.js:717-725`)の時点、つまりページ読み込み直後から
回り始める。最初のタッチで投げる解錠用 Utterance(§3-E)で `speaking` が `true` になり、それが
戻らないと、以降 5秒おきに `pause()`/`resume()` が延々と呼ばれ続ける。その状態で案内の
`speak()` が来ても、キューに積まれたまま発声されない。

タイミングとしても符合する。#80 以前は解錠が無くて鳴らず、#80 で解錠と同時にこの keep-alive が
入ったため、Mac(`speaking` が正しく戻る)は直り、iOS は解錠が効いても keep-alive 側で潰される。

対策案: keep-alive を Chrome 系だけに限定する。あわせて `speaking` に依存しない形
(直前に `speak()` した時刻を持って一定時間内だけ回す等)へ変える。

### B. ボタンバーのタップでは解錠関数が一度も呼ばれない(確度: 高)

`unlockSpeechSynthesisOnce()` の呼び出しは2箇所だけで、どちらもボタンバーを通らない。

| 呼び出し箇所 | 条件 |
|---|---|
| `web/touch-overlay.js:491`(document の `touchstart`) | 直前の `:482` で `if (!inTerminal(e.target)) { return; }` |
| `web/touch-overlay.js:541`(document の `mousedown`) | `if (sawTouch \|\| !inTerminal(e.target)) { return; }` |

`inTerminal()`(`web/touch-overlay.js:374-378`)は、ボタンバー上の要素を明示的に除外する。

```js
if (node.closest('#' + BAR_ID)) { return false; } // ボタンバー上の操作は対象外
```

一方、各ボタンは自前のハンドラを持ち、そこでは解錠を呼んでいない。

| ハンドラ | 行 | 解錠呼び出し |
|---|---|---|
| `btn` の `touchstart` | `web/touch-overlay.js:676-682` | 無し |
| `btn` の `click` | `web/touch-overlay.js:684-691` | 無し(かつ `sawTouch` が真なら即 return) |

document 側は capture フェーズで先に走るが、`e.target` がボタンなので `:482` で抜け、
`:491` の解錠には到達しない。つまり **`Menu` → `▲▼` → `⏎` だけでルート再生まで進めた場合、
iPhone では解錠が一度も起きない**。地図(端末領域)を1回でもスワイプ/タップしていれば解錠されるので、
操作順によって鳴ったり鳴らなかったりする不安定さがある。

Mac で問題が出ないのは、地図のドラッグが `mousedown` on 端末領域で解錠を通ること、および
デスクトップのブラウザがそもそも解錠を要求しないことの両方による。

対策案: `runButtonAction()` の先頭、またはボタンの `touchstart`/`click` ハンドラ内で
`unlockSpeechSynthesisOnce()` を呼ぶ。`inTerminal()` の判定より前に無条件で呼ぶ形が安全。

### C. 端末のサイレントスイッチ / 着信音量(確度: 高)

iOS では音の鳴動条件が API ごとに違う。`new Audio(...)` によるメディア再生はサイレントスイッチの
影響を受けにくいが、`speechSynthesis` は AVSpeechSynthesizer 経由で、サイレントスイッチが
オンだと無音になる報告が多い。音量も、メディア音量ではなく着信/通知音量側に従う。

これに該当する場合、コード側の修正では直らない。効果音(`playSfx`, `web/touch-overlay.js:123-128`)が
鳴っているかどうかで切り分けられる(§4)。

対策案: 実機でサイレントスイッチを解除し、着信音量を上げて再確認する。コード側では、
案内が発声できたかを `utterance.onstart` / `onerror` で拾って画面に出す診断表示を用意すると、
以降の切り分けが速くなる。

### D. 解錠を touchstart にしか結び付けていない(確度: 中)

iOS の音声解錠は、慣習的に `touchend` と `click` にも結び付ける。`touchstart` だけで解錠が
成立しない iOS バージョンがあるためで、音声ライブラリの多くが3種類とも張っている。

現状 `touchend`(`web/touch-overlay.js:523-532`)には解錠呼び出しが無い。加えて document の
`touchstart` は `:488-489` で `preventDefault()` と `stopPropagation()` を呼ぶため、後続の
`click` も発生しない。解錠の機会が `touchstart` 一択に絞られている。

対策案: `touchstart` / `touchend` / `click` / `keydown` のいずれでも解錠を呼ぶ。

### E. 解錠用 Utterance が volume=0 かつ空白文字のみ(確度: 中)

`web/touch-overlay.js:153-161`。

```js
var u = new SpeechSynthesisUtterance(' ');
u.volume = 0;
window.speechSynthesis.speak(u);
```

WebKit では、読み上げる内容が空白のみの Utterance が即座に完了扱いになり音声セッションを
起こさない、`volume=0` の Utterance が発話としてカウントされない、といった報告がある。
どちらに当たっても解錠は成立しないが、コードは `voiceUnlocked = true` を **speak の成否に
関わらず先に立てる**(`:155`)ため、失敗しても再試行されない。

また §3-A の通り、この Utterance が `speaking` を立てたまま戻らないと keep-alive の暴走を
引き起こす側にもなる。

対策案: 解錠を無音の空発話ではなく、実際に読ませる短い文字列(`lang='ja-JP'`、`volume` は既定)で
行う。`voiceUnlocked` は `utterance.onstart` / `onend` が来てから立てる。解錠前に
`speechSynthesis.cancel()` でキューを空にしておく。

### F. voice 未指定で lang='ja-JP' のみ(確度: 中〜低)

`speakVoiceGuide()`(`web/touch-overlay.js:173-176`)は `u.lang = 'ja-JP'` を設定するだけで、
`u.voice` を明示していない。

iOS は言語ボイスをオンデマンドで追加する仕組みで、日本語ボイスが端末に入っていない場合、
ja-JP の発話が無音で終わることがある。macOS 側は `say -v Kyoko`(`src/voice.rs:121`)を使っている
ことからも分かる通り日本語ボイスが入っているため、この差が出るなら Mac と iPhone で挙動が割れる。
ただし端末の言語設定が日本語であれば通常は日本語ボイスが存在するため、確度は下げてある。

あわせて、iOS の `getVoices()` は初回に空配列を返し `voiceschanged` 後でないと正しい一覧が
取れない。現状はボイスを引いていないのでこの点は直接の原因ではないが、`voice` を明示する
修正を入れる際は `voiceschanged` を待つ実装が必要になる。

対策案: `voiceschanged` を待って `getVoices()` から `lang` が `ja` で始まるボイスを選び、
`u.voice` に設定する。見つからない場合は現状どおり `lang` のみで投げる。

### G. iOS Safari のサイト別「自動再生」設定(確度: 低)

iOS Safari は「あぁ」メニュー → Webサイトの設定、および 設定アプリ → Safari から、サイト単位で
自動再生を制限できる。cloudflared のホスト名は起動のたびに変わるため設定が残りにくいが、
固定ホスト名で運用している場合は残る。コード側では対処できない。

対策案(ユーザー側の確認手順):

1. Safari でページを開いた状態でアドレスバー左の「あぁ」→「Webサイトの設定」を開き、自動再生が
   「すべてのメディアを自動再生」または「音声のあるメディアを停止」以外になっていないか確認する。
2. 設定アプリ → Safari → 詳細 → Webサイトデータ から該当ホストのデータを削除して、設定を初期化する。
3. 設定アプリ → アクセシビリティ → 読み上げコンテンツ → 声 に日本語(Kyoko / Otoya)が入っているか確認する(§3-F)。

### H. ページの非アクティブ化 / 自動ロック(確度: 低)

プレビュー走行中は画面に触らない時間が続く。iPhone の自動ロックが働くか、Safari が
バックグラウンドへ回ると `speechSynthesis` は発話しない。長い区間で後半だけ聞こえない、
といった症状ならこれを疑う。全く聞こえない症状の説明としては弱い。

対策案: 実機確認時は自動ロックを一時的に「なし」にする。

### I. OSC ハンドラ登録リトライの取りこぼし(確度: 低)

`bindVoiceGuideOsc()` は `window.term` を `[0, 100, 300, 700, 1500]` ms の5回で探す
(`web/touch-overlay.js:180-188`)。回線やデバイスが遅く、1500ms 以内に ttyd 側の初期化が
終わらないと登録に失敗する。

ただし効果音側の `bindSoundOsc()`(`:134-142`)も同じリトライ間隔なので、**効果音が鳴っているなら
この候補は除外できる**。

## 4. 実機での切り分け手順

コードからはここまでしか詰められない。次の順で実機を見れば候補を大きく減らせる。

| 手順 | 見るもの | 分岐 |
|---|---|---|
| 1 | iPhone で操作して効果音(メニュー移動時等)が鳴るか | 鳴らない → 音量/サイレント/OSC登録(C, I)。鳴る → speechSynthesis 固有(A, B, D, E, F) |
| 2 | サイレントスイッチを解除し着信音量を上げて再確認 | これで鳴る → C で確定 |
| 3 | 地図(端末領域)を一度スワイプしてからルート再生する | これで鳴る → B で確定 |
| 4 | Mac と USB 接続し Safari の Web インスペクタで iPhone のページに接続、コンソールで `window.__termmapTouch.speakVoiceGuide(btoa('テスト'))` を実行 | 鳴らない → OSC 経路ではなく発声そのものの問題(A, E, F) |
| 5 | 同コンソールで `speechSynthesis.speaking` / `speechSynthesis.paused` を継続監視 | `speaking` が `true` のまま戻らない → A で確定 |
| 6 | 同コンソールで keep-alive を止めた状態(`clearInterval`)で再確認 | これで鳴る → A で確定 |

`window.__termmapTouch` は `web/touch-overlay.js:735-748` で公開済みで、`speakVoiceGuide` も
含まれている。手順4-6 はコード変更なしで実行できる。

## 5. 除外できるもの

- **Secure Context**: `speechSynthesis` は Secure Context 必須の API ではなく、平文 HTTP でも
  利用できる。加えて README の運用手順では cloudflared 経由の `https://....trycloudflare.com` で
  接続するため、正規の証明書で Secure Context を満たしている。自己署名証明書も使っていない。
- **Basic 認証まわり**: 認証は `src/bin/webauth-proxy.rs` の Cookie 方式で、Cookie に `Secure` を
  付けている。素の HTTP では Safari がログイン状態を保持できず地図まで到達しないため、
  iPhone で地図が出ている時点で HTTPS 接続が成立している。
- **Rust 側の設定**: `speak_web()` は無条件に OSC を書くため、設定の端末別 ON/OFF で web 側だけ
  黙る経路は存在しない。

## 6. 対策の優先順(実装時の想定)

| 順 | 内容 | 対象 | 状態 |
|---|---|---|---|
| 1 | keep-alive を iOS(WebKit)以外だけに限定する | `bindVoiceGuideOsc()` | **実装済み**(`isIOS`判定を追加、iOSでは`setInterval`自体を張らない) |
| 2 | ボタンバーのタップでも解錠する(`touchstart`/`click`ハンドラ内に追加) | `buildBar()` | **実装済み** |
| 3 | 解錠を `touchend` / `click` にも張り、`voiceUnlocked` は `onstart` 到達後に立てる | `bindGestures()` / `unlockSpeechSynthesisOnce()` | 未実装 |
| 4 | 解錠用 Utterance を `volume=0` の空白から、実際に読ませる短文へ変える | `unlockSpeechSynthesisOnce()` | 未実装 |
| 5 | `voiceschanged` を待って ja ボイスを `u.voice` に明示指定する | `speakVoiceGuide()` | 未実装 |
| 6 | `onstart` / `onerror` を拾って失敗をステータス行に出す診断経路を持つ | `speakVoiceGuide()` | 未実装 |

1・2は確度が高く実機確認前でも入れて問題ない対策として実装済み(`node --check`で構文確認済み、
実機確認は未実施)。3〜6は§4の実機切り分け結果を見てから判断する。
