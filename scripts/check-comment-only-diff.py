#!/usr/bin/env python3
"""変更がコメントだけかを確かめる。docs/comment-cleanup-design.md §3。

使い方: scripts/check-comment-only-diff.py [<git rev>] [ファイル...]
rev(既定 HEAD)の内容と作業ツリーの内容からコメントを除き、トークン列が一致しなければ失敗する。
ファイル省略時は rev から変更のある .rs/.js 全部。
"""
import re
import subprocess
import sys


def strip_comments(src: str, rust: bool) -> str:
    out = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        two = src[i:i + 2]
        if two == "//":
            j = src.find("\n", i)
            i = n if j < 0 else j
            continue
        if two == "/*":
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i) and rust:
                    depth, i = depth + 1, i + 2
                elif src.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            out.append(" ")
            continue
        if rust:
            m = re.match(r'b?r(#*)"', src[i:])
            if m and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
                end = src.find('"' + m.group(1), i + len(m.group(0)))
                end = n if end < 0 else end + 1 + len(m.group(1))
                out.append(src[i:end])
                i = end
                continue
            if c == "'":
                m = re.match(r"'(\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'\n])'", src[i:])
                if m:
                    out.append(m.group(0))
                    i += len(m.group(0))
                    continue
        if c == '"' or (not rust and c in "'`"):
            q, j = c, i + 1
            while j < n and src[j] != q:
                j += 2 if src[j] == "\\" else 1
            out.append(src[i:j + 1])
            i = j + 1
            continue
        if not rust and c == "/" and re.match(r"/(?![/*])(\\.|\[[^\]\n]*\]|[^/\n\\])+/[gimsuy]*", src[i:]):
            prev = "".join(out).rstrip()[-1:]
            if prev in "(,=:[!&|?{};" or prev == "":
                m = re.match(r"/(\\.|\[[^\]\n]*\]|[^/\n\\])+/[gimsuy]*", src[i:])
                out.append(m.group(0))
                i += len(m.group(0))
                continue
        out.append(c)
        i += 1
    return "".join(out)


def tokens(src: str, rust: bool):
    return strip_comments(src, rust).split()


def main() -> int:
    args = sys.argv[1:]
    rev = "HEAD"
    if args and not args[0].endswith((".rs", ".js")):
        rev, args = args[0], args[1:]
    files = args or [f for f in subprocess.run(["git", "diff", "--name-only", rev], capture_output=True,
                                              text=True, check=True).stdout.split() if f.endswith((".rs", ".js"))]
    bad = 0
    for f in files:
        before = subprocess.run(["git", "show", f"{rev}:{f}"], capture_output=True, text=True).stdout
        after = open(f, encoding="utf-8").read()
        rust = f.endswith(".rs")
        a, b = tokens(before, rust), tokens(after, rust)
        if a != b:
            bad += 1
            k = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
            print(f"NG {f}: トークン{k}番目から不一致 {a[k:k + 6]} -> {b[k:k + 6]}")
        else:
            print(f"ok {f}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
