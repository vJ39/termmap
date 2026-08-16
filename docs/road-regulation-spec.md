国土交通省「道路情報提供システム」(road-info-prvs.mlit.go.jp)の通行規制情報を取得し、
通行止め・車線規制・片側交互通行・チェーン規制・移動規制の区間を種別ごとの色で地図に線描画する。

対象コード: `src/regulation.rs` / `src/ui.rs` の `regulation_*`。
状態: 実装済み(データ層・地図への線描画・設定)。

JARTIC の交通量(`docs/road-traffic-spec.md`)には事故・災害・工事による通行止めが含まれないため、
そこを埋めるのがこの機能。ライブカメラ(`docs/road-camera-spec.md`)と同じ非公式システムを使う。

## 1. データソース

| 項目 | 値 |
|---|---|
| パス発見用ページ | `https://www.road-info-prvs.mlit.go.jp/roadinfo/pc/pcTukokisei_81_1.html` |
| データ本体 | `{JSON配信元}/TukoKisei/{1次メッシュコード}.json` |
| 配信元ベース | `https://www.road-info-prvs.mlit.go.jp/roadinfo/backup/{timestamp}/{hash}/` |
| 認証 | 不要(APIキー無しで叩ける。開発者向けAPIとして公開されたものではない非公式利用) |
| User-Agent | `termmap/0.1 (personal experiment)` |
| タイムアウト | 20秒 (`HTTP_TIMEOUT_SECS`) |
| 依存 | `std` + `ureq` + `serde_json` のみ。`crate::` を参照しない |

### 1.1 2段階フェッチ

JSON の配信元パスは `../backup/{timestamp}/{ハッシュ}/` の形で、更新のたびに変わる。
そのため毎回まず HTML を取得してパスを発見する。

```
pcTukokisei_81_1.html を取得
  └─ extract_json_base: 本文中の最初の "../backup/" 以降から
     "{timestamp}/{hash}/" の2階層ぶんを切り出す(3つ目の '/' の手前まで)
       → "{BASE}/backup/{timestamp}/{hash}/"
  └─ 各メッシュについて "{json_base}TukoKisei/{mesh}.json" を取得
```

ページ名の `_81_`(北海道開発局)は配信元パスの発見にしか使っておらず、取得するデータは
メッシュコードで決まる(全国分がメッシュ単位で置かれている)。

### 1.2 1次メッシュコード

データは1次メッシュ(JIS X 0410、約80km四方)単位でファイルが分かれているため、表示範囲を覆う
メッシュを列挙して個別に取得する。

```
p = floor(lat * 1.5)
u = floor(lon) - 100
code = p * 100 + u
```

bbox からコードを割り出すのは `src/mesh.rs` の `primary_codes(lat_min, lon_min, lat_max, lon_max)`
(交通量と共通)。`lat_min > lat_max` のような不正な範囲や日本のメッシュ空間の外では空 Vec を返し、
1回も通信しない。`regulation.rs` 自身はメッシュコード1件ぶんを取る役に徹する。

## 2. データ層 (`src/regulation.rs`)

```rust
pub enum RegulationKind { Closed, LaneRestriction, AlternatingOneLane, ChainRequired, MovementRestriction, Other }

pub struct ClosureEvent {
    pub line: Vec<(f64, f64)>,   // (lat, lon) の順。ソースは lon,lat 順なので変換する
    pub kind: RegulationKind,
}

pub fn discover_json_base() -> Result<String, String>                    // 1段目: 配信元パスの発見
pub fn fetch_mesh(base: &str, mesh: u32) -> Result<Vec<ClosureEvent>, String>  // 2段目: メッシュ1枚
pub fn parse_closures(body: &str) -> Vec<ClosureEvent>   // ネットワークに触れない純関数
```

2段に分けてあるのは、呼び出し側がメッシュ(セル)単位でキャッシュするため。1ジョブの中で
`discover_json_base()` を1回だけ呼び、不足しているメッシュに使い回す。配信元パスは更新のたびに
変わるので永続化しない(保存すると次回404になる)。`ClosureEvent` / `RegulationKind` は
serde の derive を持ち、そのままディスクキャッシュへ保存できる(`kind` はバリアント名の文字列)。

### 2.1 種別の判定と配色

`kisei_naiyo_cd`(規制内容CD)から分類する。実測値ベースで、未知の値は黙って `Other` に落とす。

| コード | 種別 | `label()` | 描画色(RGB) |
|---|---|---|---|
| `01`, `08` | `Closed` | 通行止め | `[220, 30, 30]` |
| `04` | `LaneRestriction` | 車線規制 | `[230, 140, 30]` |
| `05` | `AlternatingOneLane` | 片側交互通行 | `[230, 200, 40]` |
| `06` | `ChainRequired` | チェーン規制 | `[60, 170, 230]` |
| `09` | `MovementRestriction` | 移動規制 | `[160, 80, 200]` |
| 上記以外 | `Other` | 規制 | `[150, 150, 150]` |

`08` を通行止め扱いにしているのは、ソース側の `Tukokisei.js` が同様に扱っていたため。
元データ(`geo_json.style.color`)は実測でほぼ灰色 `#808080` 一色で見分けが付かないため、
色は termmap 側で持つ。`label()` は現在 `ui.rs` から使われておらず `#[allow(dead_code)]` を付けてある
(将来の詳細表示用に残している)。

### 2.2 パース規則

- 応答は配列。各要素の `geo_json` は**文字列としてエスケープされた GeoJSON** なので、もう一度パースする。
- `/geometry/coordinates` の各要素 `[lon, lat]` を `(lat, lon)` に入れ替えて `line` に積む。
- `kisei_naiyo_cd` または `geo_json` が無い要素、座標が2点未満で線にならない要素は黙って捨てる。
- 壊れた JSON・配列でない本文は空 Vec(panic しない)。

### 2.3 失敗時のフォールバック

失敗と0件を区別できるよう、通信する関数はどちらも `Result` を返す。

| 失敗箇所 | 挙動 |
|---|---|
| メッシュコードが0件 | 通信しない(呼び出し側がセルを1枚も要求しない) |
| HTML の取得・本文化に失敗 | `Err`(配信元パス不明。そのジョブのメッシュは全部取れない) |
| `extract_json_base` が見つけられない | `Err` |
| 個別メッシュの取得に失敗 | そのメッシュだけ `Err`(呼び出し側が他のメッシュを巻き添えにしない) |

`Err` を受けた側(`plotlayer`)は、そのセルの手元の値を**消さずに保持する**。ディスクに
stale上限(24時間)内の控えがあればそれを表示し、ステータス行に経過時間を添える。

## 3. ui.rs の配線

### 3.1 状態

```rust
regulation_layer: PlotLayer<regulation::ClosureEvent>   // plotlayer::regulation()
```

セル表(メッシュコード→取得結果)・進行中ジョブ・ディスク永続化はすべて `PlotLayer` が持つ。
以前あった `regulation_events` / `regulation_job` / `regulation_last_fetch` / `regulation_bbox` は
この1つに畳まれた。

### 3.2 取得スケジュール

道路交通量・ライブカメラ・主要道路と共通の仕組み(`src/plotlayer.rs`)。設計は
`docs/plot-data-disk-cache-design.md`。

| 項目 | 値 |
|---|---|
| 発火条件 | `cfg.regulation_enabled` かつ、視野を覆うメッシュに fresh でないものがある |
| 取得単位 | 1次メッシュ1枚(1回の判定で最大9枚まで) |
| 再取得の抑止 | fresh TTL 10分。この間は通信しない |
| オフライン表示 | stale上限 24時間。この間はディスクの控えを出し続ける |
| ズーム下限 | z11(これより広域では取りに行かない) |
| 実行方法 | `std::thread::spawn` + `mpsc`、毎ループ `tick()` |

ジョブが生きている間はポーリング側に倒す。キー入力が無いまま取得が完了しても結果が反映される。

成功したメッシュだけが更新され、失敗したメッシュは手元の値を保持する(空で上書きしない)。

### 3.3 描画 (`src/ui.rs:798-805`)

`cfg.regulation_enabled` のときだけ、各 `ClosureEvent` の `line` を画面座標へ直し、
連続する2点ごとに `draw_line(ov, x0, y0, x1, y1, ev.kind.color(), 3)`(太さ3)で描く。
ルート・waypoint・交通量・カメラより後に描くので最前面。共通の `OverlayLayer` に載るため、
halfblock / 実画像 / braille / edge のどの描画モードでも出る。

### 3.4 ステータス行 (`src/ui_status.rs:166-175`)

| 状態 | 表示 |
|---|---|
| OFF | (何も出さない) |
| 0件かつ取得中 | `⚠取得中… ` |
| 0件かつ取得完了 | `⚠規制無し ` |
| 取得済み | `⚠{件数}件 ` |

取得済みなら取得中でも件数を優先する。

## 4. 設定

| 場所 | 値 |
|---|---|
| 設定画面(`,`)の行 | 25 `通行規制 ON/OFF`(`src/settings.rs:162`、説明文は `setting_description(25)`) |
| `Config` フィールド | `regulation_enabled: bool`。既定 `false` |
| `config.toml` | `[regulation] enabled = false` |

既定 OFF は「ONにした人だけが外部サービスへ問い合わせる」方針による。設定画面でONにしたときは
`regulation_bbox = None` にして次のポーリングで即取得させる(`src/ui.rs:1500-1503`)。
専用のキーバインドとメニュー項目は無い(設定画面のみ)。

## 5. 制約

- **1回の更新で HTTP リクエストが「HTML 1回 + メッシュ数」発生する**。1次メッシュは約80km四方なので、
  広域ズームで表示すると1回の更新で数本〜十数本のリクエストになる。90秒ごとに繰り返す。
- 種別しか使っていない。原因(`genin_jisho_cd`)・規制開始日時(`kisei_kaishi_nichiji`)・
  実施状況(`kisei_jishi_jyokyo`)・道路コード(`doro_cd`)はパースしていないため、
  「事故なのか工事なのか冬期閉鎖なのか」「いつからいつまでか」は画面に出ない。
  原因コードの文言対応表も特定できていない。
- 線を引くだけで、クリック/選択して詳細を見る導線が無い(`RegulationKind::label()` は未使用)。
- 表示が「今の規制とは限らない」場合がある(オフライン時は最大24時間前の内容を出す)。
  そのときはステータス行に経過時間が付く(`⚠3件(2時間前)`)。線の色は変えていないので、
  色だけでは新旧を区別できない。
- 配信元パス(`backup/{timestamp}/{hash}/`)は更新のたびに変わるため、HTML の構造が変わると
  全件取得できなくなる。影響は `TUKOKISEI_PAGE` と `extract_json_base` に閉じている。
- 非公式エンドポイントのため、提供そのものが予告なく止まりうる。

## 6. テスト (`src/regulation.rs` の `mod tests`)

- 1次メッシュコードの計算(東京 → `5339` 等)は `src/mesh.rs` の `mod tests` へ移した。
- `extract_json_base`: `../backup/{timestamp}/{hash}/` を切り出す / 無いときは `None`。
- `RegulationKind::from_code`: 既知コード5種 + `08` + 未知値(`99` → `Other`)。
- `parse_closures`: 実 JSON の抜粋(2026/08/16 実測)から種別と `(lat, lon)` 順の線を取り出す /
  1点しかない要素を除外する / 壊れた入力・空配列・`geo_json` 欠如で空 Vec。
- serde: `ClosureEvent` の JSON 往復(`{"line":[[lat,lon],..],"kind":"Closed"}`)/ 全種別の往復。
- `live_fetch_real_regulation_data` は `#[ignore]`(実ネットワーク・`cargo test --release -- --ignored`)。

ステータス行のラベル分岐は `src/ui_status.rs` 側のテスト
(`regulation_label_distinguishes_loading_from_no_regulations`)でカバーされている。

## 7. 残作業

- `README.md` と `docs/MANUAL.md`、ヘルプ(`src/keymap.rs`)にこの機能の記載が無い。
- 凡例が無いため、色と種別の対応が画面から分からない(現状は本仕様書 §2.1 のみ)。
