//! The executor: applies one [`Action`] to the [`Buffer`] (and [`History`]).
//!
//! Edits flow through a [`ChangeSet`] — even a single-char insert — so undo and
//! multicursor fall out of the same machinery. Motions only move the selection
//! and are not recorded in history. All coordinates are byte offsets; motion
//! logic converts to ropey's char indices at the boundary and moves by `char`
//! (M0; grapheme/column-memory refinement is later). Every motion clamps at the
//! buffer edges and never panics.
//!
//! Multicursor: one edit builds a single [`ChangeSet`] over every range in the
//! old coordinate space, applies it once, then maps every range into the new
//! space and merges overlaps — so one keystroke edits all cursors atomically.

use ropey::Rope;

use crate::action::{Action, Direction, Expansion, Motion};
use crate::buffer::Buffer;
use crate::expand;
use crate::find;
use crate::history::{History, Transaction};
use crate::selection::{Range, Selection};
use crate::transaction::{Assoc, ChangeSet};

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
        } => move_all(buffer, motion, extend, count),
        Action::SpawnCursor(dir) => spawn_cursor(buffer, dir),
        Action::Expand(kind) => expand_primary(buffer, history, kind),
        Action::FindChar { ch } => find_move(buffer, ch, true),
        Action::FindRepeat { ch, forward } => find_move(buffer, ch, forward),
        Action::Undo => {
            history.undo(buffer);
        }
    }
}

// ---------------------------------------------------------------------------
// Edits — every text change flows through a ChangeSet committed to history.
// ---------------------------------------------------------------------------

/// Typing inserts `s` at every bare cursor and **replaces** every selected span
/// (`KEYMAP.md`: "insert `c` at every cursor's head, advance, collapse", and a
/// selection is "replaced"). Each resulting cursor lands just past the inserted
/// text, regardless of the selection's orientation.
fn insert_text(buffer: &mut Buffer, history: &mut History, s: &str) {
    let changes: Vec<(usize, usize, String)> = buffer
        .selection
        .ranges
        .iter()
        .map(|r| (r.min(), r.max(), s.to_string()))
        .collect();
    // Follow each range's span end so the caret lands past the inserted text
    // even for a backward selection (where `head` is the span's left edge).
    let targets: Vec<usize> = buffer.selection.ranges.iter().map(|r| r.max()).collect();
    apply_edit(buffer, history, changes, targets);
}

/// `Backspace`: a non-empty selection is deleted whole (standard editor
/// behavior — without Alt held, Alteria is a normal editor); a bare cursor
/// deletes the char before its head (a cursor at the buffer start contributes
/// nothing). If nothing can be deleted it is a no-op.
fn delete_backward(buffer: &mut Buffer, history: &mut History) {
    // Per-range deletion intervals, plus where each range's caret should land.
    let mut intervals: Vec<(usize, usize)> = Vec::new();
    let mut targets: Vec<usize> = Vec::new();
    for r in &buffer.selection.ranges {
        if r.is_empty() {
            let head = r.head;
            let head_char = buffer.text.byte_to_char(head);
            if head_char == 0 {
                targets.push(head); // at the buffer start: nothing to delete
            } else {
                let from = buffer.text.char_to_byte(head_char - 1);
                intervals.push((from, head));
                targets.push(head); // map_pos lands it on the deletion start
            }
        } else {
            // A selection is removed whole; the caret lands at its start.
            intervals.push((r.min(), r.max()));
            targets.push(r.min());
        }
    }
    if intervals.is_empty() {
        return; // every cursor at the buffer start: no-op
    }
    // Merge overlapping intervals so the changeset stays non-overlapping: a bare
    // cursor can sit on the shared edge of a neighbouring span's deletion.
    intervals.sort_by_key(|iv| iv.0);
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(intervals.len());
    for iv in intervals {
        match merged.last_mut() {
            Some(last) if iv.0 < last.1 => last.1 = last.1.max(iv.1),
            _ => merged.push(iv),
        }
    }
    let changes: Vec<(usize, usize, String)> = merged
        .into_iter()
        .map(|(a, b)| (a, b, String::new()))
        .collect();
    apply_edit(buffer, history, changes, targets);
}

/// Build one changeset spanning all `changes` (in old coordinates), record its
/// inverse + selections in history, apply it once, then place each resulting
/// cursor at its mapped `target` and merge overlaps. `targets[i]` is the
/// old-space byte position range `i` should follow (the span end for an insert,
/// the head for a backspace); it is provided by the caller because the right
/// landing spot differs per edit. Every range collapses to a bare cursor.
fn apply_edit(
    buffer: &mut Buffer,
    history: &mut History,
    mut changes: Vec<(usize, usize, String)>,
    targets: Vec<usize>,
) {
    changes.sort_by_key(|c| c.0);
    let before_text = buffer.text.clone();
    let selection_before = buffer.selection.clone();
    let forward = ChangeSet::from_changes(before_text.len_bytes(), &changes);
    let inverse = forward.invert(&before_text);

    // Map every target through the one changeset. `After` keeps a cursor past
    // inserted text; at a deletion's right edge it lands on the deletion start.
    let mut after = Selection {
        ranges: targets
            .iter()
            .map(|&p| Range::cursor(forward.map_pos(p, Assoc::After)))
            .collect(),
        primary: selection_before.primary,
    };

    if !forward.apply(&mut buffer.text) {
        return; // malformed (should not happen for executor-built changes)
    }
    after.normalize();
    buffer.selection = after.clone();
    history.commit(Transaction {
        forward,
        inverse,
        selection_before,
        selection_after: after,
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

/// Move every range's head `count` times, extending or collapsing, then merge
/// any ranges that now coincide.
fn move_all(buffer: &mut Buffer, motion: Motion, extend: bool, count: usize) {
    let mut moved = Selection {
        ranges: buffer
            .selection
            .ranges
            .iter()
            .map(|r| {
                let new_head = move_head(&buffer.text, r.head, motion, count);
                if extend {
                    Range {
                        anchor: r.anchor,
                        head: new_head,
                    }
                } else {
                    Range::cursor(new_head)
                }
            })
            .collect(),
        primary: buffer.selection.primary,
    };
    moved.normalize();
    buffer.selection = moved;
}

/// `I`/`U`/`O`/`P`: expand the primary range one level and record a
/// selection-only history entry (identity changeset) so `Ctrl+Z` steps back
/// through expansions too. Outermost level is a no-op (no history entry).
fn expand_primary(buffer: &mut Buffer, history: &mut History, kind: Expansion) {
    let primary = buffer.selection.primary();
    let new_range = expand::expand(&buffer.text, primary, kind);
    // Compare by span, not orientation: `expand` returns a forward range, so a
    // backward primary at the outermost level must still be recognized as a
    // no-op (no orientation flip, no spurious history entry).
    if new_range.min() == primary.min() && new_range.max() == primary.max() {
        return;
    }
    let selection_before = buffer.selection.clone();
    let mut after = buffer.selection.clone();
    let p = after.primary;
    after.ranges[p] = new_range;
    after.normalize(); // a wider primary may now swallow a sibling cursor
    buffer.selection = after.clone();

    let id = ChangeSet::identity(buffer.text.len_bytes());
    history.commit(Transaction {
        forward: id.clone(),
        inverse: id,
        selection_before,
        selection_after: after,
    });
}

/// `Alt+F` find: move every cursor's head to the next/previous occurrence of
/// `ch` on its own line, collapsing each to a bare cursor. A cursor with no
/// match on its line stays put (so a single bare cursor with no match is a
/// no-op). Like every other motion it maps over all ranges rather than dropping
/// the secondary cursors. Find is a motion, so it is not recorded in history.
fn find_move(buffer: &mut Buffer, ch: char, forward: bool) {
    let mut moved = Selection {
        ranges: buffer
            .selection
            .ranges
            .iter()
            .map(
                |r| match find::find_on_line(&buffer.text, r.head, ch, forward) {
                    Some(new_head) => Range::cursor(new_head),
                    None => *r,
                },
            )
            .collect(),
        primary: buffer.selection.primary,
    };
    moved.normalize();
    buffer.selection = moved;
}

/// Provisional (`KEYMAP.md` "not yet specified"): add a bare cursor one line
/// above/below the primary at the same column and make it the new primary, so
/// repeated spawns build a column. No-op when there is no line that way.
fn spawn_cursor(buffer: &mut Buffer, dir: Direction) {
    let primary = buffer.selection.primary();
    let new_head = vertical(&buffer.text, primary.head, matches!(dir, Direction::Up));
    if new_head == primary.head {
        return;
    }
    let mut sel = buffer.selection.clone();
    sel.ranges.push(Range::cursor(new_head));
    sel.primary = sel.ranges.len() - 1;
    sel.normalize();
    buffer.selection = sel;
}

// ---------------------------------------------------------------------------
// Motions — pure functions over the rope, byte in / byte out, always clamped.
// ---------------------------------------------------------------------------

/// Apply `motion` to a head byte-offset `count` times, stopping early if it
/// stops making progress (clamped at an edge).
pub fn move_head(text: &Rope, mut head: usize, motion: Motion, count: usize) -> usize {
    // `MatchingBracket` is an involution (jump to the partner); a repeat count
    // would just oscillate between the two ends, so it always runs exactly once.
    let reps = if matches!(motion, Motion::MatchingBracket) {
        1
    } else {
        count.max(1)
    };
    for _ in 0..reps {
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

/// The count of navigable lines, excluding the phantom empty line ropey reports
/// after a trailing newline. That phantom line is not a real blank line the user
/// can land on, so blank-line leaps must not treat it as a target — otherwise
/// the same visible text behaves differently with and without a trailing `\n`.
fn nav_line_count(text: &Rope) -> usize {
    let lc = text.len_chars();
    if lc > 0 && text.char(lc - 1) == '\n' {
        text.len_lines() - 1
    } else {
        text.len_lines()
    }
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
    let n = nav_line_count(text);
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

/// Jump from a bracket at `head` to its partner; or, when `head` is **inside** a
/// pair but not on a delimiter, to the enclosing pair's close (`KEYMAP.md`:
/// on/inside). Nesting-aware and across lines. `None` if there is no bracket to
/// act on.
fn matching_bracket(text: &Rope, head: usize) -> Option<usize> {
    let hc = text.byte_to_char(head);
    if let Some(here) = text.get_char(hc) {
        match here {
            '(' | '[' | '{' => return close_for(text, hc).map(|i| text.char_to_byte(i)),
            ')' | ']' | '}' => return open_for(text, hc).map(|i| text.char_to_byte(i)),
            _ => {}
        }
    }
    // Not on a delimiter: jump to the close of the pair that encloses `head`.
    let op = enclosing_open(text, hc)?;
    close_for(text, op).map(|i| text.char_to_byte(i))
}

/// The matching close for the opening bracket at char index `open` (else `None`).
fn close_for(text: &Rope, open: usize) -> Option<usize> {
    let open_c = text.get_char(open)?;
    let close_c = match open_c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => return None,
    };
    let len = text.len_chars();
    let mut depth = 0i32;
    let mut i = open;
    while i < len {
        let ch = text.char(i);
        if ch == open_c {
            depth += 1;
        } else if ch == close_c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The matching open for the closing bracket at char index `close` (else `None`).
fn open_for(text: &Rope, close: usize) -> Option<usize> {
    let close_c = text.get_char(close)?;
    let open_c = match close_c {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        _ => return None,
    };
    let mut depth = 0i32;
    let mut i = close;
    loop {
        let ch = text.char(i);
        if ch == close_c {
            depth += 1;
        } else if ch == open_c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    None
}

/// The nearest opening bracket enclosing char index `from` (scanning left).
fn enclosing_open(text: &Rope, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    while i > 0 {
        i -= 1;
        let ch = text.char(i);
        if matches!(ch, ')' | ']' | '}') {
            depth += 1;
        } else if matches!(ch, '(' | '[' | '{') {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
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
    fn cursors(text: &str, heads: &[usize]) -> Buffer {
        let mut b = Buffer::from_str(text);
        b.selection = Selection {
            ranges: heads.iter().map(|&h| Range::cursor(h)).collect(),
            primary: 0,
        };
        b
    }
    fn heads(b: &Buffer) -> Vec<usize> {
        b.selection.ranges.iter().map(|r| r.head).collect()
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

    #[test]
    fn backspace_over_a_span_deletes_the_selection() {
        // Without Alt, Alteria is a normal editor: Backspace over a selection
        // removes the whole span, not just one char.
        let mut b = span("abcde", 1, 4); // "bcd" selected
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text, "ae");
        assert_eq!(b.selection.primary(), Range::cursor(1)); // caret at the span start
    }

    #[test]
    fn backspace_over_a_backward_span_deletes_the_selection() {
        let mut b = span("abcde", 4, 1); // same span, head left of anchor
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text, "ae");
        assert_eq!(b.selection.primary(), Range::cursor(1));
    }

    #[test]
    fn backspace_over_multiple_spans_deletes_each() {
        // "abcdef": spans "ab" [0,2) and "ef" [4,6) -> "cd".
        let mut b = Buffer::from_str("abcdef");
        b.selection = Selection {
            ranges: vec![Range { anchor: 0, head: 2 }, Range { anchor: 4, head: 6 }],
            primary: 0,
        };
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text, "cd");
        assert_eq!(heads(&b), vec![0, 2]);
    }

    #[test]
    fn typing_over_a_forward_span_replaces_it() {
        // KEYMAP.md: "typing replaces it." The whole word "abc" is selected;
        // typing 'X' must replace the span, not insert past it.
        let mut b = span("abc", 0, 3);
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text, "X");
        assert_eq!(b.selection.primary(), Range::cursor(1));
    }

    #[test]
    fn typing_over_a_backward_span_replaces_it() {
        // Orientation must not matter: "bcd" selected backward (head left of
        // anchor) still replaces, and the caret lands past the inserted text.
        let mut b = span("abcde", 4, 1); // anchor=4, head=1 -> span [1,4) = "bcd"
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text, "aXe");
        assert_eq!(b.selection.primary(), Range::cursor(2)); // min(1)+len("X")
    }

    #[test]
    fn newline_over_a_span_replaces_it() {
        let mut b = span("abc", 0, 3);
        run(&mut b, Action::InsertNewline);
        assert_eq!(b.text, "\n");
        assert_eq!(b.selection.primary(), Range::cursor(1));
    }

    #[test]
    fn typing_over_multiple_spans_replaces_each() {
        // "ab cd": select "ab" [0,2) and "cd" [3,5); typing 'X' replaces both.
        let mut b = Buffer::from_str("ab cd");
        b.selection = Selection {
            ranges: vec![Range { anchor: 0, head: 2 }, Range { anchor: 3, head: 5 }],
            primary: 0,
        };
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text, "X X");
        assert_eq!(heads(&b), vec![1, 3]);
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

    #[test]
    fn blank_line_down_ignores_the_phantom_trailing_line() {
        // A file ending in '\n' makes ropey report a phantom empty last line.
        // It is not a blank line the user can see, so `]` must not jump to it —
        // otherwise the same visible text behaves differently with/without a
        // trailing newline.
        let mut b = at("abc\ndef\n", 0);
        run(&mut b, mv(BlankLine(Direction::Down), false, 1));
        assert_eq!(head(&b), 0); // no real blank line below -> no-op
    }

    #[test]
    fn blank_line_down_still_finds_a_real_trailing_blank_line() {
        // "abc\n\n": line1 is a genuine blank line (then the phantom). `]` reaches it.
        let mut b = at("abc\n\n", 0);
        run(&mut b, mv(BlankLine(Direction::Down), false, 1));
        assert_eq!(head(&b), 4); // start of the blank line
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

    #[test]
    fn matching_bracket_ignores_repeat_count() {
        // Jumping to a partner is an involution; a count would just oscillate
        // between the two ends. A count must run it once, not bounce back.
        let mut b = at("()", 0);
        run(&mut b, mv(MatchingBracket, false, 2));
        assert_eq!(head(&b), 1); // partner, not back to the start
        let mut b3 = at("()", 0);
        run(&mut b3, mv(MatchingBracket, false, 3));
        assert_eq!(head(&b3), 1);
    }

    #[test]
    fn matching_bracket_from_inside_a_pair() {
        // (abc): '('=0 a=1 b=2 c=3 ')'=4 ; cursor on 'b' (byte 2), inside the pair
        let mut b = at("(abc)", 2);
        run(&mut b, mv(MatchingBracket, false, 1));
        assert_eq!(head(&b), 4); // jumps to the enclosing pair's ')'
        run(&mut b, mv(MatchingBracket, false, 1));
        assert_eq!(head(&b), 0); // and from the ')' back to the '('
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

    // ---- multicursor ----------------------------------------------------

    #[test]
    fn two_cursors_insert_at_both() {
        let mut b = cursors("abcde", &[1, 3]);
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text, "aXbcXde");
        assert_eq!(heads(&b), vec![2, 5]);
    }

    #[test]
    fn motion_moves_every_cursor() {
        let mut b = cursors("abcde", &[0, 2]);
        run(&mut b, mv(Char(Direction::Right), false, 1));
        assert_eq!(heads(&b), vec![1, 3]);
    }

    #[test]
    fn edit_that_makes_cursors_coincide_merges_them() {
        // cursors at 1 and 2: deleting before each removes "ab" -> both land at 0
        let mut b = cursors("abc", &[1, 2]);
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text, "c");
        assert_eq!(b.selection.ranges.len(), 1);
        assert_eq!(b.selection.primary(), Range::cursor(0));
    }

    #[test]
    fn esc_collapses_multicursor_to_the_primary() {
        let mut b = Buffer::from_str("abcde");
        b.selection = Selection {
            ranges: vec![Range { anchor: 1, head: 2 }, Range { anchor: 3, head: 4 }],
            primary: 1,
        };
        run(&mut b, Action::CollapseSelection);
        assert_eq!(b.selection.ranges.len(), 1);
        assert_eq!(b.selection.primary(), Range::cursor(4)); // primary head
    }

    #[test]
    fn spawn_cursor_down_adds_a_cursor_below_at_same_column() {
        let mut b = at("abc\ndef", 1); // line0, col1
        run(&mut b, Action::SpawnCursor(Direction::Down));
        let mut hs = heads(&b);
        hs.sort_unstable();
        assert_eq!(hs, vec![1, 5]); // line1 col1 = byte 5
    }

    #[test]
    fn spawn_cursor_up_adds_a_cursor_above() {
        let mut b = at("abc\ndef", 5); // line1, col1
        run(&mut b, Action::SpawnCursor(Direction::Up));
        let mut hs = heads(&b);
        hs.sort_unstable();
        assert_eq!(hs, vec![1, 5]);
    }

    #[test]
    fn spawn_cursor_noop_at_buffer_edge() {
        let mut b = at("abc", 1); // only one line
        run(&mut b, Action::SpawnCursor(Direction::Up));
        assert_eq!(b.selection.ranges.len(), 1);
    }

    // ---- find -----------------------------------------------------------

    #[test]
    fn find_moves_to_occurrence_then_repeats_both_ways() {
        // a0 ' '1 x2 ' '3 b4 ' '5 x6 ' '7 c8
        let mut b = at("a x b x c", 0);
        let mut h = History::new();
        apply(Action::FindChar { ch: 'x' }, &mut b, &mut h);
        assert_eq!(head(&b), 2);
        apply(
            Action::FindRepeat {
                ch: 'x',
                forward: true,
            },
            &mut b,
            &mut h,
        );
        assert_eq!(head(&b), 6);
        apply(
            Action::FindRepeat {
                ch: 'x',
                forward: false,
            },
            &mut b,
            &mut h,
        );
        assert_eq!(head(&b), 2);
    }

    #[test]
    fn find_no_match_is_a_noop() {
        let mut b = at("abc", 0);
        let mut h = History::new();
        apply(Action::FindChar { ch: 'z' }, &mut b, &mut h);
        assert_eq!(head(&b), 0);
    }

    #[test]
    fn find_moves_every_cursor_and_keeps_the_multicursor() {
        // Find is a motion; like every other motion it must move each cursor on
        // its own line, not silently discard the secondary cursors.
        // "axbx": a0 x1 b2 x3
        let mut b = cursors("axbx", &[0, 2]);
        let mut h = History::new();
        apply(Action::FindChar { ch: 'x' }, &mut b, &mut h);
        assert_eq!(b.selection.ranges.len(), 2);
        assert_eq!(heads(&b), vec![1, 3]);
    }

    #[test]
    fn find_with_no_match_for_one_cursor_leaves_that_cursor_put() {
        // "ax\ncd": cursor on line0 finds 'x'; cursor on line1 has none -> stays.
        // a0 x1 \n2 c3 d4
        let mut b = cursors("ax\ncd", &[0, 3]);
        let mut h = History::new();
        apply(Action::FindChar { ch: 'x' }, &mut b, &mut h);
        assert_eq!(heads(&b), vec![1, 3]);
    }

    // ---- expansion (wired through the executor, undoable) ---------------

    #[test]
    fn expand_updates_primary_and_is_undoable() {
        let mut b = at("a aa b", 2); // cursor in "aa"
        let mut h = History::new();

        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        assert_eq!(b.selection.primary(), Range { anchor: 2, head: 4 }); // "aa"
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        let p = b.selection.primary();
        assert_eq!((p.min(), p.max()), (2, 6)); // "aa b"

        // Ctrl+Z steps back through the expansion levels; text never changed.
        apply(Action::Undo, &mut b, &mut h);
        let p = b.selection.primary();
        assert_eq!((p.min(), p.max()), (2, 4));
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(b.selection.primary(), Range::cursor(2));
        assert_eq!(b.text, "a aa b");
    }

    #[test]
    fn expand_at_outermost_does_not_commit_history() {
        let mut b = at("abc", 1);
        let mut h = History::new();
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h); // word "abc"
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h); // outermost: no-op
                                                                     // A second Undo would be the no-op root if the no-op did not commit.
        assert!(h.undo(&mut b)); // undoes the word selection
        assert!(!h.undo(&mut b)); // root: nothing more
    }

    #[test]
    fn expand_outermost_backward_selection_is_a_strict_noop() {
        // A right-to-left selection of the whole word (reachable via Alt+Shift
        // extend-left) must be a true no-op at the outermost level: no history
        // entry and no orientation flip.
        let mut b = Buffer::from_str("abc");
        b.selection = Selection {
            ranges: vec![Range { anchor: 3, head: 0 }],
            primary: 0,
        };
        let mut h = History::new();
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        assert_eq!(b.selection.primary(), Range { anchor: 3, head: 0 }); // unchanged
        assert!(!h.undo(&mut b)); // nothing committed: at the root
    }

    #[test]
    fn expand_normalizes_overlap_with_a_sibling_cursor() {
        // Two cursors; expanding the primary into a word swallows the sibling.
        let mut b = Buffer::from_str("a aa b");
        b.selection = Selection {
            ranges: vec![Range::cursor(2), Range::cursor(3)],
            primary: 0,
        };
        let mut h = History::new();
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        assert_eq!(b.selection.ranges.len(), 1);
        let p = b.selection.primary();
        assert_eq!((p.min(), p.max()), (2, 4)); // "aa"
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
