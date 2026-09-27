"""Chunk 3 check: do wordbox's words agree with PyMuPDF's?

A word matches when the other tool has a word with the same text whose box centre is within 2 points.
Reported both ways: the share of our words PyMuPDF also has, and the share of PyMuPDF's words we have.
Only page content on the visible page (no annotation, off-page or unmapped words). Dev files only.

usage: python tools/word_check.py <wordbox-cli> <list.txt> [--every N]
"""
import collections, json, os, subprocess, sys, tempfile, unicodedata

import fitz

cli, listfile = sys.argv[1], sys.argv[2]
every = int(sys.argv[sys.argv.index('--every') + 1]) if '--every' in sys.argv else 1
files = [l.strip() for l in open(listfile, encoding='utf-8') if l.strip()][::every]
tmp = tempfile.NamedTemporaryFile('w', delete=False, suffix='.txt', encoding='utf-8')
tmp.write('\n'.join(files)); tmp.close()
out = subprocess.run([cli, '--list', tmp.name], capture_output=True, text=True, encoding='utf-8').stdout
os.unlink(tmp.name)

norm = lambda t: unicodedata.normalize('NFKC', t)
tot = collections.Counter()
split_ex, join_ex = collections.Counter(), collections.Counter()


def index(words):
    g = collections.defaultdict(list)
    for t, cx, cy in words:
        g[(int(cx // 4), int(cy // 4))].append((t, cx, cy))
    return g


def has(grid, t, cx, cy):
    for dx in (-1, 0, 1):
        for dy in (-1, 0, 1):
            for u, x, y in grid.get((int(cx // 4) + dx, int(cy // 4) + dy), []):
                if u == t and abs(x - cx) <= 2 and abs(y - cy) <= 2:
                    return True
    return False


for line in out.split('\n'):
    if not line.strip():
        continue
    j = json.loads(line)
    if j.get('status') != 'ok':
        continue
    doc = fitz.open(j['file'])
    for p, pg in zip(j['pages'], doc):
        # pages whose text layer we report as largely unmapped are the garbled-verdict case: skip them
        if p['glyph_count'] and p['unmapped'] > 0.3 * p['glyph_count']:
            tot['skipped_pages'] += 1
            continue
        ours = [(norm(w['t']), (w['x0'] + w['x1']) / 2, (w['y0'] + w['y1']) / 2) for w in p['words']
                if not (w.get('annot') or w.get('offpage') or w.get('unmapped'))]
        rm = pg.rotation_matrix
        theirs = []
        for x0, y0, x1, y1, t, *_ in pg.get_text('words'):
            r = fitz.Rect(x0, y0, x1, y1) * rm
            # PyMuPDF's rendering of undecodable codes: control characters (C0, C1) or U+FFFD
            if '�' in t or any(ord(c) < 32 or 127 <= ord(c) < 160 for c in t):
                continue
            theirs.append((norm(t), (r.x0 + r.x1) / 2, (r.y0 + r.y1) / 2))
        go, gt = index(ours), index(theirs)
        for t, cx, cy in ours:
            tot['ours'] += 1
            if has(gt, t, cx, cy): tot['ours_matched'] += 1
            else: join_ex[t] += 1
        for t, cx, cy in theirs:
            tot['theirs'] += 1
            if has(go, t, cx, cy): tot['theirs_matched'] += 1
            else: split_ex[t] += 1

print(f"our words: {tot['ours']}, also in PyMuPDF: {100 * tot['ours_matched'] / max(tot['ours'], 1):.2f}%")
print(f"PyMuPDF words: {tot['theirs']}, also in ours: {100 * tot['theirs_matched'] / max(tot['theirs'], 1):.2f}%")
print(f"pages skipped as mostly unmapped: {tot['skipped_pages']}")
print('ours only, most common:', join_ex.most_common(20))
print('theirs only, most common:', split_ex.most_common(20))
