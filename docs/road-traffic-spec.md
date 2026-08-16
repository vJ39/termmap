JARTIC「交通量オープンデータ」の常時観測点(直轄国道)の5分値を取得し、混雑度の目安として
地図に重ねる。観測点は最寄りの主要道路ラインへスナップし、その前後の区間を混雑度の色で塗る
「線」表示が基本(§8)。周囲500m以内に道路データが無い場合は従来通り点(リング)で表示する。

対象コード: `src/traffic.rs` / `src/roadsearch.rs` の `fetch_major_roads` / `src/roadtrace.rs` の
`nearest_way_segment` / `src/ui.rs` の `traffic_*`・`major_roads_*`。

## 1. 実装状況

| 機能 | データ層 | ui.rs 配線 | 描画 | 状態 |
|---|---|---|---|---|
| 観測点を道路ラインへスナップして線で色分け | 実装済み(`fetch_major_roads` / `nearest_way_segment_within`) | 実装済み | 実装済み(`draw_line`) | **実装済み** |
| 観測点を点で色分け表示(フォールバック) | 実装済み(`traffic.rs`) | 実装済み | 実装済み(リング) | **実装済み** |

線色付けの詳細は §8 に分けて書く。`cargo check` に warning は出ない。

## 2. データソース

| 項目 | 値 |
|---|---|
| エンドポイント | `https://api.jartic-open-traffic.org/geoserver` (WFS / GeoServer) |
| リクエスト | `service=WFS&version=2.0.0&request=GetFeature&typeNames=t_travospublic_measure_5m&srsName=EPSG:4326&outputFormat=application/json&exceptions=application/json&cql_filter=...` |
| 認証 | 不要(無料・登録不要。利用規約への同意のみ) |
| User-Agent | `termmap/0.1 (personal experiment)` |
| タイムアウト | 20秒 (`HTTP_TIMEOUT_SECS`) |
| 依存 | `std` + `ureq` + `serde_json` のみ。`crate::` を参照しない(`gpslive.rs`/`radar.rs` と同方針) |

実測で確認済みの注意点(`src/traffic.rs` 冒頭のコメントに記載):

- 時間コードを絞らずに叩くとバックエンドが OOM で落ちるため、`CQL_FILTER` に時間範囲が必須。
- 観測から取得可能になるまで約20分のラグがある。直近すぎる時刻を指定すると0件になる。
- このデータセットの道路種別は実測で `"3"`(一般国道)のみ。高速道路は含まれない。
- 事故情報・区間旅行時間・交通規制はこのデータセットに含まれない(規制は `regulation.rs` 側で別途取得する)。

### 2.1 時間窓の作り方

`fetch_traffic()` は現在時刻(UTC)を JST(+9h)へ直し、時間コード `YYYYMMDDHHMM` を組む。

| 定数 | 値 | 意味 |
|---|---|---|
| `OBSERVE_LAG_MIN` | 25 | 観測から取得可能になるまでのラグ(公式案内は約20分)を安全側に見た値。窓の終端 |
| `WINDOW_MIN` | 10 | 窓の幅。`tc_from = tc_to - 10分` |

`CQL_FILTER` は `時間コード>={from} AND 時間コード<={to} AND BBOX(ジオメトリ,{lon_min},{lat_min},{lon_max},{lat_max},'EPSG:4326')`。
日本語を含むためパーセントエンコードは自前実装(`urlencode`、英数字と `-_.~` 以外を全てエンコード)。
暦日変換は `civil_from_days`(Howard Hinnant)で、`chrono` 等には依存しない。
テスト容易性のため、基準時刻(epoch分・UTC)を引数で受ける `fetch_traffic_at()` を内部で分離している。

## 3. データ層 (`src/traffic.rs`)

```rust
pub struct TrafficPoint { pub lat: f64, pub lon: f64, pub volume: u32 }
pub enum CongestionLevel { Light, Moderate, Heavy }

pub fn fetch_traffic(lat_min, lon_min, lat_max, lon_max) -> Result<Vec<TrafficPoint>, String>
pub fn parse_traffic(body: &str) -> Vec<TrafficPoint>   // ネットワークに触れない純関数
pub fn classify(volume: u32) -> CongestionLevel
```

### 3.1 パース規則

- 座標は `/geometry/coordinates/0` の `[lon, lat]`(ジオメトリは MultiPoint)。
- `volume` = 上り+下り × (小型交通量 + 大型交通量 + 車種判別不能交通量) の合計(5分間の実測台数)。
- **欠測方向は0として加算せず、その方向を丸ごと足さない**。`{方向}・欠測` が `"1"` のときが対象。
  センサー故障で「空いている」と誤表示するのを避けるため。
- 壊れた JSON・`features` キー無し・座標欠損の feature は黙って捨て、常に `Vec` を返す(panic しない)。

### 3.2 混雑度の判定

| level | 条件 | 描画色(RGB) |
|---|---|---|
| `Heavy` | `volume >= 150` | `[220, 50, 40]` |
| `Moderate` | `60 <= volume < 150` | `[230, 200, 40]` |
| `Light` | `volume < 60` | `[80, 200, 90]` |

閾値は実測サンプル(直轄国道の5分値、概ね0〜300台)から見た経験則で、道路容量による正規化ではない。
色は `ui.rs` の描画側が持つ(`traffic.rs` は色を持たない)。

## 4. ui.rs の配線

### 4.1 状態 (`src/ui.rs:130-141`)

```rust
traffic_points: Vec<traffic::TrafficPoint>
traffic_job: Option<Receiver<Result<Vec<TrafficPoint>, String>>>
traffic_last_fetch: Option<Instant>
traffic_bbox: Option<(lat_min, lon_min, lat_max, lon_max)>   // 直近フェッチ範囲
```

### 4.2 取得スケジュール (`src/ui.rs:951-981`)

`cfg.traffic_enabled` かつジョブが走っていないときだけ判定する。ライブカメラ・通行規制も
同じ形の独立ブロックを持っており(3箇所で同じ定数を使う)、判定規則は共通。

| 項目 | 値 |
|---|---|
| 取得範囲 | 画面中心から `MARGIN_PX = 900` ピクセル分を広げた bbox(現在のズームで `pixel_to_deg`) |
| 定期更新 | `REFRESH = 90秒`。前回取得からこれ以上経っていれば再取得 |
| 即時更新 | 画面中心が `traffic_bbox` の外に出たら間隔を待たずに再取得 |
| 実行方法 | `std::thread::spawn` + `mpsc`。結果は `try_recv()` で毎ループ回収する |

`traffic_job` が生きている間は入力待ちをポーリング側に倒す(`src/ui.rs:1253`)。これが無いと、
キー入力が無いまま取得が完了した場合に結果が最大 `IDLE_SAVE_INTERVAL`(60秒)反映されない。

### 4.3 失敗時のフォールバック

`fetch_traffic` が `Err` を返した場合、ジョブを片付けるだけで **`traffic_points` は前回の値のまま**にする
(`src/ui.rs:977`)。通信エラーで表示が消えない。エラー文言は画面に出さない。

### 4.4 描画 (`src/ui.rs:777-806` 付近)

`cfg.traffic_enabled` のときだけ、`traffic_points` の各点について `roadtrace::nearest_way_segment_within`
で最寄りの主要道路区間を探し、見つかれば `draw_line` でその区間を `classify()` の色に塗る(§8)。
道路データが無い/500m以内に見つからない場合は `draw_ring(ov, ix, iy, 3, color, 3)` の点表示(半径3・
太さ3のリング)にフォールバックする。ルート・waypoint より後に描くので前面に出る。描画対象は共通の
`OverlayLayer` なので、halfblock / 実画像 / braille / edge のどの描画モードでも同じ経路を通る。

### 4.5 ステータス行 (`src/ui_status.rs:147-155`)

| 状態 | 表示 |
|---|---|
| OFF | (何も出さない) |
| 0件かつ取得中 | `🚗取得中… ` |
| 0件かつ取得完了 | `🚗観測点無し ` |
| 取得済み | `🚗{件数}地点 ` |

取得済みなら取得中でも件数を優先して出す。0件を「圏外/観測点無し」と区別できるようにしてあるのは、
このデータが直轄国道の観測点のみで、それ以外の道路には点が存在しないため。

## 5. 設定

| 場所 | 値 |
|---|---|
| 設定画面(`,`)の行 | 22 `道路交通量 ON/OFF`(`src/settings.rs:159`、説明文は `setting_description(22)`) |
| `Config` フィールド | `traffic_enabled: bool`。既定 `false` |
| `config.toml` | `[traffic] enabled = false` |

既定 OFF は「ONにした人だけが外部サービスへ問い合わせる」方針による。設定画面でONにしたときは
`traffic_bbox = None` にして、次のポーリングで待ち時間なく取得させる(`src/ui.rs:1491-1494`)。
専用のキーバインドは無い(設定画面のみ)。

## 6. 制約

- 直轄国道の常時観測点のみ。高速道路・都道府県道・市町村道には点が出ない。
- 「渋滞度」ではなく通過台数。道路容量で正規化していないため、車線数の違う道を横並びに比較できない。
- 約20分のラグがあり、リアルタイムの詰まりは映らない。
- 事故・規制・旅行時間は取得できない(JARTIC のこのデータセットに無い)。
- 90秒ごとに画面周辺の bbox を丸ごと取り直す。画面外にパンしたら即時再取得するため、
  広域を素早く動かすとリクエストが続けて出る。

## 7. テスト (`src/traffic.rs` の `mod tests`)

- `civil_from_days` と `days_from_civil` の往復。
- `urlencode` が非ASCII・記号をエスケープする。
- `parse_traffic`: 実応答の抜粋(2026/08/16 実測、3件)で lat/lon と上下線合計、欠測方向の非加算、
  壊れた入力(`not json` / `{}` / `features` 空)で空 Vec。
- `classify` の閾値(0/59/60/149/150)。
- `live_fetch_real_jartic_api` は `#[ignore]`。実ネットワークを叩く手動確認用
  (`cargo test --release -- --ignored`)。

## 8. 道路交通量のライン色付け表示

観測点を点で置くだけの表示は、道路がどこまで混んでいるかが読み取りにくい。観測点を最寄りの
主要道路ラインへスナップし、その前後の区間を混雑度の色で塗る。

### 8.1 実装済み: データ取得基盤

**`roadsearch::fetch_major_roads(s, w, n, e)` (`src/roadsearch.rs:126-143`)**

```
[out:json][timeout:25];way["highway"~"^(trunk|primary)$"]({bbox});out geom;
```

- Overpass API (`https://overpass-api.de/api/interpreter`)。UA `termmap/0.1 (personal experiment)`、
  `Accept: application/json`、タイムアウト20秒。
- 既存の `fetch()`(道路名/ref で1本の道を検索する機能)と違い、名前や ref では絞らずタグだけで広く取る。
  JARTIC の観測点には路線名が入っていないため、まず近くの主要道路を全部集めてから最寄りを選ぶ方式にした。
- 戻り値は `Vec<(Vec<(lat, lon)>, oneway)>`。パースは既存の `parse_road_fragments` を共用する
  (壊れた JSON・`elements` 無し・`geometry` 欠如は空/スキップ)。

**`roadtrace::nearest_way_segment` / `nearest_way_segment_within(ways, point, radius, max_dist_m)`**

- 複数の道路断片(それぞれ別の道)の全頂点から `point` に最も近い頂点を線形探索し、その道の中で
  頂点の前後 `radius` 個ぶんの部分列を返す。端では `saturating_sub` / `min` でクランプする。
- `nearest_way_segment_within` は最寄り頂点までの距離が `max_dist_m` を超えたら空 Vec を返す
  (周囲に該当道路が無い観測点を、無関係な遠い道へスナップしないための上限)。
  `nearest_way_segment` は `nearest_way_segment_within(..., f64::INFINITY)` の薄いラッパー。
- 距離は Haversine(地球半径6371km)。`ways` が空、または全断片が空なら空 Vec。
- `std` のみに依存(外部 crate・`crate::` 参照なし)。

**ui.rs の取得ジョブ (`src/ui.rs:136-141, 982-1012`)**

```rust
major_roads: Vec<Vec<(f64, f64)>>          // 断片ごとの点列。oneway は捨てている
major_roads_job: Option<Receiver<Result<Vec<(Vec<(f64,f64)>, bool)>, String>>>
major_roads_last_fetch: Option<Instant>
major_roads_bbox: Option<(f64, f64, f64, f64)>
```

- 発火条件は `cfg.traffic_enabled`(交通量と同じフラグに相乗り。専用の設定項目は作っていない)。
- 間引き規則は §4.2 と完全に同じ(`MARGIN_PX = 900` / `REFRESH = 90秒` / bbox 外で即時)。
- 受信時に `oneway` を捨てて `Vec<Vec<(lat, lon)>>` へ畳む(`src/ui.rs:1007`)。
- 取得失敗時は前回の道路データをそのまま使う(`src/ui.rs:1008`)。

### 8.2 実装済み: ライン描画本体 (`src/ui.rs:777-806` 付近)

各 `TrafficPoint` について `roadtrace::nearest_way_segment_within(&major_roads, (p.lat, p.lon),
TRAFFIC_SNAP_RADIUS, TRAFFIC_SNAP_MAX_DIST_M)` を呼び、2点以上返れば `deg_to_pixel` で画面座標へ
直して `draw_line` で `classify()` の色を塗る。1点以下(道路データ未取得、または最寄り道路が
`TRAFFIC_SNAP_MAX_DIST_M` より遠い)なら、従来通り `draw_ring` の点表示にフォールバックする。

| 定数 | 値 | 意味 |
|---|---|---|
| `TRAFFIC_SNAP_RADIUS` | 15 | 最寄り頂点の前後何個ぶんを区間として塗るか |
| `TRAFFIC_SNAP_MAX_DIST_M` | 500.0 | 最寄り頂点までの距離がこれを超えたら点表示にフォールバック |

`TRAFFIC_SNAP_RADIUS` は頂点数であって距離ではない。2026/08/16、東京都内(35.6-35.8,
139.6-139.9)の `trunk|primary` way(5530本)を実測したところ、隣接ノード間隔は中央値約20m・
90%タイルで74m、1way平均7ノード。したがって実際に塗られる区間の長さは道によってばらつく
(短い way では前後にはみ出さずクランプされ、way 全体がそのまま描かれる)。距離ベースへ
正規化する改善は §8.3 に残す。

### 8.3 既知の制約・今後の改善余地

1. **区間長がばらつく**: `radius` が頂点数ベースのため、同じ設定値でも道によって塗られる
   区間の実長が変わる。距離ベース(例: 前後500m)へ直すか、`roadtrace::sample_every` で
   正規化してから使う改善余地がある。
2. **点表示との情報の混同**: 観測点は「その1点での実測」だが、線にすると区間全体がその値だと
   誤読される余地がある。現状は割り切って線で塗っている。
3. **再描画トリガ**: `major_roads` や `traffic_points` の更新だけでは `map_sig`(`src/ui.rs:676-715`)
   が変わらない。地図操作中(パン/ズーム/GPS移動)は他の要因で頻繁に `map_sig` が変わるため
   実用上は反映されるが、地図を完全に静止させたまま90秒待った場合、次に何か別の要因で
   `map_sig` が変わるまで新しいデータが画面に反映されない可能性がある(交通量・カメラ・
   通行規制のいずれも共通の既存の制約で、今回の変更で新たに生じたものではない)。
4. **性能**: `nearest_way_segment_within` は観測点ごとに全 way の全頂点を線形走査する
   (O(観測点数 × 総頂点数))。実測(§8.2)では `trunk|primary` の頂点は東京都内相当の範囲で
   約3.9万点、画面内の観測点は通常数十件程度のため実害は出ていないが、より広域・高密度な
   場面で重くなるようなら、取得完了時に一度だけ計算してキャッシュする形へ変える。
5. **Overpass の負荷**: 90秒ごとに画面周辺の `trunk|primary` を丸ごと取り直している。
   交通量側と同じ間隔に相乗りしているが、道路形状は交通量ほど頻繁に変わらないため、
   取得間隔を分けて負荷を下げる余地がある。
6. **捨てている情報**: `oneway` は受信時に捨てている。上り/下りで別々に色を出すなら、
   `TrafficPoint` 側も方向別の台数を持つ形へ広げる必要がある(現在は上下合計のみ)。

### 8.4 テスト状況

- `roadsearch::parse_road_fragments`: 空入力・壊れた JSON・途中で切れた JSON・点列と `oneway` の抽出・
  `geometry` 欠如のスキップ・`oneway` が `yes` のときだけ true。
- `roadsearch::live_fetch_major_roads_returns_real_ways` は `#[ignore]`(実ネットワーク)。
- `roadtrace::nearest_way_segment` / `nearest_way_segment_within`: 近い方の way を選ぶ・頂点の
  前後を窓で切る・端でクランプする・空入力で空 Vec・距離上限を超えたら空 Vec・上限無しの
  `nearest_way_segment` は `nearest_way_segment_within(..., f64::INFINITY)` と同じ結果になる。
- `src/ui.rs` の描画分岐(ライン/フォールバック点の切り替え)自体には専用のユニットテストが無い
  (`interactive()` 本体はテストが難しい既存構造。`regulation`/`camera` の描画分岐も同様)。
