# Devlog 009 — Syntax-highlight engine: a headless `crates/syntax` (tree-sitter)

**Date:** 2026-06-03
**Plan:** `plans/009-syntax-engine.md`
**Branch:** `alteria_a3`
**Status:** ✅ Complete. New `crates/syntax` parses buffer text with **tree-sitter** and returns
`highlight(&Rope, Lang::Rust) -> Vec<Highlight>` — **sorted, non-overlapping** `(byte_range,
capture_index)` spans, driven by Zed's **verbatim** Rust `highlights.scm`. The crate is **gpui-free**,
emits **capture identity, never color**, makes **zero `alteria-core` changes**, and is now in both
workspace `members` **and** `default-members` (fast headless engine code). 6 headless tests; the
`default-members` `build`/`test`/`clippy` loop stays green, fast, and gpui-free.

This is the **engine half** of syntax highlighting. The frontend half (capture → theme color, colored
`TextRun`s) is a later wave-2 plan; nothing here touches `alteria-gpui`.

---

## 1. The pins (match Zed exactly) and the one feature we drop

| Crate | Pin | Source of truth |
|---|---|---|
| `tree-sitter` | `0.26.9` | `zed-main/Cargo.toml:772` — Zed enables `features = ["wasm"]`; **we drop it** (no WASM grammars; native synchronous parse only). |
| `tree-sitter-rust` | `0.24.2` | `zed-main/Cargo.toml:792`. Grammar ships separately, exposing `tree_sitter_rust::LANGUAGE` (a `LanguageFn`) `.into()`-converted to a `tree_sitter::Language`. |
| `streaming-iterator` | `0.1` | `zed-main/Cargo.toml:742`. tree-sitter's `QueryCaptures` is a `StreamingIterator`; iterating captures needs this trait in scope. |

**Why pin to Zed's exact versions:** a grammar/query version skew is the classic tree-sitter breakage
(a query that references a node kind the grammar renamed fails to compile). T1's query-build test is
exactly that guard. `cargo` resolved `tree-sitter-language v0.1.7` transitively; nothing else added.

This wave **only 009 touches `Cargo.toml`/`Cargo.lock`** (007 render + 008 core add no deps), so the
manifest merge hazard was uncontended — parallel-safe as planned.

## 2. The Zed path mirrored (read the approach, reproduce on our stack)

The synchronous reference is `Language::highlight_text` (`crates/language/src/language.rs:1026-1054`):
*parse the rope → run `highlights.scm` over the single tree → innermost capture wins per byte.* We
reproduce each step:

- **Parse (`highlight.rs::parse`)** mirrors `parse_text` (`language.rs:1287`). We feed the rope to
  `tree_sitter::Parser` through its **chunk callback** so the whole document is never materialized as
  one `String`:
  ```rust
  let mut chunks = text.chunks_in_range(0..text.len());
  parser.parse_with_options(
      &mut move |offset, _| { chunks.seek(offset); chunks.next().unwrap_or("").as_bytes() },
      None, None)
  ```
  This is **byte-for-byte Zed's `parse_text` body** — our `rope` is vendored verbatim from Zed, so
  `Chunks::seek`/`next` exist with the same semantics. (Note: the current API is
  `parse_with_options`, *not* the older `parse_with` the plan sketched — verified against the real
  0.26.9 crate and Zed's own call site.)
- **Run the query** with a `QueryCursor` over the tree's root, the way Zed's `single_tree_captures`
  (`syntax_map.rs:938`) does for a single layer. tree-sitter applies the query's `#match?` **text
  predicates** for us, reading node text through a `TextProvider` — our `TextProvider`/`ByteChunks`
  are Zed's (`syntax_map.rs:255`, `:2104`), wrapping `rope::Chunks` as `&[u8]`. (This is why `Foo`
  → `@type`, `MAX` → `@constant`, but `main`/`x` stay below those uppercase/all-caps predicates.)
- **Resolve overlaps** with Zed's `BufferChunks` **stack** (`buffer.rs:5749` pop, `:5762` push,
  `:5805` the active highlight is `stack.last()`), reduced to a batch over the whole buffer: captures
  sorted by **start ascending, end descending** (Zed's `SyntaxMapCaptures::sort_key`), then a left-to-
  right walk that pops captures whose `end <= pos`, pushes those that have started, and emits up to the
  next boundary tagged with the top of the stack. **Innermost / last-matching capture wins**, exactly
  as Zed renders. The output coalesces contiguous same-capture runs and is sorted + non-overlapping.

## 3. `highlights.scm` is **verbatim Zed**

`crates/syntax/queries/rust/highlights.scm` is copied **byte-for-byte** from
`zed-main/crates/grammars/src/rust/highlights.scm` (`diff` is empty; 260 lines) and embedded with
`include_str!` (mirrors Zed's `HighlightsConfig { query, .. }`, `grammar.rs:47`). Treated like the
vendored `rope`/`text` `.rs`: **do not edit — re-vendor from upstream** so future Zed diffs line up.
Provenance lives here and in a doc comment on the `RUST_HIGHLIGHTS` const (the `.scm` itself is left
untouched to stay diff-clean). The query defines the captures we assert against — `@keyword`,
`@function.definition`, `@variable`, `@number`, `@string`, `@comment`, `@type`, … 29 in all.

## 4. The capture → color **seam** — the one place we adjust Zed

This is the deliberate deviation, and it is forced by the **gpui-free core** rule, not invented:

- Zed's `HighlightId(NonZeroU32)` / `HighlightMap` (`language_core/src/highlight_map.rs:3-6`) is
  **theme-relative** — built by `build_highlight_map(query.capture_names(), theme)`
  (`language.rs:1098`), where `theme.highlight_id(name)` lives in `syntax_theme/src/syntax_theme.rs`
  and **`use gpui::HighlightStyle`** — i.e. the capture→id step is **gpui-coupled**.
- Our engine has **no theme** and must not import gpui. So `syntax` emits the **capture index** (the
  query's stable, theme-free identity) as `Highlight { range, capture }`, and exposes
  `capture_names(Lang) -> Vec<&'static str>` (straight from `Query::capture_names()`). The **frontend**
  will build the `capture → color` map against its theme.

This keeps Zed's exact split — **capture identity in the `language` layer, color in the `theme`/gpui
layer** — while honoring the one hard rule. We deliberately **do not** pull in Zed's theme-built
`HighlightMap`: it needs a theme we don't have in core. (See `[[gpui-free-rule-mirrors-zed]]`.)

## 5. What we deliberately left out (the perf/scale layers)

Faithful **subset**, not a re-derivation — these are explicit non-goals (CLAUDE.md: *"feels instant on
the files I actually open"*, not huge-file engineering):

- **No parser/cursor pooling.** Zed reuses parsers and query cursors via `with_parser` /
  `QueryCursorHandle` thread-local pools. We build a fresh `Parser`/`QueryCursor` per call — simpler,
  and a whole-buffer parse of "files I actually open" is already sub-millisecond.
- **No incremental reparse** (`old_tree = None` every call), **no language injections** (single tree,
  no JS-in-HTML), **no multi-layer `SyntaxMap`**, **no async/background parsing or budgets**, **one
  language** (Rust). Also out: selection/bracket-match and semantic (LSP) highlighting.

## 6. Verification

| Check | Result |
|---|---|
| `cargo build -p syntax` | clean |
| `cargo test -p syntax` | **6 passed** (smoke parse · query compiles · the span fixture · sorted/non-overlapping · empty input · garbage-tolerant) |
| `cargo build` (default-members, now incl. `syntax`) | clean, ~4s |
| `cargo test` (default-members) | **alteria-core 152 + syntax 6**, green, fast |
| `cargo clippy` (default-members) | clean (no warnings; one `collapsible_if` fixed to a let-chain) |
| `cargo fmt -p syntax -- --check` | clean |
| `cargo tree -p syntax \| grep -i gpui` | **empty** — the crate is gpui-free ✓ |
| `cargo tree -p alteria-core \| grep -iE gpui\|tree-sitter\|ropey` | **empty** — 008/009 didn't entangle the core ✓ |

The span fixture (`// greet\nfn main() {\n  let x = 1;\n  let s = "hi"; }`) asserts real
`(byte_range, capture_name)` pairs: `fn`/`let`→`keyword`, `main`→`function.definition`, `x`→`variable`,
`1`→`number`, `"hi"`→`string`, `// greet`→`comment`. `main` is the interesting one — it is captured by
**both** the catch-all `(identifier) @variable` and the specific `@function.definition`; the stack/sort
resolution makes the specific one win, matching how Zed paints it. That single assertion validates both
the overlap rule and that `#match?` predicates are actually applied (else the uppercase/all-caps
identifier rules would mis-fire).

## 7. Files

```
crates/syntax/Cargo.toml                    # new crate; rope (path) + tree-sitter 0.26.9 + rust 0.24.2 + streaming-iterator 0.1
crates/syntax/src/lib.rs                    # Lang, Highlight { range, capture }, capture_names(), pub use highlight; + tests
crates/syntax/src/highlight.rs              # config/Query load + parse(rope) + highlight() stack walk + TextProvider
crates/syntax/queries/rust/highlights.scm   # VERBATIM zed-main/crates/grammars/src/rust/highlights.scm
Cargo.toml (workspace)                      # members += syntax; default-members += syntax (gpui-free engine, fast loop)
Cargo.lock                                  # tree-sitter + tree-sitter-rust + tree-sitter-language + streaming-iterator
```

No `alteria-core` files, no `alteria-gpui` files — consumed by the frontend in a later wave-2 plan.
