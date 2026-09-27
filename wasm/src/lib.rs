//! The browser build of wordbox: every word of a PDF with its box, as JSON (see wordbox::Doc::to_json).
use wasm_bindgen::prelude::*;

/// Words per page, with boxes, verdicts and fonts, for the bytes of one PDF.
#[wasm_bindgen]
pub fn extract_json(bytes: &[u8]) -> String { wordbox::extract(bytes).to_json(false) }
