//! A fallback for CFF glyphs ttf-parser won't outline: it rejects any charstring using the
//! deprecated dotsection operator (12 0), which Adobe's Type 2 Charstring Format (TN 5177, Appendix C)
//! says to treat as a no-op (where-are-the-regions results/findings.md). Just enough of the Compact Font Format (TN 5176) to
//! find a glyph's charstring and its subroutines, and a Type 2 interpreter that keeps the box of the
//! outline's points, control points included. seac-style accented glyphs (endchar with four arguments)
//! and the arithmetic operators aren't handled; such a glyph gives what it drew before them. Ported
//! from where-are-the-regions.

fn u16_at(b: &[u8], i: usize) -> Option<usize> { Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as usize) }

fn offset(b: &[u8], i: usize, size: usize) -> Option<usize> {
    let mut v = 0usize;
    for k in 0..size { v = (v << 8) | *b.get(i + k)? as usize; }
    Some(v)
}

/// A CFF INDEX starting at `at`: its items as byte ranges, and where it ends.
fn index(b: &[u8], at: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let count = u16_at(b, at)?;
    if count == 0 { return Some((Vec::new(), at + 2)); }
    let size = *b.get(at + 2)? as usize;
    if !(1..=4).contains(&size) { return None; }
    let base = at + 3 + (count + 1) * size - 1;
    let mut items = Vec::with_capacity(count);
    for k in 0..count {
        let (s, e) = (offset(b, at + 3 + k * size, size)?, offset(b, at + 3 + (k + 1) * size, size)?);
        if e < s || base + e > b.len() { return None; }
        items.push((base + s, base + e));
    }
    let end = base + offset(b, at + 3 + count * size, size)?;
    Some((items, end))
}

/// A DICT's entries as (operator, operands); two-byte operators are 1200 + the second byte.
fn dict(b: &[u8]) -> Vec<(u16, Vec<f64>)> {
    let mut out = Vec::new();
    let mut ops = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let v = b[i];
        match v {
            0..=21 => {
                let op = if v == 12 { i += 1; 1200 + *b.get(i).unwrap_or(&0) as u16 } else { v as u16 };
                out.push((op, std::mem::take(&mut ops)));
                i += 1;
            }
            28 => { ops.push(i16::from_be_bytes([*b.get(i + 1).unwrap_or(&0), *b.get(i + 2).unwrap_or(&0)]) as f64); i += 3; }
            29 => { ops.push(b.get(i + 1..i + 5).map(|s| i32::from_be_bytes([s[0], s[1], s[2], s[3]])).unwrap_or(0) as f64); i += 5; }
            30 => {
                // a real: nibbles until 0xf
                let mut s = String::new();
                i += 1;
                'real: while i < b.len() {
                    for n in [b[i] >> 4, b[i] & 15] {
                        match n {
                            0..=9 => s.push((b'0' + n) as char),
                            0xa => s.push('.'),
                            0xb => s.push('E'),
                            0xc => s.push_str("E-"),
                            0xe => s.push('-'),
                            0xf => { i += 1; break 'real; }
                            _ => {}
                        }
                    }
                    i += 1;
                }
                ops.push(s.parse().unwrap_or(0.0));
            }
            32..=246 => { ops.push(v as f64 - 139.0); i += 1; }
            247..=250 => { ops.push((v as f64 - 247.0) * 256.0 + *b.get(i + 1).unwrap_or(&0) as f64 + 108.0); i += 2; }
            251..=254 => { ops.push(-(v as f64 - 251.0) * 256.0 - *b.get(i + 1).unwrap_or(&0) as f64 - 108.0); i += 2; }
            _ => i += 1,
        }
    }
    out
}

fn get(d: &[(u16, Vec<f64>)], op: u16) -> Option<&[f64]> { d.iter().find(|(o, _)| *o == op).map(|(_, v)| v.as_slice()) }

/// A Private DICT's local subroutines, from its (size, offset) operands.
fn local_subrs(b: &[u8], private: &[f64]) -> Vec<(usize, usize)> {
    let (Some(&size), Some(&off)) = (private.first(), private.get(1)) else { return Vec::new() };
    let (size, off) = (size.max(0.0) as usize, off.max(0.0) as usize);
    let Some(p) = b.get(off..off.saturating_add(size)) else { return Vec::new() };
    match get(&dict(p), 19).and_then(|v| v.first()) {
        Some(&s) => index(b, off + s.max(0.0) as usize).map(|(v, _)| v).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// The FD a glyph belongs to in a CID-keyed font, from FDSelect formats 0 and 3.
fn fd_of(b: &[u8], at: usize, gid: usize) -> Option<usize> {
    match *b.get(at)? {
        0 => b.get(at + 1 + gid).map(|&v| v as usize),
        3 => {
            let n = u16_at(b, at + 1)?;
            for k in 0..n {
                let (first, fd, next) = (u16_at(b, at + 3 + 3 * k)?, *b.get(at + 5 + 3 * k)? as usize, u16_at(b, at + 6 + 3 * k)?);
                if gid >= first && gid < next { return Some(fd); }
            }
            None
        }
        _ => None,
    }
}

fn bias(n: usize) -> i64 { if n < 1240 { 107 } else if n < 33900 { 1131 } else { 32768 } }

/// The box of a glyph's outline points in font units (x0, y0, x1, y1), from a bare CFF program; None
/// for an empty glyph or one that can't be read.
pub fn bounds(b: &[u8], gid: u16) -> Option<[f64; 4]> {
    let hdr = *b.get(2)? as usize;
    let (_names, e1) = index(b, hdr)?;
    let (tops, e2) = index(b, e1)?;
    let (_strings, e3) = index(b, e2)?;
    let (gsubrs, _) = index(b, e3)?;
    let top = dict(b.get(tops.first()?.0..tops.first()?.1)?);
    let cs_at = *get(&top, 17)?.first()? as usize;
    let (charstrings, _) = index(b, cs_at)?;
    let &(s, e) = charstrings.get(gid as usize)?;
    let lsubrs = match (get(&top, 1236), get(&top, 1237)) {
        (Some(fda), Some(fds)) => {
            let fd = fd_of(b, *fds.first()? as usize, gid as usize)?;
            let (fdarray, _) = index(b, *fda.first()? as usize)?;
            let &(fs, fe) = fdarray.get(fd)?;
            get(&dict(&b[fs..fe]), 18).map(|p| local_subrs(b, p)).unwrap_or_default()
        }
        _ => get(&top, 18).map(|p| local_subrs(b, p)).unwrap_or_default(),
    };
    let mut r = Run { b, gsubrs: &gsubrs, lsubrs: &lsubrs, stack: Vec::new(), x: 0.0, y: 0.0, pending: None, bbox: None, stems: 0, width_seen: false, done: false };
    r.exec(s, e, 0);
    r.bbox
}

struct Run<'a> {
    b: &'a [u8],
    gsubrs: &'a [(usize, usize)],
    lsubrs: &'a [(usize, usize)],
    stack: Vec<f64>,
    x: f64, y: f64,
    pending: Option<(f64, f64)>,
    bbox: Option<[f64; 4]>,
    stems: usize,
    width_seen: bool,
    done: bool,
}

impl Run<'_> {
    fn add(&mut self, x: f64, y: f64) {
        self.bbox = Some(match self.bbox { None => [x, y, x, y], Some(b) => [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)] });
    }

    fn start(&mut self) { if let Some((x, y)) = self.pending.take() { self.add(x, y); } }

    fn to(&mut self, dx: f64, dy: f64) { self.x += dx; self.y += dy; self.add(self.x, self.y); }

    fn line(&mut self, dx: f64, dy: f64) { self.start(); self.to(dx, dy); }

    fn curve(&mut self, d: [f64; 6]) { self.start(); for k in 0..3 { self.to(d[2 * k], d[2 * k + 1]); } }

    fn moveto(&mut self, dx: f64, dy: f64) { self.x += dx; self.y += dy; self.pending = Some((self.x, self.y)); }

    /// The first stack-clearing operator may carry the glyph's width as an extra first operand.
    fn drop_width(&mut self, s: &mut Vec<f64>, extra: bool) {
        if !self.width_seen { self.width_seen = true; if extra && !s.is_empty() { s.remove(0); } }
    }

    fn exec(&mut self, mut i: usize, end: usize, depth: usize) {
        if depth > 10 { self.done = true; return; }
        let b = self.b;
        while i < end && !self.done {
            let v = b[i];
            i += 1;
            match v {
                28 => { self.stack.push(i16::from_be_bytes([*b.get(i).unwrap_or(&0), *b.get(i + 1).unwrap_or(&0)]) as f64); i += 2; }
                32..=246 => self.stack.push(v as f64 - 139.0),
                247..=250 => { self.stack.push((v as f64 - 247.0) * 256.0 + *b.get(i).unwrap_or(&0) as f64 + 108.0); i += 1; }
                251..=254 => { self.stack.push(-(v as f64 - 251.0) * 256.0 - *b.get(i).unwrap_or(&0) as f64 - 108.0); i += 1; }
                255 => {
                    let f = b.get(i..i + 4).map(|s| i32::from_be_bytes([s[0], s[1], s[2], s[3]])).unwrap_or(0);
                    self.stack.push(f as f64 / 65536.0);
                    i += 4;
                }
                _ => {
                    let op = if v == 12 { let e = *b.get(i).unwrap_or(&0); i += 1; 1200 + e as u16 } else { v as u16 };
                    let mut s = std::mem::take(&mut self.stack);
                    let a = |s: &[f64], k: usize| s.get(k).copied().unwrap_or(0.0);
                    match op {
                        1 | 3 | 18 | 23 => { let odd = s.len() % 2 == 1; self.drop_width(&mut s, odd); self.stems += s.len() / 2; }
                        19 | 20 => {
                            // hintmask, cntrmask: operands left on the stack are vstems
                            let odd = s.len() % 2 == 1;
                            self.drop_width(&mut s, odd);
                            self.stems += s.len() / 2;
                            i += self.stems.div_ceil(8);
                        }
                        21 => { let x = s.len() > 2; self.drop_width(&mut s, x); self.moveto(a(&s, 0), a(&s, 1)); }
                        22 => { let x = s.len() > 1; self.drop_width(&mut s, x); self.moveto(a(&s, 0), 0.0); }
                        4 => { let x = s.len() > 1; self.drop_width(&mut s, x); self.moveto(0.0, a(&s, 0)); }
                        5 => { for p in s.chunks_exact(2) { self.line(p[0], p[1]); } }
                        6 | 7 => {
                            let mut h = op == 6;
                            for &d in &s { if h { self.line(d, 0.0) } else { self.line(0.0, d) } h = !h; }
                        }
                        8 => { for c in s.chunks_exact(6) { self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]); } }
                        24 => {
                            // rcurveline: curves, then a line
                            let n = s.len().saturating_sub(2) / 6;
                            for c in s[..6 * n].chunks_exact(6) { self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]); }
                            if s.len() >= 6 * n + 2 { self.line(s[6 * n], s[6 * n + 1]); }
                        }
                        25 => {
                            // rlinecurve: lines, then a curve
                            let n = s.len().saturating_sub(6) / 2;
                            for p in s[..2 * n].chunks_exact(2) { self.line(p[0], p[1]); }
                            if let Some(c) = s.get(2 * n..2 * n + 6) { self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]); }
                        }
                        26 | 27 => {
                            // vvcurveto, hhcurveto: an optional first offset, then groups of four
                            let (mut k, mut first) = (0, 0.0);
                            if s.len() % 4 == 1 { first = s[0]; k = 1; }
                            while k + 4 <= s.len() {
                                let c = &s[k..k + 4];
                                if op == 27 { self.curve([c[0], first, c[1], c[2], c[3], 0.0]); } else { self.curve([first, c[0], c[1], c[2], 0.0, c[3]]); }
                                first = 0.0;
                                k += 4;
                            }
                        }
                        30 | 31 => {
                            // vhcurveto, hvcurveto: alternating, the last curve may end with an extra offset
                            let mut h = op == 31;
                            let mut k = 0;
                            while k + 4 <= s.len() {
                                let c = &s[k..k + 4];
                                let last = if s.len() - k == 5 { s[k + 4] } else { 0.0 };
                                if h { self.curve([c[0], 0.0, c[1], c[2], last, c[3]]); } else { self.curve([0.0, c[0], c[1], c[2], c[3], last]); }
                                h = !h;
                                k += 4;
                            }
                        }
                        10 | 29 => {
                            let n = s.pop().unwrap_or(0.0) as i64;
                            let subrs = if op == 10 { self.lsubrs } else { self.gsubrs };
                            self.stack = s;
                            let k = n + bias(subrs.len());
                            if let Some(&(ss, se)) = (k >= 0).then(|| subrs.get(k as usize)).flatten() { self.exec(ss, se, depth + 1); }
                            continue;
                        }
                        11 => { self.stack = s; return; }
                        14 => { let x = s.len() == 1 || s.len() == 5; self.drop_width(&mut s, x); self.done = true; }
                        1234 => { // hflex
                            if s.len() >= 7 {
                                self.curve([s[0], 0.0, s[1], s[2], s[3], 0.0]);
                                self.curve([s[4], 0.0, s[5], -s[2], s[6], 0.0]);
                            }
                        }
                        1235 => { // flex
                            if s.len() >= 12 {
                                self.curve([s[0], s[1], s[2], s[3], s[4], s[5]]);
                                self.curve([s[6], s[7], s[8], s[9], s[10], s[11]]);
                            }
                        }
                        1236 => { // hflex1
                            if s.len() >= 9 {
                                self.curve([s[0], s[1], s[2], s[3], s[4], 0.0]);
                                self.curve([s[5], 0.0, s[6], s[7], s[8], -(s[1] + s[3] + s[7])]);
                            }
                        }
                        1237 => { // flex1
                            if s.len() >= 11 {
                                let dx: f64 = (0..5).map(|k| s[2 * k]).sum();
                                let dy: f64 = (0..5).map(|k| s[2 * k + 1]).sum();
                                let (lx, ly) = if dx.abs() > dy.abs() { (s[10], -dy) } else { (-dx, s[10]) };
                                self.curve([s[0], s[1], s[2], s[3], s[4], s[5]]);
                                self.curve([s[6], s[7], s[8], s[9], lx, ly]);
                            }
                        }
                        // 12 0 dotsection and anything else: a no-op that clears the stack
                        _ => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn num(v: i32) -> Vec<u8> {
        match v {
            -107..=107 => vec![(v + 139) as u8],
            108..=1131 => { let w = v - 108; vec![(247 + w / 256) as u8, (w % 256) as u8] }
            _ => { let w = -v - 108; vec![(251 + w / 256) as u8, (w % 256) as u8] }
        }
    }

    fn index(items: &[Vec<u8>]) -> Vec<u8> {
        if items.is_empty() { return vec![0, 0]; }
        let mut out = vec![0, items.len() as u8, 1, 1];
        let mut at = 1;
        for it in items { at += it.len(); out.push(at as u8); }
        for it in items { out.extend(it); }
        out
    }

    fn int5(v: usize) -> Vec<u8> { let mut b = vec![29]; b.extend((v as i32).to_be_bytes()); b }

    /// A bare CFF with .notdef and one glyph: the triangle (-100, -50) (700, 0) (300, 900), with a
    /// dotsection (12 0) before its last side, which local subroutine 0 draws.
    pub(crate) fn tiny_cff() -> Vec<u8> {
        let glyph = [num(-100), num(-50), vec![21], num(800), num(50), vec![5], vec![12, 0], num(-107), vec![10], vec![14]].concat();
        let subr = [num(-400), num(900), vec![5, 11]].concat();
        let charstrings = index(&[vec![14], glyph]);
        let subrs = index(&[subr]);
        let private = [int5(6), vec![19]].concat();
        let head = [vec![1, 0, 4, 1], index(&[b"Tiny".to_vec()])].concat();
        // the Top DICT has a fixed size (five-byte integers), so the offsets can be worked out first
        let top_len = 5 + 1 + 5 + 5 + 1;
        let top_index_len = 5 + top_len;
        let cs_at = head.len() + top_index_len + 2 + 2;
        let priv_at = cs_at + charstrings.len();
        let top = [int5(cs_at), vec![17], int5(private.len()), int5(priv_at), vec![18]].concat();
        assert_eq!(top.len(), top_len);
        [head, index(&[top]), vec![0, 0], vec![0, 0], charstrings, private, subrs].concat()
    }

    #[test]
    fn a_cff_glyph_with_dotsection_is_boxed() {
        let d = tiny_cff();
        // ttf-parser gives up on 12 0; our reader treats it as a no-op (TN 5177, Appendix C)
        let t = ttf_parser::cff::Table::parse(&d).unwrap();
        assert!(t.outline(ttf_parser::GlyphId(1), &mut crate::outline::tests::NoPen).is_err());
        assert_eq!(super::bounds(&d, 1), Some([-100.0, -50.0, 700.0, 900.0]));
        assert_eq!(super::bounds(&d, 0), None);
        let o = crate::outline::Outlines::parse(d, true).unwrap();
        // the font matrix comes through ttf-parser as f32
        let b = o.bounds(1).unwrap();
        assert!(b.iter().zip([-100.0, -50.0, 700.0, 900.0]).all(|(x, y)| (x - y).abs() < 1e-3), "{b:?}");
    }
}
