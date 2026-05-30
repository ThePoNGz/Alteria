# Devlog 001 — Core Backend (`alteria-core`)

**Date:** 2026-05-30
**Plan:** `plans/001-core-backend.md`
**Status:** ✅ Complete. The entire pure-Rust editing engine is built, reviewed, and on `main`. **155 tests pass; `cargo fmt`, `cargo clippy -D warnings`, and `cargo build` are clean; no `gpui` anywhere in the tree.**

---

## 1. What this chunk is

This is the **engine, not the UI** — the part of Alteria that turns input events into edits, with zero knowledge of rendering. It is the realization of the pipeline from CLAUDE.md, every stage a pure function over plain data:

```
InputEvent → Resolver(held mods) → Action → Executor → Buffer (+ Selection) / History
```

A frontend's only job will be the first translation (OS event → `InputEvent`) and the last render (read `Buffer` → draw). Everything between is done and headless-tested.

The single entry point a frontend calls is `Editor::handle(InputEvent) -> bool` (the bool = "should repaint").

## 2. Modules built (one concept per file)

| Module | Responsibility |
|---|---|
| `input` | `InputEvent` enum + plain-data `Key`, `Modifiers` (`NONE`, `is_none`). No GUI types. |
| `action` | `Action` (intent, decoupled from keys), `Direction`, `Motion`, `Expansion`. |
| `selection` | `Range { anchor, head }` (byte offsets) + `Selection { ranges, primary }`; `overlaps`, `normalize` (sort + merge overlaps, track the primary). |
| `buffer` | `Buffer { text: ropey::Rope, selection }`. |
| `transaction` | **The spine.** `ChangeSet { ops, len_before }` of `Op::{Retain,Delete,Insert}` byte lengths; `from_changes`, `apply` (validates then mutates all-or-nothing), `invert`, `map_pos`, `Assoc::{Before,After}`. |
| `keymap` | `Layer { bindings }`, `Keymap { layers: Modifiers → Layer }`, `default_alteria` (Alt / Alt+Shift / Ctrl / Alt+Ctrl). Declarative, remappable. |
| `resolver` | The state machine. Holds the only mutable input state — `held` mods, pending `count`, `find` sub-mode. `resolve((held, key)) → Option<Action>`. |
| `history` | Revision-tree undo: `Transaction { forward, inverse, selection_before/after }`, `Revision { parent, children }`, `History::{commit,undo}`. Redo-ready. |
| `executor` | `apply(action, &mut Buffer, &mut History)` — motions, transaction-based edits, multicursor. |
| `expand` | `expand(text, range, kind)` — delimiter-matched bracket-level selection growth (I/O/U/P). |
| `find` | `find_on_line(text, head, ch, forward)` — inline char search on the current line. |
| `lib` | The `Editor` facade tying resolver + keymap + buffer + history together; `handle`. |

### Behaviors implemented (all the pure-logic parts of the keymap)

- **Base layer:** type to insert, `Enter`, `Backspace`, `Esc` (collapse selection / cursors).
- **Alt layer:** hold-Alt + `WASD` inverted-T navigation; `Q/E` word, `Z/C` line edges, `[ ]` blank-line leaps, `R` matching bracket.
- **Alt+Shift:** the same motions, but *extend* the selection instead of collapsing.
- **Counts:** `Alt`+digits accumulate a repeat count for the next motion.
- **Find:** `Alt+F` captures a target char, jumps to it on the line; `D`/`A` repeat forward/back.
- **Expansion:** `I/O/U/P` grow the selection by bracket levels.
- **Multicursor:** spawn (provisional), and one edit applied atomically across all cursors.
- **Undo:** `Ctrl+Z` walks the revision tree; selection-expansion steps are history entries too.

## 3. How it was built

- **TDD throughout**, per CLAUDE.md: write the failing test → run red → implement → run green → `fmt`/`clippy` → commit. Every algorithmic module went stub → red → green, verified non-vacuous.
- **Zed + Helix referenced for *approach only*** (honoring "refer to Zed" + the GPL line): Zed's `text`/anchor/transaction architecture and Helix's `ChangeSet` algorithm shaped the transaction layer (byte-indexed Retain/Delete/Insert with `apply`/`invert`/`map_pos`). **No Zed GPL editor source was pasted** — ideas/architecture only.
- **Context7 + research** used to de-risk the spine before building on it.

## 4. Key decisions & deviations from the plan

1. **Built `history` before the `executor`** (reordered from the plan). The executor commits transactions to history from its very first edit, so history had to exist first — avoids a retrofit.
2. **`FindRepeat { forward }` → `FindRepeat { ch, forward }`.** The plan's sketch couldn't work: on repeat the executor wouldn't know *which* char to re-find. The resolver now carries the captured target through.
3. **`SpawnCursor` is provisional** (KEYMAP marks multicursor "not yet specified"): the new cursor becomes primary so repeated spawns build a column. Flagged in code to revisit when the spec lands.
4. **Expansion scope = brackets `()[]{}` only.** Quote/string-contents pairing is deferred to tree-sitter (a context problem), consistent with KEYMAP deferring "string contents."
5. **Coordinate model:** `Range` stores **byte** offsets; ropey 1.6 is char-indexed, so every rope op bridges via `byte_to_char`/`char_to_byte`. Motion is by `char` for now (grapheme clusters later) — a deliberate scope choice, not a gap.
6. **`from_changes` emits Delete-before-Insert** so that `map_pos` lands a position in the interior of a replaced region on the insert's right edge, not clamped to the deletion start.
7. **`transaction::apply` validates char boundaries up front** and mutates all-or-nothing. (A review claimed a panic here; I empirically disproved it — ropey *rounds* a mid-codepoint byte rather than panicking — but added the validation anyway so a malformed changeset is cleanly rejected instead of silently editing at a rounded spot.)

## 5. Quality gates — adversarial review found real bugs, all fixed

Because this is the foundation ("nothing goes wrong here"), the engine went through **three** review passes, not one.

### During the build — two adversarial review workflows
- One on the **transaction spine** before anything was built on it, one on the **whole engine** at the end. Between them they found **9 real correctness issues**, each fixed with a regression test (count-0-digit reset, count leaking across find, `R` inside a pair, `normalize` mistracking the primary, expansion storing an un-normalized overlap, backward-outermost expansion not a no-op, the `map_pos` replace-interior bug, etc.).

### Final pass — `/code-review` (xhigh, 9 angles, 57 agents)
Reviewed `origin/main...HEAD` (~3,677 lines). It surfaced **6 distinct defects** (several found independently by 3–5 angles), all now fixed test-first:

| # | Defect | Severity | Fix |
|---|---|---|---|
| 1 | Typing over a selection didn't replace it (inserted past the span) | High (spec violation) | `apply_edit` now takes explicit per-range cursor targets; typing deletes `[min,max)` and lands the caret past the insert for either orientation. |
| 2 | Repeat-count `count*10+d` overflowed `usize` → **panic in debug** on a held/auto-repeated `Alt`+digit | High (no-panic rule) | Saturating arithmetic; a huge count just clamps at the buffer edge. |
| 3 | `find_move` discarded all secondary cursors | Med | Find now maps over every cursor on its own line, like every other motion. |
| 4 | `blank_line` jumped to EOF on files ending in `\n` (ropey's phantom trailing line treated as blank) | Med | `nav_line_count` excludes the phantom line; real trailing blank lines still reachable. |
| 5 | `MatchingBracket` + a count oscillated between the pair's two ends | Low (altitude) | It's an involution, so it runs once regardless of count. |
| 6 | `invert` silently dropped recovered text if a Delete ran past `original.len()` | Low (defensive) | `debug_assert`s the invariant in debug/test; release stays panic-free. |

The review also independently **confirmed** the non-negotiables: no `gpui` import, no panics on input-reachable paths, pure pipeline.

## 6. The Backspace decision (a core-concept call)

The reviewers flagged that Backspace over a selection deleted only one char. The literal (parent) keymap spec said "delete the grapheme before head," but that conflicts with Alteria's core principle: **with no modifier held, Alteria is an ordinary editor.** Decision (confirmed with the author): **Backspace deletes the whole selection** when one exists (caret lands at the span start); a bare cursor still deletes the preceding char. This is now in code + tests, and the "no-Alt = a normal editor; accessibility first" principle was written into CLAUDE.md as a first-class concept, with the old milestone ("M0"/"post-M0") plan markers stripped out.

## 7. Verification (final)

- **155 tests pass** (`cargo test -p alteria-core`).
- `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo build` — all clean, zero warnings.
- Dependency tree: `ropey 1.6.1` (+ its `smallvec`, `str_indices`) — **no `gpui`, no GUI/render crate.** The only `gpui` strings in the source are the doc-comments stating the rule.
- No `unwrap()`/`expect()`/`panic!` on input-reachable paths; buffer start/end, empty buffer, and modifier release (the no-stuck-quasimode guarantee) are handled.

## 8. Commit trail (this milestone)

```
06d30d7  chore: scaffold cargo workspace (alteria-core)
3460083  feat(core): input and action types
acb1b45  feat(core): byte-offset selection model (multicursor-ready)
03b3223  feat(core): ropey buffer + current selection
7b87369  feat(core): changeset transaction layer (apply/invert/map)
19fe6a8  feat(core): layered keymap (Alt / Alt+Shift / Ctrl / Alt+Ctrl)
b616259  feat(core): resolver state machine — held mods, count, find sub-mode
170a9b3  feat(core): revision-tree undo (Ctrl+Z), expansion steps included
8f34ffe  feat(core): executor — motions and transaction-based edits (single cursor)
b1f1080  feat(core): multicursor — one edit over all ranges, map + merge
49dd29e  feat(core): delimiter-based selection expansion (I/O/U/P)
dd4bb59  feat(core): inline char search (Alt+F find sub-mode)
2e9ca2d  feat(core): Editor facade + end-to-end engine tests
b192e24  fix(core): resolve adversarial-review findings
6ba5dc8  fix(core): resolve code-review findings (span-replace, count overflow, find multicursor, blank-line phantom, bracket count)
e3213af  fix(core): Backspace deletes the selection when one exists
cacb4f6  docs: make "no-Alt = a normal editor" a core concept; drop milestone artifacts
cf97c26  chore: track plan 001 (core backend) in plans/
```

## 9. What is NOT done / next

- **No UI.** The engine can't draw anything or receive a real keystroke yet. You can't run or *feel* the editor until there's a window.
- **Next milestone — the GPUI frontend (plan 002):** a window that renders the buffer's text + cursor(s)/selection, and translates OS events (`on_key_down`/`on_key_up`/**`on_modifiers_changed`** for bare Alt, focus-lost) into `InputEvent`s fed to `Editor::handle`. That bare-modifier event is the primitive the whole quasimode concept rides on. First task there: pin a specific GPUI/Zed SHA and write against *that rev's* real API (not memory), verify Vulkan on the machine.
- Deferred by design (post-this-milestone, not in scope here): grapheme-cluster motion + column memory, tree-sitter highlighting, clipboard/redo/save bindings, a serialized keymap config file.
