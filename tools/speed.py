"""Speed and size: wordbox against PyMuPDF, pdfplumber and pdf.js, on the same files, in process.

Each tool starts from the file's bytes in memory and extracts words (text items for pdf.js) from
every page. Run it with nothing else busy on the machine.

usage: python tools/speed.py <wordbox-cli> <list.txt> <pdfjs results.jsonl>
  (make the pdf.js results first: node bench/pdfjs_speed.mjs <list.txt> > results.jsonl)
"""
import io, json, os, statistics, subprocess, sys, time, warnings

import fitz
import pdfplumber

warnings.filterwarnings('ignore')
cli, listfile, pdfjs_path = sys.argv[1:4]
files = [l.strip() for l in open(listfile, encoding='utf-8') if l.strip()]
ms = {k: {} for k in ('wordbox', 'PyMuPDF', 'pdfplumber', 'pdf.js')}
pages = {}

out = subprocess.run([cli, '--list', listfile], capture_output=True, text=True, encoding='utf-8').stdout
for l in out.split('\n'):
    if l.strip():
        j = json.loads(l)
        if j.get('status') == 'ok':
            ms['wordbox'][j['file']] = j['micros'] / 1000
            pages[j['file']] = len(j['pages'])

for f in files:
    data = open(f, 'rb').read()
    t = time.perf_counter()
    try:
        d = fitz.open(stream=data, filetype='pdf')
        for p in d: p.get_text('words')
        ms['PyMuPDF'][f] = (time.perf_counter() - t) * 1000
    except Exception:
        pass
    t = time.perf_counter()
    try:
        with pdfplumber.open(io.BytesIO(data)) as pdf:
            for p in pdf.pages:
                p.extract_words()
                p.flush_cache()
        ms['pdfplumber'][f] = (time.perf_counter() - t) * 1000
    except Exception:
        pass

for l in open(pdfjs_path, encoding='utf-8'):
    j = json.loads(l)
    if not j.get('error'):
        ms['pdf.js'][j['file']] = j['ms']

common = sorted(set.intersection(*(set(v) for v in ms.values())) & set(pages))
n_pages = sum(pages[f] for f in common)
print(f'files timed by all four tools: {len(common)} of {len(files)} ({n_pages} pages)')
print(f'{"tool":<11} {"median ms/file":>15} {"p95":>9} {"total s":>9} {"ms/page":>9}')
for k, v in ms.items():
    xs = sorted(v[f] for f in common)
    print(f'{k:<11} {statistics.median(xs):>15.2f} {xs[int(len(xs) * 0.95)]:>9.1f} {sum(xs) / 1000:>9.1f} {sum(xs) / n_pages:>9.2f}')


def dir_size(path):
    return sum(os.path.getsize(os.path.join(r, f)) for r, _, fs in os.walk(path) for f in fs)


here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
pj = os.path.join(here, 'bench', 'node_modules', 'pdfjs-dist', 'build')
print('\nsizes:')
print(f'  wordbox CLI (native, release): {os.path.getsize(cli) / 1024:.0f} KB')
print(f'  pdf.js 6.3 pdf.min.mjs + pdf.worker.min.mjs: {(os.path.getsize(os.path.join(pj, "pdf.min.mjs")) + os.path.getsize(os.path.join(pj, "pdf.worker.min.mjs"))) / 1024:.0f} KB')
import pymupdf  # fitz is a thin alias; the binaries live in the pymupdf package
print(f'  PyMuPDF package: {dir_size(os.path.dirname(pymupdf.__file__)) / 1048576:.1f} MB')
import pdfminer
print(f'  pdfplumber + pdfminer.six packages: {(dir_size(os.path.dirname(pdfplumber.__file__)) + dir_size(os.path.dirname(pdfminer.__file__))) / 1048576:.1f} MB (plus their dependencies)')
