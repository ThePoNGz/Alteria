# Devlog 005 — Phase 2: Anchors + clock/`UndoMap` undo + Zed `Selection` shape (+ A5 line endings)

**Date:** 2026-06-02
**Plan:** `plans/005-anchors-clock-undo.md`
**Branch:** `alteria_a1`
**Status:** ✅ Complete. The interim Helix changeset / revision-tree edit+undo layer is gone; `alteria-core` now edits, undoes, and stores selections on Zed's `text` model — byte-true `Anchor`s, a Lamport-clock op log + `UndoMap` (with **real redo**), and Zed's `Selection<T>` shape. Line endings normalize on load. `cargo test -p alteria-core` is **151/151**, `cargo fmt`/`clippy` clean, and `cargo tree` shows `alteria-core → {rope, sum_tree, text}` with **no gpui, no ropey**.

---

## 1. What this chunk is

The second and final "wrong stack" fix. Plan 002 swapped the text *store* to Zed's `rope`; plan 004 vendored Zed's `text`/`clock`/`collections`. This plan rebuilds the *editing* layer on top of them: every position is an `Anchor`, every edit goes through `text::Buffer::edit`, undo/redo is Zed's clock + `UndoMap`, and the selection model adopts Zed's `Selection<T> { id, start, end, reversed, goal }`. The hand-rolled `ChangeSet`/`map_pos`/`Assoc` and the revision-tree `History` are retired.

## 2. The keystone: `BufferSnapshot::as_rope()`

The motion layer (`executor.rs` step/word/vertical/bracket fns, `expand.rs`, `find.rs`, `char_kind.rs`, the `buffer.rs` line helpers) is large, well-tested, and entirely **byte-offset + `&Rope`** code that already mirrors Zed's `movement.rs`. `text::BufferSnapshot` exposes `as_rope() -> &Rope` over the *same* vendored `rope::Rope`, so **none of that code changed**. The engine only crosses the anchor↔offset boundary in two spots (`Buffer::resolved` / `Buffer::set_selections`): an action resolves the anchored selection to offsets, runs the unchanged motion math, then re-anchors. This kept the diff to the edit/undo/selection *plumbing* and left the motion *fidelity* untouched.

## 3. Selection model (`selection.rs`)

- **`Selection<T> { id, start, end, reversed, goal }`** — Zed's shape verbatim (`text/src/selection.rs`): `start <= end` always; `reversed` marks the head end. Stored as `T = Anchor`; resolved to `T = usize` for math. Anchor is `Copy`, so the type stays `Copy`.
- **`SelectionGoal { None, Column(u32) }`** — mirrors the *shape* of Zed's enum but keeps plan 003's **byte-column** goal. The vendored `text::SelectionGoal` only has the pixel-x variants (`HorizontalPosition`, …); plan 003 deliberately deferred pixel-x to the frontend ("rope columns are bytes"), so we define our own enum rather than `use text::SelectionGoal`. `column()`/`from_column()` bridge to the offset-space vertical helpers, which kept their `Option<u32>` signatures.
- **`Selections`** — the multicursor wrapper (`Vec<Selection<Anchor>>` + a stable `primary_id` + an `id` allocator). `normalize()` runs in **offset space** (anchors only order against a snapshot), merging overlaps and re-finding the primary by id; `buffer.rs` resolves → normalizes → re-anchors.

We did **not** `use text::Selection` directly: keeping our own type lets us carry the byte-column goal and the multicursor/primary wrapper while matching Zed's field shape exactly (plan: "adopt Zed's full `Selection<T>` shape (`reversed`/`goal`/`id`)").

## 4. Buffer + edits (`buffer.rs`)

`Buffer` now wraps `text::Buffer` (rope + Lamport clock + op log + `History`) plus `Selections`. Reads go through `snapshot()` / `rope()` / `text()`.

- **A5 line endings:** `text::Buffer::new` normalizes `\r\n`/`\r` → `\n` on construction and records the detected `LineEnding` internally (Zed's model) for a future save path. `buffer.rs`/`executor.rs` therefore drop *nothing* extra — the `\r` special-cases in `line_content_len` stay correct, and a buffer built from `"a\r\nb\rc"` holds `"a\nb\nc"` (tested).
- **Edits:** `Buffer::edit(Vec<(Range<usize>, String)>)` wraps the edit in an explicit `start_transaction`/`end_transaction` so it can return the **`TransactionId`** (Zed's `edit()` returns the `Operation`, whose timestamp is *not* the transaction id). Multicursor is one `edit` over all sorted, non-overlapping ranges. Carets are re-placed with a small right-associative `map_offset` over the same `(from, to, ins_len)` list (the only piece of the old `map_pos` worth keeping — anchors handle the rest).
- **`set_group_interval(0)`:** undo grouping is disabled so each edit is exactly one transaction. This (a) preserves the engine's existing per-keystroke undo granularity and its tests, and (b) keeps the history timeline in 1:1 lockstep with `text::Buffer`'s undo/redo stacks (see §5). Typing-coalescing is a deliberate future concern.

## 5. Undo/redo: one shared timeline (`history.rs`)

`text::Buffer` owns text undo/redo (clock + `UndoMap`; **real redo = undo-of-undo**). But KEYMAP requires `Ctrl+Z` to also step back through `I`/`O`/`U`/`P` **selection-only** expansions ("one shared timeline"), and an empty transaction can't live on Zed's stack. So `History` keeps its own timeline of entries that are either `Edit(TransactionId)` (delegating the text inverse to `text::Buffer::undo`/`redo`) or `SelectionOnly` (no text op). Every entry stores the `Selections` to restore on undo (`before`) and redo (`after`) — anchors, so once the text op is reversed they resolve correctly.

**Invariant:** the engine drives `text::Buffer` undo/redo *only* through `Edit` entries, in timeline order; `SelectionOnly` steps never touch Zed's stacks. With grouping off, the `Edit` entries stay 1:1 with Zed's undo/redo stacks, so `buf.undo()`/`buf.redo()` always reverse the matching transaction — guarded by a `debug_assert_eq!` on the returned `TransactionId` (which also puts the stored id to use).

**Redo is implemented and tested** as an engine capability (`History::redo`, round-trips text + selection) but is **not bound to a key**: KEYMAP.md lists redo under "Not yet bound", and KEYMAP is the binding source of truth. A future plan binds it by adding one `Action::Redo → history.redo`.

## 6. Retirements / decoupling

- `transaction.rs` (Helix `ChangeSet`/`Op`/`Assoc`/`map_pos`) **deleted**; `pub mod transaction` removed from `lib.rs`.
- The revision-tree `History`/`Transaction` is **replaced** by the timeline above.
- `expand::expand` now takes/returns plain `(lo, hi)` offsets instead of the old `Range` (it was already offset-only internally), decoupling it from the selection type. `find.rs` and `char_kind.rs` are `&Rope` + offset and **unchanged**.

## 7. Deviations from the plan

- **Only `text` was added as a path dep, not `clock`.** `text` re-exports everything the core touches (`Anchor`, `ToOffset`, `ReplicaId`, `BufferId`, `BufferSnapshot`, `Bias`, and `TransactionId = clock::Lamport`); a direct `clock` dep would be unused. (Plan T1 listed both as an outline before the surface was known.)
- **Own `Selection<T>` + `SelectionGoal`, not `text::Selection`.** Needed to keep plan 003's byte-column goal (the vendored `text::SelectionGoal` has no `Column`) and the multicursor/primary wrapper, while matching Zed's field shape.
- **Undo grouping disabled** (`group_interval = 0`) to preserve per-keystroke undo and the 1:1 timeline lockstep; coalescing deferred.
- **Redo unbound** (engine capability only), per KEYMAP "not yet bound".
- **The collab-oriented deferred-op machinery in `text`** (flagged in devlog 004 §4) was left compiled-but-unused: single-user local editing never invokes it, and excising it would be risky surgery on the apply spine for no dep win — out of scope here, still available for a later cleanup plan.

## 8. Verification

- `cargo test -p alteria-core` — **151 passed**, 0 failed. New tests cover: CRLF normalization, anchors surviving an edit elsewhere, edit→undo→redo round-trip, the shared edit/expansion timeline, and redo (history + executor).
- `cargo fmt -p alteria-core --check` — clean. `cargo clippy -p alteria-core --all-targets` — **0 warnings**.
- `cargo tree -p alteria-core -e no-dev | grep -iE 'gpui|ropey'` — **empty**; deps are exactly `rope`, `sum_tree`, `text`.
- `cargo build --workspace` — green.
- KEYMAP behaviors unchanged: the full motion/expand/find/multicursor/undo e2e suite (ported to the new types) passes byte-for-byte.

## 9. Zed-parity audit (post-implementation review)

Three focused reviews against the local `zed-main` checkout (`editor` + `text` crates) confirmed the model **aligns with Zed** on every load-bearing decision, and surfaced one real fix:

**Confirmed ALIGNED (with Zed file refs):**
- **Selection restore** stores `(before, after)` anchored selections per undo step — same as Zed's `SelectionHistory.selections_by_transaction` (`editor.rs`). We co-locate them in the timeline `Entry` instead of a tx-keyed `HashMap`; equivalent and simpler (Alteria has no async/multi-buffer transaction creation).
- **Transaction-id capture:** `start_transaction` + `inner.edit` + `end_transaction → (id, _)` is exactly Zed's `language::Buffer` pattern; Zed also doesn't surface the tx id from `edit` (the id you want is the *grouped* one from the transaction boundary).
- **`Selection<T>` shape** (`id/start/end/reversed/goal`) and **`set_head`** reordering match `text::Selection` field-for-field.
- **Multi-edit caret placement:** our `map_offset` (recompute every caret over the full sorted edit list) reproduces Zed's collective anchor-ride; the **span** anchor bias (`start=anchor_after`, `end=anchor_before`) matches Zed's `selection_to_anchor_selection`.

**Fixed (this devlog's second commit):**
- **Bare-cursor anchor bias.** Zed stores a collapsed cursor with `Bias::Right` on both ends (`anchor_after`); we had `Bias::Left`. Changed `buffer.rs::anchorize` to `anchor_after` to match `selection_to_anchor_selection`, so a foreign edit *at* the caret rides it rightward (matters for the anchor model and any future LSP/collab/format edit not routed through explicit re-placement). Added a direct test. Also made `Buffer::edit`/`undo`/`redo` `pub(crate)` so the undo timeline can only be driven through `History` — turning the 1:1-lockstep invariant from convention into a visibility guarantee.

**Acceptable / deferred divergences (documented for the Planner):**
- **Undo grouping disabled (`group_interval = 0`).** Zed's production default is 300ms time-coalescing (`ZERO` is test-only); ours is the interim choice that keeps each edit one transaction and the timeline 1:1 with Zed's stacks. When typing-coalescing lands, restore 300ms and record one `Entry` per *grouped* transaction id (out of scope here; a deliberate future concern).
- **Cursor-at-a-span-endpoint does not merge.** Zed's `should_merge` absorbs a bare cursor sitting exactly on a span's boundary; Alteria keeps it a distinct caret. This is **pre-existing** behavior from plans 001/003 (with explicit tests), not introduced here and outside 005's scope — flagged for a future parity decision rather than changed mid-plan.
- **Abutting-edit caret bias.** Zed switches the left caret of an end-to-start-touching edit pair to `Bias::Right`→`Left` (`next_is_adjacent`); unreachable today (edits are per-selection and coincident cursors merge first). Port when surround / auto-pair / snippet ops can emit touching non-merged edits.
- **Selection-only undo steps on one shared timeline** (KEYMAP "one shared timeline" for `I/O/U/P`): Zed has no equivalent — its selection-undo is a *separate* `Ctrl+U` command. Our unified `Step::Edit | Step::SelectionOnly` stack is the necessary Alteria-specific adaptation.

## 10. Commits (on `alteria_a1`)

```
feat(core): rebuild edit/undo/selection on Zed's text model (anchors + clock/UndoMap)
fix(core): bare-cursor anchor bias + pub(crate) undo entry points (Zed parity)
```
(plus this devlog.)
