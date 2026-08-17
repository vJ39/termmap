ルート確定後、Google Directions(`departure_time=now`)でルート上の区間ごとの渋滞状況を
問い合わせ、混雑している区間だけルート線を上塗り表示する機能の仕様。

前提: Google Maps JavaScript APIの`TrafficLayer`は表示専用ウィジェットで、裏側の混雑データを
取り出す手段が公式に用意されていない → 一般道路網を面で色分けする方式は不可能。Directions APIの
`duration_in_traffic`(区間=leg単位)が、個人開発で現実的に使える唯一のデータ源 →
**選択中のルート自身を区間分割して問い合わせ、混雑区間だけ上塗りする**方式に絞る。

## 1. 決定事項

| # | 論点 | 結論 |
|---|---|---|
| 1 | 対象 | BRouterで確定したルート`pts`を距離ベースで区間分割し、区間境界を中間waypointとして**1回のDirections問い合わせ**で全区間の`duration`/`duration_in_traffic`をまとめて取得する(legごとに個別APIコールはしない) |
| 2 | 区間数 | 目標区間長5km、ルート総延長から算出(最小1・最大24=Google Directionsの中間waypoint上限23+終点側1区間) |
| 3 | 色分け | `duration_in_traffic / duration`の比で3段階: 1.15未満=順調(**上塗りしない=ルート基調色の青のまま**)、1.15〜1.5未満=黄(やや混雑)、1.5以上=赤(混雑)。`duration_in_traffic`欠損は順調(上塗りなし)扱い(過剰に混雑扱いしない側へ倒す) |
| 4 | 設定 | トグル`route_traffic_enabled`(既定false)。ラベル「渋滞状況の色分け」 |
| 5 | 発火 | ルート成功時(route_jobの結果処理)、設定ON かつ Google APIキー設定済み かつ`pts.len()>=2`の時だけ非同期で1回問い合わせる |
| 6 | 表示 | ルート本体(`spec.routes`、基調色の青)はそのまま。混雑区間(黄/赤)だけを`spec.traffic_segments`として重ね描きする(順調区間は何も描かないので基調色の青が見える)。1件でも混雑区間があればroute_noteに「(渋滞あり: 黄/赤)」を一言追記 |
| 7 | 失敗時・全区間順調時 | 静かに諦め、基調色ルート線のまま。全区間順調(混雑無し)の場合も同様に何も上塗りしない(結果的に失敗時と見分けはつかないが、どちらも「特筆すべき混雑は無い」という点で実害はない) |
| 8 | 通行止め表示との視認性 | `RegulationKind::Closed`(通行止め、regulation.rs)の色は黒`[0,0,0]`。混雑(赤)と混同しない |

## 2. 実装

### 2.1 route.rs: 純関数

```rust
// ルート総延長(m)から、渋滞問い合わせに使う区間数を決める(1以上、TRAFFIC_MAX_WAYPOINTS+1以下)。
// 目標区間長 TRAFFIC_SEGMENT_TARGET_M=5000.0 で割った丸め値をクランプする。
pub fn traffic_segment_count(total_len_m: f64) -> usize

// 区間境界の累積距離(m)の配列(区間数-1個。0とtotal_len_mは含まない)。
pub fn traffic_breakpoints_m(total_len_m: f64, segments: usize) -> Vec<f64>

// 区間境界のwaypoint座標。roadtrace::point_atで pts 上の対応点を引く薄いラッパー。
pub fn traffic_waypoints(pts: &[(f64, f64)], breakpoints_m: &[f64]) -> Vec<(f64, f64)>

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrafficLevel { Smooth, Moderate, Heavy }

// duration_in_traffic/durationの比で3段階に分類する。
// duration_s<=0.0 または duration_in_traffic_s 欠損なら Smooth 扱い。
pub fn traffic_level(duration_s: f64, duration_in_traffic_s: Option<f64>) -> TrafficLevel

// Moderate/Heavyの色。Smoothの色は呼び出し側で上塗りしないため到達しない
// (列挙の網羅性のためだけに値を持つ)。
pub fn traffic_level_color(level: TrafficLevel) -> [u8; 3]

// legs(区間ごとのduration/duration_in_traffic、既存parse_directions_legsの戻り値と同じ形)を
// pts に沿って色分けした(色, 点列)の列へ変換する。Smooth区間はエントリを作らない
// (基調色の青のまま=何も上塗りしない)。legs.len() != breakpoints_m.len()+1 なら空Vec。
pub fn colorize_route_by_traffic(
    pts: &[(f64, f64)],
    breakpoints_m: &[f64],
    legs: &[(f64, Option<f64>)],
) -> Vec<([u8; 3], Vec<(f64, f64)>)>
```

- 出力される区間の点列は隣接区間と境界点を共有する(線が途切れて見えないように)。
  ただしSmooth区間を挟んだ場合はその限りではない(間が基調色の青のまま途切れて見えるのが正しい)
- Smooth区間をスキップしても内部の位置合わせ(スキップ区間の終点=次に出力する区間の始点)は保持する

### 2.2 route.rs: ネットワーク関数

```rust
pub type TrafficColorRx = std::sync::mpsc::Receiver<Vec<([u8; 3], Vec<(f64, f64)>)>>;
pub fn trigger_traffic_coloring(pts: &[(f64, f64)], mode: &str, key: &str) -> TrafficColorRx

// 失敗しても呼び出し側は「色分けなし」に静かにフォールバックできるよう常に(空なら空)Vecを返す。
fn fetch_traffic_coloring(pts: &[(f64, f64)], mode: &str, key: &str) -> Vec<([u8; 3], Vec<(f64, f64)>)>
```

`fetch_traffic_coloring`の内部手順:
1. `key`が空、または`pts.len()<2`なら空Vec
2. `total_len = roadtrace::polyline_len(pts)`、`segments = traffic_segment_count(total_len)`、
   `breakpoints = traffic_breakpoints_m(total_len, segments)`、`waypoints = traffic_waypoints(pts, breakpoints)`
3. `via_wps = [pts[0]] + waypoints + [pts[last]]`を組み立て、`directions_common_params(&via_wps, mode, key)`
   (origin/destination/waypoints/avoid=highwaysの組み立てを`fetch_google_route`と共有)に
   `&departure_time=now`を追加してGoogle Directionsへ問い合わせる
4. `parse_directions_legs`でlegsを取り出し、`colorize_route_by_traffic(pts, &breakpoints, &legs)`で
   色分け結果を作る。パース失敗・legs件数不一致は空Vec

### 2.3 ui.rs

- state: `traffic_color_job: Option<route::TrafficColorRx>`
- route_job成功時、`cfg.route_traffic_enabled && !cfg.google_maps_api_key.trim().is_empty() && r.pts.len() >= 2`
  なら`trigger_traffic_coloring(&r.pts, &mode, &cfg.google_maps_api_key)`を起動。ルートが変わるたびに
  `spec.traffic_segments`をクリアしてから再発火する(古いルートの色分けを引き継がない)
- ポーリングで色分け結果を受け取ったら`spec.traffic_segments`(独立レイヤ、`spec.routes`は触らない)へ
  差し替える。結果が空でなければroute_noteに「(渋滞あり: 黄/赤)」を一言追記。結果が空
  (失敗・対象外・全区間順調)なら何もしない
- jobs_active/polling/Ctrl-C中断チェーンに`traffic_color_job`を含める

### 2.4 render.rs

`OverlaySpec`に`traffic_segments: Vec<Route>`を追加(routes/roadsとは独立レイヤ)。GPX保存・
標高表示・次の曲がり案内は`spec.routes.last()`を「ルート全体」として参照しているため、
`spec.routes`自体は差し替えず、色分け結果は別レイヤとして`roads`の後・`pois`の前に重ね描きする。

### 2.5 settings.rs

- idx=28、ラベル「渋滞状況の色分け」
- 説明文: 「渋滞状況の色分け: ルート確定後、Google Directionsで区間ごとの渋滞状況を追加確認し、
  混雑している区間だけルート線を黄(やや混雑)/赤(混雑)で上塗りする(順調な区間は基調色の青の
  まま)。道路網全体ではなく表示中のルートのみ。要Google APIキー。区間数に応じて1回のAdvanced
  課金対象リクエストを送る(無料枠超過分は1000件$8、個人利用なら通常は無料枠内)」

### 2.6 regulation.rs

`RegulationKind::Closed.color()`は黒`[0, 0, 0]`(混雑の赤と混同しないため)。他の種別
(LaneRestriction等)の色は変えない。

## 3. テスト

- `traffic_segment_count`: 0以下→1 / 5km未満の短いルート→1 / ちょうど5km区切りになるケース /
  上限(TRAFFIC_MAX_WAYPOINTS+1=24)に達する長大ルートでクランプされること
- `traffic_breakpoints_m`: segments=1→空Vec / segments=3で総延長を3等分した2点になること
- `traffic_waypoints`: 各breakpointに対しroadtrace::point_atと同じ座標が返ること
- `traffic_level`: ratio<1.15→Smooth / 1.15〜1.5未満境界→Moderate / 1.5以上→Heavy /
  duration_in_traffic欠損→Smooth / duration<=0→Smooth
- `traffic_level_color`: Moderate/Heavyが異なる色であること
- `colorize_route_by_traffic`: 全区間Smooth→空Vec / Smooth区間はエントリを作らず
  Moderate/Heavyのみエントリが返ること / Smooth区間を挟むと前後のエントリの境界点が
  つながらない(途切れる)こと / 件数不一致なら空Vecを返すこと
- `RegulationKind::Closed.color()`が黒`[0,0,0]`であること(regulation.rs側)
- `fetch_traffic_coloring`はネットワーク関数のため単体テスト対象外(実データでのlive
  `#[ignore]`テストで確認する)

## 4. 対象外(今回はやらない)

- 一般道路網全体の広域渋滞オーバーレイ(Google側に相当するAPIが無いため不可能)。
  ルートを引いていない状態で周辺の主要道路の渋滞状況を見たいという要望は別途あり、その場合は
  固定の「監視区間」リストに対して今回と同じ区間色分けの仕組みを使う案がある。次回検討する
  パラメータ(会話でユーザーから指定済み):
  - 監視区間の本数はズーム(拡大)状況に応じて絞る
  - 表示範囲があまりに広域な場合はそもそもGoogle Directionsへ問い合わせない
  - 更新間隔30分。取得結果はTTL30分でディスクにも保存する(再起動しても直前の状態を
    失わないため)。ただしBRouterルートキャッシュ(route_cache_path)のような無期限保存は
    しない=読み出し時に必ず鮮度(取得時刻からの経過)を確認し、TTLを超えていれば
    キャッシュミス扱いで再取得する。無期限保存はしないという点が
    [[feedback-fallback-result-cache-poisoning]]の教訓を踏まえた要件
- 表示後の周期的な再取得(渋滞状況の経時変化への追従)。ルート確定時の1回取得のみ
- BRouterが失敗しGoogleへフォールバックした場合の色分け(フォールバック結果はそもそも
  ディスクキャッシュしない方針[[feedback-fallback-result-cache-poisoning]]で、かつ
  `fetch_google_route`はdeparture_timeを付けていないため対象外)
