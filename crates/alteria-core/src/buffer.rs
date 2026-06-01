//! The text buffer: a Zed [`Rope`] plus the current [`Selection`].
//!
//! `Buffer` is plain data — all editing logic lives in the `executor`. Zed's
//! `Rope` (vendored from `zed-industries/zed`) is **byte-indexed** and clones
//! cheaply (structure-shared via `sum_tree`), so snapshotting a buffer for
//! history is inexpensive.

use crate::selection::Selection;
use rope::{Point, Rope};

/// The byte length of line `row`'s content, excluding its trailing line break
/// (`\n`, and a `\r` immediately before it for `\r\n` endings).
///
/// Rope counts lines by `\n` only, so [`Rope::line_len`] already stops before
/// the `\n`; this additionally trims a trailing `\r` so a `\r\n` line behaves
/// like its `\n` counterpart. Returns a **byte** column (rope columns are
/// bytes). Shared by `executor` and `find` so line-edge math is defined once.
pub(crate) fn line_content_len(text: &Rope, row: u32) -> u32 {
    let col = text.line_len(row);
    if col > 0 {
        // Peek the last byte of the line's content: a trailing '\r' is part of a
        // '\r\n' break and must not count toward the visible line length.
        let end = text.point_to_offset(Point::new(row, col));
        if text.reversed_chars_at(end).next() == Some('\r') {
            return col - 1;
        }
    }
    col
}

/// The count of navigable lines, excluding the phantom empty line that a
/// trailing newline produces (`"a\n"` is one navigable line, not two). That
/// phantom line is not somewhere the user can land, so blank-line leaps must
/// not treat it as a target — otherwise the same visible text would behave
/// differently with and without a trailing `\n`.
pub(crate) fn nav_line_count(text: &Rope) -> u32 {
    let rows = text.max_point().row; // last line index; +1 == total lines
    if ends_with_newline(text) {
        rows // the phantom trailing line is dropped
    } else {
        rows + 1
    }
}

/// True when the document ends in a `\n` (and so has a phantom trailing line).
pub(crate) fn ends_with_newline(text: &Rope) -> bool {
    text.reversed_chars_at(text.len()).next() == Some('\n')
}

/// A document and where the cursor(s) currently are.
#[derive(Clone, Debug)]
pub struct Buffer {
    pub text: Rope,
    pub selection: Selection,
}

impl Buffer {
    /// Build a buffer from a string, with a single bare cursor at offset 0.
    ///
    /// Named `from_str` for familiarity; it is infallible, so it does not (and
    /// cannot) implement the `Result`-returning `FromStr` trait. Internally it
    /// constructs the rope via `Rope::from`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Buffer {
            text: Rope::from(s),
            selection: Selection::at(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_holds_the_text() {
        let buf = Buffer::from_str("abc");
        assert_eq!(buf.text.len(), 3);
        assert_eq!(buf.text.to_string(), "abc");
    }

    #[test]
    fn from_str_starts_with_a_bare_cursor_at_zero() {
        let buf = Buffer::from_str("abc");
        assert_eq!(buf.selection.ranges.len(), 1);
        let primary = buf.selection.primary();
        assert!(primary.is_empty());
        assert_eq!(primary.head, 0);
    }

    #[test]
    fn empty_buffer_has_zero_bytes_and_a_cursor_at_zero() {
        let buf = Buffer::from_str("");
        assert_eq!(buf.text.len(), 0);
        assert_eq!(buf.selection.primary(), crate::selection::Range::cursor(0));
    }
}
