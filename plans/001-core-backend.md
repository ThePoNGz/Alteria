# Plan 001 — The complete `alteria-core` backend

> **⚠️ Stack update (post-execution):** this plan was executed on `ropey`. The text-buffer choice has since changed — Alteria now uses **Zed's vendored, byte-indexed `rope`** (see `CLAUDE.md` → Stack, and **`plans/002`**, the migration). Everything below that mentions `ropey`, the `byte_to_char`/`char_to_byte` bridge, or "convert at the ropey boundary" is **obsolete — read it as history**. The pipeline, keymap, resolver, selection model, and all KEYMAP behaviors are unchanged.

**Goal:** Build the entire pure-Rust editing engine — the whole input→intent→action pipeline,
every `KEYMAP.md` behavior that is pure logic, a transaction layer, revision-tree undo, and
multicursor — test-first, ending green under `cargo test -p alteria-core`. After this plan the
full editor *logic* is proven with **no window**: type, navigate, extend, expand, find,
multi-edit, and undo, all headless and unit-tested.

This crate is governed by the project's **one hard rule**: `alteria-core` **never imports
`gpui`** — no rendering, no GPU, no optimization, no Zed render code. The heavy
render/perf/Zed-reference work lives entirely in the *frontend* (`alteria-gpui`) and is plan
002+. This plan is plain data + pure functions only.

> **Sources of truth (do not contradict):** `../CLAUDE.md` (architecture, the two hard rules,
> conventions, the parallel-agent workflow) and `../KEYMAP.md` (the exact behavior of every
> binding — the behavioral spec this plan implements). `CLAUDE.md` is explicit that context
> lives in those two files only; the old `DESIGN.md` is gone. Locked decisions still in force:
> **byte-offset coordinates**; multicursor = `Vec<Range>`, one edit applies to all; char
> movement for M0 (grapheme later). The undo model is migrating to Zed's `text`/`clock` (anchor
> + operation log) — see `plans/002`. This plan does not restate the keymap — read `KEYMAP.md`
> for each binding and implement it.

---

## Scope

### In — the complete in-memory editing engine (all pure logic, all decision-free)
- **Full pipeline**, one concept per module: `input · action · selection · buffer ·
  transaction · keymap · resolver · executor · expand · find · history · lib`.
- **Base layer** (no modifier): printable → `InsertChar`, `Enter` → `InsertNewline`,
  `Backspace` → `DeleteBackward`, `Esc` → collapse selection span **and** collapse
  multicursors to the primary.
- **Alt layer — motions** (collapsing): `W S A D`, `Q`/`E` (prev/next word start), `Z`/`C`
  (line start/end), `R` (matching bracket, nesting-aware, across lines), `[`/`]` (prev/next
  blank-line leap).
- **Alt+Shift layer:** the same motions, **extend** (keep `anchor`, move `head`).
- **Repeat count:** `Alt+1`..`9` accumulate `N` (digits accumulate → 12); next motion runs
  `N` times then clears; releasing Alt clears `N`.
- **Find sub-mode:** `Alt+F` + char → jump to next occurrence on the current line; while Alt
  held `D` = next, `A` = previous; no match = no-op; releasing Alt or any non-`A`/`D` key ends it.
- **Selection expansion** `I` `O` `U` `P` — the **delimiter-matched levels only** (`KEYMAP.md`
  defers the deeper tree-sitter levels to later): `I` enclosing span climbing out, `U` same but
  biased left, `O` bracket **content** climbing, `P` brackets alternating content/delimiters.
- **Transaction / changeset** layer — the spine (see Architecture).
- **Undo** via **revision-tree** history (`Ctrl+Z`). Expansion steps (`I`/`O`/`U`/`P`) are
  history entries even with no text change, so `Ctrl+Z` steps back through them too
  (`KEYMAP.md`). Structure so **redo** is a cheap later add (binding deferred).
- **Multicursor:** `Selection = Vec<Range>`; one `Action` → one `ChangeSet` over all ranges in
  the **old** coordinate space → applied once → all ranges **mapped** to the new space →
  overlaps **merged**.

### Out — explicitly deferred (each needs a decision or an external resource)
- **tree-sitter** highlighting — needs grammar/ABI decisions; `KEYMAP.md` itself defers the
  deeper expansion levels to it.
- **config / keymap-file** serialization — a format decision; a feature, not the engine.
- **file open/save** — `DESIGN.md §9` flags v1 file-scope as an *unresolved* open question.
- **clipboard** copy/cut/paste and other deferred edit verbs (redo binding, indent/dedent,
  save) — `KEYMAP.md` "Deferred"; clipboard is an OS resource.
- the **GPUI frontend** (`alteria-gpui`) — plan 002; no UI here.
- **grapheme-cluster movement + column-memory** — M0 moves by `char` (`DESIGN.md`); later refinement.

---

## Files this plan owns
No other plan may touch these until 001 is merged (they include the merge hotspots in
`CLAUDE.md`: workspace `Cargo.toml`, `Cargo.lock`, `alteria-core/src/lib.rs`).

```
Cargo.toml                              # [workspace]
crates/alteria-core/Cargo.toml
crates/alteria-core/src/lib.rs          # Editor facade + pub mod list + e2e tests
crates/alteria-core/src/input.rs
crates/alteria-core/src/action.rs
crates/alteria-core/src/selection.rs
crates/alteria-core/src/buffer.rs
crates/alteria-core/src/transaction.rs
crates/alteria-core/src/keymap.rs
crates/alteria-core/src/resolver.rs
crates/alteria-core/src/executor.rs
crates/alteria-core/src/expand.rs
crates/alteria-core/src/find.rs
crates/alteria-core/src/history.rs
```

---

## Architecture the executor must follow (do not deviate)
- **One concept per module file** (`CLAUDE.md`). Plain data + pure functions. The only mutable
  state is the `Resolver` (held mods + pending count + find sub-state) and the
  `Buffer`/`History`.
- **Edits flow through `transaction` from the first editing task.** Even a single-cursor
  `InsertChar` produces a `ChangeSet` applied once — never mutate the rope ad-hoc. This is what
  makes undo and multicursor fall out naturally instead of being retrofits. Do **not** take the
  M0 shortcut of mutating the rope directly.
- **Coordinate convention:** `Range` and the `ChangeSet` both operate in **UTF-8 byte offsets**
  — one coordinate space across selection + transaction. *(Superseded by `plans/002`: on Zed's
  byte-indexed `rope` there is **no** char-index boundary to convert at; the old
  `text.byte_to_char(b)` bridge is deleted, offsets stay byte offsets end to end, and boundaries
  are handled with `rope`'s `is_char_boundary` / `clip_offset` + `Bias`.)*
- **The one hard rule:** no `gpui` in the dep tree at all. Keep it that way.

---

## Conventions (from `CLAUDE.md`, enforced every task)
- **TDD:** failing test → run red → implement → run green → `cargo fmt && cargo clippy` clean → commit.
- **No `unwrap()`/`panic!` on input-reachable paths.** Explicit edges: buffer start/end, empty
  buffer, first/last line, modifier release, focus lost, overlapping-range merge, find with no
  match, expansion at the outermost level, undo at the history root.
- One concept per module; small files.
- Commit style `feat(core): …` / `chore: …`. **Never** add a `Co-Authored-By`/AI/"Generated
  with" trailer. Author stays the human git user.

## Prerequisites
- `rustup default stable` (present: cargo 1.92.0). **No system/Vulkan deps** — those arrive
  with the GPUI frontend in plan 002.

---

## Tasks

> Each task: **Files** · **Types/signatures** (concrete sketches — refine under test) ·
> **Tests** (intent + key assertions; encode `KEYMAP.md` behavior) · **Commit**. `pub mod`
> lines go in `lib.rs`. Run `cargo test -p alteria-core <filter>` red→green each task.

### Task 1 — Workspace + crate scaffold
**Files:** `Cargo.toml`, `crates/alteria-core/Cargo.toml`, `crates/alteria-core/src/lib.rs`
- Root `Cargo.toml`: `[workspace]`, `resolver = "2"`, `members = ["crates/alteria-core"]`.
- Crate `Cargo.toml`: `name = "alteria-core"`, `version = "0.0.0"`, `edition = "2021"`,
  `[dependencies] ropey = "1.6"`. *(Superseded — `plans/002` replaces `ropey` with the vendored
  `rope` crate; do not add `ropey` in new work.)*
- `src/lib.rs`: empty (modules added per task).
- **Verify:** `cargo build` green. **Commit:** `chore: scaffold cargo workspace (alteria-core)`.

### Task 2 — `input` + `action` types
**Files:** `src/input.rs`, `src/action.rs`, `src/lib.rs`
- `input.rs` (plain data, no GUI types):
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
  pub struct Modifiers { pub alt: bool, pub ctrl: bool, pub shift: bool, pub super_key: bool }
  impl Modifiers {
      pub const NONE: Modifiers = Modifiers { alt:false, ctrl:false, shift:false, super_key:false };
      pub fn is_none(self) -> bool { self == Modifiers::NONE }
  }
  #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
  pub enum Key { Char(char), Backspace, Enter, Escape }
  #[derive(Clone, Copy, PartialEq, Debug)]
  pub enum InputEvent {
      KeyDown { key: Key, mods: Modifiers, repeat: bool },
      KeyUp   { key: Key, mods: Modifiers },
      ModifiersChanged { mods: Modifiers },
      FocusLost,
  }
  ```
- `action.rs` (intent, decoupled from keys):
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum Direction { Up, Down, Left, Right }
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum Motion {
      Char(Direction),      // W/S/A/D
      WordStart(Direction), // Q = Left, E = Right
      LineEdge(Direction),  // Z = Left (start), C = Right (end)
      BlankLine(Direction), // [ = Up, ] = Down
      MatchingBracket,      // R
  }
  /// I/U/O/P — see KEYMAP.md for exact climbing semantics.
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum Expansion { Enclosing, EnclosingLeft, BracketContent, BracketAlternating }
  #[derive(Clone, PartialEq, Debug)]
  pub enum Action {
      InsertChar(char),
      InsertNewline,
      DeleteBackward,
      CollapseSelection,                                   // Esc
      Move { motion: Motion, extend: bool, count: usize }, // resolver fills count>=1
      FindChar { ch: char },                               // Alt+F then char
      FindRepeat { forward: bool },                        // A/D while find active
      Expand(Expansion),                                   // I/U/O/P
      SpawnCursor(Direction),                              // Alt+Ctrl W/S (provisional)
      Undo,                                                // Ctrl+Z
  }
  ```
- **Tests:** `Modifiers::NONE.is_none()`; `Modifiers{alt:true,..NONE}.is_none()==false`.
- **Commit:** `feat(core): input and action types`.

### Task 3 — `selection`
**Files:** `src/selection.rs`, `src/lib.rs`
- ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub struct Range { pub anchor: usize, pub head: usize } // byte offsets
  impl Range {
      pub fn cursor(pos: usize) -> Self { Range { anchor: pos, head: pos } }
      pub fn is_empty(&self) -> bool { self.anchor == self.head }
      pub fn min(&self) -> usize { self.anchor.min(self.head) }
      pub fn max(&self) -> usize { self.anchor.max(self.head) }
      pub fn overlaps(&self, other: &Range) -> bool { /* by min/max */ }
  }
  #[derive(Clone, PartialEq, Debug)]
  pub struct Selection { pub ranges: Vec<Range>, pub primary: usize }
  impl Selection {
      pub fn at(pos: usize) -> Self { Selection { ranges: vec![Range::cursor(pos)], primary: 0 } }
      pub fn primary(&self) -> Range { self.ranges[self.primary] }
      /// sort ranges by min, merge overlaps, keep the primary pointing at the merged primary.
      pub fn normalize(&mut self);
  }
  ```
- **Tests:** bare cursor empty; `overlaps`; `normalize` merges two overlapping ranges into one
  and preserves the primary; `normalize` on a single range is a no-op.
- **Commit:** `feat(core): byte-offset selection model (multicursor-ready)`.

### Task 4 — `buffer`
**Files:** `src/buffer.rs`, `src/lib.rs`
- ```rust
  use ropey::Rope;
  use crate::selection::Selection;
  pub struct Buffer { pub text: Rope, pub selection: Selection }
  impl Buffer { pub fn from_str(s: &str) -> Self { Buffer { text: Rope::from_str(s), selection: Selection::at(0) } } }
  ```
- **Tests:** `from_str("abc").text.len_bytes()==3`; selection starts at a bare cursor at 0.
- **Commit:** `feat(core): ropey buffer + current selection`.

### Task 5 — `transaction` (the spine)
**Files:** `src/transaction.rs`, `src/lib.rs`
- A Helix-style changeset, byte-based at char boundaries:
  ```rust
  #[derive(Clone, PartialEq, Eq, Debug)]
  pub enum Op { Retain(usize), Delete(usize), Insert(String) } // lengths in bytes
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum Assoc { Before, After } // which side a position sticks to at an insert boundary
  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct ChangeSet { pub ops: Vec<Op>, pub len_before: usize } // len_before in bytes
  impl ChangeSet {
      pub fn apply(&self, text: &mut ropey::Rope);          // mutate (convert byte→char at ropey edges)
      pub fn invert(&self, original: &ropey::Rope) -> ChangeSet; // inverse, for undo
      pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize;  // map a byte offset through the change
  }
  ```
- **Tests (pure, in isolation):**
  - `apply` an insert / a delete / a mixed set → resulting rope text correct.
  - `invert` then apply restores the original text (round-trip) for insert, delete, and mixed.
  - `map_pos` shifts a position after an insert; clamps inside a deletion; `Assoc::Before` vs
    `After` differ exactly at an insert boundary.
  - Multi-byte char (e.g. `é`) edits keep byte offsets char-aligned.
- **Commit:** `feat(core): changeset transaction layer (apply/invert/map)`.

### Task 6 — `keymap` with all layers
**Files:** `src/keymap.rs`, `src/lib.rs`
- `Layer { bindings: HashMap<Key, Action> }`, `Keymap { layers: HashMap<Modifiers, Layer> }`,
  `lookup(mods, key) -> Option<Action>`.
- `default_alteria()` builds:
  - **Alt:** `w/s/a/d`→`Move{Char(..),false,1}`, `q/e`→`WordStart`, `z/c`→`LineEdge`,
    `r`→`MatchingBracket`, `[`/`]`→`BlankLine`; `i`→`Expand(Enclosing)`,
    `u`→`Expand(EnclosingLeft)`, `o`→`Expand(BracketContent)`, `p`→`Expand(BracketAlternating)`.
  - **Alt+Shift:** the same motions with `extend:true`. Expansions are **not** motions, so they
    are identical to the Alt layer (per `KEYMAP.md`).
  - **Ctrl:** `z`→`Undo`.
  - **Alt+Ctrl:** `w`→`SpawnCursor(Up)`, `s`→`SpawnCursor(Down)` (provisional — see Task 9).
  - **Not in the keymap** (resolver specials, Task 7): digits `1`–`9` (count), `Alt+F` and the
    `A`/`D` find-repeats (find sub-mode), and the base-layer keys (handled by resolver fallback).
- **Tests:** `lookup(alt, Char('w'))` is `Move{Char(Up),false,1}`; `lookup(alt_shift, Char('w'))`
  has `extend:true`; `lookup(ctrl, Char('z'))==Some(Undo)`; `lookup(NONE, Char('w'))==None`.
- **Commit:** `feat(core): layered keymap (Alt / Alt+Shift / Ctrl / Alt+Ctrl)`.

### Task 7 — `resolver` (state machine: held + count + find)
**Files:** `src/resolver.rs`, `src/lib.rs`
- State: `held: Modifiers`, `count: usize` (0 = none), and a find sub-state:
  ```rust
  enum FindState { Inactive, Pending, Active { ch: char } }
  ```
- `resolve(event, &keymap) -> Option<Action>`:
  - `ModifiersChanged { mods }` → `held = mods`. If Alt no longer held: clear `count`; if find
    was `Active`/`Pending`, set `Inactive`. Return `None`.
  - `FocusLost` → `held = NONE`, `count = 0`, find `Inactive`. Return `None`.
  - `KeyUp{..}` → `None`.
  - `KeyDown { key, mods, .. }` → `held = mods`, then in order:
    1. **Find pending** (`FindState::Pending`, Alt held, `key == Char(c)`): set
       `Active{ch:c}`, return `FindChar{ch:c}`.
    2. **Find active** (`FindState::Active{ch}`, Alt held): `Char('d')`→`FindRepeat{forward:true}`,
       `Char('a')`→`FindRepeat{forward:false}` (keep `ch`); any other key → set `Inactive` and
       fall through to normal handling of that key.
    3. **Begin find:** Alt held + `Char('f')` → `FindState::Pending`, return `None`.
    4. **Count:** Alt held + `Char('1'..='9')` → `count = count*10 + d`, return `None`.
    5. **Keymap hit:** `keymap.lookup(held, key)` → if it's a `Move`, set
       `count = max(1, self.count)` then reset `self.count = 0`; return it. (Other bindings: as-is.)
    6. **Base fallback** (`held.is_none()`): `Char(c)`→`InsertChar(c)`, `Enter`→`InsertNewline`,
       `Backspace`→`DeleteBackward`, `Escape`→`CollapseSelection`.
    7. Otherwise (modifier held, key unbound) → reset `count = 0`, return `None`.
- Expose `held()`, and for tests `pending_count()` + `find_state()` shape.
- **Tests (the core guarantees):**
  - `plain_char_inserts`; `alt_w_moves_up_not_types`; `releasing_alt_returns_to_typing`
    (Alt→NONE→`w` → `InsertChar('w')`); `focus_lost_clears_held` (`held()==NONE`).
  - `count_repeats_next_motion` (Alt,`5`,`s` → `Move{Char(Down),..,5}`, count back to 0);
    `count_accumulates` (Alt,`1`,`2`,`w` → 12); `releasing_alt_clears_count`.
  - `find_then_repeat` (Alt,`f`,`x` → `FindChar{x}`; then `d`→`FindRepeat{true}`,
    `a`→`FindRepeat{false}`); `find_ends_on_alt_release`; `find_ends_on_other_key`.
- **Commit:** `feat(core): resolver state machine — held mods, count, find sub-mode`.

### Task 8 — `executor` (single cursor first: motions + edits via transactions)
**Files:** `src/executor.rs`, `src/lib.rs`
- `apply(action, &mut Buffer, &mut History)` operating on the **primary** range for now. Edits
  build a `ChangeSet`, record a `Transaction { changes, selection_before, selection_after }`,
  apply it, and **commit it to history**. Motions/collapse change only the selection (no
  ChangeSet), but see Task 12 for expansion-step history entries.
- **Editing:** `InsertChar(c)` (insert at head, advance, collapse), `InsertNewline`,
  `DeleteBackward` (remove the char before head; no-op at start) — all via `ChangeSet`.
- **`CollapseSelection`:** set `anchor=head` on the primary (drop span); multicursor collapse in Task 9.
- **`Move { motion, extend, count }`:** apply `motion` to `head` **`count`** times; then
  `extend ? Range{anchor, head:dest} : Range::cursor(dest)`. Every motion **clamps** at buffer
  edges — never panics. Motion semantics (all by `char`, per `KEYMAP.md`):
  - `Char(Left/Right)`: ±1 char clamped. `Char(Up/Down)`: same column on adjacent line, column
    clamped to that line's length excluding the trailing `\n`; no-op past first/last line.
  - `WordStart(Right/Left)`: to start of next/previous **word**, where a word is a maximal run
    of alphanumeric-or-`_` (sensible default; tunable later). End-of-word = `E` then `A`.
  - `LineEdge(Left)`: first char of the line; `LineEdge(Right)`: last char before `\n` / EOF.
  - `BlankLine(Up/Down)`: nearest blank (empty/whitespace-only) line above/below, skipping the
    current block; no-op if none.
  - `MatchingBracket`: if head is on/inside `() [] {}`, jump to the partner (nesting-aware,
    across lines); else no-op.
- **Tests:** insert/newline/delete (incl. `delete_backward_at_start_noop`); each motion incl.
  edge clamps and empty buffer; `extend_keeps_anchor`; `count_moves_n`; `collapse_drops_span`;
  every edit round-trips through a `ChangeSet` (assert via undo later, but here assert text+head).
- **Commit:** `feat(core): executor — motions and transaction-based edits (single cursor)`.

### Task 9 — Multicursor execution
**Files:** `src/executor.rs`, `src/selection.rs`, `src/lib.rs`
- Generalize `apply` to **all** ranges:
  - **Motions/collapse:** map each range independently; `CollapseSelection` collapses every
    range's span **and** drops all but the primary (Esc → single cursor).
  - **Edits:** build **one** `ChangeSet` covering every range (sorted by position) in the **old**
    coordinate space; apply once; then `map_pos` every range's `anchor`/`head` into the new space;
    `Selection::normalize` to **merge overlaps**. One edit affects all cursors atomically.
  - **`SpawnCursor(Up/Down)`** (provisional, `KEYMAP.md` "not yet specified"): add a bare cursor
    on the line above/below the primary at the same column; mark clearly in code + `devlog` as
    provisional pending the exact spec.
- **Tests:** two cursors + `InsertChar` inserts at both, offsets stay correct; an edit that
  makes two ranges overlap merges them; spawn-above/below adds a cursor; `Esc` collapses to one.
- **Commit:** `feat(core): multicursor — one edit over all ranges, map + merge`.

### Task 10 — `expand` (selection expansion I/O/U/P)
**Files:** `src/expand.rs`, `src/executor.rs`, `src/lib.rs`
- Pure functions over the rope + a `Range`, returning the next expanded `Range` per `KEYMAP.md`:
  - `I` (`Enclosing`): word → token → `(…)` → `[…]` → `{…}`, climbing one level out (delimiter
    matching across lines).
  - `U` (`EnclosingLeft`): like `I` but biased toward the start (see the `a aa b` example).
  - `O` (`BracketContent`): content **inside** the nearest pair, climbing to the parent's content.
  - `P` (`BracketAlternating`): content → that pair incl. delimiters → parent content → … out.
- Executor wires `Expand(..)` to update the **primary** range (multi-range expansion can follow;
  primary is enough to match `KEYMAP.md`'s described behavior). Expansion at the outermost level
  is a no-op.
- **Tests:** drive each of I/U/O/P through the `KEYMAP.md` worked examples (e.g. `a aa b` for
  `I` vs `U`); nested brackets climb correctly; quotes/brackets match across lines; outermost = no-op.
- **Commit:** `feat(core): delimiter-based selection expansion (I/O/U/P)`.

### Task 11 — `find` (inline char search)
**Files:** `src/find.rs`, `src/executor.rs`, `src/lib.rs`
- Pure helper: given the rope, the current `head`, a target `char`, and a direction, return the
  next/previous occurrence **on the current line** (or `None`). `FindChar{ch}` moves to the next
  occurrence after head; `FindRepeat{forward}` re-runs in the given direction from head. No match
  = no-op (head unchanged).
- **Tests:** find moves to the next occurrence on the line; repeat-next / repeat-prev step
  through occurrences; stops at line boundaries; no match leaves head put.
- **Commit:** `feat(core): inline char search (Alt+F find sub-mode)`.

### Task 12 — `history` (revision-tree undo)
**Files:** `src/history.rs`, `src/executor.rs`, `src/lib.rs`
- Revision-tree (not a linear stack), anchor/transaction based:
  ```rust
  struct Revision { parent: Option<usize>, transaction: Transaction }
  pub struct History { revisions: Vec<Revision>, current: usize } // root at index 0
  impl History {
      pub fn commit(&mut self, tx: Transaction); // append child of `current`, advance current
      pub fn undo(&mut self, buf: &mut Buffer) -> bool; // apply inverse, restore selection_before, move to parent
      // redo-ready: a Revision tracks its children; redo binding is deferred.
  }
  ```
- **Expansion steps are history entries:** a `Transaction` with an empty `ChangeSet` but
  `selection_before != selection_after` is committed for each `I`/`O`/`U`/`P` step, so `Ctrl+Z`
  reverts the selection even when no text changed (`KEYMAP.md`). The executor commits these.
- **`Undo` action:** `history.undo(&mut buf)`; no-op at the history root (never panic).
- **Tests:** type then undo reverts text + selection; multiple edits undo in reverse; an
  expansion step is undone (selection reverts, text unchanged); undo at root is a safe no-op;
  invert-round-trip via history equals the original buffer.
- **Commit:** `feat(core): revision-tree undo (Ctrl+Z), expansion steps included`.

### Task 13 — `Editor` facade + comprehensive end-to-end
**Files:** `src/lib.rs`
- ```rust
  pub struct Editor { pub buffer: Buffer, resolver: Resolver, keymap: Keymap, history: History }
  impl Editor {
      pub fn new(text: &str) -> Self;
      /// Feed one raw input event. Returns true if the view should redraw.
      pub fn handle(&mut self, event: InputEvent) -> bool; // resolve → Some(action) ? execute+notify : false
  }
  ```
- Ensure `lib.rs` `pub mod`s every module (the Reviewer owns the final list, but list them here).
- **E2E tests through the facade:**
  - type → hold-Alt navigate → release → type lands in the right spot (the core quasimode property);
  - Alt+Shift extend round-trip; count round-trip; `Alt+F`+char then `D`/`A` find-repeat;
  - an expansion sequence (`I`/`O`/`U`/`P`);
  - **multicursor:** spawn a cursor, type once, both lines get the char;
  - **undo:** `Ctrl+Z` walks back through text edits *and* expansion steps.
- **Verify:** `cargo test -p alteria-core` all green; `cargo fmt --check` + `cargo clippy` clean.
- **Commit:** `feat(core): Editor facade + end-to-end engine tests`.

---

## Done criteria
- `cargo test -p alteria-core` passes (every module + the e2e suite).
- `cargo fmt --check` and `cargo clippy` clean (no warnings).
- No `gpui` anywhere; no `unwrap()`/`panic!` on input-reachable paths; all listed edges handled.
- The full type/API set exists and is internally consistent — this is the stable engine API the
  GPUI frontend (plan 002) will call via `Editor::handle`.

## Self-review (run before opening for review — mirrors `CLAUDE.md`)
1. Decoupled core: no `gpui` import; every stage a pure function over plain data. ✓
2. Tests: test-first; `cargo test -p alteria-core` green; new behavior driven by tests. ✓
3. Clean build: `cargo fmt`, `cargo clippy`, `cargo build` warning-free. ✓
4. Edges: buffer start/end, empty buffer, first/last line, modifier release, focus lost,
   overlap merge, no-match find, outermost expansion, undo root. ✓
5. Conventions: commit style, no AI trailer, one concept per module. ✓

## Executor notes
- Work in your worktree (e.g. `alteria_a1`). Start by syncing to main —
  `git fetch origin && git reset --hard origin/main && git clean -fd` — then execute task by
  task, commit per task, and **push your own branch** (never `main`).
- Write a matching **`devlog/001`** entry: decisions, anything that differed from this plan, the
  provisional multicursor-spawn spec to revisit, and any binding that felt wrong in testing.
- This plan is one long sequence (the transaction layer underpins everything, so the work is
  intentionally serial). It is sized to be implementable end-to-end with no further design
  decisions; if a behavior is ambiguous, `KEYMAP.md` is the tiebreaker.
