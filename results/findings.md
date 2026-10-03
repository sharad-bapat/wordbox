# Findings about other software and real files

Facts the checks turned up about libraries, data sources and PDFs in the wild, as opposed to bugs in this repo's own code. Each entry says how it was found and what it was checked against.

## PyMuPDF reads ZapfDingbats glyph names as decimal character codes (3 October 2026)

PyMuPDF 1.24.9 (MuPDF 1.24.8) turns a ZapfDingbats glyph name aNN into the character whose code is NN in decimal, unless the font has a ToUnicode map. A Type 1 /ZapfDingbats font, not embedded, showing the string "lnu4" comes out as "G", "I", "N" and the control character U+0014. The font's built-in encoding names those codes a71, a73, a78 and a20 (ZapfDingbats.afm, codes 108, 110, 117 and 52), and Adobe's zapfdingbats.txt maps them to U+25CF ●, U+25A0 ■, U+25C6 ◆ and U+2714 ✔. The result is the same when /Differences names the glyph (/Differences [108 /a71]). With a ToUnicode map, PyMuPDF reads the map and gets ■ (001116). pdfminer.six 20251230 gets the same string wrong another way: it returns the codes themselves, "lnu4".

Found when wordbox learned ZapfDingbats' built-in encoding (results/after-heldout.md): on the dev set, PyMuPDF reads the bullets of 001498 as "G" (29 of them), 001931 as "N" (65) and 001063 as "I" (3). A render of 001498 p1 shows a black circle where the first is drawn. wordbox's reference words come from PyMuPDF, so the reference holds the letters and neither the old nor the new wordbox matches them.
