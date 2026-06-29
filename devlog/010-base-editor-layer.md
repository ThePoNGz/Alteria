# Devlog 010 - Import Zed's base editor layer

**Date:** 2026-06-29
**Plan:** `plans/010-base-editor-layer.md`
**Branch:** `alteria_a10`
**Status:** Implemented with automated checks clean; pending a hands-on runtime
pass. Zed Linux base keys, forward Delete, arrows, Shift-arrows, Home/End,
Ctrl+A/C/X/V, page-row movement, and OS clipboard routing are wired. Mouse
click/drag selection is wired. Platform committed text enters through GPUI
`EntityInputHandler`; marked-text IME preedit remains.

## T0 Runtime Audit

Checks run before code changes:

| Check | Result |
|---|---|
| `cargo test` | pass: `alteria-core` 173 tests, `syntax` 6 tests, vendored crates/doc tests clean |
| `cargo test -p alteria-gpui` | pass: 27 tests |
| `cargo build -p alteria-gpui` | pass |
| `cargo run -p alteria-gpui Cargo.toml` | launched and stayed running; closed with Ctrl-C after confirming startup |

Manual GUI interaction was not verified through the terminal session. The app launch
itself works from this branch and display environment.

## Direct Dependency Audit

Directly depending on Zed's `editor` crate is not a good fit for this slice.
`../zed-main/crates/editor/Cargo.toml` pulls the full Zed application stack:
`gpui`, `project`, `workspace`, `settings`, `theme`, `language`,
`multi_buffer`, LSP, git, client/RPC, UI crates, telemetry, and more.

Alteria's current shape is a single-buffer editor with a gpui-free
`alteria-core`; importing `editor` directly would pull GPUI and application
state into the backend boundary. The selected behavior will therefore be ported
into Alteria's smaller seams while following Zed behavior and naming the
simplifications.

## Import Map

| Behavior | Zed source | Alteria target | Decision |
|---|---|---|---|
| Linux base bindings | `assets/keymaps/default-linux.json` Editor block lines 67-122 | `KEYMAP.md`, `input.rs`, `keymap.rs`, `resolver.rs`, `event.rs` | Port selected Linux subset |
| Action surface | `crates/editor/src/actions.rs` | `action.rs` | Port plain-data variants; no GPUI `Action` derive |
| Text insertion | `crates/editor/src/input.rs::handle_input`, `insert`, `replace_selections` | existing `InputEvent::InsertText`, executor insert paths | Existing core path already mirrors the single-buffer subset |
| Backspace/delete | `crates/editor/src/editor.rs::backspace`, `delete` | `executor.rs` | Backspace mostly present; forward delete to add |
| Left/right/up/down | `crates/editor/src/movement.rs` | `executor.rs` `Motion::Char` | Existing single-buffer subset matches core ideas: wrap, clip, vertical goal |
| Home/End | `movement.rs::line_beginning`, `line_end`; action structs with soft-wrap/indent flags | `executor.rs` `Motion::LineEdge` plus base bindings | Port logical-line subset; soft-wrap and indent-stop parity deferred until display map/indent support exists |
| Page movement | `actions.rs::MovePageUp/MovePageDown`, `movement.rs::up_by_rows/down_by_rows`, scroll/autoscroll | core page-row action plus `alteria-gpui` viewport plumbing | Port visible-row subset using current viewport height/line height |
| Select all | `crates/editor/src/selection.rs::select_all` | `executor.rs`, `buffer.rs` | Port whole-buffer selection as `0..len` in single-buffer offset space |
| Clipboard copy/cut/paste | `crates/editor/src/clipboard.rs::do_copy`, `cut_common`, `paste`, `do_paste` | core facade effects plus `alteria-gpui` clipboard handling | Port OS I/O in frontend; keep core gpui-free |
| Mouse selection | `crates/editor/src/element/mouse.rs`, `selection.rs::SelectPhase` | `text_element.rs`, `view.rs`, possibly `mouse.rs` | Port single-buffer click/shift-click/drag; defer gutter, multibuffer, drag-drop moving |
| Platform text input / IME | `crates/editor/src/input.rs`, GPUI `InputHandler` examples | `alteria-gpui` input seam | Committed text path to port; marked-text composition likely deferred |

## Zed Bindings Selected For This Plan

Imported from the Linux Editor context:

- `backspace`, `shift-backspace`, `delete`
- `ctrl-c`, `ctrl-x`, `ctrl-v`
- `ctrl-z`, `ctrl-y`, `ctrl-shift-z`
- `up`, `down`, `left`, `right`
- `shift-up`, `shift-down`, `shift-left`, `shift-right`
- `home`, `end`, `shift-home`, `shift-end`
- `pageup`, `pagedown`, `shift-pageup`, `shift-pagedown`
- `ctrl-a`

Skipped Zed bindings in the same block for this slice: Tab/backtab, word
deletion/motion (`ctrl-backspace`, `ctrl-delete`, `ctrl-left`, `ctrl-right` and
shift variants), document start/end, line select, formatting, code actions,
signature help, git/diff/editor UI commands, character palette, and line-number
toggles. Those are outside plan 010's selected normal-editor subset.

## T2-T5 Implemented Subset

### Core input/action surface

- Added plain-data `Key` variants for Delete, arrows, Home/End, and PageUp/PageDown.
- Added `Action` variants for forward delete, select-all, copy/cut/paste effects,
  and page movement with a frontend-supplied row count.
- Resolver behavior now mirrors the selected Zed Linux bindings:
  - no Alt/Ctrl/Super: Delete, arrows, Shift-arrows, Home/End, Shift-Home/End;
  - PageUp/PageDown request a frontend page-row count;
  - Ctrl+A/C/X/V dispatch standard editor actions;
  - existing Alt quasimode bindings still win when Alt is held.

### Movement, delete, select-all

- `Delete` mirrors Zed `editor.rs::delete`: if a selection is non-empty, delete
  the span; otherwise extend one grapheme right and replace with `""`.
- Arrow movement reuses the existing Zed-style single-buffer motion layer:
  grapheme-aware left/right, goal-column vertical motion, and Shift extension.
- Home/End reuse `Motion::LineEdge`. This is the logical-line subset; Zed's
  soft-wrap and indent-stop flags remain display-map/indent debt.
- `Ctrl+A` selects `0..buffer.len()` as the single-buffer equivalent of
  Zed `Anchor::Min..Anchor::Max`.

### Page movement

- Page keys resolve to `Action::MovePage { rows: 0 }`, which the facade returns
  as `EditorEffect::MovePage`.
- `alteria-gpui` computes the Zed subset of `visible_row_count()`: visible full
  lines minus one row, at least one row, from viewport height / line height.
- Core applies that row count through the same vertical goal motion used by Up/Down.

### Clipboard

- `Editor::handle_result` preserves the old `handle() -> bool` wrapper while
  exposing frontend effects for OS clipboard/page work.
- Copy/cut gather uses Zed's empty-selection rule: empty selections copy/cut the
  whole current line, adding a trailing newline for a last line without one.
- Cut deletes explicit resolved ranges while preserving the original pre-cut
  selection as the undo selection.
- GPUI writes clipboard text with JSON metadata shaped as `(len, is_entire_line)`
  per copied selection, and paste reads that metadata from `ClipboardEntry::String`.
- Matching metadata lengths distribute paste text per cursor. Count mismatch
  follows Zed's fallback: use the whole clipboard for each cursor, while still
  preserving the "all copied selections were full lines" insertion position.
- Full-line metadata now follows Zed `do_paste`: when the copied slice came from
  an empty selection/current-line copy and the destination is also empty, paste
  before the current line rather than at the cursor column.
- Without metadata, paste mirrors Zed's external-editor fallback: if there are
  multiple live cursors and the clipboard line count exactly matches, distribute
  one line per cursor; otherwise paste the whole clipboard at each cursor.

## T6 Mouse Placement And Drag Selection

- Added core selection placement methods on `Editor`:
  - `set_cursor(offset)`;
  - `extend_primary_to(offset)`;
  - `set_primary_range(anchor, head)`.
- Added `text_element::offset_for_point`, the single-buffer subset of Zed's
  `PositionMap::point_for_position`: row from bounds + scroll + line height,
  column from shaping the target line and using GPUI `closest_index_for_x`.
- Wired GPUI left-click, Shift-left-click, left-drag, and mouse-up:
  - click focuses the editor and places the primary cursor;
  - Shift-click extends the primary selection;
  - drag keeps the original byte offset as anchor and updates the primary range;
  - mouse-up ends the pending drag state.

Deferred Zed mouse behavior: double-click word selection, gutter selection,
columnar selection, drag-and-drop moving selections, multibuffer/diff/link
special cases, and drag autoscroll at viewport edges.

## T7 Platform Text Input Seam

- Registered `ElementInputHandler::new(bounds, view)` during text element paint,
  matching GPUI's current `examples/input.rs` pattern.
- Implemented `EntityInputHandler` for `EditorView`:
  - `replace_text_in_range` converts UTF-16 replacement ranges to byte ranges,
    places the primary selection, then feeds `InputEvent::InsertText`;
  - selected/text ranges map between byte offsets and UTF-16 code-unit offsets;
  - `character_index_for_point` reuses the T6 hit-test path and returns UTF-16;
  - bounds use the same shaped-line x positions as rendering.
- Raw keydown now drops base-layer printable characters so committed platform
  text owns ordinary typing; named keys and modifier layers still use raw events.

Deferred T7 debt: full marked-text/preedit composition is not implemented.
`replace_and_mark_text_in_range` intentionally does not mutate the buffer; real
committed text still enters through `replace_text_in_range`.

## T8 Verification

Automated checks after closing the full-line paste metadata path:

| Check | Result |
|---|---|
| `cargo test` | pass: `alteria-core` 198 tests, `syntax` 6 tests, vendored crates/doc tests clean |
| `cargo test -p alteria-gpui` | pass: 35 tests |
| `cargo build -p alteria-gpui` | pass |
| `cargo clippy -p alteria-core --all-targets -- -D warnings` | pass |
| `cargo clippy -p alteria-gpui -- -D warnings` | pass |
| `cargo tree -p alteria-core` dependency scan | no `gpui` or `ropey` dependency |
| `cargo run -p alteria-gpui Cargo.toml` | sandboxed launch hit GPUI Wayland `NoCompositor`; unsandboxed launch stayed running until Ctrl-C |

Runtime checklist note: the terminal session verified startup and no immediate
crash against the host compositor. Hands-on behavior for typing/arrows,
selection, clipboard, mouse drag, scrolling, and Alt-WASD still needs a human
pass in the opened window.

## Verification So Far

| Check | Result |
|---|---|
| `cargo test -p alteria-core` | pass: 195 tests |
| `cargo test -p alteria-gpui` | pass: 29 tests |
| `git diff --check` | clean |

After T6:

| Check | Result |
|---|---|
| `cargo test -p alteria-core` | pass: 196 tests |
| `cargo test -p alteria-gpui` | pass: 31 tests |

After T7:

| Check | Result |
|---|---|
| `cargo test -p alteria-core` | pass: 196 tests |
| `cargo test -p alteria-gpui` | pass: 35 tests |
