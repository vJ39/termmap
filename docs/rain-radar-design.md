# 雨雲レーダー オーバーレイ 設計書

気象庁(JMA)の降水ナウキャストタイルを、地図の上に半透明オーバーレイとして重ねて表示する機能の設計。
実況(過去)と予報(未来60分)のタイムラインをキー操作で前後に動かせるようにする。

対象リポジトリ: `github.com/vJ39/termmap`
状態: 設計のみ(実装未着手)

---

## 1. 前提と確定事実

### 1.1 データソース(検証済み)

| 種別 | URL | 内容 |
|---|---|---|
| 実況の時刻一覧 | `https://www.jma.go.jp/bosai/jmatile/data/nowc/targetTimes_N1.json` | `basetime == validtime`。5分刻み、直近から数時間分 |
| 予報の時刻一覧 | `https://www.jma.go.jp/bosai/jmatile/data/nowc/targetTimes_N2.json` | `validtime > basetime`。basetimeは最新固定、5分刻みで最大60分先(12フレーム) |
| タイル画像 | `https://www.jma.go.jp/bosai/jmatile/data/nowc/{basetime}/none/{validtime}/surf/hrpns/{z}/{x}/{y}.png` | 背景透過PNG。降水強度を色分け。降水なしの領域は透明 |

- APIキー不要・無料。
- z/x/y は標準的なWebメルカトルタイル座標(termmap既存の `geo.rs` と同じ系)。
- RainViewerは2026/01/01でnowcast(未来方向)を廃止し過去2時間のみになったため不採用。

### 1.2 実装前に1回だけ確認すべき項目(未検証)

設計はいずれも「パラメータ化して外から与える」形にしてあるので、値が違っても構造は変わらない。

| 項目 | 想定値 | 確認方法 |
|---|---|---|
| targetTimes JSONのフィールド名 | `[{"basetime":"20260814T060000Z","validtime":"...","elements":[...]}, ...]` の配列 | `curl` して1件目を見る |
| タイルの最大ズーム | z10前後(高解像度ナウキャストは250mメッシュ) | z11/z12を叩いて404 or 空タイルか確認 |
| タイルの最小ズーム | z4前後 | 同上 |
| 圏外(日本国外)の応答 | 404 または全面透明 | 例: ハワイのタイル座標で確認 |
| 期限切れbasetimeの応答 | 404 | 1時間以上前のbasetimeで確認 |

最大/最小ズームは `RADAR_MAX_Z` / `RADAR_MIN_Z` の定数1個ずつで吸収する。

### 1.3 出典表示

気象庁ホームページのコンテンツは自由利用が認められているが、このタイルエンドポイントは
開発者向けAPIとして文書化されたものではない(非公式流用)。以下を必須とする。

- ステータス行およびヘルプに `出典: 気象庁ナウキャスト` を表示する。
- READMEに「非公式エンドポイントの個人利用であり、予告なく停止しうる」旨を書く。
- User-Agent は既存の `termmap/0.1 (personal experiment)` をそのまま使う。
- 全フレームを先読みするような叩き方をしない(§6.3)。

---

## 2. 全体アーキテクチャ

```
                        ┌──────────────────────────────────────────┐
                        │ radar.rs (新規)                          │
  5分ごと               │                                          │
  ┌────────────────────>│  RadarClock (背景スレッド + mpsc)        │
  │                     │    targetTimes_N1.json ─┐                │
  │                     │    targetTimes_N2.json ─┴─> Vec<Frame>   │
  │                     │                                          │
  │                     │  Frame { basetime, validtime, kind }     │
  │                     │  kind = Observed | Forecast              │
  │                     └───────────────┬──────────────────────────┘
  │                                     │ mpsc::Receiver<Timeline>
  │                                     v
  │                     ┌──────────────────────────────────────────┐
  │                     │ ui.rs  interactive()                     │
  │                     │   radar_on: bool                         │
  │                     │   radar_tl: Timeline                     │
  └─────────────────────│   radar_idx: usize   ← < > キーで移動    │
                        │   radar_follow: bool ← 最新に追従するか  │
                        └───────────┬──────────────────────────────┘
                                    │ 表示中の Frame を毎フレーム渡す
                                    v
    ┌────────────────────────────────────────────────────────────────┐
    │ tiles.rs (拡張)                                                │
    │                                                                │
    │   TileKey { src: TileSource, z, x, y }                         │
    │     TileSource::Base(style)                  ← 既存の地図タイル │
    │     TileSource::Radar { basetime, validtime } ← 新規           │
    │                                                                │
    │   Cache<RgbaImage>  (base用/radar用に別々のLRU予算)            │
    │   TileLoader (既存のワーカー8本をそのまま共用)                 │
    │   NegativeCache (404を覚えて再取得しない ← 新規/必須)          │
    │                                                                │
    │   build_window_nowait()        -> RgbImage   (既存・地図)      │
    │   build_radar_window_nowait()  -> RgbaImage  (新規・雨雲)      │
    └───────────────┬────────────────────────────────────────────────┘
                    │ base: RgbImage / radar: RgbaImage
                    v
    ┌────────────────────────────────────────────────────────────────┐
    │ render.rs (拡張)                                               │
    │                                                                │
    │   実画像モード / halfblock / classify                          │
    │     blend_rgba_over(&mut RgbImage, &RgbaImage, opacity)        │
    │        → 真のアルファ合成。地図が透けて見える                  │
    │                                                                │
    │   braille / edge                                               │
    │     ink_radar_into_overlay(&mut OverlayLayer, &RgbaImage, ..)  │
    │        → 降水を「点のインク」として最背面に置く(ディザ間引き)  │
    │                                                                │
    │   どちらの経路でも、経路/POI/中心十字は雨雲より前面に残る      │
    └────────────────────────────────────────────────────────────────┘
```

### 2.1 なぜ合成方式を2系統に分けるか

termmapの描画経路は `main.rs::render()` の分岐で5通りある。雨雲は「面」のデータなので、
経路(線)やPOI(点)用の `OverlayLayer` にそのまま載せると経路と同じ扱いになり破綻する。

| 描画モード | 実体 | 雨雲の合成方式 | 理由 |
|---|---|---|---|
| 実画像(iTerm2) | RgbImage をそのままPNG化 | **アルファ合成** | 元画像がフルカラーなので素直に混ぜられる |
| halfblock(既定) | RgbImage → 上下半ブロック文字 | **アルファ合成** | 同上。1セル2画素の平均色に自然に混ざる |
| classify(地物色分け) | `recolor()` で6色に量子化 → halfblock | **recolor後**にアルファ合成 | 量子化前に混ぜると `classify()` が淡い青の降水を `Cat::Water`(湖)と誤判定する |
| braille(点字ドット) | 輝度/エッジ閾値でドットON/OFF | **OverlayLayerへインク** | braille出力に「背景色」の概念が無く、ドットが立つか立たないかしかない。アルファ合成しても輝度が少し動くだけで降水として読めない |
| edge(輪郭抽出) | 隣接画素の色差でドットON/OFF | **OverlayLayerへインク** | アルファ合成すると降水の境界が全部輪郭として出て線画が壊れる |

`OverlayLayer` は不透明な1色インクなので、降水域を全画素塗ると地図が完全に隠れる。
そこでインク経路では**ディザ間引き**する: 降水強度が閾値以上で、かつ `(x + y) % 2 == 0` の画素だけインクを置く。
市松模様(スクリーンドア)になり、下の地図が半分透けて読める。強度が強いほど間引き率を下げる。

`build_overlay()` の**先頭**(リングより前)に雨雲インクを置くことで、経路・道路・マーカー・
中心十字は従来どおり雨雲の上に描かれる。これは `build_overlay()` が既に「後に描いたものが勝つ」
順序で書かれているため、追加は1ブロックの挿入で済む。

---

## 3. データフロー

### 3.1 起動〜定常

```
起動
  └─ cfg.radar_enabled == true なら RadarClock::start() で背景スレッド起動
       └─ 即座に1回 targetTimes_N1/N2 を取得 → mpsc へ Timeline を送る
       └─ 以後300秒ごとに再取得(200ms刻みでstopフラグを見る = gpslive.rs と同じ形)

メインループ(毎フレーム)
  1. radar_rx.try_recv() で新しい Timeline があれば差し替え
       └─ radar_idx の再アンカー(§3.3)
  2. 表示中フレーム cur = radar_tl.get(radar_idx)
  3. loader.set_view(rcx, rcy, rz, style, cur)   ← 近傍優先の基準に雨雲フレームも渡す
  4. map_sig に radar_on / cur.basetime / cur.validtime / cfg.radar_opacity を混ぜる
  5. need_build なら
       base  = build_window_nowait(rcx, rcy, rz, rw, rh, style, &loader)
       radar = radar_on.then(|| build_radar_window_nowait(rcx, rcy, rz, rw, rh, cur, &loader))
  6. 合成(§2.1の表に従って分岐)
  7. ステータス行に時刻と読込状況を出す
```

### 3.2 タイル1枚が届くまで

既存の地図タイルと完全に同じ経路を通る。`TileLoader` のワーカーは `TileSource` を見て
URL・TTL・並列上限を切り替えるだけで、キューイング/近傍優先/generation加算の仕組みは共用する。

```
build_radar_window_nowait
  └─ Cache に無い TileKey を集める
       └─ NegativeCache(404既知)に入っているものは除外し、その画素は「透明のまま」
  └─ loader.request_tiles(missing)          … queued へ
       ワーカー … queued から view に最も近い1枚を inflight へ
                  fetch_tile(TileSource::Radar{..}, z, x, y)
                    ├─ 200 → RgbaImage を Cache へ insert → generation += 1
                    ├─ 404 → NegativeCache へ登録(再取得しない)
                    └─ その他 → 何もしない(次フレームで自然にリトライ)
  └─ 未取得タイルの領域は「全透明」で返す(グレーのプレースホルダーもLOADING透かしも描かない)
```

**未取得タイルにLOADING透かしを描かない**のは意図的。雨雲レイヤの下には既に地図が描かれており、
そこにグレーの箱や文字を重ねると地図が読めなくなる。読込中であることはステータス行の
`雨雲 読込中 3/9` で伝える(§5.8)。

### 3.3 タイムライン再アンカー(重要)

targetTimesは5分ごとに更新され、**basetimeが動く**。古いbasetimeのタイルはJMA側から消える。
`radar_idx` を素朴に保持していると、更新のたびに表示が別の時刻へずれる/消えたフレームを指す。

```
更新前の表示時刻 = old_validtime
新しい frames を受け取ったとき:

  radar_follow == true  (ユーザーが実況の最新を見ている・既定)
      → radar_idx = 最新の実況フレームのindex   … 自動で「今」に追従する
  radar_follow == false (ユーザーが < > でスクラブ済み)
      → new_frames の中で validtime == old_validtime を探す
         見つかった  → そのindexへ
         見つからない → 最も近い validtime のindexへクランプし、
                        ステータスに「表示時刻を調整しました」を1回出す
```

`radar_follow` は「`>` で最新の実況フレームちょうどに戻ったとき」に `true` へ復帰する。
つまり右端まで送り返せば追従モードに戻る、という自然な操作になる。

### 3.4 フレーム列の作り方

```
frames = N1(実況) の全件 ++ N2(予報) の全件
       を validtime でソートし、同一 validtime は実況を優先して重複排除
```

- N1 の各要素は `basetime == validtime`。
- N2 の各要素は `basetime` が共通(最新の実況時刻)で `validtime` が5分刻みに進む。
- 結果として `[…, -20分, -15分, -10分, -5分, 現在, +5分, …, +60分]` の一列になる。
- 「現在」= 実況フレームの最後尾。ここを `now_idx` として保持し、追従判定と `>` の折返しに使う。

---

## 4. キーバインド

### 4.1 A/D 案の可否 — **不採用**

ユーザー提案の A/D は既存バインドと衝突する。`grep` で確認した結果は以下のとおり。

| キー | 現在の割り当て | 場所 | 判定 |
|---|---|---|---|
| `A` | ルート再生 開始/停止 (`MenuAction::PlayRoute`) | `src/ui.rs:2240`、`src/menu.rs:43` | **衝突。使えない** |
| `a` | 中心の住所を表示 (`reverse_geocode`) | `src/ui.rs:2196`、`src/menu.rs:22` | **衝突。使えない** |
| `D` | Focus::Map では**未割り当て**。Spaceメニュー経由でのみ「道路の塊を管理」 | `src/menu.rs:30` | 技術的には空きだが、メニューの `D` と意味が食い違うため不可 |
| `d` | Focus::Map では未割り当て(オンボーディング画面とPOI一覧でのみ使用) | `src/ui.rs:1385, 1894` | 空きだが片割れの `a` が埋まっているので対にならない |

`A` はルート再生というかなり目立つ機能に当たっており、避けようがない。
また `D` を使うと「メニューでは D = 道路の塊、地図では D = タイムライン」という
場所依存の意味になり、Spaceメニューが全操作の索引として機能している現在の設計を崩す。

### 4.2 採用案

| キー | 動作 | 空き確認 |
|---|---|---|
| `>` | タイムラインを1コマ**進める**(未来方向) | `Char('>')` はリポジトリ全体で未使用 |
| `<` | タイムラインを1コマ**戻す**(過去方向) | `Char('<')` はリポジトリ全体で未使用 |
| `C` | 雨雲レーダー ON/OFF | `Char('C')` は未使用。`MENU_CATEGORIES` の全キーとも重複なし |

**`<` / `>` を選んだ理由**

1. 動画プレイヤー・スライドショーの「1コマ送り/戻し」として定着した記号で、
   時間軸の操作だと説明なしで分かる。矢印キーを地図パン専用に残す制約とも噛み合う。
2. 既存の `[` / `]` は「今の文脈のスカラーを増減する」キーとして
   再生速度調整とルート点の並べ替えの**2つの意味を既に持っている**(`src/ui.rs:2290-2291`)。
   ここに3つ目を足すと、再生中にタイムラインを動かせない等の分岐が増える。別記号にする。
3. `,`(設定)と `.` は隣接キーだが、`<` `>` はShift付きなので誤爆しにくい。
   crossterm は Shift付き `,` `.` を `KeyCode::Char('<')` / `Char('>')` として返すため、
   `Char('<')` `Char('>')` のマッチだけで拾える(SHIFT modifier を見る必要はない)。

**`C` を選んだ理由**

- 雨雲 = Cloud の頭文字。`R`(rain)は `menu.rs:28` が `MenuAction::RouteForm` に使っており、
  地図とメニューで意味がずれるため避けた。
- 小文字 `c` は「ルート全消去」だが、これは `clear_route_confirm` による y/n 確認が入っている
  (`src/ui.rs:874`)ので、`C` のつもりで `c` を押しても即座に壊れることはない。逆方向の誤爆
  (`c` のつもりで `C`)は表示が切り替わるだけで無害。大文字/小文字の隣接リスクは許容範囲と判断した。

**専用の「現在に戻る」キーは作らない。** `C` で OFF→ON にしたときは必ず最新の実況フレーム
(`now_idx`)から始める。スクラブして迷子になったら `C` を2回押せば戻れる、という導線にする。
キーを1つ節約でき、覚えることも減る。

### 4.3 挙動の詳細

```
C   radar_on を反転
      OFF→ON: radar_idx = now_idx, radar_follow = true
              まだフレーム一覧が届いていなければ「雨雲: 時刻を取得中…」を表示
      ON→OFF: 表示を消すだけ(取得済みタイルはキャッシュに残す=すぐ再表示できる)

>   radar_on == false のとき: ONにして now_idx から開始(発見しやすさのため)
    radar_on == true  のとき: radar_idx を +1(上限でクランプ、折り返さない)
                              radar_idx == now_idx になったら radar_follow = true
                              radar_idx  > now_idx なら        radar_follow = false

<   radar_on == false のとき: 何もしない(誤爆で勝手にONにしない)
    radar_on == true  のとき: radar_idx を -1(下限でクランプ)
                              radar_follow = false
```

`>` だけを「OFFからでも起動する」非対称にしているのは、未来の雨を見たいという
主動機に最短で到達させるため。`<` まで起動キーにすると誤爆時に驚きが大きい。

### 4.4 Spaceメニューへの追加

`src/menu.rs` の `ナビ・表示` カテゴリに1行足す。キー無しの操作を作らない既存方針を守る。

```rust
MenuItem { label: "雨雲レーダー",  key: 'C', action: MenuAction::ToggleRadar },
```

`MenuAction` に `ToggleRadar` を追加し、`ui.rs` の `run_action!` に処理を1本足す。
タイムライン移動はメニューに載せない(連打前提の操作でメニューと相性が悪い)。

### 4.5 ヘルプ(`src/keymap.rs`)

`[ナビ・表示]` 相当の位置に追記する。

```
 [雨雲レーダー]
   C              雨雲レーダー 表示/非表示 (気象庁ナウキャスト)
   < / >          表示時刻を 過去 / 未来 へ1コマ(5分)移動。最大+60分先まで
                   出典: 気象庁ナウキャスト
```

**あわせて既存の誤記を1行修正する。** `src/keymap.rs:52` は
`D  道路の塊を一覧` と書いているが、`Focus::Map` に `Char('D')` のアームは存在せず
(`src/ui.rs:2166-2308`)、実際には Space メニュー経由でしか到達できない。
ヘルプが実態と食い違っている状態なので、この機会に直す(§9.2)。

---

## 5. 新規/変更ファイルと関数シグネチャ案

### 5.1 `src/radar.rs`(新規・約280行)

`gpslive.rs` と同じ方針で、`std` + `ureq` + `serde_json` のみに依存し `crate::` を参照しない
(単体でコンパイル/テストできる)。

```rust
//! 気象庁ナウキャスト(降水)のフレーム時刻管理。タイル取得そのものは tiles.rs が行う。

/// フレームの種別。実況(過去〜現在)と予報(未来)。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameKind { Observed, Forecast }

/// 1コマ分の時刻。文字列は JMA の "YYYYMMDDTHHMMSSZ" 形式をそのまま保持する
/// (URL構築にそのまま使うので、日時型へ変換して往復させない)。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub basetime: String,
    pub validtime: String,
    pub kind: FrameKind,
}

/// フレーム一覧と「現在」の位置。ui.rs が保持する表示状態の入れ物。
#[derive(Clone, Default, Debug)]
pub struct Timeline {
    pub frames: Vec<Frame>,
    pub now_idx: usize,   // 実況の最後尾 = 「現在」
}

impl Timeline {
    pub fn is_empty(&self) -> bool;
    pub fn get(&self, idx: usize) -> Option<&Frame>;
    /// 新しい frames を受けたときの再アンカー(§3.3)。戻り値は (新idx, 新follow, 調整した旨のメッセージ)。
    pub fn reanchor(&self, prev_validtime: Option<&str>, prev_follow: bool)
        -> (usize, bool, Option<String>);
}

// ---- 取得 ----

/// targetTimes_N1.json / N2.json を取得してマージした Timeline を返す。
/// どちらか片方でも取れれば成功扱い(実況だけでも表示価値があるため)。
pub fn fetch_timeline() -> Result<Timeline, String>;

/// JSON本文 → Vec<Frame>。ネットワークに触れない純関数(テスト用に分離)。
pub fn parse_target_times(body: &str, kind_hint: FrameKind) -> Vec<Frame>;

/// N1 と N2 を validtime 順にマージし、重複は実況を優先。now_idx も決める。純関数。
pub fn merge_timeline(observed: Vec<Frame>, forecast: Vec<Frame>) -> Timeline;

// ---- 表示用の時刻整形(chrono非依存・純関数) ----

/// "20260814T060000Z" → "15:00" (UTC+9のJST・時分のみ)。日付跨ぎも正しく扱う。
pub fn jst_hhmm(utc_compact: &str) -> Option<String>;

/// 表示中フレームの人間向けラベル。 例: "15:00 実況" / "15:30 予報 +30分"
pub fn frame_label(tl: &Timeline, idx: usize) -> String;

// ---- 圏域判定 ----

/// 日本国内(ナウキャストの提供範囲)にかかっているか。
/// 範囲外なら1枚もリクエストしない(公共サービスへの無駄打ちを避ける)。
pub fn covers_japan(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64) -> bool;

// ---- 背景ポーラー(gpslive::GpsPoller と同型) ----

pub struct RadarClock {
    pub rx: std::sync::mpsc::Receiver<Timeline>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for RadarClock { /* stopを立ててjoin */ }

/// interval_secs ごとに fetch_timeline() し、成功したものだけ channel へ送る。
/// 停止契機は ①受信側drop ②RadarClock drop の2つ。待機は200ms刻みでstopを見る。
pub fn start_clock(interval_secs: u64) -> RadarClock;
```

**`jst_hhmm` を自前で書く理由**: `Cargo.toml` に `chrono` は無く、この機能のためだけに
日時crateを足すのは重い。`YYYYMMDDTHHMMSSZ` から時分を取り出して +9時間するだけなので、
`24` で剰余を取れば10行程度の純関数になり、テストも書きやすい。

### 5.2 `src/tiles.rs`(拡張)

```rust
/// タイルの取得元。従来 TileKey.style: String が持っていた「どのタイル群か」の軸を型にする。
/// 地図スタイル(base map style)と雨雲フレームは直交する軸なので、String に押し込めない。
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum TileSource {
    Base(String),                                   // "osm" / "voyager" / "dark" / "light" / "topo"
    Radar { basetime: String, validtime: String },  // 気象庁ナウキャスト
}

impl TileSource {
    fn url(&self, z: u32, x: i64, y: i64) -> String;
    fn max_z(&self) -> u32;          // Base=18(topoのみ17) / Radar=RADAR_MAX_Z
    fn min_z(&self) -> u32;          // Base=0 / Radar=RADAR_MIN_Z
    fn concurrency(&self) -> usize;  // Base=8(topoのみ2) / Radar=3
    fn disk_cached(&self) -> bool;   // Base=true / Radar=false (§6.2)
    fn cache_dir(&self) -> Option<String>;
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TileKey { src: TileSource, z: u32, x: i64, y: i64 }

/// 値の型が RgbImage → RgbaImage に変わる。地図タイルは decode 時に alpha=255 で持つ。
/// LRU予算は取得元の種別ごとに分ける(雨雲のスクラブで地図タイルが全部追い出されるのを防ぐ)。
pub struct Cache {
    base:  HashMap<TileKey, (RgbaImage, u64)>,
    radar: HashMap<TileKey, (RgbaImage, u64)>,
    tick: u64,
    base_cap: usize,   // 256 (据え置き)
    radar_cap: usize,  // 192
}

/// 404など「取っても無駄」と分かったタイル。無いと再取得が無限ループする(§8.2)。
struct NegativeCache { seen: HashSet<TileKey>, cap: usize }

/// TileSource 対応版。戻り値が RgbaImage になる以外は既存と同じ流れ。
pub fn fetch_tile(src: &TileSource, z: u32, x: i64, y: i64) -> Result<RgbaImage, FetchError>;

/// 404(恒久的失敗)と一時的失敗を呼び出し側で区別するためのエラー型。
pub enum FetchError { NotFound, Transient(String) }

/// ワーカーの近傍優先の基準。雨雲の現在フレームも渡し、並列上限の判定に使う。
impl TileLoader {
    pub fn set_view(&self, cx: f64, cy: f64, z: u32, style: &str, radar: Option<&TileSource>);
}

/// 雨雲レイヤの窓を組む。未取得タイルは「全透明」で返す(グレー箱もLOADING透かしも出さない)。
/// z が RADAR_MAX_Z を超える場合は既存の overzoom_geometry() をそのまま使って拡大する。
/// RADAR_MIN_Z 未満、または視野が日本国外なら None を返す(1枚もリクエストしない)。
pub fn build_radar_window_nowait(
    cx: f64, cy: f64, z: u32, win_w: u32, win_h: u32,
    frame: &radar::Frame, loader: &TileLoader,
) -> Option<RgbaImage>;

/// 表示中フレームの読込進捗(ステータス行用)。 (取得済み, 必要枚数)
pub fn radar_progress(loader: &TileLoader, cx: f64, cy: f64, z: u32,
                      win_w: u32, win_h: u32, frame: &radar::Frame) -> (usize, usize);

/// 古いフレームのタイルをまとめて捨てる。targetTimes 更新時に呼ぶ。
pub fn drop_radar_frames_except(cache: &mut Cache, keep: &[radar::Frame]);
```

既存 `build_window` / `build_window_nowait` のシグネチャ(`style: &str`)は**変えない**。
内部で `TileSource::Base(style.into())` を組み立てるだけにして、呼び出し側(`ui.rs` 2箇所、
`main.rs` の一発描画)を触らずに済ませる。`find_fallback_tile` は `TileSource::Base` のときだけ
働くよう条件を1つ足す(雨雲タイルを地図の代用にしない/地図タイルを雨雲の代用にしない)。

### 5.3 `src/render.rs`(拡張)

```rust
/// 半透明レイヤを base の上に source-over 合成する。
/// a' = (src.a / 255) * opacity として out = base*(1-a') + src*a'。
/// opacity は 0.0..=1.0。寸法が違う場合は重なる範囲だけ処理する(パニックしない)。
pub fn blend_rgba_over(base: &mut RgbImage, layer: &RgbaImage, opacity: f64);

/// braille/edge 用。降水域を OverlayLayer の「インク」として置く。
/// alpha が min_alpha 以上の画素のみ対象。さらに density(0.0..=1.0)でディザ間引きし、
/// 下の地図が透けて読めるようにする(density=1.0 で全塗り、0.5 で市松)。
/// build_overlay() の先頭で呼ぶ = 経路/POI/中心十字は必ず雨雲より前面になる。
pub fn ink_radar_into_overlay(ov: &mut OverlayLayer, layer: &RgbaImage,
                              min_alpha: u8, density: f64);
```

`OverlayLayer::put` は private なので、`ink_radar_into_overlay` は
`render.rs` 内に置いて `put` をそのまま呼ぶ(可視性を広げない)。

### 5.4 `src/main.rs`(変更)

```rust
// 第4引数を追加。None なら従来と完全に同じ挙動。
fn render(img: &RgbImage, a: &Args, ov: Option<&OverlayLayer>,
          radar: Option<(&RgbaImage, f64)>) -> String {
    // classify 経路は recolor() の「後」に blend する(§2.1)
    // braille/edge 経路では blend しない(インクは build_overlay 側で入っている)
}
```

`Args` への `--radar` フラグ追加は任意。CLI一発描画(`--png` 等)でも雨雲を焼けると便利だが、
Phase 1 では対話モード限定にして `Args` は触らない(スコープを絞る)。

### 5.5 `src/config.rs`(変更)

```rust
pub struct Config {
    // ... 既存 ...
    pub radar_enabled: bool,     // 起動時に雨雲レーダーをONにするか。既定 false
    pub radar_opacity: String,   // "light"(0.35) / "mid"(0.55) / "strong"(0.75)。既定 "mid"
    pub radar_refresh_sec: f64,  // targetTimes の再取得間隔(秒)。既定 300
}
```

- `[radar] enabled` / `opacity` / `refresh_sec` として `load_config_from` の match に3本、
  `save_config_to` のフォーマット文字列に1セクション追加する。
- `opacity` を数値でなく `String` の3択にしているのは、設定画面の
  「アコーディオン式3択」(`settings::CHOICES`)にそのまま載せられるため。
  数値だと `sample_interval_m` と同じインライン数値編集の実装が要る。
- `refresh_sec` は設定画面には出さない(config.toml のみ)。普通のユーザーが触る値ではない。

### 5.6 `src/settings.rs`(変更)

**行番号を末尾に追加する。既存の 0〜18 は1つも動かさない。**

```rust
// 追加行: 19=雨雲レーダー(ON/OFF) / 20=雨雲の濃さ(3択)
pub(crate) const CHOICES: &[SettingChoice] = &[
    // ... 既存の 4, 5, 9, 12, 18 ...
    SettingChoice { idx: 20, values: &["light", "mid", "strong"],
                    labels: &["薄い", "標準", "濃い"] },
];
```

- `is_pickable` / `pick_current` / `apply_pick` / `setting_description` / `settings_rows` に
  19・20 の分岐を足す。
- `apply_pick(20, ..)` は `ApplyEffect { force_reemit: true }` を返す(濃さを変えたら描き直す)。
- `settings_rows` の `its` 末尾に2行追加。

**末尾に足す理由**: `CHOICES` の `idx`、`pick_current`/`apply_pick`/`setting_description` の
match アーム、さらに `ui.rs` の `set_sel == 6`(道路の点間隔) `set_sel == 17`(APIキー)という
生の数値比較が全部この行番号に依存している。途中に挿入すると全部を手で振り直すことになり、
1個でも漏らすと「設定画面で別の項目が書き換わる」という気付きにくい壊れ方をする。

### 5.7 `src/ui.rs`(変更)

追加するローカル状態(`interactive()` 内):

```rust
let mut radar_on = cfg.radar_enabled;
let mut radar_tl = radar::Timeline::default();
let mut radar_idx: usize = 0;
let mut radar_follow = true;
let mut radar_clock: Option<radar::RadarClock> =
    radar_on.then(|| radar::start_clock(cfg.radar_refresh_sec as u64));
```

変更点は以下の8箇所。

| 箇所 | 変更 |
|---|---|
| `interactive()` 冒頭 | 上記の状態を追加。`radar_on` なら `RadarClock` 起動 |
| ポーリング条件(`ui.rs:1347`) | `radar_clock.is_some()` を or に足す(時刻更新を取りこぼさない) |
| ジョブ受信ブロック | `radar_clock.rx.try_recv()` → `Timeline` 差し替え + `reanchor` |
| `map_sig`(`ui.rs:570-603`) | `radar_on` / 表示中フレームの `basetime`+`validtime` / `cfg.radar_opacity` を hash に混ぜる |
| 描画ブロック(`ui.rs:616-658`) | `build_radar_window_nowait` を呼び、実画像経路は `blend_rgba_over`、AA経路は `render(.., radar)` へ渡す。braille/edge 用のインクは `build_overlay` 呼び出しの直後に `ink_radar_into_overlay` |
| ステータス行(`ui.rs:875-890`) | 雨雲の時刻・種別・読込状況を追記(§5.8) |
| `Focus::Map` の match(`ui.rs:2166-`) | `Char('C')` / `Char('<')` / `Char('>')` の3アーム追加 |
| `Focus::Settings`(`ui.rs:1524`) | `set_sel + 1 < 19` → `< 21`。ついでに定数化する |

### 5.8 ステータス行の表示

`Focus::Map` のステータス行に、雨雲がONのときだけ差し込む。既存の `playing` / `live` と
同じ組み立て方にする。

```
☂15:00実況        … 実況を表示中(追従モード)
☂15:30予報+30分   … 予報を表示中
☂15:00実況 読込3/9 … タイル取得中(枚数が出る)
☂時刻取得中…      … targetTimes がまだ届いていない
☂範囲外           … 日本国外を表示している
```

出典表記(`出典: 気象庁ナウキャスト`)は毎フレーム出すと幅を食うので、
**`C` でONにした直後の `addr` メッセージ**として1回出す:
`addr = "雨雲レーダー: ON (出典: 気象庁ナウキャスト)"`。恒久的な表記はヘルプとREADMEに置く。

### 5.9 ドキュメント

- `docs/MANUAL.md`: 操作章に `C` / `<` / `>` を追記。
- `README.md`: 機能一覧に1行。非公式エンドポイントである旨と出典を明記。

---

## 6. キャッシュ・更新頻度の方針

### 6.1 メモリキャッシュ

| 対象 | 上限 | 根拠 |
|---|---|---|
| 地図タイル | 256枚(据え置き) | 既存の挙動を変えない |
| 雨雲タイル | 192枚 | RGBA 256×256 = 256KB/枚 → 約48MB。z10前後の1画面は概ね4〜9枚なので、全25コマ弱をスクラブしても大半が残る |

**LRU予算を種別ごとに分ける**のが要点。1つのHashMapに混ぜると、タイムラインを端から端まで
スクラブした瞬間に雨雲タイルが地図タイルを全部追い出し、地図が LOADING だらけになる。

`Cache` の値型が `RgbImage` → `RgbaImage` になることで、地図タイルのメモリは
192KB → 256KB/枚(合計48MB → 64MB)に増える。これは許容する。RGB/RGBA を enum で
出し分ける案も検討したが、`build_window` 系の合成ループが全部2分岐になり割に合わない。

### 6.2 ディスクキャッシュ — **雨雲は保存しない**

既存の地図タイルは `~/.config/termmap/tiles/<style>/<z>/<x>/<y>.png` に30日TTLで保存されるが、
雨雲タイルはこの仕組みに**乗せない**。

1. **ヒット率がほぼゼロ**。パスに `basetime`/`validtime` が入るため、1時間もすればどの
   エントリも二度と参照されない。書き込み専用のファイルが無限に積み上がる。
   1時間あたり最大 25コマ × 9枚 = 225ファイルで、放置すれば数日で数万ファイルになる。
2. **30日TTLは雨に対して危険**。もし乗せると、期限内と判断された30日前の降水が
   そのまま地図に描かれる。ツーリングアプリで古い雨を新しい雨として見せるのは実害がある。
3. メモリLRUだけで、同一セッション内のスクラブ往復は十分に賄える。

将来どうしても永続化したくなった場合は、`~/.config/termmap/radar/` という**別ディレクトリ**に
**20分TTL**で置き、起動時に2時間より古いエントリを掃除する、という別実装にする(Phase 2扱い)。

### 6.3 ネットワークの叩き方

| 項目 | 値 | 根拠 |
|---|---|---|
| `targetTimes` 再取得間隔 | 300秒 | ナウキャスト自体が5分更新。これより短くしても新しい情報は無い |
| 雨雲タイルの並列数 | 3 | 地図の8とは別枠。非公式利用なので控えめにする(topoの2に近い水準) |
| 先読み | **表示中フレームのみ**。前後1コマの投機取得もしない | 25コマ×9枚を先読みすると1回のON操作で225リクエストになり、公共サービスに対して明らかに過剰 |
| 圏外の扱い | 視野が日本の範囲(概ね lat 20〜50 / lon 120〜150)に全くかからなければ1枚も投げない | 海外でtermmapを開いたときに無意味な404を量産しない |
| 404の扱い | `NegativeCache` に記録して二度と取りに行かない | §8.2 |

タイムラインをキー連打で高速スクラブされると、通過した全コマの取得依頼が `queued` に
溜まってしまう。**キー入力から120ms静止するまで `request_tiles` を出さない**デバウンスを
`build_radar_window_nowait` の呼び出し側(ui.rs)に入れる。静止するまでは「取得済みのタイルだけ
描く(無ければ透明)」。既存の `moved_at` / `settling` と同じ考え方で、変数を1つ増やすだけで済む。

### 6.4 古いフレームの破棄

`targetTimes` を更新したとき、新しい一覧に含まれない `basetime`/`validtime` のタイルは
JMA側から消えており二度と使えない。`drop_radar_frames_except()` で
メモリキャッシュと `NegativeCache` の該当エントリをまとめて捨てる。
これをやらないと LRU が「もう絶対に使わないタイル」で埋まる。

---

## 7. 設定項目案

### 7.1 設定画面(`,`)

| 行 | 表示 | 型 | 既定 | 説明文 |
|---|---|---|---|---|
| 19 | `雨雲レーダー OFF` | ON/OFF | OFF | 雨雲レーダー: 気象庁ナウキャストの降水を地図に重ねる。Cキーでも切替。`<` `>` で表示時刻を前後(過去〜+60分) |
| 20 | `▸ 雨雲の濃さ 標準` | 3択 | 標準 | 雨雲の濃さ: 重ねる強さ。Enterで一覧を開いて選択(薄い=地図優先 / 標準 / 濃い=雨優先) |

既定を OFF にする理由: 起動のたびに外部サービスへ問い合わせが飛ぶのは、地図しか使わない
ユーザーにとって不要。`C` を押した人だけが通信する。

### 7.2 `config.toml`

```toml
[radar]
enabled = false      # 起動時に雨雲レーダーをONにするか
opacity = "mid"      # light / mid / strong
refresh_sec = 300    # targetTimes の再取得間隔(秒)。設定画面には出さない
```

### 7.3 不透明度の値

| ラベル | 値 | 想定 |
|---|---|---|
| 薄い(`light`) | 0.35 | 道路の判別を優先。ルート確認しながら雨の位置だけ見たいとき |
| 標準(`mid`) | 0.55 | 既定 |
| 濃い(`strong`) | 0.75 | 雨の強弱を読みたいとき。地図はうっすら残る |

1.0(完全不透明)は用意しない。地図が消えたら地図アプリとして機能しないため。

braille/edge 経路(インク合成)では不透明度をディザ密度に読み替える:
`light`=0.35 → 3画素に1つ、`mid`=0.55 → 市松、`strong`=0.75 → 4画素に3つ。

---

## 8. 既知のリスクと対策

### 8.1 非公式エンドポイントの変更・停止

**リスク**: 開発者向けAPIとして文書化されていないため、URL体系・JSON形式・提供そのものが
予告なく変わりうる。

**対策**:
- URL構築を `TileSource::url()` と `radar.rs` の定数の2箇所だけに閉じる。壊れたら1箇所直せば済む。
- `targetTimes` の取得に失敗しても**地図は絶対に落とさない**。`radar_on` を維持したまま
  ステータスに `☂時刻取得できず` を出し、次の300秒後にまた試す。
- JSONのパース失敗は `Vec::new()` を返す(`serde_json::from_str(..).ok()` の既存パターン)。
  フィールドが増えても無視、減っても既定値で動く。
- タイルが全部404でも、雨雲レイヤが全透明になるだけで地図は正常。

### 8.2 404無限リトライ(**既存コードの潜在バグでもある**)

**リスク**: 現在の `worker_loop` は `fetch_tile` 失敗時にキャッシュへ入れず `inflight` から
外すだけなので、次フレームで `build_window_nowait` が同じタイルを再登録する。つまり
**恒久的に404なタイルは、再描画のたびに永久に取りに行く**。
再描画は `loader.is_busy()` の間80ms間隔で回るので、実質的なポーリング攻撃になる。

地図タイルでは404がほぼ起きないため今まで表面化していないが、雨雲では
「期限切れbasetime」「日本国外の座標」で常時起きる。**これを直さずに実装するとJMAを叩き続ける。**

**対策**: `fetch_tile` の戻り値を `Result<RgbaImage, FetchError>` にして 404 を区別し、
`FetchError::NotFound` なら `NegativeCache` に登録する。`build_*_window_nowait` は
`NegativeCache` にあるキーを `missing` に積まない。`NegativeCache` は
`drop_radar_frames_except()` のタイミングで一緒に掃除する。

地図タイル側も同じ仕組みの恩恵を受けるので、副次的な改善になる。

### 8.3 透明タイルの合成方式

**リスク**: `image` crate の `to_rgb8()` はアルファを捨てる。既存 `fetch_tile` をそのまま
使うと「降水なし＝透明」の情報が失われ、PNGが透明部に持っている生のRGB値(黒や白)が
そのまま地図の上に貼られて画面が真っ黒/真っ白になる。

**対策**: 雨雲経路は必ず `to_rgba8()` を通す。`Cache` の値型を `RgbaImage` に統一することで
「雨雲なのにRGBで持ってしまう」経路を型で作れなくする。

**副次リスク**: JMAのPNGがパレット+tRNS形式の場合、`image` の `png` feature でも
アルファ付きでデコードできるはずだが、期待どおりか実装時に1枚デコードして
`pixel[3]` の分布を確認する(全部255なら透過が失われている)。

### 8.4 classify モードでの誤分類

**リスク**: `render.rs::classify()` は `b - r > 12 && b + 6 > g && b > 150` を水域と判定する。
JMAの弱い雨の色(淡い青)はこの条件に合致するので、量子化前に合成すると雨が「湖」になる。

**対策**: §2.1 のとおり `recolor()` の**後**に合成する。`main.rs::render()` の
classify 分岐の順序を `recolor → blend → composite` にする。

### 8.5 ズーム範囲の不一致

**リスク**: termmapは z2〜z19。雨雲タイルは z10前後が上限と見られる。上限を超えて要求すると
404、あるいは topo のように「プレースホルダー画像がHTTP 200で返る」可能性もある。

**対策**: `RADAR_MAX_Z` を超えたら既存の `overzoom_geometry()` で z=RADAR_MAX_Z のタイルを
拡大する(topo と全く同じ手口・関数もそのまま再利用)。降水は元々250mメッシュの粗いデータなので、
拡大でぼやけても情報は失われない。`RADAR_MIN_Z` 未満は日本全体が画面に収まらない広域なので
雨雲を出さない(`None` を返す)。

### 8.6 タイムライン再アンカーの取りこぼし

**リスク**: `targetTimes` 更新でフレーム一覧が入れ替わるため、素朴に `radar_idx` を保持すると
表示時刻が勝手にずれる/範囲外を指す。

**対策**: §3.3 の `reanchor()`。`radar_idx` ではなく **`validtime` 文字列**を同一性の基準にする。
`Timeline::reanchor` は純関数なのでテストで固められる。

### 8.7 メモリ増加

**リスク**: `Cache` の RGBA 化で地図タイルのメモリが 48MB → 64MB、雨雲で最大 +48MB。合計112MB。

**対策**: 種別ごとのLRU予算(§6.1)で上限は確定している。実測して重ければ
`radar_cap` を下げるだけで調整できる。ターミナルアプリとしては許容範囲と判断する。

### 8.8 タイムゾーン

**リスク**: `validtime` は UTC(`...Z`)。JSTで表示しないと「30分後の予報」が「9時間半前」に見える。

**対策**: `radar::jst_hhmm()` で +9時間して時分表示。日付跨ぎ(15:00Z → 翌00:00 JST)は
`(hh + 9) % 24` で正しく出る。時分しか出さないので日付繰り上がりの表示は不要。
`frame_label()` で `予報 +30分` のような相対表記も併記し、絶対時刻だけに頼らない。

### 8.9 スクラブ時の描画負荷

**リスク**: `<` `>` を押すたびに `map_sig` が変わって全画面を再構築する。実画像モードの
`image_res = high` では 4倍解像度のPNGを毎回エンコードするので連打がつらい。

**対策**: 既存の `image_settle_low_res`(移動中は低解像度)の判定に、雨雲のフレーム移動も
「移動」として含める。`moved_at` を更新するだけで既存の仕組みに乗る。

---

## 9. スコープ外・別途対応

### 9.1 今回やらないこと(将来の候補)

- **アニメーション再生**(タイムラインを自動でコマ送り)。ルート再生(`A`)と同じ枠組みで
  作れるが、先読みが必要で通信量が跳ねる。まず手動スクラブの使い勝手を見てから判断する。
- **画面下部のタイムラインバー**(`実況├───●──┤予報` のような目盛り)。
  レイアウト計算(`map_rows` 等)に手を入れることになるので、Phase 1 はステータス行の
  文字表示だけにする。
- **雨量凡例**(色と mm/h の対応表)。
- **降水以外のレイヤ**(雷・竜巻)。同じ `nowc` 配下に `thns`/`trns` があるが別スコープ。
- **CLI一発描画(`--png` 等)への対応**。
- **ディスク永続キャッシュ**(§6.2)。

### 9.2 実装中に見つけた既存の不具合(この機能とは独立・別コミット推奨)

いずれも今回の変更で触る周辺にあるため、まとめて直すか、少なくとも記録しておきたい。

1. **設定画面のGoogle APIキーが、キーボード入力では保存されない**
   `src/ui.rs:1534` は `set_sel == 17` で `Focus::SettingsEdit(17, buf)` を開くが、
   Enter時の確定処理(`src/ui.rs:1583`)は `idx == 15` のときしか `cfg.google_maps_api_key` に
   書き戻さない。入力文字のフィルタ(`src/ui.rs:1594`)も `idx == 15` を見ている。
   結果、入力してEnterを押しても黙って破棄される。
   一方 Paste 経由(`src/ui.rs:2329`)は `set_sel == 17` を見ているので**貼り付けだけは動く**。
   `idx == 15` を `idx == 17` に直せば解消する。

2. **ヘルプの `D` が実際には効かない**
   `src/keymap.rs:52` が `D  道路の塊を一覧` と書いているが、`Focus::Map` の match
   (`src/ui.rs:2166-2308`)に `Char('D')` のアームは無い。到達経路は Space メニュー
   (`src/menu.rs:30`)だけ。ヘルプの記述を実態に合わせるか、`Focus::Map` に
   `Char('D') => run_action!(MenuAction::ManageRoads, ..)` を足すかの二択。

3. **設定画面の行数が生の数値でハードコードされている**
   `src/ui.rs:1524` の `set_sel + 1 < 19` は `settings::settings_rows()` が返す行数と
   手で同期させる必要がある。今回2行増えるので必ず踏む。定数化するか
   `settings_rows()` の戻り値の長さから導出するのが望ましい。

4. **404タイルの無限リトライ**(§8.2)。雨雲がなくても存在する構造的な問題。

---

## 10. テスト方針

`tiles.rs` / `render.rs` / `settings.rs` の既存テストと同じ粒度で、純関数に寄せて書く。

| 対象 | テスト内容 |
|---|---|
| `radar::parse_target_times` | 正常JSON / 空配列 / 壊れたJSON / 未知フィールド混入 でパニックしない |
| `radar::merge_timeline` | N1+N2 が validtime 順に並ぶ / 重複 validtime は実況が勝つ / `now_idx` が実況の最後尾を指す / 片方が空でも成立 |
| `radar::jst_hhmm` | `20260814T060000Z` → `15:00` / 日付跨ぎ `20260814T150000Z` → `00:00` / 不正文字列 → `None` |
| `radar::Timeline::reanchor` | follow時は新しい now_idx へ / 非follow時は同一validtimeを追う / 消えたvalidtimeは最近傍へクランプ / 空リストでパニックしない |
| `radar::covers_japan` | 東京=true / ハワイ=false / 日本を含む広域ビュー=true / 境界値 |
| `render::blend_rgba_over` | alpha=0 は base 不変 / alpha=255 かつ opacity=1.0 は完全置換 / opacity=0.5 の中間値 / 寸法不一致でパニックしない |
| `render::ink_radar_into_overlay` | min_alpha 未満は置かれない / density=1.0 で全塗り / density=0.5 で概ね半分 / density=0.0 で何も置かれない |
| `tiles::TileSource` | `url()` が想定文字列を組む(basetime/validtime が正しい位置に入る) / `max_z`/`concurrency`/`disk_cached` が種別ごとに正しい |
| `tiles::NegativeCache` | 登録したキーが弾かれる / cap超過で最古が落ちる / `drop_radar_frames_except` で該当フレームだけ消える |
| `tiles::Cache` | base と radar のLRU予算が独立(radarを大量投入してもbaseが落ちない) |
| `settings` | 既存テストの `is_pickable` / `pick_current` / `apply_pick` に 19・20 を追加。**既存 0〜18 の期待値が変わっていないこと**を回帰として残す |

ネットワークを叩くテストは書かない(`fetch_timeline` / `fetch_tile` は既存方針どおり手動確認)。

---

## 11. 実装フェーズ分け

段階ごとに `cargo test` が通り、動く状態を保つ。

| Phase | 内容 | 目安 |
|---|---|---|
| 0 | §9.2 の既存不具合(1)(3)(4)を先に直す。特に(4)の `NegativeCache` は雨雲の前提 | 小 |
| 1 | `tiles.rs` の `TileSource` 化 + `Cache` の RGBA 化。**挙動は完全に不変**なことをテストで確認 | 中 |
| 2 | `radar.rs` 新規(取得・パース・マージ・時刻整形・ポーラー)。単体テストのみで動作確認 | 中 |
| 3 | `build_radar_window_nowait` + `blend_rgba_over`。まず実画像モード/halfblockだけ通す | 中 |
| 4 | `ui.rs` の状態・キー(`C` `<` `>`)・ステータス行。ここで実際に触れるようになる | 中 |
| 5 | `ink_radar_into_overlay` で braille/edge/classify 対応 | 小 |
| 6 | `config.rs` / `settings.rs` / `menu.rs` / `keymap.rs` / MANUAL / README | 小 |

Phase 1 が最も慎重を要する(既存の地図描画全体に触るため)。ここは挙動不変のリファクタとして
独立コミットにし、`cargo test` 全通過と目視での地図表示確認をもって完了とする。

---

## 12. 実装前に確認したい点

1. 既定を OFF(§7.1)としたが、ツーリング用途では常時ONの方が自然という判断もありうる。
2. `C` / `<` / `>` のキー割り当てで問題ないか(§4.2)。特に `C` の妥当性。
3. Phase 0 の既存不具合修正を、この機能と同じブランチでやるか分けるか。
4. §1.2 の未検証項目(特にタイルの最大ズーム)は、実装着手時に `curl` で確認してから
   `RADAR_MAX_Z` を確定させる方針でよいか。
