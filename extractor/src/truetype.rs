//! Just enough of an embedded TrueType font program to say which Unicode text a glyph stands for,
//! when the PDF itself doesn't: the 'cmap' table (character code -> glyph) and the 'post' table
//! (glyph -> name). Only tables inside the file are read; nothing is guessed.
//! Every read is bounds-checked; a damaged table just yields no mapping.
use std::collections::HashMap;

use crate::tables;

fn u16_at(b: &[u8], i: usize) -> Option<u16> { Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?])) }
fn u32_at(b: &[u8], i: usize) -> Option<u32> { Some(u32::from_be_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?])) }

const MAX_ENTRIES: usize = 70_000;

pub struct TrueType {
    /// (platform, encoding) -> code -> glyph
    subtables: Vec<((u16, u16), HashMap<u32, u16>)>,
    /// glyph -> Unicode text, from the font's Unicode cmap (lowest code point wins) or post names
    glyph_text: HashMap<u16, String>,
}

fn table<'f>(font: &'f [u8], tag: &[u8; 4]) -> Option<&'f [u8]> {
    let n = u16_at(font, 4)? as usize;
    for k in 0..n.min(64) {
        let r = 12 + 16 * k;
        if font.get(r..r + 4)? == tag {
            let (off, len) = (u32_at(font, r + 8)? as usize, u32_at(font, r + 12)? as usize);
            return font.get(off..off.checked_add(len)?.min(font.len()));
        }
    }
    None
}

/// One cmap subtable as code -> glyph pairs (formats 0, 4, 6 and 12).
fn subtable(t: &[u8]) -> Option<HashMap<u32, u16>> {
    let mut m = HashMap::new();
    match u16_at(t, 0)? {
        0 => { for c in 0..256usize { let g = *t.get(6 + c)? as u16; if g != 0 { m.insert(c as u32, g); } } }
        4 => {
            let seg = u16_at(t, 6)? as usize / 2;
            let (ends, starts, deltas, ranges) = (14, 16 + 2 * seg, 16 + 4 * seg, 16 + 6 * seg);
            // a truncated table (seen in real files) loses only the segments it can't hold
            for s in 0..seg {
                let (Some(end), Some(start), Some(delta), Some(ro)) =
                    (u16_at(t, ends + 2 * s), u16_at(t, starts + 2 * s), u16_at(t, deltas + 2 * s), u16_at(t, ranges + 2 * s)) else { continue };
                let (end, start, ro) = (end as u32, start as u32, ro as usize);
                if start > end { continue; }
                for c in start..=end.min(0xFFFE) {
                    let g = if ro == 0 { (c as u16).wrapping_add(delta) } else {
                        let at = ranges + 2 * s + ro + 2 * (c - start) as usize;
                        match u16_at(t, at) { Some(0) | None => 0, Some(g) => g.wrapping_add(delta) }
                    };
                    if g != 0 { m.insert(c, g); }
                    if m.len() > MAX_ENTRIES { return Some(m); }
                }
            }
        }
        6 => {
            let (first, count) = (u16_at(t, 6)? as u32, u16_at(t, 8)? as usize);
            for k in 0..count { let g = u16_at(t, 10 + 2 * k)?; if g != 0 { m.insert(first + k as u32, g); } }
        }
        12 => {
            let groups = u32_at(t, 12)? as usize;
            for k in 0..groups.min(MAX_ENTRIES) {
                let r = 16 + 12 * k;
                let (s, e, g0) = (u32_at(t, r)?, u32_at(t, r + 4)?, u32_at(t, r + 8)?);
                if s > e || (e - s) as usize > MAX_ENTRIES { continue; }
                for c in s..=e { let g = g0 + (c - s); if g != 0 && g <= 0xFFFF { m.insert(c, g as u16); } }
                if m.len() > MAX_ENTRIES { break; }
            }
        }
        _ => return None,
    }
    Some(m)
}

/// Glyph names from a 'post' table, formats 1 and 2.
fn post_names(t: &[u8]) -> Vec<Option<String>> {
    match u32_at(t, 0) {
        Some(0x0001_0000) => tables::MAC_GLYPHS.iter().map(|s| Some(s.to_string())).collect(),
        Some(0x0002_0000) => {
            let n = match u16_at(t, 32) { Some(n) => n as usize, None => return Vec::new() };
            let mut custom = Vec::new();
            let mut p = 34 + 2 * n;
            while p < t.len() {
                let len = t[p] as usize;
                custom.push(t.get(p + 1..p + 1 + len).map(|s| String::from_utf8_lossy(s).into_owned()));
                p += 1 + len;
            }
            (0..n).map(|g| {
                let idx = u16_at(t, 34 + 2 * g)? as usize;
                if idx < 258 { Some(tables::MAC_GLYPHS[idx].to_string()) } else { custom.get(idx - 258).cloned().flatten() }
            }).collect()
        }
        _ => Vec::new(),
    }
}

impl TrueType {
    pub fn parse(font: &[u8]) -> Option<TrueType> {
        let cmap = table(font, b"cmap");
        let mut subtables = Vec::new();
        if let Some(c) = cmap {
            let n = u16_at(c, 2).unwrap_or(0) as usize;
            for k in 0..n.min(32) {
                let r = 4 + 8 * k;
                let (Some(pid), Some(eid), Some(off)) = (u16_at(c, r), u16_at(c, r + 2), u32_at(c, r + 4)) else { continue };
                if let Some(m) = c.get(off as usize..).and_then(subtable) { subtables.push(((pid, eid), m)); }
            }
        }
        let mut glyph_text: HashMap<u16, String> = HashMap::new();
        // the font's own Unicode cmaps, reversed: the lowest code point for each glyph
        for ((pid, eid), m) in &subtables {
            if *pid == 0 || (*pid == 3 && (*eid == 1 || *eid == 10)) {
                let mut pairs: Vec<(&u32, &u16)> = m.iter().collect();
                pairs.sort();
                for (c, g) in pairs {
                    if let Some(ch) = char::from_u32(*c) { glyph_text.entry(*g).or_insert_with(|| ch.to_string()); }
                }
            }
        }
        // then glyph names, for glyphs the Unicode cmaps didn't cover
        if let Some(p) = table(font, b"post") {
            for (g, name) in post_names(p).into_iter().enumerate() {
                if g > 0xFFFF { break; }
                if let Some(u) = name.and_then(|n| crate::font::glyph_unicode(n.as_bytes())) {
                    glyph_text.entry(g as u16).or_insert(u);
                }
            }
        }
        if subtables.is_empty() && glyph_text.is_empty() { return None; }
        Some(TrueType { subtables, glyph_text })
    }

    fn sub(&self, pid: u16, eid: u16) -> Option<&HashMap<u32, u16>> {
        self.subtables.iter().find(|(k, _)| *k == (pid, eid)).map(|(_, m)| m)
    }

    /// Glyph for a simple (one-byte) font's code, by ISO 32000-1 9.6.6.4: the (3,0) symbol cmap at
    /// code, 0xF000+code, 0xF100+code or 0xF200+code; otherwise the (1,0) Mac cmap.
    pub fn simple_glyph(&self, code: u32) -> Option<u16> {
        if let Some(m) = self.sub(3, 0) {
            for base in [0u32, 0xF000, 0xF100, 0xF200] { if let Some(g) = m.get(&(base + code)) { return Some(*g); } }
        }
        self.sub(1, 0).and_then(|m| m.get(&code).copied())
    }

    pub fn glyph_text(&self, gid: u16) -> Option<&str> { self.glyph_text.get(&gid).map(|s| s.as_str()) }

    /// Text for a simple font's code, from the font program alone: the glyph's own Unicode (reversed
    /// Unicode cmap or post name); failing that, when the font maps the code in its (1,0) subtable,
    /// the Mac Roman character at that code, since a (1,0) subtable is keyed by Mac Roman codes.
    pub fn simple_text(&self, code: u32) -> Option<String> {
        if let Some(t) = self.simple_glyph(code).and_then(|g| self.glyph_text(g)) { return Some(t.to_string()); }
        if code < 256 && self.sub(1, 0).map(|m| m.contains_key(&code)).unwrap_or(false) {
            let u = tables::MAC_ROMAN[code as usize];
            if u != 0 { return char::from_u32(u as u32).map(|c| c.to_string()); }
        }
        None
    }
}
