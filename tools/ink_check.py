"""Ink test: do wordbox's word boxes cover the text a page draws?

Renders each page with PyMuPDF, independently of either parser, and covers its ink with:

  others     everything on the page that isn't text, from where-are-the-regions' page map (its images,
             painted paths and annotations, each grown by MARGIN_PT for anti-aliasing), so that only
             text ink is left to find
  words      wordbox's visible words (their "ink" box when the output gives one), grown the same way
  ref        the same with where-are-the-regions' own words instead, for comparison

  ink        a pixel darker than INK_LEVEL on any channel (the page background is white)
  coverage   share of ink pixels covered, per page
  missed     ink left uncovered with wordbox's words, attributed to what PyMuPDF says is there, first
             match wins: text (its characters), image, vector (its drawings), annot, other

Ink a glyph draws outside its word box (an italic overhang, a tall accent) shows up as missed ink
next to text. Both maps share the same non-text regions, so any difference between words and ref
comes from the word boxes.

usage:
  python tools/ink_check.py <list.txt> [--every=N] [--regions=<regions-cli>] [--worst=N] [--out=file.jsonl]

--every=N takes every Nth page of the list's files, in order (default 10). The held-out list is
refused unless tools/check_frozen.py passes.
"""
import json
import statistics
import subprocess
import sys
from collections import Counter
from pathlib import Path

import fitz
import numpy as np
from rich.progress import MofNCompleteColumn, Progress, TimeElapsedColumn

ROOT = Path(__file__).resolve().parent.parent
CLI = ROOT / "extractor" / "target" / "release" / "wordbox-cli.exe"
REGIONS = ROOT.parent / "where-are-the-regions" / "regions" / "target" / "release" / "regions-cli.exe"
DPI = 150
INK_LEVEL = 250
MARGIN_PT = 1.0
KINDS = ["text", "image", "vector", "annot", "other"]


def run(cli, path):
    out = subprocess.run([str(cli), str(path)], capture_output=True, text=True, encoding="utf-8").stdout
    return json.loads(out) if out.strip() else {"status": "no output"}


def fill(mask, box, scale, grow=0.0):
    """Set the pixels of box (points, top-left origin) in mask."""
    h, w = mask.shape
    x0 = max(int((box[0] - grow) * scale), 0)
    y0 = max(int((box[1] - grow) * scale), 0)
    x1 = min(int(np.ceil((box[2] + grow) * scale)), w)
    y1 = min(int(np.ceil((box[3] + grow) * scale)), h)
    if x1 > x0 and y1 > y0:
        mask[y0:y1, x0:x1] = True


def box_of(r):
    return tuple(r["ink"]) if "ink" in r else (r["x0"], r["y0"], r["x1"], r["y1"])


def others(pmap):
    """Where-are-the-regions' non-text regions that paint something, as in its own ink test."""
    out = [r for r in pmap["regions"] if "offpage" not in [w for w, _ in r["reasons"]]]
    out += [p for p in pmap.get("paths", []) if not (p.get("offpage") or p.get("hidden") or p.get("empty"))]
    out += [a for a in pmap.get("annots", []) if a.get("appearance") and not (a.get("hidden") or a.get("offpage"))]
    return out


def pymupdf_masks(page, shape, scale):
    """What PyMuPDF says is on the page, one mask per kind, for attributing missed ink. Its boxes are
    on the unrotated page while it renders the page turned, so each goes through the rotation first."""
    m = {k: np.zeros(shape, bool) for k in KINDS[:-1]}
    rot = page.rotation_matrix

    def put(kind, box, grow):
        r = fitz.Rect(box) * rot
        fill(m[kind], (r.x0, r.y0, r.x1, r.y1), scale, grow)

    for b in page.get_text("rawdict")["blocks"]:
        for line in b.get("lines", []):
            for span in line["spans"]:
                for c in span["chars"]:
                    if not c["c"].isspace():
                        put("text", c["bbox"], MARGIN_PT)
    for info in page.get_image_info():
        put("image", info["bbox"], MARGIN_PT)
    for d in page.get_drawings():
        put("vector", d["rect"], MARGIN_PT + (d.get("width") or 0) / 2)
    for a in page.annots() or []:
        put("annot", a.rect, MARGIN_PT)
    for wd in page.widgets() or []:
        put("annot", wd.rect, MARGIN_PT)
    return m


def check(page, wpage, rpage):
    pix = page.get_pixmap(dpi=DPI, alpha=False)
    a = np.frombuffer(pix.samples, dtype=np.uint8).reshape(pix.height, pix.width, pix.n)
    ink, scale = a.min(axis=2) < INK_LEVEL, pix.width / page.rect.width
    base = np.zeros(ink.shape, bool)
    for r in others(rpage):
        fill(base, box_of(r), scale, MARGIN_PT)
    ours, ref = base.copy(), base.copy()
    n_ink = 0
    for w in wpage["words"]:
        if not (w.get("invisible") or w.get("offpage")):
            fill(ours, box_of(w), scale, MARGIN_PT)
            n_ink += "ink" in w
    for w in rpage["words"]:
        if not (w.get("invisible") or w.get("offpage") or w.get("hidden")):
            fill(ref, box_of(w), scale, MARGIN_PT)
    total = int(ink.sum())
    missed = ink & ~ours
    by_kind = {}
    if missed.any():
        left = missed.copy()
        for k, m in pymupdf_masks(page, ink.shape, scale).items():
            by_kind[k] = int((left & m).sum())
            left &= ~m
        by_kind["other"] = int(left.sum())
    cov = lambda c: 1.0 if total == 0 else 1 - (ink & ~c).sum() / total
    return {"ink": total, "coverage": cov(ours), "ref": cov(ref), "missed": by_kind, "ink_boxes": n_ink}


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    opt = dict(a[2:].split("=", 1) for a in sys.argv[1:] if a.startswith("--") and "=" in a)
    if len(args) != 1:
        sys.exit(__doc__)
    if "heldout" in Path(args[0]).name:
        if subprocess.run([sys.executable, str(ROOT / "tools" / "check_frozen.py")], cwd=ROOT).returncode:
            sys.exit("held-out list refused: the frozen source has changed")
    every, worst = int(opt.get("every", 10)), int(opt.get("worst", 10))
    regions = Path(opt.get("regions", REGIONS))
    files = [l.strip() for l in open(args[0], encoding="utf-8") if l.strip()]

    rows, k = [], 0
    bar = Progress(*Progress.get_default_columns(), TimeElapsedColumn(), MofNCompleteColumn(), transient=True)
    task = bar.add_task("ink test", total=len(files))
    bar.start()
    for f in files:
        bar.advance(task)
        d = None
        with fitz.open(f) as doc:
            for pno in range(doc.page_count):
                k += 1
                if (k - 1) % every:
                    continue
                if d is None:
                    d, r = run(CLI, f), run(regions, f)
                name = Path(f).name
                if d.get("status") != "ok" or r.get("status") not in (None, "ok") or pno >= len(d["pages"]) or pno >= len(r["pages"]):
                    rows.append({"file": name, "page": pno, "error": d.get("status") if d.get("status") != "ok" else r.get("status") or "pages"})
                    continue
                rows.append({"file": name, "page": pno, **check(doc[pno], d["pages"][pno], r["pages"][pno])})
    bar.stop()

    ok = [r for r in rows if "error" not in r]
    print(f"{len(rows)} pages (every {every}th of {k}), {len(rows) - len(ok)} errors, {DPI} dpi, ink < {INK_LEVEL}, margin {MARGIN_PT} pt")
    print(f"{'boxes':<9} {'100%':>6} {'>=99.9%':>8} {'>=99.5%':>8}   median  worst")
    for label, key in (("wordbox", "coverage"), ("regions", "ref")):
        c = [r[key] for r in ok]
        print(f"{label:<9} {sum(x == 1 for x in c):>6} {sum(x >= 0.999 for x in c):>8} {sum(x >= 0.995 for x in c):>8}"
              f"   {100 * statistics.median(c):6.2f}%  {100 * min(c):6.2f}%")
    ink = sum(r["ink"] for r in ok)
    miss = Counter()
    for r in ok:
        miss.update(r["missed"])
    print(f"missed ink with wordbox's words, share of all ink ({ink} px): " + ", ".join(f"{k} {100 * miss[k] / max(ink, 1):.3f}%" for k in KINDS))
    print(f"pages where wordbox covers less than regions: {sum(1 for r in ok if r['coverage'] < r['ref'] - 1e-9)}, more: {sum(1 for r in ok if r['coverage'] > r['ref'] + 1e-9)}")
    print(f"words with an ink box: {sum(r['ink_boxes'] for r in ok)}")
    print(f"worst {worst}:")
    for r in sorted(ok, key=lambda r: r["coverage"])[:worst]:
        print(f"  {r['file']} p{r['page']} {100 * r['coverage']:.2f}% (regions {100 * r['ref']:.2f}%) missed {r['missed']}")
    for r in rows:
        if "error" in r:
            print(f"  error {r['file']} p{r['page']}: {r['error']}")
    if "out" in opt:
        Path(opt["out"]).write_text("\n".join(json.dumps(r) for r in rows) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
