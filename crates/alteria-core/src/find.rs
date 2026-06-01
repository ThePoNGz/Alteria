//! Inline char search — the `Alt+F` find sub-mode (see `KEYMAP.md`).
//!
//! A pure helper: from the caret `head`, find the next (or previous) occurrence
//! of a target char **on the current line only**. All offsets are **bytes** into
//! the [`rope::Rope`]; returns the byte offset of the match, or `None` for no
//! match — the executor then leaves the caret where it is.

use rope::{Point, Rope};

use crate::buffer::line_content_len;

/// The next (`forward`) or previous occurrence of `ch` on the line containing
/// `head`, strictly past `head`. `None` if there is no match before the line
/// boundary.
pub fn find_on_line(text: &Rope, head: usize, ch: char, forward: bool) -> Option<usize> {
    let row = text.offset_to_point(head).row;
    let line_start = text.point_to_offset(Point::new(row, 0));
    let line_end = text.point_to_offset(Point::new(row, line_content_len(text, row)));
    if forward {
        // Scan chars after `head` (exclusive) up to the line's content end,
        // tracking the byte offset of each char.
        let mut offset = head;
        for c in text.chars_at(head) {
            if offset >= line_end {
                break;
            }
            if offset > head && c == ch {
                return Some(offset);
            }
            offset += c.len_utf8();
        }
    } else {
        // Scan chars before `head` (exclusive) down to the line's start.
        let mut offset = head;
        for c in text.reversed_chars_at(head) {
            offset -= c.len_utf8();
            if offset < line_start {
                break;
            }
            if c == ch {
                return Some(offset);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(text: &str, head: usize, ch: char, forward: bool) -> Option<usize> {
        find_on_line(&Rope::from(text), head, ch, forward)
    }

    #[test]
    fn finds_next_occurrence_forward() {
        // a0 b1 c2 a3 b4 c5
        assert_eq!(find("abcabc", 0, 'b', true), Some(1));
        assert_eq!(find("abcabc", 1, 'b', true), Some(4)); // strictly after head
    }

    #[test]
    fn finds_previous_occurrence_backward() {
        assert_eq!(find("abcabc", 5, 'b', false), Some(4));
        assert_eq!(find("abcabc", 4, 'b', false), Some(1));
    }

    #[test]
    fn finds_from_mid_line_both_ways() {
        // a0 x1 b2 x3 c4 ; head on 'b' (byte 2)
        assert_eq!(find("axbxc", 2, 'x', true), Some(3));
        assert_eq!(find("axbxc", 2, 'x', false), Some(1));
    }

    #[test]
    fn no_match_returns_none() {
        assert_eq!(find("abc", 0, 'z', true), None);
        assert_eq!(find("abc", 3, 'z', false), None);
    }

    #[test]
    fn does_not_cross_into_the_next_line() {
        // ab\nxb : a0 b1 \n2 x3 b4 ; from line0, 'x' is on line1
        assert_eq!(find("ab\nxb", 0, 'x', true), None);
    }

    #[test]
    fn searches_within_the_current_line() {
        // line1 starts at byte 3 ("xb")
        assert_eq!(find("ab\nxb", 3, 'b', true), Some(4));
    }
}
