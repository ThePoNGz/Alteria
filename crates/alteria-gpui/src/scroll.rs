//! Pure viewport geometry for vertical scrolling — which rows are visible, how
//! far the scroll may travel, and where to scroll so the primary cursor stays on
//! screen. **No gpui types**: plain `f32`/`usize` so the tricky math is unit-
//! tested headless, the same gpui-free discipline as `text_element::row_col`.
//!
//! Mirrors Zed's production scrolling (`../zed-main`):
//! - clamp: `editor/src/scroll.rs` `set_scroll_position` clamps the scroll top to
//!   `[0, max]` so the last line cannot scroll past the bottom.
//! - keep-cursor-visible: `editor/src/scroll/autoscroll.rs` `autoscroll_vertically`
//!   `AutoscrollStrategy::Fit` — `needs_scroll_up = target_top < start_row`,
//!   `needs_scroll_down = target_bottom >= end_row`, snapping the cursor row to the
//!   top or bottom of the viewport.
//!
//! **Simplifications vs Zed (skeleton scope, per plan 007):** we store a plain
//! pixel `scroll_top` rather than Zed's anchor-based scroll, and use a `0`
//! `vertical_scroll_margin` (Zed keeps a few rows of context around the caret).

/// Index of the first row whose top edge may be visible: the row that contains
/// `scroll_top`. Painting starts here.
pub(crate) fn first_visible_row(scroll_top: f32, line_height: f32) -> usize {
    if line_height <= 0.0 || scroll_top <= 0.0 {
        return 0;
    }
    (scroll_top / line_height).floor() as usize
}

/// How many rows to shape to fill the viewport. Rounds up so a partially visible
/// bottom line is still painted, plus one extra to cover the partially scrolled
/// top line when `scroll_top` falls mid-row. Over-painting one row is harmless —
/// it is clipped to the element bounds (`with_content_mask`).
pub(crate) fn visible_row_count(viewport_height: f32, line_height: f32) -> usize {
    if line_height <= 0.0 || viewport_height <= 0.0 {
        return 0;
    }
    (viewport_height / line_height).ceil() as usize + 1
}

/// Rows moved by PageUp/PageDown. Zed's `visible_row_count()` is the visible line
/// count minus one row, so page movement leaves one row of continuity.
pub(crate) fn page_row_count(viewport_height: f32, line_height: f32) -> usize {
    if line_height <= 0.0 || viewport_height <= 0.0 {
        return 1;
    }
    let visible_lines = (viewport_height / line_height).floor() as usize;
    visible_lines.saturating_sub(1).max(1)
}

/// Clamp ceiling for `scroll_top`: never scroll past the last line. At the max the
/// last line rests at the bottom of the viewport. A document shorter than the
/// viewport yields `0` (nothing to scroll). Mirrors Zed's `scroll.rs` clamp.
pub(crate) fn max_scroll_top(line_count: usize, line_height: f32, viewport_height: f32) -> f32 {
    (line_count as f32 * line_height - viewport_height).max(0.0)
}

/// Clamp a proposed `scroll_top` into `[0, max_scroll_top]`.
pub(crate) fn clamp_scroll_top(
    proposed: f32,
    line_count: usize,
    line_height: f32,
    viewport_height: f32,
) -> f32 {
    proposed.clamp(
        0.0,
        max_scroll_top(line_count, line_height, viewport_height),
    )
}

/// Keep-cursor-visible: given the primary cursor's row, return the `scroll_top`
/// that brings it back into view, or the unchanged `scroll_top` if it is already
/// visible. If the cursor is above the first visible row, scroll up so it is the
/// top line; if at/below the last fully visible row, scroll down so it is the
/// bottom line. Result is clamped to `[0, max]`. Mirrors Zed `autoscroll.rs`
/// `AutoscrollStrategy::Fit` with a `0` scroll margin.
pub(crate) fn autoscroll_top(
    cursor_row: usize,
    scroll_top: f32,
    line_height: f32,
    viewport_height: f32,
    line_count: usize,
) -> f32 {
    if line_height <= 0.0 {
        return scroll_top;
    }
    let visible_lines = viewport_height / line_height;
    let target_top = cursor_row as f32;
    let target_bottom = target_top + 1.0;
    let start_row = scroll_top / line_height;
    let end_row = start_row + visible_lines;

    let needs_up = target_top < start_row;
    let needs_down = target_bottom >= end_row;

    let new_row = if needs_up && !needs_down {
        target_top
    } else if needs_down && !needs_up {
        target_bottom - visible_lines
    } else {
        start_row
    };
    clamp_scroll_top(
        new_row * line_height,
        line_count,
        line_height,
        viewport_height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // line_height 20, viewport 100 -> exactly 5 full lines visible.
    const LH: f32 = 20.0;
    const VP: f32 = 100.0;

    #[test]
    fn first_row_at_top_is_zero() {
        assert_eq!(first_visible_row(0.0, LH), 0);
    }

    #[test]
    fn first_row_floors_partial_scroll() {
        // scrolled 2.25 rows down -> the first painted row is 2.
        assert_eq!(first_visible_row(45.0, LH), 2);
        assert_eq!(first_visible_row(40.0, LH), 2);
    }

    #[test]
    fn first_row_never_negative() {
        assert_eq!(first_visible_row(-10.0, LH), 0);
    }

    #[test]
    fn visible_count_covers_partial_top_and_bottom() {
        // 5 full lines fit; +1 guards the partially scrolled top line.
        assert_eq!(visible_row_count(VP, LH), 6);
        // 4.5 lines -> ceil 5, +1 = 6.
        assert_eq!(visible_row_count(90.0, LH), 6);
    }

    #[test]
    fn page_row_count_is_visible_lines_minus_one() {
        assert_eq!(page_row_count(VP, LH), 4);
        assert_eq!(page_row_count(90.0, LH), 3);
    }

    #[test]
    fn page_row_count_keeps_at_least_one_row() {
        assert_eq!(page_row_count(10.0, LH), 1);
        assert_eq!(page_row_count(0.0, LH), 1);
    }

    #[test]
    fn max_scroll_top_pins_last_line_to_bottom() {
        // 20 lines * 20px = 400 tall; viewport 100 -> 300 of scroll travel.
        assert_eq!(max_scroll_top(20, LH, VP), 300.0);
    }

    #[test]
    fn short_document_has_no_scroll() {
        // 3 lines (60px) fit inside the 100px viewport -> nothing to scroll.
        assert_eq!(max_scroll_top(3, LH, VP), 0.0);
    }

    #[test]
    fn clamp_floors_at_zero_and_ceils_at_content() {
        assert_eq!(clamp_scroll_top(-10.0, 20, LH, VP), 0.0);
        assert_eq!(clamp_scroll_top(9999.0, 20, LH, VP), 300.0);
        assert_eq!(clamp_scroll_top(120.0, 20, LH, VP), 120.0);
    }

    #[test]
    fn autoscroll_brings_cursor_above_viewport_up() {
        // showing rows 4.. (scroll_top 80), cursor on row 2 -> scroll up to row 2.
        assert_eq!(autoscroll_top(2, 80.0, LH, VP, 20), 40.0);
    }

    #[test]
    fn autoscroll_brings_cursor_below_viewport_down() {
        // showing rows 0..5 (scroll_top 0), cursor on row 7 -> row 7 to the bottom.
        // new top row = (7 + 1) - 5 = 3 -> 60px.
        assert_eq!(autoscroll_top(7, 0.0, LH, VP, 20), 60.0);
    }

    #[test]
    fn autoscroll_leaves_visible_cursor_alone() {
        // showing rows 2..7 (scroll_top 40), cursor on row 4 -> no change.
        assert_eq!(autoscroll_top(4, 40.0, LH, VP, 20), 40.0);
    }

    #[test]
    fn autoscroll_clamps_at_top() {
        assert_eq!(autoscroll_top(0, 0.0, LH, VP, 20), 0.0);
        // cursor on row 0 from a scrolled position -> back to the very top.
        assert_eq!(autoscroll_top(0, 40.0, LH, VP, 20), 0.0);
    }

    #[test]
    fn autoscroll_clamps_at_bottom() {
        // 6 lines (120px), viewport 100 -> max scroll 20. Cursor on the last row.
        // raw target = (5 + 1) - 5 = 1 row = 20px, exactly the clamp ceiling.
        assert_eq!(autoscroll_top(5, 0.0, LH, VP, 6), 20.0);
    }

    #[test]
    fn autoscroll_noop_when_everything_fits() {
        // 3-line doc inside a 5-line viewport -> never scrolls.
        assert_eq!(autoscroll_top(2, 0.0, LH, VP, 3), 0.0);
    }
}
