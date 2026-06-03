# Plan 009 — Syntax-highlight engine: a headless `crates/syntax` (tree-sitter)

**Goal:** Stand up `crates/syntax` — a **headless, gpui-free** crate that parses buffer text with
**tree-sitter** and returns highlight spans: `(byte_range, capture-identity)`. This is the engine half
of syntax highlighting; the frontend (wave 2) maps capture identity → color and paints colored runs.
It is a **faithful subset of Zed's** stack (same tree-sitter, same `highlights.scm` query mechanism,
same `(range, capture)` output as Zed's own `Language::highlight_text`), deliberately omitting the
performance/scale layers.

**Scope = one language (Rust), whole-buffer parse, single tree, no injections.** That is exactly what
Zed's synchronous `Language::highlight_text` does standalone — so it is a legitimate Zed subset, not a
re-derivation. Incremental reparse, language injections, multi-layer `SyntaxMap`, and async parsing
are explicit Non-goals (CLAUDE.md: *"feels instant on the files I actually open"*, not huge-file perf).

**Status / deps:** Plans 001–007 on `main` (007/008 may be executing in parallel — fine, see
Parallelism). This crate is **new and self-contained**: it depends on the vendored `rope` crate (to
read buffer text) + the new tree-sitter deps, and is consumed **later** by `alteria-gpui`. It makes
**no change to `alteria-core`** and **no frontend change**.

> **Sources of truth — Zed (`../zed-main`), verified while planning:** reproduce these (the "Zed is
> the source of truth" hard rule). Cite the file you mirrored in `devlog/009`.
> - **Stack pins (match exactly):** `tree-sitter = "0.26.9"` (`zed-main/Cargo.toml:772`; drop the
>   `wasm` feature — not needed) and `tree-sitter-rust = "0.24.2"` (`Cargo.toml:792`). The grammar is
>   a separate crate exposing `tree_sitter_rust::LANGUAGE`, `.into()`-converted to a
>   `tree_sitter::Language`.
> - **The synchronous reference path to mirror:** `crates/language/src/language.rs:1026-1054`
>   `Language::highlight_text(text: &Rope, range) -> Vec<(Range<usize>, HighlightId)>`:
>   `parse_text` (`:1287`) runs `tree_sitter::Parser` over the rope (chunk callback — don't
>   materialize the whole string) → one `Tree`; the `highlights.scm` `Query` runs via `QueryCursor`
>   (`syntax_map.rs:938` `single_tree_captures`); innermost capture wins per byte range
>   (`buffer.rs:5770` maps capture index → highlight, `Chunk` at `:5830`).
> - **`highlights.scm`:** the highlight query (`(type_identifier) @type`, …). Copy Zed's Rust query
>   from `crates/grammars/src/rust/highlights.scm` into our crate and load it as a
>   `tree_sitter::Query`. (Mirror `grammar.rs:47` `HighlightsConfig { query, … }`.)
> - **The capture→color SEAM (the one place we adjust Zed):** Zed's `HighlightId(NonZeroU32)` /
>   `HighlightMap` (`crates/language_core/src/highlight_map.rs:3-6`, gpui-free) is **theme-relative** —
>   it is built by `build_highlight_map(query.capture_names(), theme)` (`language.rs:1098`) where
>   `theme.highlight_id(name)` lives in `syntax_theme/src/syntax_theme.rs` and **`use gpui::HighlightStyle`
>   (gpui-coupled)**. Since our core has **no theme**, the `syntax` crate must emit the **capture
>   identity**, not a theme id: return `(Range<usize>, capture_index)` plus expose the language's
>   `capture_names() -> &[&str]`, so the **frontend** builds the `capture → color` map. This keeps the
>   exact Zed split (capture identity in the language layer, color in the theme/gpui layer) while
>   honoring the gpui-free rule. **Do NOT pull in Zed's theme-built `HighlightMap` here** — it needs a
>   theme we deliberately don't have in core.

## Files this plan owns
```
crates/syntax/Cargo.toml              # new crate; deps: rope (path), tree-sitter 0.26.9, tree-sitter-rust 0.24.2
crates/syntax/src/lib.rs              # pub: Lang enum, Highlight span type, capture_names(), highlight()
crates/syntax/src/highlight.rs        # parse rope -> tree -> run highlights.scm -> Vec<(Range, capture_idx)>
crates/syntax/queries/rust/highlights.scm   # copied verbatim from zed-main/crates/grammars/src/rust/highlights.scm
Cargo.toml (workspace)                # members += "crates/syntax"; default-members += "crates/syntax" (it IS engine, gpui-free, fast to test)
Cargo.lock                            # tree-sitter + grammar deps (shared-state hazard — this plan only, in this wave)
devlog/009-syntax-engine.md
```
**No `alteria-core` files, no `alteria-gpui` files.** The crate is consumed by the frontend in a later
(wave-2) plan that adds the `capture → color` theme and colored `TextRun`s.

## Parallelism (why this is safe to run concurrently)
009 is the **syntax lane**: brand-new files plus the workspace `Cargo.toml`/`Cargo.lock`. It shares
**no source file** with 007 (render) or 008 (core logic). In this wave **only 009 touches
`Cargo.toml`/`Cargo.lock`** (007 adds no deps; 008 adds no deps), so the manifest merge hazard is
uncontended — **007 ∥ 008 ∥ 009 merge to `main` with zero conflicts.** (General rule still holds:
manifest edits route through the Planner; here the Planner has confirmed 009 is the sole manifest
writer this wave. The Reviewer owns the final `members` list at merge as always.)

## Tasks (TDD — a `.rs` fixture, assert spans; fully headless)

### T0 — Crate scaffold + deps + it parses (`Cargo.toml`, `lib.rs`)
- New `crates/syntax` lib crate. `Cargo.toml`: `rope = { path = "../rope" }`,
  `tree-sitter = "0.26.9"`, `tree-sitter-rust = "0.24.2"`. Add `crates/syntax` to the workspace
  `members` **and** `default-members` (it is gpui-free engine code — it belongs in the fast
  `cargo test` loop, unlike `alteria-gpui`).
- `lib.rs`: `pub enum Lang { Rust }`; a span type `pub struct Highlight { pub range: Range<usize>,
  pub capture: u32 }` (capture = the query's capture index — our gpui-free identity, NOT a theme id);
  `pub fn capture_names(lang: Lang) -> Vec<&'static str>` (from the loaded `Query::capture_names()`).
- Smoke test: build a `tree_sitter::Parser`, `set_language(&tree_sitter_rust::LANGUAGE.into())`,
  parse `"fn main() {}"`, assert a non-null root node. Confirms the pinned versions link & parse.
- **Verify:** `cargo build -p syntax`; `cargo test -p syntax` runs.
- **Commit:** `feat(syntax): scaffold crates/syntax; tree-sitter 0.26.9 + rust 0.24.2 parse`.

### T1 — `highlights.scm` + the highlight query (`queries/`, `highlight.rs`)
- Copy `zed-main/crates/grammars/src/rust/highlights.scm` **verbatim** into
  `crates/syntax/queries/rust/highlights.scm` (embed via `include_str!`). Note its provenance + that
  it is verbatim in a header comment / the devlog (same discipline as the vendored `rope`/`text`).
- Build the `tree_sitter::Query` once from the grammar + that string (mirror Zed's
  `HighlightsConfig`). Expose its `capture_names()` through `capture_names(Lang::Rust)`.
- **Verify:** the query compiles against `tree-sitter-rust` 0.24.2 (a mismatched grammar/query version
  fails here — that's the check); `capture_names()` is non-empty and stable.
- **Commit:** `feat(syntax): load Zed's Rust highlights.scm as a tree-sitter Query`.

### T2 — `highlight(&Rope, Lang) -> Vec<Highlight>` (`highlight.rs`)
- Mirror `Language::highlight_text` / `parse_text`: parse the **rope** via the parser's chunk callback
  (`parser.parse_with(|byte, _| rope.chunk_at(byte)…)` — confirm the exact tree-sitter 0.26.9
  callback signature against the crate) so we never materialize the whole document; one `Tree`.
- Run the highlight `Query` with a `QueryCursor` over the tree; for each capture emit
  `Highlight { range: node.byte_range(), capture: capture.index }`. Resolve overlaps the way Zed does
  — **innermost/last capture wins** for a given byte (a small stack or last-write-wins per range);
  return spans **sorted by start, non-overlapping** (the frontend paints them left-to-right per line).
- Byte offsets throughout (our coordinate model); ranges are buffer byte ranges, directly usable by
  the renderer.
- **Tests (headless, the heart of the plan):** a small Rust fixture
  (e.g. `fn main() { let x = 1; }`) → assert specific `(byte_range, capture_name)` pairs by mapping
  `capture` back through `capture_names()` — e.g. `fn`→`keyword`, `main`→`function`, `x`… Cover: a
  keyword, an identifier/type, a number/string literal, and a comment; assert spans are sorted &
  non-overlapping; empty input → no spans; non-Rust-ish text → still parses (tree-sitter is
  error-tolerant) without panicking.
- **Commit:** `feat(syntax): highlight(rope, Rust) -> sorted (byte_range, capture) spans`.

### T3 — `devlog/009`
- Record: the exact pins (`tree-sitter 0.26.9`, `tree-sitter-rust 0.24.2`) and why they match Zed; the
  Zed path mirrored (`language.rs::highlight_text`/`parse_text`, `single_tree_captures`,
  `highlights.scm`); that `highlights.scm` is **verbatim Zed**; the **capture-identity-not-color**
  decision and the Zed seam it preserves (`language_core` HighlightId/`HighlightMap` vs gpui-coupled
  `syntax_theme`), plus the adjustment that our core emits the capture index (theme lives frontend);
  and what is deferred (incremental reparse, injections, `SyntaxMap` layering, async).
- **Verify:** `cargo test -p syntax` green; `cargo test`/`build`/`clippy` on default-members (now
  including `syntax`) clean & fast; `cargo tree -p syntax | grep -i gpui` empty (the crate is
  gpui-free); `cargo tree -p alteria-core` unchanged (008/009 didn't entangle the core).
- **Commit:** `docs(devlog): record 009 — headless syntax engine (tree-sitter, Zed subset)`.

## Done criteria
- `crates/syntax` builds and `highlight(&Rope, Lang::Rust)` returns **sorted, non-overlapping**
  `(byte_range, capture_index)` spans for Rust, driven by Zed's verbatim `highlights.scm` and the
  pinned `tree-sitter 0.26.9` / `tree-sitter-rust 0.24.2`.
- The crate is **gpui-free** and emits **capture identity, never color** (theme/color is the
  frontend's job — Zed's `language` vs `theme` split, with the theme-coupled `HighlightMap` correctly
  left out of core).
- `crates/syntax` is in `members` **and** `default-members` (it's fast headless engine code); the
  engine `cargo test` loop stays green & fast and **gpui-free**.
- Headless unit tests assert real capture spans on a Rust fixture; no GUI, no network.
- `devlog/009` records the pins, the mirrored Zed path, the verbatim query, and the deferred layers.

## Non-goals (later plans)
Frontend wiring & the `capture → color` theme map · colored `TextRun` painting · more languages than
Rust · incremental reparse on edit · language injections (JS-in-HTML etc.) · multi-layer `SyntaxMap` ·
background/async parsing & budgets · selection/bracket-match highlighting · semantic (LSP) highlighting.
009 only produces correct spans for one language, synchronously.

## Notes
- **Match Zed's pins exactly** (`tree-sitter 0.26.9`, `tree-sitter-rust 0.24.2`) — a version skew
  between grammar and query is the classic breakage; T1 surfaces it. Verify the `parse_with` chunk-
  callback signature against the actual `tree-sitter` 0.26.9 API (it has changed across releases).
- `highlights.scm` is **vendored verbatim from Zed** — treat it like the vendored `.rs` (don't edit;
  record provenance).
- Build with the shared cache (`CARGO_TARGET_DIR=~/.cache/alteria-target`); tree-sitter's first
  compile isn't free.
- This is the syntax lane: no shared source file with 007/008; sole manifest writer this wave →
  parallel-safe.
