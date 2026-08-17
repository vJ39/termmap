#!/usr/bin/env bash
#
# termmap をブラウザから操作できる状態で起動する(ttyd + タッチ操作オーバーレイ)。
#
# 認証は ttyd の Basic 認証ではなく、手前に置く webauth-proxy の Cookie 認証で行う
# (iOS Safari が Basic 認証の資格情報を裏の /token fetch や WebSocket ハンドシェイクへ
#  再利用できず、Cloudflare Tunnel 経由だと繋がらないため。docs/web-auth-proxy-design.md)。
#
#   ブラウザ ⇄ webauth-proxy:7681(公開) ⇄ ttyd:17681(認証なし・127.0.0.1のみ) ⇄ termmap
#
# 待ち受けは両方とも 127.0.0.1 のみ。インターネットへ出す場合は別ターミナルで
#   cloudflared tunnel --url http://127.0.0.1:7681
# を手で起動して、表示された https URL をスマホで開く運用にしている。
# このスクリプトはトンネルには一切関与しない。
#
# 事前に必要なもの:
#   - cargo build --release   (./target/release/termmap と ./target/release/webauth-proxy)
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
PROXY="$REPO_ROOT/target/release/webauth-proxy"
PORT="${TERMMAP_WEB_PORT:-7681}"          # 公開ポート(webauth-proxy が持つ)
TTYD_PORT="${TERMMAP_WEB_TTYD_PORT:-17681}" # 内部ポート(ttyd。プロキシからのみ叩く)

# 認証情報は必ず環境変数から受け取る。既定値やスクリプト内の固定値は置かない
# (このまま外部トンネルへ晒される前提のため、資格情報をリポジトリに残さない)。
# webauth-proxy 側も未設定なら起動を拒否するが、ttyd を起こす前に止めたいのでここでも見る。
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
[ -x "$PROXY" ] || { echo "error: $PROXY が無い。先に cargo build --release" >&2; exit 1; }
[ -f "$INDEX" ] || { echo "error: $INDEX が無い。先に scripts/build-web-index.sh" >&2; exit 1; }

echo "termmap web: http://127.0.0.1:$PORT  (user: $TERMMAP_WEB_USER)"
echo "公開する場合は別ターミナルで: cloudflared tunnel --url http://127.0.0.1:$PORT"

# web/vendor/xterm-addon-image.js(build-web-index.shが埋め込む、本家 @xterm/addon-image)
# のおかげで、ブラウザ(xterm.js)側もiTerm2のインライン画像(OSC 1337)を描画できる。
# ttyd はこのスクリプトを起動したシェルの環境変数をそのまま子プロセスへ渡すため、
# TERM_PROGRAM を明示的に iTerm.app にしておく(起動元のターミナルが何であっても
# termmap 側の image_capable() が真になり、地図を実画像で描けるようにする)。
# LC_TERMINAL/ITERM_SESSION_ID は不要なので渡さない(TERM_PROGRAMだけで判定は満たせる)。
# 実際に実画像を使うかは cfg.image_mode(既定OFF・`I`キーか設定画面で切替)の方で決まる。
CHILD_ENV=(env -u LC_TERMINAL -u ITERM_SESSION_ID TERM_PROGRAM=iTerm.app)

# web 版の既定描画モードは braille にする(docs/web-pan-smoothness-design.md §5.3 C-2)。
# 1フレームの出力が halfblock の約3分の1(94×23 の実測で 24KB 対 75KB)で、地図が動ける
# 最小単位も半分(横1/2セル・縦1/4セル)。ブラウザで見えるコマ数は「処理できるバイト数 ÷
# 1フレームのバイト数」で決まる(同 §2.4)ので、バイトが安い braille は同じ回線・同じ端末で
# コマ数が増える。失うのは色の階調(前景色のみ・背景なし)だけ。
#
# halfblock で使いたいときは TERMMAP_WEB_BRAILLE=0 を付けて起動する。
#   TERMMAP_WEB_BRAILLE=0 scripts/serve-web.sh
#
# 注意: termmap は終了時に描画設定を ~/.config/termmap の config へ書き戻す(braille は
# CLI フラグと config の OR で決まる)。ここで --braille を渡すと、その後に手元のターミナルで
# termmap を起動したときも braille で始まる。戻したいときは termmap 上で B キー(または設定画面)
# で halfblock へ切り替えて終了する。
TERMMAP_ARGS=()
case "${TERMMAP_WEB_BRAILLE:-1}" in
  0|false|off|no) echo "描画モード: halfblock (TERMMAP_WEB_BRAILLE で無効化されている)" ;;
  *) TERMMAP_ARGS+=(--braille); echo "描画モード: braille (halfblock で使うなら TERMMAP_WEB_BRAILLE=0)" ;;
esac

TTYD_PID=""
PROXY_PID=""
# ttyd とプロキシの2プロセス構成なので、どちらが落ちても/どう止められても
# 片方だけ残らないようにまとめて片付ける。
cleanup() {
  trap - EXIT INT TERM
  for pid in "$PROXY_PID" "$TTYD_PID"; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
}
trap cleanup EXIT INT TERM

# ttyd は認証なし(-c 無し)。外から直接叩けない内部ポートに閉じ、
# 認証は前段の webauth-proxy が受け持つ。
# -W  書き込み可(既定は読み取り専用なので、これが無いと操作できない)
# -i  127.0.0.1 に限定(直接インターネットへ晒さない)
# -t  ブラウザ側 xterm.js のオプション
# TERMMAP_ARGS の展開が "${arr[@]+"${arr[@]}"}" の形なのは、macOS 標準の bash 3.2 が set -u 下で
# 空配列の "${arr[@]}" を unbound variable として落とすため(TERMMAP_WEB_BRAILLE=0 で空になる)。
"${CHILD_ENV[@]}" ttyd \
  -i 127.0.0.1 \
  -p "$TTYD_PORT" \
  -W \
  -I "$INDEX" \
  -t 'disableLeaveAlert=true' \
  "$BIN" "${TERMMAP_ARGS[@]+"${TERMMAP_ARGS[@]}"}" "$@" &
TTYD_PID=$!

sleep 0.3
kill -0 "$TTYD_PID" 2>/dev/null || { echo "error: ttyd が起動できなかった(ポート $TTYD_PORT を確認)" >&2; exit 1; }

# 公開ポートは webauth-proxy が持つ。
WEBAUTH_PROXY_PORT="$PORT" \
WEBAUTH_PROXY_UPSTREAM_PORT="$TTYD_PORT" \
  "$PROXY" &
PROXY_PID=$!

# 前面で待つのは sleep にしておく。bash は前面の子を待っている間シグナルを保留するため、
# プロキシを前面で待つと SIGTERM で trap が動かず ttyd が残ってしまう
# (Ctrl-C はプロセスグループ全体に届くので前面待ちでも止まるが、kill だと残る)。
while kill -0 "$PROXY_PID" 2>/dev/null && kill -0 "$TTYD_PID" 2>/dev/null; do
  sleep 1
done

# 先に落ちたのがプロキシなら、その終了ステータスをそのまま返す
# (ポート衝突などの起動失敗を成功扱いにしない)。ttyd が先に落ちた場合も異常終了。
if kill -0 "$PROXY_PID" 2>/dev/null; then
  echo "error: ttyd が終了した" >&2
  exit 1
fi
STATUS=0
wait "$PROXY_PID" 2>/dev/null || STATUS=$?
exit "$STATUS"
