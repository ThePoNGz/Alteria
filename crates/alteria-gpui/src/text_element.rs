//! The buffer renderer: a custom GPUI [`Element`] that shapes and paints **only
//! the rows visible at the current scroll offset**, plus the primary cursor and
//! selection, clipped to the viewport.
//!
//! This mirrors GPUI's own `examples/input.rs` `TextElement`
//! (`request_layout`/`prepaint`/`paint`, `text_system().shape_line(...)`,
//! `line.x_for_index(..)`, `fill(..)` cursor quad, the `last_bounds` cache),
//! extended from one line to a scrolling viewport — which is exactly how Zed's
//! production `editor/src/element.rs` lays out text: it computes a visible row
//! range from the element bounds + scroll position, shapes/paints **only those
//! rows**, and offsets each line's origin by the scroll position. We keep the
//! skeleton form: a plain pixel `scroll_top` (no anchors), vertical-only, no
//! syntax runs, only the primary selection. The cursor offset comes straight from
//! the engine (`buffer.primary_resolved()`); `(row, col)` is derived by counting
//! `\n`s, so no `rope`/`text` types leak into the frontend.

use alteria_core::selection::Selection;
use gpui::{
    fill, point, px, relative, rgba, size, App, Bounds, ContentMask, Element, ElementId, Entity,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextAlign, TextRun, Window,
};

use crate::view::{scroll, EditorView};

/// Translucent fill behind a selected span.
const SELECTION_COLOR: u32 = 0x3b82f64d;
/// The thin primary-cursor bar.
const CURSOR_COLOR: u32 = 0x2f81f7ff;
/// Cursor bar width.
const CURSOR_WIDTH: f32 = 2.0;

/// Paints the buffer for an [`EditorView`]. Reads engine state read-only through
/// the public facade; never mutates engine state (it only caches the painted
/// bounds back onto the view for autoscroll).
pub(crate) struct TextElement {
    pub(crate) view: Entity<EditorView>,
}

/// Everything `prepaint` shapes/positions for `paint` to draw. `lines` holds only
/// the visible rows; `first_row` is the absolute buffer row of `lines[0]`.
pub(crate) struct PrepaintState {
    first_row: usize,
    lines: Vec<ShapedLine>,
    line_height: Pixels,
    scroll_top: Pixels,
    cursor: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // The element fills the viewport and owns scrolling itself (it paints only
        // the visible rows), mirroring Zed's `EditorElement` in Full mode, which
        // sizes to `relative(1.)` rather than the whole document height.
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let view = self.view.read(cx);
        let text = view.editor.buffer.text();
        let primary = view.editor.buffer.primary_resolved();
        let scroll_top = view.scroll_top;

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        // The visible row window (Zed shapes only the visible display rows).
        let line_count = text.split('\n').count();
        let first_row = scroll::first_visible_row(f32::from(scroll_top), f32::from(line_height));
        let visible_count =
            scroll::visible_row_count(f32::from(bounds.size.height), f32::from(line_height));
        let last_row = (first_row + visible_count).min(line_count);

        // Shape just those lines, each independently.
        let mut lines = Vec::new();
        for line in text
            .split('\n')
            .skip(first_row)
            .take(last_row.saturating_sub(first_row))
        {
            let runs = if line.is_empty() {
                Vec::new()
            } else {
                vec![TextRun {
                    len: line.len(),
                    font: style.font(),
                    color: style.color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }]
            };
            let shaped = window.text_system().shape_line(
                SharedString::from(line.to_string()),
                font_size,
                &runs,
                None,
            );
            lines.push(shaped);
        }

        let cursor = caret_quad(
            &lines,
            first_row,
            &text,
            &primary,
            bounds,
            line_height,
            scroll_top,
        );
        let selections = selection_quads(
            &lines,
            first_row,
            &text,
            &primary,
            bounds,
            line_height,
            scroll_top,
        );

        PrepaintState {
            first_row,
            lines,
            line_height,
            scroll_top,
            cursor,
            selections,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // The caret only shows while we hold focus (Zed gates the same way).
        let focused = self.view.read(cx).focus_handle.is_focused(window);
        let line_height = prepaint.line_height;
        let scroll_top = prepaint.scroll_top;
        let first_row = prepaint.first_row;

        // Clip every draw to the element bounds so a partially-scrolled top or
        // bottom line cannot bleed outside the viewport — Zed masks the text
        // region the same way in `editor/src/element.rs`.
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            // Selection fills sit behind the glyphs.
            for quad in prepaint.selections.drain(..) {
                window.paint_quad(quad);
            }
            for (i, line) in prepaint.lines.iter().enumerate() {
                let row = first_row + i;
                let y = bounds.origin.y + line_height * row as f32 - scroll_top;
                // A paint failure on one line should not panic the editor; skip it.
                line.paint(
                    point(bounds.origin.x, y),
                    line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .ok();
            }
            if focused {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
        });

        // Cache the painted viewport so the view can autoscroll the cursor into
        // view next frame (the `last_bounds` pattern from gpui's examples/input.rs).
        self.view.update(cx, |view, _cx| {
            view.last_bounds = Some(bounds);
        });
    }
}

/// Build the primary-cursor bar at the head offset, offset by the scroll. Returns
/// `None` when the cursor's row is outside the visible window (off-screen).
fn caret_quad(
    lines: &[ShapedLine],
    first_row: usize,
    text: &str,
    primary: &Selection<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    scroll_top: Pixels,
) -> Option<PaintQuad> {
    let (row, col) = row_col(text, primary.head());
    let line = lines.get(row.checked_sub(first_row)?)?;
    let x = bounds.left() + line.x_for_index(col);
    let y = bounds.top() + line_height * row as f32 - scroll_top;
    Some(fill(
        Bounds::new(point(x, y), size(px(CURSOR_WIDTH), line_height)),
        rgba(CURSOR_COLOR),
    ))
}

/// Build one translucent quad per *visible* row the primary selection covers.
/// Spans that cross lines get a rectangle per line (first line from its start
/// column to its end, interior lines full, last line up to its end column); rows
/// scrolled out of the visible window are skipped, and each quad is offset by the
/// scroll.
fn selection_quads(
    lines: &[ShapedLine],
    first_row: usize,
    text: &str,
    primary: &Selection<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    scroll_top: Pixels,
) -> Vec<PaintQuad> {
    let (min, max) = (primary.min(), primary.max());
    if min == max {
        return Vec::new();
    }
    let (r0, c0) = row_col(text, min);
    let (r1, c1) = row_col(text, max);
    let mut quads = Vec::new();
    for row in r0..=r1 {
        // Skip rows above or below the visible window.
        let Some(idx) = row.checked_sub(first_row) else {
            continue;
        };
        let Some(line) = lines.get(idx) else { continue };
        let start_col = if row == r0 { c0 } else { 0 };
        // Interior/first lines extend to their full shaped text width.
        let end_col = if row == r1 { c1 } else { line.text.len() };
        let x0 = bounds.left() + line.x_for_index(start_col);
        let x1 = bounds.left() + line.x_for_index(end_col);
        let y = bounds.top() + line_height * row as f32 - scroll_top;
        quads.push(fill(
            Bounds::from_corners(point(x0, y), point(x1, y + line_height)),
            rgba(SELECTION_COLOR),
        ));
    }
    quads
}

/// Byte offset → `(row, byte-column)` by counting `\n`s before `offset`. The
/// rope already normalized line endings to `\n` on load, so this matches how the
/// engine counts lines without pulling in any `rope`/`text` type. `pub(crate)` so
/// the view can derive the primary cursor's row for autoscroll.
pub(crate) fn row_col(text: &str, offset: usize) -> (usize, usize) {
    let mut row = 0;
    let mut line_start = 0;
    for (i, b) in text.bytes().enumerate() {
        if i >= offset {
            break;
        }
        if b == b'\n' {
            row += 1;
            line_start = i + 1;
        }
    }
    (row, offset - line_start)
}

/// Convert a viewport point to a byte offset in `text`, shaping the target line
/// and asking GPUI for the nearest character boundary. This is the single-buffer
/// subset of Zed's `PositionMap::point_for_position`.
pub(crate) fn offset_for_point(
    text: &str,
    position: Point<Pixels>,
    bounds: Bounds<Pixels>,
    scroll_top: Pixels,
    line_height: Pixels,
    window: &mut Window,
) -> usize {
    let line_count = text.split('\n').count();
    let row = row_for_y(
        f32::from(position.y),
        f32::from(bounds.top()),
        f32::from(scroll_top),
        f32::from(line_height),
        line_count,
    );
    let (line_start, line) = line_at_row(text, row);
    let x = (position.x - bounds.left()).max(px(0.));
    let col = if line.is_empty() {
        0
    } else {
        shape_plain_line(line, window).closest_index_for_x(x)
    };
    line_start + col.min(line.len())
}

fn shape_plain_line(line: &str, window: &mut Window) -> ShapedLine {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let runs = vec![TextRun {
        len: line.len(),
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    window
        .text_system()
        .shape_line(SharedString::from(line.to_string()), font_size, &runs, None)
}

fn row_for_y(
    position_y: f32,
    bounds_top: f32,
    scroll_top: f32,
    line_height: f32,
    line_count: usize,
) -> usize {
    if line_height <= 0.0 || line_count == 0 {
        return 0;
    }
    let y = (position_y - bounds_top + scroll_top).max(0.0);
    ((y / line_height).floor() as usize).min(line_count - 1)
}

fn line_at_row(text: &str, target_row: usize) -> (usize, &str) {
    let mut row = 0;
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            if row == target_row {
                return (start, &text[start..i]);
            }
            row += 1;
            start = i + 1;
        }
    }
    (start, &text[start..])
}

#[cfg(test)]
mod tests {
    use super::{line_at_row, row_col, row_for_y};

    #[test]
    fn row_col_at_start() {
        assert_eq!(row_col("abc", 0), (0, 0));
    }

    #[test]
    fn row_col_within_first_line() {
        assert_eq!(row_col("abc", 2), (0, 2));
    }

    #[test]
    fn row_col_after_a_newline() {
        // "ab\ncd": offset 4 -> row 1, col 1 (the 'd').
        assert_eq!(row_col("ab\ncd", 4), (1, 1));
    }

    #[test]
    fn row_col_on_the_trailing_empty_line() {
        // "a\n": offset 2 (end) -> the phantom empty line at row 1, col 0.
        assert_eq!(row_col("a\n", 2), (1, 0));
    }

    #[test]
    fn row_col_at_line_end_before_break() {
        // "ab\ncd": offset 2 is the '\n' position -> still row 0, col 2.
        assert_eq!(row_col("ab\ncd", 2), (0, 2));
    }

    #[test]
    fn line_at_row_returns_start_offset_and_line_text() {
        assert_eq!(line_at_row("ab\ncd\nef", 0), (0, "ab"));
        assert_eq!(line_at_row("ab\ncd\nef", 1), (3, "cd"));
        assert_eq!(line_at_row("ab\ncd\nef", 2), (6, "ef"));
        assert_eq!(line_at_row("ab\n", 1), (3, ""));
    }

    #[test]
    fn row_for_y_accounts_for_scroll_and_clamps() {
        assert_eq!(row_for_y(10.0, 0.0, 0.0, 20.0, 3), 0);
        assert_eq!(row_for_y(10.0, 0.0, 40.0, 20.0, 3), 2);
        assert_eq!(row_for_y(999.0, 0.0, 0.0, 20.0, 3), 2);
        assert_eq!(row_for_y(-20.0, 0.0, 0.0, 20.0, 3), 0);
    }
}
