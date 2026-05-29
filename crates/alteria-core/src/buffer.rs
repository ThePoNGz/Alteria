//! The text buffer: a ropey [`Rope`] plus the current [`Selection`].
//!
//! `Buffer` is plain data — all editing logic lives in the `executor`. ropey's
//! `Rope` clones cheaply (structure-shared via `Arc`), so snapshotting a buffer
//! for history is inexpensive.

use crate::selection::Selection;
use ropey::Rope;

/// A document and where the cursor(s) currently are.
#[derive(Clone, Debug)]
pub struct Buffer {
    pub text: Rope,
    pub selection: Selection,
}

impl Buffer {
    /// Build a buffer from a string, with a single bare cursor at offset 0.
    ///
    /// Named to mirror `ropey::Rope::from_str`; it is infallible, so it does
    /// not (and cannot) implement the `Result`-returning `FromStr` trait.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Buffer {
            text: Rope::from_str(s),
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
        assert_eq!(buf.text.len_bytes(), 3);
        assert_eq!(buf.text, "abc");
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
        assert_eq!(buf.text.len_bytes(), 0);
        assert_eq!(buf.selection.primary(), crate::selection::Range::cursor(0));
    }
}
