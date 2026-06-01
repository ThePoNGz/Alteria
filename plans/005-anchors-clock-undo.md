# Plan 005 — Phase 2 integration: Anchors + clock/UndoMap undo + full Selection (+ line-endings)

**Goal:** Replace the interim Helix-style edit/undo layer with Zed's `text`/`clock` model — **`Anchor`-based positions**, **`BufferSnapshot`**, and a **Lamport-clock operation log + `UndoMap`** undo (undo is a forward op; redo = undo-of-undo). Adopt Zed's full `Selection<T>` shape (`reversed`/`goal`/`id`). This is the second and final "wrong stack" fix (the Helix changeset/revision-tree → Zed's `text`). Also fold in **A5 line-ending normalization**.

**Dependencies (NOT parallel):** requires **Plan 003** merged (the `selection.rs` goal field / Selection shape) **and Plan 004** merged (vendored `text`/`clock`/`collections`). Touches the same files Plan 003 does (`selection.rs`, `executor.rs`) → must run **after** it. This is the heavy plan; expect to split it into its own task breakdown once 003/004 land and the exact `text` surface is known.

> **Sources of truth:** `../CLAUDE.md`, `../KEYMAP.md`, and Zed `../zed-main/crates/text`:
> - `src/text.rs` — `Buffer`/`BufferSnapshot`, `edit()` (`:870`), `apply_local_edit` (`:892`), `History`/`Transaction` (`:153,:135`), `undo`/`undo_or_redo` (`:1547,:1618`), `apply_undo` (`:1416`), `edits_since`/`subscribe` (`:2640,:1743`).
> - `src/anchor.rs` — `Anchor { timestamp, offset, bias }`, resolution model.
> - `src/selection.rs` — `Selection<T> { id, start, end, reversed, goal }`, `SelectionGoal`, `set_head` (`:71`).
> - `src/undo_map.rs` — `UndoMap` (undo via count parity). `crates/clock/src/clock.rs` — `Lamport`/`Global`.

## Files this plan owns
```
crates/alteria-core/Cargo.toml          # + text, clock (path deps); ropey already gone
crates/alteria-core/src/buffer.rs       # Buffer wraps text::Buffer / holds BufferSnapshot; A5 normalize
crates/alteria-core/src/selection.rs    # Selection<Anchor>: {start,end,reversed,goal,id}
crates/alteria-core/src/transaction.rs  # RETIRE the Helix ChangeSet (edits go through text::Buffer)
crates/alteria-core/src/history.rs      # RETIRE revision-tree; undo/redo via clock + UndoMap
crates/alteria-core/src/executor.rs     # apply_edit/expand_primary on anchors + text edits
crates/alteria-core/src/lib.rs          # module list, e2e
```

## Tasks (outline — refine after 003/004)

### T1 — Wire deps + Buffer on `text`
- `alteria-core/Cargo.toml`: add `text = {path="../text"}`, `clock = {path="../clock"}` (rope/sum_tree already present; ropey already removed).
- `buffer.rs`: back `Buffer` with `text::Buffer` (owns the rope, history, lamport clock) and expose a `BufferSnapshot` for reads. Keep `Buffer::from_str` as the constructor. **A5:** detect line ending + normalize `\r\n`/`\r`→`\n` on construction, store the `LineEnding` for a future save path (Zed's model), letting `buffer.rs`/`executor.rs` drop the scattered `\r` special-cases.
- **Tests:** construction, snapshot reads, CRLF normalization.

### T2 — Anchors + Selection shape
- `selection.rs`: adopt Zed's `Selection<Anchor> { id, start, end, reversed, goal }` (build on the `goal` field added in Plan 003); `head()`/`tail()` derive from `reversed`; `set_head` flips `reversed` + updates `goal` (Zed `selection.rs:71`). Keep the multicursor wrapper (`Vec<Selection>` + `primary`/stable `id`); `normalize` merges by resolved offset while preserving `reversed`.
- Resolve anchors→offsets/points against the snapshot for motion/render.
- **Tests:** anchors survive edits elsewhere; reversed orientation preserved through merge; goal retained.

### T3 — Edits through `text::Buffer`
- `executor.rs`: replace `ChangeSet` construction with `text::Buffer::edit(ranges→new_text)`; multicursor = one `edit` over all ranges, then re-resolve anchors (no hand-rolled `map_pos` — anchors move for free).
- **Retire `transaction.rs`** (the Helix ChangeSet/`map_pos`/`Assoc`).
- **Tests:** single + multicursor edits; offsets/anchors correct post-edit; the existing edit tests stay green.

### T4 — Undo/redo via clock + UndoMap
- `history.rs`: replace the revision-tree + stored-inverse model with Zed's: edits are timestamped ops; undo emits an undo op (`undo_or_redo`), `UndoMap` flips fragment visibility; **real redo** = undo-of-undo. Transaction grouping via `group_interval` (Zed `History::group`).
- **Selection history** kept separate from the edit op log (Zed does this): the `I/O/U/P` expansion "selection-only undo step" (KEYMAP) becomes a selection-history entry, not a fake empty-changeset transaction.
- **Tests:** undo/redo round-trips text + selection; expansion steps undo; grouping; undo at root is a safe no-op.

### T5 — Green + e2e
- Full `cargo test -p alteria-core` green; fmt + clippy clean; `cargo tree` shows no gpui, no ropey; KEYMAP behaviors unchanged.
- **Commit style:** per-task `feat(core): …` / `refactor(core): …`.

## Done criteria
- No Helix `ChangeSet` / revision-tree remain; positions are `Anchor`s; undo/redo is clock + `UndoMap`; `Selection` matches Zed's shape.
- `alteria-core` depends on the vendored `text`/`clock`; behavior (KEYMAP + existing tests) preserved; redo implemented.
- Line endings normalized on load.

## Notes
- This is the largest plan; once 003/004 are merged, re-break T2–T4 into smaller tasks against the **actual** vendored `text` surface (it differs depending on how much collab code Plan 004 gated vs deleted — read `devlog/004`).
- Collaboration remains a **non-goal**: adopt the clock for *local* undo identity only; do not re-enable the gated network/CRDT-sync surface.
