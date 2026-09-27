import collections, json, subprocess, sys, unicodedata, fitz
cli, lst = sys.argv[1], sys.argv[2]
out = subprocess.run([cli, '--text', '--list', lst], capture_output=True, text=True, encoding='utf-8').stdout
def bag(s):
    s = unicodedata.normalize('NFKC', s); return collections.Counter(c for c in s if not c.isspace() and c != '\ufffd')
cat = collections.Counter(); ex = collections.defaultdict(list)
for line in out.split('\n'):
    if not line.strip(): continue
    j = json.loads(line)
    if j.get('status') != 'ok': continue
    d = fitz.open(j['file'])
    for p, pg in zip(j['pages'], d):
        a, b = bag(p['text']), bag(pg.get_text('text'))
        na, nb = sum(a.values()), sum(b.values()); m = sum((a & b).values())
        if na == nb == 0: continue
        pr, rc = (m / na if na else 0), (m / nb if nb else 0)
        f1 = 2 * pr * rc / (pr + rc) if pr + rc else 0
        if f1 >= 0.99: continue
        cb, mb = pg.cropbox, pg.mediabox
        if p['glyphs'] and p['unmapped'] > 0.3 * p['glyphs']: k = 'mostly unmapped (verdict case)'
        elif (cb.width < mb.width - 1 or cb.height < mb.height - 1) and pr < rc: k = 'cropbox smaller than mediabox, we have extra'
        elif pr >= 0.99 and rc < 0.99: k = 'we miss text'
        elif rc >= 0.99 and pr < 0.99: k = 'we have extra text'
        else: k = 'both differ'
        cat[k] += 1
        ex[k].append((round(f1, 3), j['file'].split('/')[-1][:50], p['n'], na, nb, [(c, n) for c, n in (a - b).most_common(4)], [(c, n) for c, n in (b - a).most_common(4)]))
for k, n in cat.most_common():
    print(f'{n:5}  {k}')
    for e in sorted(ex[k])[:6]: print('       ', e)
