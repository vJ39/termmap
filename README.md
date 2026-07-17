OSMラスタタイルを端末にhalfblock/braille/実画像で描画し、POI・ルート・航続リング・マイスポットを重畳するツーリング計画ツール。地名検索・ルート作成・目的地検索・お気に入り管理をすべて対話モード(キー操作)で完結できる。macOSを主眼に開発されているが、地図描画やルート計算など主要機能はLinux(x86_64)でも動作確認済み。macOS限定なのはライブ現在地取得(`--here`、CoreLocationCLI依存)のみ。

## screenshots

![color halfblock](docs/demo-color.png)
![braille](docs/demo-braille.png)

対話モードの操作イメージ(地点を置く→モード切替→並べ替え→目的地検索→ヘルプ)。

![対話モードのデモ](docs/demo.gif)

## 対応OS

macOS 想定。GPS連携(`--here`・ライブ現在地)は CoreLocationCLI 依存のため macOS 限定。地図描画・ルート計算・検索は他OSでも動く可能性はあるが未検証。

Linux(x86_64)は `--target x86_64-unknown-linux-musl` でのクロスコンパイルと動作(`--help`・`--place`+PNG出力)を確認済み。追加の依存修正は不要だった。

## インストール

    cargo build --release

バイナリ: `target/release/termmap`

## 使い方

    termmap                      引数なし=前回位置から対話起動(保存が無ければ東京中心)
    termmap --place "住所"  [options]
    termmap --lat LAT --lon LON  [options]
    termmap --here | --resume  [options]
    termmap --image PNG  [options]

引数なしの `termmap` で前回終了時の位置から対話モードが立ち上がり、キー操作で地図を動かしながらルート・目的地・お気に入りを組み立てられる。前回の保存が無ければ東京中心で開く。

    termmap                   前回位置(なければ東京)で対話起動
    termmap --place "東京駅"    場所を指定して対話起動

`-i` / `--interactive` は対話が既定になる前からの後方互換エイリアス。付けても付けなくても対話で起動する。

## 主要機能

- **地名・住所検索**: Google Geocoding(APIキー設定時・優先)→ Nominatim の順にフォールバック。番地まで含めて0件のときは大字/町名レベルに丸めて再検索する
- **検索結果キャッシュ**: キーワード+位置をキーにした結果をローカルに保存し、同条件の再検索はAPIを叩かない
- **周辺キーワード検索**: 表示中の範囲内で施設名/ブランド名を Overpass で部分一致検索(例: 「セブン」でセブン-イレブンも拾う)
- **目的地カテゴリ検索**: ガソスタ/カフェ/コンビニ/道の駅/展望/公園/峠道の7カテゴリをワンキーで検索
- **ルート作成**: 中心クロスヘアに地点を置く(並び順で始点→…→終点が自動)、並べ替えパネル(左袖)での組み立て、道路名/refでの経路追加(複数連結可)、代替ルート候補の巡回
- **走りまくり**: 現在地から峠・展望スポットを巡る周回(または片道)ルートを自動生成
- **お気に入りルート**: 名前を付けて保存/呼び出し、一覧表示
- **マイスポット**: カテゴリ階層(登録・改名・並べ替え・色分け)で任意の地点を保存。GoogleマップURLを貼るだけで座標+店名を抽出登録できる
- **標高プロファイル・ルート再生・ライブ現在地**: 確定ルートの高低差表示、プレビュー走行アニメ、CoreLocationCLI経由の現在地トラッキング
- **実写(Street View)**: 中心地点の実写を全画面表示(要 Google APIキー)
- **QR共有**: ルートをGoogleマップ経路URL化し、端末にQRコードを表示してスマホで開ける
- **2階層Spaceメニュー**: 全操作をキー無しでも選べる(カテゴリ→項目)。熟練者は各項目のキーを直打ちしてもよい
- **設定画面**: 描画スタイル・ルート既定・APIキー等を実行中に切り替え、`config.toml` へ保存できる

## config.toml

場所: `~/.config/termmap/config.toml`

```toml
[llm]
recommend_enabled = true
model = "claude-sonnet-5"
command = "claude"

[route]
profile = "car-fast"
sample_interval_m = 800.0

[display]
style = "osm"
show_spots = true
braille = false
classify = false
edge = false
mono = false

[google]
maps_api_key = ""

[streetview]
enabled = true
```

- `[google] maps_api_key`: 地名検索(Geocoding)と実写(Street View)で共通に使うキー。環境変数 `TERMMAP_GOOGLE_API_KEY` があればこちらを優先(configにキーを書かず運用できる)
- 旧スキーマ `[streetview] api_key` は後方互換で読める(`[google] maps_api_key` が空のときのみ採用)
- 未設定でも動く。地名検索は Nominatim のみに、実写は「APIキー未設定」表示になる

対話モードの設定画面(`,`)からも同じ項目を切り替えて保存できる。

## 必要な外部依存

- 地図タイル: `tile.openstreetmap.org`(標準)、CARTO(voyager/dark/light)、OpenTopoMap(topo。地形陰影・等高線)
- 地名検索・逆ジオコーディング: Nominatim(無料・キー不要)
- 地名検索(優先): Google Geocoding API(任意・要APIキー)
- ルーティング: BRouter(公開API)
- 目的地・周辺検索: Overpass API
- 実写: Google Street View Static API(任意・要APIキー)
- GPS/現在地: CoreLocationCLI (`brew install corelocationcli`。macOSのみ、初回は位置情報の許可が必要)
- おすすめ機能: `claude` CLI (Claude Code。config.toml `[llm] command` で変更可)

## options

### 中心の指定
    --place STR     住所/地名を検索して中心にする(Google Geocoding優先→Nominatim)
    --lat LAT       中心の緯度
    --lon LON       中心の経度
    --resume        前回終了時の位置/ズーム/style/ルートを復元 (--last 同義)
    --here          GPS/測位で現在地を中心にする (要 CoreLocationCLI + 位置情報許可)

### 表示
    --zoom Z        ズーム 0..=20 (既定 14)
    --style NAME    タイル種別 osm|voyager|dark|light|topo (既定 osm)。voyager/dark/light はラベル無し、topoは地形陰影・等高線(OpenTopoMap)
    -i, --interactive   対話モードの後方互換エイリアス (対話は既定。下記キー参照・詳細は docs/MANUAL.md)
    --braille       点字ドットで描画
    --mono          色なし (braille をプレーンテキスト化)
    --classify      地物カテゴリ色分け (水域/緑地/幹線道路/線路/建物)
    --edge          輪郭抽出 (道路/建物/川の境界を線画化)。clean な --style と併用
    --width N       出力桁数 (既定=端末幅・1..=1024)
    --threshold T   braille/edge の閾値 (braille 既定 195, edge 既定 45)

### ツーリング (重畳)
    --range KM,..   航続距離リング(複数可)。中心 or --home 基準
    --home LAT,LON  リングの基準点 (省略時は地図中心)
    --route "LAT,LON;LAT,LON[;..]"  ルート(始点;経由;終点)を BRouter で計算し重畳
    --route-mode M  surface(下道/高速回避) | highway(高速OK) | short(最短)。既定 surface
    --gpx OUT       ルートを GPX 書き出し
    --save-route N  現在の --route を名前 N でお気に入り保存
    --load-route N  お気に入り N を読み込む(始点を中心に)
    --routes        お気に入り一覧を表示
    --share         ルートをGoogleマップ経路URL+端末QRで出力(スマホで開く)
    --wander        峠/展望を巡る周回(または片道)ルートを自動生成
    --dist KM       走りまくりの目安距離 (既定 40)
    --shape S       走りまくりの形状 loop(周回)|oneway(片道) (既定 loop)

### 出力
    --png OUT       カテゴリ色の PNG を書き出して終了
    --image PNG     既存 PNG を描画 (タイル取得なし・地理原点が無いため重畳は不可)

## interactive (-i) キー概要

キー全体・Spaceメニューの構造・各画面の詳細操作は `docs/MANUAL.md` を参照。

    移動   ←↑↓→ パン(既定は細かく・押し続けで加速/Shift+矢印で常に高速) / hjkl 矢印と同じ(大文字HJKLで常に高速) / + - ズーム / Space メニュー
    場所   / 住所・地名で検索して移動 / a 中心の住所
    ルート点   v 中心クロスヘアに地点を置く(並び順で始点→…→終点が自動)
    編集   Tab で並べ替えビューへ(地図のままw/sでも一覧を上下できる) / [ ] 選択点を前後へ並替 / x 選択点を削除
    ルート設定   m モード(下道→高速→最短) / c ルート消去(確認あり) / g GPX保存 / n 代替ルート / r 道路名で追加 / W 走りまくり
    目的地 f カテゴリ検索(1-7)→左袖リスト(↑↓/ws選択 / v 追加 / Enter移動 / f 再検索 / Esc 閉)
    お気に入り  S 保存/呼び出しの小メニュー / P マイスポット
    表示・ナビ  E 標高プロファイル(高さ目盛り付き) / A ルート再生(実速度・[ ]で速度調整) / G ライブ現在地 / i 実写(+/-でズーム) / V スポット表示切替 / o QR共有
    設定   , 設定画面(braille/classify/edge/mono/style等。3択以上の項目はEnterでその場にアコーディオン展開。変更は自動保存)
    終了   ?  ヘルプ   q  終了   Esc  サブモード取消   Ctrl+C  通信中の処理を中断(終了はq)

- 目的地カテゴリ: 1ガソスタ 2カフェ 3コンビニ 4道の駅 5展望 6公園 7峠道
- ルートの下道=BRouter moped(高速回避) / 高速=car-fast / 最短=shortest。高速時は料金概算(高速km×¥30, 普通車概算)を表示
- 初回起動時は簡易オンボーディング(Space/?/qの案内)を表示。以後は出さない

## examples

    termmap
    termmap --place "東京都北区田端" --zoom 15 --classify
    termmap --place "王子駅" --zoom 16 --edge --mono --style voyager --width 92
    termmap --place "王子駅" -i --style voyager
    termmap --resume -i
    termmap --lat 35.75 --lon 139.74 --range 20,40 --png out.png
    termmap --route "35.737,139.760;35.659,139.773" --route-mode surface --gpx ride.gpx
    termmap --load-route 台場 -i
    termmap --home 35.68,139.76 --wander --dist 60 --shape loop -i

## トラブルシュート

- 検索が0件(「見つからない」)と通信・サーバ障害は区別される。通信障害時はメッセージにエラー内容が出る
- 実写(`i`)が「APIキー未設定」になる場合は `config.toml` の `[google] maps_api_key`(または環境変数 `TERMMAP_GOOGLE_API_KEY`)を設定する
- `--here` やライブ現在地(`G`)が動かない場合は `brew install corelocationcli` の有無と、システム設定 > プライバシーとセキュリティ > 位置情報サービスでの許可を確認する
- おすすめ(`@`)が使えない場合は `config.toml [llm] recommend_enabled` と `claude` CLI の有無を確認する
- 対話モード終了時の状態(位置/ズーム/style/ルート)は自動保存され、次回 `--resume` で復元できる

## notes

- タイル: `tile.openstreetmap.org` (© OpenStreetMap contributors, ODbL)、CARTO(voyager/dark/light)、OpenTopoMap(topo。© OpenTopoMap (CC-BY-SA))
- ジオコーディング/逆ジオコーディング/語検索: Nominatim。優先で Google Geocoding
- ルーティング: BRouter (公開API)。目的地・周辺検索: Overpass API
- 料金は概算(高速区間 × ¥30/km, 普通車, 割引なし)。実額とは異なる
- お気に入りルート: `~/.config/termmap/routes/<名前>.txt`
- マイスポット: `~/.config/termmap/spots.txt` / カテゴリ: `~/.config/termmap/spot-categories.txt`
- 検索キャッシュ: `~/.config/termmap/search-cache.tsv`
