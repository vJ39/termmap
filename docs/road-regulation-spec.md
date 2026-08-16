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

`primary_mesh_codes(lat_min, lon_min, lat_max, lon_max)` が bbox を覆う全コードを返す。
`lat_min > lat_max` のような不正な範囲では空 Vec を返し、1回も通信しない。

## 2. データ層 (`src/regulation.rs`)

```rust
pub enum RegulationKind { Closed, LaneRestriction, AlternatingOneLane, ChainRequired, MovementRestriction, Other }

pub struct ClosureEvent {
    pub line: Vec<(f64, f64)>,   // (lat, lon) の順。ソースは lon,lat 順なので変換する
    pub kind: RegulationKind,
}

pub fn fetch_closures(lat_min, lon_min, lat_max, lon_max) -> Vec<ClosureEvent>
pub fn parse_closures(body: &str) -> Vec<ClosureEvent>   // ネットワークに触れない純関数
```

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

`fetch_closures` は**常に `Vec` を返す**(`Result` にしていない)。

| 失敗箇所 | 挙動 |
|---|---|
| メッシュコードが0件 | 空 Vec(通信しない) |
| HTML の取得・本文化に失敗 | 空 Vec |
| `extract_json_base` が見つけられない | 空 Vec |
| 個別メッシュの取得に失敗 | **そのメッシュだけ諦めて次へ**(全体を巻き添えにしない) |

## 3. ui.rs の配線

### 3.1 状態 (`src/ui.rs:151-155`)

```rust
regulation_events: Vec<regulation::ClosureEvent>
regulation_job: Option<Receiver<Vec<regulation::ClosureEvent>>>
regulation_last_fetch: Option<Instant>
regulation_bbox: Option<(lat_min, lon_min, lat_max, lon_max)>
```

### 3.2 取得スケジュール (`src/ui.rs:1042-1070`)

道路交通量・ライブカメラと同じ形の独立ブロック。

| 項目 | 値 |
|---|---|
| 発火条件 | `cfg.regulation_enabled` かつ `regulation_job` が無い |
| 取得範囲 | 画面中心から `MARGIN_PX = 900` ピクセル分を広げた bbox |
| 定期更新 | `REFRESH = 90秒` |
| 即時更新 | 画面中心が `regulation_bbox` の外に出たとき |
| 実行方法 | `std::thread::spawn` + `mpsc`、毎ループ `try_recv()` |

`regulation_job` が生きている間はポーリング側に倒す(`src/ui.rs:1253`)。キー入力が無いまま取得が
完了しても結果が反映される。

受信すると `regulation_events` を丸ごと置き換える。`fetch_closures` は失敗を空 Vec で返すので、
通信に失敗した回は線が消え、ステータス行が `⚠規制無し` になる(交通量のように前回値を保持しない)。

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
- 取得失敗が空 Vec と区別できないため、通信断と規制0件が同じ表示(`⚠規制無し`)になる。
- 配信元パス(`backup/{timestamp}/{hash}/`)は更新のたびに変わるため、HTML の構造が変わると
  全件取得できなくなる。影響は `TUKOKISEI_PAGE` と `extract_json_base` に閉じている。
- 非公式エンドポイントのため、提供そのものが予告なく止まりうる。

## 6. テスト (`src/regulation.rs` の `mod tests`)

- `primary_mesh_codes`: 東京(35.68, 139.77) → `5339` / 複数セルにまたがる範囲で複数コード /
  `lat_min > lat_max` の不正範囲で空。
- `extract_json_base`: `../backup/{timestamp}/{hash}/` を切り出す / 無いときは `None`。
- `RegulationKind::from_code`: 既知コード5種 + `08` + 未知値(`99` → `Other`)。
- `parse_closures`: 実 JSON の抜粋(2026/08/16 実測)から種別と `(lat, lon)` 順の線を取り出す /
  1点しかない要素を除外する / 壊れた入力・空配列・`geo_json` 欠如で空 Vec。
- `live_fetch_real_regulation_data` は `#[ignore]`(実ネットワーク・`cargo test --release -- --ignored`)。

ステータス行のラベル分岐は `src/ui_status.rs` 側のテスト
(`regulation_label_distinguishes_loading_from_no_regulations`)でカバーされている。

## 7. 残作業

- `README.md` と `docs/MANUAL.md`、ヘルプ(`src/keymap.rs`)にこの機能の記載が無い。
- 凡例が無いため、色と種別の対応が画面から分からない(現状は本仕様書 §2.1 のみ)。
