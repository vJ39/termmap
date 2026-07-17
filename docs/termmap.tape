# termmap 対話モードの自動撮影 (vhs)
# 生成: cd リポジトリ && vhs docs/termmap.tape   (要ネットワーク: Overpass/BRouter/Nominatim/標高API)
# 舞台: 箱根峠(道路網・峠道/展望スポットが豊富でWの走りまくりが映える)
Output docs/demo.gif
Set Shell zsh
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 8
Set Framerate 12
Set TypingSpeed 40ms

Hide
Type "/Users/work/.ghq/github.com/vJ39/termmap/target/release/termmap --place 箱根峠 --style voyager"
Enter
Sleep 6s
Show
Sleep 1s

# 初回起動の案内が出ていれば何かキーで閉じる(以後は出ない)。
# 案内が無い場合はSpaceでメニューが開くだけなのでEscapeで閉じておく(どちらでも安全)。
Space
Sleep 500ms
Escape
Sleep 500ms

# 地図をパン。大文字HJKLは常時高速(Shift+矢印の代替) — 見た目に大きく動かす
Type "LLL"
Sleep 400ms
Type "JJ"
Sleep 400ms
Type "KKK"
Sleep 400ms
Type "HH"
Sleep 1s

# ルートを作る: 中心に始点を置く
Type "v"
Sleep 800ms

# 移動して終点を置く → BRouterが自動計算しルート線が引かれる
Type "LLLLLL"
Sleep 200ms
Type "KKK"
Sleep 800ms
Type "v"
Sleep 5s

# w/s: Tabで一覧へ入らなくても地図のまま左袖(ルート一覧)を上下できる
Type "w"
Sleep 400ms
Type "s"
Sleep 600ms

# モード切替(下道→高速)。ルートが引き直される
Type "m"
Sleep 5s

# 設定画面: 地図種別のようなアコーディオン展開(その場でインデント展開)をちらっと見せる
Type ","
Sleep 500ms
Type "ssss"
Sleep 400ms
Enter
Sleep 600ms
Type "s"
Sleep 400ms
Escape
Sleep 400ms
Escape
Sleep 600ms

# 走りまくり: 距離ゲージを開いて距離を伸ばす→Enterでバックグラウンド検索(スピナー)→周回ルート
Type "W"
Sleep 800ms
Right@250ms 4
Sleep 500ms
Enter
Sleep 10s

# 標高プロファイルを表示(高さ目盛り付き。周回ルートの峠のアップダウンが見える)
Type "E"
Sleep 3s

# お気に入りルート(保存/呼び出しの小メニュー)をちらっと見せる
Type "S"
Sleep 1200ms
Escape
Sleep 500ms

# スマホ共有QRをポップアップ表示
Type "o"
Sleep 3s
Escape
Sleep 500ms

# 締めにヘルプ画面をちらっと見せる
Type "?"
Sleep 2500ms
Space
Sleep 600ms

Type "q"
Sleep 800ms
