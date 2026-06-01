//! The executor: applies one [`Action`] to the [`Buffer`] (and [`History`]).
//!
//! Edits flow through a [`ChangeSet`] — even a single-char insert — so undo and
//! multicursor fall out of the same machinery. Motions only move the selection
//! and are not recorded in history. All coordinates are **byte offsets** into
//! the byte-indexed [`rope::Rope`]. Horizontal motion steps by **grapheme**
//! (via the rope's grapheme-aware `clip_point`), vertical motion keeps a **goal
//! column** across short lines, and word motion uses the three-class
//! [`char_kind`](crate::char_kind) model — all mirroring Zed's `movement.rs`.
//! Every motion clamps at the buffer edges and never panics.
//!
//! Multicursor: one edit builds a single [`ChangeSet`] over every range in the
//! old coordinate space, applies it once, then maps every range into the new
//! space and merges overlaps — so one keystroke edits all cursors atomically.

use rope::{Point, Rope};
use sum_tree::Bias;

use crate::action::{Action, Direction, Expansion, Motion};
use crate::buffer::{line_content_len, nav_line_count, Buffer};
use crate::char_kind::{char_kind, find_boundary, find_preceding_boundary, CharKind};
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
            if head == 0 {
                targets.push(head); // at the buffer start: nothing to delete
            } else {
                // Delete back over the whole previous grapheme cluster (Zed
                // `backspace` = `movement::left` then delete), not one codepoint.
                let from = grapheme_left(&buffer.text, head);
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
    let forward = ChangeSet::from_changes(before_text.len(), &changes);
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
                let (new_head, goal) = move_range_head(&buffer.text, r, motion, count);
                if extend {
                    Range {
                        anchor: r.anchor,
                        head: new_head,
                        goal,
                    }
                } else {
                    Range {
                        anchor: new_head,
                        head: new_head,
                        goal,
                    }
                }
            })
            .collect(),
        primary: buffer.selection.primary,
    };
    moved.normalize();
    buffer.selection = moved;
}

/// Move one range's head, returning the new head and the goal column to carry
/// forward. Vertical motion is goal-aware (it keeps the column across short
/// lines); every other motion clears the goal — Zed resets `SelectionGoal` on
/// horizontal motion and on edits.
fn move_range_head(text: &Rope, r: &Range, motion: Motion, count: usize) -> (usize, Option<u32>) {
    match motion {
        Motion::Char(Direction::Up) => vertical_run(text, r.head, r.goal, count, true),
        Motion::Char(Direction::Down) => vertical_run(text, r.head, r.goal, count, false),
        _ => (move_head(text, r.head, motion, count), None),
    }
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

    let id = ChangeSet::identity(buffer.text.len());
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
        Motion::Char(Direction::Left) => char_horizontal(text, head, false),
        Motion::Char(Direction::Right) => char_horizontal(text, head, true),
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

/// The next char boundary after byte offset `i` (clamped to the buffer end).
fn next_boundary(text: &Rope, i: usize) -> usize {
    match text.chars_at(i).next() {
        Some(c) => i + c.len_utf8(),
        None => i,
    }
}

/// True when line `row` is empty or all whitespace. `row` must be a real line
/// index (`<= max_point().row`). The line's content (excluding its `\n`) is
/// scanned; a `\n`/`\r` break is itself whitespace, so this matches treating
/// the whole line — break included — as whitespace.
fn is_blank(text: &Rope, row: u32) -> bool {
    let start = text.point_to_offset(Point::new(row, 0));
    let end = text.point_to_offset(Point::new(row, text.line_len(row)));
    text.slice(start..end).chars().all(|c| c.is_whitespace())
}

/// One grapheme cluster to the right of byte offset `head`, porting Zed
/// `movement::right`: step one column forward (wrapping to the next line's start
/// at a line end), then snap to a grapheme boundary with the rope's
/// grapheme-aware `clip_point` (`Bias::Right`). Clamps at the buffer end.
fn grapheme_right(text: &Rope, head: usize) -> usize {
    let mut p = text.offset_to_point(head);
    if p.column < text.line_len(p.row) {
        p.column += 1;
    } else if p.row < text.max_point().row {
        p.row += 1;
        p.column = 0;
    } else {
        return head; // already at the buffer end
    }
    text.point_to_offset(text.clip_point(p, Bias::Right))
}

/// One grapheme cluster to the left of byte offset `head`, porting Zed
/// `movement::left`: step one column back (wrapping to the previous line's end),
/// then snap with `clip_point` (`Bias::Left`). Clamps at the buffer start.
fn grapheme_left(text: &Rope, head: usize) -> usize {
    let mut p = text.offset_to_point(head);
    if p.column > 0 {
        p.column -= 1;
    } else if p.row > 0 {
        p.row -= 1;
        p.column = text.line_len(p.row);
    } else {
        return head; // already at the buffer start
    }
    text.point_to_offset(text.clip_point(p, Bias::Left))
}

fn char_horizontal(text: &Rope, head: usize, right: bool) -> usize {
    if right {
        grapheme_right(text, head)
    } else {
        grapheme_left(text, head)
    }
}

/// One row up/down preserving the goal column (Zed `up_by_rows`/`down_by_rows`):
/// land at `min(goal, target line length)`, but return the *un-clamped* goal so
/// a later longer line restores the column. The goal is seeded from the current
/// column when `None`. A no-op at the first/last line returns the head and goal
/// unchanged.
fn vertical_goal(text: &Rope, head: usize, goal: Option<u32>, up: bool) -> (usize, Option<u32>) {
    let point = text.offset_to_point(head);
    let target_row = if up {
        if point.row == 0 {
            return (head, goal);
        }
        point.row - 1
    } else {
        if point.row + 1 > text.max_point().row {
            return (head, goal);
        }
        point.row + 1
    };
    let goal_col = goal.unwrap_or(point.column);
    let col = goal_col.min(line_content_len(text, target_row));
    (
        text.point_to_offset(Point::new(target_row, col)),
        Some(goal_col),
    )
}

/// Run vertical motion `count` times, threading the goal so the column is
/// computed once and reused on each row. Stops early when clamped at an edge.
fn vertical_run(
    text: &Rope,
    mut head: usize,
    mut goal: Option<u32>,
    count: usize,
    up: bool,
) -> (usize, Option<u32>) {
    for _ in 0..count.max(1) {
        let (next, next_goal) = vertical_goal(text, head, goal, up);
        if next == head {
            break; // clamped at the first/last line
        }
        head = next;
        goal = next_goal;
    }
    (head, goal)
}

/// Offset-only vertical step (goal seeded from the current column). Used by
/// `move_head` for completeness and by `spawn_cursor`, which don't track goals.
fn vertical(text: &Rope, head: usize, up: bool) -> usize {
    vertical_goal(text, head, None, up).0
}

/// `E` — to the **start of the next word** (KEYMAP: "start of next word").
/// Alteria's `E`/`Q` are both word-*start* motions; Zed's `movement.rs` has no
/// `next_word_start`, so this is the same word-start predicate `previous_word_start`
/// uses — `kind(left) != kind(right)` with `right` non-whitespace, i.e. the start
/// of a new Word/Punctuation run — applied *forward*. Whitespace runs are skipped;
/// punctuation is its own run, so `.`/`(` are stops. Stops at a newline.
fn word_right(text: &Rope, head: usize) -> usize {
    find_boundary(text, head, |left, right| {
        (char_kind(left) != char_kind(right) && char_kind(right) != CharKind::Whitespace)
            || right == '\n'
    })
}

/// `Q` — to the **start of the previous word**, porting Zed
/// `previous_word_start`: scanning back, stop at the first `kind(left) !=
/// kind(right)` where `right` is non-whitespace, or at a newline. The first-step
/// rule steps over trailing punctuation so `bar.|` jumps to `|bar.`.
fn word_left(text: &Rope, head: usize) -> usize {
    let mut first = true;
    find_preceding_boundary(text, head, |left, right| {
        if first
            && char_kind(right) == CharKind::Punctuation
            && char_kind(left) != CharKind::Punctuation
            && left != '\n'
        {
            first = false;
            return false;
        }
        first = false;
        (char_kind(left) != char_kind(right) && char_kind(right) != CharKind::Whitespace)
            || left == '\n'
    })
}

fn line_start(text: &Rope, head: usize) -> usize {
    let row = text.offset_to_point(head).row;
    text.point_to_offset(Point::new(row, 0))
}

fn line_end(text: &Rope, head: usize) -> usize {
    let row = text.offset_to_point(head).row;
    text.point_to_offset(Point::new(row, line_content_len(text, row)))
}

fn blank_line(text: &Rope, head: usize, up: bool) -> usize {
    let row = text.offset_to_point(head).row;
    let n = nav_line_count(text);
    let target = if up {
        let mut j = row;
        while j > 0 && is_blank(text, j) {
            j -= 1;
        }
        while j > 0 && !is_blank(text, j) {
            j -= 1;
        }
        is_blank(text, j).then_some(j)
    } else {
        let mut j = row;
        while j < n && is_blank(text, j) {
            j += 1;
        }
        while j < n && !is_blank(text, j) {
            j += 1;
        }
        (j < n).then_some(j)
    };
    match target {
        Some(j) => text.point_to_offset(Point::new(j, 0)),
        None => head,
    }
}

/// Jump from a bracket at `head` to its partner; or, when `head` is **inside** a
/// pair but not on a delimiter, to the enclosing pair's close (`KEYMAP.md`:
/// on/inside). Nesting-aware and across lines. `None` if there is no bracket to
/// act on.
fn matching_bracket(text: &Rope, head: usize) -> Option<usize> {
    if let Some(here) = text.chars_at(head).next() {
        match here {
            '(' | '[' | '{' => return close_for(text, head),
            ')' | ']' | '}' => return open_for(text, head),
            _ => {}
        }
    }
    // Not on a delimiter: jump to the close of the pair that encloses `head`.
    let op = enclosing_open(text, head)?;
    close_for(text, op)
}

/// The matching close for the opening bracket at byte offset `open` (else `None`).
fn close_for(text: &Rope, open: usize) -> Option<usize> {
    let open_c = text.chars_at(open).next()?;
    let close_c = match open_c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => return None,
    };
    let mut depth = 0i32;
    let mut i = open;
    for ch in text.chars_at(open) {
        if ch == open_c {
            depth += 1;
        } else if ch == close_c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += ch.len_utf8();
    }
    None
}

/// The matching open for the closing bracket at byte offset `close` (else `None`).
fn open_for(text: &Rope, close: usize) -> Option<usize> {
    let close_c = text.chars_at(close).next()?;
    let open_c = match close_c {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        _ => return None,
    };
    // Scan left from (and including) the closing bracket, tracking byte offsets.
    let mut depth = 0i32;
    let mut i = next_boundary(text, close); // one past the close bracket
    for ch in text.reversed_chars_at(next_boundary(text, close)) {
        i -= ch.len_utf8();
        if ch == close_c {
            depth += 1;
        } else if ch == open_c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The nearest opening bracket enclosing byte offset `from` (scanning left).
fn enclosing_open(text: &Rope, from: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = from;
    for ch in text.reversed_chars_at(from) {
        i -= ch.len_utf8();
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
            ranges: vec![Range::new(anchor, head)],
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
        assert_eq!(b.text.to_string(), "Xabc");
        assert_eq!(b.selection.primary(), Range::cursor(1));
    }

    #[test]
    fn insert_char_appends_at_end() {
        let mut b = at("abc", 3);
        run(&mut b, Action::InsertChar('Z'));
        assert_eq!(b.text.to_string(), "abcZ");
        assert_eq!(head(&b), 4);
    }

    #[test]
    fn insert_multibyte_char_advances_by_byte_len() {
        let mut b = at("ab", 1);
        run(&mut b, Action::InsertChar('é')); // 2 bytes
        assert_eq!(b.text.to_string(), "aéb");
        assert_eq!(head(&b), 3);
    }

    #[test]
    fn insert_newline() {
        let mut b = at("ab", 1);
        run(&mut b, Action::InsertNewline);
        assert_eq!(b.text.to_string(), "a\nb");
        assert_eq!(head(&b), 2);
    }

    #[test]
    fn delete_backward_removes_prev_char() {
        let mut b = at("abc", 2);
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text.to_string(), "ac");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn delete_backward_at_start_is_noop() {
        let mut b = at("abc", 0);
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text.to_string(), "abc");
        assert_eq!(head(&b), 0);
    }

    #[test]
    fn delete_backward_multibyte() {
        let mut b = at("aéb", 3); // cursor after 'é' (a=0, é=1..3)
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text.to_string(), "ab");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn backspace_over_a_span_deletes_the_selection() {
        // Without Alt, Alteria is a normal editor: Backspace over a selection
        // removes the whole span, not just one char.
        let mut b = span("abcde", 1, 4); // "bcd" selected
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text.to_string(), "ae");
        assert_eq!(b.selection.primary(), Range::cursor(1)); // caret at the span start
    }

    #[test]
    fn backspace_over_a_backward_span_deletes_the_selection() {
        let mut b = span("abcde", 4, 1); // same span, head left of anchor
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text.to_string(), "ae");
        assert_eq!(b.selection.primary(), Range::cursor(1));
    }

    #[test]
    fn backspace_over_multiple_spans_deletes_each() {
        // "abcdef": spans "ab" [0,2) and "ef" [4,6) -> "cd".
        let mut b = Buffer::from_str("abcdef");
        b.selection = Selection {
            ranges: vec![Range::new(0, 2), Range::new(4, 6)],
            primary: 0,
        };
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text.to_string(), "cd");
        assert_eq!(heads(&b), vec![0, 2]);
    }

    #[test]
    fn typing_over_a_forward_span_replaces_it() {
        // KEYMAP.md: "typing replaces it." The whole word "abc" is selected;
        // typing 'X' must replace the span, not insert past it.
        let mut b = span("abc", 0, 3);
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text.to_string(), "X");
        assert_eq!(b.selection.primary(), Range::cursor(1));
    }

    #[test]
    fn typing_over_a_backward_span_replaces_it() {
        // Orientation must not matter: "bcd" selected backward (head left of
        // anchor) still replaces, and the caret lands past the inserted text.
        let mut b = span("abcde", 4, 1); // anchor=4, head=1 -> span [1,4) = "bcd"
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text.to_string(), "aXe");
        assert_eq!(b.selection.primary(), Range::cursor(2)); // min(1)+len("X")
    }

    #[test]
    fn newline_over_a_span_replaces_it() {
        let mut b = span("abc", 0, 3);
        run(&mut b, Action::InsertNewline);
        assert_eq!(b.text.to_string(), "\n");
        assert_eq!(b.selection.primary(), Range::cursor(1));
    }

    #[test]
    fn typing_over_multiple_spans_replaces_each() {
        // "ab cd": select "ab" [0,2) and "cd" [3,5); typing 'X' replaces both.
        let mut b = Buffer::from_str("ab cd");
        b.selection = Selection {
            ranges: vec![Range::new(0, 2), Range::new(3, 5)],
            primary: 0,
        };
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text.to_string(), "X X");
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

    #[test]
    fn char_right_crosses_a_full_grapheme_cluster() {
        // "e" + combining acute is one grapheme (3 bytes); one Right step must
        // cross the whole cluster, not stop on the codepoint boundary at byte 1.
        let s = "e\u{0301}";
        let mut b = at(s, 0);
        run(&mut b, mv(Char(Direction::Right), false, 1));
        assert_eq!(head(&b), s.len()); // past the cluster
    }

    #[test]
    fn char_right_does_not_split_a_flag_emoji() {
        // A regional-indicator flag is one grapheme spanning two 4-byte
        // codepoints; Right crosses all 8 bytes in a single step.
        let s = "🇺🇸";
        let mut b = at(s, 0);
        run(&mut b, mv(Char(Direction::Right), false, 1));
        assert_eq!(head(&b), s.len());
    }

    #[test]
    fn backspace_deletes_the_whole_previous_grapheme() {
        // Backspace removes the previous grapheme cluster, not one codepoint:
        // deleting back over "e" + combining acute clears the whole cluster.
        let s = "e\u{0301}";
        let mut b = at(s, s.len());
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text.to_string(), "");
        assert_eq!(head(&b), 0);
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
    fn vertical_goal_column_clamps_then_restores() {
        // "abcd\nef\nghij": a0 b1 c2 d3 \n4 e5 f6 \n7 g8 h9 i10 j11
        // The middle line "ef" is shorter; the goal column survives it so the
        // third line restores column 3 (Zed goal-column behavior, not a plain
        // clamp that would stick at column 2).
        let mut b = at("abcd\nef\nghij", 3); // line0 col3
        run(&mut b, mv(Char(Direction::Down), false, 1));
        assert_eq!(head(&b), 7); // clamped to the end of "ef" (col 2)
        run(&mut b, mv(Char(Direction::Down), false, 1));
        assert_eq!(head(&b), 11); // col 3 restored on "ghij", not stuck at col 2
    }

    #[test]
    fn vertical_noop_past_first_and_last_line() {
        let mut b = at("abc", 1);
        run(&mut b, mv(Char(Direction::Up), false, 1));
        assert_eq!(head(&b), 1);
        run(&mut b, mv(Char(Direction::Down), false, 1));
        assert_eq!(head(&b), 1);
    }

    // ---- word motion (E = next-word-start, Q = previous-word-start) -----

    #[test]
    fn word_right_lands_on_next_word_start() {
        // `E` = start of the next word (KEYMAP): from the start of "foo" it
        // skips to the start of "bar", not the end of "foo".
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
    fn word_right_from_mid_word_reaches_next_word_start() {
        let mut b = at("foo bar baz", 1); // inside "foo"
        run(&mut b, mv(WordStart(Direction::Right), false, 1));
        assert_eq!(head(&b), 4); // start of "bar"
    }

    #[test]
    fn word_right_stops_at_a_word_punctuation_boundary() {
        // Three-class model: `.` starts its own (Punctuation) run, so `E` from
        // the start of "foo" lands on the '.', then on "bar".
        let mut b = at("foo.bar", 0);
        run(&mut b, mv(WordStart(Direction::Right), false, 1));
        assert_eq!(head(&b), 3); // the '.'
        run(&mut b, mv(WordStart(Direction::Right), false, 1));
        assert_eq!(head(&b), 4); // start of "bar"
    }

    #[test]
    fn word_right_stops_at_an_open_bracket() {
        let mut b = at("foo(bar)", 0);
        run(&mut b, mv(WordStart(Direction::Right), false, 1));
        assert_eq!(head(&b), 3); // before '('
    }

    #[test]
    fn word_right_skips_leading_whitespace_to_next_word_start() {
        // Whitespace is skipped: from the leading space, `E` lands on the start
        // of "foo".
        let mut b = at(" foo", 0);
        run(&mut b, mv(WordStart(Direction::Right), false, 1));
        assert_eq!(head(&b), 1); // start of "foo"
    }

    #[test]
    fn word_left_skips_trailing_punctuation() {
        // Zed's first-step rule: `Q` from `bar.|` jumps to `|bar.`, stepping
        // over the trailing punctuation rather than stopping on it.
        let mut b = at("bar.", 4);
        run(&mut b, mv(WordStart(Direction::Left), false, 1));
        assert_eq!(head(&b), 0); // start of "bar"
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
        // A file ending in '\n' has a phantom empty last line in the rope's
        // line model. It is not a blank line the user can see, so `]` must not
        // jump to it — otherwise the same visible text behaves differently
        // with/without a trailing newline.
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
        assert_eq!(b.selection.primary(), Range::new(0, 1));
        run(&mut b, mv(Char(Direction::Right), true, 1));
        assert_eq!(b.selection.primary(), Range::new(0, 2));
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
        assert_eq!(b.text.to_string(), "Xabc");
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(b.text.to_string(), "abc");
        assert_eq!(b.selection.primary(), Range::cursor(0));
    }

    // ---- multicursor ----------------------------------------------------

    #[test]
    fn two_cursors_insert_at_both() {
        let mut b = cursors("abcde", &[1, 3]);
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text.to_string(), "aXbcXde");
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
        assert_eq!(b.text.to_string(), "c");
        assert_eq!(b.selection.ranges.len(), 1);
        assert_eq!(b.selection.primary(), Range::cursor(0));
    }

    #[test]
    fn esc_collapses_multicursor_to_the_primary() {
        let mut b = Buffer::from_str("abcde");
        b.selection = Selection {
            ranges: vec![Range::new(1, 2), Range::new(3, 4)],
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
        assert_eq!(b.selection.primary(), Range::new(2, 4)); // "aa"
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        let p = b.selection.primary();
        assert_eq!((p.min(), p.max()), (2, 6)); // "aa b"

        // Ctrl+Z steps back through the expansion levels; text never changed.
        apply(Action::Undo, &mut b, &mut h);
        let p = b.selection.primary();
        assert_eq!((p.min(), p.max()), (2, 4));
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(b.selection.primary(), Range::cursor(2));
        assert_eq!(b.text.to_string(), "a aa b");
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
            ranges: vec![Range::new(3, 0)],
            primary: 0,
        };
        let mut h = History::new();
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        assert_eq!(b.selection.primary(), Range::new(3, 0)); // unchanged
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
