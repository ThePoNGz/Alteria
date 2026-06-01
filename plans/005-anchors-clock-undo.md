# Plan 005 — Phase 2: Anchors + clock/UndoMap undo + full Selection (+ line-endings)

**Goal:** Replace the interim Helix-style edit/undo layer with Zed's `text`/`clock` model —
**`Anchor`-based positions**, **`BufferSnapshot`**, and **clock + `UndoMap`** undo/redo. Adopt
Zed's full `Selection<T>` shape (`reversed`/`goal`/`id`). This is the second and final "wrong
stack" fix (the Helix changeset/revision-tree → Zed's `text`). Folds in **line-ending
normalization** and removes the collab surface `text` still carries.

**Status / deps:** Plans **003 and 004 are MERGED** on `main` (`2bc4b45`). The vendored
`text`/`clock`/`collections` crates exist and build gpui-free; `alteria-core` is on the
byte-indexed rope with grapheme/goal/CharKind motion. 005 is **unblocked**. It rewrites the same
files 003 owns (`selection.rs`, `executor.rs`) + the edit spine, so it runs **alone** (no parallel
sibling). This is the heaviest plan — work it task by task, keeping `cargo test --workspace` green.

> **Sources of truth:** `../CLAUDE.md`, `../KEYMAP.md`, and the **vendored** `text` now in-repo at
> `crates/text/src/` (cross-check against `../zed-main/crates/text` — they are verbatim-equal
> modulo the documented collab removals):
> - `text.rs` — `Buffer`/`BufferSnapshot`, `edit()`, `apply_local_edit`, `History`/`Transaction`
>   (group_interval), `undo`/`redo`/`undo_or_redo`/`apply_undo`, `edits_since`/`subscribe`,
>   `ToOffset`/`ToPoint`/`FromAnchor`, `anchor_before`/`anchor_after`/`anchor_at`.
> - `anchor.rs` — `Anchor { timestamp, offset, bias }` + resolution.
> - `selection.rs` — `Selection<T> { id, start, end, reversed, goal }`, `SelectionGoal`, `set_head`.
> - `undo_map.rs` — `UndoMap` (undo via count parity). `crates/clock/src/clock.rs` — `Lamport`/`Global`.
> Read the actual signatures in these in-repo files — do not write `text` APIs from memory.

## Files this plan owns
```
crates/text/**                          # T0: drop collab deferred-op surface; fix test harness
crates/util-shim/**                     # T0 (only if vendoring RandomCharIter for text's tests)
crates/alteria-core/Cargo.toml          # + text, clock path deps
crates/alteria-core/src/buffer.rs       # Buffer on text::Buffer / BufferSnapshot; line-ending normalize
crates/alteria-core/src/selection.rs    # Selection<Anchor>: {id,start,end,reversed,goal}
crates/alteria-core/src/transaction.rs  # RETIRE (delete) — edits go through text::Buffer
crates/alteria-core/src/history.rs      # RETIRE revision-tree; undo/redo via text History + UndoMap
crates/alteria-core/src/executor.rs     # edits + motion/expand/find resolve through anchors/snapshot
crates/alteria-core/src/expand.rs       # operate on snapshot offsets (resolve anchors)
crates/alteria-core/src/find.rs         # operate on snapshot offsets
crates/alteria-core/src/lib.rs          # module list (drop `pub mod transaction`), e2e
```

## Tasks

### T0 — `text` housekeeping: workspace tests green + drop collab surface
*(Do first — it makes the foundation clean before wiring it in. Closes the known Gate-1 caveat.)*
- **`cargo test --workspace` is currently broken**: `text`'s `#[cfg(any(test, feature = "test-support"))]`
  helpers (`edit_via_marked_text`, `random_byte_range`, `get_random_edits`, the `RandomCharIter`
  import) reference unvendored `rand` + `util::RandomCharIter` + `util::test`. Fix cleanly — pick one:
  - **(a)** vendor `RandomCharIter` into `util-shim` (it's ~15 lines in Zed `util`) and add
    `rand` as a `[dev-dependencies]` of `text`, so `text`'s randomized property tests compile/run; or
  - **(b)** if those harness helpers aren't wanted yet, change their gate from
    `cfg(any(test, feature = "test-support"))` to `cfg(feature = "test-support")` so `cargo test -p text`
    compiles 0 tests cleanly (opt-in later).
  Bar: **`cargo test --workspace` compiles and passes.**
- **Remove the collaboration surface** (single-user is a permanent non-goal — devlog/004 flagged
  this as the one collab piece still compiled): delete the `operation_queue` module and
  `Buffer::deferred_ops`/`deferred_replicas` + `apply_ops`/`apply_op`/`can_apply_op`/
  `flush_deferred_ops`/`has_deferred_ops`/`deferred_ops_len`. **Keep** `subscription`
  (`subscribe`/`edits_since` — the frontend's change-notification channel) and the local edit/undo
  path (`edit`/`apply_local_edit`/`undo`/`redo`/`UndoMap`). This is the only edit to retained Zed
  logic in the whole project — do it surgically and note it in `devlog/005`.
- **Verify:** `cargo build -p text` green, gpui/ropey-free; `cargo test --workspace` green.
- **Commit:** `chore(text): workspace tests green; drop collab deferred-op surface (single-user)`.

### T1 — `Buffer` on `text::Buffer` + line-ending normalization
- `alteria-core/Cargo.toml`: add `text = { path = "../text" }`, `clock = { path = "../clock" }`.
- `buffer.rs`: back `Buffer` with `text::Buffer` (owns rope + history + lamport clock); reads go
  through a `BufferSnapshot` (`buffer.snapshot()`). Keep the `Buffer::from_str(s)` constructor —
  build the `text::Buffer` with a fixed single-user identity (`ReplicaId::LOCAL` = 0 and a constant
  `BufferId`; confirm the exact `text::Buffer` constructor in `crates/text/src/text.rs`).
- **Line endings:** `text` already normalizes `\r\n`/`\r` → `\n` on construction
  (`LineEnding` + `normalize_line_separators`). Route `from_str` through it so the interior is
  uniformly `\n`, then **delete the interim `\r` special-cases** in `buffer.rs::line_content_len`
  (trailing-`\r` trim) and `executor.rs::is_blank`.
- **Tests:** construction; `snapshot` text/`Point` reads; `"a\r\nb"` is stored as `"a\nb"`.
- **Commit:** `feat(core): Buffer on text::Buffer + BufferSnapshot; normalize line endings on load`.

### T2 — Anchor-based `Selection` (Zed `Selection<T>` shape)
- `selection.rs`: move selections to **anchors**. Reuse `text::Selection<Anchor>`
  (`{ id, start, end, reversed, goal }`) — it's vendored at `crates/text/src/selection.rs` and is
  generic exactly so it can hold `Anchor`. `head()`/`tail()` derive from `reversed`; `set_head`
  flips `reversed` and updates `goal`. Make anchors with `snapshot.anchor_before/after(offset)`.
- **Goal column:** map Plan 003's `goal: Option<u32>` (byte column) onto `SelectionGoal::Column(u32)`.
  Pixel-x `SelectionGoal` variants stay frontend-deferred.
- Multicursor wrapper stays `Vec<Selection<Anchor>>` + a `primary` identified by stable `id`;
  `normalize` resolves anchors→offsets to sort/merge, preserving `reversed`.
- Everywhere motion/find/expand/render needs a concrete position, resolve the anchor against the
  current `snapshot` (`ToOffset`/`ToPoint`).
- **Tests:** an edit *before* a selection leaves the selected text intact (anchor tracks, no manual
  remap); `reversed` preserved through a merge; `goal` retained across vertical motion.
- **Commit:** `feat(core): anchor-based Selection (Zed Selection<Anchor>)`.

### T3 — Edits through `text::Buffer::edit` (retire the Helix changeset)
- `executor.rs`: resolve each selection to offsets against the snapshot and call
  `buffer.edit([(range, new_text), …])` — **one** `edit` over all cursor ranges. Selections
  re-resolve from their anchors automatically (delete the hand-rolled `map_pos` remap). Rewrite
  `InsertChar`/`InsertNewline`/`DeleteBackward` as `edit` calls; `DeleteBackward` deletes the prior
  grapheme range (reuse 003's grapheme-left).
- **Retire `transaction.rs`** entirely (the Helix `ChangeSet`/`Op`/`Assoc`/`map_pos`) and drop its
  `pub mod transaction` from `lib.rs`.
- **Tests:** single + multicursor insert/delete; multibyte (`é`); the existing edit + e2e tests
  stay green (behavior identical, just anchor-backed).
- **Commit:** `feat(core): edits via text::Buffer::edit; retire Helix changeset`.

### T4 — Undo/redo via `text` History + `clock`/`UndoMap` (retire revision-tree)
- `history.rs`: replace the `Vec<Revision>` + stored-inverse model with `text::Buffer`'s
  transactions. Wrap each user edit in `start_transaction`/`end_transaction` (grouping via
  `group_interval`). `Ctrl+Z` → `buffer.undo()`; **implement real redo** → `buffer.redo()`
  (`text` supports it via `UndoMap` count parity — confirm method names in `text.rs`). Then bind
  redo in `keymap.rs`/`action.rs` (KEYMAP currently defers the redo binding — add it now or leave
  the action wired and binding deferred per KEYMAP; note the choice).
- **Selection-only undo (KEYMAP):** `I`/`O`/`U`/`P` expansion steps must be undoable even with no
  text change. `text`'s undo only reverts text ops, so keep a **separate selection-history stack**
  the executor pushes on each expansion (and on edits), and make `Ctrl+Z` pop the **combined**
  timeline: if the last step was a text edit → `buffer.undo()` + restore the paired selection; if it
  was a selection-only expansion → pop the selection stack. One shared timeline, as KEYMAP requires.
- Retire the revision-tree `History` and the `ChangeSet` inverse logic.
- **Tests:** type→undo reverts text **and** selection; redo re-applies; an expansion step undoes
  (selection reverts, text unchanged); grouping coalesces a burst; undo at the root is a safe no-op.
- **Commit:** `feat(core): clock+UndoMap undo/redo via text::Buffer; retire revision-tree`.

### T5 — Green + e2e
- `expand.rs`/`find.rs`: confirm they read through the snapshot (resolve anchors to offsets/Points).
- **Verify:** `cargo test --workspace` green; `cargo fmt --check` + `cargo clippy --workspace --no-deps`
  clean; `cargo tree` shows no `gpui`, no `ropey`. All KEYMAP behaviors unchanged (the 171 motion/
  edit tests + new anchor/undo/redo tests).
- **Commit:** `test(core): e2e anchors + undo/redo through the facade`.

## Done criteria
- No Helix `ChangeSet` and no revision-tree remain; `transaction.rs` is gone.
- Positions are `Anchor`s; `Selection` is Zed's `{id,start,end,reversed,goal}` shape.
- Undo **and** redo work via `text` History + `clock` + `UndoMap`; expansion steps are on the undo
  timeline; transaction grouping works.
- `text`'s collab/deferred-op surface is removed; `cargo test --workspace` green; line endings
  normalized on load; no `gpui`/`ropey` in the tree.

## Design decisions to confirm with the maintainer (before/while executing)
1. **Single-user identity:** `text::Buffer` needs a `ReplicaId` + `BufferId` — use fixed
   `ReplicaId::LOCAL` (0) + a constant `BufferId`. (Recommended; no multi-replica anything.)
2. **Selection-only undo:** a separate selection-history stack merged into the `Ctrl+Z` timeline
   (recommended) vs attaching selection snapshots to `text` transactions.
3. **Wrap vs expose `text::Buffer`:** keep a thin `Buffer` wrapper so `alteria-core`'s public API
   (`Editor::handle`, etc.) stays stable and the frontend (future plan) isn't coupled to `text`'s
   surface directly. (Recommended.)
4. **Redo binding:** KEYMAP lists redo as deferred — wire `buffer.redo()` now and bind `Ctrl+Y`
   (or leave binding deferred, action ready). Maintainer's call.

## Notes
- Collaboration stays a **non-goal**: adopt `clock` for *local* operation identity + undo only;
  T0 removes the network/deferred-op surface — do not re-add it.
- Write `devlog/005`, including exactly what T0 removed from `text` (the one place retained Zed
  logic is edited) so the fidelity record stays honest.
