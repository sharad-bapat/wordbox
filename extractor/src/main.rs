//! CLI: extract PDFs and print one JSON object per file, per line.
//! usage: wordbox-cli [--text | --glyphs] [--list files.txt] [file.pdf...]
//!   (default)  every word with its box, per page
//!   --glyphs   words plus every glyph with its box and baseline start
//!   --text     decoded text per page, no geometry (for tools/decode_check.py)
use std::time::Instant;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut mode = "words";
    let mut paths = Vec::new();
    while let Some(a) = args.first().cloned() {
        match a.as_str() {
            "--text" => { mode = "text"; args.remove(0); }
            "--glyphs" => { mode = "glyphs"; args.remove(0); }
            "--list" => {
                if let Some(list) = args.get(1).and_then(|p| std::fs::read_to_string(p).ok()) {
                    paths.extend(list.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()));
                }
                args.drain(0..2.min(args.len()));
            }
            _ => break,
        }
    }
    paths.extend(args);
    for path in paths {
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => { println!("{{\"file\":{},\"error\":{}}}", wordbox::json_str(&path), wordbox::json_str(&e.to_string())); continue; }
        };
        let t = Instant::now();
        let doc = wordbox::extract(&data);
        let micros = t.elapsed().as_secs_f64() * 1e6;
        let json = match mode { "text" => doc.text_json(), "glyphs" => doc.to_json(true), _ => doc.to_json(false) };
        println!("{{\"file\":{},\"bytes\":{},\"micros\":{:.1},{}", wordbox::json_str(&path), data.len(), micros, &json[1..]);
    }
}
