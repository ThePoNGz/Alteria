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
//! Internally everything is in **char** coordinates; the entry point converts
//! to/from the byte offsets that [`Range`] stores.

use ropey::Rope;

use crate::action::Expansion;
use crate::selection::Range;

/// Expand `range` one level according to `kind`. Returns `range` unchanged when
/// already at the outermost level (the executor treats that as a no-op).
pub fn expand(text: &Rope, range: Range, kind: Expansion) -> Range {
    let lo = text.byte_to_char(range.min());
    let hi = text.byte_to_char(range.max());
    let (nlo, nhi) = match kind {
        Expansion::Enclosing => enclosing(text, lo, hi, false),
        Expansion::EnclosingLeft => enclosing(text, lo, hi, true),
        Expansion::BracketContent => bracket_content(text, lo, hi),
        Expansion::BracketAlternating => bracket_alternating(text, lo, hi),
    };
    Range {
        anchor: text.char_to_byte(nlo),
        head: text.char_to_byte(nhi),
    }
}

/// A word character (alphanumeric or `_`).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
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

fn char_at(text: &Rope, i: usize) -> Option<char> {
    text.get_char(i)
}

/// The maximal word run touching the selection's left edge (the char at `lo`,
/// or the char just before it), in char coordinates.
fn word_span(text: &Rope, lo: usize) -> Option<(usize, usize)> {
    let len = text.len_chars();
    let at_word = |i: usize| char_at(text, i).map(is_word).unwrap_or(false);
    let center = if at_word(lo) {
        lo
    } else if lo > 0 && at_word(lo - 1) {
        lo - 1
    } else {
        return None;
    };
    let mut ws = center;
    while ws > 0 && at_word(ws - 1) {
        ws -= 1;
    }
    let mut we = center + 1;
    while we < len && at_word(we) {
        we += 1;
    }
    Some((ws, we))
}

/// Nearest opening bracket that encloses position `from` (scanning left).
fn enclosing_open(text: &Rope, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    while i > 0 {
        i -= 1;
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
    let len = text.len_chars();
    let mut depth = 0i32;
    let mut i = open;
    while i < len {
        let c = char_at(text, i)?;
        if c == open_c {
            depth += 1;
        } else if c == close_c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The end of the next word to the right of `from`, skipping non-word /
/// non-bracket chars. `None` if a bracket is hit first or there is no word.
fn next_word_end(text: &Rope, from: usize) -> Option<usize> {
    let len = text.len_chars();
    let mut i = from;
    while i < len {
        let c = char_at(text, i)?;
        if is_bracket(c) {
            return None;
        }
        if is_word(c) {
            let mut e = i;
            while e < len && char_at(text, e).map(is_word).unwrap_or(false) {
                e += 1;
            }
            return Some(e);
        }
        i += 1;
    }
    None
}

/// The start of the previous word to the left of `from`, skipping non-word /
/// non-bracket chars. `None` if a bracket is hit first or there is no word.
fn prev_word_start(text: &Rope, from: usize) -> Option<usize> {
    let mut i = from;
    while i > 0 {
        let c = char_at(text, i - 1)?;
        if is_bracket(c) {
            return None;
        }
        if is_word(c) {
            let mut s = i - 1;
            while s > 0 && char_at(text, s - 1).map(is_word).unwrap_or(false) {
                s -= 1;
            }
            return Some(s);
        }
        i -= 1;
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
            return (op, cl + 1);
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
        if op + 1 < lo || cl > hi {
            return (op + 1, cl); // strictly larger content
        }
        from = op; // content equals current selection: climb to the parent
    }
}

/// `P`: alternate content → that pair with delimiters → parent content → …
fn bracket_alternating(text: &Rope, lo: usize, hi: usize) -> (usize, usize) {
    // Case A: the selection is a full pair `[lo, hi)` (delimiters included).
    if hi > lo
        && char_at(text, lo).and_then(close_of).is_some()
        && matching_close(text, lo) == Some(hi - 1)
    {
        if let Some(pop) = enclosing_open(text, lo) {
            if let Some(pcl) = matching_close(text, pop) {
                // The parent's content; if it coincides with the current pair,
                // skip straight to the parent pair (with delimiters).
                if (pop + 1, pcl) != (lo, hi) {
                    return (pop + 1, pcl);
                }
                return (pop, pcl + 1);
            }
        }
        return (lo, hi); // outermost pair: no-op
    }
    // Case B: the selection is exactly a pair's content -> add its delimiters.
    if lo > 0
        && char_at(text, lo - 1).and_then(close_of).is_some()
        && matching_close(text, lo - 1) == Some(hi)
    {
        return (lo - 1, hi + 1);
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
        let r = expand(&Rope::from_str(text), range, kind);
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
