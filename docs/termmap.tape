# termmap 対話モードの自動デモ撮影 (vhs)
# 生成: cd リポジトリ && vhs docs/termmap.tape   (要 dangerouslyDisableSandbox 相当のネットワーク)
Output docs/demo.gif
Set Shell zsh
Set FontSize 16
Set Width 1200
Set Height 700
Set Padding 8
Set Framerate 10
Set TypingSpeed 40ms

Hide
Type "/Users/work/.ghq/github.com/vJ39/termmap/target/release/termmap --place 東京駅 -i --style voyager"
Enter
Sleep 5s
Show
Sleep 1s

# 始点を置く
Down@250ms 2
Right@250ms 2
Sleep 700ms
Type "s"
Sleep 1200ms

# 移動して終点(ここでBRouterが計算)
Right@250ms 6
Up@250ms 3
Sleep 700ms
Type "e"
Sleep 5s

# モード切替(下道→高速)
Type "m"
Sleep 4s

# 並べ替えパネル
Tab
Sleep 2s
Down
Sleep 800ms
Escape
Sleep 700ms

# 目的地検索(ガソスタ)
Type "f"
Sleep 700ms
Type "1"
Sleep 5s
Escape
Sleep 700ms

# ヘルプ
Type "?"
Sleep 2500ms
Space
Sleep 600ms
Type "q"
Sleep 800ms
