国土交通省「道路情報提供システム」(road-info-prvs.mlit.go.jp)の道路ライブカメラを地図にマーカーで
重ね、`N` キーで中心に一番近いカメラの写真を全画面表示する。

対象コード: `src/camera.rs` / `src/ui.rs` の `camera_*`・`cam_view`・`cam_job`。
状態: 実装済み(データ層・地図マーカー・全画面表示・設定・メニュー)。

## 1. データソース

| 項目 | 値 |
|---|---|
| 一覧ページ | `https://www.road-info-prvs.mlit.go.jp/roadinfo/pc/pcImage_{整備局CD}_1.html` |
| 画像 | `https://www.road-info-prvs.mlit.go.jp/roadinfo/img/doro_gazo/pc/{fileList[].file}` |
| 認証 | 不要(APIキー無しで叩ける。開発者向けAPIとして公開されたものではない非公式利用) |
| User-Agent | `termmap/0.1 (personal experiment)` |
| タイムアウト | 20秒 (`HTTP_TIMEOUT_SECS`) |
| 依存 | `std` + `ureq` + `serde_json`(画像デコードのみ `image`)。`crate::` を参照しない |

実測で確認済みの構造(`src/camera.rs` 冒頭のコメントに記載):

- カメラ一覧は地方整備局CDごとのページ `pcImage_{CD}_1.html` に、`input#kokudoJson` の
  `value` 属性(シングルクォート囲み)として**生JSONがそのまま**埋め込まれている。
  HTMLエンティティエスケープはされていない。`regulation.rs` と違い2段階フェッチは不要。
- JSON構造は `{"路線コード&路線名": [ {"R_路線コード": {カメラ本体}}, ... ], ...}`。
- カメラ本体は `doro_gazo_joho_kanri_id` / `gis_point`(`[lon, lat]` の文字列2要素) /
  `image_name`(地点名) / `fileList`(直近の撮影一覧・新しい順。各要素に `get_datetime` と `file`)。
- `file` 名が `s_` 始まりならサムネイル(148x98)。`s_` を外すとフル画像(720x480)になる。

### 1.1 地方整備局の選び方

整備局は10局(CD 81〜90)しかなく、管轄境界のポリゴンは持っていない。そのため
**bbox の中心に最も近い代表点の局1つ**だけを取りに行く簡易割当にしている(`nearest_bureau`)。
代表座標は同システムの `common/js/Common.js` の `SEIBIKYOKU_LIST` から採取した実測値。

| CD | 局 | CD | 局 |
|---|---|---|---|
| 81 | 北海道開発局 | 86 | 近畿地方整備局 |
| 82 | 東北地方整備局 | 87 | 中国地方整備局 |
| 83 | 関東地方整備局 | 88 | 四国地方整備局 |
| 84 | 北陸地方整備局 | 89 | 九州地方整備局 |
| 85 | 中部地方整備局 | 90 | 沖縄総合事務局 |

距離は緯度経度の二乗和(度単位)で比較する。局境界付近のカメラを取りこぼす可能性はあるが、
termmap の表示範囲は通常1局の管轄より十分小さいため実用上問題にしない、という判断。

## 2. データ層 (`src/camera.rs`)

```rust
pub struct RoadCamera {
    pub id: String,
    pub lat: f64, pub lon: f64,
    pub name: String,               // image_name(地点名)
    pub thumb_url: Option<String>,  // 直近のサムネイル
    pub full_url: Option<String>,   // 直近のフル画像(s_ を外したURL)
    pub taken_at: String,           // フル画像の撮影時刻(get_datetime をそのまま)
}

pub fn nearest_bureau(lat: f64, lon: f64) -> u32              // 代表点の最近傍で1局選ぶ
pub fn fetch_bureau(cd: u32) -> Result<Vec<RoadCamera>, String>   // その局の全カメラ(bboxで絞らない)
pub fn parse_cameras(html: &str) -> Vec<RoadCamera>          // 純関数
pub fn fetch_image(url: &str) -> Result<image::RgbImage, String>
```

- `fetch_bureau` は「その局のページを1回取得 → `parse_cameras`」。bbox での絞り込みはしない
  (呼び出し側がメモリ上で絞る)。失敗と0件を区別できるよう `Result` を返す。以前は両方 `Vec::new()`
  だったため、圏外に入った瞬間に呼び出し側が「カメラ0台」で上書きし、直前まで見えていたカメラが
  消えていた。`Err` を受けた側(`plotlayer`)は手元の値を保持する。
- `RoadCamera` は serde の derive を持つが、**写真URL3項目(`thumb_url` / `full_url` / `taken_at`)は
  保存しない**(`#[serde(skip)]`)。URLに15分ごとの撮影ディレクトリが入るため、保存すると1時間後には
  404になる。読み戻すと `None` / 空文字になり、必要になった時点(N キーを押したとき)だけ取り直す。
- `parse_cameras` は `id` / `gis_point` の lon,lat が揃わない要素を黙って除外する(座標が無いと地図に置けない)。
  `fileList` が空なら URL は `None`、`taken_at` は空文字。
- `extract_kokudo_json` は `id="kokudoJson"` の後の `value='...'` を素朴に切り出す(HTMLパーサは使わない)。
- `fetch_image` は本文を読み切って `image::load_from_memory` → `to_rgb8()`。失敗時はエラー文字列を返す。

## 3. ui.rs の配線

### 3.1 状態

```rust
camera_layer: PlotLayer<camera::RoadCamera>                 // plotlayer::camera()
cam_view: Option<(RgbImage, camera::RoadCamera)>            // 全画面表示中の写真
cam_job: Option<Receiver<(camera::RoadCamera, Result<RgbImage, String>)>>
```

セル表(整備局CD→カメラ一覧)・進行中ジョブ・ディスク永続化はすべて `PlotLayer` が持つ。
以前あった `camera_points` / `camera_job` / `camera_last_fetch` / `camera_bbox` はこの1つに畳まれた。

### 3.2 一覧の取得スケジュール

道路交通量・通行規制・主要道路と共通の仕組み(`src/plotlayer.rs`)。設計は
`docs/plot-data-disk-cache-design.md`。

| 項目 | 値 |
|---|---|
| 発火条件 | `cfg.camera_enabled` かつ、視野中心の局が手元に fresh で無い |
| 取得単位 | 地方整備局1局(局は全国で10しかないので必ず1件) |
| 再取得の抑止 | fresh TTL 7日。この間は通信しない |
| オフライン表示 | 上限なし(位置が数日古くても実害が無い) |
| ズーム下限 | なし |
| 実行方法 | `std::thread::spawn` + `mpsc`、毎ループ `tick()` |

ジョブが生きている間はポーリング側に倒す。キー入力が無いまま取得が完了しても結果が反映される。
表示範囲での絞り込みは `PlotLayer::items(視野bbox)` がメモリ上で行う。

**失敗時の挙動**: `fetch_bureau` が `Err` を返しても手元の値は消さない。ディスクに控えがあれば
それを表示し、無ければマーカーが出ないままステータス行が `📷カメラ無し` になる。

### 3.3 地図マーカー (`src/ui.rs:790-796`)

`cfg.camera_enabled` のときだけ、各カメラを `draw_ring(ov, ix, iy, 3, [170, 90, 220], 2)` で描く
(半径3・太さ2の紫のリング)。他のオーバーレイと同じ `OverlayLayer` に載るので、描画モードを問わず出る。

### 3.4 写真の全画面表示

**起動**: 地図で `N`(`src/ui.rs:2257`)、または Space メニュー → ナビ・表示 → 「道路カメラを見る」(`N`)。
どちらも `MenuAction::ViewCamera`(`src/ui.rs:297-320`)に集約されている。

```
cfg.camera_enabled == false → 効果音(error) + "道路ライブカメラ: OFF(設定で有効化)"
視野内にカメラが無い        → 効果音(error) + "道路ライブカメラ: 周辺に無し"
それ以外                    → 地図中心に最も近いカメラを選ぶ(緯度経度の二乗和で比較)
                              full_url があれば背景スレッドで fetch_image → cam_job
                              full_url が無ければ(キャッシュ由来のカメラ)、同じ背景スレッドで
                              その局を取り直して URL を補ってから fetch_image。
                              ステータスは "📷カメラ情報を更新中…"
```

キャッシュから読んだカメラは写真URLを持たない(§2 の `#[serde(skip)]`)。取り直しは
**押したときだけ**の1回で、以前のように押す/押さないに関わらず90秒ごとに局ページを
取り直すことはしない。

`cam_job` の受信(`src/ui.rs:1071-1078`)で `cam_view` に入り、以後は対話ループ冒頭の早期 return
(`src/ui.rs:480-515`)で全画面表示になる。実写(Street View)と同じパターンだが、道路カメラは
固定視点の1枚画像なので**パン/ズームは無い**。

| 表示 | 内容 |
|---|---|
| 画像 | `cfg.image_mode` かつ端末が対応していれば iTerm2 インライン画像。そうでなければ `Triangle` で縮小して halfblock |
| ステータス行 | ` 道路カメラ {name}({taken_at})  Esc/q戻る  {lat:.4},{lon:.4} ` |

| キー | 動作 |
|---|---|
| `Esc` / `q` | 地図へ戻る(`cam_view = None`) |
| `I` | 実画像モードの ON/OFF(地図と同じ挙動) |

画像取得中は `cam_job` が `jobs_active` に含まれるためスピナーが回る(`src/ui.rs:871`)。
`Esc` や `Ctrl+C` によるジョブ一括キャンセルの対象にも入っている(`src/ui.rs:1289-1292, 1338-1340`)。

### 3.5 ステータス行 (`src/ui_status.rs` の `plot_label`)

| 状態 | 表示 |
|---|---|
| OFF | (何も出さない) |
| 0件かつ取得中 | `📷取得中… ` |
| 0件かつ取得完了 | `📷カメラ無し ` |
| 取得済み | `📷{件数}台(N) ` |
| 取得済みだが fresh(7日)を過ぎている | `📷{件数}台(9日前)(N) ` |

`(N)` は「`N` キーで見られる」ことの案内。取得済みなら取得中でも件数を優先する。

## 4. 設定

| 場所 | 値 |
|---|---|
| 設定画面(`,`)の行 | 24 `道路ライブカメラ ON/OFF`(`src/settings.rs:161`、説明文は `setting_description(24)`) |
| `Config` フィールド | `camera_enabled: bool`。既定 `false` |
| `config.toml` | `[camera] enabled = false` |
| メニュー | ナビ・表示 → 「道路カメラを見る」`N`(`src/menu.rs:46`) |
| ヘルプ(`?`) | `[道路ライブカメラ] (, の設定でON要)` の節(`src/keymap.rs`) |

既定 OFF は「ONにした人だけが外部サービスへ問い合わせる」方針による。設定画面でONにしたときは
`camera_bbox = None` にして次のポーリングで即取得させる(`src/ui.rs:1496-1499`)。

`N` を割り当てた理由はコード上のコメントのとおり: `C`/`K`/`L`/`V`/`P`/`I` 等の自然な字は
全て他機能で使用済みだったため、空いていた `N` を使った(小文字 `n` は代替ルート巡回)。

## 5. 制約

- 局の管轄境界を持たず代表点の最近傍で1局だけ取るため、**局境界付近のカメラは取りこぼす**。
  複数局にまたがる広域表示でも取得は1局分。
- 1回の取得でその局の全カメラ一覧(HTMLページ1枚)を落としてから bbox で絞る。表示範囲が狭くても
  転送量は同じ。
- 表示するのは `fileList` の先頭(最新)1枚のみ。過去コマの切り替えは無い。
- 撮影時刻はソースの `get_datetime` をそのまま出す(タイムゾーン変換・整形はしない)。
- `thumb_url` は構造体には入っているが、現在 UI からは使っていない(全画面表示は `full_url` のみ)。
- 非公式エンドポイントのため、URL体系・HTML構造・提供そのものが予告なく変わりうる。
  変更点は `HTML_BASE` / `IMG_BASE` / `extract_kokudo_json` / `parse_cameras` に閉じている。

## 6. テスト (`src/camera.rs` の `mod tests`)

- `nearest_bureau`: 北海道・東京・沖縄でそれぞれ 81 / 83 / 90 を選ぶ。
- serde: 位置(id/lat/lon/name)は往復し、写真URL3項目は保存されず読み戻しで `None` / 空文字になる。
- `extract_kokudo_json`: シングルクォートの `value` を取り出す / 無いときは `None`。
- `parse_cameras`: 実 HTML の抜粋(2026/08/16 実測、1カメラ)から id・座標・地点名・撮影時刻・
  サムネイルURL・フル画像URL(`/s_` 除去)を組み立てる。
- `parse_cameras` の異常系: HTMLでない / JSONでない / 空オブジェクト / `gis_point` 欠如は空 Vec。
- `live_fetch_real_camera_data` は `#[ignore]`(実ネットワーク・`cargo test --release -- --ignored`)。

ステータス行のラベル分岐は `src/ui_status.rs` 側のテスト
(`camera_label_distinguishes_loading_from_no_cameras`)でカバーされている。

## 7. 既知の記述ずれ・残作業

- `README.md` と `docs/MANUAL.md` にこの機能の記載が無い(ヘルプ `src/keymap.rs` にはある)。
