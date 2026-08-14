#!/usr/bin/env bash
#
# web/index.html を生成する。
#
# ttyd の既定ページ(CSS/JSが全部インライン化された単一ファイル)を実際に動かして取得し、
#   - <head> に viewport 指定を挿入(ttyd 既定ページには入っておらず、無いと iPhone で
#     980px 幅として描画されて文字が極端に小さくなる)
#   - </body> の直前に web/vendor/xterm-addon-image.js(公式アドオン。ttyd同梱xterm.jsには
#     コード自体は入っているがttyd側がロードしていないので別途読み込む) → web/touch-overlay.js
#     の順で <script> として埋め込む(touch-overlay.js が window.ImageAddon を使うため順序が要る)
# という3箇所だけを足したものを web/index.html として書き出す。
#
# 既定ページを手で編集して育てるのではなく毎回ここから作り直すので、
# ttyd を更新したときはこのスクリプトを再実行するだけで追従できる。
#
# 使い方:  scripts/build-web-index.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OVERLAY="$REPO_ROOT/web/touch-overlay.js"
IMAGE_ADDON="$REPO_ROOT/web/vendor/xterm-addon-image.js"
OUT="$REPO_ROOT/web/index.html"

# 取得用の一時 ttyd が使うポート。本番用(7681)とぶつけないようにずらしてある。
PROBE_PORT="${TERMMAP_WEB_BUILD_PORT:-7699}"

command -v ttyd >/dev/null 2>&1 || { echo "error: ttyd が見つからない (brew install ttyd)" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "error: curl が見つからない" >&2; exit 1; }
[ -f "$OVERLAY" ] || { echo "error: $OVERLAY が無い" >&2; exit 1; }
[ -f "$IMAGE_ADDON" ] || { echo "error: $IMAGE_ADDON が無い" >&2; exit 1; }

TMPDIR_WORK="$(mktemp -d)"
BASE="$TMPDIR_WORK/base.html"
TTYD_PID=""

cleanup() {
  if [ -n "$TTYD_PID" ] && kill -0 "$TTYD_PID" 2>/dev/null; then
    kill "$TTYD_PID" 2>/dev/null || true
    wait "$TTYD_PID" 2>/dev/null || true
  fi
  rm -rf "$TMPDIR_WORK"
}
trap cleanup EXIT

echo "==> ttyd の既定ページを取得 (127.0.0.1:$PROBE_PORT)"
# 取得目的なので書き込み不可(-W を付けない)のまま起動する。コマンドは何でもよい。
ttyd -i 127.0.0.1 -p "$PROBE_PORT" /bin/cat >"$TMPDIR_WORK/ttyd.log" 2>&1 &
TTYD_PID=$!

ok=""
for _ in $(seq 1 60); do
  if [ "$(curl -s -o "$BASE" -w '%{http_code}' "http://127.0.0.1:$PROBE_PORT/" 2>/dev/null)" = "200" ]; then
    ok=1
    break
  fi
  sleep 0.25
done
[ -n "$ok" ] || { echo "error: ttyd から既定ページを取得できなかった" >&2; cat "$TMPDIR_WORK/ttyd.log" >&2; exit 1; }

# 想定した形(単一HTML・</body>がちょうど1つ・xterm入り)であることを確認してから加工する。
# ttyd 側の構成が変わったらここで気付けるようにしておく。
body_count="$(grep -c '</body>' "$BASE" || true)"
[ "$body_count" = "1" ] || { echo "error: </body> が $body_count 個。ttyd のページ構成が想定と違う" >&2; exit 1; }
grep -q 'xterm-helper-textarea' "$BASE" || { echo "error: xterm-helper-textarea が見つからない。ttyd の同梱 xterm.js が想定と違う" >&2; exit 1; }
grep -q 'id="terminal-container"\|"terminal-container"' "$BASE" || { echo "error: terminal-container が見つからない" >&2; exit 1; }

echo "==> viewport とオーバーレイを埋め込み"
VIEWPORT='<meta name="viewport" content="width=device-width, initial-scale=1, maximum-scale=1, viewport-fit=cover">'

# python3 で組み立てる。sed だと 730KB の1行 HTML と JS 内の記号のエスケープが煩雑になるため。
OUT="$OUT" BASE="$BASE" OVERLAY="$OVERLAY" IMAGE_ADDON="$IMAGE_ADDON" VIEWPORT="$VIEWPORT" python3 - <<'PY'
import os, sys

base        = open(os.environ['BASE'],        encoding='utf-8').read()
overlay     = open(os.environ['OVERLAY'],      encoding='utf-8').read()
image_addon = open(os.environ['IMAGE_ADDON'],  encoding='utf-8').read()
viewport    = os.environ['VIEWPORT']

# 埋め込む JS の中に </script> が現れると <script> が途中で閉じてしまう。
# 現状は含まれないが、将来ファイルに文字列として入っても壊れないようにしておく。
overlay = overlay.replace('</script>', '<\\/script>')
image_addon = image_addon.replace('</script>', '<\\/script>')

if 'name="viewport"' not in base:
    marker = '<meta charset="UTF-8">'
    if marker not in base:
        sys.exit('error: <meta charset="UTF-8"> が見つからず viewport を挿入できない')
    base = base.replace(marker, marker + viewport, 1)

# xterm-addon-image は touch-overlay.js より前に置く(touch-overlay.js が
# window.ImageAddon を参照するため読み込み順序が要る)。
block = (
    '<script id="termmap-xterm-addon-image">\n' + image_addon + '\n</script>\n'
    '<script id="termmap-touch-overlay">\n' + overlay + '\n</script>'
)
if base.count('</body>') != 1:
    sys.exit('error: </body> がちょうど1つではない')
base = base.replace('</body>', block + '</body>', 1)

with open(os.environ['OUT'], 'w', encoding='utf-8') as f:
    f.write(base)
PY

echo "==> 生成完了: $OUT ($(wc -c <"$OUT" | tr -d ' ') bytes)"
grep -q 'termmap-touch-overlay' "$OUT"      || { echo "error: overlay埋め込みの検証に失敗" >&2; exit 1; }
grep -q 'termmap-xterm-addon-image' "$OUT"  || { echo "error: addon-image埋め込みの検証に失敗" >&2; exit 1; }
grep -q 'name="viewport"' "$OUT"            || { echo "error: viewport の挿入に失敗" >&2; exit 1; }
echo "    起動は scripts/serve-web.sh"
