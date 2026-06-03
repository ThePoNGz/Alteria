//! Loading the highlight query and (in [`highlight`](crate::highlight)) running
//! it over a rope. Mirrors Zed's `Grammar`/`HighlightsConfig` + the synchronous
//! `Language::highlight_text`/`parse_text` path, on our gpui-free stack.

use std::sync::LazyLock;

use tree_sitter::Query;

use crate::Lang;

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
