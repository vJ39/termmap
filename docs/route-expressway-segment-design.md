確定したルートのうち、高速道路を通る区間がどこかをルート線の色分けで示す機能の設計。
追加の外部問い合わせは不要で、既にBRouterのレスポンスに入っていて解析済みの情報だけを使う。

背景: 既定ルートを`car-fast`にしていても下道が返ることがある。これはBRouterの仕様で、
`car-fast`は高速道路の使用を禁止しないだけであり、必ず使うわけではない(都市高速は出入口が
少なく、近距離では下道の方が速いと判定されることがある)。ルートを見て「高速を使ったのか、
使ったならどこからどこまでか」が分かるようにする。既存の`hw_m`(`src/route.rs`の
`expressway_meters`が求める高速区間の合計距離。高速料金の概算に使用)は距離の合計だけで、
位置の情報を持っていない。

## 1. BRouterレスポンスの実測(2026/08/17)

`https://brouter.de/brouter?...&format=geojson`の応答を3ルートで実測して確認した結果。
設計はこの実測に基づく。

| ルート | プロファイル | coordinates | messages行数 | motorway合計 |
|---|---|---|---|---|
| 新宿→小田原 | car-fast | 1131点 | 119行 | 54,738m |
| 新宿→(経由地)→西 | car-fast | 1081点 | 216行 | 32,294m |
| 都心の短距離 | moped | 129点 | 89行 | 0m |

確認できた事実:

| # | 事実 | 実測結果 |
|---|---|---|
| 1 | `messages`各行の`Longitude`/`Latitude`は整数マイクロ度の文字列(`139701812` = 139.701812度) | 3ルート424行すべて |
| 2 | 各行の座標は`geometry.coordinates`の頂点と一致する(マイクロ度整数に丸めて等値) | 424行中424行が一致・未一致0 |
| 3 | 行の並びは頂点インデックスの昇順で、最終行は最終頂点を指す | 3ルートとも成立 |
| 4 | 同じ座標が複数の頂点に現れる曖昧な一致 | 0件(ただし折り返す経路では原理的に起こりうる) |
| 5 | `Distance`列の合計は`track-length`と一致する | 77661/77661・49291/49291・3766/3766 |
| 6 | 各行の`Distance`は「直前の行の座標からこの行の座標まで」の距離 | haversineとの差は区間あたり-8.5〜+6.4m、全長では77.7kmに対し7m |
| 7 | `WayTags`に出るキーは`highway`/`surface`/`oneway`/`reversedirection`/`maxspeed`/`access`/`motorcar`のみ | 3ルート分の全行。`name`・`ref`・`toll`は出ない |
| 8 | `highway`の値は`motorway`/`motorway_link`/`trunk`/`trunk_link`/`primary`/`primary_link`/`secondary`/`tertiary`/`unclassified`/`residential`/`service` | 同上 |

結論として、`messages`の各行は「直前の行の座標から、その行の座標まで」の区間を表し、両端は
ルート線(`RouteResult.pts`)の頂点そのものである。したがって座標の一致だけで`pts`のインデックス
範囲へ落とせる。距離を按分して位置を推定する必要はない。

事実7から、路線名(東名・首都高など)はこのレスポンスからは取り出せない。名称の表示は§6で
対象外とする。

高速区間の統合結果も実測した。新宿→小田原は41行が1つの連続区間(インデックス95〜854)に
まとまり、経由地ありのルートは24行が2区間(95〜311・628〜897)にまとまって、間には
`tertiary`/`primary`の一般道が挟まっていた。区間数はそのまま「途中で一度降りる」ことを表す。

## 2. 決定事項

| # | 論点 | 結論 |
|---|---|---|
| 1 | 高速の判定 | `WayTags`が`highway=motorway`を含む行。部分一致のため`highway=motorway_link`も該当する。既存の`expressway_meters`と同じ判定を使い回すので、表示する距離と色を塗る区間が必ず一致する。ランプ・JCT連絡路を含めることでIC入口からIC出口まで線が途切れない |
| 2 | 位置の特定 | 整数マイクロ度での座標一致(許容±2マイクロ度≒0.2m)。`pts`を前方向にだけ進むカーソルで走査し、折り返す経路で同じ座標が2回出ても手前を誤って選ばないようにする |
| 3 | 保持形式 | `RouteResult`に`hw_segments: Vec<(usize, usize)>`(`pts`のインデックス範囲、両端を含む)を追加。隣り合う範囲は統合してから保持する |
| 4 | 位置特定に失敗した場合 | 1行でも対応する頂点が見つからなければ区間を空にし、距離(`hw_m`)だけ返す。色分けは出ないが、距離と料金概算は従来通り出る |
| 5 | 表示 | 専用レイヤ`spec.expressway_segments`(緑`[0, 230, 100]`・太さ2)。ルート本体(シアン)の直後、`roads`・`traffic_segments`より先に描く。高速区間が渋滞していれば渋滞の色が上に乗る |
| 6 | 設定 | 追加しない(常時ON)。追加の外部問い合わせが無いため、「ONにした人だけが外部サービスへ問い合わせる」という既定OFFの方針の対象外。下道モードでは該当する行が出ないので表示は変わらない |
| 7 | ステータス行 | 既存の`route_summary`の「高速XX.Xkm」に、区間が2つ以上のときだけ「(N区間)」を足す。1区間のときは足さない |
| 8 | Googleフォールバック時 | `hw_m`が0になるのと同じ理由で`hw_segments`も空。色分けは出ない |
| 9 | ディスクキャッシュ | `route_cache_path`のキーにスキーマ版を足して、旧形式の保存済みルートを読まないようにする(§3.3) |

## 3. 実装

### 3.1 route.rs: 純関数

```rust
// 高速区間の色。日本の道路案内標識に合わせて緑(高速)、ルート本体はシアンのまま。
pub const EXPRESSWAY_COLOR: [u8; 3] = [0, 230, 100];

// WayTags 1行分が高速道路か。"highway=motorway" の部分一致は "highway=motorway_link" にも
// 当たる。ランプ・JCT連絡路を高速に含めるのは意図した挙動で、hw_m(料金概算)の集計と
// 判定を揃えるためにこの関数を両方から呼ぶ。
fn is_expressway_tags(waytags: &str) -> bool

// BRouterの応答本文と、そこから作った pts から、(高速の合計メートル, ptsのインデックス範囲)
// を求める。ネットワークに触れない純関数。既存の expressway_meters はこれに置き換える。
pub fn expressway_segments(body: &str, pts: &[(f64, f64)]) -> (f64, Vec<(usize, usize)>)

// インデックス範囲を描画用の点列へ。pts の範囲外・2点未満になる範囲は捨てる。
pub fn expressway_polylines(pts: &[(f64, f64)], segs: &[(usize, usize)]) -> Vec<Vec<(f64, f64)>>
```

`expressway_segments`の手順:

1. `messages[0]`(ヘッダ行)から`Longitude`/`Latitude`/`Distance`/`WayTags`の列位置を引く。
   `Distance`か`WayTags`が無ければ`(0.0, vec![])`。`Longitude`/`Latitude`が無い場合は距離だけ
   集計して範囲は空で返す(距離の集計は既存の`expressway_meters`と同じ動作のまま)。
2. `pts`を整数マイクロ度`(i64, i64)`へ丸めた配列を1度だけ作る。`pts`が空なら距離だけ返す。
3. `cursor = 0`、`prev_idx = 0`、`meters = 0.0`、`raw: Vec<(usize, usize)>`を用意する。
4. 各行について:
   - `Longitude`/`Latitude`を`i64`として読む。読めなければ位置特定を失敗とする。
   - `cursor`から前方へ走査し、緯度・経度の差がどちらも2マイクロ度以内の最初の頂点を
     `end_idx`とする。見つからなければ位置特定を失敗とする。
   - `is_expressway_tags`が真なら`meters += Distance`、`raw.push((prev_idx, end_idx))`。
   - `prev_idx = end_idx`、`cursor = end_idx`(`end_idx + 1`にしない。長さ0の行が来ても
     同じ頂点で受けられるようにするため。長さ0の範囲は最後に捨てる)。
5. 位置特定に一度でも失敗したら、残りの行は距離の集計だけ続け、範囲は空で返す。距離の集計は
   行ごとに独立しているので、位置特定に失敗しても`hw_m`は正しい値になる。
6. `raw`を前から見て、次の範囲の始点が直前の範囲の終点以下なら1つの範囲に繋ぐ。
7. 統合後に始点と終点が同じになった範囲は線として描けないので捨てる。距離は`Distance`列を
   足したものなので、この切り捨ての影響を受けない。

走査は前方向にしか進まないので、全体の計算量は`pts`の点数と行数の和に比例する。位置特定に
失敗する場合でも`pts`を最後まで走るのは1回だけ。

### 3.2 route.rs: RouteResult と呼び出し

```rust
pub struct RouteResult {
    pub pts: Vec<(f64, f64)>, pub ele: Vec<f64>, pub dist_m: f64, pub time_s: f64,
    pub hw_m: f64,
    #[serde(default)] pub hw_segments: Vec<(usize, usize)>,
    pub ascend_m: f64,
    #[serde(default)] pub via_google: bool,
}
```

`fetch_route_once`では`pts`を組んだ後に1回だけ呼ぶ。

```rust
let (hw_m, hw_segments) = expressway_segments(&body, &pts);
```

`fetch_google_route`は`hw_m: 0.0`と同様に`hw_segments: Vec::new()`を返す(Google Directionsの
応答から高速区間を判定する手段が無いため)。

`#[serde(default)]`は`via_google`と同じ理由で付ける。旧形式のキャッシュJSONを読んでも
パースが失敗しないようにするためで、内容の正しさは§3.3のスキーマ版で担保する。

### 3.3 ディスクキャッシュの扱い

ルートキャッシュ(`~/.config/termmap/route-cache/`)は期限を持たない。`hw_segments`を足しただけ
だと、既に保存されている高速を含むルートは`hw_m > 0`なのに`hw_segments`が空のまま読み込まれ、
「距離は出るのに色が出ない」状態が消えずに残る。

`route_cache_path`のキー文字列の先頭にスキーマ版を足して、旧形式を読まないようにする。

```rust
// RouteResult の中身の作り方を変えたらこの値を上げる(古い保存分を読まないようにするため)。
const ROUTE_CACHE_SCHEMA: &str = "v2";
let mut key = format!("{}|{}|{}|{}", ROUTE_CACHE_SCHEMA, route_profile(mode), alt, nogos);
```

検討した別案: 読み出し時に`hw_m > 0.0 && hw_segments.is_empty()`ならキャッシュミス扱いにして
取り直す方式。取り直しの対象が高速を含むルートだけで済む一方、BRouterの出力形式が変わって
位置特定が恒久的に失敗するようになると、そのルートは毎回キャッシュミスになり通信が増え続ける。
キャッシュの目的が通信削減であることに反するので採らない。

スキーマ版を上げると旧ファイルは参照されないまま残る。1件あたり数十KBで、`route-cache`
ディレクトリごと消せば片付く範囲なので、自動削除は入れない。

### 3.4 route.rs: 要約表示

`route_summary`の`hw_m > 50.0`の分岐に、区間数が2つ以上のときだけ区間数を足す。

```
高速54.7km ¥1642概算          区間が1つのとき(従来と同じ)
高速32.3km(2区間) ¥969概算    区間が2つ以上のとき
```

区間が1つのときに出さないのは、「途中で一度高速を降りる」ことが分かる場合にだけ意味がある
情報だからで、常に出すと通常のルートで冗長になる。

### 3.5 render.rs

`OverlaySpec`に`expressway_segments: Vec<Route>`を追加する。`build_overlay`の描画順は
リング → ルート本体 → **高速区間** → 道路の塊 → 渋滞の色分け → POI → スポット。

```rust
for ex in &spec.expressway_segments { // 高速区間(ルート本体の上・渋滞の色分けの下)
    let pts: Vec<(i32, i32)> = ex.pts.iter().map(|&(la, lo)| to_img(la, lo)).collect();
    draw_polyline(&mut ov, &pts, ex.color, ex.thickness);
}
```

`spec.routes`に足さず独立したフィールドにするのは、`spec.routes.last()`を「ルート全体」として
参照している箇所(GPX保存・標高表示・次の曲がり案内・`ui_overlay.rs`・`ui_helpers.rs`)を
壊さないため。`traffic_segments`を別レイヤにしたのと同じ理由。

`OverlaySpec::is_empty()`の判定と、構築している箇所(`src/main.rs`の`build_spec`、`render.rs`の
テスト3箇所)にも新フィールドを足す。

### 3.6 ui.rs の配線

このファイルは他の作業と衝突しやすいので、実装時に現状を読み直してから当てる。

- ルート取得成功時(`route_job`の結果処理)、`spec.routes.push(Route { pts: r.pts, ... })`は
  `r.pts`をムーブするため、**その前に**高速区間の点列を作る。

```rust
spec.expressway_segments = route::expressway_polylines(&r.pts, &r.hw_segments)
    .into_iter()
    .map(|pts| Route { pts, color: route::EXPRESSWAY_COLOR, thickness: 2 })
    .collect();
```

- `spec.routes.clear()`・`spec.traffic_segments.clear()`と同じ場所、およびルートを全消しする
  箇所で`spec.expressway_segments.clear()`も呼ぶ。古いルートの高速区間を引き継がない。
- 再描画の判定に使っているハッシュ(`map_sig`、`spec.routes`/`roads`/`traffic_segments`を
  連結して回している箇所)のチェーンに`spec.expressway_segments`を足す。
- 追加のジョブ・ポーリングは不要。高速区間はルート結果と同時に確定するので、渋滞の色分けの
  ような非同期の受け取り口は要らない。

### 3.7 main.rs

`attach_route`(`--route`のCLI一発描画)でも同じように`spec.expressway_segments`を組み立てる。
CLIから地図を1枚出すだけの使い方でも高速区間が色分けされる。

## 4. 色の選び方

| レイヤ | 色 | 高速の緑とぶつからない理由 |
|---|---|---|
| ルート本体 | シアン`[0, 220, 255]` | 高速区間だけ緑に変わるので、同じ線の中で対比が付く |
| 渋滞の色分け | 黄`[230, 200, 0]` / 赤`[220, 40, 40]` | 高速の後に描くので、高速区間が混んでいれば渋滞の色が上に出る。今の混み具合の方が優先度が高い |
| 通行規制 | 黒 / 橙 / 黄 / 水色 / 紫 | 別レイヤで色域が重ならない |
| 道路交通量のライン | 緑`[80, 200, 90]` / 黄 / 赤 | 緑同士だが、同じ線の上に両方が乗ることはない。JARTICの観測点は直轄国道のみで高速道路には存在しない(`docs/road-traffic-spec.md` §2・`src/traffic.rs`のコメント) |
| POIマーカー | 赤 / 橙 / 金 / 水色 / 桃 / 黄緑 / 白 | 線ではなくマーカーで、形状も違う |

緑にするのは、日本の道路案内標識が高速道路=緑・一般道=青で、色の意味を説明しなくても
伝わるため。値は道路交通量の緑`[80, 200, 90]`より彩度を上げて、並んで見えても見分けが付く
ようにする。

太さはルート本体と同じ2にする。高速区間を太さ3にして渋滞の色の下から緑を覗かせる案も
考えたが、端末の描画は1セルあたり2px程度で細い縁が潰れて見分けが付かなくなるため採らない。

## 5. テスト

`expressway_segments`・`expressway_polylines`はネットワークに触れない純関数なので通常の
ユニットテストで確認する。テスト用の`messages`は実測値の座標(`139701812`/`35689780`等)を
使い、`pts`側も同じ座標で組む(既存の`GPX_TURNS_SAMPLE`と同じやり方)。

- `expressway_segments`
  - 高速1区間: 該当行のインデックス範囲が1件返る
  - 連続する複数行が1つの範囲に統合される
  - 一般道を挟んだ高速2区間が2件の範囲になる
  - `highway=motorway_link`も高速として数える(距離・範囲とも)
  - 座標が`pts`に一致しない場合、`hw_m`は従来通り返り範囲は空になる
  - ヘッダに`Distance`/`WayTags`が無い場合は`(0.0, 空)`
  - `messages`が空・本文が壊れている場合は`(0.0, 空)`
  - 始点と終点が同じになる範囲は捨てられる
  - 同じ座標が2回出る折り返し経路で、2回目の行が手前の頂点に一致しない(前方向のカーソルが効く)
- `expressway_polylines`
  - `pts`の範囲外のインデックスを含む範囲は除外される
  - 2点未満になる範囲は除外される
  - 正常な範囲は`pts[a..=b]`と同じ点列になる
- `route_summary`
  - 区間1つでは区間数を出さない(既存の期待値のまま)
  - 区間2つ以上で「(2区間)」が付く
  - `hw_m <= 50.0`では従来通り高速の表示自体が出ない
- `RouteResult`のserde: `hw_segments`が無い旧JSONを読める
- `route_cache_path`: スキーマ版がキーに含まれ、版が違えば別のパスになる
- 実ネットワークを叩く`#[ignore]`テスト: 新宿→小田原(car-fast)で`hw_segments`が1件返り、
  その範囲の点列の長さの合計が`hw_m`と概ね一致すること(誤差5%程度で判定。`pts`をhaversineで
  測った値とBRouterの`Distance`の差は実測で77.7kmに対し7m)

`src/ui.rs`の配線と`src/render.rs`の描画順には専用のユニットテストを置かない(`interactive()`
本体はテストしにくい既存構造で、`traffic_segments`・規制・カメラの描画分岐も同様)。
代わりに、実装後にPTYで実機確認する。設定行の追加は無いので、設定トグルの配線漏れは今回は
発生しない。

## 6. 対象外(今回はやらない)

- **路線名の表示**(東名・首都高など): BRouterの`WayTags`に`name`・`ref`が出ない(§1の事実7)。
  出すにはOverpass等への追加問い合わせが必要になるため、今回は入れない
- **motorway以外の有料道路の判定**: `WayTags`に`toll`が出ないため判定手段が無い。一般有料道路や
  有料の橋・トンネルは高速として色が付かず、料金概算にも入らない(既存の`hw_m`と同じ制約)
- **高速の入口/出口へのマーカー表示**: 線の色が変わるので端の位置は分かる。マーカーを足すと
  既存のwaypoint・POIのマーカーと混ざるため、必要になってから検討する
- **料金の精緻化**: 既存の「高速区間km × ¥30(普通車・割引なし)」の概算のまま。区間別料金・
  ETC割引・首都高の距離別料金は扱わない
- **高速道路上の渋滞・規制の専用表示**: 既存の渋滞の色分けと通行規制の範囲。高速道路の
  リアルタイム渋滞データについては`docs/nexco-shutoko-data-investigation.md`の通り、
  無料・登録不要で使えるものが見つかっていない

## 7. 影響範囲

| ファイル | 変更内容 |
|---|---|
| `src/route.rs` | `is_expressway_tags`/`expressway_segments`/`expressway_polylines`/`EXPRESSWAY_COLOR`の追加、`expressway_meters`の置き換え、`RouteResult.hw_segments`、`fetch_route_once`・`fetch_google_route`の戻り値、`route_cache_path`のスキーマ版、`route_summary`の区間数 |
| `src/render.rs` | `OverlaySpec.expressway_segments`の追加、`build_overlay`の描画順、`is_empty()`、テスト内の構築3箇所 |
| `src/ui.rs` | ルート確定時の組み立て、クリア処理、再描画判定のハッシュ(他の作業と衝突しやすいため実装時に現状を確認する) |
| `src/main.rs` | `build_spec`の構築、`attach_route`の組み立て |
| `docs/MANUAL.md` | 高速区間の色の説明を「渋滞状況の色分け」の近くに追記 |
| `README.md` | 機能一覧に1行 |
