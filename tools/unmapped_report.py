"""Where do unmapped glyphs come from, and does PyMuPDF make real text of them?

For each page with unmapped glyphs, PyMuPDF's text for the page is called "readable" when at least 90%
of its non-space characters are letters, digits or common punctuation. Unmapped glyphs are then
grouped by font category. Dev files only.

usage: python tools/unmapped_report.py <wordbox-cli> <list.txt>
"""
import collections, json, subprocess, sys, unicodedata

import fitz

cli, listfile = sys.argv[1], sys.argv[2]
out = subprocess.run([cli, '--text', '--list', listfile], capture_output=True, text=True, encoding='utf-8').stdout


def readable(s):
    cs = [c for c in s if not c.isspace()]
    if len(cs) < 20:
        return None
    ok = sum(1 for c in cs if unicodedata.category(c)[0] in 'LNP' or c in '$%&+<=>|~^`')
    return ok / len(cs) >= 0.9


groups = collections.Counter()
files = collections.defaultdict(set)
for line in out.split('\n'):
    if not line.strip():
        continue
    j = json.loads(line)
    if j.get('status') != 'ok':
        continue
    doc = None
    for p in j['pages']:
        if not p['unmapped']:
            continue
        doc = doc or fitz.open(j['file'])
        r = readable(doc[p['n'] - 1].get_text('text'))
        tag = {True: 'pymupdf readable', False: 'pymupdf garbage', None: 'pymupdf little text'}[r]
        for fi, c in p['unmapped_by_font']:
            f = j['fonts'][fi] if fi >= 0 else {'kind': 'no font', 'encoding': '', 'embedded': False, 'to_unicode': False}
            enc = f['encoding'].replace('+Differences', '').split(' (')[0]
            key = (tag, f['kind'], enc, 'embedded' if f['embedded'] else 'not embedded', 'ToUnicode' if f['to_unicode'] else 'no ToUnicode')
            groups[key] += c
            files[key].add(j['file'].split('/')[-1])

total = sum(groups.values())
print(f'unmapped glyphs: {total}')
for key, c in groups.most_common(25):
    print(f'{c:>8}  {100 * c / total:5.1f}%  {" | ".join(key)}  ({len(files[key])} files, e.g. {sorted(files[key])[:3]})')
