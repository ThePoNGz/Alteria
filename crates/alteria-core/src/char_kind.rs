//! The three-class character model and the generic word-boundary scanners.
//!
//! Both are ports from Zed, kept as pure functions over the rope so motion and
//! expansion can share one definition of "what a word is":
//!
//! - [`CharKind`] mirrors Zed's `CharKind` (`language/src/buffer.rs`), reduced
//!   to the scope-less three classes — the `LanguageScope`/tree-sitter branch
//!   that lets a grammar treat e.g. `-` as a word char is deferred until syntax
//!   lands, so here `-` is [`CharKind::Punctuation`].
//! - [`find_boundary`] / [`find_preceding_boundary`] mirror Zed's
//!   `find_boundary_point` / `find_preceding_boundary_point`
//!   (`editor/src/movement.rs`), walking the rope's `chars_at` /
//!   `reversed_chars_at` by byte offset (multi-line, no display clipping).

use rope::Rope;

/// A character's lexical class, used to locate word boundaries. A word char is
/// alphanumeric or `_`; whitespace is `char::is_whitespace`; everything else is
/// punctuation. Mirrors Zed's three-class `CharKind`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CharKind {
    Whitespace,
    Punctuation,
    Word,
}

/// Classify `c`. Scope-less mirror of Zed `CharClassifier::kind_with` (no
/// language word-character set, so punctuation like `-` stays punctuation).
pub fn char_kind(c: char) -> CharKind {
    if c.is_alphanumeric() || c == '_' {
        CharKind::Word
    } else if c.is_whitespace() {
        CharKind::Whitespace
    } else {
        CharKind::Punctuation
    }
}

/// Scan forward from byte offset `from` to the first boundary. `is_boundary` is
/// called for each adjacent pair as `(left, right)` — `left` behind the gap,
/// `right` ahead — and the scan stops at the offset *between* the first pair it
/// accepts. Returns the buffer end if no pair matches. Mirrors Zed
/// `find_boundary_point` (multi-line, byte-offset based).
pub fn find_boundary(
    text: &Rope,
    from: usize,
    mut is_boundary: impl FnMut(char, char) -> bool,
) -> usize {
    let mut offset = from;
    let mut prev: Option<char> = None;
    for ch in text.chars_at(from) {
        if let Some(left) = prev {
            if is_boundary(left, ch) {
                break;
            }
        }
        offset += ch.len_utf8();
        prev = Some(ch);
    }
    offset
}

/// Scan backward from byte offset `from` to the first preceding boundary.
/// `is_boundary` sees each pair in document order as `(left, right)` (`left` is
/// the char further back). Returns `0` if no pair matches. Mirrors Zed
/// `find_preceding_boundary_point`.
pub fn find_preceding_boundary(
    text: &Rope,
    from: usize,
    mut is_boundary: impl FnMut(char, char) -> bool,
) -> usize {
    let mut offset = from;
    let mut prev: Option<char> = None; // the char to the right of the candidate gap
    for ch in text.reversed_chars_at(from) {
        if let Some(right) = prev {
            if is_boundary(ch, right) {
                break;
            }
        }
        offset -= ch.len_utf8();
        prev = Some(ch);
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_three_kinds() {
        for c in ['a', 'Z', '7', '_', 'é'] {
            assert_eq!(char_kind(c), CharKind::Word, "{c:?} should be Word");
        }
        for c in [' ', '\t', '\n', '\r'] {
            assert_eq!(
                char_kind(c),
                CharKind::Whitespace,
                "{c:?} should be Whitespace"
            );
        }
        for c in ['.', '(', ')', '-', '+', ','] {
            assert_eq!(
                char_kind(c),
                CharKind::Punctuation,
                "{c:?} should be Punctuation"
            );
        }
    }

    // A kind-transition predicate, the shared shape both word motions build on.
    fn kind_change(left: char, right: char) -> bool {
        char_kind(left) != char_kind(right)
    }

    #[test]
    fn find_boundary_stops_at_the_first_kind_transition() {
        let t = Rope::from("foo.bar");
        // Word -> Punctuation between "foo" and '.'.
        assert_eq!(find_boundary(&t, 0, kind_change), 3);
    }

    #[test]
    fn find_boundary_runs_to_the_end_with_no_transition() {
        let t = Rope::from("foobar");
        assert_eq!(find_boundary(&t, 0, kind_change), 6);
    }

    #[test]
    fn find_preceding_boundary_stops_at_the_first_kind_transition() {
        let t = Rope::from("foo.bar");
        // Scanning back from the end stops at the start of "bar".
        assert_eq!(find_preceding_boundary(&t, 7, kind_change), 4);
    }

    #[test]
    fn find_preceding_boundary_runs_to_the_start_with_no_transition() {
        let t = Rope::from("foobar");
        assert_eq!(find_preceding_boundary(&t, 6, kind_change), 0);
    }

    #[test]
    fn scanners_handle_buffer_edges_without_panicking() {
        let t = Rope::from("");
        assert_eq!(find_boundary(&t, 0, kind_change), 0);
        assert_eq!(find_preceding_boundary(&t, 0, kind_change), 0);
    }
}
