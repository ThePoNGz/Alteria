//! The executor: applies one [`Action`] to the [`Buffer`] (and [`History`]).
//!
//! Edits flow through a [`ChangeSet`] — even a single-char insert — so undo and
//! multicursor fall out of the same machinery. Motions only move the selection
//! and are not recorded in history. All coordinates are byte offsets; motion
//! logic converts to ropey's char indices at the boundary and moves by `char`
//! (M0; grapheme/column-memory refinement is later). Every motion clamps at the
//! buffer edges and never panics.
//!
//! Task 8 operates on the **primary** range only; multicursor generalization is
//! Task 9.

use ropey::Rope;

use crate::action::{Action, Direction, Motion};
use crate::buffer::Buffer;
use crate::history::{History, Transaction};
use crate::selection::{Range, Selection};
use crate::transaction::ChangeSet;

/// Apply one action to the buffer, recording text edits in `history`.
pub fn apply(action: Action, buffer: &mut Buffer, history: &mut History) {
    match action {
        Action::InsertChar(c) => insert_text(buffer, history, &c.to_string()),
        Action::InsertNewline => insert_text(buffer, history, "\n"),
        Action::DeleteBackward => delete_backward(buffer, history),
        Action::CollapseSelection => collapse(buffer),
        Action::Move {
            motion,
            extend,
            count,
        } => move_primary(buffer, motion, extend, count),
        Action::Undo => {
            history.undo(buffer);
        }
        // Implemented in later tasks:
        Action::Expand(_) => {}                                   // Task 10
        Action::FindChar { .. } | Action::FindRepeat { .. } => {} // Task 11
        Action::SpawnCursor(_) => {}                              // Task 9
    }
}

// ---------------------------------------------------------------------------
// Edits — every text change flows through a ChangeSet committed to history.
// ---------------------------------------------------------------------------

/// Insert `s` at the primary head, advance past it, and collapse to a cursor.
fn insert_text(buffer: &mut Buffer, history: &mut History, s: &str) {
    let head = buffer.selection.primary().head;
    apply_edit(
        buffer,
        history,
        &[(head, head, s.to_string())],
        Selection::at(head + s.len()),
    );
}

/// Delete the char before the primary head (no-op at the buffer start).
fn delete_backward(buffer: &mut Buffer, history: &mut History) {
    let head = buffer.selection.primary().head;
    let head_char = buffer.text.byte_to_char(head);
    if head_char == 0 {
        return;
    }
    let from = buffer.text.char_to_byte(head_char - 1);
    apply_edit(
        buffer,
        history,
        &[(from, head, String::new())],
        Selection::at(from),
    );
}

/// Build a changeset for `changes`, record its inverse + selections in history,
/// apply it, and set the new selection.
fn apply_edit(
    buffer: &mut Buffer,
    history: &mut History,
    changes: &[(usize, usize, String)],
    new_selection: Selection,
) {
    let before_text = buffer.text.clone();
    let selection_before = buffer.selection.clone();
    let forward = ChangeSet::from_changes(before_text.len_bytes(), changes);
    let inverse = forward.invert(&before_text);
    if !forward.apply(&mut buffer.text) {
        return; // malformed (should not happen for executor-built changes)
    }
    buffer.selection = new_selection.clone();
    history.commit(Transaction {
        forward,
        inverse,
        selection_before,
        selection_after: new_selection,
    });
}

// ---------------------------------------------------------------------------
// Selection-only actions.
// ---------------------------------------------------------------------------

/// `Esc`: collapse the primary span to a bare cursor at its head.
fn collapse(buffer: &mut Buffer) {
    let head = buffer.selection.primary().head;
    buffer.selection = Selection::at(head);
}

/// Move the primary head `count` times, extending or collapsing.
fn move_primary(buffer: &mut Buffer, motion: Motion, extend: bool, count: usize) {
    let range = buffer.selection.primary();
    let new_head = move_head(&buffer.text, range.head, motion, count);
    let new_range = if extend {
        Range {
            anchor: range.anchor,
            head: new_head,
        }
    } else {
        Range::cursor(new_head)
    };
    buffer.selection = Selection {
        ranges: vec![new_range],
        primary: 0,
    };
}

// ---------------------------------------------------------------------------
// Motions — pure functions over the rope, byte in / byte out, always clamped.
// ---------------------------------------------------------------------------

/// Apply `motion` to a head byte-offset `count` times, stopping early if it
/// stops making progress (clamped at an edge).
pub fn move_head(text: &Rope, mut head: usize, motion: Motion, count: usize) -> usize {
    for _ in 0..count.max(1) {
        let next = step(text, head, motion);
        if next == head {
            break;
        }
        head = next;
    }
    head
}

fn step(text: &Rope, head: usize, motion: Motion) -> usize {
    match motion {
        Motion::Char(Direction::Left) => char_horizontal(text, head, -1),
        Motion::Char(Direction::Right) => char_horizontal(text, head, 1),
        Motion::Char(Direction::Up) => vertical(text, head, true),
        Motion::Char(Direction::Down) => vertical(text, head, false),
        Motion::WordStart(Direction::Right) => word_right(text, head),
        Motion::WordStart(Direction::Left) => word_left(text, head),
        Motion::LineEdge(Direction::Left) => line_start(text, head),
        Motion::LineEdge(Direction::Right) => line_end(text, head),
        Motion::BlankLine(Direction::Up) => blank_line(text, head, true),
        Motion::BlankLine(Direction::Down) => blank_line(text, head, false),
        Motion::MatchingBracket => matching_bracket(text, head).unwrap_or(head),
        // The keymap never produces these direction/motion combinations.
        Motion::WordStart(_) | Motion::LineEdge(_) | Motion::BlankLine(_) => head,
    }
}

/// True when `c` is part of a word (alphanumeric or `_`).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True when line `line_idx` is empty or all whitespace. `line_idx` must be in
/// bounds (`< len_lines`).
fn is_blank(text: &Rope, line_idx: usize) -> bool {
    text.line(line_idx).chars().all(|c| c.is_whitespace())
}

/// Character length of a line excluding its trailing line break (`\n`/`\r\n`).
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

fn char_horizontal(text: &Rope, head: usize, dir: isize) -> usize {
    let hc = text.byte_to_char(head);
    if dir < 0 {
        if hc == 0 {
            head
        } else {
            text.char_to_byte(hc - 1)
        }
    } else if hc >= text.len_chars() {
        head
    } else {
        text.char_to_byte(hc + 1)
    }
}

fn vertical(text: &Rope, head: usize, up: bool) -> usize {
    let hc = text.byte_to_char(head);
    let line = text.char_to_line(hc);
    let target_line = if up {
        if line == 0 {
            return head;
        }
        line - 1
    } else {
        if line + 1 >= text.len_lines() {
            return head;
        }
        line + 1
    };
    let col = hc - text.line_to_char(line);
    let target_len = visual_line_len_chars(text, target_line);
    let target_char = text.line_to_char(target_line) + col.min(target_len);
    text.char_to_byte(target_char)
}

fn word_right(text: &Rope, head: usize) -> usize {
    let len = text.len_chars();
    let mut i = text.byte_to_char(head);
    while i < len && is_word(text.char(i)) {
        i += 1;
    }
    while i < len && !is_word(text.char(i)) {
        i += 1;
    }
    text.char_to_byte(i)
}

fn word_left(text: &Rope, head: usize) -> usize {
    let mut i = text.byte_to_char(head);
    while i > 0 && !is_word(text.char(i - 1)) {
        i -= 1;
    }
    while i > 0 && is_word(text.char(i - 1)) {
        i -= 1;
    }
    text.char_to_byte(i)
}

fn line_start(text: &Rope, head: usize) -> usize {
    let line = text.char_to_line(text.byte_to_char(head));
    text.line_to_byte(line)
}

fn line_end(text: &Rope, head: usize) -> usize {
    let line = text.char_to_line(text.byte_to_char(head));
    let end_char = text.line_to_char(line) + visual_line_len_chars(text, line);
    text.char_to_byte(end_char)
}

fn blank_line(text: &Rope, head: usize, up: bool) -> usize {
    let line = text.char_to_line(text.byte_to_char(head));
    let n = text.len_lines();
    let target = if up {
        let mut j = line;
        while j > 0 && is_blank(text, j) {
            j -= 1;
        }
        while j > 0 && !is_blank(text, j) {
            j -= 1;
        }
        is_blank(text, j).then_some(j)
    } else {
        let mut j = line;
        while j < n && is_blank(text, j) {
            j += 1;
        }
        while j < n && !is_blank(text, j) {
            j += 1;
        }
        (j < n).then_some(j)
    };
    match target {
        Some(j) => text.line_to_byte(j),
        None => head,
    }
}

/// Jump from a bracket at `head` to its partner (nesting-aware, across lines).
/// Returns `None` when `head` is not on a bracket character.
fn matching_bracket(text: &Rope, head: usize) -> Option<usize> {
    let hc = text.byte_to_char(head);
    let len = text.len_chars();
    let here = text.get_char(hc)?;
    let (open, close, forward) = match here {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        _ => return None,
    };
    let mut depth = 0i32;
    if forward {
        let mut i = hc;
        while i < len {
            let ch = text.char(i);
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Some(text.char_to_byte(i));
                }
            }
            i += 1;
        }
    } else {
        let mut i = hc;
        loop {
            let ch = text.char(i);
            if ch == close {
                depth += 1;
            } else if ch == open {
                depth -= 1;
                if depth == 0 {
                    return Some(text.char_to_byte(i));
                }
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Motion::*;

    fn at(text: &str, head: usize) -> Buffer {
        let mut b = Buffer::from_str(text);
        b.selection = Selection::at(head);
        b
    }
    fn span(text: &str, anchor: usize, head: usize) -> Buffer {
        let mut b = Buffer::from_str(text);
        b.selection = Selection {
            ranges: vec![Range { anchor, head }],
            primary: 0,
        };
        b
    }
    fn mv(motion: Motion, extend: bool, count: usize) -> Action {
        Action::Move {
            motion,
            extend,
            count,
        }
    }
    fn run(buffer: &mut Buffer, action: Action) {
        let mut h = History::new();
        apply(action, buffer, &mut h);
    }
    fn head(b: &Buffer) -> usize {
        b.selection.primary().head
    }

    // ---- edits ----------------------------------------------------------

    #[test]
    fn insert_char_inserts_and_advances_collapsed() {
        let mut b = at("abc", 0);
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text, "Xabc");
        assert_eq!(b.selection.primary(), Range::cursor(1));
    }

    #[test]
    fn insert_char_appends_at_end() {
        let mut b = at("abc", 3);
        run(&mut b, Action::InsertChar('Z'));
        assert_eq!(b.text, "abcZ");
        assert_eq!(head(&b), 4);
    }

    #[test]
    fn insert_multibyte_char_advances_by_byte_len() {
        let mut b = at("ab", 1);
        run(&mut b, Action::InsertChar('é')); // 2 bytes
        assert_eq!(b.text, "aéb");
        assert_eq!(head(&b), 3);
    }

    #[test]
    fn insert_newline() {
        let mut b = at("ab", 1);
        run(&mut b, Action::InsertNewline);
        assert_eq!(b.text, "a\nb");
        assert_eq!(head(&b), 2);
    }

    #[test]
    fn delete_backward_removes_prev_char() {
        let mut b = at("abc", 2);
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text, "ac");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn delete_backward_at_start_is_noop() {
        let mut b = at("abc", 0);
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text, "abc");
        assert_eq!(head(&b), 0);
    }

    #[test]
    fn delete_backward_multibyte() {
        let mut b = at("aéb", 3); // cursor after 'é' (a=0, é=1..3)
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text, "ab");
        assert_eq!(head(&b), 1);
    }

    // ---- char motions ---------------------------------------------------

    #[test]
    fn char_left_and_right() {
        let mut b = at("abc", 1);
        run(&mut b, mv(Char(Direction::Right), false, 1));
        assert_eq!(head(&b), 2);
        run(&mut b, mv(Char(Direction::Left), false, 1));
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn char_left_clamps_at_start() {
        let mut b = at("abc", 0);
        run(&mut b, mv(Char(Direction::Left), false, 1));
        assert_eq!(head(&b), 0);
    }

    #[test]
    fn char_right_clamps_at_end() {
        let mut b = at("abc", 3);
        run(&mut b, mv(Char(Direction::Right), false, 1));
        assert_eq!(head(&b), 3);
    }

    #[test]
    fn char_motion_over_multibyte() {
        let mut b = at("aé", 0);
        run(&mut b, mv(Char(Direction::Right), false, 1)); // past 'a'
        assert_eq!(head(&b), 1);
        run(&mut b, mv(Char(Direction::Right), false, 1)); // past 'é' (2 bytes)
        assert_eq!(head(&b), 3);
    }

    // ---- vertical motion ------------------------------------------------

    #[test]
    fn vertical_down_then_up_keeps_column() {
        // a0 b1 c2 \n3 d4 e5 f6
        let mut b = at("abc\ndef", 1);
        run(&mut b, mv(Char(Direction::Down), false, 1));
        assert_eq!(head(&b), 5); // line1 col1
        run(&mut b, mv(Char(Direction::Up), false, 1));
        assert_eq!(head(&b), 1); // back to line0 col1
    }

    #[test]
    fn vertical_clamps_column_to_shorter_line() {
        // abcd\nef : a0 b1 c2 d3 \n4 e5 f6  (line1 "ef" has length 2)
        let mut b = at("abcd\nef", 3); // line0 col3
        run(&mut b, mv(Char(Direction::Down), false, 1));
        assert_eq!(head(&b), 7); // clamped to end of "ef"
    }

    #[test]
    fn vertical_noop_past_first_and_last_line() {
        let mut b = at("abc", 1);
        run(&mut b, mv(Char(Direction::Up), false, 1));
        assert_eq!(head(&b), 1);
        run(&mut b, mv(Char(Direction::Down), false, 1));
        assert_eq!(head(&b), 1);
    }

    // ---- word motion ----------------------------------------------------

    #[test]
    fn word_right_to_next_word_start() {
        let mut b = at("foo bar", 0);
        run(&mut b, mv(WordStart(Direction::Right), false, 1));
        assert_eq!(head(&b), 4); // start of "bar"
    }

    #[test]
    fn word_left_to_prev_word_start() {
        let mut b = at("foo bar", 7);
        run(&mut b, mv(WordStart(Direction::Left), false, 1));
        assert_eq!(head(&b), 4); // start of "bar"
    }

    #[test]
    fn word_right_from_mid_word() {
        let mut b = at("foo bar baz", 1); // inside "foo"
        run(&mut b, mv(WordStart(Direction::Right), false, 1));
        assert_eq!(head(&b), 4); // start of "bar"
    }

    // ---- line edges -----------------------------------------------------

    #[test]
    fn line_edges() {
        // abc\ndef
        let mut b = at("abc\ndef", 6); // on 'f', line1
        run(&mut b, mv(LineEdge(Direction::Left), false, 1));
        assert_eq!(head(&b), 4); // line1 start
        run(&mut b, mv(LineEdge(Direction::Right), false, 1));
        assert_eq!(head(&b), 7); // after 'f' (line/EOF end)
    }

    #[test]
    fn line_end_stops_before_newline() {
        let mut b = at("abc\ndef", 0); // line0
        run(&mut b, mv(LineEdge(Direction::Right), false, 1));
        assert_eq!(head(&b), 3); // before the '\n'
    }

    // ---- blank-line leaps ----------------------------------------------

    #[test]
    fn blank_line_down_and_up() {
        // "aa\n\nbb": line0 "aa\n", line1 "\n" (blank), line2 "bb"
        // bytes: a0 a1 \n2 \n3 b4 b5 ; line1 starts at byte 3
        let mut b = at("aa\n\nbb", 0);
        run(&mut b, mv(BlankLine(Direction::Down), false, 1));
        assert_eq!(head(&b), 3); // the blank line
        let mut b2 = at("aa\n\nbb", 4); // line2
        run(&mut b2, mv(BlankLine(Direction::Up), false, 1));
        assert_eq!(head(&b2), 3);
    }

    #[test]
    fn blank_line_noop_when_none() {
        let mut b = at("abc", 1);
        run(&mut b, mv(BlankLine(Direction::Up), false, 1));
        assert_eq!(head(&b), 1);
        run(&mut b, mv(BlankLine(Direction::Down), false, 1));
        assert_eq!(head(&b), 1);
    }

    // ---- matching bracket ----------------------------------------------

    #[test]
    fn matching_bracket_jumps_both_ways() {
        let mut b = at("(a)", 0);
        run(&mut b, mv(MatchingBracket, false, 1));
        assert_eq!(head(&b), 2); // the ')'
        run(&mut b, mv(MatchingBracket, false, 1));
        assert_eq!(head(&b), 0); // back to '('
    }

    #[test]
    fn matching_bracket_is_nesting_aware() {
        let mut b = at("((a))", 0);
        run(&mut b, mv(MatchingBracket, false, 1));
        assert_eq!(head(&b), 4); // outer ')'
    }

    #[test]
    fn matching_bracket_across_lines() {
        // "(\n)" : ( 0, \n 1, ) 2
        let mut b = at("(\n)", 0);
        run(&mut b, mv(MatchingBracket, false, 1));
        assert_eq!(head(&b), 2);
    }

    #[test]
    fn matching_bracket_noop_off_bracket() {
        let mut b = at("abc", 1);
        run(&mut b, mv(MatchingBracket, false, 1));
        assert_eq!(head(&b), 1);
    }

    // ---- extend / count / collapse -------------------------------------

    #[test]
    fn extend_keeps_anchor_moves_head() {
        let mut b = at("abcde", 0);
        run(&mut b, mv(Char(Direction::Right), true, 1));
        assert_eq!(b.selection.primary(), Range { anchor: 0, head: 1 });
        run(&mut b, mv(Char(Direction::Right), true, 1));
        assert_eq!(b.selection.primary(), Range { anchor: 0, head: 2 });
    }

    #[test]
    fn count_moves_n_times() {
        let mut b = at("abcde", 0);
        run(&mut b, mv(Char(Direction::Right), false, 3));
        assert_eq!(head(&b), 3);
    }

    #[test]
    fn collapse_drops_span_to_head() {
        let mut b = span("abcde", 1, 4);
        run(&mut b, Action::CollapseSelection);
        assert_eq!(b.selection.primary(), Range::cursor(4));
    }

    // ---- undo through the executor -------------------------------------

    #[test]
    fn undo_reverts_an_edit() {
        let mut b = at("abc", 0);
        let mut h = History::new();
        apply(Action::InsertChar('X'), &mut b, &mut h);
        assert_eq!(b.text, "Xabc");
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(b.text, "abc");
        assert_eq!(b.selection.primary(), Range::cursor(0));
    }

    // ---- empty buffer ---------------------------------------------------

    #[test]
    fn empty_buffer_motions_never_panic() {
        for motion in [
            Char(Direction::Left),
            Char(Direction::Right),
            Char(Direction::Up),
            Char(Direction::Down),
            WordStart(Direction::Right),
            WordStart(Direction::Left),
            LineEdge(Direction::Left),
            LineEdge(Direction::Right),
            BlankLine(Direction::Up),
            BlankLine(Direction::Down),
            MatchingBracket,
        ] {
            let mut b = at("", 0);
            run(&mut b, mv(motion, false, 1));
            assert_eq!(head(&b), 0);
        }
    }
}
