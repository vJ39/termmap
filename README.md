# termmap

OSM ラスタタイルを端末に描画する mapscii 風レンダラ。

## build

    cargo build --release

バイナリ: `target/release/termmap`

## usage

    termmap --place "住所"  [options]
    termmap --lat LAT --lon LON  [options]
    termmap --image PNG  [options]

## options

    --place STR     住所/地名をジオコーディング(Nominatim)して中心にする
    --lat LAT       中心の緯度
    --lon LON       中心の経度
    --zoom Z        ズーム 0..=20 (既定 14)
    -i, --interactive   対話モード (矢印=パン, +/-=ズーム, q=終了)
    --braille       点字ドットで描画
    --mono          色なし (braille をプレーンテキスト化)
    --classify      地物カテゴリ色分け (水域/緑地/幹線道路/線路/建物)
    --width N       出力桁数 (既定=端末幅)
    --win N         取得する地理窓のピクセル辺長 1..=2048 (既定 640)
    --threshold T   braille 非 classify 時の暗さ閾値 (既定 195)
    --png OUT       カテゴリ色の PNG を書き出して終了
    --image PNG     既存 PNG を描画 (タイル取得なし)

## examples

    termmap --place "東京都北区田端" --zoom 15 --classify
    termmap --lat 35.7495 --lon 139.7376 -i
    termmap --place "王子" --zoom 15 --braille --mono --width 100
    termmap --lat 35.75 --lon 139.74 --zoom 15 --classify --png out.png

## notes

- タイル: `tile.openstreetmap.org` (© OpenStreetMap contributors, ODbL)
- ジオコーディング: Nominatim
- 描画は「ラスタ画像を端末文字に変換」。classify の地物判定はピクセル色からの推定。
