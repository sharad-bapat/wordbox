# wordbox: plan

Where is the text? What text does this PDF contain, and exactly where is it drawn? (The page on sharadbapat.com will be "Where is the text?" at /experiments/where-is-the-text/.)

The third tool in a chain, after file-checker (what is this file?) and scan-or-text (does it need OCR?). For a born-digital PDF, the file already records every character it draws and where. This tool reads that record and reports it: every word, with its box on the page, and a verdict on whether the text layer can be trusted. It's fully deterministic. There's no model and nothing is inferred about meaning.

## Scope

In:

- Every glyph drawn on each page, decoded to Unicode, with its box.
- Words, built from glyphs by a fixed rule: a new word starts at a whitespace glyph, a gap along the baseline wider than 0.1 em, a step back of more than 0.5 em, or a baseline or direction change (constants in lib.rs, tuned on dev only, then frozen).
- A line number per word, by a fixed rule: a new line starts when the baseline moves more than half the font size or the pen steps back. Geometry only, in drawing order.
- Text drawn by annotations and form fields, from each shown annotation's normal appearance stream (not Hidden, not NoView), flagged `annot`. Glyphs whose box centre falls outside the visible page are kept and flagged `offpage`.
- A verdict per page: `text` (usable), `garbled` (the text layer exists but doesn't decode to real characters), `invisible` (an OCR layer: text drawn with render mode 3), or `none`.
- A browser demo: the page rendered by pdf.js on one side with the boxes over it, the extracted words on the other, and hovering a word or a box lights up both.

Out, on purpose:

- Paragraphs, columns, reading order, tables, headings or any grouping by meaning. Words come out in the order the file draws them.
- Scanned pages. scan-or-text routes those to OCR first.
- Form fields that have a value but no appearance stream: turning the value into drawn text would mean making up the drawing. When a form asks viewers to regenerate appearances (NeedAppearances), a viewer can show a field's current value while wordbox reports the stored drawing.
- Vertical writing (Identity-V fonts).

## Output

One JSON document per file. Coordinates are PDF points with the origin at the top-left of the visible page (after CropBox and /Rotate), so a box can be drawn straight onto a rendered page.

```json
{
  "pages": [
    {
      "n": 1, "width": 612, "height": 792, "rotate": 0,
      "verdict": "text",
      "words": [
        { "t": "Agreement", "x0": 72.0, "y0": 81.4, "x1": 131.6, "y1": 93.4, "line": 0, "font": 3, "size": 12 },
        { "t": "Signature", "x0": 310.2, "y0": 640.0, "x1": 355.8, "y1": 651.1, "line": 41, "font": 5, "size": 10, "annot": true }
      ]
    }
  ]
}
```

A box is the glyph advance along the baseline, and the font's descent to ascent across it (FontDescriptor, else FontBBox, else 0.8 and -0.2 em). `font` indexes a `fonts` list with each font's name, type, encoding and width source. Flags appear only when true: `invisible`, `annot`, `offpage`, and `unmapped` (a count). `--glyphs` adds every glyph with its box and baseline start.

Chunk 3 checks against PyMuPDF on every fifth dev file (results/chunk3-geometry-dev.txt): glyph x0, x1 and baseline within 0.5 pt for 99.88%, 99.92% and 100% of 3.7M glyphs (median 0.02 pt); rotated pages 100% matched; words agree 99.73% one way and 99.74% the other. Output is byte-identical across runs.

## How it works

It starts from a copy of scan-or-text's parser (object index, page tree, decryption, stream filters, content-stream interpreter). This is a copy, not a dependency, so each tool stays independent. New parts:

1. Fonts and decoding (done, chunk 2). A code's text is looked up in a fixed order:
   1. the font's ToUnicode CMap;
   2. the encoding: WinAnsi, MacRoman or Standard, plus /Differences, and a Type1 font program's built-in `dup N /name put` encoding; glyph names go to Unicode by the Adobe Glyph List rules;
   3. an embedded TrueType program: code to glyph through its `cmap`, then glyph to Unicode through its own Unicode `cmap` (the lowest code point wins) or `post` glyph names. When a simple font maps the code in its (1,0) subtable, that subtable is keyed by Mac Roman codes, so the Mac Roman character is used. The program decides only when the file gives no /Encoding; otherwise it fills gaps. For CID fonts it goes code, then CID (from the encoding CMap), then glyph (/CIDToGIDMap), then Unicode;
   4. otherwise the glyph is unmapped. Nothing is guessed.

   The parser also gained the LZW and RunLength filters (common in 1990s PDFs), and reads /Contents given as a reference to an array (a bug in scan-or-text's copy of the code).
2. Widths: /Widths with /FirstChar, CID /W arrays with /DW, and built-in width tables for the 14 standard fonts, which are often not embedded.
3. Text state and geometry: Tc, Tw, Tz, TL, Ts, TJ adjustments, the text and graphics matrices, form XObjects, and page rotation and CropBox.
4. Words and lines by the fixed rules above.
5. The verdict, from signals the parser knows directly: codes it couldn't map to Unicode, U+FFFD, private-use characters and control characters. Whether a word list is needed to catch letter-shifted gibberish is one of the questions below.

Budget: under 2,000 lines of Rust, one or two small dependencies (inflate, plus the decryption crates scan-or-text already uses), and a WebAssembly build under 250 KB.

## Data

All public, none redistributed:

- ContractNLI's 375 source PDFs (Koreeda & Manning 2021, CC BY 4.0), mostly TrueType, with 181 files using Identity-H CID fonts.
- govdocs1 thread 001, 284 PDFs (public domain), with more Type1 and some Type3 fonts. This is the dev set, together with ContractNLI.
- govdocs1 thread 002, 242 PDFs, never looked at. This is the held-out set.
- Constructed garble cases (tools/garble.py, chunk 4). 60 ContractNLI files chosen by seed 20260927 from the 354 of 374 whose every font can be rewritten (fonts with a ToUnicode map, or plain WinAnsi or MacRoman simple fonts): 30 dev, 30 held-out, with neutral names. Each file's ToUnicode maps are replaced, which changes what the text layer says while the page looks the same. ToUnicode wins in every extractor. Deleting a map wouldn't work, because the embedded font program can legitimately recover the text. The variants, labelled by construction:
  - `pua`: every code maps into the Private Use Area,
  - `control`: every code maps to a control character, the `\x02G` kind of garbage seen in govdocs,
  - `shift`: the real text moved one letter along (the hard case: it looks like letters but reads as nonsense),
  - `clean`: saved the same way, unchanged, as a control.

  Checked independently with PyMuPDF (tools/garble_verify.py): clean copies keep 100% of the original text; pua pages are at least 97.9% private-use characters; shift pages match the shifted clean text at least 98.9%; and page 1 of all 240 files renders pixel-identical to its clean copy. One finding: PyMuPDF throws away ToUnicode entries that map to control characters and falls back to the encoding, so it shows mostly real text for the control variant (and loses some letters: "ONE W Y NON-DISCLOSURE GREEMENT"). pdfplumber and wordbox pass the file's control characters through.

## Reference and scoring

The reference for text and boxes is consensus: PyMuPDF 1.24.9 and pdfplumber 0.11.9, where they agree. Pages where they disagree get reported separately and a sample is checked by hand. Neither is treated as truth on its own.

How it's built (tools/reference.py, chunk 4): both tools' words are put in wordbox's frame. PyMuPDF reports the unrotated page, so its points go through `page.rotation_matrix`. pdfplumber reports the rotated MediaBox, so its points go back to user space and then to the CropBox frame. A reference word has the same NFKC text in both, horizontal centres within 2 pt, x0 and x1 within 1 pt, and boxes that overlap vertically by at least half the shorter box. Vertical overlap is used because the tools size boxes differently: PyMuPDF uses the font's ascent and pdfplumber the font size. x0 and x1 are the mean of the two. The baseline and box height come from PyMuPDF, since pdfplumber rotates its character matrices on rotated pages. So the baseline metric compares against PyMuPDF on words both tools place the same way. Words only one tool has, or both have but place apart, are counted and left out. pdfplumber's frame is anchored on its own `page.bbox`, which handles MediaBoxes that don't start at the origin.

The metrics, fixed before any results:

- Text: word match rate and character error rate against the reference, per page.
- Geometry, for matched words: the difference in x0, x1 and baseline, in points, and the share within 1 point. The box height depends on each tool's ascent convention, so full-box overlap is reported but isn't the main number.
- Verdict: precision and recall of `garbled` on the constructed set, and false alarms on clean files.
- Speed and size: median and p95 per file, native and WebAssembly, against pdf.js 6.3 `getTextContent` in Node, pdfplumber and PyMuPDF. pdf.js returns text runs, not words, so its runs are compared at line level, and the method is written down with the results.

The rules get tuned on dev only, then frozen by a hash of the source before the held-out run, as in file-checker.

## Claims to test

These are targets written before the results, not promises:

1. On born-digital pages, at least 99% of words match the reference, and at least 99% of matched words are within 1 point on x-extent and baseline.
2. The verdict flags every private-use and control-character constructed page, with no false alarms on the untouched copies, and flags real unmapped text (like govdocs 001157 and 001389). The letter-shift case may need a word list; we'll report whether it did. (Written before results; the variant names changed in chunk 4 from "unmappable" to "control", because deleting a map lets the font program recover the text.)
3. Faster than pdf.js text extraction in the same runtime, at a small fraction of its size.

## Known limits, stated up front

- Type3 fonts draw glyphs with their own procedures; their boxes come from the font matrix and widths only.
- Text that's clipped away, or drawn in the page's background colour, is still extracted. Only render mode 3 counts as invisible.
- Ligatures come out as whatever the ToUnicode map says ("fi" or U+FB01).
- Right-to-left text comes out in drawing order.
- Words are split by a spacing rule, so tightly kerned or letter-spaced text can split or join differently from the reference.

## Demo

pdf.js (Apache-2.0) renders the page. It's the one runtime dependency, and it's only used on the demo page. Its eval-based font path is switched off (`isEvalSupported: false`). The site's check refuses scripts that contain `eval` or `new Function`, and pdf.js contains that code path even when it's disabled. It gets a one-off exception, for the vendored pdf.js files only, listed by exact path. The rule stays for every other script.

## Chunks

1. Plan and data check (this file).
2. Fonts and decoding: glyphs to Unicode.
3. Geometry: glyph boxes, words and lines.
4. Reference labeller (PyMuPDF plus pdfplumber consensus) and the constructed garble set.
5. The verdict and the dev scoring loop, then freeze.
6. Held-out run and the competitor benchmark.
7. WebAssembly build and the side-by-side demo with pdf.js.
8. README, then the write-up on sharadbapat.com (following WRITING.md).
