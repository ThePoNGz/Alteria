//! The buffer renderer: a custom GPUI [`Element`] that shapes and paints every
//! line of the buffer plus the primary cursor and selection.
//!
//! This mirrors GPUI's own `examples/input.rs` `TextElement`
//! (`request_layout`/`prepaint`/`paint`, `text_system().shape_line(...)`,
//! `line.x_for_index(..)`, `fill(..)` cursor quad), extended from one line to
//! many — which is exactly how Zed's production `editor/src/element.rs`
//! (`EditorElement::layout_lines`/`paint_cursors`) lays out and positions text:
//! shape per line, paint at `y = row * line_height`, place the caret with
//! `x_for_index`. We deliberately keep the skeleton form: no scrolling/viewport,
//! no syntax runs, only the primary selection. The cursor offset comes straight
//! from the engine (`buffer.primary_resolved()`); `(row, col)` is derived by
//! counting `\n`s, so no `rope`/`text` types leak into the frontend.

use alteria_core::selection::Selection;
use gpui::{
    fill, point, px, relative, rgba, size, App, Bounds, Element, ElementId, Entity,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, PaintQuad, Pixels, ShapedLine,
    SharedString, Style, TextAlign, TextRun, Window,
};

use crate::view::EditorView;

/// Translucent fill behind a selected span.
const SELECTION_COLOR: u32 = 0x3b82f64d;
/// The thin primary-cursor bar.
const CURSOR_COLOR: u32 = 0x2f81f7ff;
/// Cursor bar width.
const CURSOR_WIDTH: f32 = 2.0;

/// Paints the buffer for an [`EditorView`]. Reads engine state read-only through
/// the public facade; never mutates.
pub(crate) struct TextElement {
    pub(crate) view: Entity<EditorView>,
}

/// Everything `prepaint` shapes/positions for `paint` to draw.
pub(crate) struct PrepaintState {
    lines: Vec<ShapedLine>,
    line_height: Pixels,
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
        // Reserve full width and exactly enough height for every line — there is
        // no scrolling yet, so the element is as tall as the document.
        let line_count = self.view.read(cx).editor.buffer.text().split('\n').count();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * line_count.max(1) as f32).into();
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
        let text = self.view.read(cx).editor.buffer.text();
        let primary = self.view.read(cx).editor.buffer.primary_resolved();

        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();

        // Shape every line independently (Zed shapes per display line).
        let mut lines = Vec::new();
        for line in text.split('\n') {
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

        let cursor = caret_quad(&lines, &text, &primary, bounds, line_height);
        let selections = selection_quads(&lines, &text, &primary, bounds, line_height);

        PrepaintState {
            lines,
            line_height,
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
        // Selection fills sit behind the glyphs.
        for quad in prepaint.selections.drain(..) {
            window.paint_quad(quad);
        }
        let line_height = prepaint.line_height;
        for (row, line) in prepaint.lines.iter().enumerate() {
            let origin = point(bounds.origin.x, bounds.origin.y + line_height * row as f32);
            // A paint failure on one line should not panic the editor; skip it.
            line.paint(origin, line_height, TextAlign::Left, None, window, cx)
                .ok();
        }
        // The caret only shows while we hold focus (Zed gates the same way).
        let focused = self.view.read(cx).focus_handle.is_focused(window);
        if focused {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }
    }
}

/// Build the primary-cursor bar at the head offset.
fn caret_quad(
    lines: &[ShapedLine],
    text: &str,
    primary: &Selection<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Option<PaintQuad> {
    let (row, col) = row_col(text, primary.head());
    let line = lines.get(row)?;
    let x = bounds.left() + line.x_for_index(col);
    let y = bounds.top() + line_height * row as f32;
    Some(fill(
        Bounds::new(point(x, y), size(px(CURSOR_WIDTH), line_height)),
        rgba(CURSOR_COLOR),
    ))
}

/// Build one translucent quad per row the primary selection covers. Spans that
/// cross lines get a rectangle per line (first line from its start column to its
/// end, interior lines full, last line up to its end column).
fn selection_quads(
    lines: &[ShapedLine],
    text: &str,
    primary: &Selection<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<PaintQuad> {
    let (min, max) = (primary.min(), primary.max());
    if min == max {
        return Vec::new();
    }
    let (r0, c0) = row_col(text, min);
    let (r1, c1) = row_col(text, max);
    let mut quads = Vec::new();
    for row in r0..=r1 {
        let Some(line) = lines.get(row) else { continue };
        let start_col = if row == r0 { c0 } else { 0 };
        // Interior/first lines extend to their full shaped text width.
        let end_col = if row == r1 { c1 } else { line.text.len() };
        let x0 = bounds.left() + line.x_for_index(start_col);
        let x1 = bounds.left() + line.x_for_index(end_col);
        let y = bounds.top() + line_height * row as f32;
        quads.push(fill(
            Bounds::from_corners(point(x0, y), point(x1, y + line_height)),
            rgba(SELECTION_COLOR),
        ));
    }
    quads
}

/// Byte offset → `(row, byte-column)` by counting `\n`s before `offset`. The
/// rope already normalized line endings to `\n` on load, so this matches how the
/// engine counts lines without pulling in any `rope`/`text` type.
fn row_col(text: &str, offset: usize) -> (usize, usize) {
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

#[cfg(test)]
mod tests {
    use super::row_col;

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
}
