# Plan 003 — `alteria-syntax`: tree-sitter highlighting, theme & language detection

**Goal:** Build the **meaning layer** — a pure, headless crate that turns buffer text into
**colored style-spans**: detect the language from a path, parse with **tree-sitter**, run a
highlight query, and emit `Vec<StyleSpan>` plus a `Theme` mapping each highlight kind to a color.
No window, no GPUI, fully unit-tested (`cargo test -p alteria-syntax`).

This plan **runs in parallel with Plan 002** (`alteria-gpui`, the interactive frontend). The two
own **disjoint files** (separate crates) and **neither touches `alteria-core/src`**. They meet at
exactly one frozen interface (below); the **Reviewer writes the ~15-line adapter at merge** that
feeds these spans into 002's renderer. Build this crate **blind to 002** — verified entirely by
its own unit tests.

> **Read first — do not contradict:**
> - `../CLAUDE.md` — the **one hard rule** generalized: this crate, like `alteria-core`, is
>   **frontend-agnostic and never imports `gpui`** (keeps Floem a cheap fallback and keeps this
>   unit-testable). Stack table: **syntax highlighting = tree-sitter**. Licensing: tree-sitter +
>   grammars are permissive (MIT/Apache) — fine to depend on; this is our own crate.
> - `crates/alteria-core/src/selection.rs` / `buffer.rs` — the coordinate model this must match:
>   **byte offsets** into the buffer, ropey `Rope` is the text. `StyleSpan.range` is **byte
>   offsets**, same space as `Range`. (tree-sitter nodes are byte-indexed too — they line up.)
> - `../../KEYMAP.md` — context only: `KEYMAP.md` defers the **deeper `O`/`P` expansion levels**
>   (string contents, syntactic nodes) to tree-sitter. Those are **out of scope here** (this plan
>   is highlighting only); but building the tree-sitter foundation is what later unblocks them.

---

## Why this is the clean parallel chunk
A text editor's frontend (window + render + input + files) is one tightly-coupled crate — you
can't parallelize render-from-input. **Highlighting is the one big piece that genuinely is
separable:** it's a pure transform `text → spans`, it touches none of the frontend's files, and
it's verifiable headless. So it's the second agent's whole track while Plan 002 builds the window.

---

## The 002↔003 interface (frozen — this crate OWNS these types)
```rust
// crates/alteria-syntax/src/style.rs  — plain data, no gpui, no ropey-in-the-type
pub struct StyleSpan { pub range: core::ops::Range<usize>, pub kind: HighlightKind } // byte offsets
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HighlightKind {            // a small, theme-friendly, language-agnostic set
    Keyword, Function, Type, String, Number, Comment, Constant,
    Variable, Operator, Punctuation, Property, Namespace, Attribute, Boolean,
}
pub struct Rgba { pub r: f32, pub g: f32, pub b: f32, pub a: f32 } // 0.0..=1.0, plain data

// crates/alteria-syntax/src/theme.rs
pub struct Theme { /* HighlightKind -> Rgba */ }
impl Theme { pub fn default_dark() -> Self; pub fn color(&self, kind: HighlightKind) -> Rgba; }

// crates/alteria-syntax/src/highlight.rs
pub fn highlight(text: &ropey::Rope, lang: Language) -> Vec<StyleSpan>; // sorted, non-overlapping
```
- **Plan 002 does NOT import this crate.** Its renderer paints from its own
  `Vec<(Range<usize>, gpui::Hsla)>`, default empty.
- **The Reviewer's adapter** (added to `alteria-gpui` at merge, ~15 lines): on buffer change,
  `highlight(&buffer.text, lang)` → map each `StyleSpan` through `Theme::color` → `gpui::Hsla` →
  set 002's field. That adapter is the *only* coupling, and it's the Reviewer's, not either
  executor's.
- **Keep `Rgba` plain (no gpui).** The frontend converts `Rgba`→`gpui::Hsla` in the adapter.

---

## Scope

### In
- A **library crate** `alteria-syntax` (no binary, no gpui), depending on `ropey 1.6` (match the
  core's pin), `tree-sitter`, the **`tree-sitter-highlight`** helper, and grammar crates.
- The frozen interface types above.
- **Language detection:** `Language` enum + `detect(path: &Path) -> Option<Language>` by file
  extension (first-line/shebang sniffing optional).
- **Highlighting:** `highlight(&Rope, Language) -> Vec<StyleSpan>` via tree-sitter, with capture
  names mapped to `HighlightKind`; spans **sorted by start, non-overlapping** (resolve precedence
  so the renderer can paint them in order without overlap logic).
- A **default dark `Theme`**.
- **Initial language: Rust** (we're writing Rust — instant dogfood). One more (e.g. JSON or TOML)
  if cheap; otherwise leave the enum open for later grammars.

### Out — deferred
- **Incremental reparse** (feed tree-sitter the previous tree + an edit for sub-ms updates) — v1
  does a **full parse**; tree-sitter is fast enough for "feels instant on my files." Expose a
  shape that *allows* adding incrementality later, but don't build it now.
- **Injections / locals** beyond what `tree-sitter-highlight` gives for free; semantic tokens;
  LSP; the deeper `O`/`P` syntactic expansion levels (a future core-side plan).
- **Theme file format / config** (a serialization decision) — a built-in default only.
- Anything frontend (painting, GPUI) — that's Plan 002 + the Reviewer's adapter.

---

## Files this plan owns
**Touches no `alteria-gpui` file and no `alteria-core/src` file.** Shared hotspots are only the
workspace manifest + lockfile (also touched by 002 — the Reviewer merges the two `members` lines
and regenerates `Cargo.lock`).

```
Cargo.toml                                  # [workspace] members += alteria-syntax  (HOTSPOT, shared w/002)
Cargo.lock                                  # tree-sitter + grammar deps             (HOTSPOT, shared w/002)
crates/alteria-syntax/Cargo.toml            # ropey + tree-sitter + tree-sitter-highlight + grammars
crates/alteria-syntax/src/lib.rs            # pub mod + re-export the frozen interface
crates/alteria-syntax/src/style.rs          # StyleSpan, HighlightKind, Rgba
crates/alteria-syntax/src/lang.rs           # Language enum + detect(path) + grammar/query wiring
crates/alteria-syntax/src/highlight.rs      # highlight(&Rope, Language) -> Vec<StyleSpan>
crates/alteria-syntax/src/theme.rs          # Theme + default_dark
```

---

## Architecture rules (do not deviate)
- **No `gpui`, ever** (same rationale as the core). No frontend types. Plain data + pure
  functions; the public surface is exactly the frozen interface.
- **Byte-offset coordinates** throughout — `StyleSpan.range` is byte offsets into the same buffer
  the engine edits. tree-sitter is byte-indexed, so no conversion drama; just don't mix in char
  indices.
- **`highlight()` returns sorted, non-overlapping spans.** Resolve tree-sitter capture precedence
  here so the renderer is dumb (paint span by span). Gaps (unhighlighted text) are fine — the
  renderer uses the default foreground there.
- **One concept per file.** Pure functions; the only "state" is an owned parser/query inside
  `highlight` (construct per call in v1, or lazily cache the `HighlightConfiguration` — but keep
  the public fn pure in/out).

## Conventions (`../CLAUDE.md`, every task)
- **TDD** (this crate is fully headless, so real TDD applies — like the core): failing test →
  red → implement → green → `cargo fmt && cargo clippy --all-targets -D warnings` → commit.
- **No `unwrap()`/`panic!` on reachable paths:** unknown extension → `None`/plain; empty buffer →
  empty `Vec`; a grammar/query that fails to load → return empty spans (degrade to plain text,
  never panic). Grammar load failures are a *programming* error surfaced in tests, not a runtime
  panic.
- One concept per file. Commit `feat(syntax): …` / `chore: …`. **Never** an AI/`Co-Authored-By`
  trailer; author stays the human git user.
- **Build cost:** use the shared `CARGO_TARGET_DIR=~/.cache/alteria-target`. (This crate is small,
  but grammars compile C — first build is non-trivial; the shared cache helps.)

## Tree-sitter — verify the current API (use Context7 + docs before writing)
The agent's tree-sitter knowledge may be stale. Before writing, **pull current docs via Context7**
(resolve `tree-sitter` / `tree-sitter-highlight` / `tree-sitter-rust`) and the crate docs, and
**pin exact crate versions** in `Cargo.toml`. Confirm: `Parser::set_language`, the
`tree-sitter-rust` language accessor (e.g. `tree_sitter_rust::LANGUAGE` / `language()` — versions
differ), the `HIGHLIGHTS_QUERY` constant name, and the `tree_sitter_highlight::{Highlighter,
HighlightConfiguration, HighlightEvent}` API (the recommended path — it resolves precedence and
yields `HighlightStart{Highlight(idx)} / Source{start,end} / HighlightEnd` over a fixed list of
recognized highlight names you supply). Record the exact versions + any API deviations in the
devlog.

---

## Tasks (TDD throughout; each independently verifiable headless)

### Task 1 — Crate scaffold + the frozen interface types
**Files:** `Cargo.toml`, `crates/alteria-syntax/Cargo.toml`, `…/src/lib.rs`, `…/src/style.rs`
- Add `"crates/alteria-syntax"` to workspace `members` (expected hotspot with 002 — Reviewer
  merges). Crate `Cargo.toml`: `license = "GPL-3.0-or-later"`, deps `ropey = "1.6"`,
  `tree-sitter = "<pin>"`, `tree-sitter-highlight = "<pin>"`, `tree-sitter-rust = "<pin>"`.
- `style.rs`: `StyleSpan`, `HighlightKind` (the enum above), `Rgba` — exactly the frozen
  interface. `lib.rs`: `pub mod` + re-exports.
- **Tests:** trivial constructors/derives compile; `HighlightKind` is `Copy`+`Hash` (theme keys).
- **Commit:** `chore: scaffold alteria-syntax + frozen style interface`.

### Task 2 — Language detection
**Files:** `…/src/lang.rs`, `…/src/lib.rs`
- `enum Language { Rust, /* + any second grammar */ }`; `pub fn detect(path: &Path) ->
  Option<Language>` by extension (`.rs` → Rust). Unknown → `None`.
- **Tests (TDD):** `detect("a.rs") == Some(Rust)`; `detect("a.unknown") == None`;
  case-insensitive extension; a path with no extension → `None`.
- **Commit:** `feat(syntax): language detection by extension`.

### Task 3 — `highlight()` for Rust via tree-sitter (the core of the plan)
**Files:** `…/src/highlight.rs`, `…/src/lang.rs`, `…/src/lib.rs`
- Wire the grammar + `HIGHLIGHTS_QUERY` into a `HighlightConfiguration` with a fixed list of
  recognized highlight names; map each recognized name → `HighlightKind`
  (`"keyword"`→`Keyword`, `"function"`/`"function.method"`→`Function`, `"type"`→`Type`,
  `"string"`→`String`, `"comment"`→`Comment`, `"number"`→`Number`, `"constant*"`→`Constant`,
  `"operator"`→`Operator`, `"punctuation*"`→`Punctuation`, `"property"`→`Property`,
  etc. — list lives in `lang.rs`).
- `highlight(&Rope, Language)`: get the text (v1: `rope.to_string()` — note the allocation,
  optimize with a chunk callback later), run the highlighter, fold the `HighlightEvent` stream
  into **sorted, non-overlapping** `StyleSpan`s (innermost capture wins; emit a span only when a
  highlight is active; leave gaps unstyled). Empty buffer / unknown → empty `Vec`. **Never panic**
  on a query/grammar error — return empty.
- **Tests (TDD on a known snippet), e.g.** `fn main() { let x = 1; /* c */ "s" }`:
  - `fn` is `Keyword`; `main` is `Function`; `1` is `Number`; `"s"` is `String`; `/* c */` is
    `Comment`; spans are sorted, non-overlapping, and within buffer bounds.
  - empty buffer → `[]`; a buffer of only whitespace → `[]`.
- **Commit:** `feat(syntax): tree-sitter highlight() for Rust -> sorted style spans`.

### Task 4 — Default dark theme
**Files:** `…/src/theme.rs`, `…/src/lib.rs`
- `Theme { /* HighlightKind -> Rgba */ }`, `default_dark()`, `color(kind) -> Rgba`. Every
  `HighlightKind` maps to a sensible color; an unmapped kind falls back to a default foreground.
- **Tests:** `default_dark().color(Keyword) != color(Comment)`; every `HighlightKind` returns a
  color (exhaustive match — no panic).
- **Commit:** `feat(syntax): default dark theme (HighlightKind -> Rgba)`.

### Task 5 — (optional, if cheap) a second grammar + verify the kind-mapping generalizes
**Files:** `…/src/lang.rs`, `…/src/highlight.rs`, `…/src/Cargo.toml`
- Add one more grammar (e.g. JSON/TOML) to prove the capture-name→`HighlightKind` mapping isn't
  Rust-specific. Skip if it risks the timeline — Rust alone satisfies the plan.
- **Tests:** a known snippet of the second language highlights its keywords/strings/numbers.
- **Commit:** `feat(syntax): add <lang> grammar`.

### Task 6 — Verify, boundary audit, devlog
**Files:** `devlog/003-syntax-highlighting.md`
- `cargo test -p alteria-syntax` green; `cargo fmt --check`; `cargo clippy --all-targets -D
  warnings`.
- **Boundary audit:** `cargo tree -p alteria-syntax` shows **no `gpui`**; the public API is
  exactly the frozen interface.
- **Devlog `003`:** the exact pinned tree-sitter + grammar crate versions, the recognized
  highlight-name list and the name→`HighlightKind` mapping, the precedence/sorting approach, the
  `rope.to_string()` allocation (and the incremental/chunk-callback follow-up), and a one-line
  **note to the Reviewer**: the adapter to add in `alteria-gpui` is
  `highlight(&buf.text, lang).into_iter().map(|s| (s.range, theme.color(s.kind).into_hsla()))…`.
- **Commit:** `feat(syntax): boundary audit + verify` + `docs(devlog): add devlog 003`.

---

## Done criteria
- `cargo test -p alteria-syntax` passes; `cargo fmt --check` + `cargo clippy -D warnings` clean.
- `highlight(&Rope, Rust)` returns **sorted, non-overlapping, byte-offset** `StyleSpan`s correct
  for a known snippet; `detect` maps extensions; `Theme::default_dark` colors every kind.
- **No `gpui` anywhere** in the crate or its tree; the public surface is exactly the frozen
  interface (so the Reviewer's adapter is mechanical).
- No `unwrap()`/`panic!` on reachable paths (unknown lang, empty buffer, grammar/query failure all
  degrade gracefully). Pinned versions + name mapping recorded in `devlog/003`.

## Self-review (before opening for review)
1. **Frontend-agnostic:** no `gpui`, no frontend types; pure `text → spans`. ✓
2. **Coordinates:** byte offsets, same space as the engine's `Range`; spans sorted &
   non-overlapping. ✓
3. **tree-sitter current:** API + versions verified via Context7/docs and pinned; deviations in
   the devlog. ✓
4. **Tests:** TDD; `cargo test -p alteria-syntax` green; highlighting asserted on real snippets. ✓
5. **Clean build:** `fmt`/`clippy -D warnings` clean. ✓
6. **Edges:** unknown extension, empty/whitespace buffer, grammar/query load failure — all
   no-panic, graceful. ✓
7. **Interface honored:** public surface == the frozen `StyleSpan`/`HighlightKind`/`Rgba`/`Theme`/
   `highlight` contract; the Reviewer's adapter note is in the devlog. ✓

## Executor notes
- Worktree (e.g. `alteria_a2`). **Sync first — reset, never rebase:** `git fetch origin && git
  reset --hard origin/main && git clean -fd`. Then `export CARGO_TARGET_DIR=~/.cache/alteria-target`.
- TDD task by task, commit per task, **push your own branch** (never `main`).
- **You run in parallel with Plan 002.** Do **not** touch `alteria-gpui` or `alteria-core/src`.
  Build **blind to the frontend** — your only contract is the frozen interface, verified by your
  own unit tests. The Reviewer wires your `highlight`+`Theme` into 002's renderer at merge.
- Use **Context7** for current tree-sitter API before writing; pin exact crate versions.
