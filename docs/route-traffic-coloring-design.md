ルート確定後、Google Directions(`departure_time=now`)でルート上の区間ごとの渋滞状況を
問い合わせ、ルート線を緑/黄/赤に色分け表示する機能の設計。

**この文書は旧設計(テキストで「(現在+8分の遅れ)」と追記するだけの版)を置き換える。**
実装後にユーザーへ見せたところ「道が赤くなるのを期待していた」と、テキスト追記では
イメージと違うことが判明したため(参考: camera-map.com/around-trafficのGoogleマップ
埋め込みウィジェット、緑/黄/赤の道路色分け)。前提調査は会話内で完了済み:

- Google Maps JavaScript APIの`TrafficLayer`は表示専用ウィジェットで、裏側の混雑データを
  取り出す手段が公式に用意されていない → 一般道路網を面で色分けする方式は不可能
- Directions APIの`duration_in_traffic`(区間=leg単位)が、個人開発で現実的に使える
  唯一のデータ源 → **選択中のルート自身を区間分割して問い合わせ、区間ごとに色分けする**
  方式に絞る(道路網全体の色分けではなく、あくまで表示中のルート線だけ)

## 1. 決定事項

| # | 論点 | 結論 |
|---|---|---|
| 1 | 対象 | BRouterで確定したルート`pts`を距離ベースで区間分割し、区間境界を中間waypointとして**1回のDirections問い合わせ**で全区間の`duration`/`duration_in_traffic`をまとめて取得する(legごとに個別APIコールはしない) |
| 2 | 区間数 | 目標区間長5km、ルート総延長から算出(最小1・最大24=Google Directionsの中間waypoint上限23+終点側1区間) |
| 3 | 色分け | `duration_in_traffic / duration`の比で3段階: 1.15未満=緑(順調)、1.15〜1.5未満=黄(やや混雑)、1.5以上=赤(混雑)。`duration_in_traffic`欠損は緑扱い(過剰に赤く塗らない側へ倒す) |
| 4 | 設定 | 既存トグル`route_traffic_enabled`のラベル・説明文を変更(「渋滞込み所要時間」→「渋滞状況の色分け」)。フィールド名・保存形式は変えない |
| 5 | 発火 | ルート成功時(route_jobの結果処理)、設定ON かつ Google APIキー設定済み かつ`pts.len()>=2`の時だけ非同期で1回問い合わせる。既存の`traffic_delay_job`(遅延テキスト版)は撤去 |
| 6 | 表示 | 取得成功時、`spec.routes`に積む単色1本のRouteを、区間ごとに色分けした複数本のRouteへ差し替える。route_noteに凡例を一言追記「(渋滞: 緑/黄/赤)」 |
| 7 | 失敗時 | 静かに諦め、従来通りの単色ルート線のまま(BRouter結果自体は壊さない) |

## 2. 実装対象

### 2.1 route.rs: 純関数(まずテストから書く)

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
pub fn traffic_level(duration_s: f64, duration_in_traffic_s: Option<f64>) -> TrafficLevel

pub fn traffic_level_color(level: TrafficLevel) -> [u8; 3]

// legs(区間ごとのduration/duration_in_traffic、既存parse_directions_legsの戻り値と同じ形)を
// pts に沿って色分けした(色, 点列)の列へ変換する。legs.len() != breakpoints_m.len()+1 なら
// 空Vecを返す(呼び出し側は従来の単色描画にフォールバックする)。
pub fn colorize_route_by_traffic(
    pts: &[(f64, f64)],
    breakpoints_m: &[f64],
    legs: &[(f64, Option<f64>)],
) -> Vec<([u8; 3], Vec<(f64, f64)>)>
```

- 区間の点列は隣接区間と境界点を共有する(線が途切れて見えないように、境界点を両側に含める)
- `traffic_level`は`duration_s<=0.0`または`duration_in_traffic_s`欠損ならSmooth扱い

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
3. `via_wps = [pts[0]] + waypoints + [pts[last]]`を組み立て、既存`directions_common_params(&via_wps, mode, key)`
   (origin/destination/waypoints/avoid=highwaysの組み立てを共有)に`&departure_time=now`を追加してGoogle
   Directionsへ問い合わせる(旧`fetch_traffic_delay`と同じ土台)
4. 既存`parse_directions_legs`でlegsを取り出し、`colorize_route_by_traffic(pts, &breakpoints, &legs)`で
   色分け結果を作る。パース失敗・legs件数不一致は空Vec

### 2.3 ui.rs

- 既存の`traffic_delay_job: Option<route::TrafficDelayRx>`を`traffic_color_job: Option<route::TrafficColorRx>`へ置換
- route_job成功時、`cfg.route_traffic_enabled && !cfg.google_maps_api_key.trim().is_empty() && r.pts.len() >= 2`
  なら`trigger_traffic_coloring(&r.pts, &mode, &cfg.google_maps_api_key)`を起動
- ポーリングで色分け結果(空でない)を受け取ったら、`spec.routes`内の該当ルート(単色1本)を、
  受け取った複数本の色付きRouteへ差し替える。route_noteに「(渋滞: 緑/黄/赤)」を一言追記。
  結果が空(失敗・対象外)なら何もしない(単色のまま)
- jobs_active/polling/Ctrl-C中断チェーンに`traffic_color_job`を追加(旧`traffic_delay_job`と同じ場所)

### 2.4 settings.rs

- idx=28のラベルを「渋滞込み所要時間」→「渋滞状況の色分け」に変更
- 説明文を「渋滞状況の色分け: ルート確定後、Google Directionsで区間ごとの渋滞状況を追加確認し、
  ルート線を緑(順調)/黄(やや混雑)/赤(混雑)に色分けする。道路網全体ではなく表示中のルートのみ。
  要Google APIキー。区間数に応じて1回のAdvanced課金対象リクエストを送る(無料枠超過分は
  1000件$8、個人利用なら通常は無料枠内)」に変更

### 2.5 撤去する旧実装

`ROUTE_TRAFFIC_DELAY_THRESHOLD_S` / `should_show_traffic_delay` / `traffic_delay_from_legs` /
`fetch_traffic_delay` / `TrafficDelayRx` / `trigger_traffic_delay`とそれらのテストを削除する
(`parse_directions_legs`と`directions_common_params`は新実装でも使うため残す)。

## 3. テスト(実装前に書く)

- `traffic_segment_count`: 0以下→1 / 5km未満の短いルート→1 / ちょうど5km区切りになるケース /
  上限(TRAFFIC_MAX_WAYPOINTS+1=24)に達する長大ルートでクランプされること
- `traffic_breakpoints_m`: segments=1→空Vec / segments=3で総延長を3等分した2点になること
- `traffic_waypoints`: 各breakpointに対しroadtrace::point_atと同じ座標が返ること
- `traffic_level`: ratio<1.15→Smooth / 1.15〜1.5未満境界→Moderate / 1.5以上→Heavy /
  duration_in_traffic欠損→Smooth / duration<=0→Smooth
- `traffic_level_color`: 3値がそれぞれ異なる色であること
- `colorize_route_by_traffic`: legs件数とbreakpoints+1が一致する通常ケースで区間数分の
  (色,点列)が返ること / 隣接区間の境界点が共有されている(線が途切れない)こと /
  件数不一致なら空Vecを返すこと
- `fetch_traffic_coloring`はネットワーク関数のため単体テスト対象外(既存の
  `fetch_traffic_delay`同様、実データでのlive `#[ignore]`テストで確認する)

## 4. 対象外(今回もやらない)

- 一般道路網全体の広域渋滞オーバーレイ(Google側に相当するAPIが無いため不可能、既存調査済み)。
  ルートを引いていない状態で周辺の主要道路の渋滞状況を見たいという要望は別途あり、その場合は
  固定の「監視区間」リストに対して今回と同じ区間色分けの仕組みを使う案がある。次回検討する
  パラメータ(会話でユーザーから指定済み):
  - 監視区間の本数はズーム(拡大)状況に応じて絞る
  - 表示範囲があまりに広域な場合はそもそもGoogle Directionsへ問い合わせない
  - 更新間隔30分
  - 取得結果はTTL30分のメモリ上キャッシュ(ディスクへは保存しない。理由は本文冒頭の
    フォールバックキャッシュ事故と同じ=時々刻々変わるデータを長期キャッシュしない)
- 表示後の周期的な再取得(渋滞状況の経時変化への追従)。今回はルート確定時の1回取得のみ
- BRouterが失敗しGoogleへフォールバックした場合の色分け(フォールバック結果はそもそも
  ディスクキャッシュしない方針に変更済み[[feedback-fallback-result-cache-poisoning]]で、
  かつ`fetch_google_route`はdeparture_timeを付けていないため対象外。将来やるなら別途検討)
