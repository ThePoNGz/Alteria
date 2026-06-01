# Devlog 003 — Motion fidelity (grapheme motion · goal column · CharKind word model)

**Date:** 2026-06-01
**Plan:** `plans/003-motion-fidelity.md`
**Branch:** `alteria_a1`
**Status:** ✅ Complete. **171 tests pass**; `cargo fmt --check`, `cargo clippy -p alteria-core --no-deps -D warnings`, and the build are clean; **no new dependency**; **no `gpui`** anywhere in the crate or its tree (`rope` + `sum_tree` only).

---

## 1. What this chunk is

Three motion refinements that bring cursor movement to Zed/VS Code parity, all pure
`alteria-core` logic (gpui-free, headless-tested):

1. **Grapheme-correct horizontal motion + backspace** — Left/Right and `Backspace` step by
   whole grapheme cluster, never splitting a combining mark, flag, or ZWJ emoji.
2. **Goal-column vertical motion** — Up/Down remember the column and restore it after passing
   through a short line.
3. **Three-class `CharKind` word model** — word motion and selection-expansion share one
   classifier; `E`/`Q` port Zed's `next_word_end` / `previous_word_start`.

Behavior parity with Zed was the spec — every algorithm is ported from `zed-main`, not
re-derived. Sources read verbatim: `crates/editor/src/movement.rs` (`left`/`right`,
`up_by_rows`/`down_by_rows`, `next_word_end`/`previous_word_start`,
`find_boundary_point`/`find_preceding_boundary_point`) and `crates/language/src/buffer.rs`
(`CharKind`, `CharClassifier::kind_with`). Grapheme snapping rides the already-vendored rope
(`ChunkSlice::clip_point` + `unicode_segmentation::GraphemeCursor`).

## 2. Tasks / commits

| Task | Commit | What |
|---|---|---|
| T1 | `feat(core): CharKind classifier + boundary scanners (Zed-parity)` | New `char_kind.rs`: `CharKind {Whitespace, Punctuation, Word}`, `char_kind(c)`, and `find_boundary`/`find_preceding_boundary` (offset-based ports of Zed's scanners). |
| T2 | `feat(core): word motion on the three-class CharKind model` | `word_right` → `next_word_end`, `word_left` → `previous_word_start`, both via the scanners. `expand.rs`'s `is_word` now routes through `char_kind` so expansion and motion agree on what a word is. |
| T3 | `feat(core): grapheme-aware horizontal motion + backspace` | `grapheme_left`/`grapheme_right` (Zed `movement::left/right`: step one column, snap with `clip_point` + `Bias`). `char_horizontal` and `delete_backward` use them; removed the now-dead `prev_boundary`. |
| T4 | `feat(core): goal-column memory for vertical motion (SelectionGoal::Column)` | `Range` gains `goal: Option<u32>` + `Range::new`. `move_all` threads the goal: vertical motion keeps the un-clamped column, every other motion (and edits) reset it to `None`. |
| T5 | `test(core): e2e motion-fidelity coverage` (+ this devlog) | `Editor::handle` e2e: grapheme step, goal-column survival, word-end. |

## 3. Key implementation decisions

### Grapheme motion goes through `Point`, not `clip_offset`
The rope's `clip_offset` snaps only to **UTF-8 char boundaries**; the **grapheme**-aware clip
lives on the `Point` path (`Rope::clip_point` → `ChunkSlice::clip_point`, which spins a
`GraphemeCursor`). So `grapheme_left/right` mirror Zed exactly: `offset_to_point` → adjust the
column (wrapping rows at line edges) → `clip_point(…, Bias::Left/Right)` → `point_to_offset`.
`Bias` is `sum_tree::Bias` (the rope doesn't re-export it); `alteria-core` already depends on
`sum_tree`, so no new dependency. `Backspace` = `grapheme_left` then delete the span (Zed's
`backspace` = `movement::left` then delete).

### Goal column stores the *un-clamped* column
`vertical_goal` lands at `min(goal, target_line_len)` but returns `Some(goal_col)` — the
original column, **not** the clamped landing column — so a later longer line restores it. The
goal is seeded from the current column when `None`, computed once and reused across a multi-row
`count`. It's a **byte** column (rope columns are bytes); Zed's pixel-x `SelectionGoal` variants
need font metrics and stay deferred to the frontend. `move_range_head` resets the goal to `None`
for every non-vertical motion, and edits build cursors via `Range::cursor` (goal `None`) —
matching Zed resetting `SelectionGoal` on horizontal motion and edits.

### Word motion: `E` = word **end**, `Q` = word **start** (ported from Zed)
Per the plan and the maintainer's call, `E` (`WordStart(Right)`) ports Zed `next_word_end` and
`Q` (`WordStart(Left)`) ports `previous_word_start`, including Zed's first-step rule that steps
over leading/trailing punctuation (`|.foo`→`.foo|`, `bar.|`→`|bar.`). Both build on the shared
`find_boundary`/`find_preceding_boundary` scanners with the predicate
`kind(left) != kind(right) && !whitespace(boundary-side)`.

`expand.rs` keeps its **bracket-aware** stepping (expansion must stop at brackets to defer to
bracket-pair growth); only the *word-character definition* was unified onto `char_kind`
(`alphanumeric || '_'` ≡ `CharKind::Word`, so behavior is identical and every expand test stays
green).

## 4. Notes for the Reviewer ⚠️

1. **Word-motion semantics now differ from the KEYMAP/`action.rs` prose.** `KEYMAP.md:32` reads
   "`E` → move to start of next word" and `action.rs:23-24` documents `E`/`Q` as next/previous
   word **start**. The implemented behavior is now word **end** / word **start** (Zed
   `next_word_end`/`previous_word_start`), per the maintainer's decision that the keymap lists
   *bindings* while the plan defines *behavior*. I did **not** edit `KEYMAP.md` (outside this
   worktree) or `action.rs`/`keymap.rs` (not owned by plan 003). Consider updating that prose so
   the docs match: `E` lands on a word's trailing edge, `Q` on a word's leading edge.

2. **Tests intentionally flipped — two beyond the one the plan called out.** The plan noted only
   the goal-column test would flip. Because `next_word_end ≠ next_word_start`, two word tests
   also changed value (and name): `"foo bar"` from 0 is now **3** (end of "foo", was 4), and
   `"foo bar baz"` from 1 is now **3** (was 4). `word_left_to_prev_word_start` was unchanged
   (`previous_word_start` already matched). The goal-column test
   (`vertical_clamps_column_to_shorter_line` → `vertical_goal_column_clamps_then_restores`) now
   asserts column **restoration** across a short line.

3. **One-line cross-boundary edit in `history.rs` (Plan 005's file).** Adding `Range.goal` breaks
   the 2-field struct literal `Range { anchor: 0, head: 5 }` in a `history.rs` test, so the crate
   wouldn't compile. I changed only that one line to `Range::new(0, 5)` (behavior-identical).
   Plan 005 owns/rewrites `history.rs`; resolve this trivially at merge (prefer 005's version).

## 5. Verification

- `cargo test -p alteria-core` → **171 passed**, 0 failed.
- `cargo fmt -p alteria-core -- --check` → clean.
- `cargo clippy -p alteria-core --no-deps --all-targets -- -D warnings` → clean (the 6 warnings
  shown are pre-existing ones from the vendored `rope` crate, not `alteria-core`).
- Boundary: `grep gpui crates/alteria-core/src` finds only the rule's own doc comments — no
  import. `cargo tree -p alteria-core` = `rope` + `sum_tree` only; **no new dependency** (no
  `Cargo.toml` change).
- Edges covered: empty buffer (all motions no-op, no panic), buffer start/end clamps, vertical
  no-op at first/last line, grapheme clip at cluster boundaries.

## 6. Deferred (unchanged from the plan's scope)

- Pixel-x `SelectionGoal` (variable-width columns) — needs frontend font metrics.
- Incremental/`Point`-space optimization of the scanners — `chars_at`/`reversed_chars_at` walks
  are fine for "feels instant on my files".
- Language-scoped word characters (e.g. CSS `-` as a word char) — the `LanguageScope` branch of
  Zed's classifier is dropped until tree-sitter syntax lands.
