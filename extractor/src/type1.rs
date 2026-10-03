//! Embedded Type 1 font programs (/FontFile), read just far enough to box each glyph's outline for the
//! ink box: Adobe Type 1 Font Format 1.1. The program is cleartext, then the private part
//! encrypted with eexec (binary or hex); its /Subrs and /CharStrings are encrypted again. A glyph's box
//! is the box of its outline's points, control points included, as FreeType's control box is. A part
//! that can't be read gives no glyphs, and those glyphs keep their advance box. Ported from
//! where-are-the-regions.
use std::collections::HashMap;

use crate::tables;

pub struct Type1 {
    /// Font units to text space along x and y, from /FontMatrix (0.001 for nearly every font).
    pub scale: (f64, f64),
    /// Glyph names and their decrypted charstrings, in program order (the index is the glyph id).
    glyphs: Vec<(String, Vec<u8>)>,
    by_name: HashMap<String, usize>,
    subrs: Vec<Vec<u8>>,
    /// The program's own encoding, code -> glyph name; None means StandardEncoding.
    encoding: Option<Vec<Option<String>>>,
}

fn decrypt(data: &[u8], mut r: u16, skip: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        out.push(c ^ (r >> 8) as u8);
        r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
    }
    out.into_iter().skip(skip).collect()
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() { return None; }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

fn is_ws(b: u8) -> bool { matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | 0) }

/// The next whitespace-separated token at or after `i`, and where it ends.
fn token(s: &[u8], mut i: usize) -> Option<(&[u8], usize)> {
    while i < s.len() && is_ws(s[i]) { i += 1; }
    let start = i;
    while i < s.len() && !is_ws(s[i]) { i += 1; }
    if start == i { None } else { Some((&s[start..i], i)) }
}

fn int_of(t: &[u8]) -> Option<i64> { std::str::from_utf8(t).ok()?.parse().ok() }

/// After "<len> RD " (or "-|"): the `len` binary bytes that follow the single space, and where they end.
fn binary_after(s: &[u8], i: usize, len: usize) -> Option<(&[u8], usize)> {
    let (_rd, j) = token(s, i)?;
    let start = j + 1;
    let end = start.checked_add(len)?;
    Some((s.get(start..end)?, end))
}

impl Type1 {
    pub fn parse(program: &[u8]) -> Option<Type1> {
        let e = find(program, b"eexec", 0)?;
        let clear = &program[..e];
        let mut i = e + 5;
        while i < program.len() && is_ws(program[i]) { i += 1; }
        let rest = &program[i..];
        // hex when the first four bytes are hex digits (ISO 32000-1 9.9 allows either)
        let cipher: Vec<u8> = if rest.len() >= 4 && rest[..4].iter().all(|b| b.is_ascii_hexdigit()) {
            let digits: Vec<u8> = rest.iter().copied().filter(|b| !is_ws(*b)).take_while(|b| b.is_ascii_hexdigit()).collect();
            digits.chunks(2).filter(|p| p.len() == 2).filter_map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok()).collect()
        } else { rest.to_vec() };
        let private = decrypt(&cipher, 55665, 4);

        let scale = find(clear, b"/FontMatrix", 0).and_then(|k| {
            let a = find(clear, b"[", k)?;
            let b = find(clear, b"]", a)?;
            let v: Vec<f64> = std::str::from_utf8(&clear[a + 1..b]).ok()?.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if v.len() == 6 && v[0] != 0.0 && v[3] != 0.0 { Some((v[0], v[3])) } else { None }
        }).unwrap_or((0.001, 0.001));

        let encoding = if find(clear, b"/Encoding StandardEncoding", 0).is_some() { None } else {
            find(clear, b"/Encoding", 0).map(|k| {
                let mut enc = vec![None; 256];
                let mut j = k;
                while let Some(d) = find(clear, b"dup ", j) {
                    let Some((code, a)) = token(clear, d + 4) else { break };
                    let Some((name, b)) = token(clear, a) else { break };
                    if let (Some(c), Some(n)) = (int_of(code), name.strip_prefix(b"/")) {
                        if (0..256).contains(&c) { enc[c as usize] = Some(String::from_utf8_lossy(n).into_owned()); }
                    }
                    j = b;
                }
                enc
            })
        };

        let len_iv = find(&private, b"/lenIV", 0).and_then(|k| token(&private, k + 6)).and_then(|(t, _)| int_of(t)).unwrap_or(4);
        let cs = |b: &[u8]| if len_iv < 0 { b.to_vec() } else { decrypt(b, 4330, len_iv as usize) };

        let mut subrs = Vec::new();
        if let Some((n, mut j)) = find(&private, b"/Subrs", 0).and_then(|k| token(&private, k + 6)).and_then(|(t, j)| Some((int_of(t)?, j))) {
            subrs = vec![Vec::new(); n.clamp(0, 65_535) as usize];
            // "/Subrs 287 array" then "dup 0 15 RD ..."
            if let Some((b"array", a)) = token(&private, j).map(|(t, a)| (t, a)) { j = a; }
            for _ in 0..subrs.len() {
                let Some((t, a)) = token(&private, j) else { break };
                if t != b"dup" { break; }
                let Some((idx, b)) = token(&private, a).and_then(|(t, b)| Some((int_of(t)?, b))) else { break };
                let Some((len, c)) = token(&private, b).and_then(|(t, c)| Some((int_of(t)?, c))) else { break };
                let Some((bytes, d)) = binary_after(&private, c, len.max(0) as usize) else { break };
                if let Some(slot) = subrs.get_mut(idx.max(0) as usize) { *slot = cs(bytes); }
                // skip "NP" (or "|", "noaccess put")
                j = d;
                while let Some((t, e)) = token(&private, j) { if t == b"dup" || t == b"ND" || t == b"|-" || t == b"end" || t.starts_with(b"/") { break; } j = e; }
            }
        }

        let mut glyphs = Vec::new();
        let k = find(&private, b"/CharStrings", 0)?;
        let mut j = k + 12;
        while let Some((t, a)) = token(&private, j) {
            if t == b"end" { break; }
            if let Some(name) = t.strip_prefix(b"/") {
                let Some((len, b)) = token(&private, a).and_then(|(t, b)| Some((int_of(t)?, b))) else { j = a; continue };
                let Some((bytes, c)) = binary_after(&private, b, len.max(0) as usize) else { break };
                glyphs.push((String::from_utf8_lossy(name).into_owned(), cs(bytes)));
                j = c;
            } else {
                j = a;
            }
        }
        if glyphs.is_empty() { return None; }
        let mut by_name = HashMap::new();
        for (i, (n, _)) in glyphs.iter().enumerate() { by_name.entry(n.clone()).or_insert(i); }
        Some(Type1 { scale, glyphs, by_name, subrs, encoding })
    }

    pub fn glyph_count(&self) -> usize { self.glyphs.len() }

    pub fn name(&self, i: usize) -> &str { &self.glyphs[i].0 }

    pub fn index_of(&self, name: &str) -> Option<usize> { self.by_name.get(name).copied() }

    /// The glyph for a code in the program's own encoding; StandardEncoding goes through its Unicode.
    pub fn builtin(&self, code: u32, by_unicode: impl Fn(&str) -> Option<usize>) -> Option<usize> {
        match &self.encoding {
            Some(e) => self.index_of(e.get(code as usize)?.as_deref()?),
            None => {
                let u = *tables::STANDARD.get(code as usize)?;
                if u == 0 { return None; }
                by_unicode(&char::from_u32(u as u32)?.to_string())
            }
        }
    }

    /// The box of a glyph's outline points, in font units (x0, y0, x1, y1); None for an empty glyph.
    pub fn bounds(&self, i: usize) -> Option<[f64; 4]> {
        let mut st = Run { t1: self, b: None, x: 0.0, y: 0.0, sbx: 0.0, pending: None, flex: false, ps: Vec::new(), stack: Vec::new(), done: false };
        st.exec(&self.glyphs.get(i)?.1, 0, (0.0, 0.0));
        st.b
    }
}

struct Run<'a> {
    t1: &'a Type1,
    b: Option<[f64; 4]>,
    x: f64, y: f64,
    sbx: f64,
    /// A moveto point, counted once something is drawn from it.
    pending: Option<(f64, f64)>,
    /// Inside a flex (othersubr 1 to 0): every moveto point is part of the curve.
    flex: bool,
    /// Results of callothersubr, for pop.
    ps: Vec<f64>,
    stack: Vec<f64>,
    done: bool,
}

impl Run<'_> {
    fn add(&mut self, x: f64, y: f64) {
        self.b = Some(match self.b { None => [x, y, x, y], Some(b) => [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)] });
    }

    fn line(&mut self, dx: f64, dy: f64, off: (f64, f64)) {
        if let Some((px, py)) = self.pending.take() { self.add(px + off.0, py + off.1); }
        self.x += dx; self.y += dy;
        self.add(self.x + off.0, self.y + off.1);
    }

    fn mv(&mut self, dx: f64, dy: f64, off: (f64, f64)) {
        self.x += dx; self.y += dy;
        if self.flex { self.add(self.x + off.0, self.y + off.1); } else { self.pending = Some((self.x, self.y)); }
    }

    fn curve(&mut self, d: [f64; 6], off: (f64, f64)) {
        if let Some((px, py)) = self.pending.take() { self.add(px + off.0, py + off.1); }
        for k in 0..3 {
            self.x += d[2 * k]; self.y += d[2 * k + 1];
            self.add(self.x + off.0, self.y + off.1);
        }
    }

    fn glyph_by_std(&self, code: f64) -> Option<usize> {
        let u = *tables::STANDARD.get(code as usize)?;
        let s = char::from_u32(u as u32)?.to_string();
        (0..self.t1.glyph_count()).find(|&i| crate::font::glyph_unicode(self.t1.name(i).as_bytes()).as_deref() == Some(s.as_str()))
    }

    fn exec(&mut self, cs: &[u8], depth: usize, off: (f64, f64)) {
        if depth > 10 { self.done = true; return; }
        let mut i = 0;
        while i < cs.len() && !self.done {
            let v = cs[i];
            i += 1;
            match v {
                32..=246 => self.stack.push(v as f64 - 139.0),
                247..=250 => { let w = *cs.get(i).unwrap_or(&0) as f64; i += 1; self.stack.push((v as f64 - 247.0) * 256.0 + w + 108.0); }
                251..=254 => { let w = *cs.get(i).unwrap_or(&0) as f64; i += 1; self.stack.push(-(v as f64 - 251.0) * 256.0 - w - 108.0); }
                255 => {
                    let b = cs.get(i..i + 4).map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
                    i += 4;
                    self.stack.push(b as f64);
                }
                _ => {
                    let op = if v == 12 { let e = *cs.get(i).unwrap_or(&0); i += 1; 100 + e as u16 } else { v as u16 };
                    let s = std::mem::take(&mut self.stack);
                    let a = |k: usize| s.get(k).copied().unwrap_or(0.0);
                    match op {
                        13 => { self.sbx = a(0); self.x = a(0); self.y = 0.0; } // hsbw
                        107 => { self.sbx = a(0); self.x = a(0); self.y = a(1); } // sbw
                        21 => self.mv(a(0), a(1), off),
                        22 => self.mv(a(0), 0.0, off),
                        4 => self.mv(0.0, a(0), off),
                        5 => self.line(a(0), a(1), off),
                        6 => self.line(a(0), 0.0, off),
                        7 => self.line(0.0, a(0), off),
                        8 => self.curve([a(0), a(1), a(2), a(3), a(4), a(5)], off),
                        30 => self.curve([0.0, a(0), a(1), a(2), a(3), 0.0], off),
                        31 => self.curve([a(0), 0.0, a(1), a(2), 0.0, a(3)], off),
                        9 | 1 | 3 | 100 | 101 | 102 => {} // closepath, hints, dotsection
                        133 => {} // setcurrentpoint: the flex's last moveto already put the point there
                        10 => {
                            // callsubr: the rest of the stack stays for the subroutine
                            let mut s = s;
                            let n = s.pop().unwrap_or(-1.0);
                            self.stack = s;
                            if let Some(sub) = (n >= 0.0).then(|| self.t1.subrs.get(n as usize)).flatten() {
                                let sub = sub.clone();
                                self.exec(&sub, depth + 1, off);
                            }
                        }
                        11 => { self.stack = s; return; } // return
                        14 => { self.done = true; } // endchar
                        112 => { // div
                            let mut s = s;
                            let (d, n) = (s.pop().unwrap_or(1.0), s.pop().unwrap_or(0.0));
                            s.push(if d != 0.0 { n / d } else { 0.0 });
                            self.stack = s;
                        }
                        116 => { // callothersubr: othersubr 1 starts a flex, 0 ends it, 3 hands back its subr number
                            let mut s = s;
                            let n = s.pop().unwrap_or(-1.0) as i64;
                            let k = (s.pop().unwrap_or(0.0).max(0.0) as usize).min(s.len());
                            let args: Vec<f64> = s.split_off(s.len() - k);
                            match n {
                                1 => { self.flex = true; self.ps.clear(); }
                                0 => { self.flex = false; self.ps = args.get(1..3).map(|v| vec![v[1], v[0]]).unwrap_or_default(); }
                                _ => { self.ps = args.into_iter().rev().collect(); }
                            }
                            self.stack = s;
                        }
                        117 => { let mut s = s; s.push(self.ps.pop().unwrap_or(0.0)); self.stack = s; } // pop
                        106 => { // seac asb adx ady bchar achar: the base glyph, then the accent moved to (adx, ady)
                            let (asb, adx, ady, bchar, achar) = (a(0), a(1), a(2), a(3), a(4));
                            let base_sbx = self.sbx;
                            if let Some(g) = self.glyph_by_std(bchar) {
                                let cs = self.t1.glyphs[g].1.clone();
                                let mut r = Run { t1: self.t1, b: self.b, x: 0.0, y: 0.0, sbx: 0.0, pending: None, flex: false, ps: Vec::new(), stack: Vec::new(), done: false };
                                r.exec(&cs, depth + 1, off);
                                self.b = r.b;
                            }
                            if let Some(g) = self.glyph_by_std(achar) {
                                let cs = self.t1.glyphs[g].1.clone();
                                let o = (off.0 + base_sbx + adx - asb, off.1 + ady);
                                let mut r = Run { t1: self.t1, b: self.b, x: 0.0, y: 0.0, sbx: 0.0, pending: None, flex: false, ps: Vec::new(), stack: Vec::new(), done: false };
                                r.exec(&cs, depth + 1, o);
                                self.b = r.b;
                            }
                            self.done = true;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    fn num(v: i32) -> Vec<u8> {
        match v {
            -107..=107 => vec![(v + 139) as u8],
            108..=1131 => { let w = v - 108; vec![(247 + w / 256) as u8, (w % 256) as u8] }
            -1131..=-108 => { let w = -v - 108; vec![(251 + w / 256) as u8, (w % 256) as u8] }
            _ => { let mut b = vec![255]; b.extend(v.to_be_bytes()); b }
        }
    }

    fn encrypt(plain: &[u8], mut r: u16) -> Vec<u8> {
        plain.iter().map(|&p| { let c = p ^ (r >> 8) as u8; r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719); c }).collect()
    }

    /// A Type 1 program whose glyph A (code 65) is the triangle (-100, -50) (700, 0) (300, 900), its last
    /// side drawn by subroutine 0, the private part behind binary eexec.
    pub(crate) fn tiny_type1() -> Vec<u8> {
        let cs = |ops: Vec<Vec<u8>>| encrypt(&[vec![0u8; 4], ops.concat()].concat(), 4330);
        let subr = cs(vec![num(-400), num(900), vec![5], vec![11]]);
        let a = cs(vec![num(0), num(500), vec![13], num(-100), num(-50), vec![21], num(800), num(50), vec![5], num(0), vec![10], vec![9], vec![14]]);
        let notdef = cs(vec![num(0), num(500), vec![13], vec![14]]);
        let mut private = b"dup /Private 8 dict dup begin /lenIV 4 def /Subrs 1 array
".to_vec();
        private.extend(format!("dup 0 {} RD ", subr.len()).as_bytes()); private.extend(&subr); private.extend(b" NP
ND
");
        private.extend(b"2 index /CharStrings 2 dict dup begin
");
        private.extend(format!("/.notdef {} RD ", notdef.len()).as_bytes()); private.extend(&notdef); private.extend(b" ND
");
        private.extend(format!("/A {} RD ", a.len()).as_bytes()); private.extend(&a); private.extend(b" ND
end
end
");
        let mut out = b"%!FontType1-1.0: Tiny
/FontMatrix [0.001 0 0 0.001 0 0] readonly def
/Encoding 256 array
0 1 255 {1 index exch /.notdef put} for
dup 65 /A put
readonly def
currentfile eexec
".to_vec();
        out.extend(encrypt(&[vec![0u8; 4], private].concat(), 55665));
        out
    }

    #[test]
    fn a_type1_glyph_is_boxed_through_its_subroutines() {
        let t = super::Type1::parse(&tiny_type1()).unwrap();
        assert_eq!(t.scale, (0.001, 0.001));
        let a = t.index_of("A").unwrap();
        assert_eq!(t.bounds(a), Some([-100.0, -50.0, 700.0, 900.0]));
        assert_eq!(t.builtin(65, |_| None), Some(a));
        assert!(t.bounds(t.index_of(".notdef").unwrap()).is_none());
    }
}
