"""On words the two reference tools disagree about, which one does wordbox agree with?

For each page: PyMuPDF's words and pdfplumber's words, as in tools/reference.py. A word only one
tool has (no same-text word from the other tool at the same place) is "disputed". For each disputed
word, check whether wordbox has a word with the same text at the same place (the same matching rule).

usage: python tools/disputes.py <wordbox-cli> <list.txt> [--every N]
"""
import collections, json, os, subprocess, sys, tempfile, warnings

import fitz
import pdfplumber

sys.path.insert(0, os.path.dirname(__file__))
import reference as R

warnings.filterwarnings('ignore')
cli, listfile = sys.argv[1], sys.argv[2]
every = int(sys.argv[sys.argv.index('--every') + 1]) if '--every' in sys.argv else 1
files = [l.strip() for l in open(listfile, encoding='utf-8') if l.strip()][::every]
tmp = tempfile.NamedTemporaryFile('w', delete=False, suffix='.txt', encoding='utf-8')
tmp.write('\n'.join(files)); tmp.close()
out = subprocess.run([cli, '--list', tmp.name], capture_output=True, text=True, encoding='utf-8').stdout
os.unlink(tmp.name)
ours = {j['file']: j for j in (json.loads(l) for l in out.split('\n') if l.strip())}


def same(w, v):
    cx, vx = (w['x0'] + w['x1']) / 2, (v['x0'] + v['x1']) / 2
    return v['t'] == w['t'] and abs(cx - vx) <= R.NEAR and R.v_overlap(w, v)


def has(ws, w):
    return any(same(w, v) for v in ws)


t = collections.Counter()
for f in files:
    o = ours.get(f)
    if not o or o.get('status') != 'ok':
        continue
    doc = fitz.open(f)
    with pdfplumber.open(f) as pdf:
        for i, pg in enumerate(doc):
            if i >= len(pdf.pages) or i >= len(o['pages']):
                continue
            if o['pages'][i]['verdict'] != 'text':
                continue
            media, crop, rot = R.boxes(pg)
            a, b = R.pymupdf_words(pg), R.plumber_words(pdf.pages[i], media, crop, rot)
            mine = [{'t': R.norm(w['t']), 'x0': w['x0'], 'x1': w['x1'], 'y0': w['y0'], 'y1': w['y1']}
                    for w in o['pages'][i]['words'] if not (w.get('annot') or w.get('offpage'))]
            by_text = collections.defaultdict(list)
            for w in mine: by_text[w['t']].append(w)
            bt = collections.defaultdict(list)
            for v in b: bt[v['t']].append(v)
            at = collections.defaultdict(list)
            for w in a: at[w['t']].append(w)
            for w in a:
                if not has(bt[w['t']], w):
                    t['only_pymupdf'] += 1
                    t['only_pymupdf_ours'] += has(by_text[w['t']], w)
            for v in b:
                if not has(at[v['t']], v):
                    t['only_pdfplumber'] += 1
                    t['only_pdfplumber_ours'] += has(by_text[v['t']], v)
    t['files'] += 1

print(f"files {t['files']} (every {every}th), pages with verdict text only")
print(f"words only PyMuPDF has: {t['only_pymupdf']}; wordbox has {t['only_pymupdf_ours']} of them ({100 * t['only_pymupdf_ours'] / max(t['only_pymupdf'], 1):.1f}%)")
print(f"words only pdfplumber has: {t['only_pdfplumber']}; wordbox has {t['only_pdfplumber_ours']} of them ({100 * t['only_pdfplumber_ours'] / max(t['only_pdfplumber'], 1):.1f}%)")
