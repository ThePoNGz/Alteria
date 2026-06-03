//! Headless syntax-highlight engine: parse buffer text with **tree-sitter** and
//! return `(byte_range, capture-identity)` spans for one language (Rust).
//!
//! A faithful subset of Zed's `Language::highlight_text` stack — same
//! tree-sitter, same `highlights.scm` query mechanism, same `(range, capture)`
//! output — deliberately omitting the performance/scale layers (incremental
//! reparse, injections, multi-layer `SyntaxMap`, async parsing). See plan 009.
//!
//! **gpui-free, capture-identity not color.** Zed's theme-built `HighlightMap`
//! turns a capture into a theme color id, and that lives in gpui-coupled
//! `syntax_theme`. Our core has no theme, so this crate emits the query's
//! **capture index** (a stable, theme-free identity) and exposes the language's
//! [`capture_names`]; the *frontend* builds the `capture -> color` map. This
//! keeps Zed's exact `language` (capture identity) vs `theme` (color) split while
//! honoring the one hard rule that the engine never imports gpui. See `devlog/009`.

use std::ops::Range;

mod highlight;

/// A source language the engine can highlight. One language for now (Rust),
/// matching plan 009's scope: whole-buffer parse, single tree, no injections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
}

/// One highlight span: a buffer **byte range** tagged with the highlight query's
/// **capture index** — our gpui-free highlight identity, *not* a theme color id.
/// Map `capture` back to its name with [`capture_names`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Highlight {
    pub range: Range<usize>,
    pub capture: u32,
}

/// The highlight query's capture names for `lang`, indexed by capture id: the
/// name of the capture behind [`Highlight::capture`] `i` is
/// `capture_names(lang)[i as usize]`. These come straight from the loaded
/// `tree_sitter::Query::capture_names()`; the frontend maps each name to a theme
/// color (the `capture -> color` seam this engine deliberately leaves out).
pub fn capture_names(lang: Lang) -> Vec<&'static str> {
    highlight::config(lang).query.capture_names().to_vec()
}

#[cfg(test)]
mod tests {
    use tree_sitter::Parser;

    /// The vendored Rust `highlights.scm` compiles against `tree-sitter-rust`
    /// 0.24.2 and exposes a stable, non-empty set of capture names. A grammar/
    /// query version skew (or a botched re-vendoring) breaks the query build.
    #[test]
    fn rust_highlight_query_compiles_and_exposes_captures() {
        let names = crate::capture_names(crate::Lang::Rust);
        assert!(!names.is_empty(), "the Rust query must define captures");
        // A representative subset Zed's query is known to define; their absence
        // would mean the query failed to load or the wrong grammar was linked.
        for expected in ["keyword", "type", "function", "variable", "comment"] {
            assert!(names.contains(&expected), "missing capture @{expected}");
        }
    }

    /// Smoke test: the pinned `tree-sitter` 0.26.9 runtime links the
    /// `tree-sitter-rust` 0.24.2 grammar and parses a trivial program. A version
    /// skew between the two would fail here (grammar ABI / link error).
    #[test]
    fn tree_sitter_rust_links_and_parses() {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("tree-sitter-rust grammar is ABI-compatible with the pinned runtime");
        let tree = parser
            .parse("fn main() {}", None)
            .expect("parse yields a tree when a language is set");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(!root.has_error());
    }
}
