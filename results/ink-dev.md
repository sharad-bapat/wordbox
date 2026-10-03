# Ink test on dev

tools/ink_check.py renders every 10th dev page with PyMuPDF (150 dpi, ink darker than 250, boxes grown by 1 pt) and covers the ink with where-are-the-regions' non-text regions (images, painted paths, annotations) plus wordbox's visible words. What's left is text ink outside the word boxes. Where-are-the-regions' own words, which carry ink boxes from the glyph outlines, are run alongside as the reference. Rows per page are in results/ink-dev-<n>.jsonl.

## Baseline: typographic boxes (3 October 2026, wordbox c348ba2)

979 pages, 0 errors.

| Boxes | Pages at 100% | At 99.9% or more | Median | Worst |
|---|---|---|---|---|
| wordbox | 790 | 976 | 100.00% | 85.19% |
| where-are-the-regions | 974 | 979 | 100.00% | 99.98% |

wordbox covers less than the reference on 186 pages and more on none. The missed ink is 0.017% of all ink. The three pages under 99.9% (001494 p19 at 85.19% and p9 at 96.43%, 001157 p2 at 98.93%) are Type 3 fonts whose glyphs have zero width, so their words are zero-width boxes while the glyphs draw ink.

## Outlines from embedded TrueType and CFF programs (3 October 2026)

extractor/src/outline.rs reads glyph outlines with ttf-parser 0.25.1 (FontFile2, and FontFile3 as bare CFF or OpenType), and a word gets an "ink" box when an outline reaches past its typographic box, cut to the page. Every other field is unchanged on all 659 dev files; 216,097 of 3,451,329 words get an ink box. Ported from where-are-the-regions.

| Boxes | Pages at 100% | At 99.9% or more | Median | Worst |
|---|---|---|---|---|
| wordbox, typographic boxes | 790 | 976 | 100.00% | 85.19% |
| wordbox, with ink boxes | 925 | 976 | 100.00% | 85.19% |
| where-are-the-regions | 974 | 979 | 100.00% | 99.98% |

wordbox covers less than the reference on 50 pages (186 before). The worst three are the Type 3 pages above, which this change doesn't touch. The WebAssembly build goes from 382 KB to 441 KB (158 KB to 182 KB gzipped).
