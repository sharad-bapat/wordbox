"""Build the constructed garble set: clean ContractNLI PDFs with their ToUnicode maps replaced.

ToUnicode takes precedence over everything else in every extractor, so replacing it changes what
the file's text layer says, while the page still looks the same. Deleting a map wouldn't do: the
embedded font program can legitimately recover the text. Variants, labelled by construction:

  clean    saved through PyMuPDF unchanged, as a control
  pua      every code -> a Private Use Area character
  control  every code -> a control character (the "\\x02G" kind of garbage)
  shift    the real text moved one letter along (a->b, z->a); the hard case

Only files whose every font can be rewritten are used: fonts with a ToUnicode map, or simple fonts
with a plain WinAnsiEncoding or MacRomanEncoding. 60 are chosen by seed: 30 dev, 30 held-out.

usage: python tools/garble.py <contract-nli raw dir> <out dir>
"""
import glob, json, os, random, re, sys

import fitz

SEED, N_DEV, N_HELD = 20260927, 30, 30
CONTROL = [c for c in range(1, 32) if c not in (9, 10, 13)]


def shift(s):
    out = []
    for c in s:
        if 'a' <= c <= 'z': out.append(chr((ord(c) - 97 + 1) % 26 + 97))
        elif 'A' <= c <= 'Z': out.append(chr((ord(c) - 65 + 1) % 26 + 65))
        else: out.append(c)
    return ''.join(out)


def utf16(h):
    b = bytes.fromhex(h if len(h) % 2 == 0 else h + '0')
    return b.decode('utf-16-be', errors='replace')


def parse_tounicode(data):
    """Code -> text, the code byte length, and the codespace block, from a ToUnicode CMap."""
    s = data.decode('latin-1')
    m = {}
    width = None
    for block in re.findall(r'beginbfchar(.*?)endbfchar', s, re.S):
        for src, dst in re.findall(r'<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]*)>', block):
            m[int(src, 16)] = utf16(dst); width = width or len(src)
    for block in re.findall(r'beginbfrange(.*?)endbfrange', s, re.S):
        for lo, hi, rest in re.findall(r'<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*(\[[^\]]*\]|<[0-9A-Fa-f]*>)', block):
            a, b = int(lo, 16), int(hi, 16)
            width = width or len(lo)
            if b - a > 20000:
                b = a + 20000
            if rest.startswith('['):
                for k, d in enumerate(re.findall(r'<([0-9A-Fa-f]*)>', rest)):
                    if a + k <= b: m[a + k] = utf16(d)
            else:
                d = rest[1:-1]
                if not d:
                    continue
                units = [int(d[i:i + 4], 16) for i in range(0, len(d) - len(d) % 4, 4)] or [int(d, 16)]
                for k in range(b - a + 1):
                    u = units[:-1] + [(units[-1] + k) & 0xFFFF]
                    m[a + k] = b''.join(x.to_bytes(2, 'big') for x in u).decode('utf-16-be', errors='replace')
    cs = re.search(r'begincodespacerange(.*?)endcodespacerange', s, re.S)
    return m, (width or 2) // 2, (cs.group(1).strip() if cs else None)


def cmap_bytes(mapping, nbytes, codespace):
    lines = ['/CIDInit /ProcSet findresource begin', '12 dict begin', 'begincmap',
             '/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def',
             '/CMapName /Adobe-Identity-UCS def', '/CMapType 2 def',
             '1 begincodespacerange', codespace or ('<00> <FF>' if nbytes == 1 else '<0000> <FFFF>'), 'endcodespacerange']
    items = sorted(mapping.items())
    for i in range(0, len(items), 100):
        chunk = items[i:i + 100]
        lines.append(f'{len(chunk)} beginbfchar')
        for code, text in chunk:
            dst = text.encode('utf-16-be').hex().upper() or '0000'
            lines.append(f'<{code:0{nbytes * 2}X}> <{dst}>')
        lines.append('endbfchar')
    lines += ['endcmap', 'CMapName currentdict /CMap defineresource pop', 'end', 'end']
    return '\n'.join(lines).encode('latin-1')


def font_map(doc, xref):
    """(code -> text, bytes per code, codespace) for a font we can rewrite, else None."""
    tu = doc.xref_get_key(xref, 'ToUnicode')
    if tu[0] == 'xref':
        data = doc.xref_stream(int(tu[1].split()[0]))
        if data:
            m, n, cs = parse_tounicode(data)
            if m:
                return m, n, cs
        return None
    sub = doc.xref_get_key(xref, 'Subtype')[1]
    enc = doc.xref_get_key(xref, 'Encoding')
    if sub in ('/Type1', '/TrueType') and enc[0] == 'name' and enc[1] in ('/WinAnsiEncoding', '/MacRomanEncoding'):
        codec = 'cp1252' if enc[1] == '/WinAnsiEncoding' else 'mac_roman'
        m = {}
        for c in range(32, 256):
            t = bytes([c]).decode(codec, errors='ignore')
            if t:
                m[c] = t
        return m, 1, None
    return None


def fonts_of(doc):
    xs = set()
    for p in doc:
        for f in p.get_fonts(full=True):
            xs.add(f[0])
    return sorted(xs)


def variant_text(kind, code, text):
    if kind == 'pua': return chr(0xE000 + code % 6400)
    if kind == 'control': return chr(CONTROL[code % len(CONTROL)])
    if kind == 'shift': return shift(text)
    return text


def build(src, dst, kind):
    doc = fitz.open(src)
    if kind != 'clean':
        for x in fonts_of(doc):
            fm = font_map(doc, x)
            if fm is None:
                continue
            m, n, cs = fm
            new = {c: variant_text(kind, c, t) for c, t in m.items()}
            nx = doc.get_new_xref()
            doc.update_object(nx, '<<>>')
            doc.update_stream(nx, cmap_bytes(new, n, cs))
            doc.xref_set_key(x, 'ToUnicode', f'{nx} 0 R')
    doc.save(dst, garbage=1, deflate=True)
    doc.close()


def eligible(path):
    try:
        doc = fitz.open(path)
    except Exception:
        return False
    xs = fonts_of(doc)
    # fonts written inline (no object number) can't be given a new ToUnicode map
    try:
        ok = bool(xs) and 0 not in xs and all(font_map(doc, x) is not None for x in xs) and not doc.is_encrypted
    except ValueError:
        ok = False
    ok = ok and sum(len(p.get_text().strip()) for p in doc) > 200
    doc.close()
    return ok


if __name__ == '__main__':
    raw, out = sys.argv[1], sys.argv[2]
    files = sorted(glob.glob(os.path.join(raw, '*.pdf')))
    ok = [f for f in files if eligible(f)]
    rnd = random.Random(SEED)
    rnd.shuffle(ok)
    chosen = {'dev': ok[:N_DEV], 'heldout': ok[N_DEV:N_DEV + N_HELD]}
    manifest = []
    for split, fs in chosen.items():
        for k, f in enumerate(fs):
            stem = f'g{split[0]}{k:02d}'  # neutral names
            clean_doc = fitz.open(f)
            has_text = [len(p.get_text().strip()) > 0 for p in clean_doc]
            clean_doc.close()
            for kind in ('clean', 'pua', 'control', 'shift'):
                d = os.path.join(out, split, kind)
                os.makedirs(d, exist_ok=True)
                dst = os.path.join(d, stem + '.pdf')
                build(f, dst, kind)
                labels = [('text' if kind == 'clean' else 'garbled') if t else 'none' for t in has_text]
                manifest.append({'file': dst.replace('\\', '/'), 'source': os.path.basename(f), 'split': split, 'kind': kind, 'pages': labels})
    with open(os.path.join(out, 'manifest.json'), 'w', encoding='utf-8') as fh:
        json.dump({'seed': SEED, 'eligible': len(ok), 'of': len(files), 'items': manifest}, fh, indent=1)
    print(f'eligible {len(ok)} of {len(files)}; built {len(manifest)} files')
