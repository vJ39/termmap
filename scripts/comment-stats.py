#!/usr/bin/env python3
"""コメント内のタスクID・タスク名タグ・日付・5行以上の連続コメント塊を数える。docs/comment-cleanup-design.md §3。

使い方: scripts/comment-stats.py [-v] [ファイル...]  (ファイル省略時は src/・build.rs・web/*.js 全部)
-v で該当行を表示。M/D形式(7/12等)は分数と区別できないので md_cand として別に数える(要目視)。
行頭 // の連続だけを塊として数えるので、JS の /* */ 塊は対象外。"""
import re, glob, sys
args = [a for a in sys.argv[1:] if a != '-v']
verbose = '-v' in sys.argv
files = args or sorted(glob.glob('src/*.rs') + glob.glob('src/bin/*.rs') + ['build.rs'] + glob.glob('web/*.js'))
TID = re.compile(r'(?<![&0-9A-Za-z_])#\d{1,5}(?![\dA-Fa-f])')
TAG = re.compile(r'#[぀-ヿ一-鿿]')
DATE = re.compile(r'(?<!\d)20\d\d[-/]\d\d?[-/]\d\d?(?!\d)|20\d\d年\d\d?月(?:\d\d?日)?')
MD = re.compile(r'(?<![\d/.])(1[0-2]|0?[1-9])/(3[01]|[12]\d|0?[1-9])(?![\d/.])')
tot = [0, 0, 0, 0, 0]
for f in files:
    lines = open(f, encoding='utf-8').read().split('\n')
    c = [0, 0, 0, 0, 0]
    run = 0
    shown = []
    for no, l in enumerate(lines, 1):
        s = l.strip()
        m = re.search(r'(?<![:"\'])//', l)
        cpart = l[m.start():] if m else ''
        k = [len(TID.findall(cpart)), len(TAG.findall(cpart)), len(DATE.findall(cpart)), len(MD.findall(cpart))]
        for i in range(4): c[i] += k[i]
        if any(k[:3]) or (verbose and k[3]):
            shown.append(f"  {no}: {cpart.strip()[:110]}")
        if s.startswith('//'):
            run += 1
        else:
            if run >= 5: c[4] += 1
            run = 0
    if run >= 5: c[4] += 1
    for i in range(5): tot[i] += c[i]
    if any(c[:3]) or c[4] or (verbose and c[3]):
        print(f"{f}: taskid={c[0]} tag={c[1]} date={c[2]} md_cand={c[3]} long_blocks={c[4]}")
        if verbose: print('\n'.join(shown))
print(f"TOTAL taskid={tot[0]} tag={tot[1]} date={tot[2]} md_cand={tot[3]} long_blocks={tot[4]}")
