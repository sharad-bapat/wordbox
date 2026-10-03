//! Glyph outlines from an embedded font program, for each glyph's ink box: the box its outline covers,
//! which can reach past the advance (italic overhang) and above the descriptor's /Ascent (tall
//! capitals). Read with ttf-parser: TrueType and OpenType programs (FontFile2, FontFile3 /OpenType) and
//! bare CFF (FontFile3 /Type1C and /CIDFontType0C). A program it can't read gives no boxes, and the glyph
//! keeps its advance box alone. Ported from where-are-the-regions.
use std::cell::RefCell;
use std::collections::HashMap;

use ttf_parser::{cff, Face, GlyphId};

pub struct Outlines {
    data: Vec<u8>,
    /// A bare CFF program; otherwise an sfnt (TrueType or OpenType).
    cff: bool,
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
        Some(Outlines { data, cff: cff_program, scale, cids, names, cache: RefCell::new(HashMap::new()) })
    }

    /// The glyph for a character through the program's own tables: its Unicode cmap (sfnt) or its glyph
    /// names (CFF).
    pub fn by_unicode(&self, s: &str) -> Option<u16> {
        if self.cff { return self.names.get(s).copied(); }
        let mut cs = s.chars();
        let (c, None) = (cs.next()?, cs.next()) else { return None };
        Face::parse(&self.data, 0).ok()?.glyph_index(c).map(|g| g.0).filter(|&g| g != 0)
    }

    /// The glyph for a simple font's code with no help from the encoding: an sfnt's (3,0) symbol cmap at
    /// code, 0xF000+code, 0xF100+code or 0xF200+code, then its (1,0) Mac cmap (ISO 32000-1 9.6.6.4); a CFF
    /// program's built-in encoding.
    pub fn by_code(&self, code: u32) -> Option<u16> {
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
        let r = if self.cff {
            cff::Table::parse(&self.data).and_then(|t| t.outline(GlyphId(gid), &mut Sink).ok())
        } else {
            Face::parse(&self.data, 0).ok().and_then(|f| f.outline_glyph(GlyphId(gid), &mut Sink))
        };
        let (sx, sy) = self.scale;
        let b = r.filter(|r| r.x_max > r.x_min || r.y_max > r.y_min).map(|r| {
            let (xa, xb, ya, yb) = (r.x_min as f64 * sx, r.x_max as f64 * sx, r.y_min as f64 * sy, r.y_max as f64 * sy);
            [xa.min(xb), ya.min(yb), xa.max(xb), ya.max(yb)]
        });
        self.cache.borrow_mut().insert(gid, b);
        b
    }
}
