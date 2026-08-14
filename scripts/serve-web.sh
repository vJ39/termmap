#!/usr/bin/env bash
#
# termmap をブラウザから操作できる状態で起動する(ttyd + タッチ操作オーバーレイ)。
#
# 待ち受けは 127.0.0.1 のみ。インターネットへ出す場合は別ターミナルで
#   cloudflared tunnel --url http://127.0.0.1:7681
# を手で起動して、表示された https URL をスマホで開く運用にしている。
# このスクリプトはトンネルには一切関与しない。
#
# 事前に必要なもの:
#   - cargo build --release   (./target/release/termmap)
#   - scripts/build-web-index.sh  (web/index.html の生成。初回と ttyd 更新時)
#
# 使い方:
#   export TERMMAP_WEB_USER=your-name
#   export TERMMAP_WEB_PASS=your-strong-password
#   scripts/serve-web.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INDEX="$REPO_ROOT/web/index.html"
BIN="$REPO_ROOT/target/release/termmap"
PORT="${TERMMAP_WEB_PORT:-7681}"

# 認証情報は必ず環境変数から受け取る。既定値やスクリプト内の固定値は置かない
# (このまま外部トンネルへ晒される前提のため、資格情報をリポジトリに残さない)。
if [ -z "${TERMMAP_WEB_USER:-}" ] || [ -z "${TERMMAP_WEB_PASS:-}" ]; then
  cat >&2 <<'EOS'
error: TERMMAP_WEB_USER と TERMMAP_WEB_PASS を設定すること。

  export TERMMAP_WEB_USER=your-name
  export TERMMAP_WEB_PASS=your-strong-password
  scripts/serve-web.sh

既定のユーザー名・パスワードは用意していない(公開経路に出す前提のため)。
EOS
  exit 1
fi

command -v ttyd >/dev/null 2>&1 || { echo "error: ttyd が見つからない (brew install ttyd)" >&2; exit 1; }
[ -x "$BIN" ]   || { echo "error: $BIN が無い。先に cargo build --release" >&2; exit 1; }
[ -f "$INDEX" ] || { echo "error: $INDEX が無い。先に scripts/build-web-index.sh" >&2; exit 1; }

echo "termmap web: http://127.0.0.1:$PORT  (user: $TERMMAP_WEB_USER)"
echo "公開する場合は別ターミナルで: cloudflared tunnel --url http://127.0.0.1:$PORT"

# ブラウザ(xterm.js)は iTerm2 のインライン画像(OSC 1337)に対応していない。
# ttyd はこのスクリプトを起動したシェルの環境変数をそのまま子プロセスへ渡すため、
# iTerm2 から起動すると TERM_PROGRAM=iTerm.app / LC_TERMINAL=iTerm2 が termmap まで
# 届いてしまい、termmap 側の image_capable() が真になって地図を実画像で描こうとする。
# その結果ブラウザでは地図が何も表示されない(画像用のエスケープが大量に流れるだけ)状態になる。
# 接続してくるのは iTerm2 ではなくブラウザなので、この3つは子へ渡さない。
SCRUB_ENV=(env -u TERM_PROGRAM -u LC_TERMINAL -u ITERM_SESSION_ID)

# -W  書き込み可(既定は読み取り専用なので、これが無いと操作できない)
# -i  127.0.0.1 に限定(直接インターネットへ晒さない)
# -t  ブラウザ側 xterm.js のオプション
exec "${SCRUB_ENV[@]}" ttyd \
  -i 127.0.0.1 \
  -p "$PORT" \
  -W \
  -c "$TERMMAP_WEB_USER:$TERMMAP_WEB_PASS" \
  -I "$INDEX" \
  -t 'disableLeaveAlert=true' \
  "$BIN" "$@"
