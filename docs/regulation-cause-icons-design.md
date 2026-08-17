通行規制(regulation.rs)の規制原因(cause)を分類し、事故は✕、工事は黄色いアイコンを
地図上の該当区間に重ね描きする機能の仕様。

## 0. 前提調査(実データ、2026/08/17)

関東〜中部の36メッシュから通行止め(Closed)205件を検出し、60件をサンプルして規制原因を
集計した結果:

```
16  災害等
13  道路損壊
 9  道路工事
 6  土砂崩れ
 5  落石
 3  工事
 3  作業
 3  架橋工事
 2  災害工事
```

**「事故」は0件。** このデータソース(road-info-prvs)は災害・道路損壊・工事系が主で、
通行可能なまま残る単発事故は元々ここに現れない。事故アイコンは実装しても現状ほぼ表示
されない見込みだが、将来別データ源(例: 警察発表系)が加わった時の受け皿として、ユーザー
了承のうえ両方実装する。工事系(工事/道路工事/架橋工事/作業/災害工事)はキーワードで
拾える。

## 1. 決定事項

| # | 論点 | 結論 |
|---|---|---|
| 1 | 分類方法 | `ClosureDetail.cause`(自由記述文字列)へのキーワード一致。「事故」を含む→Accident、「工事」または「作業」を含む→Construction、どちらも無ければOther(アイコン無し、従来通り線のみ) |
| 2 | 対象 | `RegulationKind::Closed`のみ(既存のnogo回避・Tキー詳細と同じ扱い範囲)。車線規制等は対象外(範囲を広げると原因取得の通信量が増えるため、まずは通行止めに絞る) |
| 3 | 原因データの取得 | 表示中(bbox内)のClosedかつdetail_idを持つイベントのうち、まだ分類済みでないものを**1フレームにつき1件だけ**バックグラウンドで`regulation::fetch_detail`し、結果をメモリ上のキャッシュ(`HashMap<detail_id, CauseCategory>`)へ積む。同時に走らせるのは1ジョブのみ(既存のTキー詳細取得ジョブと同じ非ブロッキング方式) |
| 4 | キャッシュ | メモリのみ(ディスク保存しない)。規制原因は一度確定した内容が事後変わることは想定しにくく、無期限保持で問題ない。プロセス再起動時は再取得(小さなHTMLの再取得コストは軽微) |
| 5 | アイコン位置 | 規制ラインの中点(`roadtrace::polyline_len`+`point_at`で算出) |
| 6 | アイコン見た目 | 事故=✕(新規マーカー形状、対角線の交差)・赤系。工事=三角(既存の警告的形状)・黄色 |
| 7 | 描画 | 通行規制のライン自体は`OverlaySpec`を経由せず、ui.rs内で直接`OverlayLayer`へ`draw_line`する専用の描画ブロック(`if cfg.regulation_enabled { for ev in &regulation_events { ... draw_line(...) } }`)を持つ。アイコンもこの同じブロック内に`render::draw_marker`を追加呼び出しして重ね描きする(`OverlaySpec`に新フィールドは追加しない) |

## 2. 実装

### 2.1 regulation.rs: 純関数

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CauseCategory { Accident, Construction, Other }

// cause(自由記述文字列)からキーワード一致で分類する。「事故」→Accident、
// 「工事」または「作業」を含む→Construction、どちらも無ければOther。
pub fn categorize_cause(cause: &str) -> CauseCategory

// 分類→(色, マーカー形状)。Otherはアイコン無し(None)。
pub fn cause_icon(category: CauseCategory) -> Option<([u8; 3], u8)>

// 規制ラインの中点(アイコンを置く位置)。空のラインはNone。
pub fn closure_icon_position(line: &[(f64, f64)]) -> Option<(f64, f64)>
```

### 2.2 ui.rs: 選定ロジック(純関数、ui.rs内でテスト)

```rust
// 表示中のClosedイベントのうち、まだcauseキャッシュに無い最初の1件のdetail_idを返す。
// 無ければNone(=今フレームは新規フェッチしない)。detail_id空文字は対象外。
fn next_closure_to_categorize<'a>(
    visible: &[&'a ClosureEvent],
    cached: &std::collections::HashMap<String, regulation::CauseCategory>,
) -> Option<&'a str>
```

### 2.3 ui.rs: 配線

- state: `cause_cache: HashMap<String, regulation::CauseCategory>`(セッション内メモリのみ)、
  `cause_job: Option<Receiver<Result<regulation::ClosureDetail, String>>>`
- 毎フレーム、`regulation_enabled`かつ`cause_job.is_none()`の時だけ
  `next_closure_to_categorize(&visible_closed, &cause_cache)`を呼び、Someならバックグラウンドで
  `regulation::fetch_detail`を1件だけ起動する(Tキーの詳細取得と同じ inline スレッド方式)
- ポーリングで結果を受け取ったら`categorize_cause(&detail.cause)`を`cause_cache`へ格納し
  `cause_job = None`(失敗時もキャッシュへ`Other`相当を入れて無限リトライを避ける)
- 既存の`if cfg.regulation_enabled { for ev in &regulation_events { ... draw_line(...) } }`
  ブロック内で、`ev.detail_id`が`cause_cache`にAccident/Constructionとして載っていれば、
  `closure_icon_position(&ev.line)`+`cause_icon`で得た座標・色・形状を`render::draw_marker`で
  同じ`ov`(OverlayLayer)へ重ね描きする(`draw_line`によるライン描画のすぐ後)
- jobs_active/polling/Ctrl-C中断チェーンに`cause_job`を含める

### 2.4 render.rs

`draw_marker`を`pub fn`にする(既存の`draw_line`/`draw_ring`と同じく、ui.rsのink描画ブロックから
直接呼べるようにするため)。`OverlaySpec`に新フィールドは追加しない(通行規制の描画は元々
`OverlaySpec`を経由しない別経路のため)。

### 2.5 marker形状

`render.rs`の`marker_inside`に形状6(対角線の✕、`dx.abs() == dy.abs()`)を追加し、
`NUM_MARKER_SHAPES`を6→7にする。工事は既存の三角(形状1)を流用する。

## 3. テスト

- `categorize_cause`: 「事故」を含む→Accident / 「工事」を含む→Construction /
  「作業」を含む→Construction / 「道路損壊」等の無関係な文字列→Other / 空文字→Other /
  「事故」と「工事」両方を含む場合はAccident優先(事故の方が重要度が高いと判断)
- `cause_icon`: Accident/Constructionはそれぞれ異なる(色,形状) / Otherは`None`
- `closure_icon_position`: 2点の直線→中点 / 空ライン→None / 1点のみ→その点
- `next_closure_to_categorize`: 未キャッシュの最初の1件を返す / 全件キャッシュ済みなら`None` /
  detail_id空文字はスキップされる / 複数件visible時は先頭優先(順序はvisibleの並び順)
- marker形状6(✕)がshape 0〜5と異なる範囲を塗ること(既存の`marker_inside`テストがあれば
  そこに追加、無ければ新設)

## 4. 対象外(今回はやらない)

- `RegulationKind::Closed`以外(LaneRestriction等)へのアイコン適用
- 原因カテゴリのディスク永続化(メモリのみ。再起動での再取得コストは許容範囲)
- 事故データの拡充(警察発表等の別データ源の追加調査)
