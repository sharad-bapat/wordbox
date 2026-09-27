"""Build the reference: words where PyMuPDF and pdfplumber agree.

For each page, both tools' words are put in the frame wordbox uses: points from the top-left of the
page as displayed (CropBox, then /Rotate). A word is in the reference when both tools have it with
the same text (NFKC), their x0 and x1 agree within 1 point and their boxes overlap vertically by
at least half the shorter box (the tools size boxes by different ascent conventions); x0 and x1 are the mean of the two, and the baseline and box height come from PyMuPDF
(pdfplumber rotates its character matrices on rotated pages, so rebuilding its baseline is fragile). Everything else is counted, not used: words only one tool has ("only_pymupdf",
"only_pdfplumber") and words both have but place differently ("moved").

usage: python tools/reference.py <list.txt> <out.jsonl> [--jobs N]
Writes one JSON line per file: {"file", "pages": [{"n", "words": [...], counts...}]}.
"""
import json, multiprocessing, sys, unicodedata, warnings

import fitz
import pdfplumber

warnings.filterwarnings('ignore')
TOL = 1.0      # points: x0, x1 and baseline must agree this closely
NEAR = 2.0     # points: word centres this close count as the same place
norm = lambda t: unicodedata.normalize('NFKC', t)


def display_map(box, rot):
    """User space -> displayed page for a box (x0, y0, x1, y1) and rotation, as wordbox does it."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    def f(x, y):
        u, v = x - x0, y1 - y
        return {90: (h - v, u), 180: (w - u, h - v), 270: (v, w - u)}.get(rot, (u, v))
    return f


def display_unmap(box, rot):
    """Displayed page -> user space (the inverse of display_map)."""
    x0, y0, x1, y1 = box
    w, h = x1 - x0, y1 - y0
    def f(a, b):
        u, v = {90: (b, h - a), 180: (w - a, h - b), 270: (w - b, a)}.get(rot, (a, b))
        return (u + x0, y1 - v)
    return f


def boxes(pg):
    """Unrotated MediaBox and visible CropBox in PDF user space (y up), and the rotation.
    PyMuPDF gives page.mediabox in raw PDF coordinates, but page.cropbox flipped (y down from the
    MediaBox top)."""
    mb = pg.mediabox
    media = (mb.x0, mb.y0, mb.x1, mb.y1)
    cb = pg.cropbox
    crop = (cb.x0, mb.y1 - cb.y1, cb.x1, mb.y1 - cb.y0)
    return media, crop, pg.rotation


def pymupdf_words(pg):
    rm = pg.rotation_matrix
    chars = []
    for b in pg.get_text('rawdict')['blocks']:
        for l in b.get('lines', []):
            for s in l['spans']:
                for c in s['chars']:
                    if c['c'].strip():
                        o = fitz.Point(c['origin']) * rm
                        r = fitz.Rect(c['bbox']) * rm
                        chars.append((r.x0, r.y0, r.x1, r.y1, o.y))
    out = []
    for x0, y0, x1, y1, t, *_ in pg.get_text('words'):
        r = fitz.Rect(x0, y0, x1, y1) * rm
        # baseline: the first character inside the word's box, leftmost
        inside = [c for c in chars if c[0] >= r.x0 - 0.5 and c[2] <= r.x1 + 0.5 and c[1] >= r.y0 - 0.5 and c[3] <= r.y1 + 0.5]
        base = min(inside)[4] if inside else r.y1
        out.append({'t': norm(t), 'x0': r.x0, 'y0': r.y0, 'x1': r.x1, 'y1': r.y1, 'b': base})
    return out


def plumber_words(ppg, media, crop, rot):
    # pdfplumber's frame is the MediaBox as displayed, shifted so that page.bbox lands where it says:
    # anchor on page.bbox, then go through user space to wordbox's CropBox frame
    bx0, btop = ppg.bbox[0], ppg.bbox[1]
    to_user = display_unmap(media, rot)
    to_disp = display_map(crop, rot)
    conv = lambda a, b: to_disp(*to_user(a - bx0, b - btop))
    out = []
    for w in ppg.extract_words(keep_blank_chars=False):
        (a0, b0), (a1, b1) = conv(w['x0'], w['top']), conv(w['x1'], w['bottom'])
        out.append({'t': norm(w['text']), 'x0': min(a0, a1), 'y0': min(b0, b1), 'x1': max(a0, a1), 'y1': max(b0, b1)})
    return out


def centre(w): return ((w['x0'] + w['x1']) / 2, (w['y0'] + w['y1']) / 2)


def v_overlap(w, v):
    o = min(w['y1'], v['y1']) - max(w['y0'], v['y0'])
    return o >= 0.5 * max(min(w['y1'] - w['y0'], v['y1'] - v['y0']), 0.1)


def consensus(a, b):
    grid = {}
    for k, w in enumerate(b):
        cx, cy = centre(w)
        grid.setdefault((int(cx // 4), int(cy // 16)), []).append(k)
    used = set()
    ref, moved, only_a = [], 0, 0
    for w in a:
        cx, cy = centre(w)
        best = None
        for dx in (-1, 0, 1):
            for dy in (-1, 0, 1):
                for k in grid.get((int(cx // 4) + dx, int(cy // 16) + dy), []):
                    v = b[k]
                    if k in used or v['t'] != w['t']:
                        continue
                    vx, vy = centre(v)
                    d = abs(vx - cx) + abs(vy - cy)
                    if abs(vx - cx) <= NEAR and v_overlap(w, v) and (best is None or d < best[0]):
                        best = (d, k)
        if best is None:
            only_a += 1
            continue
        v = b[best[1]]
        used.add(best[1])
        if abs(v['x0'] - w['x0']) <= TOL and abs(v['x1'] - w['x1']) <= TOL:
            ref.append({'t': w['t'], 'x0': round((w['x0'] + v['x0']) / 2, 2), 'x1': round((w['x1'] + v['x1']) / 2, 2),
                        'b': round(w['b'], 2), 'y0': round(w['y0'], 2), 'y1': round(w['y1'], 2)})
        else:
            moved += 1
    return ref, moved, only_a, len(b) - len(used)


def one(path):
    try:
        doc = fitz.open(path)
        pdf = pdfplumber.open(path)
    except Exception as e:
        return {'file': path, 'error': str(e)}
    pages = []
    for i, pg in enumerate(doc):
        try:
            media, crop, rot = boxes(pg)
            if i >= len(pdf.pages):
                pages.append({'n': i + 1, 'error': 'not in pdfplumber (it finds fewer pages)'})
                continue
            a = pymupdf_words(pg)
            b = plumber_words(pdf.pages[i], media, crop, rot)
            ref, moved, only_a, only_b = consensus(a, b)
            pages.append({'n': i + 1, 'words': ref, 'moved': moved, 'only_pymupdf': only_a, 'only_pdfplumber': only_b,
                          'pymupdf': len(a), 'pdfplumber': len(b)})
        except Exception as e:
            pages.append({'n': i + 1, 'error': str(e)[:200]})
    pdf.close()
    return {'file': path, 'pages': pages}


if __name__ == '__main__':
    files = [l.strip() for l in open(sys.argv[1], encoding='utf-8') if l.strip()]
    jobs = int(sys.argv[sys.argv.index('--jobs') + 1]) if '--jobs' in sys.argv else max(1, multiprocessing.cpu_count() - 1)
    with open(sys.argv[2], 'w', encoding='utf-8') as out, multiprocessing.Pool(jobs) as pool:
        for k, r in enumerate(pool.imap(one, files, chunksize=2)):
            out.write(json.dumps(r, ensure_ascii=False) + '\n')
            if (k + 1) % 50 == 0:
                print(f'{k + 1}/{len(files)}', file=sys.stderr, flush=True)
