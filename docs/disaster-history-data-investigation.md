過去災害データ(豪雨/地震/台風等)を地図にプロットする機能(#75)のための、データソース調査結果
(2026/08/16、実APIアクセスで確認済み)。

## 結論

防災科学技術研究所(NIED)の「災害事例データベース」がArcGIS Server REST API経由で
無料・認証不要・GeoJSON形式で提供されている。実際にAPIを叩いてデータ取得を確認済み。
実装は可能と判断する。設計は別途行う。

## データソース

| 項目 | 値 |
|---|---|
| 運営元 | 防災科学技術研究所(NIED)「災害事例データベース」(`https://dil.bosai.go.jp/`) |
| APIエンドポイント | `https://agis.bosai.go.jp/webgis/rest/services/dil-db/saigai/MapServer/0/query` |
| レイヤー0 | `災害事例`(ポイント、`esriGeometryPoint`)。レイヤー1は`都道府県`(ポリゴン、境界表示用) |
| 認証 | 不要 |
| 料金 | 無料 |
| レスポンス形式 | `f=geojson`指定でGeoJSON(`f=json`だとArcGIS標準のJSON) |
| サポート | 公式サイトに「APIの内容や使用法に関する問い合わせはお受けしておりません」と明記。自己責任で使う想定 |

### クエリ例(実際に確認したリクエスト)

```
https://agis.bosai.go.jp/webgis/rest/services/dil-db/saigai/MapServer/0/query
  ?where=SAIGAI_YEAR>=1990
  &outFields=JIREI_BANGO,SAIGAI_MEISYO,SAIGAI_MEISYO_JMA,SAIGAI_YEAR,SAIGAI_MONTH,SAIGAI_DAY,SAIGAI_SYUBETSU_1,BASHO_KEN,BASHO_SHI,SHIBOU_SU
  &geometry=139.0,35.0,140.5,36.2&geometryType=esriGeometryEnvelope
  &inSR=4326&outSR=4326&f=geojson&resultRecordCount=5
```

`geometry`にbbox(west,south,east,north)、`geometryType=esriGeometryEnvelope`、`inSR=4326`
(WGS84度単位)を指定すれば、既存のtraffic.rs/roadsearch.rs等と同じ緯度経度ベースで問い合わせできる。
サービス自体の内部投影はWebメルカトル(WKID 102100/3857)だが、`inSR`/`outSR`指定でWGS84のまま
やり取りできることを確認済み。

### 主なフィールド(全体はAPI技術情報ページ`https://dstr.mhr.bosai.go.jp/dedb/API.html`参照)

| フィールド | 意味 |
|---|---|
| `JIREI_BANGO` | 事例番号(一意キー) |
| `SAIGAI_MEISYO` / `SAIGAI_MEISYO_JMA` | 災害名称(気象庁命名) |
| `SAIGAI_YEAR` / `SAIGAI_MONTH` / `SAIGAI_DAY` | 発生年月日(西暦)。月・日は「上旬/中旬/下旬」等のコード値が入ることがある |
| `SAIGAI_SYUBETSU_1/2/3` | 災害種別(コード値。例: `3`=風水害, `4`=斜面災害, `9`=その他気象災害。地震・津波等のコードは別途確認要) |
| `BASHO_KEN` / `BASHO_GUN` / `BASHO_SHI` | 発生場所(都道府県/郡/市区町村) |
| `SHIBOU_SU` / `YUKUEHUMEI_SU` / `SHISHO_SU` | 死者数/行方不明者数/死傷者数(コード値: `0`=被害なし, `-1`=不明, `-2`=被害あり等が混じる。実数と非実数コードの区別が必要) |

## 注意点(実装時に考慮すること)

1. **データ量が非常に多い**: 関東地方相当のbbox(約1.5度四方)で1990年以降に絞っても
   `exceededTransferLimit: true`が返り、デフォルトの1ページあたりの件数上限を超える。
   全期間(西暦567年のデータまで確認済み)を対象にすると、狭い範囲でも大量になる。
   年代・災害種別でのフィルタと、ズームレベルに応じたbbox絞り込みが必須。
2. **年代が古代まで遡る**: 位置情報が現代の行政区分に基づく推定であることが多く、
   古い時代のデータは地図表示の精度・意味が薄い可能性がある。表示対象年代を
   (例えば直近100年等に)絞ることを検討する。
3. **数値フィールドにコード値が混在**: `SHIBOU_SU`等の被害数フィールドは、実数の他に
   `-1`(不明)`-2`(被害あり・詳細不明)等のコード値が混じるため、そのまま数値として
   扱うと誤読する。パース時にコード値を除外する処理が要る。
4. **サポート対象外の非公式API**: 公式に問い合わせを受け付けていないため、仕様変更・
   停止のリスクは通常の政府オープンデータより高いと見て、取得失敗時のフォールバック
   (前回データ保持等、既存のtraffic/roadsearchと同じ方針)を必須にする。

## 次のステップ

調査時に未確定だった災害種別コード(6分類・詳細36種)・被害統計のコード値・地点の粒度
(市区町村の代表点で1点に最大166件が重なる)は、設計時の実測で確定した。値の一覧は
`docs/disaster-history-overlay-design.md` §2 にある。

設計フェーズへ進める。既存の`traffic.rs`/`camera.rs`/`regulation.rs`と同じ構成
(fetch関数・パース関数・ui.rsへの配線)に倣う想定。表示は災害種別ごとに色分けした
マーカー(既存の交通量・カメラマーカーと同系統)が妥当と考えられるが、データ量の多さを
踏まえたフィルタ・間引き設計は別途詰める。
