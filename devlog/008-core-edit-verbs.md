# Devlog 008 — Core edit verbs: redo + the clipboard core (copy/cut/paste plumbing)

**Date:** 2026-06-03
**Plan:** `plans/008-core-edit-verbs.md`
**Branch:** `alteria_a2`
**Status:** ✅ Merged. The headless engine half of the remaining everyday edit verbs is in: **redo**
bound to `Ctrl+Y` and `Ctrl+Shift+Z`, an **`InsertText`/`InsertTexts`** injection seam for external
text (paste / IME) through the resolver→executor pipeline, and a read-only **`Buffer::selected_texts()`**
for copy/cut gather. All pure `alteria-core`, TDD'd headless; zero new deps, the vendored
`text`/`rope`/`clock` crates untouched, and no `lib.rs` non-test change. `cargo test -p alteria-core`
is **173/173**; `clippy`/`fmt` clean on the crate; `cargo tree -p alteria-core` still shows no gpui,
no ropey.

> **Authored at review/merge time.** The `alteria_a2` executor shipped the code + tests across two
> commits (`feat(core): redo verb + InsertText/InsertTexts injection`,
> `feat(core): Buffer::selected_texts() …`) but did not write this devlog or touch the binding spec.
> The reviewer wrote this record during the 007∥008∥009 merge so the numbered devlog matches the plan;
> the content below is reconstructed from the merged diff and the verification actually run on `main`.

---

## 1. Zed is the source of truth — what was mirrored

Per the "Zed is the source of truth" hard rule, every piece reproduces Zed's approach rather than
being re-derived. Files read in `../zed-main` (line numbers as cited in plan 008):

- **Redo = undo-of-undo via `UndoMap`** — `crates/text/src/text.rs` `redo`/`undo_or_redo`; editor-level
  mirror at `crates/editor/src/editor.rs`. Our `Buffer::redo()` + `History::redo` (landed in plan 005)
  already wrap this and restore the transaction's *after*-selections; 008 only adds the `Action::Redo`
  arm and the keys.
- **Redo keybindings** — `assets/keymaps/default-linux.json` binds `editor::Redo` to **both `ctrl-y`
  and `ctrl-shift-z`**. Alteria is a Linux GUI, so we mirror the Linux defaults exactly.
- **One insert primitive underlies everything** — `crates/editor/src/input.rs` `insert` →
  `replace_selections` edits each selection as `(start..end, text)` in one transaction; an **empty
  string ⇒ pure delete**. Type-over, backspace, cut, and paste all funnel through it. Our executor's
  `insert_text` is the same idea, and `InsertText`/`InsertTexts` reuse it.
- **Copy/cut gather** — `crates/editor/src/clipboard.rs` `do_copy`/`cut_common`: iterate selections,
  push each `text_for_range(min..max)` into a `\n`-joined string. `Buffer::selected_texts()` is the
  per-cursor slice gather; the `\n`-join + the OS-clipboard write stay in the (wave-2) frontend.
- **Paste / multi-cursor rule** — `clipboard.rs` `do_paste`: if the clipboard's per-cursor metadata
  count == live cursor count, distribute one slice per cursor; else paste the whole text at every
  cursor. `Action::InsertTexts(Vec<String>)` is the distribute case; the count-mismatch fallback
  joins the slices with `\n` and `InsertText`s the whole at each cursor.

## 2. The text-injection design call (`InputEvent` vs facade method)

Zed's GUI text entry is the platform `InputHandler::replace_text_in_range`, which calls
`editor.insert(text)`. The faithful, pipeline-pure analog is a new
`InputEvent::InsertText(String)` → `Action::InsertText` → `executor` (reusing `insert_text`), **not**
a facade method that bypasses the resolver/history — a bypass would be the engine's only text mutation
skipping the pipeline, breaking the `buffer.rs` invariant that every edit is resolver→action→executor→
history. The resolver passes `InsertText` straight through, untouched by held-modifier / count / find
state (it is not a keystroke).

`InputEvent` consequently **drops `Copy`** (it now carries an owned `String`); the frontend moves each
event into `Editor::handle`, so nothing needs to copy one. `Action` gains `Redo`, `InsertText(String)`,
`InsertTexts(Vec<String>)` — all additive, so the merged tree still compiles where the frontend
constructs (never exhaustively matches) these enums. The executor's `match` on `Action` and the
resolver's `match` on `InputEvent` are exhaustive and live in this plan.

## 3. The core/frontend clipboard boundary (explicit)

008 ships **NO frontend and NO OS-clipboard I/O.** Per Zed's own split, the clipboard write/read lives
in `editor` (which has `cx`); the buffer/text layer only gathers. So:

- **In core (here):** `selected_texts()` (gather), `InsertText("")` (the delete that cut performs),
  `InsertText`/`InsertTexts` (the paste injection), `Redo`.
- **Wave-2 frontend:** `Ctrl+C/X/V` key interception, the OS clipboard read/write, and the per-cursor
  clipboard-metadata round-trip that decides distribute-vs-whole. Cut = (frontend) `selected_texts()`
  → OS clipboard → feed `InsertText("")`. The core API above is shaped to fit Zed's paste rule.

Redo, unlike the clipboard verbs, is **fully usable the moment it merges** — the frontend already
forwards every `KeyDown`.

## 4. Files changed (exactly the plan's core set, plus test-only `lib.rs`)

- `action.rs` — `Action::Redo`, `InsertText(String)`, `InsertTexts(Vec<String>)`.
- `input.rs` — `InputEvent::InsertText(String)`; `InputEvent` is now `Clone` (no longer `Copy`).
- `keymap.rs` — `CTRL` += `'y' → Redo`; new `CTRL_SHIFT` layer with `'z' → Redo`.
- `resolver.rs` — `InsertText` event passthrough; `ctrl+shift` layer selection (ASCII-case normalized).
- `executor.rs` — `Redo` arm (`history.redo`); `insert_texts` (per-cursor distribute with the
  `\n`-join count-mismatch fallback), reusing `insert_text` (empty string ⇒ delete).
- `buffer.rs` — `selected_texts() -> Vec<String>` (per-cursor rope `min..max` slices in `resolved()`
  order; empty for a bare cursor).
- `lib.rs` — **tests only** (the facade-level redo round-trips the plan's T0 asked for: `Ctrl+Y` and
  `Ctrl+Shift+Z`, text + selection, plus the top-of-stack no-op). No non-test `lib.rs` change — the
  `Editor::handle` routing already dispatches every event/action.

## 5. Verification (run on merged `main`)

| Check | Result |
|---|---|
| `cargo test -p alteria-core` | **173 passed** (was 152 @006; +21 across redo/insert/selected_texts + facade) |
| `cargo test` (default-members) | green, gpui-free, <1s |
| `cargo clippy -p alteria-core --all-targets -- -D warnings` | clean |
| `cargo fmt -p alteria-core -- --check` | clean |
| `cargo tree -p alteria-core \| grep -iE 'gpui\|ropey'` | empty — **core stayed pure** |

## 6. Deviations from the plan (recorded honestly)

- **Commit granularity:** the plan asked for three commits (T0 redo / T1 insert / T2 gather); the
  executor used two (redo+insert combined, then `selected_texts`). Cosmetic — all tests present.
- **`devlog/008` was missing** — written here at merge time (see the banner above).
- **`KEYMAP.md` redo entries were NOT added.** Plan 008 (and devlog 005 §5) treat `KEYMAP.md` as the
  binding source of truth — but `KEYMAP.md` **does not exist in the repo** and never has. So redo's
  binding currently lives in `keymap.rs` (`Ctrl+Y` / `Ctrl+Shift+Z`, tested) and in this devlog. The
  missing `KEYMAP.md` is a pre-existing repo-wide gap (every prior plan referenced it without the file
  being committed), flagged for a dedicated follow-up rather than improvised during this merge.

## 7. Non-goals kept out (wave 2 / later)

OS clipboard read/write · `Ctrl+C/X/V` interception & their bindings · paste's clipboard-metadata
round-trip *wiring* (the core API is here; the decision lives frontend) · forward `Delete` ·
undo/redo grouping/coalescing · IME (though `InsertText` is the seam it will reuse).
