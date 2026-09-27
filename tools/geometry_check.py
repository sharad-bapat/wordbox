"""Chunk 3 check: are wordbox's glyph positions the same as PyMuPDF's?

For every glyph wordbox places on the visible page (mapped, not whitespace), look for a PyMuPDF glyph
with the same character whose baseline start is within 2 points. For matched pairs, report the
differences in x0, x1 and baseline. Annotation glyphs and rotated pages are counted separately.
Dev files only.

usage: python tools/geometry_check.py <wordbox-cli> <list.txt> [--every N]
"""
import collections, json, statistics, subprocess, sys, tempfile, os

import fitz

cli, listfile = sys.argv[1], sys.argv[2]
every = int(sys.argv[sys.argv.index('--every') + 1]) if '--every' in sys.argv else 1
files = [l.strip() for l in open(listfile, encoding='utf-8') if l.strip()][::every]
tmp = tempfile.NamedTemporaryFile('w', delete=False, suffix='.txt', encoding='utf-8')
tmp.write('\n'.join(files)); tmp.close()
out = subprocess.run([cli, '--glyphs', '--list', tmp.name], capture_output=True, text=True, encoding='utf-8').stdout
os.unlink(tmp.name)

stats = {k: collections.Counter() for k in ('page', 'annot', 'rotated')}
diffs = {k: collections.defaultdict(list) for k in stats}
examples = collections.defaultdict(list)
for line in out.split('\n'):
    if not line.strip():
        continue
    j = json.loads(line)
    if j.get('status') != 'ok':
        continue
    doc = fitz.open(j['file'])
    for p, pg in zip(j['pages'], doc):
        # PyMuPDF reports text in the unrotated page; turn it to the page as displayed, as wordbox reports it
        rm = pg.rotation_matrix
        grid = collections.defaultdict(list)
        for b in pg.get_text('rawdict')['blocks']:
            for l in b.get('lines', []):
                for s in l['spans']:
                    for c in s['chars']:
                        if c['c'].strip():
                            o = fitz.Point(c['origin']) * rm
                            r = fitz.Rect(c['bbox']) * rm
                            c = {'c': c['c'], 'origin': (o.x, o.y), 'bbox': (r.x0, r.y0, r.x1, r.y1)}
                            grid[(int(o.x // 2), int(o.y // 2))].append(c)
        for g in p['glyphs']:
            if g.get('unmapped') or g.get('offpage') or not g['c'].strip():
                continue
            kind = 'annot' if g.get('annot') else ('rotated' if p['rotate'] else 'page')
            stats[kind]['ours'] += 1
            gx, gy = int(g['ox'] // 2), int(g['oy'] // 2)
            best = None
            for dx in (-1, 0, 1):
                for dy in (-1, 0, 1):
                    for c in grid.get((gx + dx, gy + dy), []):
                        if c['c'] != g['c']:
                            continue
                        d = abs(c['origin'][0] - g['ox']) + abs(c['origin'][1] - g['oy'])
                        if d <= 2 and (best is None or d < best[0]):
                            best = (d, c)
            if best is None:
                stats[kind]['unmatched'] += 1
                if len(examples[kind]) < 6:
                    examples[kind].append((j['file'].split('/')[-1][:40], p['n'], g['c'], g['ox'], g['oy']))
                continue
            c = best[1]
            stats[kind]['matched'] += 1
            diffs[kind]['x0'].append(abs(c['bbox'][0] - g['x0']))
            diffs[kind]['x1'].append(abs(c['bbox'][2] - g['x1']))
            diffs[kind]['baseline'].append(abs(c['origin'][1] - g['oy']))

for kind in stats:
    s = stats[kind]
    if not s['ours']:
        print(f'{kind}: no glyphs'); continue
    print(f"{kind}: {s['ours']} glyphs, matched {s['matched']} ({100 * s['matched'] / s['ours']:.2f}%)")
    for k, v in diffs[kind].items():
        v.sort()
        within = lambda t: 100 * sum(1 for x in v if x <= t) / len(v)
        print(f'   {k:8} median {statistics.median(v):.3f}  p99 {v[int(len(v) * 0.99)]:.2f}  within 0.5pt {within(0.5):.2f}%  within 1pt {within(1):.2f}%')
    if examples[kind]:
        print('   unmatched e.g.', examples[kind])
