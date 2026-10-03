//! wordbox: where is the text?
//!
//! Reads what a born-digital PDF draws and reports each glyph as Unicode text, in drawing order.
//! It never renders anything and never infers meaning: every character comes from the file's own
//! records (fonts, encodings, ToUnicode maps), by rules fixed in the PDF spec.
//!
//! The parser (object index, page tree, decryption, stream filters) is copied from scan-or-text,
//! so the two tools stay independent.
use std::collections::HashMap;

mod cff;
mod cmap;
mod crypt;
mod font;
mod outline;
mod tables;
mod truetype;
mod type1;

pub use font::Kind;

const MAX_PAGES: usize = 2000;
const MAX_FORM_DEPTH: usize = 8;

// ---------- low-level byte helpers ----------

fn is_ws(b: u8) -> bool { matches!(b, b' ' | b'\n' | b'\r' | b'\t' | 0x0c | 0) }
fn is_delim(b: u8) -> bool { matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%') }
fn is_regular(b: u8) -> bool { !is_ws(b) && !is_delim(b) }

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() { return None; }
    let first = needle[0];
    let last = hay.len().checked_sub(needle.len())?;
    let mut i = from;
    while i <= last {
        match hay[i..=last].iter().position(|&b| b == first) {
            None => return None,
            Some(off) => {
                i += off;
                if &hay[i..i + needle.len()] == needle { return Some(i); }
                i += 1;
            }
        }
    }
    None
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() { return None; }
    (0..=hay.len() - needle.len()).rev().find(|&i| &hay[i..i + needle.len()] == needle)
}

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() {
        if is_ws(s[i]) { i += 1; }
        else if s[i] == b'%' { while i < s.len() && s[i] != b'\n' && s[i] != b'\r' { i += 1; } }
        else { break; }
    }
    i
}

fn parse_uint(s: &[u8], i: usize) -> Option<(u64, usize)> {
    let mut j = i;
    let mut v: u64 = 0;
    while j < s.len() && s[j].is_ascii_digit() { v = v.saturating_mul(10).saturating_add((s[j] - b'0') as u64); j += 1; }
    if j == i { None } else { Some((v, j)) }
}

/// Index of the matching close for a balanced "<<...>>" or "[...]" starting at `i` (at the opener).
fn matching(s: &[u8], i: usize) -> usize {
    let mut depth = 0i32;
    let mut j = i;
    while j < s.len() {
        match s[j] {
            b'(' => { j = skip_string(s, j); continue; }
            b'<' if j + 1 < s.len() && s[j + 1] == b'<' => { depth += 1; j += 2; continue; }
            b'>' if j + 1 < s.len() && s[j + 1] == b'>' => { depth -= 1; j += 2; if depth == 0 { return j; } continue; }
            b'[' => depth += 1,
            b']' => { depth -= 1; if depth == 0 { return j + 1; } }
            _ => {}
        }
        j += 1;
    }
    s.len()
}

/// Skip a literal string "(...)" starting at `i`; returns the index after it.
fn skip_string(s: &[u8], i: usize) -> usize {
    let mut depth = 0i32;
    let mut j = i;
    while j < s.len() {
        match s[j] {
            b'\\' => { j += 2; continue; }
            b'(' => depth += 1,
            b')' => { depth -= 1; if depth == 0 { return j + 1; } }
            _ => {}
        }
        j += 1;
    }
    s.len()
}

// ---------- values inside dictionaries ----------

#[derive(Clone, Debug)]
enum Val { Ref(u32), Num(f64), Name(Vec<u8>), Dict(Vec<u8>), Array(Vec<u8>), Other }

fn parse_val(s: &[u8], i: usize) -> Val {
    let i = skip_ws(s, i);
    if i >= s.len() { return Val::Other; }
    match s[i] {
        b'/' => {
            let mut j = i + 1;
            while j < s.len() && is_regular(s[j]) { j += 1; }
            Val::Name(s[i + 1..j].to_vec())
        }
        b'<' if i + 1 < s.len() && s[i + 1] == b'<' => { let e = matching(s, i); Val::Dict(s[i..e.min(s.len())].to_vec()) }
        b'[' => { let e = matching(s, i); Val::Array(s[i + 1..e.saturating_sub(1).max(i + 1)].to_vec()) }
        b'0'..=b'9' => {
            if let Some((n, j)) = parse_uint(s, i) {
                let k = skip_ws(s, j);
                if let Some((_, k2)) = parse_uint(s, k) {
                    let k3 = skip_ws(s, k2);
                    if k3 < s.len() && s[k3] == b'R' && (k3 + 1 >= s.len() || !is_regular(s[k3 + 1])) { return Val::Ref(n as u32); }
                }
                return parse_num(s, i).map(Val::Num).unwrap_or(Val::Other);
            }
            Val::Other
        }
        b'-' | b'+' | b'.' => parse_num(s, i).map(Val::Num).unwrap_or(Val::Other),
        _ => Val::Other,
    }
}

fn parse_num(s: &[u8], i: usize) -> Option<f64> {
    let mut j = i;
    while j < s.len() && (s[j].is_ascii_digit() || matches!(s[j], b'-' | b'+' | b'.')) { j += 1; }
    std::str::from_utf8(&s[i..j]).ok()?.parse().ok()
}

/// Value of a top-level key in a dictionary's bytes (nested dictionaries are skipped).
fn get(dict: &[u8], key: &[u8]) -> Option<Val> {
    let mut i = if dict.starts_with(b"<<") { 2 } else { 0 };
    while i < dict.len() {
        i = skip_ws(dict, i);
        if i >= dict.len() { break; }
        match dict[i] {
            b'/' => {
                let mut j = i + 1;
                while j < dict.len() && is_regular(dict[j]) { j += 1; }
                let k = &dict[i..j];
                let v = parse_val(dict, j);
                if k == key { return Some(v); }
                i = skip_val(dict, j);
            }
            b'>' => break,
            _ => i += 1,
        }
    }
    None
}

fn skip_val(s: &[u8], i: usize) -> usize {
    let i = skip_ws(s, i);
    if i >= s.len() { return i; }
    match s[i] {
        b'<' if i + 1 < s.len() && s[i + 1] == b'<' => matching(s, i),
        b'[' => matching(s, i),
        b'(' => skip_string(s, i),
        b'<' => find(s, b">", i).map(|e| e + 1).unwrap_or(s.len()),
        b'/' => { let mut j = i + 1; while j < s.len() && is_regular(s[j]) { j += 1; } j }
        _ => {
            // a number, or "N G R"
            let mut j = i;
            while j < s.len() && is_regular(s[j]) { j += 1; }
            let k = skip_ws(s, j);
            if let Some((_, k2)) = parse_uint(s, k) {
                let k3 = skip_ws(s, k2);
                if k3 < s.len() && s[k3] == b'R' { return k3 + 1; }
            }
            j
        }
    }
}

fn refs_in(s: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        i = skip_ws(s, i);
        if i >= s.len() { break; }
        if s[i].is_ascii_digit() {
            if let Val::Ref(n) = parse_val(s, i) { out.push(n); }
        }
        i = skip_val(s, i).max(i + 1);
    }
    out
}

fn nums_in(s: &[u8]) -> Vec<f64> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        i = skip_ws(s, i);
        if i < s.len() && (s[i].is_ascii_digit() || matches!(s[i], b'-' | b'+' | b'.')) {
            if let Some(v) = parse_num(s, i) { out.push(v); }
        }
        i = skip_val(s, i).max(i + 1);
    }
    out
}

/// ASCII85 (base-85) decoding, up to the "~>" end marker.
fn ascii85(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 4 / 5);
    let (mut acc, mut n) = (0u64, 0usize);
    let mut i = if s.starts_with(b"<~") { 2 } else { 0 };
    while i < s.len() {
        let c = s[i];
        i += 1;
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                acc = acc * 85 + (c - b'!') as u64;
                n += 1;
                if n == 5 { out.extend_from_slice(&(acc as u32).to_be_bytes()); acc = 0; n = 0; }
            }
            _ if is_ws(c) => {}
            _ => return None,
        }
    }
    if n > 1 {
        for _ in n..5 { acc = acc * 85 + 84; }
        out.extend_from_slice(&(acc as u32).to_be_bytes()[..n - 1]);
    }
    Some(out)
}

/// LZW decoding as PDF uses it (ISO 32000-1 7.4.4): 9 to 12 bit codes, 256 = clear, 257 = end.
/// With `early` (the default EarlyChange 1), the code width grows one code sooner.
fn lzw(data: &[u8], early: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 3);
    // each entry is (prefix entry, last byte, first byte, length); strings are rebuilt on output
    let mut table: Vec<(u32, u8, u8, u32)> = (0..256u32).map(|b| (u32::MAX, b as u8, b as u8, 1)).collect();
    table.push((0, 0, 0, 0));
    table.push((0, 0, 0, 0));
    let (mut width, mut buf, mut nbits, mut i) = (9u32, 0u32, 0u32, 0usize);
    let mut prev: Option<u32> = None;
    let mut scratch = Vec::new();
    loop {
        while nbits < width {
            if i >= data.len() { return out; }
            buf = (buf << 8) | data[i] as u32;
            i += 1;
            nbits += 8;
        }
        let code = (buf >> (nbits - width)) & ((1 << width) - 1);
        nbits -= width;
        buf &= (1 << nbits) - 1;
        if code == 256 { table.truncate(258); width = 9; prev = None; continue; }
        if code == 257 { break; }
        let known = (code as usize) < table.len();
        let (first, entry) = match (known, prev) {
            (true, _) => (table[code as usize].2, code),
            (false, Some(p)) if code as usize == table.len() => (table[p as usize].2, u32::MAX),
            _ => break,
        };
        if let Some(p) = prev {
            let pe = table[p as usize];
            if table.len() < 4096 { table.push((p, first, pe.2, pe.3 + 1)); }
        }
        let e = if entry == u32::MAX { (table.len() - 1) as u32 } else { entry };
        // walk the entry back to its root, then reverse
        scratch.clear();
        let mut k = e;
        while k != u32::MAX { let t = table[k as usize]; scratch.push(t.1); k = t.0; }
        out.extend(scratch.iter().rev());
        prev = Some(e);
        let size = table.len() as u32 + if early { 1 } else { 0 };
        if size >= (1 << width) && width < 12 { width += 1; }
    }
    out
}

/// RunLengthDecode (ISO 32000-1 7.4.5).
fn run_length(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut i = 0;
    while i < data.len() {
        let n = data[i] as usize;
        i += 1;
        if n < 128 { let e = (i + n + 1).min(data.len()); out.extend_from_slice(&data[i..e]); i = e; }
        else if n > 128 { if i < data.len() { out.extend(std::iter::repeat(data[i]).take(257 - n)); } i += 1; }
        else { break; }
    }
    out
}

/// A string value (literal "(...)" with escapes, or hex "<...>") at position `i`.
fn string_at(s: &[u8], i: usize) -> Option<Vec<u8>> {
    if i >= s.len() { return None; }
    if s[i] == b'<' {
        let e = find(s, b">", i)?;
        let hex: Vec<u8> = s[i + 1..e].iter().copied().filter(|b| b.is_ascii_hexdigit()).collect();
        let val = |c: u8| (c as char).to_digit(16).unwrap() as u8;
        return Some(hex.chunks(2).map(|p| (val(p[0]) << 4) | if p.len() > 1 { val(p[1]) } else { 0 }).collect());
    }
    if s[i] != b'(' { return None; }
    let (mut out, mut depth, mut j) = (Vec::new(), 1i32, i + 1);
    while j < s.len() {
        let c = s[j];
        match c {
            b'\\' if j + 1 < s.len() => {
                j += 1;
                match s[j] {
                    b'n' => out.push(b'\n'), b'r' => out.push(b'\r'), b't' => out.push(b'\t'),
                    b'b' => out.push(8), b'f' => out.push(12),
                    b'\r' => { if j + 1 < s.len() && s[j + 1] == b'\n' { j += 1; } }
                    b'\n' => {}
                    d @ b'0'..=b'7' => {
                        let mut v = (d - b'0') as u32;
                        for _ in 0..2 { if j + 1 < s.len() && (b'0'..=b'7').contains(&s[j + 1]) { j += 1; v = v * 8 + (s[j] - b'0') as u32; } }
                        out.push(v as u8);
                    }
                    other => out.push(other),
                }
            }
            b'(' => { depth += 1; out.push(c); }
            b')' => { depth -= 1; if depth == 0 { return Some(out); } out.push(c); }
            _ => out.push(c),
        }
        j += 1;
    }
    Some(out)
}

/// A top-level string value of `key` in a dictionary.
fn get_string(dict: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let mut i = if dict.starts_with(b"<<") { 2 } else { 0 };
    while i < dict.len() {
        i = skip_ws(dict, i);
        if i >= dict.len() || dict[i] != b'/' { i += 1; continue; }
        let mut j = i + 1;
        while j < dict.len() && is_regular(dict[j]) { j += 1; }
        if &dict[i..j] == key { return string_at(dict, skip_ws(dict, j)); }
        i = skip_val(dict, j);
    }
    None
}

// ---------- the object index ----------

enum Loc {
    Top { gen: u16, dict: (usize, usize), stream: Option<(usize, usize)> },
    Packed { buf: usize, start: usize, end: usize },
}

struct Pdf<'a> {
    data: &'a [u8],
    bufs: Vec<Vec<u8>>,
    objs: HashMap<u32, Loc>,
    crypt: Option<crypt::Crypt>,
}

impl<'a> Pdf<'a> {
    fn index(data: &'a [u8]) -> Pdf<'a> {
        let mut objs = HashMap::new();
        let mut i = 0;
        while let Some(p) = find(data, b"obj", i) {
            i = p + 3;
            if p == 0 || !is_ws(data[p - 1]) { continue; }
            if p + 3 < data.len() && is_regular(data[p + 3]) { continue; }
            // walk back over "N G "
            let mut j = p - 1;
            while j > 0 && is_ws(data[j]) { j -= 1; }
            let gen_end = j + 1;
            while j > 0 && data[j].is_ascii_digit() { j -= 1; }
            if j + 1 == gen_end || !is_ws(data[j]) { continue; }
            let gen = parse_uint(data, j + 1).map(|(g, _)| g as u16).unwrap_or(0);
            while j > 0 && is_ws(data[j]) { j -= 1; }
            let num_end = j + 1;
            while j > 0 && data[j].is_ascii_digit() { j -= 1; }
            let num_start = if data[j].is_ascii_digit() { j } else { j + 1 };
            if num_start == num_end { continue; }
            let num = match parse_uint(data, num_start) { Some((n, _)) => n as u32, None => continue };
            let start = p + 3;
            let end = find(data, b"endobj", start).unwrap_or(data.len());
            // search only inside this object: an unbounded search runs to the next stream in the file,
            // which is quadratic in files with many small objects
            let stream = find(&data[..end], b"stream", start);
            let loc = match stream {
                Some(k) => {
                    let mut s = k + 6;
                    if s < data.len() && data[s] == b'\r' { s += 1; }
                    if s < data.len() && data[s] == b'\n' { s += 1; }
                    let e = rfind(&data[s..end], b"endstream").map(|x| s + x).unwrap_or(end);
                    Loc::Top { gen, dict: (start, k), stream: Some((s, e)) }
                }
                None => Loc::Top { gen, dict: (start, end), stream: None },
            };
            objs.insert(num, loc);
            i = end.max(i);
        }
        let mut pdf = Pdf { data, bufs: Vec::new(), objs, crypt: None };
        pdf.crypt = pdf.security_handler();
        pdf.unpack_object_streams();
        pdf
    }

    /// The standard security handler, when the file opens with an empty user password.
    fn security_handler(&self) -> Option<crypt::Crypt> {
        let p = rfind(self.data, b"/Encrypt")?;
        let enc = match parse_val(self.data, p + 8) { Val::Ref(n) => self.dict(n)?, Val::Dict(d) => d, _ => return None };
        if !matches!(get(&enc, b"/Filter"), Some(Val::Name(f)) if f == b"Standard") { return None; }
        let r = match get(&enc, b"/R") { Some(Val::Num(v)) => v as u32, _ => return None };
        // Key length in bits: the top-level /Length if present; revision 4 files usually give it only
        // inside the crypt filter (/CF /StdCF /Length, often in bytes) and AESV2 is always 128.
        let length = match get(&enc, b"/Length") {
            Some(Val::Num(v)) => v as u32,
            _ if r >= 4 => {
                let inner = find(&enc, b"/StdCF", 0).and_then(|k| find(&enc, b"/Length", k)).map(|k| parse_val(&enc, k + 7));
                match inner { Some(Val::Num(v)) if v <= 32.0 => v as u32 * 8, Some(Val::Num(v)) => v as u32, _ => 128 }
            }
            _ => 40,
        };
        let perms = match get(&enc, b"/P") { Some(Val::Num(v)) => v as i64 as i32, _ => return None };
        let o = get_string(&enc, b"/O")?;
        let aes = find(&enc, b"/AESV2", 0).is_some();
        let encrypt_metadata = !matches!(find(&enc, b"/EncryptMetadata", 0), Some(k) if enc[k..].starts_with(b"/EncryptMetadata false"));
        let idp = rfind(self.data, b"/ID")?;
        let id0 = match parse_val(self.data, idp + 3) { Val::Array(a) => string_at(&a, skip_ws(&a, 0))?, _ => return None };
        crypt::Crypt::new(r, if r == 2 { 40 } else { length }, &o, perms, &id0, aes, encrypt_metadata)
    }

    /// Objects packed in object streams. Deterministic: streams are read in file order, and a packed
    /// copy replaces an earlier one, or a top-level object that sits earlier in the file, as an
    /// incremental update would. (Iterating the HashMap directly made the winner depend on its seed.)
    fn unpack_object_streams(&mut self) {
        let mut stms: Vec<(usize, u32)> = self.objs.iter()
            .filter_map(|(n, l)| match l { Loc::Top { dict, stream: Some(_), .. } => Some((dict.0, *n)), _ => None })
            .filter(|(_, n)| matches!(self.dict(*n).and_then(|d| get(&d, b"/Type")), Some(Val::Name(t)) if t == b"ObjStm"))
            .collect();
        stms.sort_unstable();
        // file position of each packed object's stream, for the "newer wins" rule
        let mut packed_at: HashMap<u32, usize> = HashMap::new();
        for (pos, n) in stms {
            let dict = match self.dict(n) { Some(d) => d, None => continue };
            let count = match get(&dict, b"/N") { Some(Val::Num(v)) => v as usize, _ => continue };
            let first = match get(&dict, b"/First") { Some(Val::Num(v)) => v as usize, _ => continue };
            let buf = match self.stream(n) { Some(b) => b, None => continue };
            if first > buf.len() { continue; }
            let header = nums_in(&buf[..first]);
            let idx = self.bufs.len();
            let mut entries = Vec::new();
            for k in 0..count.min(header.len() / 2) {
                let num = header[2 * k] as u32;
                let off = first + header[2 * k + 1] as usize;
                let next = if k + 1 < header.len() / 2 { first + header[2 * k + 3] as usize } else { buf.len() };
                if off <= next && next <= buf.len() { entries.push((num, off, next)); }
            }
            self.bufs.push(buf);
            for (num, start, end) in entries {
                let newer = match self.objs.get(&num) {
                    None => true,
                    Some(Loc::Packed { .. }) => packed_at.get(&num).map(|&p| p < pos).unwrap_or(true),
                    Some(Loc::Top { dict, .. }) => dict.0 < pos,
                };
                if newer {
                    self.objs.insert(num, Loc::Packed { buf: idx, start, end });
                    packed_at.insert(num, pos);
                }
            }
        }
    }

    fn dict(&self, n: u32) -> Option<Vec<u8>> {
        let raw: &[u8] = match self.objs.get(&n)? {
            Loc::Top { dict, .. } => &self.data[dict.0..dict.1],
            Loc::Packed { buf, start, end } => &self.bufs[*buf][*start..*end],
        };
        let s = skip_ws(raw, 0);
        if raw[s..].starts_with(b"<<") { let e = matching(raw, s); Some(raw[s..e].to_vec()) } else { Some(raw[s..].to_vec()) }
    }

    fn resolve(&self, v: &Val) -> Option<Vec<u8>> {
        match v { Val::Dict(d) => Some(d.clone()), Val::Ref(n) => self.dict(*n), _ => None }
    }

    /// Decoded stream data (FlateDecode or unfiltered). None for unsupported filters.
    fn stream(&self, n: u32) -> Option<Vec<u8>> {
        let (gen, s, e) = match self.objs.get(&n)? { Loc::Top { gen, stream: Some(r), .. } => (*gen, r.0, r.1), _ => return None };
        let dict = self.dict(n)?;
        let is_xref = matches!(get(&dict, b"/Type"), Some(Val::Name(t)) if t == b"XRef");
        let decrypted;
        let raw: &[u8] = match &self.crypt {
            Some(c) if !is_xref => { decrypted = c.decrypt(n, gen, &self.data[s..e.max(s)])?; &decrypted }
            _ => &self.data[s..e.max(s)],
        };
        let filters: Vec<Vec<u8>> = match get(&dict, b"/Filter") {
            None => Vec::new(),
            Some(Val::Name(f)) => vec![f],
            Some(Val::Array(a)) => {
                let mut v = Vec::new();
                let mut i = 0;
                while i < a.len() { if let Val::Name(f) = parse_val(&a, i) { v.push(f); } i = skip_val(&a, i).max(i + 1); }
                v
            }
            Some(Val::Ref(r)) => match self.dict(r) { Some(d) if d.starts_with(b"/") => vec![d[1..].to_vec()], _ => return None },
            _ => return None,
        };
        let mut out = raw.to_vec();
        for f in filters {
            if f == b"ASCII85Decode" || f == b"A85" {
                out = ascii85(&out)?;
            } else if f == b"ASCIIHexDecode" || f == b"AHx" {
                let mut v = Vec::with_capacity(out.len() / 2);
                let digits: Vec<u8> = out.iter().copied().take_while(|&b| b != b'>').filter(|b| b.is_ascii_hexdigit()).collect();
                for p in digits.chunks(2) {
                    let h = |c: u8| (c as char).to_digit(16).unwrap() as u8;
                    v.push((h(p[0]) << 4) | if p.len() > 1 { h(p[1]) } else { 0 });
                }
                out = v;
            } else if f == b"LZWDecode" || f == b"LZW" {
                let early = !find(&dict, b"/EarlyChange 0", 0).is_some();
                out = lzw(&out, early);
            } else if f == b"RunLengthDecode" || f == b"RL" {
                out = run_length(&out);
            } else if f == b"FlateDecode" || f == b"Fl" {
                out = match miniz_oxide::inflate::decompress_to_vec_zlib(&out) {
                    Ok(v) => v,
                    Err(e) if !e.output.is_empty() => e.output,
                    Err(_) => miniz_oxide::inflate::decompress_to_vec(&out).ok()?,
                };
            } else {
                return None;
            }
        }
        Some(out)
    }

    fn pages(&self) -> Vec<u32> {
        let mut out = Vec::new();
        if let Some(root) = self.root() {
            if let Some(cat) = self.dict(root) {
                if let Some(Val::Ref(p)) = get(&cat, b"/Pages") {
                    let mut seen = std::collections::HashSet::new();
                    self.walk(p, &mut out, &mut seen, 0);
                }
            }
        }
        if out.is_empty() {
            let mut v: Vec<u32> = self.objs.keys().copied()
                .filter(|n| matches!(self.dict(*n).and_then(|d| get(&d, b"/Type")), Some(Val::Name(t)) if t == b"Page"))
                .collect();
            v.sort_unstable();
            out = v;
        }
        out.truncate(MAX_PAGES);
        out
    }

    fn walk(&self, n: u32, out: &mut Vec<u32>, seen: &mut std::collections::HashSet<u32>, depth: usize) {
        if depth > 32 || !seen.insert(n) || out.len() >= MAX_PAGES { return; }
        let d = match self.dict(n) { Some(d) => d, None => return };
        match get(&d, b"/Type") {
            Some(Val::Name(t)) if t == b"Pages" => {
                if let Some(Val::Array(kids)) = get(&d, b"/Kids") { for k in refs_in(&kids) { self.walk(k, out, seen, depth + 1); } }
            }
            _ => {
                if let Some(Val::Array(kids)) = get(&d, b"/Kids") { for k in refs_in(&kids) { self.walk(k, out, seen, depth + 1); } }
                else { out.push(n); }
            }
        }
    }

    fn root(&self) -> Option<u32> {
        // the last /Root wins (trailers of incremental updates, or an xref stream's dictionary)
        let p = rfind(self.data, b"/Root")?;
        match parse_val(self.data, p + 5) { Val::Ref(n) => Some(n), _ => None }
    }

    /// A page attribute, inherited through /Parent when missing.
    fn inherited(&self, page: u32, key: &[u8]) -> Option<Val> {
        let mut n = page;
        for _ in 0..16 {
            let d = self.dict(n)?;
            if let Some(v) = get(&d, key) { return Some(v); }
            match get(&d, b"/Parent") { Some(Val::Ref(p)) => n = p, _ => return None }
        }
        None
    }
}

impl<'a> Pdf<'a> {
    /// A value with an indirect reference replaced by the object it points to.
    fn direct(&self, v: Val) -> Val {
        match v {
            Val::Ref(n) => match self.dict(n) { Some(raw) => parse_val(&raw, 0), None => Val::Other },
            other => other,
        }
    }
}

// ---------- the content-stream interpreter ----------

type M = [f64; 6];
const IDENT: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
fn mul(a: &M, b: &M) -> M {
    [a[0] * b[0] + a[1] * b[2], a[0] * b[1] + a[1] * b[3],
     a[2] * b[0] + a[3] * b[2], a[2] * b[1] + a[3] * b[3],
     a[4] * b[0] + a[5] * b[2] + b[4], a[4] * b[1] + a[5] * b[3] + b[5]]
}
fn apply(m: &M, x: f64, y: f64) -> (f64, f64) { (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]) }
fn translate(tx: f64, ty: f64) -> M { [1.0, 0.0, 0.0, 1.0, tx, ty] }

/// Word and line rules, as fractions of the font size. Fixed; tuned on dev only, then frozen.
pub const WORD_GAP: f64 = 0.15;      // a gap along the baseline wider than this starts a new word
pub const BACKSTEP: f64 = 0.5;      // moving back further than this starts a new word (and a new line)
pub const BASELINE_SHIFT: f64 = 0.5; // a baseline moving more than this starts a new word and line

/// One drawn glyph. Boxes are in points from the top-left of the visible page (after CropBox and /Rotate).
#[derive(Clone, Debug)]
pub struct Glyph {
    /// The Unicode text the file gives for this glyph; empty when it gives none.
    pub text: String,
    pub mapped: bool,
    /// The character code as drawn.
    pub code: u32,
    /// Index into `Doc::fonts`, or u32::MAX when no font was set.
    pub font: u32,
    /// Drawn with render mode 3 or 7 (neither filled nor stroked), as OCR layers are.
    pub invisible: bool,
    /// Drawn by an annotation's appearance stream, not the page's content.
    pub annot: bool,
    /// The box's centre is outside the visible page.
    pub offpage: bool,
    pub x0: f64, pub y0: f64, pub x1: f64, pub y1: f64,
    /// The box with the glyph's outline added, where the outline reaches past it: italic overhang,
    /// capitals above the font's /Ascent. None when the outline is unknown or inside the box.
    pub ink: Option<[f64; 4]>,
    /// Baseline start, in page coordinates.
    pub ox: f64, pub oy: f64,
    /// Font size on the page, in points.
    pub size: f64,
    // in default user space, for grouping: baseline start and end, unit direction, and the glyph's quad
    ux: f64, uy: f64, ex: f64, ey: f64, dx: f64, dy: f64, quad: [(f64, f64); 4],
    ink_quad: Option<[(f64, f64); 4]>,
}

pub struct Word {
    pub text: String,
    pub x0: f64, pub y0: f64, pub x1: f64, pub y1: f64,
    pub line: usize,
    pub font: u32,
    pub size: f64,
    pub unmapped: usize,
    pub invisible: bool, pub annot: bool, pub offpage: bool,
    /// Index of the word's first glyph in `Page::glyphs`, and how many glyphs it has.
    pub first: usize, pub count: usize,
}

pub struct Page { pub n: usize, pub width: f64, pub height: f64, pub rotate: i64, pub glyphs: Vec<Glyph>, pub words: Vec<Word>, pub verdict: &'static str }

/// Share of glyphs a verdict needs: half of the visible glyphs invisible makes an OCR layer, and half
/// of the non-space glyphs undecodable makes a garbled text layer.
pub const VERDICT_SHARE: f64 = 0.5;

/// A glyph whose text doesn't decode to real characters: unmapped, U+FFFD, private use, or control.
fn undecodable(g: &Glyph) -> bool {
    !g.mapped || g.text.chars().any(|c| {
        let u = c as u32;
        c == '\u{fffd}' || (0xE000..=0xF8FF).contains(&u) || u >= 0xF0000 || (u < 0x20 && !matches!(c, '\t' | '\n' | '\r')) || (0x7F..0xA0).contains(&u)
    })
}

/// The page verdict, by fixed rules in this order: none, invisible, garbled, text.
fn verdict(glyphs: &[Glyph]) -> &'static str {
    let shown: Vec<&Glyph> = glyphs.iter().filter(|g| !g.offpage).collect();
    if shown.is_empty() { return "none"; }
    if shown.iter().filter(|g| g.invisible).count() as f64 >= VERDICT_SHARE * shown.len() as f64 { return "invisible"; }
    let ink: Vec<&&Glyph> = shown.iter().filter(|g| !is_space(g)).collect();
    if ink.is_empty() { return "none"; }
    if ink.iter().filter(|g| undecodable(g)).count() as f64 >= VERDICT_SHARE * ink.len() as f64 { return "garbled"; }
    "text"
}

pub struct FontInfo { pub base: String, pub kind: Kind, pub encoding: String, pub to_unicode: bool, pub embedded: bool, pub widths: &'static str }

pub struct Doc {
    /// "ok", "not_pdf", or "encrypted" (a password we can't open, or an unsupported handler).
    pub status: &'static str,
    pub pages: Vec<Page>,
    pub fonts: Vec<FontInfo>,
}

#[derive(Clone)]
struct GState { ctm: M, font: Option<u32>, size: f64, tc: f64, tw: f64, tz: f64, tl: f64, ts: f64, tr: i64 }

impl Default for GState {
    fn default() -> Self { GState { ctm: IDENT, font: None, size: 0.0, tc: 0.0, tw: 0.0, tz: 100.0, tl: 0.0, ts: 0.0, tr: 0 } }
}

/// Fonts are loaded once per object and shared by every page that uses them.
struct Fonts { by_obj: HashMap<u32, u32>, list: Vec<font::Font> }

impl Fonts {
    fn lookup(&mut self, pdf: &Pdf, resources: Option<&[u8]>, name: &[u8]) -> Option<u32> {
        let fonts = resources.and_then(|r| get(r, b"/Font")).and_then(|v| pdf.resolve(&v))?;
        let mut key = Vec::with_capacity(name.len() + 1);
        key.push(b'/');
        key.extend_from_slice(name);
        match get(&fonts, &key)? {
            Val::Ref(n) => {
                if let Some(&i) = self.by_obj.get(&n) { return Some(i); }
                let d = pdf.dict(n)?;
                self.list.push(font::Font::load(pdf, &d));
                let i = (self.list.len() - 1) as u32;
                self.by_obj.insert(n, i);
                Some(i)
            }
            Val::Dict(d) => { self.list.push(font::Font::load(pdf, &d)); Some((self.list.len() - 1) as u32) }
            _ => None,
        }
    }
}

enum Tok { Num(f64), Str(Vec<u8>), Name(Vec<u8>), Arr(Vec<Tok>), Other }

/// One operand starting at `i` (after whitespace), or None when an operator starts there.
fn operand(s: &[u8], i: &mut usize) -> Option<Tok> {
    let c = s[*i];
    match c {
        b'(' => { let e = skip_string(s, *i); let v = string_at(s, *i).unwrap_or_default(); *i = e; Some(Tok::Str(v)) }
        b'<' if s.get(*i + 1) == Some(&b'<') => { *i = matching(s, *i); Some(Tok::Other) }
        b'<' => {
            let e = find(s, b">", *i).unwrap_or(s.len());
            let v = string_at(s, *i).unwrap_or_default();
            *i = (e + 1).min(s.len());
            Some(Tok::Str(v))
        }
        b'[' => {
            *i += 1;
            let mut items = Vec::new();
            loop {
                *i = skip_ws(s, *i);
                if *i >= s.len() { break; }
                if s[*i] == b']' { *i += 1; break; }
                match operand(s, i) {
                    Some(t) => items.push(t),
                    None => { let mut j = *i; while j < s.len() && is_regular(s[j]) { j += 1; } *i = j.max(*i + 1); }
                }
            }
            Some(Tok::Arr(items))
        }
        b'/' => {
            let mut j = *i + 1;
            while j < s.len() && is_regular(s[j]) { j += 1; }
            let v = s[*i + 1..j].to_vec();
            *i = j;
            Some(Tok::Name(v))
        }
        b'0'..=b'9' | b'-' | b'+' | b'.' => {
            let mut j = *i + 1;
            while j < s.len() && (s[j].is_ascii_digit() || s[j] == b'.') { j += 1; }
            let v = std::str::from_utf8(&s[*i..j]).ok().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
            *i = j;
            Some(Tok::Num(v))
        }
        _ => None,
    }
}

fn num(t: &Tok) -> f64 { if let Tok::Num(v) = t { *v } else { 0.0 } }

fn matrix_of(pdf: &Pdf, d: &[u8]) -> M {
    match get(d, b"/Matrix").map(|v| pdf.direct(v)) {
        Some(Val::Array(a)) => { let v = nums_in(&a); if v.len() == 6 { [v[0], v[1], v[2], v[3], v[4], v[5]] } else { IDENT } }
        _ => IDENT,
    }
}

struct Run<'p, 'a> { pdf: &'p Pdf<'a>, fonts: Fonts, out: Vec<Glyph>, annot: bool }

impl<'p, 'a> Run<'p, 'a> {
    /// Show a string: one glyph per character code, each placed by the text rendering matrix
    /// (ISO 32000-1 9.4.4), then the text matrix advanced by the glyph's width.
    fn show(&mut self, g: &GState, tm: &mut M, bytes: &[u8]) {
        let invisible = g.tr == 3 || g.tr == 7;
        let th = g.tz / 100.0;
        let fid = match g.font { Some(f) => f, None => {
            // text shown before any font was set: it can't be decoded or measured
            for &b in bytes {
                let (x, y) = apply(&mul(tm, &g.ctm), 0.0, 0.0);
                self.out.push(blank(b as u32, invisible, self.annot, x, y));
            }
            return;
        } };
        let f = &self.fonts.list[fid as usize];
        let (desc, asc) = f.vertical();
        for (code, len) in f.codes(bytes) {
            let w0 = f.advance(code);
            let trm = mul(&mul(&[g.size * th, 0.0, 0.0, g.size, 0.0, g.ts], tm), &g.ctm);
            let quad = [apply(&trm, 0.0, desc), apply(&trm, w0, desc), apply(&trm, w0, asc), apply(&trm, 0.0, asc)];
            // the outline, where it reaches past that box. A Type3 glyph's d1 box counts as its outline
            // and goes into the ink box only, so the box stays the advance box. where-are-the-regions
            // grows the box itself instead; to do that here, grow (0, desc, w0, asc) by f.glyph_box(code)
            // before `quad` is built, and rescore, since x0..y1 of Type3 words would change.
            let ink_quad = f.outline_box(code).or_else(|| f.glyph_box(code)).filter(|b| b[0] < 0.0 || b[1] < desc || b[2] > w0 || b[3] > asc).map(|b| {
                let (ix0, iy0, ix1, iy1) = (b[0].min(0.0), b[1].min(desc), b[2].max(w0), b[3].max(asc));
                [apply(&trm, ix0, iy0), apply(&trm, ix1, iy0), apply(&trm, ix1, iy1), apply(&trm, ix0, iy1)]
            });
            let (ux, uy) = apply(&trm, 0.0, 0.0);
            let (ex, ey) = apply(&trm, w0, 0.0);
            let dl = (trm[0] * trm[0] + trm[1] * trm[1]).sqrt();
            let (dx, dy) = if dl > 1e-9 { (trm[0] / dl, trm[1] / dl) } else { (1.0, 0.0) };
            let size = (trm[2] * trm[2] + trm[3] * trm[3]).sqrt();
            let t = f.unicode(code);
            self.out.push(Glyph {
                mapped: t.is_some(), text: t.unwrap_or_default(), code, font: fid, invisible, annot: self.annot, offpage: false,
                x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0, ink: None, ox: 0.0, oy: 0.0, size, ux, uy, ex, ey, dx, dy, quad, ink_quad,
            });
            let mut tx = w0 * g.size + g.tc;
            if f.is_word_space(code, len) { tx += g.tw; }
            *tm = mul(&translate(tx * th, 0.0), tm);
        }
    }

    fn run(&mut self, content: &[u8], resources: Option<&[u8]>, gs0: GState, depth: usize) {
        let pdf = self.pdf;
        let xobjects = resources.and_then(|r| get(r, b"/XObject")).and_then(|v| pdf.resolve(&v));
        let mut g = gs0;
        let mut stack: Vec<GState> = Vec::new();
        let mut ops: Vec<Tok> = Vec::new();
        let (mut tm, mut tlm) = (IDENT, IDENT);
        let s = content;
        let mut i = 0;
        while i < s.len() {
            if is_ws(s[i]) { i += 1; continue; }
            if s[i] == b'%' { while i < s.len() && s[i] != b'\n' && s[i] != b'\r' { i += 1; } continue; }
            if let Some(t) = operand(s, &mut i) { ops.push(t); continue; }
            let mut j = i;
            while j < s.len() && is_regular(s[j]) { j += 1; }
            if j == i { i += 1; continue; }
            let op = &s[i..j];
            i = j;
            let n = ops.len();
            match op {
                b"true" | b"false" | b"null" => { ops.push(Tok::Other); continue; }
                b"q" => stack.push(g.clone()),
                b"Q" => { if let Some(x) = stack.pop() { g = x; } }
                b"cm" if n >= 6 => {
                    let m = [num(&ops[n - 6]), num(&ops[n - 5]), num(&ops[n - 4]), num(&ops[n - 3]), num(&ops[n - 2]), num(&ops[n - 1])];
                    g.ctm = mul(&m, &g.ctm);
                }
                b"BT" => { tm = IDENT; tlm = IDENT; }
                b"Tf" if n >= 2 => {
                    if let Tok::Name(nm) = &ops[n - 2] { g.font = self.fonts.lookup(pdf, resources, nm); }
                    g.size = num(&ops[n - 1]);
                }
                b"Tc" if n >= 1 => g.tc = num(&ops[n - 1]),
                b"Tw" if n >= 1 => g.tw = num(&ops[n - 1]),
                b"Tz" if n >= 1 => g.tz = num(&ops[n - 1]),
                b"TL" if n >= 1 => g.tl = num(&ops[n - 1]),
                b"Ts" if n >= 1 => g.ts = num(&ops[n - 1]),
                b"Tr" if n >= 1 => g.tr = num(&ops[n - 1]) as i64,
                b"Td" | b"TD" if n >= 2 => {
                    let (tx, ty) = (num(&ops[n - 2]), num(&ops[n - 1]));
                    if op == b"TD" { g.tl = -ty; }
                    tlm = mul(&translate(tx, ty), &tlm);
                    tm = tlm;
                }
                b"Tm" if n >= 6 => {
                    tlm = [num(&ops[n - 6]), num(&ops[n - 5]), num(&ops[n - 4]), num(&ops[n - 3]), num(&ops[n - 2]), num(&ops[n - 1])];
                    tm = tlm;
                }
                b"T*" => { tlm = mul(&translate(0.0, -g.tl), &tlm); tm = tlm; }
                b"Tj" if n >= 1 => { if let Tok::Str(b) = &ops[n - 1] { self.show(&g, &mut tm, b); } }
                b"'" if n >= 1 => {
                    tlm = mul(&translate(0.0, -g.tl), &tlm); tm = tlm;
                    if let Tok::Str(b) = &ops[n - 1] { self.show(&g, &mut tm, b); }
                }
                b"\"" if n >= 3 => {
                    g.tw = num(&ops[n - 3]); g.tc = num(&ops[n - 2]);
                    tlm = mul(&translate(0.0, -g.tl), &tlm); tm = tlm;
                    if let Tok::Str(b) = &ops[n - 1] { self.show(&g, &mut tm, b); }
                }
                b"TJ" if n >= 1 => {
                    if let Tok::Arr(items) = &ops[n - 1] {
                        for it in items {
                            match it {
                                Tok::Str(b) => self.show(&g, &mut tm, b),
                                // a number moves the pen back by thousandths of the font size
                                Tok::Num(v) => tm = mul(&translate(-v / 1000.0 * g.size * g.tz / 100.0, 0.0), &tm),
                                _ => {}
                            }
                        }
                    }
                }
                b"Do" if n >= 1 => {
                    if let (Tok::Name(nm), Some(x)) = (&ops[n - 1], &xobjects) { self.form(x, nm, &g, resources, depth); }
                }
                b"BI" => {
                    // inline image: skip its data up to a whitespace-delimited EI
                    let id = find(s, b"ID", i).unwrap_or(s.len());
                    let mut k = id + 2;
                    // ASCII-encoded data can hold "EI" itself, so skip to its end marker first
                    if let Some(end) = ascii_data_end(&s[i..id.min(s.len())]) {
                        if let Some(e) = find(s, end, k) { k = e + end.len(); }
                    }
                    loop {
                        match find(s, b"EI", k) {
                            Some(e) if (e == 0 || is_ws(s[e - 1])) && (e + 2 >= s.len() || !is_regular(s[e + 2])) => { k = e + 2; break; }
                            Some(e) => k = e + 2,
                            None => { k = s.len(); break; }
                        }
                    }
                    i = k;
                }
                _ => {}
            }
            ops.clear();
        }
    }

    fn form(&mut self, xobjects: &[u8], name: &[u8], g: &GState, resources: Option<&[u8]>, depth: usize) {
        if depth >= MAX_FORM_DEPTH { return; }
        let pdf = self.pdf;
        let mut key = Vec::with_capacity(name.len() + 1);
        key.push(b'/');
        key.extend_from_slice(name);
        let n = match get(xobjects, &key) { Some(Val::Ref(n)) => n, _ => return };
        let d = match pdf.dict(n) { Some(d) => d, None => return };
        if !matches!(get(&d, b"/Subtype"), Some(Val::Name(s)) if s == b"Form") { return; }
        let m = matrix_of(pdf, &d);
        let res = get(&d, b"/Resources").and_then(|v| pdf.resolve(&v));
        if let Some(body) = pdf.stream(n) {
            let mut gf = g.clone();
            gf.ctm = mul(&m, &g.ctm);
            self.run(&body, res.as_deref().or(resources), gf, depth + 1);
        }
    }

    /// Text drawn by the page's annotations: each shown annotation's normal appearance stream, placed
    /// on its /Rect (ISO 32000-1 12.5.5). Hidden and NoView annotations are skipped. A widget with a
    /// value but no appearance stream draws nothing, so it contributes nothing here.
    fn annotations(&mut self, page_dict: &[u8]) {
        let pdf = self.pdf;
        let list = match get(page_dict, b"/Annots").map(|v| pdf.direct(v)) { Some(Val::Array(a)) => refs_in(&a), _ => return };
        for r in list {
            let Some(a) = pdf.dict(r) else { continue };
            let flags = match get(&a, b"/F").map(|v| pdf.direct(v)) { Some(Val::Num(f)) => f as u32, _ => 0 };
            if flags & (2 | 32) != 0 { continue; }
            if matches!(get(&a, b"/Subtype"), Some(Val::Name(s)) if s == b"Popup") { continue; }
            let Some(ap) = get(&a, b"/AP").and_then(|v| pdf.resolve(&v)) else { continue };
            // /N is a stream, or a dictionary of appearance states chosen by /AS
            let n = match get(&ap, b"/N") {
                Some(Val::Ref(n)) if pdf.dict(n).map(|d| get(&d, b"/BBox").is_some()).unwrap_or(false) => n,
                Some(v) => {
                    let states = match pdf.resolve(&v) { Some(s) => s, None => continue };
                    let state = match get(&a, b"/AS") { Some(Val::Name(s)) => s, _ => continue };
                    let mut key = vec![b'/'];
                    key.extend_from_slice(&state);
                    match get(&states, &key) { Some(Val::Ref(n)) => n, _ => continue }
                }
                None => continue,
            };
            let Some(fd) = pdf.dict(n) else { continue };
            let bbox = nums_in(&match get(&fd, b"/BBox").map(|v| pdf.direct(v)) { Some(Val::Array(b)) => b, _ => continue });
            let rect = nums_in(&match get(&a, b"/Rect").map(|v| pdf.direct(v)) { Some(Val::Array(b)) => b, _ => continue });
            if bbox.len() != 4 || rect.len() != 4 { continue; }
            let m = matrix_of(pdf, &fd);
            let pts = [apply(&m, bbox[0], bbox[1]), apply(&m, bbox[2], bbox[1]), apply(&m, bbox[2], bbox[3]), apply(&m, bbox[0], bbox[3])];
            let (bx0, bx1) = (pts.iter().map(|p| p.0).fold(f64::MAX, f64::min), pts.iter().map(|p| p.0).fold(f64::MIN, f64::max));
            let (by0, by1) = (pts.iter().map(|p| p.1).fold(f64::MAX, f64::min), pts.iter().map(|p| p.1).fold(f64::MIN, f64::max));
            let (rx0, ry0, rx1, ry1) = (rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3]));
            if bx1 - bx0 < 1e-6 || by1 - by0 < 1e-6 { continue; }
            let (sx, sy) = ((rx1 - rx0) / (bx1 - bx0), (ry1 - ry0) / (by1 - by0));
            let fit = [sx, 0.0, 0.0, sy, rx0 - bx0 * sx, ry0 - by0 * sy];
            let res = get(&fd, b"/Resources").and_then(|v| pdf.resolve(&v));
            if let Some(body) = pdf.stream(n) {
                let g = GState { ctm: mul(&m, &fit), ..GState::default() };
                self.annot = true;
                self.run(&body, res.as_deref(), g, 1);
                self.annot = false;
            }
        }
    }
}

fn blank(code: u32, invisible: bool, annot: bool, x: f64, y: f64) -> Glyph {
    Glyph { text: String::new(), mapped: false, code, font: u32::MAX, invisible, annot, offpage: false,
            x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0, ink: None, ox: 0.0, oy: 0.0, size: 0.0,
            ux: x, uy: y, ex: x, ey: y, dx: 1.0, dy: 0.0, quad: [(x, y); 4], ink_quad: None }
}

/// The visible page: CropBox (clipped to MediaBox) and /Rotate, as a map from default user space to
/// points from the page's top-left corner as displayed.
struct PageBox { x0: f64, y0: f64, x1: f64, y1: f64, rotate: i64 }

impl PageBox {
    fn of(pdf: &Pdf, page: u32) -> PageBox {
        let rect = |key: &[u8]| match pdf.inherited(page, key).map(|v| pdf.direct(v)) {
            Some(Val::Array(a)) => { let v = nums_in(&a); if v.len() == 4 { Some((v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3]))) } else { None } }
            _ => None,
        };
        let media = rect(b"/MediaBox").unwrap_or((0.0, 0.0, 612.0, 792.0));
        let crop = rect(b"/CropBox").map(|c| (c.0.max(media.0), c.1.max(media.1), c.2.min(media.2), c.3.min(media.3)))
            .filter(|c| c.2 > c.0 && c.3 > c.1).unwrap_or(media);
        let rotate = match pdf.inherited(page, b"/Rotate").map(|v| pdf.direct(v)) { Some(Val::Num(r)) => ((r as i64 % 360) + 360) % 360, _ => 0 };
        let rotate = if rotate % 90 == 0 { rotate } else { 0 };
        PageBox { x0: crop.0, y0: crop.1, x1: crop.2, y1: crop.3, rotate }
    }
    fn size(&self) -> (f64, f64) {
        let (w, h) = (self.x1 - self.x0, self.y1 - self.y0);
        if self.rotate % 180 == 0 { (w, h) } else { (h, w) }
    }
    /// User space -> displayed page, top-left origin (the page turned clockwise by /Rotate).
    fn map(&self, x: f64, y: f64) -> (f64, f64) {
        let (w, h) = (self.x1 - self.x0, self.y1 - self.y0);
        let (u, v) = (x - self.x0, self.y1 - y);
        match self.rotate { 90 => (h - v, u), 180 => (w - u, h - v), 270 => (v, w - u), _ => (u, v) }
    }
}

fn place(glyphs: &mut [Glyph], pb: &PageBox) {
    let (w, h) = pb.size();
    for g in glyphs.iter_mut() {
        let pts: Vec<(f64, f64)> = g.quad.iter().map(|p| pb.map(p.0, p.1)).collect();
        g.x0 = pts.iter().map(|p| p.0).fold(f64::MAX, f64::min);
        g.x1 = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max);
        g.y0 = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min);
        g.y1 = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max);
        let (ox, oy) = pb.map(g.ux, g.uy);
        g.ox = ox;
        g.oy = oy;
        g.ink = g.ink_quad.map(|q| {
            let pts: Vec<(f64, f64)> = q.iter().map(|p| pb.map(p.0, p.1)).collect();
            [pts.iter().map(|p| p.0).fold(f64::MAX, f64::min), pts.iter().map(|p| p.1).fold(f64::MAX, f64::min),
             pts.iter().map(|p| p.0).fold(f64::MIN, f64::max), pts.iter().map(|p| p.1).fold(f64::MIN, f64::max)]
        });
        let (cx, cy) = ((g.x0 + g.x1) / 2.0, (g.y0 + g.y1) / 2.0);
        g.offpage = cx < 0.0 || cy < 0.0 || cx > w || cy > h;
    }
}

/// A word's ink box: its glyphs' boxes with their outlines added, cut to the visible page, when any
/// outline reaches past the word's own box (D101: the box and the off-page rule stay as they are).
fn word_ink(p: &Page, w: &Word) -> Option<[f64; 4]> {
    let gs = &p.glyphs[w.first..w.first + w.count];
    if gs.iter().all(|g| g.ink.is_none()) { return None; }
    let b = gs.iter().map(|g| g.ink.unwrap_or([g.x0, g.y0, g.x1, g.y1]))
        .fold([w.x0, w.y0, w.x1, w.y1], |a, k| [a[0].min(k[0]), a[1].min(k[1]), a[2].max(k[2]), a[3].max(k[3])]);
    let b = [b[0].max(0.0), b[1].max(0.0), b[2].min(p.width), b[3].min(p.height)];
    if b[2] <= b[0] || b[3] <= b[1] { return None; }
    let grown = r1(b[0]) != r1(w.x0) || r1(b[1]) != r1(w.y0) || r1(b[2]) != r1(w.x1) || r1(b[3]) != r1(w.y1);
    if grown { Some(b) } else { None }
}

fn is_space(g: &Glyph) -> bool { g.mapped && !g.text.is_empty() && g.text.chars().all(char::is_whitespace) }

/// Glyphs to words, and words to lines, by the fixed rules above. Drawing order is kept.
fn words(glyphs: &[Glyph]) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    let mut cur: Option<(usize, usize)> = None; // (first, last) glyph index of the open word
    let close = |out: &mut Vec<Word>, first: usize, last: usize| {
        let gs = &glyphs[first..=last];
        let text: String = gs.iter().map(|g| if g.mapped { g.text.as_str() } else { "\u{fffd}" }).collect();
        out.push(Word {
            text,
            x0: gs.iter().map(|g| g.x0).fold(f64::MAX, f64::min), y0: gs.iter().map(|g| g.y0).fold(f64::MAX, f64::min),
            x1: gs.iter().map(|g| g.x1).fold(f64::MIN, f64::max), y1: gs.iter().map(|g| g.y1).fold(f64::MIN, f64::max),
            line: 0, font: gs[0].font, size: gs[0].size,
            unmapped: gs.iter().filter(|g| !g.mapped).count(),
            invisible: gs[0].invisible, annot: gs[0].annot, offpage: gs.iter().all(|g| g.offpage),
            first, count: last - first + 1,
        });
    };
    for (i, g) in glyphs.iter().enumerate() {
        if is_space(g) {
            if let Some((f, l)) = cur.take() { close(&mut out, f, l); }
            continue;
        }
        if let Some((f, l)) = cur {
            let p = &glyphs[l];
            let size = p.size.max(g.size).max(1e-6);
            let (vx, vy) = (g.ux - p.ex, g.uy - p.ey);
            let along = vx * p.dx + vy * p.dy;
            let across = (p.dx * vy - p.dy * vx).abs();
            let same_dir = p.dx * g.dx + p.dy * g.dy > 0.99;
            let same_kind = p.invisible == g.invisible && p.annot == g.annot;
            if !same_dir || !same_kind || across > BASELINE_SHIFT * size || along > WORD_GAP * size || along < -BACKSTEP * size {
                close(&mut out, f, l);
                cur = Some((i, i));
            } else {
                cur = Some((f, i));
            }
        } else {
            cur = Some((i, i));
        }
    }
    if let Some((f, l)) = cur { close(&mut out, f, l); }
    // lines: same direction and baseline (within half the font size), and not moving backwards
    let mut line = 0;
    for k in 1..out.len() {
        let (a, b) = (&glyphs[out[k - 1].first + out[k - 1].count - 1], &glyphs[out[k].first]);
        let start = &glyphs[out[k - 1].first];
        let size = a.size.max(b.size).max(1e-6);
        let (vx, vy) = (b.ux - start.ux, b.uy - start.uy);
        let across = (start.dx * vy - start.dy * vx).abs();
        let along = (b.ux - a.ex) * a.dx + (b.uy - a.ey) * a.dy;
        let same_dir = a.dx * b.dx + a.dy * b.dy > 0.99;
        if !same_dir || across > BASELINE_SHIFT * size || along < -BACKSTEP * size { line += 1; }
        out[k].line = line;
    }
    out
}

fn page_content(pdf: &Pdf, page: u32) -> (Vec<u8>, Option<Vec<u8>>) {
    let resources = pdf.inherited(page, b"/Resources").and_then(|v| pdf.resolve(&v));
    let d = match pdf.dict(page) { Some(d) => d, None => return (Vec::new(), resources) };
    // /Contents is a stream, an array of streams, or a reference to an array object holding them
    let refs = match get(&d, b"/Contents") {
        Some(Val::Ref(n)) => match pdf.direct(Val::Ref(n)) { Val::Array(a) => refs_in(&a), _ => vec![n] },
        Some(Val::Array(a)) => refs_in(&a),
        _ => Vec::new(),
    };
    let mut content = Vec::new();
    for r in refs {
        if let Some(b) = pdf.stream(r) { content.extend_from_slice(&b); content.push(b'\n'); }
    }
    (content, resources)
}

/// The end marker of an inline image's data when its outer filter is ASCII85 (`~>`) or ASCIIHex (`>`).
fn ascii_data_end(dict: &[u8]) -> Option<&'static [u8]> {
    let has = |k: &[u8]| { let mut f = 0; while let Some(p) = find(dict, k, f) { let e = p + k.len(); if e >= dict.len() || !is_regular(dict[e]) { return true; } f = e; } false };
    if has(b"/A85") || has(b"/ASCII85Decode") { Some(b"~>") } else if has(b"/AHx") || has(b"/ASCIIHexDecode") { Some(b">") } else { None }
}

/// Extract every glyph and word from a PDF's bytes.
pub fn extract(data: &[u8]) -> Doc {
    if !data.starts_with(b"%PDF") && find(&data[..data.len().min(1024)], b"%PDF", 0).is_none() {
        return Doc { status: "not_pdf", pages: Vec::new(), fonts: Vec::new() };
    }
    let pdf = Pdf::index(data);
    if find(data, b"/Encrypt", 0).is_some() && pdf.crypt.is_none() {
        return Doc { status: "encrypted", pages: Vec::new(), fonts: Vec::new() };
    }
    let mut r = Run { pdf: &pdf, fonts: Fonts { by_obj: HashMap::new(), list: Vec::new() }, out: Vec::new(), annot: false };
    let mut pages = Vec::new();
    for (k, p) in pdf.pages().iter().enumerate() {
        let (content, resources) = page_content(&pdf, *p);
        r.run(&content, resources.as_deref(), GState::default(), 0);
        if let Some(d) = pdf.dict(*p) { r.annotations(&d); }
        let pb = PageBox::of(&pdf, *p);
        let mut glyphs = std::mem::take(&mut r.out);
        place(&mut glyphs, &pb);
        let ws = words(&glyphs);
        let (width, height) = pb.size();
        let v = verdict(&glyphs);
        pages.push(Page { n: k + 1, width, height, rotate: pb.rotate, glyphs, words: ws, verdict: v });
    }
    let fonts = r.fonts.list.iter().map(|f| FontInfo {
        base: f.base.clone(), kind: f.kind, encoding: f.encoding.clone(), to_unicode: f.has_to_unicode(), embedded: f.embedded, widths: f.metrics.source,
    }).collect();
    Doc { status: "ok", pages, fonts }
}

// ---------- JSON ----------

pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn r1(v: f64) -> f64 { (v * 10.0).round() / 10.0 }

fn flags(invisible: bool, annot: bool, offpage: bool) -> String {
    let mut s = String::new();
    if invisible { s.push_str(",\"invisible\":true"); }
    if annot { s.push_str(",\"annot\":true"); }
    if offpage { s.push_str(",\"offpage\":true"); }
    s
}

impl Doc {
    fn fonts_json(&self) -> String {
        let fonts: Vec<String> = self.fonts.iter().map(|f| format!(
            "{{\"base\":{},\"kind\":\"{}\",\"encoding\":{},\"to_unicode\":{},\"embedded\":{},\"widths\":\"{}\"}}",
            json_str(&f.base), f.kind.as_str(), json_str(&f.encoding), f.to_unicode, f.embedded, f.widths
        )).collect();
        fonts.join(",")
    }

    /// The output: per page, its displayed size and every word with its box, in drawing order.
    /// With `glyphs`, each page also lists every glyph with its box and baseline start.
    pub fn to_json(&self, glyphs: bool) -> String {
        let pages: Vec<String> = self.pages.iter().map(|p| {
            let words: Vec<String> = p.words.iter().map(|w| format!(
                "{{\"t\":{},\"x0\":{},\"y0\":{},\"x1\":{},\"y1\":{},\"b\":{},\"line\":{},\"font\":{},\"size\":{}{}{}{}}}",
                json_str(&w.text), r1(w.x0), r1(w.y0), r1(w.x1), r1(w.y1), r1(p.glyphs[w.first].oy), w.line,
                if w.font == u32::MAX { -1 } else { w.font as i64 }, r1(w.size),
                match word_ink(p, w) { Some(k) => format!(",\"ink\":[{},{},{},{}]", r1(k[0]), r1(k[1]), r1(k[2]), r1(k[3])), None => String::new() },
                if w.unmapped > 0 { format!(",\"unmapped\":{}", w.unmapped) } else { String::new() },
                flags(w.invisible, w.annot, w.offpage)
            )).collect();
            let gl = if glyphs {
                let g: Vec<String> = p.glyphs.iter().map(|g| format!(
                    "{{\"c\":{},\"x0\":{},\"y0\":{},\"x1\":{},\"y1\":{},\"ox\":{},\"oy\":{},\"size\":{}{}{}}}",
                    json_str(if g.mapped { &g.text } else { "\u{fffd}" }), r1(g.x0), r1(g.y0), r1(g.x1), r1(g.y1), r1(g.ox), r1(g.oy), r1(g.size),
                    if g.mapped { "" } else { ",\"unmapped\":true" }, flags(g.invisible, g.annot, g.offpage)
                )).collect();
                format!(",\"glyphs\":[{}]", g.join(","))
            } else { String::new() };
            let unmapped = p.glyphs.iter().filter(|g| !g.mapped).count();
            format!("{{\"n\":{},\"width\":{},\"height\":{},\"rotate\":{},\"verdict\":\"{}\",\"glyph_count\":{},\"unmapped\":{},\"words\":[{}]{}}}",
                p.n, r1(p.width), r1(p.height), p.rotate, p.verdict, p.glyphs.len(), unmapped, words.join(","), gl)
        }).collect();
        format!("{{\"status\":\"{}\",\"pages\":[{}],\"fonts\":[{}]}}", self.status, pages.join(","), self.fonts_json())
    }

    /// Per page: the decoded text in drawing order (unmapped glyphs as U+FFFD) and counts.
    /// Used by tools/decode_check.py.
    pub fn text_json(&self) -> String {
        let pages: Vec<String> = self.pages.iter().map(|p| {
            let text: String = p.glyphs.iter().map(|g| if g.mapped { g.text.as_str() } else { "\u{fffd}" }).collect();
            let unmapped = p.glyphs.iter().filter(|g| !g.mapped).count();
            let invisible = p.glyphs.iter().filter(|g| g.invisible).count();
            let mut by_font: Vec<(u32, usize)> = Vec::new();
            for g in p.glyphs.iter().filter(|g| !g.mapped) {
                match by_font.iter_mut().find(|e| e.0 == g.font) { Some(e) => e.1 += 1, None => by_font.push((g.font, 1)) }
            }
            let uf: Vec<String> = by_font.iter().map(|(f, c)| format!("[{},{}]", if *f == u32::MAX { -1 } else { *f as i64 }, c)).collect();
            format!("{{\"n\":{},\"glyphs\":{},\"unmapped\":{},\"unmapped_by_font\":[{}],\"invisible\":{},\"text\":{}}}",
                p.n, p.glyphs.len(), unmapped, uf.join(","), invisible, json_str(&text))
        }).collect();
        format!("{{\"status\":\"{}\",\"pages\":[{}],\"fonts\":[{}]}}", self.status, pages.join(","), self.fonts_json())
    }
}

#[cfg(test)]
mod tests {
    /// A one-page PDF with Helvetica as /F1 and the given content stream.
    fn pdf(content: &str) -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offs = Vec::new();
        for (k, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out += &format!("{} 0 obj\n{}\nendobj\n", k + 1, o);
        }
        let x = out.len();
        out += &format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1);
        for o in offs { out += &format!("{:010} 00000 n \n", o); }
        out += &format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n", objs.len() + 1, x);
        out.into_bytes()
    }

    fn near_box(x0: f64, y0: f64, x1: f64, y1: f64, b: [f64; 4]) -> bool {
        (x0 - b[0]).abs() < 1e-6 && (y0 - b[1]).abs() < 1e-6 && (x1 - b[2]).abs() < 1e-6 && (y1 - b[3]).abs() < 1e-6
    }

    #[test]
    fn a_glyph_whose_outline_reaches_out_gets_an_ink_box() {
        // 10 pt at (100, 700): advance box x 100..105, ascent 800 and descent -200 give y 92..102 from the
        // top; the outline adds x 99..107 and reaches y 91. The word's JSON carries both.
        let doc = super::extract(&crate::font::tiny_truetype_pdf(Some("BT /F1 10 Tf 100 700 Td (A) Tj ET")));
        let g = &doc.pages[0].glyphs[0];
        assert!(near_box(g.x0, g.y0, g.x1, g.y1, [100.0, 92.0, 105.0, 102.0]), "{:?}", (g.x0, g.y0, g.x1, g.y1));
        let k = g.ink.unwrap();
        assert!(near_box(k[0], k[1], k[2], k[3], [99.0, 91.0, 107.0, 102.0]), "{k:?}");
        assert!(doc.to_json(false).contains("\"x0\":100,\"y0\":92,\"x1\":105,\"y1\":102,"));
        assert!(doc.to_json(false).contains("\"ink\":[99,91,107,102]"));
    }

    #[test]
    fn an_ink_box_is_cut_to_the_page_and_the_offpage_rule_stays() {
        // at x -3 the advance box is -3..2 with its centre off the page: still offpage (wordbox's rule),
        // and the word's ink box (-4..4) is cut to 0..4 (D101)
        let doc = super::extract(&crate::font::tiny_truetype_pdf(Some("BT /F1 10 Tf -3 700 Td (A) Tj ET")));
        let (g, w) = (&doc.pages[0].glyphs[0], &doc.pages[0].words[0]);
        assert!(g.offpage && w.offpage && near_box(w.x0, w.y0, w.x1, w.y1, [-3.0, 92.0, 2.0, 102.0]));
        assert!(doc.to_json(false).contains("\"ink\":[0,91,4,102]"));
    }

    #[test]
    fn a_type3_glyph_drawn_past_a_zero_width_gets_an_ink_box() {
        // width 0, d1 box 0 -50 500 700, /FontBBox 0 -100 600 800, 10 pt at (100, 700) on an 800 pt page:
        // the box stays the zero-width advance box (x 100, y 92..101), the d1 box goes into "ink"
        let proc = "0 0 0 -50 500 700 d1\n0 0 500 700 re f\n";
        let pdf = format!("%PDF-1.4\n1 0 obj << /Type /Font /Subtype /Type3 /FontBBox [0 -100 600 800] /FontMatrix [0.001 0 0 0.001 0 0] /FirstChar 65 /LastChar 65 /Widths [0] /Encoding << /Differences [65 /g1] >> /CharProcs << /g1 2 0 R >> >> endobj\n2 0 obj << /Length {} >> stream\n{}endstream endobj\n3 0 obj << /Type /Catalog /Pages 4 0 R >> endobj\n4 0 obj << /Type /Pages /Kids [5 0 R] /Count 1 >> endobj\n5 0 obj << /Type /Page /Parent 4 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 1 0 R >> >> /Contents 6 0 R >> endobj\n6 0 obj << /Length 34 >> stream\nBT /F1 10 Tf 100 700 Td (A) Tj ET\nendstream endobj\ntrailer << /Root 3 0 R >>\n%%EOF\n", proc.len(), proc);
        let doc = super::extract(pdf.as_bytes());
        let json = doc.to_json(false);
        assert!(json.contains("\"x0\":100,\"y0\":92,\"x1\":100,\"y1\":101,"), "{json}");
        assert!(json.contains("\"ink\":[100,92,105,101]"), "{json}");
    }

    #[test]
    fn ascii85_inline_data_holding_ei() {
        // the A85 data has a line starting "EI(": stopping there opens a string that swallows the text after it
        let d = super::extract(&pdf("BI /W 2 /H 1 /CS /G /BPC 8 /F /A85 ID ab\nEI(cd~> EI BT /F1 12 Tf 10 40 Td (Hello) Tj ET\nBI /W 2 /H 1 /CS /G /BPC 8 /F [/AHx /Fl] ID 0E\nEI(> EI BT /F1 12 Tf 10 20 Td (World) Tj ET"));
        let words: Vec<&str> = d.pages[0].words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(words, ["Hello", "World"]);
    }
}
