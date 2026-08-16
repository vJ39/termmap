過去災害(豪雨・地震・台風等)の発生履歴を、既存の4レイヤ(道路交通量・主要道路・道路ライブカメラ・
通行規制)と同じ `PlotLayer<T>` の枠組みに乗せて地図へ重ねる機能(#75)の設計。

データソース調査は `docs/disaster-history-data-investigation.md`、プロットデータ共通の
取得・キャッシュ機構は `docs/plot-data-disk-cache-design.md` にある。この文書は
「災害履歴という1レイヤをその枠組みへどう乗せるか」だけを書く。

状態: 実装済み(§9 の 1〜5。年代・種別の設定(Stage2/§8 後半)だけ未着手で、しきい値は 1926 固定)。

実装時に設計から変えた点は3つ。いずれも実データを見て決めた。
- `DamageValue::label()` に単位の引数を足した(`label("名")` / `label("棟")`)。同じ enum を
  死者数(名)と全壊棟数(棟)の両方に使うため、単位を型の中に固定できない。
- `panel_content()` の見出しは、しきい値ではなく**その地点の実際の年幅**を出す
  (「記録 89件(1926〜2019年)」)。集計行の `YMIN`/`YMAX` を持っているのに使わない理由が無い。
  年幅が分からないときだけ「1926年以降」に落ちる。
- 事例名称は `SAIGAI_MEISYO` に "令和元年台風第15号|台風15号" のように別名を `|` で
  連ねた行が実在したので、先頭の1つだけを採る。
- 詳細パネルは Esc/q だけでなく**任意のキー**で閉じる(既存の QR・名前ポップアップと同じ)。
  Esc/q 以外を素通りさせると、パネルに覆われた地図側のキー(`v` で地点追加等)が
  見えないまま発火してしまうため。

---

## 1. 何を見せるか

「今この地点は安全か」を示すものではなく、**この土地で過去にどんな災害が起きてきたか**を、
ツーリングの計画中に地図の上でそのまま読めるようにする。峠・河川沿い・海沿いを走る前に
「ここは斜面災害が多い」「この市は水害が繰り返し記録されている」が分かる状態を目標にする。

そのため表示の主語は「個々の事例」ではなく**地点(市区町村)ごとの積み重ね**にする。理由は §2 の実測にある。

---

## 2. 実測で分かったデータの形(2026/08/16 実API)

設計を左右する事実を先に置く。いずれもこの設計を書く時点で実際にAPIを叩いて確認した。

### 2.1 座標は市区町村の代表点で、1点に何十件も重なる

1次メッシュ5339(東京〜千葉西部、約80km四方)の1926年以降3,637件を取ったところ、
**座標の種類は118個しかなかった**。1地点あたりの事例数は中央値18件・最大166件。

| 上位の座標 | その座標の事例数(2,000件サンプル中) |
|---|---|
| 139.637954, 35.444035 | 114 |
| 139.982621, 35.694711 | 107 |
| 139.874828, 35.955106 | 75 |

つまり **点をそのまま打つと、同じ1画素に何十回も同じ丸を描くだけ**になる。件数も種別も
画面から読めない。「マーカーが多すぎて見づらい」以前に、点の数と災害の数が一致しない。

したがって表示単位は「事例1件=1マーカー」ではなく「**座標1つ=1マーカー、そこに件数と種別を持たせる**」にする。

### 2.2 1リクエストの上限は2,000件で、生レコードでは足りない

レイヤの `maxRecordCount` は 2000。1次メッシュ5339は全期間5,192件・1926年以降3,637件なので、
生レコードを取ると必ずページングが要る。ページングは `supportsPagination: true` で可能だが、
最小フィールド(年と種別だけ)でも1ページ約318KB、2ページで約620KBになる。

### 2.3 サーバー側集計を使うと1リクエスト・数十KBで済む

`groupByFieldsForStatistics=fX,fY,SAIGAI_SYUBETSU_1` + `outStatistics`(件数・最古年・最新年)で、
**座標×災害種別ごとの集計行**が返る。実測値は次のとおり。

| 1次メッシュ | 集計行数 | 座標数 | 応答サイズ | 打ち切り |
|---|---|---|---|---|
| 5339(東京・千葉西部) | 236 | 118 | 25.0KB | 無し |
| 5235(京阪神) | 117 | 54 | 12.6KB | 無し |
| 5030(北九州) | 81 | 44 | 8.9KB | 無し |
| 6441(北海道日高) | 56 | 18 | 6.2KB | 無し |
| 5537(新潟) | 49 | 14 | 5.6KB | 無し |

応答時間は5339で1.1〜1.2秒(3回計測)。最も多い5339でも236行なので2,000行の上限に対して十分な余裕がある。

グループ化キーに `SAIGAI_YEAR` を足すと5339だけで2,000行に達して打ち切られる(実測)。
**年はグループ化に入れられない**——これが §5.1 のキー設計を決める。

### 2.4 災害種別のコード対応表(レイヤ定義の codedValue ドメインから採取)

推測ではなくサービス定義そのものの値。

| `SAIGAI_SYUBETSU_1` | 意味 |
|---|---|
| `1` | 地震災害 |
| `2` | 火山災害 |
| `3` | 風水害 |
| `4` | 斜面災害 |
| `5` | 雪氷災害 |
| `9` | その他気象災害 |

`SAIGAI_SYUBETSU_MORE_1`〜`_10` には36種の詳細コード(`10`地震 / `11`津波 / `32`大雨 / `34`台風 /
`41`土石流 / `51`雪崩 / `93`落雷 等)がある。マーカーの色分けには粗い方(6分類)を使い、
詳細コードは §7 の詳細表示でのみ扱う。

5339の1926年以降サンプル2,000件の内訳は 風水害1,759 / 地震165 / 雪氷46 / その他気象21 / 斜面5 / 火山4 で、
**風水害が9割近い**。種別ごとの色分けは、見た目の大半が1色になることを前提に置く。

### 2.5 被害統計フィールドは実数と符号付きコードとnullが混ざる

`SHIBOU_SU`(死亡)等の被害数フィールドは、ドメイン定義上つぎの値を取る。

| 値 | 意味 |
|---|---|
| 正の整数 | 実数 |
| `0` | 被害なし |
| `-1` | 不明 |
| `-2` | 被害あり(数は不明) |
| `-7` | 大半破損、大規模被害 |
| `-8` | 全戸被害、壊滅被害 |
| `null` | 記載なし(最頻値。2,000件サンプルで1,939件) |

`SAIGAI_MONTH` も負値がコードで、`-30`春の初め頃 / `-70`夏の中頃 / `-110`秋の終り頃 のように季節を表す。
`SAIGAI_DAY` は `100`上旬 / `200`中旬 / `300`下旬。**日付も素直な整数ではない**。

`ACCURACY`(範囲精度)は `A`〜`E` で、`A`=事例の範囲と統計値の集計エリアが一致、
`E`=郡・県レベルの広域。サンプルでは A:1,753 / E:147 / B:49 / C:40 / D:11。

---

## 3. 取得方式の選択

| 方式 | 1メッシュあたりの通信 | 表示に必要な情報 | 判定 |
|---|---|---|---|
| A. 生レコード全件(年フィルタ無し) | 5,192件・3ページ・約1MB | 全部揃う | 却下(重い) |
| B. 生レコード・最小フィールド・1926年以降 | 3,637件・2ページ・約620KB | 地点集約は自前 | 却下(重い。1画面で最大9メッシュ) |
| C. サーバー側集計(座標×種別) | 236行・1リクエスト・25KB | 件数・年幅は揃う。事例名は無い | **採用** |
| D. 集計に年も含める | 2,000行で打ち切り | 打ち切られる | 却下(§2.3) |

Cを採り、**事例そのもの(名称・日付・被害統計)はユーザーが求めたときだけ取りに行く**2段構成にする(§7)。
これは道路ライブカメラで写真URLをキャッシュせず N キーを押したときだけ取り直すのと同じ考え方で、
`docs/plot-data-disk-cache-design.md` §9.2 の判断をそのまま踏襲する。

集計クエリは `f=geojson` を受け付けない(実測で `Requested format is not supported.`)ので `f=json` を使う。
座標は幾何ではなく `fX`/`fY` フィールドから取る。こちらは元データの生値で、
GeoJSON経由の座標に乗る再投影の丸め(実測: `35.955106` が `35.955105999102976` になる)を含まない。

---

## 4. データ層 `src/disaster.rs`(新規)

`traffic.rs` / `regulation.rs` / `camera.rs` と同じ方針で **std + ureq + serde_json のみ**に依存し、
`crate::` を参照しない。ネットワークに触れないパース関数を分けて単体テストできるようにする。

### 4.1 型

```rust
/// 災害種別(SAIGAI_SYUBETSU_1 の6分類)。
pub enum DisasterKind { Earthquake, Volcano, Storm, Slope, Snow, OtherWeather, Unknown }

/// 集計クエリ1行に対応する、ある地点のある種別の積み上げ。
pub struct KindCount { pub kind: DisasterKind, pub count: u32, pub year_min: i32, pub year_max: i32 }

/// 地図に打つ単位。座標1つ=マーカー1つ(§2.1)。
pub struct DisasterSite { pub lat: f64, pub lon: f64, pub kinds: Vec<KindCount> }

/// 被害統計フィールド1つぶんの値(§2.5)。実数と符号付きコードと未記入を型で分ける。
pub enum DamageValue { NotRecorded, NoDamage, Count(u32), Unknown, Reported, Major, Catastrophic }

/// 詳細表示(§7)で扱う事例1件。
pub struct DisasterEvent {
    pub jirei: String, pub name: String,
    pub year: i32, pub month: i32, pub day: i32,
    pub kind: DisasterKind, pub pref: String, pub city: String,
    pub deaths: DamageValue, pub missing: DamageValue,
    pub houses_lost: DamageValue, pub flooded: DamageValue,
    pub accuracy: String,
}
```

`DisasterSite` / `KindCount` / `DisasterKind` に serde の derive を付ける(ディスクキャッシュへ保存する型)。
`DisasterEvent` は保存しないので derive は不要。

### 4.2 関数

```rust
// 1段目: bbox内の地点集計(ディスクキャッシュに載る)
pub fn fetch_sites(lat_min: f64, lon_min: f64, lat_max: f64, lon_max: f64, since_year: i32)
    -> Result<Vec<DisasterSite>, String>;
pub fn parse_sites(body: &str) -> Vec<DisasterSite>;    // ネットワークに触れない純関数

// 2段目: 1地点の事例一覧(押したときだけ・保存しない)
pub fn fetch_events(lat: f64, lon: f64, since_year: i32, limit: u32)
    -> Result<Vec<DisasterEvent>, String>;
pub fn parse_events(body: &str) -> Vec<DisasterEvent>;

// 表示補助
impl DisasterKind { pub fn from_code(code: &str) -> Self; pub fn label(&self) -> &'static str;
                    pub fn color(&self) -> [u8; 3]; }
impl DamageValue { pub fn from_raw(v: Option<i64>) -> Self; pub fn label(&self) -> String; }
pub fn format_date(year: i32, month: i32, day: i32) -> String;
impl DisasterSite { pub fn total(&self) -> u32; pub fn dominant(&self) -> DisasterKind; }
```

### 4.3 1段目のリクエスト

```
GET https://agis.bosai.go.jp/webgis/rest/services/dil-db/saigai/MapServer/0/query
  where=SAIGAI_YEAR>={since}
  geometry={lon_min},{lat_min},{lon_max},{lat_max}&geometryType=esriGeometryEnvelope&inSR=4326
  groupByFieldsForStatistics=fX,fY,SAIGAI_SYUBETSU_1
  outStatistics=[{"statisticType":"count","onStatisticField":"OBJECTID","outStatisticFieldName":"N"},
                 {"statisticType":"min","onStatisticField":"SAIGAI_YEAR","outStatisticFieldName":"YMIN"},
                 {"statisticType":"max","onStatisticField":"SAIGAI_YEAR","outStatisticFieldName":"YMAX"}]
  returnGeometry=false&f=json
```

- `since_year` が 0 以下なら `where=1=1`(全期間)にする。
- User-Agent は既存4モジュールと同じ `termmap/0.1 (personal experiment)`、タイムアウト20秒。
  URLエンコードは `traffic.rs` の `urlencode` と同じ自前実装を置く(依存を増やさない)。
- パースは `features[].attributes` の `fX`/`fY`/`SAIGAI_SYUBETSU_1`/`N`/`YMIN`/`YMAX` を読み、
  座標が一致する行を1つの `DisasterSite` へ畳む。座標欠損・数値でない行は黙って捨てる
  (`parse_traffic` / `parse_closures` と同じで、壊れた入力でも panic せず空 `Vec` を返す)。
- 応答が `{"error":{...}}` のときは `Err` にする。**空 `Vec` と失敗を混同しない**
  (混同するとオフラインでマーカーが消える。`docs/plot-data-disk-cache-design.md` §1.2(c) と同じ問題)。

### 4.4 2段目のリクエスト

```
where=SAIGAI_YEAR>={since}
geometry={lon-δ},{lat-δ},{lon+δ},{lat+δ}&geometryType=esriGeometryEnvelope&inSR=4326
outFields=JIREI_BANGO,SAIGAI_MEISYO,SAIGAI_MEISYO_JMA,SAIGAI_YEAR,SAIGAI_MONTH,SAIGAI_DAY,
          SAIGAI_SYUBETSU_1,BASHO_KEN,BASHO_SHI,ACCURACY,SHIBOU_SU,YUKUEHUMEI_SU,ZENKAI,YUKAUESHINSUI
returnGeometry=false&orderByFields=SAIGAI_YEAR DESC&resultRecordCount={limit}&f=json
```

- 地点の指定は `fX=... AND fY=...` の浮動小数一致ではなく、**±0.0005度(約50m)の小さな矩形**にする。
  一致方式でも実際に引けた(実測)が、`fX` の小数桁は6桁が大半で12桁のものも混じっており、
  文字列に戻すときの桁で外れる余地がある。矩形なら桁に依存しない。
  実測では 139.874828,35.955106 の±0.0005度に89件が入り、同じ市区町村の事例だけが返った。
- `limit` は20を既定にする。1地点最大166件(§2.1)を全部出しても読めないので、新しい順に切る。
- 名称は `SAIGAI_MEISYO_JMA`(気象庁命名)を優先し、無ければ `SAIGAI_MEISYO`、それも無ければ
  `format_date` の日付と種別名だけを出す。サンプルでは両方 null の行が実在する。

### 4.5 コード値混在への対処(§2.5)

```rust
DamageValue::from_raw(None)      => NotRecorded    // 表示: "記載なし"
DamageValue::from_raw(Some(0))   => NoDamage       // 表示: "なし"
DamageValue::from_raw(Some(-1))  => Unknown        // 表示: "不明"
DamageValue::from_raw(Some(-2))  => Reported       // 表示: "あり(数不明)"
DamageValue::from_raw(Some(-7))  => Major          // 表示: "大規模被害"
DamageValue::from_raw(Some(-8))  => Catastrophic   // 表示: "壊滅的被害"
DamageValue::from_raw(Some(n>0)) => Count(n)       // 表示: "3名"
DamageValue::from_raw(Some(その他の負値)) => Unknown  // 未知のコードは不明へ寄せる
```

**コード値を数値として合算しない**。`-1`(不明)を0として足すと被害を過小に、`-8` を足すと符号が
逆向きに効く。`traffic.rs` が欠測方向を0扱いせずその方向ごと加算から外しているのと同じ扱いにする。

日付も同様に `format_date` で文字へ直す。

| 入力 | 出力 |
|---|---|
| 2019, 9, 6 | `2019年9月6日` |
| 2019, 9, 200 | `2019年9月中旬` |
| 1926, -70, null | `1926年夏の中頃` |
| 1926, null, null | `1926年` |

集計側(1段目)は `COUNT(OBJECTID)` なので被害統計のコード値問題に触れない。**マーカーの大きさに
死者数を使わない**理由でもある(§6.2)。

---

## 5. `PlotLayer<T>` への統合

### 5.1 キー単位: 1次メッシュ + 年代しきい値の複合キー

| 項目 | 値 |
|---|---|
| セル | 1次メッシュ(JIS X 0410、約80km四方) |
| キー | `{メッシュコード}_{しきい値年}`(例 `5339_1926`。全期間は `5339_0`) |
| 被覆関数 | `disaster_cells()`(`mesh::primary_codes` にしきい値年を付けて文字列化) |

1次メッシュにする理由は3つ。

1. **1リクエストに収まる**。メッシュ1枚の集計が5.6〜25KB・236行以下で、`maxRecordCount` 2,000 に
   対して余裕がある(§2.3)。2次メッシュに割ると通信回数だけが増えて得が無い。
2. **交通量・通行規制と同じ格子になる**。`primary_codes` を共有するのでセル境界が揃い、
   複数レイヤをONにしたときに「このレイヤだけ端が欠けている」状態が起きない。
3. **データの粒度に合う**。座標が市区町村代表点なので、2次メッシュ(約10km)だと1セルに
   0〜1点しか入らない(実測: 都心の2次メッシュ1枚に1件)。セル分割の意味が無い。

年代しきい値をキーに含めるのは、年をグループ化キーに入れられない(§2.3)ため。
`where` で絞るしかなく、絞り方が変われば中身が変わる。キーに含めておけば
`5339_1926` と `5339_0` が別ファイルになり、設定を切り替えても混ざらない。
`plotcache::valid_key` は英数字と `-` `_` の16文字以内を許すので `5339_1926`(9文字)は通る。

しきい値年は `plotlayer.rs` 内の `static DISASTER_SINCE: AtomicI32` に置く。
`CellsFn` / `FetchFn` が `fn` ポインタ(環境を捕まえられない)なので、被覆側と取得側の両方から
同じ値を読むにはこの形になる。設定で切り替えたときはセル表に古いキーのセルが残り、
`items()` が全セルを舐めるため混ざる——**切り替え時は `ui.rs` 側でレイヤを作り直す**
(`disaster_layer = plotlayer::disaster();` の1行)。Stage1ではしきい値を固定するので発生しない(§9)。

### 5.2 TTL

| 項目 | 値 | 根拠 |
|---|---|---|
| fresh(再取得抑止) | **30日** | 過去の災害そのものは変わらない。変わるのは「新しい災害が起きて登録される」「既存事例の記述が修正される」の2つで、いずれも月単位。主要道路(OSM幾何)・地図タイルと同じ性格なので同じ値(`TILE_TTL` = 30日)を使う。90日も検討したが、災害が起きてから最大3か月見えないのは長い |
| stale上限 | **無し**(GCで消えるまで) | 古い集計が誤りになることがない(新しい事例が出ないだけ)。主要道路・カメラと同じ扱い |
| `data_lag_secs` | 0 | 取得時刻がそのままデータの時刻 |
| 最大エントリ数 | 200 | 1次メッシュなので日本全土を走破しても百数十件(交通量・規制と同じ) |
| 最大バイト数 | 20MB | 1セル実測25KB以下なので200セルでも約5MB。他レイヤと桁を揃えた余裕値 |

fresh 30日・stale無制限なので、**一度見た土地は再訪しても再起動しても通信しない**。
このレイヤの通信は「初めて入った1次メッシュ1枚につき1回」に収束する。

### 5.3 ズーム下限とセル数

| 項目 | 値 |
|---|---|
| ズーム下限 | **z11**(交通量・通行規制と同じ) |
| 1ジョブのセル上限 | `MAX_CELLS_PER_JOB` = 9(共通の値をそのまま使う) |

1次メッシュを使う以上、z11未満では9枚を超えて共通の上限に掛かる。下限を交通量・規制と揃えることで
「3レイヤが同時に消える/同時に出る」挙動になり、説明が1つで済む。

広域で見たいデータではあるが、z11(1辺約113km)で1次メッシュ4〜9枚=最大9リクエストになる。
これ以上広げると1操作で数十リクエストになるため、共通の安全弁に従う。
下限より広域では既に取ってあるセルは表示し続け、何も無いときだけステータスに
`🌊広域では非表示` と出る(既存の `plot_label` がそのまま使える)。

### 5.4 `plotlayer.rs` への追加

```rust
fn disaster_cells(b: Bbox) -> Vec<String> {
    let since = disaster_since();
    mesh::primary_codes(b.0, b.1, b.2, b.3).iter().map(|c| format!("{c}_{since}")).collect()
}

fn fetch_disaster_cell(key: &str, _scratch: &mut Option<String>) -> Result<Vec<disaster::DisasterSite>, String> {
    let (code, since) = split_disaster_key(key)?;      // "5339_1926" → (5339, 1926)
    let (s, w, n, e) = mesh::shrink(mesh::primary_bbox(code));
    disaster::fetch_sites(s, w, n, e, since)
}

/// 過去災害の発生履歴(NIED 災害事例データベース)。1次メッシュ単位・z11未満では取得しない。
pub fn disaster() -> PlotLayer<disaster::DisasterSite> {
    PlotLayer::new(Layer::Disaster, 11, 0, disaster_cells, fetch_disaster_cell)
}

impl PlotItem for disaster::DisasterSite {
    fn bounds(&self) -> Bbox { (self.lat, self.lon, self.lat, self.lon) }
}
```

`mesh::shrink` を通すのは既存3レイヤと同じで、メッシュ矩形の上端をそのまま渡すと隣のメッシュの
点まで拾ってしまうため。この境界の点は隣のセルが持つ。

`plotcache.rs` 側は `Layer::Disaster` の追加(`dir_name` = `"disaster"`、TTL・上限は §5.2)と、
`ALL_LAYERS` を `[Layer; 4]` から `[Layer; 5]` へ広げるだけ。形式バージョン `v1` は据え置きでよい
(既存4種のファイル形式は変わらない)。

---

## 6. 表示

### 6.1 マーカーの形

同じ座標に何十件も重なるデータなので(§2.1)、**1座標=1マーカー**にして、そこに件数と種別を持たせる。
既存レイヤと同時にONにしても見分けが付くよう、他と違う形にする。

| レイヤ | 形 |
|---|---|
| 道路交通量 | 道路に沿った太い線(道路が無ければ半径3・太さ3のリング) |
| 道路ライブカメラ | 半径3・太さ2のリング(紫) |
| 通行規制 | 区間の線(種別色・太さ3) |
| **過去災害(このレイヤ)** | **中心の小さな塊 + 件数に応じた外周リング**(種別色・太さ1) |

```rust
draw_ring(&mut ov, ix, iy, 1, color, 2);   // 中心(塗りつぶしに見える小さい塊)
draw_ring(&mut ov, ix, iy, r, color, 1);   // 外周(細い1本。件数で半径が変わる)
```

外周を細くするのは、地図と他レイヤを覆い隠さないため。中心の塊があるので細くても位置は読める。

### 6.2 色と大きさ

色は災害種別(その地点で最も件数の多い種別)。既存レイヤが使っている赤・橙・黄・紫・水色と
なるべく離す。

| 種別 | 色(RGB) | 備考 |
|---|---|---|
| 地震災害 | `[235, 80, 80]` | 通行規制の赤と近いが、あちらは線でこちらは点なので形で区別が付く |
| 火山災害 | `[255, 130, 40]` | |
| 風水害 | `[70, 130, 245]` | 既存レイヤに青系が無い。件数の9割近くを占める(§2.4)ので最も目に入る色になる |
| 斜面災害 | `[150, 100, 60]` | 既存レイヤに茶系が無い |
| 雪氷災害 | `[180, 230, 245]` | |
| その他気象災害 | `[160, 160, 160]` | |

大きさはその地点の**総件数**(全種別の合計)で3段階。

| 件数 | 外周半径 |
|---|---|
| 1〜9件 | 2 |
| 10〜49件 | 3 |
| 50件以上 | 4 |

閾値は §2.1 の実測(1地点あたり中央値18件・最大166件)から、3段階が概ね均等に散る位置に置いた。

**死者数・全壊棟数を大きさに使わない。** 被害統計はコード値と null が混ざり(§2.5)、
サンプルの97%が null で、大きさに使うと「被害が無かった」のか「記録が無い」のかを
マーカーが取り違えて伝えてしまう。件数なら集計行の `COUNT` そのもので、この曖昧さが無い。

### 6.3 視認性(#77 と関わる部分)

このレイヤは他の4つより件数が多くなりがちなので、視認性の設計を先に決めておく。

1. **既定OFF**。他の外部データレイヤと同じ扱い(ONにした人だけが外部サービスへ問い合わせる)。
2. **座標で集約済みなので、画面内のマーカー数は市区町村数に等しい。** 1次メッシュあたり14〜118点
   (§2.3)なので、z11の画面で最大でも数十、z14では数点になる。「点が多すぎて地図が埋まる」状態には
   構造的にならない。件数フィルタや間引きは要らないと判断する。
3. **種別フィルタは再取得なしで効く。** 集計が種別ごとの行として届くので、表示側で
   `kinds` を絞れば通信も再計算も無しに切り替えられる。Stage2 で設定に足す(§9)。
4. **色が同系統に偏る前提で作る。** 風水害が9割近いので、既定では画面がほぼ青一色になる。
   これは「この地域は水害が繰り返されてきた」という事実そのもので、色の分散を目的に
   種別を無理に散らさない。地震だけを見たいときは種別フィルタで絞る。
5. **年代しきい値の既定を1926年にする。** 全期間だと西暦567年からの事例が入る。古い事例は位置が
   現代の行政区分からの推定で、被害統計も揃わない。1926年(近代的な統計が揃い始める境界)に
   置くことで、マーカーの件数が「読める記録の積み重ね」を指すようにする。
   実測では5339で全期間5,192件→1926年以降3,637件、集計行236→203行。**通信量のためではなく
   意味のための既定値**である点は §11 にも書く。

### 6.4 ステータス行

既存の `ui_status::plot_label` をそのまま使う。追加は `StatusCtx` に `disaster: PlotStatus` の1つ。

| 状態 | 表示 |
|---|---|
| OFF | (何も出さない) |
| 0件かつ取得中 | `🌊取得中… ` |
| 0件かつズーム下限より広域 | `🌊広域では非表示 ` |
| 0件かつ取得完了 | `🌊記録無し ` |
| 取得済み | `🌊{地点数}地点(B) ` |
| stale | `🌊{地点数}地点(31日前)(B) ` |

`(B)` はカメラの `(N)` と同じで、詳細表示のキーがあることを示す。
`count` には**地点数**を入れる(事例数を入れると数千になり、他レイヤの数字と桁が合わない)。
アイコンは他レイヤ(🚗📷⚠)と衝突しない `🌊` を充てる。

---

## 7. 詳細表示(2段目)

### 7.1 操作

| キー | 動作 |
|---|---|
| `B` | 地図中心に最も近い災害履歴の地点について、事例一覧を中央パネルに表示する。Esc/qで閉じる |

カメラの `N`(中心に一番近いカメラの写真)と同じ形にする。`B` は現在どこにも割り当てられていない
(`ui.rs` の `KeyCode::Char` を全部数えて確認済み。空きは B・F・O・Q・T・U・X・Z・b・e・p・t・u・z)。
防災の頭文字を当てた。

### 7.2 中身

押したときに §4.4 のクエリを1回投げる(バックグラウンドジョブ+`mpsc`。既存の取得と同じ形)。
**結果はディスクへ保存しない**。1地点166件の全文を溜める価値が薄く、押したときだけ取れば足りる。

```
 千葉県 野田市 ─ 記録 89件(1926年以降)
 ─────────────────────────────────
 2019年9月    令和元年房総半島台風        風水害
 2012年3月14日 平成24年千葉県東方沖の地震  地震   死者 記載なし
 2009年10月6日 平成21年台風第18号による…   風水害
 …(新しい順に20件)
 ─────────────────────────────────
 出典: 防災科学技術研究所 災害事例データベース   Esc=閉じる
```

- 日付は `format_date`(§4.5)で「上旬」「夏の中頃」まで含めて出す。
- 被害統計は値がある行だけ添える。`NotRecorded` は出さない(97%が該当するため、出すと画面が
  「記載なし」で埋まる)。`Unknown` / `Reported` は出す(記録として意味があるため)。
- 描画は `ui_overlay.rs` に複数行の中央パネルを1つ足す。既存の `draw_popup` は1行専用なので、
  `draw_onboarding`(複数行の中央パネル)と同じ組み方の関数を新設する。

### 7.3 出典表記

雨雲レーダーが「ONにした直後のメッセージで1回だけ出典を出す」形にしているのと揃え、
レイヤをONにした直後のステータスに `過去災害: 防災科学技術研究所 災害事例データベース` を1回出し、
詳細パネルには常時1行で入れる。

---

## 8. 設定

| 場所 | 値 |
|---|---|
| 設定画面(`,`)の行 | 26 `過去災害 ON/OFF`(`settings.rs` の `its` へ追加。`SETTINGS_ROW_COUNT` を26→27) |
| `Config` フィールド | `disaster_enabled: bool`、既定 `false` |
| `config.toml` | `[disaster] enabled = false` |
| 説明文 | `settings::setting_description(26)` に追加 |

`ui.rs` 側は `26 => { cfg.disaster_enabled = !cfg.disaster_enabled; }` の1行と、
`tick(cx, cy, z, cfg.disaster_enabled)` の1行。ONにしたときの後始末は要らない
(次の `tick()` がセル表を見に行き、キャッシュが fresh ならそのまま出る)。

Stage2で足す設定(§9)。

| 行 | 内容 |
|---|---|
| `▸ 災害の年代` | `すべて` / `1926年以降`(既定) / `1976年以降` の3択(`SettingsPick` のアコーディオン)。切替時にレイヤを作り直す(§5.1) |
| `▸ 災害の種別` | `すべて`(既定) / `地震・火山` / `風水害・斜面` の3択。再取得なしで表示側だけ絞る |

---

## 9. 段階リリース

1. `src/disaster.rs`(型・パース・`format_date`・`DamageValue`)。ネットワークに触れない部分だけで
   単体テストが完結する。実APIを叩く確認は `#[ignore]` に置く。
2. `plotcache::Layer::Disaster` 追加(TTL・上限・`ALL_LAYERS`)。
3. `plotlayer::disaster()` と `PlotItem` 実装、`disaster_cells` / `fetch_disaster_cell`。
   しきい値年は 1926 固定(`AtomicI32` は置くが設定からは触らない)。
4. `ui.rs` 配線: レイヤ生成・`tick`・描画(§6.1)・ステータス(§6.4)・設定行。ここまでで地図に出る。
5. `B` キーの詳細表示(§7)と `ui_overlay.rs` の複数行パネル。
6. 年代・種別の設定(§8 Stage2)、`README.md` / `docs/MANUAL.md` の更新。

4までで機能として成立する。5以降は無くても地図は読める。

---

## 10. 変更するファイル

| ファイル | 変更内容 |
|---|---|
| `src/disaster.rs` | 新規。§4 のすべて |
| `src/plotcache.rs` | `Layer::Disaster` 追加。`dir_name`/`fresh_ttl`(30日)/`stale_limit`(None)/`max_entries`(200)/`max_bytes`(20MB)、`ALL_LAYERS` を5要素へ |
| `src/plotlayer.rs` | `PlotItem for DisasterSite`、`disaster_cells` / `fetch_disaster_cell` / `disaster()`、`DISASTER_SINCE`(`AtomicI32`)と `set_disaster_since` |
| `src/main.rs` | `mod disaster;` |
| `src/config.rs` | `disaster_enabled`(既定false)、`("disaster","enabled")` のパース、保存フォーマットへの1行 |
| `src/settings.rs` | 設定行1つ追加、`SETTINGS_ROW_COUNT` 26→27、`setting_description` |
| `src/ui_status.rs` | `StatusCtx` に `disaster: PlotStatus`、`plot_label(cfg.disaster_enabled, "🌊", "地点", "(B)", "記録無し", &disaster)` |
| `src/ui_overlay.rs` | 詳細一覧の中央パネル(複数行) |
| `src/ui.rs` | レイヤ生成・`tick`・`items`・描画・`map_sig` へ `generation()`・`polling` 条件・設定トグル・`B` キー |
| `docs/MANUAL.md` / `README.md` | キー一覧に `B`、設定一覧に過去災害、保存先の説明に `plot-cache/v1/disaster/` |
| `docs/disaster-history-data-investigation.md` | 調査時に未確定だった災害種別コード・被害統計コード・地点の粒度が確定したので、§2 の実測値へリンクする1行を足す |

`src/ui.rs` と `web/touch-overlay.js` は #87 が編集中のため、実装着手前に競合状況を確認する。

---

## 11. テスト方針

`disaster.rs` はネットワークに触れない部分を全部単体テストにする(既存4モジュールと同じ)。

- `parse_sites`: 実応答の抜粋(集計行)から座標・種別・件数・年幅を読む。同じ座標の複数行が
  1つの `DisasterSite` へ畳まれる。座標欠損の行を捨てる。壊れたJSON・`features` 無し・
  `{"error":{...}}` で空 `Vec`(panicしない)。
- `parse_events`: 名称が `SAIGAI_MEISYO_JMA` → `SAIGAI_MEISYO` → 空の順で選ばれる。両方 null の実在行を含める。
- `DamageValue::from_raw`: null / 0 / 正数 / -1 / -2 / -7 / -8 / 未知の負値の8通り。
- `format_date`: 通常日付・上旬中旬下旬(100/200/300)・季節コード(-10〜-120)・月日 null。
- `DisasterKind::from_code`: `1`〜`5`・`9`・未知コード・空文字。
- `DisasterSite::dominant` / `total`: 同数のときの決まり方を固定する。
- serde 往復(`plotcache` へ保存して読み戻せること。`plotlayer.rs` の
  `all_four_real_item_types_survive_a_trip_through_the_disk_cache` に5つ目として足す)。
- `plotlayer`: `disaster_cells` がキーへしきい値年を付けること、`split_disaster_key` の往復、
  z11でセル数が `MAX_CELLS_PER_JOB` に収まること(既存テストと同じ3地点で)。
- 実APIは `#[ignore]` の `live_fetch_real_disaster_data`(`cargo test --release -- --ignored`)。

---

## 12. 制約・未検証

- **地点は市区町村の代表点で、災害が起きた場所そのものではない。** マーカーが河川や斜面の
  実際の位置を指しているわけではないので、詳細パネルに「市区町村単位の記録」と明示する。
  ツーリング用途で「この峠が危ない」までは読み取れない。
- **サポート対象外のAPI**(公式に問い合わせを受け付けていない)。集計クエリ
  (`groupByFieldsForStatistics`)は通常の属性クエリより踏み込んだ使い方で、仕様変更で
  止まる可能性は素の検索より高い。止まったときは `Err` を返して手元のキャッシュを出し続ける
  (fresh 30日・stale無制限なので、一度取れた土地は止まっても表示が続く)。
- **`fX`/`fY` が常に幾何と一致する保証は無い。** 実測では一致し、こちらの方が丸め誤差が無いが、
  データ更新でずれた場合はマーカー位置が飛ぶ。ずれの検出は入れない(検出のために幾何も取ると
  集計の利点が消えるため)。
- **1次メッシュあたりの集計行数の上限に対する余裕は実測4メッシュぶんのみ。** 最大236行/2,000行
  なので8倍以上の余裕があるが、全国の全メッシュを確認したわけではない。実装時に
  `exceededTransferLimit` が真なら警告を出して打ち切りが起きたことを分かるようにする。
- **応答が1.1〜1.2秒かかる**(5339で3回計測)。1画面最大9セルを直列に回すと最悪10秒強になる。
  既存の `PlotLayer` は1セル取れるたびに送るので画面は段階的に埋まるが、z11で新しい土地へ
  入った直後はしばらく虫食いに見える。
- **年代しきい値の既定1926年は意味による判断で、実測に基づく最適値ではない。** 定数1つなので、
  使ってみて古い記録も見たくなれば動かす。
- **災害種別の色は他レイヤとの同時表示を机上で確認しただけ。** 実際に3レイヤ同時ONで
  走らせた見え方は未確認。Stage4で確かめる。
- **詳細表示の `B` キーは未使用キーであることだけを確認した。** 押しやすさ・覚えやすさは
  実際に使ってみないと分からない。
