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

pub use highlight::highlight;

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
    use rope::Rope;
    use tree_sitter::Parser;

    use crate::{Highlight, Lang, capture_names, highlight};

    fn rope(src: &str) -> Rope {
        let mut r = Rope::new();
        r.push(src);
        r
    }

    fn name_of(h: &Highlight) -> &'static str {
        capture_names(Lang::Rust)[h.capture as usize]
    }

    /// The span whose byte range is exactly the first occurrence of `needle`.
    fn span_for<'a>(src: &str, spans: &'a [Highlight], needle: &str) -> &'a Highlight {
        let start = src.find(needle).expect("needle is present in the fixture");
        let range = start..start + needle.len();
        spans.iter().find(|h| h.range == range).unwrap_or_else(|| {
            panic!("no span exactly covering {needle:?} ({range:?}); got {spans:?}")
        })
    }

    const FIXTURE: &str = "// greet\nfn main() {\n    let x = 1;\n    let s = \"hi\";\n}\n";

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

    /// The heart of the plan: a Rust fixture yields the right `(byte_range,
    /// capture)` spans. Covers a keyword, a function name, a binding identifier,
    /// a number literal, a string literal, and a comment. `main` exercises the
    /// overlap rule — the catch-all `(identifier) @variable` and the more
    /// specific `@function.definition` both cover it, and the specific one wins.
    #[test]
    fn highlights_keyword_function_variable_number_string_and_comment() {
        let spans = highlight(&rope(FIXTURE), Lang::Rust);

        assert_eq!(name_of(span_for(FIXTURE, &spans, "// greet")), "comment");
        assert_eq!(name_of(span_for(FIXTURE, &spans, "fn")), "keyword");
        assert_eq!(
            name_of(span_for(FIXTURE, &spans, "main")),
            "function.definition"
        );
        assert_eq!(name_of(span_for(FIXTURE, &spans, "let")), "keyword");
        assert_eq!(name_of(span_for(FIXTURE, &spans, "x")), "variable");
        assert_eq!(name_of(span_for(FIXTURE, &spans, "1")), "number");
        assert_eq!(name_of(span_for(FIXTURE, &spans, "\"hi\"")), "string");
    }

    #[test]
    fn spans_are_sorted_and_non_overlapping() {
        let spans = highlight(&rope(FIXTURE), Lang::Rust);
        assert!(!spans.is_empty());
        for h in &spans {
            assert!(h.range.start < h.range.end, "empty span {h:?}");
        }
        for w in spans.windows(2) {
            assert!(
                w[0].range.end <= w[1].range.start,
                "overlap or out of order: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn empty_input_has_no_spans() {
        assert!(highlight(&rope(""), Lang::Rust).is_empty());
    }

    /// tree-sitter is error-tolerant: garbage still parses (with ERROR nodes) and
    /// highlighting must neither panic nor emit overlapping/disordered spans.
    #[test]
    fn non_rust_text_parses_without_panicking() {
        let spans = highlight(&rope("@@@ >>> not ;; rust {{{ 123"), Lang::Rust);
        for w in spans.windows(2) {
            assert!(w[0].range.end <= w[1].range.start);
        }
    }
}
