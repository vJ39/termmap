# termmap 対話モードの自動デモ撮影 (vhs)
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

# 地図をパン(hjklは常時高速パン) — 見た目に動きを出す
Type "lll"
Sleep 400ms
Type "jj"
Sleep 400ms
Type "kkk"
Sleep 400ms
Type "hh"
Sleep 1s

# ルートを作る: 中心に始点を置く
Type "v"
Sleep 800ms

# 移動して終点を置く → BRouterが自動計算しルート線が引かれる
Type "llllll"
Sleep 200ms
Type "kkk"
Sleep 800ms
Type "v"
Sleep 5s

# モード切替(下道→高速)。ルートが引き直される
Type "m"
Sleep 5s

# 走りまくり: 距離ゲージを開いて距離を伸ばす→Enterでバックグラウンド検索(スピナー)→周回ルート
Type "W"
Sleep 800ms
Right@250ms 4
Sleep 500ms
Enter
Sleep 10s

# 標高プロファイルを表示(周回ルートの峠のアップダウンが見える)
Type "E"
Sleep 3s

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
