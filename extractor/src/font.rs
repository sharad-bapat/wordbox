//! Fonts: which bytes make one character code, and which Unicode text each code stands for.
//!
//! The order is fixed and comes from the PDF spec (ISO 32000-1 9.10.2):
//!   1. the font's ToUnicode CMap, when it has the code;
//!   2. otherwise the font's encoding (a base encoding plus /Differences) gives a glyph name,
//!      and the Adobe Glyph List rules turn the name into Unicode;
//!   3. otherwise the code is unmapped. Nothing is guessed.
use crate::cmap::{self, CMap, Space};
use crate::truetype::TrueType;
use crate::{find, get, parse_val, skip_val, skip_ws, tables, Pdf, Val};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind { Type1, TrueType, Type3, Type0, Other }

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self { Kind::Type1 => "Type1", Kind::TrueType => "TrueType", Kind::Type3 => "Type3", Kind::Type0 => "Type0", Kind::Other => "Other" }
    }
}

pub struct Font {
    pub base: String,
    pub kind: Kind,
    /// How the codes were decoded, for reports: e.g. "WinAnsi+Differences", "Identity-H".
    pub encoding: String,
    pub embedded: bool,
    to_unicode: Option<CMap>,
    /// Simple fonts: code -> Unicode from the encoding.
    simple: Option<Vec<Option<String>>>,
    /// Type0 fonts: the code-space ranges that split strings into codes.
    spaces: Vec<Space>,
    code_len: usize,
    /// Type0 fonts: code -> CID from an embedded encoding CMap; None means Identity (CID = code).
    cid_map: Option<CMap>,
    /// Type0 fonts with an embedded TrueType program: the fallback when ToUnicode lacks a code.
    cid_program: Option<CidProgram>,
    pub metrics: Metrics,
}

struct CidProgram {
    tt: TrueType,
    /// CID -> glyph; None means Identity.
    cid_to_gid: Option<Vec<u16>>,
}

/// Glyph widths and the font's vertical extent. Widths are in glyph units (1/1000 em for every font
/// but Type3, whose glyph space goes through its /FontMatrix).
pub struct Metrics {
    first_char: u32,
    widths: Vec<f64>,
    missing: f64,
    std: Option<&'static [(char, u16)]>,
    dw: f64,
    /// CID ranges (first, last, width), sorted by first.
    w: Vec<(u32, u32, f64)>,
    ascent: f64,
    descent: f64,
    /// Glyph space to text space: 0.001 for all fonts but Type3.
    fm: [f64; 6],
    /// Where the widths came from, for reports: "Widths", "W", "standard font", "default".
    pub source: &'static str,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics { first_char: 0, widths: Vec::new(), missing: 0.0, std: None, dw: 1000.0, w: Vec::new(),
                  ascent: 800.0, descent: -200.0, fm: [0.001, 0.0, 0.0, 0.001, 0.0, 0.0], source: "default" }
    }
}

fn number(pdf: &Pdf, v: Option<Val>) -> Option<f64> { match v.map(|v| pdf.direct(v)) { Some(Val::Num(x)) => Some(x), _ => None } }

fn numbers(pdf: &Pdf, v: Option<Val>) -> Vec<f64> {
    match v.map(|v| pdf.direct(v)) {
        Some(Val::Array(a)) => {
            // array items may themselves be references (seen in /Widths of some producers)
            let mut out = Vec::new();
            let mut i = 0;
            while i < a.len() {
                i = skip_ws(&a, i);
                if i >= a.len() { break; }
                match pdf.direct(parse_val(&a, i)) { Val::Num(x) => out.push(x), _ => out.push(0.0) }
                i = skip_val(&a, i).max(i + 1);
            }
            out
        }
        _ => Vec::new(),
    }
}

/// A CIDFont /W array: `c [w1 w2 ...]` or `c_first c_last w`.
fn cid_widths(pdf: &Pdf, v: Option<Val>) -> Vec<(u32, u32, f64)> {
    let a = match v.map(|v| pdf.direct(v)) { Some(Val::Array(a)) => a, _ => return Vec::new() };
    let mut items: Vec<Val> = Vec::new();
    let mut i = 0;
    while i < a.len() {
        i = skip_ws(&a, i);
        if i >= a.len() { break; }
        items.push(pdf.direct(parse_val(&a, i)));
        i = skip_val(&a, i).max(i + 1);
    }
    let mut out = Vec::new();
    let mut k = 0;
    while k < items.len() {
        match (&items[k], items.get(k + 1), items.get(k + 2)) {
            (Val::Num(c), Some(Val::Array(ws)), _) => {
                let ws = numbers(pdf, Some(Val::Array(ws.clone())));
                for (j, w) in ws.iter().enumerate() { out.push((*c as u32 + j as u32, *c as u32 + j as u32, *w)); }
                k += 2;
            }
            (Val::Num(c0), Some(Val::Num(c1)), Some(Val::Num(w))) => { out.push((*c0 as u32, *c1 as u32, *w)); k += 3; }
            _ => k += 1,
        }
    }
    out.sort_by_key(|r| r.0);
    out
}

/// Built-in widths of a standard font, by base name (subset prefix and common Windows spellings allowed).
fn std_widths(base: &str) -> Option<&'static [(char, u16)]> {
    let bare = base.rsplit('+').next().unwrap_or(base);
    let find = |n: &str| tables::STANDARD_WIDTHS.binary_search_by(|(k, _)| k.cmp(&n)).ok().map(|i| tables::STANDARD_WIDTHS[i].1);
    if let Some(w) = find(bare) { return Some(w); }
    // ArialMT, Arial-BoldMT, TimesNewRomanPSMT, TimesNewRomanPS-BoldItalicMT, CourierNewPSMT -> Arial,Bold etc.
    let s = bare.replace("PSMT", "").replace("PS-", "-").replace("MT", "").replace("PS", "");
    find(&s).or_else(|| find(&s.replacen('-', ",", 1)))
}

fn load_vertical(pdf: &Pdf, fd: Option<&[u8]>, m: &mut Metrics) {
    let Some(fd) = fd else { return };
    let (a, d) = (number(pdf, get(fd, b"/Ascent")).unwrap_or(0.0), number(pdf, get(fd, b"/Descent")).unwrap_or(0.0));
    if a > 0.0 { m.ascent = a; m.descent = d.min(0.0); return; }
    let bb = numbers(pdf, get(fd, b"/FontBBox"));
    if bb.len() == 4 && bb[3] > bb[1] { m.ascent = bb[3]; m.descent = bb[1].min(0.0); }
}

/// Unicode for a glyph name, by the Adobe Glyph List specification's rules.
pub fn glyph_unicode(name: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(name).ok()?;
    let name = name.split('.').next().unwrap_or("");
    let mut out = String::new();
    for part in name.split('_') {
        if let Ok(k) = tables::AGL.binary_search_by(|(n, _)| n.cmp(&part)) {
            out.push_str(tables::AGL[k].1);
        } else if let Some(hex) = part.strip_prefix("uni") {
            if hex.len() >= 4 && hex.len() % 4 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                for q in hex.as_bytes().chunks(4) {
                    let v = u32::from_str_radix(std::str::from_utf8(q).ok()?, 16).ok()?;
                    if let Some(c) = char::from_u32(v) { out.push(c); }
                }
            }
        } else if let Some(hex) = part.strip_prefix('u') {
            if (4..=6).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                if let Some(c) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) { out.push(c); }
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn base_table(name: &[u8]) -> Option<(&'static [u16; 256], &'static str)> {
    match name {
        b"WinAnsiEncoding" => Some((&tables::WIN_ANSI, "WinAnsi")),
        b"MacRomanEncoding" => Some((&tables::MAC_ROMAN, "MacRoman")),
        b"StandardEncoding" => Some((&tables::STANDARD, "Standard")),
        _ => None,
    }
}

fn from_table(t: &[u16; 256]) -> Vec<Option<String>> {
    t.iter().map(|&u| if u == 0 { None } else { char::from_u32(u as u32).map(|c| c.to_string()) }).collect()
}

/// The built-in encoding in a Type1 font program's cleartext header: "dup 65 /A put" lines.
fn type1_builtin(program: &[u8]) -> Option<Vec<Option<String>>> {
    let clear = &program[..find(program, b"eexec", 0).unwrap_or(program.len())];
    if find(clear, b"/Encoding StandardEncoding", 0).is_some() { return Some(from_table(&tables::STANDARD)); }
    let mut t: Vec<Option<String>> = vec![None; 256];
    let mut any = false;
    let mut i = 0;
    while let Some(p) = find(clear, b"dup ", i) {
        i = p + 4;
        let j = skip_ws(clear, i);
        let mut k = j;
        while k < clear.len() && clear[k].is_ascii_digit() { k += 1; }
        let code: usize = match std::str::from_utf8(&clear[j..k]).ok().and_then(|x| x.parse().ok()) { Some(c) => c, None => continue };
        let k = skip_ws(clear, k);
        if code > 255 || clear.get(k) != Some(&b'/') { continue; }
        let mut e = k + 1;
        while e < clear.len() && !b" \n\r\t/[]".contains(&clear[e]) { e += 1; }
        t[code] = glyph_unicode(&clear[k + 1..e]);
        any = true;
    }
    if any { Some(t) } else { None }
}

impl Font {
    pub fn load(pdf: &Pdf, dict: &[u8]) -> Font {
        let name_of = |v: Option<Val>| match v.map(|v| pdf.direct(v)) { Some(Val::Name(n)) => String::from_utf8_lossy(&n).into_owned(), _ => String::new() };
        let kind = match name_of(get(dict, b"/Subtype")).as_str() {
            "Type1" | "MMType1" => Kind::Type1,
            "TrueType" => Kind::TrueType,
            "Type3" => Kind::Type3,
            "Type0" => Kind::Type0,
            _ => Kind::Other,
        };
        let base = name_of(get(dict, b"/BaseFont"));
        let to_unicode = match get(dict, b"/ToUnicode") {
            Some(Val::Ref(n)) => pdf.stream(n).map(|s| CMap::parse(&s, glyph_unicode)).filter(|m| m.has_unicode()),
            _ => None,
        };
        let mut f = Font { base, kind, encoding: String::new(), embedded: false, to_unicode, simple: None, spaces: Vec::new(),
                           code_len: 1, cid_map: None, cid_program: None, metrics: Metrics::default() };
        if kind == Kind::Type0 { f.load_type0(pdf, dict); } else { f.load_simple(pdf, dict); f.load_simple_metrics(pdf, dict); }
        f
    }

    fn load_simple_metrics(&mut self, pdf: &Pdf, dict: &[u8]) {
        let m = &mut self.metrics;
        let fd = get(dict, b"/FontDescriptor").and_then(|v| pdf.resolve(&v));
        m.widths = numbers(pdf, get(dict, b"/Widths"));
        m.first_char = number(pdf, get(dict, b"/FirstChar")).unwrap_or(0.0).max(0.0) as u32;
        m.missing = fd.as_ref().and_then(|d| number(pdf, get(d, b"/MissingWidth"))).unwrap_or(0.0);
        m.source = if !m.widths.is_empty() { "Widths" } else { "default" };
        if self.kind == Kind::Type3 {
            let fm = numbers(pdf, get(dict, b"/FontMatrix"));
            if fm.len() == 6 { m.fm = [fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]]; }
            let bb = numbers(pdf, get(dict, b"/FontBBox"));
            if bb.len() == 4 && bb[3] > bb[1] { m.ascent = bb[3]; m.descent = bb[1].min(0.0); }
            else { m.ascent = 0.8 / m.fm[3].abs().max(1e-9); m.descent = -0.2 / m.fm[3].abs().max(1e-9); }
        } else {
            if m.widths.is_empty() {
                m.std = std_widths(&self.base);
                if m.std.is_some() { m.source = "standard font"; }
            }
            load_vertical(pdf, fd.as_deref(), m);
        }
    }

    /// Advance width of a code in text space, per unit font size (ISO 32000-1 9.2.4 and 9.4.4).
    pub fn advance(&self, code: u32) -> f64 {
        let m = &self.metrics;
        let w = if self.kind == Kind::Type0 {
            let cid = match &self.cid_map { Some(c) => c.cid(code).unwrap_or(code), None => code };
            let k = m.w.partition_point(|r| r.0 <= cid);
            match k.checked_sub(1).map(|i| m.w[i]) { Some((_, last, w)) if cid <= last => w, _ => m.dw }
        } else if code >= m.first_char && ((code - m.first_char) as usize) < m.widths.len() {
            m.widths[(code - m.first_char) as usize]
        } else if let Some(t) = m.std {
            let c = self.unicode(code).and_then(|s| s.chars().next());
            c.and_then(|c| t.binary_search_by_key(&c, |e| e.0).ok().map(|i| t[i].1 as f64)).unwrap_or(m.missing)
        } else if m.widths.is_empty() && m.missing == 0.0 {
            500.0 // no widths at all: half an em, a fixed default
        } else {
            m.missing
        };
        w * m.fm[0]
    }

    /// The font's descent and ascent in text space, per unit font size.
    pub fn vertical(&self) -> (f64, f64) { (self.metrics.descent * self.metrics.fm[3], self.metrics.ascent * self.metrics.fm[3]) }

    /// Whether a code is the single-byte space that word spacing (Tw) applies to.
    pub fn is_word_space(&self, code: u32, len: usize) -> bool { code == 32 && len == 1 }

    fn load_type0(&mut self, pdf: &Pdf, dict: &[u8]) {
        self.code_len = 2;
        let mut encoding_cmap: Option<CMap> = None;
        match get(dict, b"/Encoding") {
            Some(Val::Name(n)) if n == b"Identity-H" || n == b"Identity-V" => {
                self.spaces = CMap::parse(b"1 begincodespacerange <0000> <FFFF> endcodespacerange", |_| None).spaces;
                self.encoding = String::from_utf8_lossy(&n).into_owned();
            }
            Some(Val::Name(n)) => {
                // a predefined CMap we don't carry: split codes by the ToUnicode map's ranges if it has them
                self.encoding = format!("{} (not supported)", String::from_utf8_lossy(&n));
            }
            Some(Val::Ref(r)) => {
                if let Some(s) = pdf.stream(r) { encoding_cmap = Some(CMap::parse(&s, |_| None)); }
                self.spaces = encoding_cmap.as_ref().map(|m| m.spaces.clone()).unwrap_or_default();
                self.encoding = "embedded CMap".into();
            }
            _ => self.encoding = "none".into(),
        }
        if self.spaces.is_empty() {
            if let Some(tu) = &self.to_unicode { self.spaces = tu.spaces.clone(); }
        }
        let cid = match get(dict, b"/DescendantFonts").map(|v| pdf.direct(v)) {
            Some(Val::Array(a)) => match parse_val(&a, 0) { Val::Ref(n) => pdf.dict(n), Val::Dict(d) => Some(d), _ => None },
            _ => None,
        };
        self.cid_map = encoding_cmap;
        let Some(cid) = cid else { return };
        self.metrics.dw = number(pdf, get(&cid, b"/DW")).unwrap_or(1000.0);
        self.metrics.w = cid_widths(pdf, get(&cid, b"/W"));
        self.metrics.source = if self.metrics.w.is_empty() { "DW" } else { "W" };
        let Some(fd) = get(&cid, b"/FontDescriptor").and_then(|v| pdf.resolve(&v)) else { return };
        load_vertical(pdf, Some(&fd), &mut self.metrics);
        self.embedded = [b"/FontFile".as_slice(), b"/FontFile2", b"/FontFile3"].iter().any(|k| get(&fd, k).is_some());
        // CIDFontType2 with a TrueType program: CID -> glyph -> Unicode, for codes ToUnicode lacks
        if let Some(Val::Ref(ff2)) = get(&fd, b"/FontFile2") {
            if let Some(tt) = pdf.stream(ff2).and_then(|p| TrueType::parse(&p)) {
                let cid_to_gid = match get(&cid, b"/CIDToGIDMap") {
                    Some(Val::Ref(m)) => pdf.stream(m).map(|b| b.chunks(2).map(|p| if p.len() == 2 { u16::from_be_bytes([p[0], p[1]]) } else { 0 }).collect()),
                    _ => None,
                };
                self.cid_program = Some(CidProgram { tt, cid_to_gid });
            }
        }
    }

    fn load_simple(&mut self, pdf: &Pdf, dict: &[u8]) {
        let fd = get(dict, b"/FontDescriptor").and_then(|v| pdf.resolve(&v));
        let flags = fd.as_ref().and_then(|d| match get(d, b"/Flags").map(|v| pdf.direct(v)) { Some(Val::Num(x)) => Some(x as u32), _ => None }).unwrap_or(0);
        let symbolic = flags & 4 != 0;
        let file = |key: &[u8]| fd.as_ref().and_then(|d| match get(d, key) { Some(Val::Ref(n)) => Some(n), _ => None });
        let (ff1, ff2, ff3) = (file(b"/FontFile"), file(b"/FontFile2"), file(b"/FontFile3"));
        self.embedded = ff1.is_some() || ff2.is_some() || ff3.is_some();
        let bare = self.base.split('+').last().unwrap_or("").to_string();
        let standard_symbol = bare.starts_with("Symbol") || bare.starts_with("ZapfDingbats");

        // the font's own encoding, used when /Encoding is missing or has no /BaseEncoding
        let builtin = |this: &Font| -> (Option<Vec<Option<String>>>, String) {
            match this.kind {
                Kind::Type1 => {
                    if let Some(t) = ff1.and_then(|n| pdf.stream(n)).and_then(|p| type1_builtin(&p)) { return (Some(t), "font program".into()); }
                    if standard_symbol { return (None, "built-in symbol (not supported)".into()); }
                    (Some(from_table(&tables::STANDARD)), "Standard".into())
                }
                Kind::TrueType if symbolic => (None, "built-in symbolic".into()),
                Kind::TrueType => (Some(from_table(&tables::WIN_ANSI)), "WinAnsi (default)".into()),
                _ => (None, "none".into()),
            }
        };

        let enc = get(dict, b"/Encoding").map(|v| pdf.direct(v));
        let (mut table, mut desc) = match &enc {
            Some(Val::Name(n)) => match base_table(n) {
                Some((t, d)) => (Some(from_table(t)), d.to_string()),
                None => builtin(self),
            },
            Some(Val::Dict(d)) => match get(d, b"/BaseEncoding").map(|v| pdf.direct(v)) {
                Some(Val::Name(n)) => match base_table(&n) { Some((t, d)) => (Some(from_table(t)), d.to_string()), None => builtin(self) },
                _ => builtin(self),
            },
            _ => builtin(self),
        };
        if let Some(Val::Dict(d)) = &enc {
            if let Some(Val::Array(diff)) = get(d, b"/Differences").map(|v| pdf.direct(v)) {
                let t = table.get_or_insert_with(|| vec![None; 256]);
                let mut code = 0usize;
                let mut i = 0;
                while i < diff.len() {
                    i = skip_ws(&diff, i);
                    if i >= diff.len() { break; }
                    match parse_val(&diff, i) {
                        Val::Num(n) => code = n as usize,
                        Val::Name(nm) => { if code < 256 { t[code] = glyph_unicode(&nm); } code += 1; }
                        _ => {}
                    }
                    i = skip_val(&diff, i).max(i + 1);
                }
                desc.push_str("+Differences");
            }
        }
        // an embedded TrueType program: code -> glyph (its cmap) -> Unicode (its own Unicode cmap or glyph
        // names), or the Mac Roman character when the font maps the code in its (1,0) Mac subtable.
        // It decides only when the file gives no /Encoding (an explicit encoding names each glyph, even in
        // a font flagged symbolic); otherwise it only fills codes the encoding leaves empty.
        if self.kind == Kind::TrueType {
            if let Some(tt) = ff2.and_then(|n| pdf.stream(n)).and_then(|p| TrueType::parse(&p)) {
                let prog: Vec<Option<String>> = (0..256u32).map(|c| tt.simple_text(c)).collect();
                if prog.iter().any(|x| x.is_some()) {
                    let program_first = enc.is_none();
                    let base = table.take().unwrap_or_else(|| vec![None; 256]);
                    table = Some(base.into_iter().zip(prog).map(|(e, p)| if program_first { p.or(e) } else { e.or(p) }).collect());
                    desc = if program_first { format!("font program (over {desc})") } else { format!("{desc}, gaps from font program") };
                }
            }
        }
        self.simple = table.take();
        self.encoding = std::mem::take(&mut desc);
    }

    /// Split a shown string into (code, byte length) pairs.
    pub fn codes(&self, bytes: &[u8]) -> Vec<(u32, usize)> { cmap::split(&self.spaces, self.code_len, bytes) }

    /// Unicode text for one character code, or None when the file doesn't say.
    pub fn unicode(&self, code: u32) -> Option<String> {
        if let Some(s) = self.to_unicode.as_ref().and_then(|m| m.unicode(code)) {
            if !s.is_empty() { return Some(s); }
        }
        if let Some(s) = self.simple.as_ref().and_then(|t| t.get(code as usize).cloned().flatten()) { return Some(s); }
        let p = self.cid_program.as_ref()?;
        let cid = match &self.cid_map { Some(m) => m.cid(code)?, None => code };
        let gid = match &p.cid_to_gid { Some(v) => *v.get(cid as usize)?, None => cid as u16 };
        p.tt.glyph_text(gid).map(|s| s.to_string())
    }

    pub fn has_to_unicode(&self) -> bool { self.to_unicode.is_some() }
}
