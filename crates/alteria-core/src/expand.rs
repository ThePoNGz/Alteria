//! Selection expansion: `I` / `U` / `O` / `P` (see `KEYMAP.md`).
//!
//! Pure functions over the rope and a [`Range`]; each returns the next expanded
//! range one level out. M0 implements the **delimiter-matched** levels only —
//! words and the bracket pairs `()` `[]` `{}` (nesting-aware, matched across
//! lines). Deeper syntactic levels (string contents, quote pairing, …) are
//! deferred to tree-sitter, as `KEYMAP.md` notes.
//!
//! - `I` ([`Expansion::Enclosing`]): word, then grow by one word toward the
//!   **end**, then the enclosing bracket pair (with delimiters), climbing out.
//! - `U` ([`Expansion::EnclosingLeft`]): like `I` but grows toward the **start**
//!   (`a aa b`, cursor in `aa`: `I` → `aa` → `aa b`; `U` → `aa` → `a aa`).
//! - `O` ([`Expansion::BracketContent`]): the content inside the nearest pair,
//!   then the parent pair's content, … — never the delimiters.
//! - `P` ([`Expansion::BracketAlternating`]): content, then that pair with its
//!   delimiters, then the parent's content, then the parent pair, … out.
//!
//! Everything is in **byte** offsets, matching the byte-indexed [`rope::Rope`]
//! and the byte offsets a [`Range`] stores — no char-index bridge. Positions
//! step by char boundaries via [`next_boundary`]/[`prev_boundary`].

use rope::Rope;

use crate::action::Expansion;
use crate::char_kind::{char_kind, CharKind};
use crate::selection::Range;

/// Expand `range` one level according to `kind`. Returns `range` unchanged when
/// already at the outermost level (the executor treats that as a no-op).
pub fn expand(text: &Rope, range: Range, kind: Expansion) -> Range {
    let lo = range.min();
    let hi = range.max();
    let (nlo, nhi) = match kind {
        Expansion::Enclosing => enclosing(text, lo, hi, false),
        Expansion::EnclosingLeft => enclosing(text, lo, hi, true),
        Expansion::BracketContent => bracket_content(text, lo, hi),
        Expansion::BracketAlternating => bracket_alternating(text, lo, hi),
    };
    Range {
        anchor: nlo,
        head: nhi,
    }
}

/// A word character — routed through the engine's shared [`char_kind`]
/// classifier so expansion and motion agree on word semantics. (Expansion keeps
/// its own bracket-aware stepping; only the word-char *definition* is unified.)
fn is_word(c: char) -> bool {
    char_kind(c) == CharKind::Word
}

/// If `c` is an opening bracket, its matching close; else `None`.
fn close_of(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        _ => None,
    }
}

fn is_bracket(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}')
}

/// The char starting at byte offset `i`, or `None` at/after the end.
fn char_at(text: &Rope, i: usize) -> Option<char> {
    text.chars_at(i).next()
}

/// The next char boundary after byte offset `i` (clamped to the buffer end).
fn next_boundary(text: &Rope, i: usize) -> usize {
    match char_at(text, i) {
        Some(c) => i + c.len_utf8(),
        None => i,
    }
}

/// The previous char boundary before byte offset `i` (clamped to 0).
fn prev_boundary(text: &Rope, i: usize) -> usize {
    match text.reversed_chars_at(i).next() {
        Some(c) => i - c.len_utf8(),
        None => i,
    }
}

/// The maximal word run touching the selection's left edge (the char at `lo`,
/// or the char just before it), in byte offsets.
fn word_span(text: &Rope, lo: usize) -> Option<(usize, usize)> {
    let at_word = |i: usize| char_at(text, i).map(is_word).unwrap_or(false);
    let center = if at_word(lo) {
        lo
    } else if lo > 0 && at_word(prev_boundary(text, lo)) {
        prev_boundary(text, lo)
    } else {
        return None;
    };
    let mut ws = center;
    while ws > 0 && at_word(prev_boundary(text, ws)) {
        ws = prev_boundary(text, ws);
    }
    let mut we = next_boundary(text, center);
    while we < text.len() && at_word(we) {
        we = next_boundary(text, we);
    }
    Some((ws, we))
}

/// Nearest opening bracket that encloses position `from` (scanning left).
fn enclosing_open(text: &Rope, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    while i > 0 {
        i = prev_boundary(text, i);
        let c = char_at(text, i)?;
        if c == ')' || c == ']' || c == '}' {
            depth += 1;
        } else if close_of(c).is_some() {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

/// The matching close bracket for the opening bracket at `open` (nesting-aware).
fn matching_close(text: &Rope, open: usize) -> Option<usize> {
    let open_c = char_at(text, open)?;
    let close_c = close_of(open_c)?;
    let mut depth = 0i32;
    let mut i = open;
    while i < text.len() {
        let c = char_at(text, i)?;
        if c == open_c {
            depth += 1;
        } else if c == close_c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i = next_boundary(text, i);
    }
    None
}

/// The end of the next word to the right of `from`, skipping non-word /
/// non-bracket chars. `None` if a bracket is hit first or there is no word.
fn next_word_end(text: &Rope, from: usize) -> Option<usize> {
    let mut i = from;
    while i < text.len() {
        let c = char_at(text, i)?;
        if is_bracket(c) {
            return None;
        }
        if is_word(c) {
            let mut e = i;
            while e < text.len() && char_at(text, e).map(is_word).unwrap_or(false) {
                e = next_boundary(text, e);
            }
            return Some(e);
        }
        i = next_boundary(text, i);
    }
    None
}

/// The start of the previous word to the left of `from`, skipping non-word /
/// non-bracket chars. `None` if a bracket is hit first or there is no word.
fn prev_word_start(text: &Rope, from: usize) -> Option<usize> {
    let mut i = from;
    while i > 0 {
        let prev = prev_boundary(text, i);
        let c = char_at(text, prev)?;
        if is_bracket(c) {
            return None;
        }
        if is_word(c) {
            let mut s = prev;
            while s > 0
                && char_at(text, prev_boundary(text, s))
                    .map(is_word)
                    .unwrap_or(false)
            {
                s = prev_boundary(text, s);
            }
            return Some(s);
        }
        i = prev;
    }
    None
}

/// `I` / `U`: word, then grow by a word in the bias direction (else the other),
/// then the enclosing bracket pair including delimiters.
fn enclosing(text: &Rope, lo: usize, hi: usize, bias_left: bool) -> (usize, usize) {
    // 1. If the selection sits inside a word but isn't the whole word, take it.
    if let Some((ws, we)) = word_span(text, lo) {
        if (ws, we) != (lo, hi) && ws <= lo && hi <= we {
            return (ws, we);
        }
    }
    // 2. Grow by a word, preferring the bias direction.
    let grow_left = || prev_word_start(text, lo).map(|s| (s, hi));
    let grow_right = || next_word_end(text, hi).map(|e| (lo, e));
    let primary = if bias_left { grow_left() } else { grow_right() };
    if let Some(r) = primary {
        return r;
    }
    let secondary = if bias_left { grow_right() } else { grow_left() };
    if let Some(r) = secondary {
        return r;
    }
    // 3. Both directions blocked: the enclosing bracket pair (with delimiters).
    if let Some(op) = enclosing_open(text, lo) {
        if let Some(cl) = matching_close(text, op) {
            return (op, next_boundary(text, cl));
        }
    }
    (lo, hi) // outermost: no-op
}

/// `O`: the content of the smallest bracket pair that strictly contains
/// `[lo, hi)`, climbing to the parent pair's content on repeat.
fn bracket_content(text: &Rope, lo: usize, hi: usize) -> (usize, usize) {
    let mut from = lo;
    loop {
        let Some(op) = enclosing_open(text, from) else {
            return (lo, hi); // no enclosing pair: no-op
        };
        let Some(cl) = matching_close(text, op) else {
            return (lo, hi);
        };
        let content_start = next_boundary(text, op);
        if content_start < lo || cl > hi {
            return (content_start, cl); // strictly larger content
        }
        from = op; // content equals current selection: climb to the parent
    }
}

/// `P`: alternate content → that pair with delimiters → parent content → …
fn bracket_alternating(text: &Rope, lo: usize, hi: usize) -> (usize, usize) {
    // Case A: the selection is a full pair `[lo, hi)` (delimiters included).
    if hi > lo
        && char_at(text, lo).and_then(close_of).is_some()
        && matching_close(text, lo) == Some(prev_boundary(text, hi))
    {
        if let Some(pop) = enclosing_open(text, lo) {
            if let Some(pcl) = matching_close(text, pop) {
                let parent_content_start = next_boundary(text, pop);
                // The parent's content; if it coincides with the current pair,
                // skip straight to the parent pair (with delimiters).
                if (parent_content_start, pcl) != (lo, hi) {
                    return (parent_content_start, pcl);
                }
                return (pop, next_boundary(text, pcl));
            }
        }
        return (lo, hi); // outermost pair: no-op
    }
    // Case B: the selection is exactly a pair's content -> add its delimiters.
    if lo > 0
        && char_at(text, prev_boundary(text, lo))
            .and_then(close_of)
            .is_some()
        && matching_close(text, prev_boundary(text, lo)) == Some(hi)
    {
        return (prev_boundary(text, lo), next_boundary(text, hi));
    }
    // Case C: cursor / other -> the nearest pair's content.
    bracket_content(text, lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Expansion::*;

    fn rng(anchor: usize, head: usize) -> Range {
        Range { anchor, head }
    }
    /// Expand once and return the resulting span as `(min, max)` byte offsets.
    fn ex(text: &str, range: Range, kind: Expansion) -> (usize, usize) {
        let r = expand(&Rope::from(text), range, kind);
        (r.min(), r.max())
    }

    #[test]
    fn i_selects_word_then_grows_right() {
        let t = "a aa b"; // cursor in "aa" (byte 2)
        assert_eq!(ex(t, Range::cursor(2), Enclosing), (2, 4)); // "aa"
        assert_eq!(ex(t, rng(2, 4), Enclosing), (2, 6)); // "aa b"
    }

    #[test]
    fn u_selects_word_then_grows_left() {
        let t = "a aa b";
        assert_eq!(ex(t, Range::cursor(2), EnclosingLeft), (2, 4)); // "aa"
        assert_eq!(ex(t, rng(2, 4), EnclosingLeft), (0, 4)); // "a aa"
    }

    #[test]
    fn i_climbs_into_enclosing_bracket_pair() {
        let t = "(a aa b)";
        // content fully selected -> the pair including its delimiters
        assert_eq!(ex(t, rng(1, 7), Enclosing), (0, 8));
    }

    #[test]
    fn i_word_then_noop_in_plain_text() {
        let t = "abc";
        assert_eq!(ex(t, Range::cursor(1), Enclosing), (0, 3)); // word
        assert_eq!(ex(t, rng(0, 3), Enclosing), (0, 3)); // outermost: no-op
    }

    #[test]
    fn i_climbs_through_nested_brackets() {
        let t = "[(x)]";
        assert_eq!(ex(t, Range::cursor(2), Enclosing), (2, 3)); // word "x"
        assert_eq!(ex(t, rng(2, 3), Enclosing), (1, 4)); // inner pair "(x)"
        assert_eq!(ex(t, rng(1, 4), Enclosing), (0, 5)); // outer pair "[(x)]"
    }

    #[test]
    fn o_climbs_bracket_content() {
        let t = "((x))";
        assert_eq!(ex(t, Range::cursor(2), BracketContent), (2, 3)); // "x"
        assert_eq!(ex(t, rng(2, 3), BracketContent), (1, 4)); // "(x)"
        assert_eq!(ex(t, rng(1, 4), BracketContent), (1, 4)); // outermost: no-op
    }

    #[test]
    fn o_matches_across_lines() {
        let t = "(\nx\n)"; // ( \n x \n ) -> bytes 0..5
        assert_eq!(ex(t, Range::cursor(2), BracketContent), (1, 4)); // "\nx\n"
    }

    #[test]
    fn p_alternates_content_then_pair() {
        let t = "(a)";
        assert_eq!(ex(t, Range::cursor(1), BracketAlternating), (1, 2)); // "a"
        assert_eq!(ex(t, rng(1, 2), BracketAlternating), (0, 3)); // "(a)"
        assert_eq!(ex(t, rng(0, 3), BracketAlternating), (0, 3)); // no-op
    }

    #[test]
    fn p_nested_skips_the_coinciding_level() {
        let t = "((x))";
        assert_eq!(ex(t, Range::cursor(2), BracketAlternating), (2, 3)); // "x"
        assert_eq!(ex(t, rng(2, 3), BracketAlternating), (1, 4)); // "(x)" pair
        assert_eq!(ex(t, rng(1, 4), BracketAlternating), (0, 5)); // "((x))" pair
        assert_eq!(ex(t, rng(0, 5), BracketAlternating), (0, 5)); // no-op
    }
}
