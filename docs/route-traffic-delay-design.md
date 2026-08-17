ルート確定後にGoogle Directions(`departure_time=now`)で渋滞込み所要時間を追加確認し、
現在の遅延を表示する機能の設計。前提調査は会話内で完了(BRouterには渋滞データが無く、
Google Directions Advancedの`duration_in_traffic`のみが個人開発で現実的に使える手段)。

## 1. 決定事項

| # | 論点 | 結論 |
|---|---|---|
| 1 | 対象 | BRouterで確定したルートに対し、**別途** Google Directionsへ同じ経由地で1回問い合わせ、ジオメトリは捨てて所要時間の差分だけ使う |
| 2 | 設定 | 新規トグル`route_traffic_enabled`(既定false)。既存の`traffic_enabled`(JARTIC交通量の地図重ね)とは別物(名前が紛らわしいので設定説明文で明記する) |
| 3 | 発火 | ルート成功時(route_jobの結果処理)、設定ON かつ Google APIキー設定済み かつ wps.len()>=2 の時だけ、非同期で1回問い合わせる |
| 4 | 表示 | route_note(ルート要約行)に「(現在+8分の遅れ)」のように追記。遅延が実質無い(閾値未満)場合は何も追記しない |
| 5 | 失敗時 | 静かに諦める(遅延情報が無いだけで、ルート自体の表示は既存のBRouter結果に依存しており壊さない)。turn_pointsの失敗時方針と同じ |

## 2. 実装対象

### 2.1 route.rs: 純関数(まずテストから書く)

```rust
// Google Directions legs[].duration/duration_in_traffic(秒)の配列から、
// 渋滞による遅延(duration_in_traffic合計 - duration合計、秒)を返す。
// duration_in_trafficを持たないlegはdurationで代用する(合計の整合を崩さないため)。
pub fn traffic_delay_from_legs(legs: &[(f64, Option<f64>)]) -> f64
// legs: Vec<(duration_s, duration_in_traffic_s)>

// Directions APIの生レスポンス(JSON文字列)からlegsの(duration, duration_in_traffic)を抜き出す。
// ネットワークに触れない純関数。
pub fn parse_directions_legs(body: &str) -> Result<Vec<(f64, Option<f64>)>, String>
```

- `traffic_delay_from_legs`が0未満(まれに交通状況が良くdurationより短く出るケース)は0にクランプする
  (「遅延」という表示の意味上、マイナス表示は誤解を招くため)
- 遅延が60秒未満なら「実質無い」として呼び出し側は表示しない(閾値は`ROUTE_TRAFFIC_DELAY_THRESHOLD_S = 60.0`)

### 2.2 route.rs: ネットワーク関数

```rust
// wps・modeはfetch_google_route/fetch_routeと同じ意味。keyが空なら即Err。
// ジオメトリは使わず、遅延秒数(0以上)だけを返す。
pub fn fetch_traffic_delay(wps: &[(f64, f64)], mode: &str, key: &str) -> Result<f64, String>

pub type TrafficDelayRx = std::sync::mpsc::Receiver<Result<f64, String>>;
pub fn trigger_traffic_delay(wps: &[(f64, f64)], mode: &str, key: &str) -> TrafficDelayRx
```

`fetch_traffic_delay`は`fetch_google_route`とURL構築ロジックを共有する(origin/destination/waypoints/
avoid=highways)が、`departure_time=now`を追加する点と、ジオメトリを見ずlegsの時間だけを見る点が違う。
共有できる部分(origin/destination/waypoints文字列の組み立て)は小さな private helper へ切り出す。

### 2.3 ui.rs

- 新規state: `traffic_delay_job: Option<route::TrafficDelayRx>`
- route_job成功時(既存の`Ok(Ok(r)) => {...}`ブロック、通行止め回避の注記を足した直後)に、
  `cfg.route_traffic_enabled && !cfg.google_maps_api_key.trim().is_empty() && wps.len() >= 2`
  なら`trigger_traffic_delay(&wps, &mode, &cfg.google_maps_api_key)`を起動
- ポーリング(disaster_job等と同じ形)で結果を受け取ったら、`route_note`に追記する
  (遅延が閾値未満・取得失敗なら何もしない)
- jobs_active/polling/Ctrl-C中断チェーンに`traffic_delay_job`を追加(既存の通行止め回避実装と
  同じ場所を触る)

### 2.4 settings.rs

- 末尾(idx=28)に追加。ラベル「渋滞込み所要時間」
- 説明文: 「渋滞込み所要時間: ルート確定後、Google Directionsで現在の渋滞による遅延を追加確認する。
  要Google APIキー。ONにした人だけがAdvanced課金対象のリクエストを追加で送る(無料枠超過分は
  1000件$8、個人利用なら通常は無料枠内)」

## 3. テスト(実装前に書く)

- `traffic_delay_from_legs`: legs無し→0 / duration_in_trafficが全legにある通常ケース /
  一部のlegにduration_in_trafficが無い(徒歩区間混在等の想定)→そのlegはdurationで代用 /
  duration_in_traffic < durationのケース(渋滞が無く早く着く)→0にクランプ
- `parse_directions_legs`: 実際のDirections APIレスポンス形の抜粋で正しく抜き出せること /
  status!=OK・routes無し・legs無しは全てErr(fetch_google_routeの既存エラー処理と同じ方針)
- 表示閾値(60秒未満は追記しない)はui.rs側のロジックなので、ui_status.rs等にテストを書くか、
  route.rs側に`should_show_traffic_delay(secs: f64) -> bool`という小さな純関数を切り出してテストする

## 4. 対象外(今回やらない)

- 一般道路の広域渋滞オーバーレイ(Google側に相当するAPIが無いため不可能、既存調査済み)
- BRouterが失敗しGoogleへフォールバックした場合の遅延確認(そのルート自体が既にGoogle製で
  duration/duration_in_trafficを同時に取れる可能性があるが、現状のfetch_google_routeは
  departure_timeを付けていないため対象外。将来やるなら別途検討)
