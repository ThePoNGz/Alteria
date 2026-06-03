# Plan 008 — Core edit verbs: redo + the clipboard core (copy/cut/paste plumbing)

**Goal:** Add the **headless engine half** of the remaining everyday edit verbs, so the frontend
(plans 009+/wave 2) can wire them to keys and the OS clipboard. Three pieces, all pure
`alteria-core` logic, all TDD'd without a window:
1. **Redo** — `Ctrl+Y` and `Ctrl+Shift+Z` (the capability already exists; this binds + dispatches it).
2. **`InsertText` injection** — a pipeline-pure way for external text (paste, later IME) to enter the
   buffer at every cursor.
3. **Copy/cut gather** — a read-only `selected_texts()` so the frontend can copy per-cursor text; cut
   reuses the existing replace-with-empty path.

**This plan ships NO frontend and NO OS-clipboard I/O.** Per the gpui-free hard rule and Zed's own
split (clipboard lives in `editor`, which has `cx`; the buffer/text layer does not), the OS clipboard
read/write and the `Ctrl+C/X/V` key interception are a **wave-2 frontend plan**. 008 only provides the
core capabilities those will call. Redo, by contrast, is fully usable the moment it merges (the
frontend already forwards every `KeyDown`).

**Status / deps:** Plans 001–007 on `main` (007 may still be executing in parallel — that's fine,
see Parallelism). The engine already has: `Buffer::redo()` + `History::redo` (Zed's clock/`UndoMap`,
`buffer.rs:193`/`history.rs`), and an executor path that **replaces a selection with text on every
edit** (typing over a selection deletes it — see the `undo_walks_back…` test in `lib.rs`). This plan
builds only on that; **zero new dependencies, no change to the vendored `text`/`rope`/`clock` crates.**

> **Sources of truth — Zed (`../zed-main`), verified while planning:** reproduce these, don't
> re-derive (the "Zed is the source of truth" hard rule). Cite the file you mirrored in `devlog/008`.
> - **Redo = undo-of-undo via `UndoMap`:** `crates/text/src/text.rs:1593-1602` (`redo`) + `1618-1627`
>   (`undo_or_redo`); editor-level mirror of undo at `crates/editor/src/editor.rs:7307-7332`. Our
>   `Buffer::redo()` already wraps this; `History::redo` already restores the transaction's *after*-
>   selections — same structure as our existing undo.
> - **Redo keybindings:** `assets/keymaps/default-linux.json:86-90` binds `editor::Redo` to **both
>   `ctrl-y` and `ctrl-shift-z`** (Alteria is a Linux GUI → mirror the Linux defaults).
> - **One insert primitive underlies everything:** `crates/editor/src/input.rs:921-926` (`insert`) →
>   `:1923` (`replace_selections`) edits each selection as `(start..end, text)` in one transaction;
>   **empty string ⇒ pure delete.** Type-over, backspace, cut, and paste all funnel through it
>   (`editor.rs:4725` backspace, `4777` delete). Our `executor`'s `insert_text` is the same idea.
> - **Copy/cut gather:** `crates/editor/src/clipboard.rs` — `do_copy`(~`:433`)/`cut_common`(~`:277-341`)
>   iterate selections, push each `text_for_range(min..max)` into a `\n`-joined string, plus a
>   per-selection `ClipboardSelection { len, … }` sidecar (`clipboard.rs:4-10`). `cut` = copy + an
>   `insert("")` over the spans. **The clipboard write itself is in the editor/gpui layer, not the
>   buffer** — which is exactly why our OS-clipboard I/O is frontend (wave 2), and 008 only gathers.
> - **Paste / multi-cursor rule (for the wave-2 plan, recorded here so the core API fits it):**
>   `clipboard.rs:59-244` — if the clipboard's per-cursor metadata count == live cursor count,
>   distribute one slice per cursor; else insert the whole text at every cursor. Our
>   `Action::InsertTexts(Vec<String>)` supports the distribute case; `InsertText(String)` the
>   whole-at-each case.
> - **Text injection shape (the design call):** Zed's GUI text entry is the platform `InputHandler`
>   `replace_text_in_range`, which calls `editor.insert(text)`. The faithful, pipeline-pure analog is
>   an `InputEvent::InsertText(String)` → `Action::InsertText` → `executor` (reusing `insert_text`),
>   **not** a facade method that bypasses the resolver/history — that would be our only text mutation
>   skipping the pipeline (violating `buffer.rs:173`'s invariant). Confirmed by the verification pass.

## Files this plan owns
```
crates/alteria-core/src/input.rs      # + InputEvent::InsertText(String)
crates/alteria-core/src/action.rs     # + Action::Redo, Action::InsertText(String), Action::InsertTexts(Vec<String>)
crates/alteria-core/src/keymap.rs     # CTRL += 'y'->Redo; new CTRL_SHIFT layer 'z'->Redo
crates/alteria-core/src/resolver.rs   # InsertText event passthrough; Ctrl+Shift layer selection
crates/alteria-core/src/executor.rs   # apply Redo (history.redo), InsertText / InsertTexts (replace-all-selections)
crates/alteria-core/src/buffer.rs     # + selected_texts() -> Vec<String> (per-cursor rope slices)
KEYMAP.md                             # Ctrl section: add redo (Ctrl+Y / Ctrl+Shift+Z)
devlog/008-core-edit-verbs.md
```
**No `lib.rs` change** (`Editor::handle` already routes every event/action; new enum variants are
additive). **No frontend, no `Cargo.toml`/`Cargo.lock`, no vendored-crate edits.**

## Parallelism (why this is safe to run concurrently)
008 is the **core-logic lane** and touches a set **disjoint** from 007 (render lane:
`scroll.rs`/`view.rs`/`text_element.rs`) and 009 (syntax lane: the new `crates/syntax` + workspace
manifest). No shared file, so **007 ∥ 008 ∥ 009 merge to `main` with zero conflicts.** Adding variants
to `InputEvent`/`Action` is additive and the frontend constructs (never exhaustively matches) those
enums, so the merged tree still compiles. (Internally, the `executor`'s `match` on `Action` and the
`resolver`'s `match` on `InputEvent` are both in *this* plan, so they stay exhaustive.)

## Tasks (TDD — write the failing test first, per CLAUDE.md)

### T0 — Redo (`action.rs`, `keymap.rs`, `resolver.rs`, `executor.rs`, `KEYMAP.md`)
- `Action::Redo`. Executor: on `Redo`, call `history.redo(&mut buffer)` (mirror the existing `Undo`
  arm — same shape, redo side). Verify `History::redo` restores the transaction's *after*-selections
  (it already does — confirm, don't reimplement).
- Keymap: `CTRL` layer `bind('y', Redo)`; add a **`CTRL_SHIFT`** layer (`ctrl+shift`, no alt) with
  `bind('z', Redo)`. Confirm the resolver selects the `ctrl+shift` layer correctly (it normalizes
  ASCII case for modified layers; `'z'` keycap).
- `KEYMAP.md` "Ctrl held": add `Y → redo` and `Ctrl+Shift+Z → redo` (mirror Zed-Linux). Leave the
  clipboard verbs under "Not yet bound" (they land with the wave-2 frontend plan).
- **Tests:** edit → undo → **redo** round-trips text *and* selection through the facade
  (`Editor::key('z', CTRL)` undo, then `key('y', CTRL)` redo; and `Ctrl+Shift+Z` redo). Redo at the
  top of the stack is a no-op (`history.redo` returns `None` → `handle` returns… see note¹).
- **Commit:** `feat(core): redo via Ctrl+Y / Ctrl+Shift+Z (undo-of-undo, Zed UndoMap)`.

¹ Match the existing `Undo` arm's redraw/return convention exactly (whatever `apply` returns for a
no-op undo today, do the same for redo — keep the facade contract uniform).

### T1 — `InsertText` injection (`input.rs`, `action.rs`, `resolver.rs`, `executor.rs`)
- `InputEvent::InsertText(String)` and `InputEvent::?` — actually a single event carrying the text;
  resolver maps it straight to `Action::InsertText(String)` (no modifier/layer logic — it's not a
  keystroke). Also add `Action::InsertTexts(Vec<String>)` for per-cursor paste distribution.
- Executor: `InsertText(s)` replaces **every** selection with `s` (reuse the existing `insert_text`
  helper that `InsertChar`/`InsertNewline` already call — one transaction, history-recorded, anchors
  re-placed after each cursor's inserted text, exactly like typing). `InsertTexts(v)` replaces the
  i-th selection with `v[i]` (requires `v.len() == cursor count`; otherwise fall back to inserting the
  whole joined text at each — mirror Zed's `do_paste` count-mismatch branch). Empty string ⇒ delete
  (the cut path).
- **Tests (headless):** `InsertText("XY")` at a bare cursor inserts and advances; over a selection it
  **replaces** it; with two cursors it inserts at **both** (mirror the multicursor type-over test).
  `InsertText("")` over a selection deletes it (this is what cut will use). `InsertTexts(["a","b"])`
  with two cursors puts `a` at cursor 0, `b` at cursor 1; count-mismatch inserts the whole at each.
- **Commit:** `feat(core): InputEvent::InsertText / InsertTexts -> executor (paste/IME injection)`.

### T2 — Copy/cut gather (`buffer.rs`)
- `Buffer::selected_texts(&self) -> Vec<String>` — for each **resolved** selection (in primary-then-
  order, matching `resolved()`), slice the rope `min..max` to a `String` (empty for a bare cursor).
  This is the read the frontend's copy/cut will join with `\n` and hand to the OS clipboard. Mirror
  Zed's per-selection `text_for_range` gather (`clipboard.rs` `do_copy`); the per-cursor *length*
  metadata Zed stores is just `each.len()` on the frontend side — no core struct needed yet.
- **No delete action for cut:** cut = (frontend) `selected_texts()` → OS clipboard → feed
  `InsertText("")`. Confirm in a test that `InsertText("")` over the current selection is exactly the
  delete cut needs (this is Zed's `cut_common` → `insert("")`).
- **Tests:** single-cursor selection → one slice; multi-cursor → one slice per cursor in order; bare
  cursor → empty string; full-buffer selection → whole text.
- **Commit:** `feat(core): Buffer::selected_texts() for copy/cut gather (per-cursor slices)`.

### T3 — `devlog/008`
- Record: the Zed files mirrored (redo `text.rs`/`editor.rs`, the single `insert`/`replace_selections`
  primitive, `clipboard.rs` gather, the Linux redo keys); the **text-injection decision** (InputEvent
  vs facade method — why InputEvent keeps the pipeline pure) and its Zed analog (`replace_text_in_range`
  → `insert`); the explicit boundary that **OS clipboard I/O + Ctrl+C/X/V interception are wave-2
  frontend work**, with the core API (`selected_texts`, `InsertText`/`InsertTexts`) shaped to fit
  Zed's paste rule. Note that **no dependency was added and the vendored crates were untouched.**
- **Verify:** `cargo test -p alteria-core` green (the new verb tests); `cargo test`/`clippy`/`build`
  on default-members clean & fast; `cargo tree -p alteria-core | grep -iE 'gpui|ropey'` empty.
- **Commit:** `docs(devlog): record 008 — core edit verbs (redo + clipboard plumbing)`.

## Done criteria
- **Redo works end-to-end** through the facade on `Ctrl+Y` and `Ctrl+Shift+Z` (text + selection),
  no-op at the top of the redo stack; `KEYMAP.md` updated.
- `InputEvent::InsertText`/`InsertTexts` insert/replace at every cursor through the
  `resolver→Action→executor→history` pipeline (no bypass); empty string deletes the selection.
- `Buffer::selected_texts()` returns correct per-cursor slices.
- **Pure logic only:** zero new deps, vendored `text`/`rope`/`clock` untouched, no `lib.rs` change, no
  frontend, no `Cargo.*` change; `cargo tree -p alteria-core` still gpui-free. Engine commands stay
  fast & green.
- `devlog/008` records the mirrored Zed approach and the core/frontend clipboard boundary.

## Non-goals (wave 2 / later)
OS clipboard read/write · `Ctrl+C/X/V` key interception & their `KEYMAP.md` entries · paste's
clipboard-metadata round-trip & per-cursor distribution *wiring* (the core API is here; the
metadata/decision lives frontend) · forward `Delete` key · undo/redo grouping/coalescing · IME
(though `InsertText` is the seam it will reuse). None of these are in 008.

## Notes
- TDD throughout (engine is fully headless). `cargo fmt`/`clippy` clean before each commit.
- Don't touch the vendored crates or `lib.rs`. New `InputEvent`/`Action` variants are additive.
- This is the core-logic lane: it shares **no file** with 007 (render) or 009 (syntax) — safe to run
  in a third parallel terminal and merge with no conflict.
