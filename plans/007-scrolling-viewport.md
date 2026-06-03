# Plan 007 — Scrolling & viewport: open files bigger than the window

**Goal:** Make `alteria-gpui` render and navigate a buffer **taller than the window**. Today the
walking skeleton (006) paints *every* line at `y = row * line_height` with no viewport — open a
200-line file and you see the top 30 and nothing scrolls. This plan adds a **scroll offset**, paints
**only the visible rows**, handles the **scroll wheel**, and **auto-scrolls to keep the primary
cursor in view** after every edit/motion. It is the gate to dogfooding on real source files and the
coordinate foundation every later frontend feature (mouse hit-testing, gutter, current-line
highlight) builds on.

**This is still a skeleton, not Zed's renderer.** Per CLAUDE.md the bar is *"feels instant on the
files I actually open"* — **not** huge-file/120fps engineering. So: shape only the visible lines, a
plain pixel scroll offset, vertical scrolling only. No sub-pixel virtualization, no GPU line cache,
no horizontal scroll (see Non-goals).

**Status / deps:** Plans 001–006 are MERGED on `main`. 006 stood up `crates/alteria-gpui`
(`main.rs`/`event.rs`/`view.rs`/`text_element.rs`) with a custom `Element` that shapes per line and
paints the primary cursor/selection. This plan **modifies only the frontend render/input path** and
aims for **zero `alteria-core` changes** (the cursor's row is already derivable from
`buffer.primary_resolved().head()` + `buffer.text()`, exactly as `text_element.rs::row_col` does it).
007 runs **alone** on the frontend files — no parallel sibling should touch `view.rs`/`text_element.rs`.

> **Sources of truth:** `../CLAUDE.md` (esp. *"GPUI — read before you write"*, *"Architecture — the
> one hard rule"*, and the latency/non-goal lines), `../KEYMAP.md`, and the pinned GPUI rev
> **`1dba7a28bb19ea2f3817ad7ed63a0fcb25d820d2`** (recorded in `devlog/006`; matches the `../zed-main`
> snapshot). The agent's GPUI memory is **stale** — verify every API against the checkout.
>
> **⮕ Zed is the source of truth for scrolling too.** Before writing the viewport math, read how Zed's
> production editor scrolls and *reproduce its approach* (the "Zed is the source of truth" hard rule
> applied to the frontend):
> - **`../zed-main/crates/editor/src/element.rs`** — `EditorElement` computes a **visible row range**
>   from the element bounds + scroll position and lays out / paints **only those lines**, offsetting
>   each line's origin by the scroll position. This is exactly what T1/T2 reproduce — *don't paint the
>   whole document.* Read its `scroll_position`/visible-range handling and the line-origin offset at
>   the `paint` sites.
> - **`../zed-main/crates/editor/src/scroll.rs`** (+ `scroll/autoscroll.rs`) — the `ScrollManager`,
>   how scroll position is stored, **clamped to content**, and the **autoscroll** that brings the
>   newly-moved cursor back into view. Mirror the *behavior* (clamp + keep-cursor-visible); we store a
>   simple pixel/row offset rather than Zed's anchor-based scroll (skeleton scope — note the
>   simplification in the devlog).
> - **gpui examples** for the mechanics: `../zed-main/crates/gpui/examples/uniform_list.rs` (a
>   virtualized list — the minimal "render only visible items + a scroll handle" pattern) and
>   `input.rs` (the custom-`Element` `request_layout`/`prepaint`/`paint` shape we already extend).
>
> Cite the Zed file you mirrored in `devlog/007` for each non-trivial decision.

### API facts to confirm against the pinned rev (re-verify at execution — do not write from memory)
- **Scroll-wheel handler** on an interactive element: `div().on_scroll_wheel(|ev: &ScrollWheelEvent,
  &mut Window, &mut App| …)`. Confirm the event shape — likely
  `ScrollWheelEvent { position, delta: ScrollDelta, modifiers, .. }` where
  `ScrollDelta` is `Pixels(Point<Pixels>)` or `Lines(Point<f32>)`. Use `delta.pixel_delta(line_height)`
  if such a helper exists (check gpui's `ScrollDelta`); otherwise convert `Lines` → pixels via
  `line_height` yourself. **Verify the exact names against the checkout.**
- **Clipping:** to stop lines painting outside the viewport, paint inside `window.with_content_mask(
  Some(ContentMask { bounds }), |window| { … })` (confirm `ContentMask`/`with_content_mask` signature
  in the rev) — mirror how Zed masks the text region in `element.rs`.
- `window.line_height()`, `window.text_system().shape_line(...)`, `ShapedLine::x_for_index`,
  `window.request_layout`, `window.paint_quad`, `fill(...)`, `Bounds`/`point`/`size`/`px` — all used
  in 006's `text_element.rs`; unchanged, just re-confirm.
- `cx.notify()` triggers a redraw (used in 006's `view.rs`).

## Files this plan owns
```
crates/alteria-gpui/src/scroll.rs        # NEW — pure viewport math (visible-row range, clamp,
                                         #       autoscroll target). No gpui types → unit-testable.
crates/alteria-gpui/src/view.rs          # scroll state on EditorView; on_scroll_wheel handler;
                                         #   autoscroll-to-cursor after handle(); declare `mod scroll`
crates/alteria-gpui/src/text_element.rs  # paint only visible rows, offset by scroll; clip to bounds
devlog/007-scrolling-viewport.md         # the record (Zed files mirrored, scroll model, clamp/autoscroll)
```
**No `alteria-core` files. No `main.rs`/`event.rs` changes.** If a tiny additive read-only core
accessor turns out unavoidable, the target is still zero — note it if it happens (there should be none).

## Tasks

> **Before writing any GPUI code:** read Zed's `editor/src/element.rs` visible-range + line-origin
> handling and `editor/src/scroll.rs` clamp/autoscroll, plus the `uniform_list.rs` example. Reproduce
> Zed's *approach*; verify every API against the pinned rev. (See the Sources block.)

### T0 — Pure viewport math, test-first (`scroll.rs`)
Factor the geometry into **pure functions over plain numbers** (no gpui types), so the tricky parts
are unit-tested headless — the same pattern as 006's `translate_key`/`row_col`. Write the tests first.
- `first_visible_row(scroll_top: f32, line_height: f32) -> usize` and
  `visible_row_count(viewport_height: f32, line_height: f32) -> usize` (round so a partially-visible
  bottom line is still painted).
- `max_scroll_top(line_count: usize, line_height: f32, viewport_height: f32) -> f32` — the clamp
  ceiling (never scroll past the last line; allow a little overscroll only if Zed does — default: clamp
  so the last line rests at the bottom, `max(0, line_count*lh - viewport_h)`).
- `clamp_scroll_top(proposed, line_count, line_height, viewport_height) -> f32` — `[0, max]`.
- `autoscroll_top(cursor_row, scroll_top, line_height, viewport_height, line_count) -> f32` — Zed's
  keep-cursor-visible: if the cursor row is above the first visible row, scroll up so it's the top
  line; if below the last fully-visible row, scroll down so it's the bottom line; else unchanged. (Zed
  adds a small `vertical_scroll_margin`; a 0-margin version is fine for the skeleton — note it.)
- **Tests:** cursor above viewport → scrolls up to it; below → down to it; inside → no change; clamp
  at top (never negative) and bottom (never past content); a document shorter than the viewport →
  `max_scroll_top == 0`, everything visible, no scroll. Mirror Zed's autoscroll semantics from
  `scroll/autoscroll.rs`.
- **Commit:** `feat(gpui): pure viewport math — visible range, clamp, autoscroll (test-first)`.

### T1 — Paint only the visible rows, offset by scroll (`text_element.rs`)
- Read the scroll offset from the view (a `Pixels` `scroll_top`, stored on `EditorView`).
- In `prepaint`: compute the visible `[first_row, first_row + visible_count]` (T0), **shape only those
  lines** (not the whole document), and place each at `y = bounds.top() + row*line_height - scroll_top`.
  Cursor/selection quads (`caret_quad`/`selection_quads`) offset by the same `scroll_top`; a quad whose
  row is outside the visible range is skipped.
- In `request_layout`: the element now **fills the viewport** (`size_full`-style — height = available,
  not the whole document), since the element owns scrolling itself (mirror `EditorElement`, which is
  not wrapped in a generic scroll container).
- **Clip** all painting to the element bounds (`with_content_mask`) so a partially-scrolled top/bottom
  line doesn't bleed outside the viewport.
- Keep everything else from 006 intact (focus-gated caret, `.ok()` on line paint, per-row selection
  quads). `row_col` stays as-is.
- **Verify:** open a file taller than the window — the top rows render; nothing paints outside the
  viewport.
- **Commit:** `feat(gpui): render only the visible rows, offset by the scroll position`.

### T2 — Scroll wheel + autoscroll-to-cursor (`view.rs` + `scroll.rs` wiring)
- Add `scroll_top: Pixels` (default 0) to `EditorView`.
- `on_scroll_wheel`: convert the wheel `delta` to pixels (via `line_height` for line-deltas), add to
  `scroll_top`, **clamp** with `clamp_scroll_top` (T0), `cx.notify()`. Wire it on the root `div()`
  alongside the existing raw key/modifier handlers (it is *not* a quasimode — an ordinary wheel
  handler is correct here).
- **Autoscroll:** after `editor.handle(ev)` returns `true` (state changed), recompute
  `scroll_top = autoscroll_top(cursor_row, …)` so a cursor that moved off-screen (Alt+WASD past the
  edge, a paste, typing at the bottom) is brought back into view — exactly as Zed autoscrolls after a
  movement/edit. `cursor_row` comes from `row_col(&editor.buffer.text(), head)` (reuse, or expose the
  helper). Viewport height: read the last laid-out bounds height (cache it from `prepaint`, the way
  Zed keeps `last_bounds`), or recompute from the window — pick the approach that matches the rev and
  note it.
- **Verify (the moment this plan exists for):**
  - Mouse-wheel scrolls a long file up/down; can't scroll above line 0 or below the last line.
  - Hold **Alt + S** to the bottom of the window and past it → the **view follows the cursor** (no
    cursor lost off-screen); **Alt + W** back up scrolls up. Type at the bottom → it stays in view.
  - Releasing Alt and typing still works (006's guarantees intact); short files behave exactly as
    before (no scroll, `scroll_top` pinned at 0).
- **Commit:** `feat(gpui): scroll-wheel scrolling + autoscroll the cursor into view`.

### T3 — `devlog/007`
- Record: the Zed files mirrored (`element.rs` visible-range/line-offset, `scroll.rs` clamp/autoscroll)
  and **where we simplified** (plain pixel `scroll_top` vs Zed's anchor-based scroll; 0 vs Zed's
  `vertical_scroll_margin`); the confirmed `ScrollWheelEvent`/`ScrollDelta`/`with_content_mask` API on
  the pinned rev and any surprises vs. priors; how viewport height is obtained; and that
  `cargo tree -p alteria-core` is still gpui-free (zero core changes).
- **Verify:** `cargo test` (default-members) green & fast; `cargo test -p alteria-gpui` green
  (the T0 scroll-math tests run here); `cargo build -p alteria-gpui` + `cargo clippy -p alteria-gpui`
  + `cargo fmt -p alteria-gpui -- --check` all clean; `cargo tree -p alteria-core | grep -iE 'gpui|ropey'`
  empty.
- **Commit:** `docs(devlog): record 007 — scrolling & viewport`.

## Done criteria
- A file taller than the window renders correctly; **mouse-wheel scrolls** it; scroll is **clamped**
  to `[top, last line]`.
- **The cursor stays visible:** Alt+WASD (or typing/paste) that moves the caret off-screen
  **auto-scrolls** it back into view; scrolling back up works.
- Only the **visible rows are shaped/painted**, offset by the scroll position and clipped to the
  viewport (no full-document paint, no bleed outside bounds).
- **Zero `alteria-core` changes**; the one hard rule intact (`cargo tree` clean). The pure viewport
  math lives in `scroll.rs` with headless unit tests; the wheel handler is a plain handler, **not** a
  gpui Action (006's quasimode handlers untouched).
- Engine commands (`cargo test`/`build`/`clippy` on default-members) stay GPUI-free, fast, and green.
- `devlog/007` records the Zed approach mirrored, the scroll model + its simplifications, and the
  confirmed scroll/clip API on the pinned rev.

## Non-goals (explicitly out of scope — keep the slice minimal)
Horizontal scrolling / line wrapping · sub-line or GPU-cached virtualization · smooth/animated
(kinetic) scrolling · scrollbar rendering & drag · Zed's anchor-based scroll & `vertical_scroll_margin`
tuning · scroll-past-end overscroll · 120fps/huge-file perf · gutter/line numbers (a later plan) ·
mouse click-to-place (a later plan; this plan only adds the *wheel*, no click hit-testing). These
come later; 007 only has to make a tall file viewable with the cursor kept in view.

## Notes
- **Read the real GPUI/Zed scrolling code first** (`editor/src/element.rs` visible-range + line
  offset, `editor/src/scroll.rs` clamp/autoscroll, `gpui/examples/uniform_list.rs`). Verify
  `ScrollWheelEvent`/`ScrollDelta`/`with_content_mask` against the pinned rev — the API churns weekly.
- **Build with the shared cache:** `export CARGO_TARGET_DIR=~/.cache/alteria-target` (GPUI's first
  compile is huge; don't pay it twice — see CLAUDE.md / devlog 006 §4).
- The decoupled core is the safety net: this is pure frontend; if a scroll API broke it's contained to
  `alteria-gpui` and the engine is untouched.
