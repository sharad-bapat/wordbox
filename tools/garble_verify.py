"""Check the constructed garble set independently of wordbox, with PyMuPDF.

- clean: same text as the original source file (the save changed nothing).
- pua / control: share of non-space characters in the Private Use Area / control range.
- shift: share of characters equal to the clean text moved one letter along.
- every variant: page 1 renders pixel-identical to the clean copy (ToUnicode doesn't affect drawing).

usage: python tools/garble_verify.py <garble dir> <contract-nli raw dir>
"""
import collections, json, os, sys, unicodedata

import fitz

sys.path.insert(0, os.path.dirname(__file__))
from garble import shift

gdir, raw = sys.argv[1], sys.argv[2]
man = json.load(open(os.path.join(gdir, 'manifest.json'), encoding='utf-8'))
text = lambda path: ''.join(p.get_text() for p in fitz.open(path))
chars = lambda s: [c for c in s if not c.isspace()]
res = collections.defaultdict(list)
pix_same = collections.Counter()
clean_of = {}
for it in man['items']:
    if it['kind'] == 'clean':
        clean_of[(it['split'], os.path.basename(it['file']))] = it['file']
for it in man['items']:
    t = chars(text(it['file']))
    clean = clean_of[(it['split'], os.path.basename(it['file']))]
    ct = chars(text(clean))
    if it['kind'] == 'clean':
        orig = chars(text(os.path.join(raw, it['source'])))
        res['clean'].append(sum((collections.Counter(t) & collections.Counter(orig)).values()) / max(len(orig), 1))
    elif it['kind'] == 'pua':
        res['pua'].append(sum(1 for c in t if 0xE000 <= ord(c) <= 0xF8FF) / max(len(t), 1))
    elif it['kind'] == 'control':
        res['control'].append(sum(1 for c in t if ord(c) < 32) / max(len(t), 1))
    elif it['kind'] == 'shift':
        want = chars(shift(''.join(ct)))
        res['shift'].append(sum((collections.Counter(t) & collections.Counter(want)).values()) / max(len(want), 1))
    a = fitz.open(it['file'])[0].get_pixmap(dpi=50).samples
    b = fitz.open(clean)[0].get_pixmap(dpi=50).samples
    pix_same[(it['kind'], a == b)] += 1

for k, v in res.items():
    v.sort()
    print(f'{k:8} files {len(v):3}  min {v[0]:.4f}  median {v[len(v) // 2]:.4f}')
print('page 1 identical to clean:', dict(pix_same))
