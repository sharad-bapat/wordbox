//! CMap parsing: ToUnicode maps (character code -> Unicode) and encoding CMaps (code -> CID),
//! plus their code-space ranges, which say how many bytes make one character code.
//! ISO 32000-1 9.7.5 and 9.10.3, and Adobe Technical Note #5014.
use std::collections::HashMap;

/// One code-space range: codes of `len` bytes whose bytes each fall within lo..=hi.
#[derive(Clone, Debug)]
pub struct Space { pub len: usize, lo: [u8; 4], hi: [u8; 4] }

#[derive(Clone, Debug)]
enum Dst { Seq(Vec<u16>), List(Vec<String>) }

#[derive(Clone, Debug, Default)]
pub struct CMap {
    pub spaces: Vec<Space>,
    single: HashMap<u32, String>,
    ranges: Vec<(u32, u32, Dst)>,
    cid_single: HashMap<u32, u32>,
    cid_ranges: Vec<(u32, u32, u32)>,
}

enum Tok { Hex(Vec<u8>), Int(i64), Name(Vec<u8>), Open, Close, Word(Vec<u8>) }

fn tokens(s: &[u8]) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b' ' | b'\n' | b'\r' | b'\t' | 0x0c | 0 => i += 1,
            b'%' => { while i < s.len() && s[i] != b'\n' && s[i] != b'\r' { i += 1; } }
            b'<' if s.get(i + 1) == Some(&b'<') => { out.push(Tok::Word(b"<<".to_vec())); i += 2; }
            b'>' if s.get(i + 1) == Some(&b'>') => { out.push(Tok::Word(b">>".to_vec())); i += 2; }
            b'<' => {
                let mut j = i + 1;
                let mut hex = Vec::new();
                while j < s.len() && s[j] != b'>' { if s[j].is_ascii_hexdigit() { hex.push(s[j]); } j += 1; }
                if hex.len() % 2 == 1 { hex.push(b'0'); }
                let v = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
                out.push(Tok::Hex(hex.chunks(2).map(|p| (v(p[0]) << 4) | v(p[1])).collect()));
                i = j + 1;
            }
            b'[' => { out.push(Tok::Open); i += 1; }
            b']' => { out.push(Tok::Close); i += 1; }
            b'(' => {
                // literal strings only appear in CMap headers (registry names); skip them
                let mut depth = 0;
                while i < s.len() {
                    match s[i] { b'\\' => i += 1, b'(' => depth += 1, b')' => { depth -= 1; if depth == 0 { i += 1; break; } } _ => {} }
                    i += 1;
                }
            }
            b'/' => {
                let mut j = i + 1;
                while j < s.len() && !b" \n\r\t\x0c\x00/[]<>(){}%".contains(&s[j]) { j += 1; }
                out.push(Tok::Name(s[i + 1..j].to_vec()));
                i = j;
            }
            _ => {
                let mut j = i;
                while j < s.len() && !b" \n\r\t\x0c\x00/[]<>(){}%".contains(&s[j]) { j += 1; }
                if j == i { i += 1; continue; }
                let w = &s[i..j];
                match std::str::from_utf8(w).ok().and_then(|x| x.parse::<i64>().ok()) {
                    Some(n) => out.push(Tok::Int(n)),
                    None => out.push(Tok::Word(w.to_vec())),
                }
                i = j;
            }
        }
    }
    out
}

fn be(b: &[u8]) -> u32 { b.iter().take(4).fold(0u32, |a, &x| (a << 8) | x as u32) }

/// UTF-16BE bytes to code units.
fn units(b: &[u8]) -> Vec<u16> { b.chunks(2).map(|p| if p.len() == 2 { u16::from_be_bytes([p[0], p[1]]) } else { p[0] as u16 }).collect() }

fn utf16(u: &[u16]) -> String { char::decode_utf16(u.iter().copied()).map(|r| r.unwrap_or('\u{fffd}')).collect() }

impl CMap {
    pub fn parse(s: &[u8], name_to_unicode: impl Fn(&[u8]) -> Option<String>) -> CMap {
        let t = tokens(s);
        let mut m = CMap::default();
        let mut i = 0;
        while i < t.len() {
            let word = match &t[i] { Tok::Word(w) => w.as_slice(), _ => { i += 1; continue; } };
            i += 1;
            match word {
                b"begincodespacerange" => {
                    while i + 1 < t.len() {
                        match (&t[i], &t[i + 1]) {
                            (Tok::Hex(lo), Tok::Hex(hi)) if !lo.is_empty() && lo.len() == hi.len() && lo.len() <= 4 => {
                                let mut sp = Space { len: lo.len(), lo: [0; 4], hi: [0; 4] };
                                sp.lo[..lo.len()].copy_from_slice(lo);
                                sp.hi[..hi.len()].copy_from_slice(hi);
                                m.spaces.push(sp);
                                i += 2;
                            }
                            _ => break,
                        }
                    }
                }
                b"beginbfchar" => {
                    while i + 1 < t.len() {
                        let src = match &t[i] { Tok::Hex(h) => be(h), _ => break };
                        let dst = match &t[i + 1] {
                            Tok::Hex(h) => Some(utf16(&units(h))),
                            Tok::Name(n) => name_to_unicode(n),
                            _ => break,
                        };
                        if let Some(d) = dst { m.single.insert(src, d); }
                        i += 2;
                    }
                }
                b"beginbfrange" => {
                    while i + 2 < t.len() {
                        let (lo, hi) = match (&t[i], &t[i + 1]) { (Tok::Hex(a), Tok::Hex(b)) => (be(a), be(b)), _ => break };
                        i += 2;
                        match &t[i] {
                            Tok::Hex(h) => { if hi >= lo { m.ranges.push((lo, hi, Dst::Seq(units(h)))); } i += 1; }
                            Tok::Open => {
                                i += 1;
                                let mut list = Vec::new();
                                while i < t.len() {
                                    match &t[i] {
                                        Tok::Hex(h) => list.push(utf16(&units(h))),
                                        Tok::Name(n) => list.push(name_to_unicode(n).unwrap_or_default()),
                                        _ => break,
                                    }
                                    i += 1;
                                }
                                if matches!(t.get(i), Some(Tok::Close)) { i += 1; }
                                if hi >= lo { m.ranges.push((lo, hi, Dst::List(list))); }
                            }
                            _ => break,
                        }
                    }
                }
                b"begincidchar" => {
                    while i + 1 < t.len() {
                        match (&t[i], &t[i + 1]) {
                            (Tok::Hex(h), Tok::Int(c)) => { m.cid_single.insert(be(h), *c as u32); i += 2; }
                            _ => break,
                        }
                    }
                }
                b"begincidrange" => {
                    while i + 2 < t.len() {
                        match (&t[i], &t[i + 1], &t[i + 2]) {
                            (Tok::Hex(a), Tok::Hex(b), Tok::Int(c)) => { m.cid_ranges.push((be(a), be(b), *c as u32)); i += 3; }
                            _ => break,
                        }
                    }
                }
                _ => {}
            }
        }
        m.ranges.sort_by_key(|r| r.0);
        m
    }

    /// Unicode for a character code, if this map has it.
    pub fn unicode(&self, code: u32) -> Option<String> {
        if let Some(s) = self.single.get(&code) { return Some(s.clone()); }
        // ranges are sorted by start; overlapping ranges are rare, so check from the last start <= code
        let k = self.ranges.partition_point(|r| r.0 <= code);
        for (lo, hi, dst) in self.ranges[..k].iter().rev() {
            if code > *hi { continue; }
            let off = code - lo;
            return match dst {
                // the last code unit counts up through the range (Adobe TN 5014, "bfrange")
                Dst::Seq(u) if !u.is_empty() => {
                    let mut v = u.clone();
                    let last = v.len() - 1;
                    v[last] = v[last].wrapping_add(off as u16);
                    Some(utf16(&v))
                }
                Dst::Seq(_) => None,
                Dst::List(l) => l.get(off as usize).cloned(),
            };
        }
        None
    }

    /// CID for a character code, from an encoding CMap.
    pub fn cid(&self, code: u32) -> Option<u32> {
        if let Some(c) = self.cid_single.get(&code) { return Some(*c); }
        self.cid_ranges.iter().find(|r| r.0 <= code && code <= r.1).map(|r| r.2 + (code - r.0))
    }

    pub fn has_unicode(&self) -> bool { !self.single.is_empty() || !self.ranges.is_empty() }
}

/// Split a string's bytes into character codes using code-space ranges (ISO 32000-1 9.7.6.2).
/// Returns (code, byte length) pairs. With no ranges, codes are `default_len` bytes.
pub fn split(spaces: &[Space], default_len: usize, bytes: &[u8]) -> Vec<(u32, usize)> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    if spaces.is_empty() {
        let n = default_len.max(1);
        while i < bytes.len() {
            let e = (i + n).min(bytes.len());
            out.push((be(&bytes[i..e]), e - i));
            i = e;
        }
        return out;
    }
    let shortest = spaces.iter().map(|s| s.len).min().unwrap_or(1);
    while i < bytes.len() {
        let mut hit = None;
        'len: for len in 1..=4 {
            if i + len > bytes.len() { break; }
            for sp in spaces.iter().filter(|s| s.len == len) {
                if (0..len).all(|k| sp.lo[k] <= bytes[i + k] && bytes[i + k] <= sp.hi[k]) { hit = Some(len); break 'len; }
            }
        }
        // no range matched: take the shortest range's length, as viewers do
        let len = hit.unwrap_or(shortest).min(bytes.len() - i);
        out.push((be(&bytes[i..i + len]), len));
        i += len;
    }
    out
}
