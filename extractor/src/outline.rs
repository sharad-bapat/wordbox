//! Glyph outlines from an embedded font program, for each glyph's ink box: the box its outline covers,
//! which can reach past the advance (italic overhang) and above the descriptor's /Ascent (tall
//! capitals). Read with ttf-parser: TrueType and OpenType programs (FontFile2, FontFile3 /OpenType) and
//! bare CFF (FontFile3 /Type1C and /CIDFontType0C); Type 1 programs (FontFile) through type1.rs. A program it can't read gives no boxes, and the glyph
//! keeps its advance box alone. Ported from where-are-the-regions.
use std::cell::RefCell;
use std::collections::HashMap;

use ttf_parser::{cff, Face, GlyphId};

use crate::type1::Type1;

pub struct Outlines {
    data: Vec<u8>,
    /// A bare CFF program; otherwise an sfnt (TrueType or OpenType), unless `t1` holds a Type 1 program.
    cff: bool,
    t1: Option<Type1>,
    /// Font units to glyph units (1/1000 em), along x and y.
    scale: (f64, f64),
    /// A CID-keyed CFF program: CID -> glyph.
    cids: Option<HashMap<u16, u16>>,
    /// A name-keyed CFF program: Unicode (from each glyph's name) -> glyph, lowest glyph first.
    names: HashMap<String, u16>,
    cache: RefCell<HashMap<u16, Option<[f64; 4]>>>,
}

/// Collects nothing: ttf-parser returns the box of the points it is given.
struct Sink;

impl ttf_parser::OutlineBuilder for Sink {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) {}
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    fn close(&mut self) {}
}

impl Outlines {
    pub fn parse(data: Vec<u8>, cff_program: bool) -> Option<Outlines> {
        let mut names = HashMap::new();
        let (scale, cids) = if cff_program {
            let t = cff::Table::parse(&data)?;
            let m = t.matrix();
            let n = t.number_of_glyphs();
            let mut cids: HashMap<u16, u16> = HashMap::new();
            for g in 0..n {
                if let Some(c) = t.glyph_cid(GlyphId(g)) { cids.entry(c).or_insert(g); }
                if let Some(u) = t.glyph_name(GlyphId(g)).and_then(|nm| crate::font::glyph_unicode(nm.as_bytes())) { names.entry(u).or_insert(g); }
            }
            ((m.sx as f64 * 1000.0, m.sy as f64 * 1000.0), if cids.is_empty() { None } else { Some(cids) })
        } else {
            let f = Face::parse(&data, 0).ok()?;
            let u = (f.units_per_em() as f64).max(1.0);
            ((1000.0 / u, 1000.0 / u), None)
        };
        Some(Outlines { data, cff: cff_program, t1: None, scale, cids, names, cache: RefCell::new(HashMap::new()) })
    }

    /// A Type 1 program (/FontFile): glyphs by name, numbered in program order.
    pub fn type1(program: &[u8]) -> Option<Outlines> {
        let t = Type1::parse(program)?;
        let mut names = HashMap::new();
        for g in 0..t.glyph_count().min(65_535) {
            if let Some(u) = crate::font::glyph_unicode(t.name(g).as_bytes()) { names.entry(u).or_insert(g as u16); }
        }
        let scale = (t.scale.0 * 1000.0, t.scale.1 * 1000.0);
        Some(Outlines { data: Vec::new(), cff: false, t1: Some(t), scale, cids: None, names, cache: RefCell::new(HashMap::new()) })
    }

    /// The glyph for a character through the program's own tables: its Unicode cmap (sfnt) or its glyph
    /// names (CFF).
    pub fn by_unicode(&self, s: &str) -> Option<u16> {
        if self.cff || self.t1.is_some() { return self.names.get(s).copied(); }
        let mut cs = s.chars();
        let (c, None) = (cs.next()?, cs.next()) else { return None };
        Face::parse(&self.data, 0).ok()?.glyph_index(c).map(|g| g.0).filter(|&g| g != 0)
    }

    /// The glyph for a simple font's code with no help from the encoding: an sfnt's (3,0) symbol cmap at
    /// code, 0xF000+code, 0xF100+code or 0xF200+code, then its (1,0) Mac cmap (ISO 32000-1 9.6.6.4); a CFF
    /// program's built-in encoding.
    pub fn by_code(&self, code: u32) -> Option<u16> {
        if let Some(t) = &self.t1 {
            return t.builtin(code, |s| self.names.get(s).map(|&g| g as usize)).map(|g| g as u16);
        }
        if self.cff {
            let t = cff::Table::parse(&self.data)?;
            return t.glyph_index(u8::try_from(code).ok()?).map(|g| g.0).filter(|&g| g != 0);
        }
        let f = Face::parse(&self.data, 0).ok()?;
        let cmap = f.tables().cmap?;
        let find = |pid: u16, eid: u16, c: u32| cmap.subtables.into_iter()
            .filter(|s| s.platform_id as u16 == pid && s.encoding_id == eid)
            .find_map(|s| s.glyph_index(c)).map(|g| g.0).filter(|&g| g != 0);
        [0u32, 0xF000, 0xF100, 0xF200].iter().find_map(|b| find(3, 0, b + code)).or_else(|| find(1, 0, code))
    }

    /// The glyph for a CID: a CID-keyed CFF program's charset, otherwise the CID itself.
    pub fn by_cid(&self, cid: u32) -> Option<u16> {
        match &self.cids { Some(m) => m.get(&u16::try_from(cid).ok()?).copied(), None => u16::try_from(cid).ok() }
    }

    /// A glyph's outline box in glyph units (x0, y0, x1, y1); None for an empty glyph or one the program
    /// lacks.
    pub fn bounds(&self, gid: u16) -> Option<[f64; 4]> {
        if let Some(b) = self.cache.borrow().get(&gid) { return *b; }
        if let Some(t) = &self.t1 {
            let (sx, sy) = self.scale;
            let b = t.bounds(gid as usize).filter(|b| b[2] > b[0] || b[3] > b[1]).map(|b| {
                let (xa, xb, ya, yb) = (b[0] * sx, b[2] * sx, b[1] * sy, b[3] * sy);
                [xa.min(xb), ya.min(yb), xa.max(xb), ya.max(yb)]
            });
            self.cache.borrow_mut().insert(gid, b);
            return b;
        }
        let r = if self.cff {
            cff::Table::parse(&self.data).and_then(|t| t.outline(GlyphId(gid), &mut Sink).ok())
        } else {
            Face::parse(&self.data, 0).ok().and_then(|f| f.outline_glyph(GlyphId(gid), &mut Sink))
        };
        let units = r.map(|r| [r.x_min as f64, r.y_min as f64, r.x_max as f64, r.y_max as f64]).or_else(|| {
            // a CFF glyph ttf-parser won't outline (dotsection): our own Type 2 reader
            if self.cff { return crate::cff::bounds(&self.data, gid); }
            let f = Face::parse(&self.data, 0).ok()?;
            crate::cff::bounds(f.raw_face().table(ttf_parser::Tag::from_bytes(b"CFF "))?, gid)
        });
        let (sx, sy) = self.scale;
        let b = units.filter(|r| r[2] > r[0] || r[3] > r[1]).map(|r| {
            let (xa, xb, ya, yb) = (r[0] * sx, r[2] * sx, r[1] * sy, r[3] * sy);
            [xa.min(xb), ya.min(yb), xa.max(xb), ya.max(yb)]
        });
        self.cache.borrow_mut().insert(gid, b);
        b
    }
}

#[cfg(test)]
pub(crate) mod tests {
    pub(crate) struct NoPen;
    impl ttf_parser::OutlineBuilder for NoPen {
        fn move_to(&mut self, _: f32, _: f32) {}
        fn line_to(&mut self, _: f32, _: f32) {}
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
        fn close(&mut self) {}
    }
}
