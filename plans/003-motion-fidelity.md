# Plan 003 — Motion fidelity (grapheme motion · goal column · CharKind word model)

**Goal:** Make cursor motion behave **exactly like Zed** by (1) moving horizontally by **grapheme**, (2) giving vertical motion a **goal column** that survives short lines, and (3) using Zed's **three-class `CharKind`** word model. Pure `alteria-core` logic — gpui-free, **no new dependencies** (uses the already-vendored `rope` grapheme primitives + std `char`).

**Parallelism:** Runs **simultaneously with Plan 004** (disjoint files). Owns only `alteria-core/src/*` listed below; does **not** touch any `Cargo.toml`, the vendored crates, or `buffer.rs`/`transaction.rs`/`history.rs` (those are Plan 005's). Plan 005 depends on this plan's `selection.rs` goal field.

> **Sources of truth:** `../CLAUDE.md`, `../KEYMAP.md`, and Zed at `../zed-main`:
> - `crates/editor/src/movement.rs` — `left`/`right` + clip (`:39-74`), `up_by_rows`/`down_by_rows` (`:119-190`), `previous_word_start` (`:266`, the word-**start** boundary rule — Alteria reuses it in both directions; see T2), the generic `find_boundary`/`find_preceding_boundary_point` scanners (`:672-753`). (Note: Zed's forward word motion is `next_word_end` at `:441` — word-*end* — which Alteria does **not** use.)
> - `crates/language/src/buffer.rs` — `enum CharKind` (`:579-588`), `CharClassifier::kind_with` (`:6066-6097`).
> - Already vendored locally: `crates/rope/src/rope.rs` `clip_offset`/`clip_point`+`Bias` (`:534-559`), `chars_at`/`reversed_chars_at` (`:339-345`), `offset_to_point`/`point_to_offset` (`:396-464`); `crates/rope/src/chunk.rs` grapheme `clip_point` (`:587-621`). `Bias` is `sum_tree::Bias` (rope doesn't re-export it); `alteria-core` **already depends on `sum_tree`** (added in Plan 002 for exactly this), so `use sum_tree::Bias;` directly — no new dependency.

## Files this plan owns
```
crates/alteria-core/src/char_kind.rs    # NEW — CharKind enum + classifier + boundary scanners
crates/alteria-core/src/executor.rs     # horizontal (grapheme), vertical (goal), word motion
crates/alteria-core/src/selection.rs    # add goal field to Range
crates/alteria-core/src/expand.rs       # converge word/grapheme scanning on char_kind
crates/alteria-core/src/lib.rs          # pub mod char_kind; e2e tests
```
Does **not** touch `Cargo.toml` (no new dep), `buffer.rs`, `find.rs` (unless a shared line helper is needed read-only), or any vendored crate.

## Tasks

### T1 — `char_kind` module (the classifier + scanners)
**Files:** `src/char_kind.rs` (new), `src/lib.rs` (`pub mod char_kind;`)
- Port Zed's classes verbatim (scope-less — drop the `LanguageScope`/tree-sitter branch, which is deferred until syntax):
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum CharKind { Whitespace, Punctuation, Word }
  /// Mirror of Zed CharClassifier::kind_with (language/src/buffer.rs:6066), scope-less.
  pub fn char_kind(c: char) -> CharKind {
      if c.is_alphanumeric() || c == '_' { CharKind::Word }
      else if c.is_whitespace() { CharKind::Whitespace }
      else { CharKind::Punctuation }
  }
  ```
- Port the generic boundary scanners (Zed `movement.rs:672-753`) as pure helpers over the rope, taking a predicate `is_boundary: FnMut(char /*left*/, char /*right*/) -> bool`:
  ```rust
  pub fn find_boundary(text: &Rope, from: usize, is_boundary: impl FnMut(char,char)->bool) -> usize;          // forward
  pub fn find_preceding_boundary(text: &Rope, from: usize, is_boundary: impl FnMut(char,char)->bool) -> usize; // backward
  ```
  Implement by walking `chars_at(from)` / `reversed_chars_at(from)` tracking the byte offset, stopping when `is_boundary(prev, next)` (handle buffer ends as boundaries).
- **Tests:** `char_kind` classifies `a`/`_`→Word, ` `/`\t`→Whitespace, `.`/`(`→Punctuation; `find_boundary` stops at a kind transition.
- **Commit:** `feat(core): CharKind classifier + boundary scanners (Zed-parity)`.

### T2 — Word motion via CharKind
**Files:** `src/executor.rs`, `src/expand.rs`
- **Alteria's `E`/`Q` are word-START motions** (`action.rs`: `WordStart(Right)` = start of the *next* word, `WordStart(Left)` = start of the *previous* word; there is **no** word-end motion — KEYMAP.md: "end-of-word is just `E` then `A`"). ⚠️ Zed's `movement.rs` has `previous_word_start` (`:266`) and `next_word_end` (`:441`) but **no `next_word_start`** (Zed's forward word motion goes to word-*end*, macOS-style) — so **do not use `next_word_end`**. Rewrite `word_right`/`word_left` (executor) with `char_kind` + the T1 scanners using the **word-start predicate** (kind changes *and* the right char is non-whitespace → the start of a new `Word`/`Punctuation` run): apply it **backward** for `Q` (this *is* Zed's `previous_word_start`, `:266`) and the **same predicate forward** for `E`. Punctuation is now its own class, so `.`/`(` are stops (the old alnum-only model skipped them).
- Route `expand.rs`'s internal word helpers (`is_word`, `word_span`, and its expansion-local `next_word_end`/`prev_word_start` — these find word **spans for selection expansion**, *not* cursor motion) through the shared `char_kind` classifier, so expansion and motion agree on what a word is. (Its `next_word_end` is an expansion span-finder — not the cursor word-end motion Alteria deliberately doesn't have.)
- **Tests:** `"foo.bar"` — `E` (`WordStart(Right)`) from col 0 → the `.` (col 3, start of the punctuation run) → `bar` (col 4); `"foo bar"` — `E` from 0 → col 4 (`bar`), whitespace skipped; `Q` from col 4 in `"foo bar"` → col 0 (`foo` start); expansion word level matches the classifier.
- **Commit:** `feat(core): word-start motion (E/Q) on the three-class CharKind model`.

### T3 — Grapheme-correct horizontal motion
**Files:** `src/executor.rs`
- Rewrite `char_horizontal` and `next_boundary`/`prev_boundary` to step by **grapheme** (Zed `movement::left/right`, `:39-74`): compute the candidate offset, then snap to a grapheme boundary via the rope's grapheme-aware clip (`clip_offset`/`clip_point` + `Bias::Left`/`Right`). A simple correct form: step one codepoint, then loop `clip` until the offset is a stable grapheme boundary; or operate in `Point` and `clip_point`.
- Fix `delete_backward` (`:83`) to delete the whole **previous grapheme**, mirroring Zed `backspace` (`editor.rs:4738`: `movement::left` then delete).
- **Tests:** `"e\u{0301}"` (e + combining acute) — `Char(Right)` from 0 lands at byte 3 (past the cluster), not byte 1; `delete_backward` at end removes both bytes; an emoji ZWJ sequence isn't split.
- **Commit:** `feat(core): grapheme-aware horizontal motion + backspace`.

### T4 — Goal column for vertical motion
**Files:** `src/selection.rs`, `src/executor.rs`
- Add a goal to the cursor. Minimal Zed-faithful form: `Range { anchor: usize, head: usize, goal: Option<u32> }` (a byte/visual **column**; Zed's pixel-x `SelectionGoal` variants need font metrics → deferred to the frontend). Add a constructor so existing call sites stay terse: `Range::cursor(pos)` / `Range::new(anchor, head)` set `goal: None`. Update Range literals in this plan's owned files to use the constructor (keeps churn inside 003).
- `vertical` (executor `:339`): if `goal` is set, target that column on the new row (clamped to `line_content_len`); else compute the column from the current point and **store it as the goal**, returning it so the next up/down reuses it (Zed `movement.rs:127-132`).
- **Reset the goal to `None` on every horizontal motion and on edits** (Zed `editor.rs:4765,4788`).
- **Update the test** `vertical_clamps_column_to_shorter_line` (executor `:761`) to assert column **restoration**: cursor col 3 → down through a 2-col line → down again lands at col 3 (the Zed behavior; the old test only asserted the clamp).
- **Commit:** `feat(core): goal-column memory for vertical motion (SelectionGoal::Column)`.

### T5 — Green + e2e
**Files:** `src/lib.rs`
- Add/extend e2e tests: a navigate sequence that exercises grapheme + goal-column + word motion through `Editor::handle`.
- **Verify:** `cargo test -p alteria-core` green; `cargo fmt --check` + `cargo clippy -p alteria-core --no-deps` clean.
- **Commit:** `test(core): e2e motion-fidelity coverage`.

## Done criteria
- Horizontal motion + backspace are grapheme-correct; vertical motion preserves the goal column through short lines; word motion stops at Word↔Punctuation boundaries (Zed/VSCode behavior).
- All motion logic mirrors `zed-main/crates/editor/src/movement.rs`; classifier mirrors Zed's `CharKind`.
- `cargo test -p alteria-core` green; clippy/fmt clean; **no new dependency added**; no `gpui`.

## Executor notes
- Behavior parity is the spec — port from Zed `movement.rs`, don't re-derive. Existing tests are the safety net (except the one goal-column test you intentionally flip in T4 — note it in the devlog).
- Write `devlog/003`.
