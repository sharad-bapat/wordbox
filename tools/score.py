"""Score wordbox: words against the reference, and the page verdict on the garble set and real files.

Words: a reference word counts as found when wordbox has a word with the same NFKC text, horizontal
centre within 2 pt, and a box overlapping it vertically by at least half the shorter box. For found
words: x0, x1 and baseline within 1 pt. Annotation and off-page words are left out (the reference
tools don't read annotations the same way).

usage: python tools/score.py <wordbox-cli> <reference.jsonl> <garble manifest> <split: dev|heldout>
"""
import collections, json, os, subprocess, sys, tempfile, unicodedata

cli, ref_path, manifest, split = sys.argv[1:5]
norm = lambda t: unicodedata.normalize('NFKC', t)


def run(files, *flags):
    tmp = tempfile.NamedTemporaryFile('w', delete=False, suffix='.txt', encoding='utf-8')
    tmp.write('\n'.join(files)); tmp.close()
    out = subprocess.run([cli, *flags, '--list', tmp.name], capture_output=True, text=True, encoding='utf-8').stdout
    os.unlink(tmp.name)
    return {j['file']: j for j in (json.loads(l) for l in out.split('\n') if l.strip())}


# ---- words against the reference
refs = [json.loads(l) for l in open(ref_path, encoding='utf-8')]
ours = run([r['file'] for r in refs])
t = collections.Counter()
pages_found = []
for r in refs:
    o = ours.get(r['file'])
    if not o or o.get('status') != 'ok':
        t['files_failed'] += 1
        continue
    for rp in r['pages']:
        if 'error' in rp or not rp['words'] or rp['n'] > len(o['pages']):
            continue
        op = o['pages'][rp['n'] - 1]
        grid = collections.defaultdict(list)
        for w in op['words']:
            if w.get('annot') or w.get('offpage'):
                continue
            cx = (w['x0'] + w['x1']) / 2
            grid[(norm(w['t']), int(cx // 4))].append(w)
        found = 0
        for rw in rp['words']:
            t['ref'] += 1
            cx = (rw['x0'] + rw['x1']) / 2
            hit = None
            for k in (-1, 0, 1):
                for w in grid.get((rw['t'], int(cx // 4) + k), []):
                    ov = min(w['y1'], rw['y1']) - max(w['y0'], rw['y0'])
                    if abs((w['x0'] + w['x1']) / 2 - cx) <= 2 and ov >= 0.5 * max(min(w['y1'] - w['y0'], rw['y1'] - rw['y0']), 0.1):
                        hit = w; break
                if hit: break
            if hit:
                found += 1
                t['found'] += 1
                t['x0_1pt'] += abs(hit['x0'] - rw['x0']) <= 1
                t['x1_1pt'] += abs(hit['x1'] - rw['x1']) <= 1
                t['b_1pt'] += abs(hit['b'] - rw['b']) <= 1
        pages_found.append(found / len(rp['words']))

print('== words against the reference')
print(f"reference words {t['ref']}, found {t['found']} ({100 * t['found'] / max(t['ref'], 1):.2f}%)")
print(f"of found: x0 within 1 pt {100 * t['x0_1pt'] / max(t['found'], 1):.2f}%, x1 {100 * t['x1_1pt'] / max(t['found'], 1):.2f}%, baseline {100 * t['b_1pt'] / max(t['found'], 1):.2f}%")
print(f"pages with >= 99% of reference words found: {100 * sum(1 for x in pages_found if x >= 0.99) / max(len(pages_found), 1):.2f}% of {len(pages_found)}")

# ---- verdict on the constructed garble set
items = [i for i in json.load(open(manifest, encoding='utf-8'))['items'] if i['split'] == split]
g = run([i['file'] for i in items])
conf = collections.Counter()
for it in items:
    o = g[it['file']]
    for want, p in zip(it['pages'], o['pages']):
        conf[(it['kind'], want, p['verdict'])] += 1
print(f'\n== verdict on the {split} garble set (kind, expected, got): pages')
for k in sorted(conf): print(f'   {k}: {conf[k]}')
by_kind = collections.defaultdict(lambda: [0, 0])
for (kind, want, got), n in conf.items():
    if want == 'none': continue
    by_kind[kind][0] += n
    by_kind[kind][1] += n if got == want else 0
for kind, (n, ok) in sorted(by_kind.items()):
    print(f'   {kind:8} {ok}/{n} pages with the expected verdict ({100 * ok / max(n, 1):.1f}%)')

# ---- verdict on the real dev files
v = collections.Counter()
flagged = collections.Counter()
for f, o in ours.items():
    for p in o.get('pages', []):
        v[p['verdict']] += 1
        if p['verdict'] in ('garbled', 'invisible'):
            flagged[(f.split('/')[-1], p['verdict'])] += 1
print('\n== verdict on the real files:', dict(v))
print('   files with garbled or invisible pages:', sorted(flagged.items()))
