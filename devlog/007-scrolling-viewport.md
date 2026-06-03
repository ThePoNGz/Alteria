# Devlog 007 — Scrolling & viewport: open files bigger than the window

**Date:** 2026-06-03
**Plan:** `plans/007-scrolling-viewport.md`
**Branch:** `alteria_a1`
**Status:** ✅ Complete — `alteria-gpui` now renders and navigates a buffer **taller than the
window**. 006 painted *every* line at `y = row * line_height` with no viewport, so a 200-line file
showed only the top ~30 and nothing scrolled. 007 adds a plain-pixel `scroll_top`, paints **only the
visible rows** (offset by the scroll and clipped to the viewport), handles the **mouse wheel**, and
**autoscrolls the primary cursor back into view** after every motion/edit. Build/test/clippy/fmt all
clean; the viewport math is gpui-free and unit-tested headless.

**Zero `alteria-core` changes.** This was a pure frontend slice. The cursor's row is still derived
from `buffer.primary_resolved().head()` + `buffer.text()` via `text_element::row_col` (now
`pub(crate)` so `view.rs` can reuse it for autoscroll) — no new core accessor was needed.
`cargo tree -p alteria-core` still shows **no gpui, no ropey**; the one hard rule holds.

**Still a skeleton, not Zed's renderer.** Per CLAUDE.md the bar is *"feels instant on the files I
actually open"* — not huge-file/120fps engineering. So: shape only the visible lines, a plain pixel
scroll offset, vertical-only. No sub-pixel virtualization, no GPU line cache, no horizontal scroll
(see §9).

---

## 1. Zed is the source of truth — what was mirrored

Per the "Zed is the source of truth" hard rule, the viewport math reproduces Zed's production
scrolling rather than being re-derived. Read against the `../zed-main` snapshot (= the pinned rev,
see §3):

- **Visible-range render** — `crates/editor/src/element.rs`. Zed's `EditorElement` computes a
  visible row range from the element bounds + scroll position and lays out / paints **only those
  rows**, offsetting each line origin by the scroll. We reproduce this in `text_element.rs`:
  `first_visible_row` + `visible_row_count` bound the rows we `shape_line`, and each line paints at
  `y = bounds.top() + row * line_height − scroll_top`. *We do not paint the whole document.*
- **Fill the viewport, own scrolling** — same file, `request_layout` for `EditorMode::Full` sizes
  the element to `relative(1.)` (width **and** height) rather than the document height, because the
  element scrolls itself (it is not wrapped in a generic scroll container). We copied that exactly
  (was `line_height * line_count` in 006).
- **Clamp** — `crates/editor/src/scroll.rs` `set_scroll_position` clamps the scroll top to
  `[0, max]` so the last line cannot scroll past the bottom. `scroll::max_scroll_top` /
  `clamp_scroll_top` reproduce it (`max(0, line_count*lh − viewport_h)`).
- **Keep-cursor-visible (autoscroll)** — `crates/editor/src/scroll/autoscroll.rs`
  `autoscroll_vertically`, `AutoscrollStrategy::Fit` (autoscroll.rs:238–259). The exact predicate:
  `needs_scroll_up = target_top < start_row`, `needs_scroll_down = target_bottom >= end_row`; scroll
  up so the cursor is the top line, or down so it is the bottom line, else leave it. `autoscroll_top`
  reproduces this with `target_top = cursor_row`, `target_bottom = cursor_row + 1`.
- **Wheel sign + clamp** — `crates/editor/src/element/mouse.rs:504–562`. Zed computes
  `y = (current.y * line_height − delta.y * sensitivity) / line_height`, i.e. **`scroll_top` decreases
  by `delta.y`**, then `.clamp(0, scroll_max)`. We do `scroll_top -= delta.pixel_delta(lh).y` then
  `clamp_scroll_top` (sensitivity left at 1.0 for the skeleton).
- **Mechanics** — `gpui/examples/input.rs` for the custom-`Element`
  `request_layout`/`prepaint`/`paint` shape and the **`last_bounds`** cache (input.rs:578–580 writes
  `last_bounds` back to the entity inside `paint`); `gpui/examples/uniform_list.rs` for the "render
  only the visible slice" idea.

## 2. The scroll model and where we simplified vs Zed

- **Plain pixel `scroll_top`, not anchor-based.** Zed stores scroll as a buffer **anchor** + offset
  so the view stays put across edits above it; we store a single `Pixels` `scroll_top` on
  `EditorView`. Fine for a read-mostly skeleton; revisit when edits above the viewport need to hold
  position.
- **`0` vertical scroll margin.** Zed keeps a few rows of context around the caret
  (`vertical_scroll_margin`, clamped into the Fit math). We use margin `0` — the cursor snaps exactly
  to the top/bottom edge. The plan explicitly allows the 0-margin version; it is a one-line change
  later (subtract the margin from `target_top` / add to `target_bottom`).
- **Vertical only.** No horizontal scroll / wrap (a non-goal); `scroll.rs` is purely the Y axis.
- **Sensitivity 1.0**, no trackpad axis-locking (`OngoingScroll::filter`) — out of scope.

`scroll.rs` carries these caveats in its module doc so the simplification is visible at the call
site, not buried here.

## 3. GPUI API confirmed on the pinned rev (not written from memory)

Rev `1dba7a28bb19ea2f3817ad7ed63a0fcb25d820d2` (= the `../zed-main` snapshot; see devlog 006 §1).
Every API below was read from the checkout:

| API | Where (in `../zed-main`) | Shape used |
|---|---|---|
| `ScrollWheelEvent` | `gpui/src/interactive.rs:428` | `{ position, delta: ScrollDelta, modifiers, touch_phase }` |
| `ScrollDelta` | `gpui/src/interactive.rs:460` | `Pixels(Point<Pixels>)` \| `Lines(Point<f32>)` |
| `ScrollDelta::pixel_delta(line_height)` | `interactive.rs:520` | `-> Point<Pixels>` (converts `Lines` via `line_height`) — used directly |
| `InteractiveElement::on_scroll_wheel` | `gpui/src/elements/div.rs:934` | fluent `.on_scroll_wheel(impl Fn(&ScrollWheelEvent, &mut Window, &mut App))`, via `cx.listener` |
| `Window::with_content_mask` | `gpui/src/window.rs:3146` | `(Option<ContentMask<Pixels>>, impl FnOnce(&mut Window) -> R)` |
| `ContentMask` | `gpui/src/window.rs:1745` | `ContentMask { bounds }` (one public field) |
| `Window::viewport_size` | `gpui/src/window.rs:2246` | `-> Size<Pixels>` (the bootstrap fallback before first paint) |
| `Pixels ↔ f32` | `gpui/src/geometry.rs:2903/2909` | `f32::from(px)` / `px(f32)` at the boundary; `Pixels` derives `Sub`/`PartialEq` |

No surprises vs the 006 priors; the `request_layout`/`prepaint`/`paint` signatures and
`shape_line`/`x_for_index`/`paint_quad`/`fill` are unchanged from 006. `should_handle_scroll`
(`window.rs:638`) is just "the hitbox is topmost under the cursor", so a plain interactive `div`
(it already has `track_focus` + key listeners) receives wheel events with no `overflow_scroll`
container — exactly the non-quasimode, ordinary-handler path the plan wanted.

## 4. How the viewport height is obtained

Autoscroll runs in `view.rs` (in the key handler, *before* the next paint), so it needs the viewport
height from the **last** frame — mirroring Zed, which autoscrolls against the last known layout.
`text_element::paint` caches the painted bounds onto the view
(`self.view.update(cx, |v, _| v.last_bounds = Some(bounds))`, the `input.rs` `last_bounds` pattern);
`EditorView::viewport_height` reads `last_bounds.size.height`, falling back to
`window.viewport_size().height` before the first paint (the window can't receive a wheel/key event
until it has painted at least once, so the fallback is only ever a safety net). Chosen over
recomputing from the window because it is the literal text region — the foundation the later
mouse-hit-testing plan will reuse, just as `input.rs` reuses `last_bounds` for hit-testing.

## 5. Module / ownership notes

`scroll.rs` is a new top-level frontend file (`crates/alteria-gpui/src/scroll.rs`) as the plan
specifies, but the plan also forbids touching `main.rs` (where a crate-root `mod scroll;` would
normally go). Resolved by declaring it from `view.rs` with an explicit
`#[path = "scroll.rs"] pub(crate) mod scroll;` — the file stays at `src/scroll.rs`, `main.rs` is
untouched, and both `view.rs` and `text_element.rs` reach the math via `crate::view::scroll`.

Files touched (exactly the plan's set): `src/scroll.rs` (new), `src/view.rs`, `src/text_element.rs`,
`devlog/007`. No `alteria-core`, no `main.rs`, no `event.rs`.

## 6. `scroll.rs` — the pure viewport math (test-first, gpui-free)

Five functions over plain `f32`/`usize`, no gpui types — same headless-testable discipline as
`row_col`/`event::translate`:

- `first_visible_row(scroll_top, lh) = floor(scroll_top / lh)` (clamped ≥ 0).
- `visible_row_count(viewport_h, lh) = ceil(viewport_h / lh) + 1` — the `+1` covers the partially
  scrolled top line when `scroll_top` falls mid-row; over-painting one row is harmless (clipped).
- `max_scroll_top(n, lh, vh) = max(0, n*lh − vh)` — last line rests at the bottom; `0` for a doc
  shorter than the viewport.
- `clamp_scroll_top(proposed, …) = proposed.clamp(0, max_scroll_top)`.
- `autoscroll_top(cursor_row, scroll_top, lh, vh, n)` — Zed's Fit predicate, returns the (clamped)
  `scroll_top` that brings the cursor into view, or the unchanged value if it's already visible.

13 unit tests drive these: first-row floor / never-negative, visible-count covers partial top+bottom,
max-scroll pins the last line / is 0 for short docs, clamp floors+ceils, and autoscroll for
cursor-above (scrolls up), cursor-below (scrolls down to bottom), cursor-inside (no change),
clamp-at-top, clamp-at-bottom (last row → exactly the clamp ceiling), and the short-doc no-op.

## 7. Rendering & input wiring

- `text_element.rs`: `prepaint` reads `scroll_top` from the view, computes `[first_row, last_row)`
  from the bounds, shapes only those lines (`split('\n').skip(first_row).take(...)`), and builds the
  caret/selection quads offset by `scroll_top` — `row.checked_sub(first_row)` + `lines.get(idx)` skip
  a cursor/selection row scrolled out of view. `paint` wraps everything in
  `with_content_mask(ContentMask { bounds })` so a partial top/bottom line can't bleed outside, and
  caches `last_bounds`. `request_layout` now fills the viewport (`relative(1.)` × 2).
- `view.rs`: `EditorView` gains `scroll_top: Pixels` (default `px(0.)`) and
  `last_bounds: Option<Bounds<Pixels>>` (default `None`). `on_scroll_wheel` (wired on the root `div`
  next to the raw key/modifier handlers) does the Zed sign+clamp and `cx.notify()` only on a real
  change. `feed` now calls `autoscroll_to_cursor` whenever `editor.handle(ev)` returns `true`, so a
  caret moved off-screen (Alt+WASD past the edge, typing at the bottom) is pulled back into view.
  The wheel is an **ordinary handler, not a gpui Action** — 006's quasimode handlers are untouched.

## 8. Verification

| Check | Result |
|---|---|
| `cargo test -p alteria-gpui` | **27 passed** (13 scroll math + 5 row_col + 9 event-mapping) |
| `cargo test` (default-members) | **alteria-core 152 passed**, GPUI-free, <1s |
| `cargo build -p alteria-gpui` | clean (gpui cache hit, ~4s) |
| `cargo clippy -p alteria-gpui -- -D warnings` | clean (zero warnings) |
| `cargo fmt -p alteria-gpui -- --check` | clean |
| `cargo tree -p alteria-core \| grep -iE 'gpui\|ropey'` | empty — **core stayed pure** |
| `grep -rn 'gpui' crates/alteria-core/src` | none — no gpui type in the core |

### Hands-on checklist (the maintainer's to run — needs a window, a wheel, and a held Alt)
The behaviours 007 exists for are interactive (mouse wheel; holding Alt while WASD-ing past the
edge), which the executor can't drive headless. The unit tests prove the math; this confirms the
wiring on real hardware. Run: `CARGO_TARGET_DIR=~/.cache/alteria-target cargo run -p alteria-gpui <a
file taller than the window>`

1. **Mouse-wheel** scrolls the file up/down; you can't scroll above line 0 or below the last line
   (the last line rests at the bottom).
2. Only the **visible rows** render, offset by the scroll; nothing paints outside the viewport (no
   bleed at the top/bottom edge).
3. **Hold Alt + S** to the bottom of the window and past it → the **view follows the cursor** (no
   caret lost off-screen); **Alt + W** back up scrolls up; typing at the bottom keeps the caret in
   view.
4. Releasing Alt and typing still works (006's guarantees intact); a **short** file behaves exactly
   as before — no scroll, `scroll_top` pinned at 0.

## 9. Non-goals kept out (later plans)

Horizontal scroll / line wrapping · sub-line or GPU-cached virtualization · smooth/kinetic
scrolling · scrollbar rendering & drag · Zed's anchor-based scroll & `vertical_scroll_margin`
tuning · scroll-past-end overscroll · 120fps/huge-file perf · gutter/line numbers · mouse
click-to-place (007 adds only the *wheel*; no click hit-testing). 007 only had to make a tall file
viewable with the cursor kept in view — it does.
