"""Chunk 2 check: does wordbox decode the same characters as PyMuPDF, page by page?

Order and spacing are ignored (they belong to the geometry chunk): each page's text is compared as
a multiset of non-whitespace characters after NFKC normalisation. Dev files only.

usage: python tools/decode_check.py <wordbox-cli> <list.txt> [--worst N]
"""
import collections, json, subprocess, sys, unicodedata

import fitz  # PyMuPDF

cli, listfile = sys.argv[1], sys.argv[2]
worst_n = int(sys.argv[sys.argv.index('--worst') + 1]) if '--worst' in sys.argv else 15


def bag(s):
    s = unicodedata.normalize('NFKC', s)
    return collections.Counter(c for c in s if not c.isspace() and c != '�')


out = subprocess.run([cli, '--text', '--list', listfile], capture_output=True, text=True, encoding='utf-8').stdout
ours = {}
for line in out.split('\n'):  # not splitlines(): extracted text may contain U+2028, U+0085...
    if not line.strip():
        continue
    j = json.loads(line)
    ours[j['file']] = j

tot = collections.Counter()
pages = []
files_status = collections.Counter()
for f, j in ours.items():
    files_status[j.get('status', 'error')] += 1
    if j.get('status') != 'ok':
        continue
    try:
        doc = fitz.open(f)
    except Exception:
        files_status['pymupdf_error'] += 1
        continue
    if doc.page_count != len(j['pages']):
        tot['page_count_mismatch'] += 1
    for p, page in zip(j['pages'], doc):
        a, b = bag(p['text']), bag(page.get_text('text'))
        m = sum((a & b).values())
        na, nb = sum(a.values()), sum(b.values())
        tot['ours'] += na; tot['theirs'] += nb; tot['matched'] += m; tot['unmapped'] += p['unmapped']
        tot['pages'] += 1
        if na == 0 and nb == 0:
            tot['both_empty'] += 1
            continue
        prec = m / na if na else 0.0
        rec = m / nb if nb else 0.0
        f1 = 2 * prec * rec / (prec + rec) if prec + rec else 0.0
        if f1 >= 0.99: tot['f1_99'] += 1
        pages.append((f1, f, p['n'], na, nb, p['unmapped'], (a - b).most_common(6), (b - a).most_common(6)))

print('files:', dict(files_status))
print(f"pages {tot['pages']}  both empty {tot['both_empty']}  page-count mismatches (files) {tot['page_count_mismatch']}")
scored = tot['pages'] - tot['both_empty']
print(f"pages with char F1 >= 0.99: {tot['f1_99']} of {scored} ({100 * tot['f1_99'] / max(scored, 1):.2f}%)")
print(f"chars: ours {tot['ours']}  pymupdf {tot['theirs']}  matched {tot['matched']}  "
      f"precision {tot['matched'] / max(tot['ours'], 1):.4f}  recall {tot['matched'] / max(tot['theirs'], 1):.4f}  unmapped glyphs {tot['unmapped']}")
print(f'\nworst {worst_n} pages:')
for f1, f, n, na, nb, un, extra, missing in sorted(pages)[:worst_n]:
    print(f'  F1 {f1:.3f}  {f.split("/")[-1][:60]} p{n}  ours {na} theirs {nb} unmapped {un}')
    print(f'      only ours: {extra}\n      only theirs: {missing}')
