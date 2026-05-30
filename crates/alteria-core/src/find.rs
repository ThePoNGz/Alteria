//! Inline char search — the `Alt+F` find sub-mode (see `KEYMAP.md`).
//!
//! A pure helper: from the caret `head`, find the next (or previous) occurrence
//! of a target char **on the current line only**. Returns the byte offset, or
//! `None` for no match — the executor then leaves the caret where it is.

use ropey::Rope;

/// The next (`forward`) or previous occurrence of `ch` on the line containing
/// `head`, strictly past `head`. `None` if there is no match before the line
/// boundary.
pub fn find_on_line(text: &Rope, head: usize, ch: char, forward: bool) -> Option<usize> {
    let head_char = text.byte_to_char(head);
    let line = text.char_to_line(head_char);
    let line_start = text.line_to_char(line);
    let line_end = line_start + visual_line_len_chars(text, line);
    if forward {
        for i in (head_char + 1)..line_end {
            if text.char(i) == ch {
                return Some(text.char_to_byte(i));
            }
        }
    } else {
        for i in (line_start..head_char).rev() {
            if text.char(i) == ch {
                return Some(text.char_to_byte(i));
            }
        }
    }
    None
}

/// Character length of a line excluding its trailing line break.
fn visual_line_len_chars(text: &Rope, line_idx: usize) -> usize {
    let line = text.line(line_idx);
    let mut n = line.len_chars();
    if n > 0 && line.char(n - 1) == '\n' {
        n -= 1;
        if n > 0 && line.char(n - 1) == '\r' {
            n -= 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(text: &str, head: usize, ch: char, forward: bool) -> Option<usize> {
        find_on_line(&Rope::from_str(text), head, ch, forward)
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
