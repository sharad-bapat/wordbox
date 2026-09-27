"""Summarise a reference file from tools/reference.py.

Per page, agreement = reference words / the larger of the two tools' word counts. Pages below 95%
are "disputed" and listed (a seeded sample) for checking by hand, with what each tool had alone.

usage: python tools/reference_report.py <reference.jsonl> [--sample N]
"""
import collections, json, random, sys

path = sys.argv[1]
n_sample = int(sys.argv[sys.argv.index('--sample') + 1]) if '--sample' in sys.argv else 12
tot = collections.Counter()
disputed = []
for line in open(path, encoding='utf-8'):
    j = json.loads(line)
    if 'error' in j:
        tot['file_errors'] += 1
        continue
    tot['files'] += 1
    for p in j['pages']:
        if 'error' in p:
            tot['page_errors'] += 1
            continue
        tot['pages'] += 1
        big = max(p['pymupdf'], p['pdfplumber'])
        ref = len(p['words'])
        tot['ref_words'] += ref
        tot['pymupdf_words'] += p['pymupdf']
        tot['pdfplumber_words'] += p['pdfplumber']
        tot['moved'] += p['moved']
        if big == 0:
            tot['empty_pages'] += 1
            continue
        agree = ref / big
        if agree >= 0.95: tot['pages_95'] += 1
        else: disputed.append((round(agree, 3), j['file'].split('/')[-1], p['n'], p['pymupdf'], p['pdfplumber'], ref, p['moved']))

scored = tot['pages'] - tot['empty_pages']
print(f"files {tot['files']} (errors {tot['file_errors']}), pages {tot['pages']} (errors {tot['page_errors']}, empty {tot['empty_pages']})")
print(f"reference words {tot['ref_words']}  (PyMuPDF {tot['pymupdf_words']}, pdfplumber {tot['pdfplumber_words']}, same text but placed apart {tot['moved']})")
print(f"word-level agreement: {100 * tot['ref_words'] / max(max(tot['pymupdf_words'], tot['pdfplumber_words']), 1):.2f}% of the larger tool's words")
print(f"pages with >= 95% agreement: {tot['pages_95']} of {scored} ({100 * tot['pages_95'] / max(scored, 1):.2f}%); disputed {len(disputed)}")
rnd = random.Random(20260927)
print(f'\nseeded sample of {n_sample} disputed pages (agreement, file, page, pymupdf, pdfplumber, reference, moved):')
for d in sorted(rnd.sample(disputed, min(n_sample, len(disputed)))):
    print('  ', d)
