iPhone(web版・ttyd経由)で実画像モードの地図がアスペクト比を崩して見える件の原因整理と対策設計。
macOSネイティブ端末(iTerm2 / WezTerm)では同じコードで正しく見える。

対象コード: `src/render.rs`(`emit_iterm2_image`) / `src/ui.rs`(描画寸法の決定と3箇所の呼び出し元) /
`src/dragmode.rs`(ブラウザ⇄termmap の通知路) / `web/touch-overlay.js`(ブラウザ側) /
`web/vendor/xterm-addon-image.js`(vendorした公式アドオン)。
状態: 設計のみ。実装は未着手。

## 1. 現状の仕組み

### 1.1 Rust側が出しているもの

実画像モードの地図は次の寸法で作られ、そのままインライン画像として出る。

| 段 | 値 | 位置 |
|---|---|---|
| 地図領域のセル数 | `map_cols` × `map_rows` | `src/ui.rs:559`, `src/ui.rs:393,547` |
| AA描画グリッド | `ow` = `map_cols`(braille は ×2) / `oh` = `map_rows*2`(braille は ×4) | `src/ui.rs:560` |
| 実画像の生成解像度 | `rw` = `map_cols*scale` / `rh` = `map_rows*2*scale` | `src/ui.rs:689-693` |
| 出力 | `ESC ]1337;File=inline=1;size=..;width=map_cols;height=map_rows;preserveAspectRatio=0:<base64> BEL` | `src/render.rs:412-425`, 呼び出しは `src/ui.rs:993` |

`rw`/`rh` は Web メルカトルのグローバル画素座標の幅・高さでもある(`build_window` が
`rcx`/`rcy` を中心に `rw`×`rh` 画素を切り出す。`src/tiles.rs:235`)。つまり画像1ピクセルは
地理的に正方形で、画像は「横 `map_cols` : 縦 `2*map_rows`」の地理範囲を持つ。

ここで縦の係数が 2 なのは「1セルの物理的な縦横比が 1:2」という前提が置かれているため。
halfblock(1セル=縦2サンプル)も braille(1セル=縦4・横2サンプル)も同じ前提で、
`preserveAspectRatio=0` は「この前提どおりのセル矩形へ強制フィットする」という指定になっている。

### 1.2 ブラウザ側が受け取ってからの処理

ttyd 同梱の xterm.js に、`web/vendor/xterm-addon-image.js`(公式 @xterm/addon-image)を
`scripts/build-web-index.sh` が埋め込み、`web/touch-overlay.js:346-371` が
`new ImageAddon({ iipSupport: true, sixelSupport: false })` でロードしている。

アドオンの IIP 処理(vendorファイル内 `_resize`)を読むと、`width`/`height` に単位なしの数値を
与えた場合の扱いはこうなっている。

```js
// o = dimensions.css.cell.width, a = dimensions.css.cell.height
l = parseInt(header.width)  * o;   // = map_cols * セル幅[CSS px]
c = parseInt(header.height) * a;   // = map_rows * セル高[CSS px]
return l ? (!header.preserveAspectRatio && l && c ? [l, c] : [l, t*l/e]) : [e*c/t, c];
```

`preserveAspectRatio=0` なので `[l, c]` がそのまま採用され、PNG は `createImageBitmap` の
`resizeWidth/resizeHeight` で `l`×`c` へ引き伸ばされる。その後 `addImage` が
`ceil(画像幅 / セル幅)` × `ceil(画像高 / セル高)` セルを占有として書き込み、描画時は
1セルにつき「セル幅×セル高」のスライスを等倍で置く。

結果として、ブラウザでの表示矩形は **`map_cols * セル幅` × `map_rows * セル高`(CSS px)**。
iTerm2 の `preserveAspectRatio=0` と意味は同じで、実装差はない。

## 2. 歪みの式

セル幅を `cw`、セル高を `ch`(いずれも表示上の物理比。CSS px でも device px でも比は同じ)とすると、

- 画像が持つ地理的な縦横比 = `map_cols : 2*map_rows`
- 画面上の表示矩形の縦横比 = `map_cols*cw : map_rows*ch`

の2つが一致したときだけ地図は正しく見える。一致条件は `ch = 2*cw`。ずれた場合の縦伸び率は

```
D = ch / (2 * cw)     D > 1 なら縦に伸びる / D < 1 なら縦に潰れる
```

で、`map_cols`・`map_rows`・`scale`・ズーム段には依存しない。**実画像モードの地図に効く歪み要因は
この1本だけ**で、アドオンのコードを追った限り他の経路は無い(§4)。

ネイティブ端末で正しく見えるのは、iTerm2 / WezTerm の等幅フォントのセル比が `ch/cw ≈ 2` に
収まっているため。

## 3. ブラウザ側のセル比の見積り(ttyd 既定設定)

`scripts/serve-web.sh:86-92` は ttyd に `-t 'disableLeaveAlert=true'` しか渡していないので、
xterm.js のフォントは ttyd の既定値のまま。生成済み `web/index.html` から実際の既定値を確認した。

| 項目 | 値 |
|---|---|
| fontFamily | `Consolas,Liberation Mono,Menlo,Courier,monospace` |
| fontSize | 13 |
| lineHeight | 1(xterm.js 既定) |
| rendererType | webgl |

iOS には Consolas / Liberation Mono が無いので **Menlo 13px** に解決される。フォントファイル
(`/System/Library/Fonts/Menlo.ttc`)の実メトリクスから計算すると、

| 量 | 値(13px) |
|---|---|
| 送り幅(= charWidth) | 7.827 px |
| `line-height: normal` の行高(ascent+descent、hhea 1901/-483、upem 2048) | 15.133 px |

xterm.js はこれを `device.char.width = floor(w*dpr)` / `device.char.height = ceil(h*dpr)` で
デバイス画素へ丸め、`css.cell = device.cell / dpr` を持つ。DPR=3(iPhone)で丸め方が最も
不利に転ぶと `cw = 23/3 = 7.667`、`ch = 51/3 = 17` となり `ch/cw = 2.217`、有利に転ぶと
`ch = 46/3 = 15.33` で `ch/cw = 2.000`。

つまり **この経路だけで説明できる歪みは D = 1.00〜1.11 程度**(最大でも1割強の縦伸び)。
セル比の不一致という仮説自体はコード上正しいが、量としては「明らかに引き伸ばされて見える」
レベルには届かない。実機の見え方がこれより明らかに大きい場合は、§4 の別要因が重なっている。

## 4. 他に疑った要因と、コードから出した結論

| 要因 | 結論 | 根拠 |
|---|---|---|
| DPR(devicePixelRatio=3) | アスペクトには効かない。`css.cell` の丸めを通じて数%効くだけ | `css.cell = device.cell / dpr` で縦横とも同じ dpr で割る |
| ページのピンチズーム / viewport スケール | 一様拡大縮小でアスペクトは変わらない | `scripts/build-web-index.sh:68` で `width=device-width, initial-scale=1` を挿入済み |
| アドオンの `preserveAspectRatio` 解釈違い | 無し。`0` は iTerm2 と同じ「指定矩形へ完全フィット」 | §1.2 の `_resize` |
| ttyd / xterm.js のバージョン差 | 現状は関係しない。画像描画をしているのは vendor した公式アドオンで、ttyd 同梱版は未ロード | `web/touch-overlay.js:341-345` のコメントと `bindImageAddon` |
| Safari が `createImageBitmap` の `resizeWidth/Height` を無視する可能性 | 無視された場合は「小さく等倍で出る」であって縦横比は崩れない。要実機確認 | `_resize` の戻り値を使うのは resize 指定のみ |
| `sixelScrolling` 既定 `true` | 現状の期待どおり(画像はカーソル位置に置かれる)。**`false` にしてはいけない** | `addImage` は `sixelScrolling` が偽だとカーソルを無視して原点(0,0)に置く。左袖(`gut`)の分だけ右に出している現状の配置が壊れる |
| pty と xterm.js の cols/rows 不一致(回転・ソフトキーボード・URLバーで一時的に起きうる) | 起きると右端でクリップ、または行送りでスクロールする。アスペクトが崩れたようにも、フレームごとに伸び縮みするようにも見える | `addImage` は `c+A>=a`(端末幅)で打ち切る。`sixelScrolling=true` では行ごとに `lineFeed()` する |
| `map_cols`/`map_rows` の下限クランプ | `cols - gut < 10`(左袖28桁を開いた状態で端末が38桁未満)のとき、実際より広い矩形を出して右端がクリップされる | `src/ui.rs:559` の `.max(10)` |
| フォント計測前の一時的な既定セル寸法 | `CELL_SIZE_DEFAULT = 7×14`(比ちょうど2.0)。占有セル数は整合し、後から実寸へ再スケールされる | `_rescaleImage` |

### 4.1 実写(Street View)・道路カメラの全画面画像は別問題

`src/ui.rs:428` と `src/ui.rs:514` は、640×480 で取得した**写真**を
`emit_iterm2_image(out, img, cols, map_rows)` として端末全体のセル矩形へ強制フィットしている。
写真は地図と違って端末の形に合わせて生成していないので、歪み率は

```
D_photo = (端末の表示矩形の縦横比) / (写真の縦横比 4:3)
```

になる。iPhone 縦持ちの実寸(CSS 幅 390 前後・Menlo 13 で 50桁 × 49行程度、表示矩形は
おおよそ 383×750 px、縦横比 0.51)を当てると **縦に約2.6倍**。ネイティブ端末の横長ウィンドウでも
3割程度は崩れるが、iPhone 縦持ちでは桁違いに目立つ。AA フォールバック側も
`resize(img, cols, map_rows*2)`(`src/ui.rs:430`, `516`)で同じだけ潰している。

「びよーん」と表現される見え方に量として一致するのはこちらなので、実機で見たのが地図画面か
実写/カメラ画面かを最初に切り分ける必要がある。地図画面であれば §2 の D は最大1割強で、
体感と合わないなら §4 のクリップ/不一致側を疑う。

## 5. 原因の確定手順(実機)

順に3つ。どれも実装前にできる。

1. **画面の切り分け**: 地図画面(実画像モード)と、実写(`V`)/道路カメラの全画面画像を見比べる。
   後者だけ極端に伸びているなら §4.1 が主因で、地図側の歪みは別途 §2 の範囲。
2. **描画モードの比較**: 同じ位置・同じズームで braille/AA モードと実画像モードを見比べる。
   §2 の歪みは AA モードにも同率で効く(§6)ので、**AA が正しく見えるのに実画像だけ大きく崩れる**
   なら、原因はセル比ではなく占有セル数のずれ(クリップ・行送り)側にある。
3. **セル寸法の実測**: Mac の Safari Web インスペクタで iPhone のページに接続し、
   `term.cols` / `term.rows` と `.xterm-screen` の `getBoundingClientRect()` から
   `cw = rect.width/cols`、`ch = rect.height/rows` を出して `D = ch/(2*cw)` を計算する。
   `term._core._renderService.dimensions.css.cell` でも同じ値が取れるが、こちらは内部APIなので
   確認用に留める。実測 D と見た目の伸び率が一致すれば §2 で確定。

## 6. braille/AA モードへの影響

**同じ式の歪みを同率で受ける**。halfblock は1セルを縦2サンプル、braille は縦4・横2サンプルで
使うので、どちらも「1セル = 縦横比 1:2」を前提にした地理範囲(`ow`×`oh`)を切り出している
(`src/ui.rs:560`)。セル比が 1:2 からずれれば、地図の地理的な縦横比はそのぶん崩れる。

それでも AA モードが問題として上がらない理由は次の2つ。

- 崩れるのは「どの範囲を切り出すか」だけで、文字そのものは端末が正しい形で描く。画像のように
  ピクセルが引き伸ばされるわけではないので、にじみや変形として目に見えない。
- braille でも1セル 2×4 ドットという粗さがあり、1割程度の幾何誤差はその量子化に埋もれる。

したがって「実画像モードだけ問題になる」のは正しい観察だが、**AA が正しくて画像が間違っている
のではなく、両方が同率でずれていて画像だけそれが見える形で出る**、という関係になっている。
今回の対策では画像側だけ直し、AA 側は据え置く(§7.6)。

## 7. 対策案の比較

### 7.1 一覧

| 案 | 概要 | 歪みの残り | 端末非依存 | 実装量 | 主なリスク |
|---|---|---|---|---|---|
| A | ブラウザが実セル比を測り、専用マーカーで termmap へ通知 | 0 | web のみ | 中(JS+Rust) | 通知が来る前の初期フレームは既定値のまま |
| B | xterm.js のフォント/行高を固定して比を 1:2 に合わせる | 数% | web のみ | 小(JS) | フォントの実体とOS側の丸めまでは固定できない。文字の行間の見た目が変わる |
| C | `preserveAspectRatio=1` に変える | 0(ただし別の破綻) | 端末実装依存 | 極小(Rust 1行) | 実装間で意味が違う(§7.4)。行あふれ・余白が出る |
| D | iPhone では AA へフォールバック | 0(画像を使わないため) | — | 小 | 実画像モードという機能自体の後退 |
| E | 端末に実セル比を問い合わせ、**画像の生成解像度側**を合わせる | 0 | ネイティブ含め全端末 | 中(Rust 中心) | 取得経路が端末ごとに違う。既定値へのフォールバックが要る |

### 7.2 推奨: 案E(取得経路の一部として案Aを使う)

考え方は「セル矩形に合わせて画像を引き伸ばす」のをやめ、**引き伸ばしても歪まない画像を作る**こと。
実セル比を `r = ch / cw` とすると、生成解像度を

```
rw = map_cols * scale                    (現状のまま)
rh = round(map_rows * r * scale)         (現状は r=2 固定)
```

にすれば、画像1ピクセルは画面上でも正方形になり、`preserveAspectRatio=0` のままで歪みが消える。
`r` の取得は次の順で試し、取れなければ 2.0 を使う。

1. `crossterm::terminal::window_size()`(crossterm 0.28 にあり)。TIOCGWINSZ の `ws_xpixel`/`ws_ypixel`
   から `r = (height/rows) / (width/cols)`。iTerm2 / WezTerm は埋めている。ttyd は cols/rows しか
   セットしていない見込み(=0 が返る。要実測)。
2. ブラウザからの paste マーカー(案A)。既存の `\u{1}GPS\u{1}...` / `\u{1}PAN\u{1}...` と同じ経路で
   `\u{1}CELL\u{1}<cw>\u{1}<ch>\u{1}` を送る。JS 側の値は `web/touch-overlay.js:442-455` の
   `terminalViewportSize()` と同じ `.xterm-screen` の矩形を `term.cols`/`term.rows` で割って出す
   (公開APIだけで足りる。内部の `_renderService.dimensions` は使わない)。
3. どちらも取れなければ `r = 2.0`(現状と同じ挙動)。

送信契機は overlay の初期化時・`resize`/`orientationchange`・`visibilitychange` での復帰時。
`CELL` を受け取って `r` が変わったら `force_reemit`(`src/ui.rs:960` 付近の既存フラグ)を立てて
1枚描き直す。値が同じなら何もしない。

なお `CSI 16 t`(セル寸法の問い合わせ)はアドオンが `windowOptions.getCellSizePixels` を有効化するので
web でも応答するが、xterm.js の応答は `toFixed(0)` で整数 CSS px へ丸められる(`ESC[6;<h>;<w>t`)。
7.667 → "8" のような丸めで比が最大7%ずれるため、**セル寸法を直接問い合わせる経路は採らない**。
使うなら `CSI 14 t`(テキスト領域の画素サイズ)と `CSI 18 t`(文字数)の組で割る方が誤差が小さいが、
crossterm の入力パーサに応答が流れ込む扱いを別途詰める必要があるので、web は案Aのマーカー、
ネイティブは `window_size()` の2本立てにする。

### 7.3 案B(フォント固定)を主対策にしない理由

`-t fontFamily=... -t fontSize=... -t lineHeight=...` で xterm.js 側を固定すれば比は寄せられる。
実際、xterm.js の `lineHeight` オプションはセル高の倍率なので、`lineHeight = 2*cw/ch_base` を
与えれば比をちょうど 2.0 に合わせられる(Rust 側の変更が要らない)。ただし

- フォントの実体はOS依存で、同じ family 指定でも iOS と macOS で同じ字形・同じ `normal` 行高に
  なる保証がない。
- 行高をいじると文字の行間の見え方が変わる。比を縮める方向(`lineHeight < 1`)では行が詰まる。
- ネイティブ端末側の比のずれには何もできない。

ので、案E を入れたうえで、どうしても残差が気になるときの微調整に留める。

### 7.4 案C(`preserveAspectRatio=1`)を採らない理由

実装間で意味が違う。

- iTerm2 の仕様: 指定した幅・高さの矩形に、縦横比を保ったまま内接させる(余白が出る)。
- xterm addon-image の実装: `width` を優先し、`height` は無視して `t*l/e`(自然な縦横比)で決める。
  指定より縦に長くなり得る。長くなった分は `sixelScrolling=true` の `lineFeed()` で画面が
  スクロールし、ステータス行や左袖と衝突する。

さらに、表示矩形が指定セル矩形と一致しなくなるため、タップ座標や `pan_ratio_to_px`
(`src/dragmode.rs:134`)が前提にしている「地図領域のセル矩形＝地図の表示範囲」という対応が崩れる。
歪みは消えても、触った位置と地図の対応がずれるという別の不具合に化ける。

### 7.5 案D(AAへフォールバック)を採らない理由

実画像モードは web 版でも使いたい機能として実装した経緯があり(`web/touch-overlay.js:341-345`)、
これは機能の取り下げになる。原因が測って直せる性質のものなので、先に案E を試す。

### 7.6 実写・道路カメラ側(§4.1)の対策

こちらは端末のセル比とは別の問題なので、対策も別に持つ。写真の縦横比を保ったまま
セル矩形へ内接させるレターボックス処理を **Rust 側で** 行い、余白込みの1枚の画像にしてから
`preserveAspectRatio=0` で出す。端末の `preserveAspectRatio` 実装差に依存せずに済み、
AA フォールバック経路(`src/ui.rs:430`, `516`)にも同じ考え方を適用できる。
必要な `r` は §7.2 と同じものを使う。

## 8. 影響範囲

| ファイル | 変更内容 |
|---|---|
| `src/render.rs` | 変更なし(`emit_iterm2_image` の引数・書式はそのまま) |
| `src/ui.rs:689-693` | `rh` を `map_rows * r * scale` にする。`r` の保持と更新 |
| `src/ui.rs:428,514` | 実写・道路カメラのレターボックス処理を通す(§7.6) |
| `src/ui.rs` の paste 受け口(`2568-2612` 付近) | `\u{1}CELL\u{1}` の分岐を追加 |
| 新規 or `src/dragmode.rs` | `CELL` マーカーの定数と parse、`window_size()` からの取得、既定値へのフォールバック。純関数として切り出す |
| `web/touch-overlay.js` | セル寸法の計測と `CELL` マーカー送信(初期化・resize・復帰時) |
| `docs/MANUAL.md` | 実画像モードの注意書きを更新する場合のみ |

`rw`/`rh` から地理範囲・オーバーレイ座標・クロスヘアまで全部が導出されている
(`src/ui.rs:838-934`)ので、`rh` を変えれば表示と当たり判定は自動的に整合する。
`pan_ratio_to_px`(`src/dragmode.rs:134`)も比で計算しているため追随する。
`ow`/`oh`(AAグリッド)は今回触らない。

## 9. テスト方針

- 純関数のユニットテスト
  - `CELL` マーカーの parse(正常・欠損・非数値・0以下・非現実的な比は棄却)。`parse_pan_marker`
    (`src/dragmode.rs`)の棄却方針に合わせる。
  - 実画像の生成解像度を返す関数について、`r = 2.0` のとき現状と同じ値になること(回帰防止)、
    `r = 2.2` のとき `rh` が 1.1 倍になること。
  - `window_size()` が 0 を返したときに既定値 2.0 へ落ちること。
- 実機確認(ユニットテストで検出できない配線ミスがあるため。`feedback_settings-toggle-wiring-gap`)
  - iPhone Safari で `CELL` が届いていること、`r` が反映された後の1枚が再描画されること。
  - iTerm2 / WezTerm で `window_size()` 経由の `r` が取れ、表示が変わらない(=従来どおり)こと。
  - 端末サイズが 38 桁未満のときのクリップ挙動(§4 の `.max(10)`)。

## 10. 未確定・要実測

- ttyd が pty の `ws_xpixel`/`ws_ypixel` を埋めるか(埋めていなければ §7.2 の経路1は web で不発、
  これは想定どおり)。
- iPhone 実機の `cw`/`ch` の実測値と、そこから出る D。§5 の手順3で取る。
- 実機の見え方が D の予測(最大1割強)より大きい場合、原因は占有セル数のずれ側にある。その場合は
  `term.cols`/`term.rows` と termmap 側の `cols`/`map_rows` を突き合わせて、リサイズ過渡や
  クランプの影響を確認する。
- iOS Safari の `createImageBitmap` が `resizeWidth/resizeHeight` を尊重するか。無視されていた場合は
  画像が小さく出るはずで、今回の症状とは別。

## 11. 今回やらないこと

- AA(halfblock/braille/edge)モードのアスペクト補正。式の上では同率でずれているが見た目には出ない
  (§6)。やるなら地理範囲を `r` 基準で切り出してから AA グリッドへ異方性リサイズする形になる。
- xterm.js のフォント設定変更(§7.3)。案E の残差を見てから判断する。
- `sixelScrolling` の変更。現状の `true` が正しい(§4)。
