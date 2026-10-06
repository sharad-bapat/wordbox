//! wordbox: where is the text?
//!
//! Reads what a born-digital PDF draws and reports each glyph as Unicode text, in drawing order.
//! It never renders anything and never infers meaning: every character comes from the file's own
//! records (fonts, encodings, ToUnicode maps), by rules fixed in the PDF spec.
//!
//! The parser (object index, page tree, decryption, stream filters) is copied from scan-or-text,
//! so the two tools stay independent.
use std::collections::HashMap;


pub use font::Kind;

const MAX_FORM_DEPTH: usize = 8;

// The PDF reading itself (byte helpers, values, filters, the object index, decryption, the page
// tree, and the fonts: programs, encodings, CMaps, glyph outlines) is pdf-core's, shared with
// scan-or-text and where-are-the-regions.
pub(crate) use pdf_core::*;

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
