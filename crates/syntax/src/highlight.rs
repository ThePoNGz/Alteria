//! Loading the highlight query and running it over a rope. Mirrors Zed's
//! `Grammar`/`HighlightsConfig` plus the synchronous `Language::highlight_text`
//! / `parse_text` path (`crates/language/src/language.rs`), on our gpui-free
//! stack and emitting capture identity instead of theme color.

use std::sync::LazyLock;

use rope::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor, Tree};

use crate::{Highlight, Lang};

/// Zed's Rust highlight query (`zed-industries/zed`,
/// `crates/grammars/src/rust/highlights.scm`), vendored **verbatim** — byte-for-
/// byte, so future upstream diffs line up (the same discipline as the vendored
/// `rope`/`text` sources). Provenance is also recorded in `devlog/009`. Do not
/// edit this file; re-vendor from upstream instead.
const RUST_HIGHLIGHTS: &str = include_str!("../queries/rust/highlights.scm");

/// The compiled highlight query for one language — built once from the grammar
/// plus `highlights.scm`. Mirrors Zed's `HighlightsConfig { query, .. }`
/// (`grammar.rs:47`), minus the theme-built `HighlightMap` (that needs a theme,
/// which lives in the frontend — see the crate docs and `devlog/009`).
pub(crate) struct HighlightsConfig {
    pub query: Query,
}

/// The `tree_sitter::Language` for a [`Lang`]. The grammar ships in a separate
/// crate exposing a `LanguageFn` (`tree_sitter_rust::LANGUAGE`) that `.into()`s
/// into a `tree_sitter::Language` (mirrors Zed's grammar wiring).
pub(crate) fn ts_language(lang: Lang) -> tree_sitter::Language {
    match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
    }
}

static RUST_CONFIG: LazyLock<HighlightsConfig> = LazyLock::new(|| {
    let query = Query::new(&ts_language(Lang::Rust), RUST_HIGHLIGHTS).expect(
        "Zed's verbatim Rust highlights.scm must compile against tree-sitter-rust 0.24.2 \
         (a grammar/query version skew is the classic breakage and would surface here)",
    );
    HighlightsConfig { query }
});

/// The cached [`HighlightsConfig`] for `lang` (the grammar + compiled query),
/// built on first use and reused thereafter.
pub(crate) fn config(lang: Lang) -> &'static HighlightsConfig {
    match lang {
        Lang::Rust => &RUST_CONFIG,
    }
}

/// Parse `text` into a single syntax tree, mirroring Zed's `parse_text`
/// (`language.rs:1287`): feed the rope to `tree_sitter::Parser` through its chunk
/// callback so we never materialize the whole document as one string. A whole-
/// buffer, from-scratch parse (`old_tree = None`); incremental reparse is a
/// deferred non-goal.
fn parse(text: &Rope, lang: Lang) -> Option<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&ts_language(lang))
        .expect("grammar ABI matches the pinned tree-sitter runtime");
    let mut chunks = text.chunks_in_range(0..text.len());
    parser.parse_with_options(
        &mut move |offset, _| {
            chunks.seek(offset);
            chunks.next().unwrap_or("").as_bytes()
        },
        None,
        None,
    )
}

/// Highlight `text` as `lang`: parse it, run the highlight query, and return
/// **byte-range spans tagged with the query's capture index**, sorted by start
/// and non-overlapping. Mirrors Zed's synchronous `Language::highlight_text`
/// (`language.rs:1026`) over a single tree, with the innermost capture winning
/// per byte (Zed's `BufferChunks` stack, `buffer.rs`).
///
/// `capture` is the gpui-free highlight identity; resolve it to a name with
/// [`capture_names`](crate::capture_names) and to a color in the frontend.
pub fn highlight(text: &Rope, lang: Lang) -> Vec<Highlight> {
    let Some(tree) = parse(text, lang) else {
        // `parse_with_options` only returns `None` under a cancellation/timeout
        // we never configure; treat the unreachable case as "no spans" rather
        // than panicking on this input-reachable path.
        return Vec::new();
    };
    let query = &config(lang).query;

    // Collect every capture as (start, end, capture_index). tree-sitter applies
    // the query's `#match?` text predicates for us, reading node text through the
    // `TextProvider`.
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(query, tree.root_node(), TextProvider(text));
    let mut caps: Vec<(usize, usize, u32)> = Vec::new();
    while let Some((mat, ix)) = captures.next() {
        let capture = mat.captures[*ix];
        let node = capture.node;
        caps.push((node.start_byte(), node.end_byte(), capture.index));
    }

    // Order captures as Zed's `SyntaxMapCaptures` does (`syntax_map.rs` `sort_key`):
    // start ascending, then end *descending*, so for nested or co-located captures
    // the outer/earlier one is opened first and the inner/last one lands on top of
    // the stack — innermost wins. `sort_by_key` is stable, so genuinely identical
    // ranges keep tree-sitter's emission order (later query pattern wins), matching
    // Zed.
    caps.sort_by_key(|&(start, end, _)| (start, std::cmp::Reverse(end)));

    // Walk left to right with a stack of open captures `(end, capture)`; the top
    // of the stack is the active highlight for the current byte. This is Zed's
    // `BufferChunks` highlight loop reduced to a batch over the whole buffer
    // (pop captures that have ended, open captures that have started, emit up to
    // the next boundary).
    let len = text.len();
    let mut spans: Vec<Highlight> = Vec::new();
    let mut stack: Vec<(usize, u32)> = Vec::new();
    let mut next = caps.into_iter().peekable();
    let mut pos = 0usize;
    loop {
        while stack.last().is_some_and(|&(end, _)| end <= pos) {
            stack.pop();
        }
        while let Some(&(start, _, _)) = next.peek() {
            if start > pos {
                break;
            }
            let (_, end, capture) = next.next().expect("peeked a capture");
            if end > pos {
                stack.push((end, capture));
            }
        }
        // Nothing open and nothing left to open: done.
        if stack.is_empty() && next.peek().is_none() {
            break;
        }
        // Next byte where the active highlight can change: the innermost capture's
        // end or the next capture's start, whichever comes first. After the
        // pop/push above both candidates are strictly past `pos`, so the walk
        // always advances.
        let mut boundary = len;
        if let Some(&(end, _)) = stack.last() {
            boundary = boundary.min(end);
        }
        if let Some(&(start, _, _)) = next.peek() {
            boundary = boundary.min(start);
        }
        if let Some(&(_, capture)) = stack.last() {
            push_span(&mut spans, pos, boundary, capture);
        }
        pos = boundary;
    }
    spans
}

/// Append `[start, end) @capture`, extending the previous span instead when it
/// carries the same capture and is contiguous — keeps the output minimal while
/// staying sorted and non-overlapping.
fn push_span(spans: &mut Vec<Highlight>, start: usize, end: usize, capture: u32) {
    if let Some(last) = spans.last_mut()
        && last.capture == capture
        && last.range.end == start
    {
        last.range.end = end;
        return;
    }
    spans.push(Highlight {
        range: start..end,
        capture,
    });
}

/// Feeds rope text to tree-sitter for `#match?`-style predicate checks during
/// query execution — Zed's `TextProvider`/`ByteChunks` (`syntax_map.rs:255`).
struct TextProvider<'a>(&'a Rope);

struct ByteChunks<'a>(rope::Chunks<'a>);

impl<'a> tree_sitter::TextProvider<&'a [u8]> for TextProvider<'a> {
    type I = ByteChunks<'a>;

    fn text(&mut self, node: tree_sitter::Node) -> Self::I {
        ByteChunks(self.0.chunks_in_range(node.byte_range()))
    }
}

impl<'a> Iterator for ByteChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(str::as_bytes)
    }
}
