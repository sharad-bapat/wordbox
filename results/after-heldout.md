# Changes after the held-out run

The held-out results in results/chunk6-heldout.txt are the frozen rules' own and stay as they are. Each change below was made later, relocked (results/frozen.sha256, tools/check_frozen.py), and measured the same way, so its effect is reported here apart from the held-out run.

## A positive /Descent (3 October 2026)

The PDF spec gives a font's /Descent as negative, but some files write it positive (ContractNLI and govdocs1 files carry "/Descent 270" for Arial Unicode and 206 for Tahoma). wordbox clamped it to zero, so those words' boxes stopped at the baseline. extractor/src/font.rs now takes its size and forces the sign, and a missing or zero value falls back to the font's /FontBBox (vertical_from, ported from where-are-the-regions, with two unit tests).

Old and new output on the 659 dev files, word by word: 31 files change, 30,827 of 3,451,358 word boxes. 30,783 of them only move their bottom edge below the baseline (for example "Bacon" in BaconNon-Disclosure.pdf, 104.5 to 106.7 pt). The other 44 are rotated text in 001103 (map labels at many angles) and 001303 (three vertical lines), where the space below the baseline points sideways, so the box grows sideways.

tools/score.py, same reference and garble sets:

| | Before | After |
|---|---|---|
| Dev: reference words found | 3,264,888 of 3,267,791 (99.91%) | unchanged |
| Dev: pages with at least 99% found | 97.45% of 9,567 | unchanged |
| Held-out: reference words found | 1,722,169 of 1,730,015 (99.55%) | 1,722,178 (99.55%) |
| Held-out: x0, x1, baseline within 1 pt | 99.98%, 99.96%, 99.86% | unchanged |
| Held-out: pages with at least 99% found | 93.89% of 6,107 | 93.91% |
| Verdicts, garble sets and real files | | unchanged |

The scorer checks the left and right edges and the baseline, not the bottom, so the headline numbers barely move; the 9 extra held-out words are matches the vertical-overlap test now finds.
