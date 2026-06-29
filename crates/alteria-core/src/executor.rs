//! The executor: applies one [`Action`] to the [`Buffer`] (and [`History`]).
//!
//! Edits flow through Zed's [`text::Buffer`](crate::buffer) — even a single-char
//! insert — so undo (clock + `UndoMap`) and multicursor fall out of the same
//! machinery. An action resolves the anchored [`Selections`](crate::selection)
//! to **byte offsets** against the snapshot, does its math in offset space, then
//! re-anchors; motions only move the selection and aren't recorded in history.
//! Horizontal motion steps by **grapheme** (the rope's grapheme-aware
//! `clip_point`), vertical motion keeps a **goal column** across short lines, and
//! word motion uses the three-class [`char_kind`](crate::char_kind) model — all
//! mirroring Zed's `movement.rs`. Every motion clamps at the buffer edges and
//! never panics.
//!
//! Multicursor: one edit builds a single set of (sorted, non-overlapping) byte
//! ranges over every selection, applies it once through `text::Buffer::edit`,
//! then re-places each caret and merges overlaps — so one keystroke edits all
//! cursors atomically and the anchors ride the change.

use std::ops::Range;

use rope::{Point, Rope};
use sum_tree::Bias;

use crate::action::{Action, Direction, Expansion, Motion};
use crate::buffer::{line_content_len, nav_line_count, Buffer};
use crate::char_kind::{char_kind, find_boundary, find_preceding_boundary, CharKind};
use crate::expand;
use crate::find;
use crate::history::History;
use crate::selection::{Selection, SelectionGoal, Selections};

/// Apply one action to the buffer, recording text edits in `history`.
pub fn apply(action: Action, buffer: &mut Buffer, history: &mut History) {
    match action {
        Action::InsertChar(c) => insert_text(buffer, history, &c.to_string()),
        Action::InsertNewline => insert_text(buffer, history, "\n"),
        Action::DeleteBackward => delete_backward(buffer, history),
        Action::DeleteForward => delete_forward(buffer, history),
        Action::CollapseSelection => collapse(buffer),
        Action::Move {
            motion,
            extend,
            count,
        } => move_all(buffer, motion, extend, count),
        Action::MovePage {
            direction,
            extend,
            rows,
        } => move_page(buffer, direction, extend, rows),
        Action::SpawnCursor(dir) => spawn_cursor(buffer, dir),
        Action::SelectAll => select_all(buffer),
        Action::Copy | Action::Paste => {}
        Action::Cut => insert_text(buffer, history, ""),
        Action::Expand(kind) => expand_primary(buffer, history, kind),
        Action::FindChar { ch } => find_move(buffer, ch, true),
        Action::FindRepeat { ch, forward } => find_move(buffer, ch, forward),
        Action::Undo => {
            history.undo(buffer);
        }
        Action::Redo => {
            history.redo(buffer);
        }
        Action::InsertText(text) => insert_text(buffer, history, &text),
        Action::InsertTexts(texts) => insert_texts(buffer, history, texts),
    }
}

// ---------------------------------------------------------------------------
// Edits — every text change flows through `text::Buffer`, recorded in history.
// ---------------------------------------------------------------------------

/// Typing inserts `s` at every bare cursor and **replaces** every selected span
/// (`KEYMAP.md`: "insert `c` at every cursor's head, advance, collapse", and a
/// selection is "replaced"). Each resulting cursor lands just past the inserted
/// text, regardless of the selection's orientation.
fn insert_text(buffer: &mut Buffer, history: &mut History, s: &str) {
    let before = buffer.selection.clone();
    let primary_id = buffer.primary_id();
    let resolved = buffer.resolved();
    let edits: Vec<(usize, usize, String)> = resolved
        .iter()
        .map(|sel| (sel.min(), sel.max(), s.to_string()))
        .collect();
    // Follow each range's span end so the caret lands past the inserted text
    // even for a backward selection (where `head` is the span's left edge).
    let targets: Vec<(usize, usize)> = resolved.iter().map(|sel| (sel.id, sel.max())).collect();
    apply_edit(buffer, history, before, primary_id, edits, targets);
}

/// Inject one string per cursor — multicursor paste distribution. Mirrors Zed's
/// `do_paste` (`editor/src/clipboard.rs`): when the slice count matches the live
/// cursor count, the i-th cursor receives `texts[i]`; otherwise Zed clears the
/// per-cursor metadata and pastes the **whole** clipboard string at every cursor
/// — and that whole string is the per-cursor slices joined by `\n` (Zed's
/// `do_copy` separator), so the mismatch fallback is exactly [`insert_text`] of
/// the `\n`-joined text. Each caret lands past its own inserted text.
fn insert_texts(buffer: &mut Buffer, history: &mut History, texts: Vec<String>) {
    let resolved = buffer.resolved();
    if texts.len() != resolved.len() {
        insert_text(buffer, history, &texts.join("\n"));
        return;
    }
    let before = buffer.selection.clone();
    let primary_id = buffer.primary_id();
    let edits: Vec<(usize, usize, String)> = resolved
        .iter()
        .zip(texts)
        .map(|(sel, t)| (sel.min(), sel.max(), t))
        .collect();
    let targets: Vec<(usize, usize)> = resolved.iter().map(|sel| (sel.id, sel.max())).collect();
    apply_edit(buffer, history, before, primary_id, edits, targets);
}

/// `Backspace`: a non-empty selection is deleted whole (standard editor
/// behavior — without Alt held, Alteria is a normal editor); a bare cursor
/// deletes the grapheme before its head (a cursor at the buffer start
/// contributes nothing). If nothing can be deleted it is a no-op.
fn delete_backward(buffer: &mut Buffer, history: &mut History) {
    let before = buffer.selection.clone();
    let primary_id = buffer.primary_id();
    let resolved = buffer.resolved();

    // Per-range deletion intervals, plus where each range's caret should land.
    let mut intervals: Vec<(usize, usize)> = Vec::new();
    let mut targets: Vec<(usize, usize)> = Vec::new();
    {
        let text = buffer.rope();
        for sel in &resolved {
            if sel.is_empty() {
                let head = sel.head();
                if head == 0 {
                    targets.push((sel.id, head)); // at the buffer start: nothing to delete
                } else {
                    // Delete back over the whole previous grapheme cluster (Zed
                    // `backspace` = `movement::left` then delete), not one codepoint.
                    let from = grapheme_left(text, head);
                    intervals.push((from, head));
                    targets.push((sel.id, head)); // maps to the deletion start
                }
            } else {
                // A selection is removed whole; the caret lands at its start.
                intervals.push((sel.min(), sel.max()));
                targets.push((sel.id, sel.min()));
            }
        }
    }
    if intervals.is_empty() {
        return; // every cursor at the buffer start: no-op
    }
    // Merge overlapping intervals so the edit stays non-overlapping: a bare
    // cursor can sit on the shared edge of a neighbouring span's deletion.
    intervals.sort_by_key(|iv| iv.0);
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(intervals.len());
    for iv in intervals {
        match merged.last_mut() {
            Some(last) if iv.0 < last.1 => last.1 = last.1.max(iv.1),
            _ => merged.push(iv),
        }
    }
    let edits: Vec<(usize, usize, String)> = merged
        .into_iter()
        .map(|(a, b)| (a, b, String::new()))
        .collect();
    apply_edit(buffer, history, before, primary_id, edits, targets);
}

/// `Delete`: a non-empty selection is deleted whole; a bare cursor deletes the
/// grapheme after its head (a cursor at the buffer end contributes nothing).
/// Mirrors Zed `editor.rs::delete`: extend right, then insert `""`.
fn delete_forward(buffer: &mut Buffer, history: &mut History) {
    let before = buffer.selection.clone();
    let primary_id = buffer.primary_id();
    let resolved = buffer.resolved();

    let mut intervals: Vec<(usize, usize)> = Vec::new();
    let mut targets: Vec<(usize, usize)> = Vec::new();
    {
        let text = buffer.rope();
        for sel in &resolved {
            if sel.is_empty() {
                let head = sel.head();
                let to = grapheme_right(text, head);
                if to == head {
                    targets.push((sel.id, head));
                } else {
                    intervals.push((head, to));
                    targets.push((sel.id, head));
                }
            } else {
                intervals.push((sel.min(), sel.max()));
                targets.push((sel.id, sel.min()));
            }
        }
    }
    if intervals.is_empty() {
        return;
    }
    intervals.sort_by_key(|iv| iv.0);
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(intervals.len());
    for iv in intervals {
        match merged.last_mut() {
            Some(last) if iv.0 < last.1 => last.1 = last.1.max(iv.1),
            _ => merged.push(iv),
        }
    }
    let edits: Vec<(usize, usize, String)> = merged
        .into_iter()
        .map(|(a, b)| (a, b, String::new()))
        .collect();
    apply_edit(buffer, history, before, primary_id, edits, targets);
}

/// Delete explicit resolved ranges while preserving `before` as the undo
/// selection. Used by cut so empty-cursor line expansion does not become the
/// selection restored by undo.
pub(crate) fn delete_resolved_ranges(
    buffer: &mut Buffer,
    history: &mut History,
    before: Selections,
    primary_id: usize,
    resolved: Vec<Selection<usize>>,
) {
    let mut edits = Vec::new();
    let mut targets = Vec::new();
    for range in resolved {
        if range.min() != range.max() {
            edits.push((range.min(), range.max(), String::new()));
        }
        targets.push((range.id, range.min()));
    }
    if edits.is_empty() {
        return;
    }
    edits.sort_by_key(|edit| edit.0);
    apply_edit(buffer, history, before, primary_id, edits, targets);
}

/// Apply `edits` (each `(from, to, text)`, **sorted and non-overlapping** in old
/// coordinates) as one undoable transaction, then place each cursor at its
/// mapped `target` (`(id, old_offset)`) and merge overlaps. `target` offsets are
/// mapped right-associatively, so a caret lands past inserted text and at a
/// deletion's new left edge. Every range collapses to a bare cursor.
fn apply_edit(
    buffer: &mut Buffer,
    history: &mut History,
    before: Selections,
    primary_id: usize,
    edits: Vec<(usize, usize, String)>,
    targets: Vec<(usize, usize)>,
) {
    // (from, to, inserted_len) for mapping carets through the change.
    let map_edits: Vec<(usize, usize, usize)> =
        edits.iter().map(|(f, t, s)| (*f, *t, s.len())).collect();
    let edit_pairs: Vec<(Range<usize>, String)> =
        edits.into_iter().map(|(f, t, s)| (f..t, s)).collect();

    let tx = buffer.edit(edit_pairs);

    let new: Vec<Selection<usize>> = targets
        .iter()
        .map(|&(id, p)| Selection::cursor(id, map_offset(&map_edits, p)))
        .collect();
    buffer.set_selections(new, primary_id);

    if let Some(tx) = tx {
        let after = buffer.selection.clone();
        history.record_edit(tx, before, after);
    }
}

/// Map an old byte offset to new space through `edits` (`(from, to, ins_len)`,
/// sorted by `from`, non-overlapping), associating to the right: a caret at an
/// insert boundary lands past the inserted text, and a caret inside a replaced
/// region lands at the replacement's new left edge plus the inserted length.
fn map_offset(edits: &[(usize, usize, usize)], p: usize) -> usize {
    let mut delta: isize = 0;
    for &(from, to, ins) in edits {
        if p < from {
            break; // this edit (and all later ones) start after p
        }
        if p >= to {
            // Edit entirely before p: shift by its net length change.
            delta += ins as isize - (to - from) as isize;
        } else {
            // p inside [from, to): land at the end of the inserted text.
            return (from as isize + delta) as usize + ins;
        }
    }
    (p as isize + delta) as usize
}

// ---------------------------------------------------------------------------
// Selection-only actions.
// ---------------------------------------------------------------------------

/// `Esc`: collapse to a single bare cursor at the primary's head (dropping any
/// secondary cursors).
fn collapse(buffer: &mut Buffer) {
    let head = buffer.primary_resolved().head();
    buffer.set_cursor(head);
}

/// Select the entire buffer (`Anchor::Min..Anchor::Max` in Zed, represented here
/// as the whole byte range in the current single buffer).
fn select_all(buffer: &mut Buffer) {
    let id = buffer.primary_id();
    buffer.set_selections(
        vec![Selection {
            id,
            start: 0,
            end: buffer.len(),
            reversed: false,
            goal: SelectionGoal::None,
        }],
        id,
    );
}

/// Move every selection's head `count` times, extending or collapsing, then
/// merge any selections that now coincide.
fn move_all(buffer: &mut Buffer, motion: Motion, extend: bool, count: usize) {
    let primary_id = buffer.primary_id();
    let resolved = buffer.resolved();
    let new: Vec<Selection<usize>> = {
        let text = buffer.rope();
        resolved
            .iter()
            .map(|sel| {
                let (new_head, goal) = move_range_head(text, sel.head(), sel.goal, motion, count);
                if extend {
                    let mut s = *sel;
                    s.set_head(new_head, goal);
                    s
                } else {
                    Selection {
                        id: sel.id,
                        start: new_head,
                        end: new_head,
                        reversed: false,
                        goal,
                    }
                }
            })
            .collect()
    };
    buffer.set_selections(new, primary_id);
}

fn move_page(buffer: &mut Buffer, direction: Direction, extend: bool, rows: usize) {
    if rows == 0 {
        return;
    }
    match direction {
        Direction::Up | Direction::Down => {
            move_all(buffer, Motion::Char(direction), extend, rows);
        }
        Direction::Left | Direction::Right => {}
    }
}

/// Move one head, returning the new head and the goal column to carry forward.
/// Vertical motion is goal-aware (it keeps the column across short lines); every
/// other motion clears the goal — Zed resets `SelectionGoal` on horizontal
/// motion and on edits.
fn move_range_head(
    text: &Rope,
    head: usize,
    goal: SelectionGoal,
    motion: Motion,
    count: usize,
) -> (usize, SelectionGoal) {
    match motion {
        Motion::Char(Direction::Up) => {
            let (h, g) = vertical_run(text, head, goal.column(), count, true);
            (h, SelectionGoal::from_column(g))
        }
        Motion::Char(Direction::Down) => {
            let (h, g) = vertical_run(text, head, goal.column(), count, false);
            (h, SelectionGoal::from_column(g))
        }
        _ => (move_head(text, head, motion, count), SelectionGoal::None),
    }
}

/// `I`/`U`/`O`/`P`: expand the primary selection one level and record a
/// selection-only history entry so `Ctrl+Z` steps back through expansions too.
/// The outermost level is a no-op (no history entry).
fn expand_primary(buffer: &mut Buffer, history: &mut History, kind: Expansion) {
    let primary = buffer.primary_resolved();
    let (nlo, nhi) = {
        let text = buffer.rope();
        expand::expand(text, primary.min(), primary.max(), kind)
    };
    // Compare by span, not orientation: `expand` returns a forward range, so a
    // backward primary at the outermost level is still recognized as a no-op (no
    // orientation flip, no spurious history entry).
    if (nlo, nhi) == (primary.min(), primary.max()) {
        return;
    }
    let before = buffer.selection.clone();
    let primary_id = buffer.primary_id();
    let mut resolved = buffer.resolved();
    if let Some(s) = resolved.iter_mut().find(|s| s.id == primary_id) {
        s.start = nlo;
        s.end = nhi;
        s.reversed = false;
        s.goal = SelectionGoal::None;
    }
    buffer.set_selections(resolved, primary_id); // a wider primary may swallow a sibling
    let after = buffer.selection.clone();
    history.record_selection(before, after);
}

/// `Alt+F` find: move every cursor's head to the next/previous occurrence of
/// `ch` on its own line, collapsing each match to a bare cursor. A cursor with
/// no match on its line stays put (so a single bare cursor with no match is a
/// no-op). Find is a motion, so it is not recorded in history.
fn find_move(buffer: &mut Buffer, ch: char, forward: bool) {
    let primary_id = buffer.primary_id();
    let resolved = buffer.resolved();
    let new: Vec<Selection<usize>> = {
        let text = buffer.rope();
        resolved
            .iter()
            .map(
                |sel| match find::find_on_line(text, sel.head(), ch, forward) {
                    Some(new_head) => Selection::cursor(sel.id, new_head),
                    None => *sel, // no match: leave this selection untouched
                },
            )
            .collect()
    };
    buffer.set_selections(new, primary_id);
}

/// Provisional (`KEYMAP.md` "not yet specified"): add a bare cursor one line
/// above/below the primary at the same column and make it the new primary, so
/// repeated spawns build a column. No-op when there is no line that way.
fn spawn_cursor(buffer: &mut Buffer, dir: Direction) {
    let primary = buffer.primary_resolved();
    let new_head = {
        let text = buffer.rope();
        vertical(text, primary.head(), matches!(dir, Direction::Up))
    };
    if new_head == primary.head() {
        return;
    }
    let new_id = buffer.alloc_id();
    let mut resolved = buffer.resolved();
    resolved.push(Selection::cursor(new_id, new_head));
    buffer.set_selections(resolved, new_id); // the new cursor becomes primary
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

    // ---- builders / accessors (offset-space views of the new model) -----

    fn at(text: &str, head: usize) -> Buffer {
        let mut b = Buffer::from_str(text);
        b.set_cursor(head);
        b
    }
    fn span(text: &str, anchor: usize, head: usize) -> Buffer {
        let mut b = Buffer::from_str(text);
        let (start, end, reversed) = if head >= anchor {
            (anchor, head, false)
        } else {
            (head, anchor, true)
        };
        let id = b.primary_id();
        b.set_selections(
            vec![Selection {
                id,
                start,
                end,
                reversed,
                goal: SelectionGoal::None,
            }],
            id,
        );
        b
    }
    fn cursors(text: &str, heads: &[usize]) -> Buffer {
        let mut b = Buffer::from_str(text);
        let sels: Vec<Selection<usize>> = heads
            .iter()
            .enumerate()
            .map(|(i, &h)| Selection::cursor(i, h))
            .collect();
        b.set_selections(sels, 0);
        b
    }
    /// Heads of every selection, in stored (position-sorted) order.
    fn heads(b: &Buffer) -> Vec<usize> {
        b.resolved().iter().map(|s| s.head()).collect()
    }
    /// The primary head.
    fn head(b: &Buffer) -> usize {
        b.primary_resolved().head()
    }
    /// The primary span as `(min, max)`.
    fn span_of(b: &Buffer) -> (usize, usize) {
        let p = b.primary_resolved();
        (p.min(), p.max())
    }
    /// The number of live selections.
    fn count(b: &Buffer) -> usize {
        b.selection.selections.len()
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

    // ---- edits ----------------------------------------------------------

    #[test]
    fn insert_char_inserts_and_advances_collapsed() {
        let mut b = at("abc", 0);
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text(), "Xabc");
        assert_eq!(head(&b), 1);
        assert!(b.primary_resolved().is_empty());
    }

    #[test]
    fn insert_char_appends_at_end() {
        let mut b = at("abc", 3);
        run(&mut b, Action::InsertChar('Z'));
        assert_eq!(b.text(), "abcZ");
        assert_eq!(head(&b), 4);
    }

    #[test]
    fn insert_multibyte_char_advances_by_byte_len() {
        let mut b = at("ab", 1);
        run(&mut b, Action::InsertChar('é')); // 2 bytes
        assert_eq!(b.text(), "aéb");
        assert_eq!(head(&b), 3);
    }

    #[test]
    fn insert_newline() {
        let mut b = at("ab", 1);
        run(&mut b, Action::InsertNewline);
        assert_eq!(b.text(), "a\nb");
        assert_eq!(head(&b), 2);
    }

    #[test]
    fn delete_backward_removes_prev_char() {
        let mut b = at("abc", 2);
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text(), "ac");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn delete_backward_at_start_is_noop() {
        let mut b = at("abc", 0);
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text(), "abc");
        assert_eq!(head(&b), 0);
    }

    #[test]
    fn delete_backward_multibyte() {
        let mut b = at("aéb", 3); // cursor after 'é' (a=0, é=1..3)
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text(), "ab");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn delete_forward_removes_next_char() {
        let mut b = at("abc", 1);
        run(&mut b, Action::DeleteForward);
        assert_eq!(b.text(), "ac");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut b = at("abc", 3);
        run(&mut b, Action::DeleteForward);
        assert_eq!(b.text(), "abc");
        assert_eq!(head(&b), 3);
    }

    #[test]
    fn delete_forward_multibyte() {
        let mut b = at("aéb", 1); // cursor before 'é' (é=1..3)
        run(&mut b, Action::DeleteForward);
        assert_eq!(b.text(), "ab");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn backspace_over_a_span_deletes_the_selection() {
        let mut b = span("abcde", 1, 4); // "bcd" selected
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text(), "ae");
        assert_eq!(head(&b), 1);
        assert!(b.primary_resolved().is_empty());
    }

    #[test]
    fn backspace_over_a_backward_span_deletes_the_selection() {
        let mut b = span("abcde", 4, 1); // same span, head left of anchor
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text(), "ae");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn delete_forward_over_a_span_deletes_the_selection() {
        let mut b = span("abcde", 1, 4);
        run(&mut b, Action::DeleteForward);
        assert_eq!(b.text(), "ae");
        assert_eq!(head(&b), 1);
        assert!(b.primary_resolved().is_empty());
    }

    #[test]
    fn backspace_over_multiple_spans_deletes_each() {
        // "abcdef": spans "ab" [0,2) and "ef" [4,6) -> "cd".
        let mut b = Buffer::from_str("abcdef");
        b.set_selections(
            vec![
                Selection {
                    id: 0,
                    start: 0,
                    end: 2,
                    reversed: false,
                    goal: SelectionGoal::None,
                },
                Selection {
                    id: 1,
                    start: 4,
                    end: 6,
                    reversed: false,
                    goal: SelectionGoal::None,
                },
            ],
            0,
        );
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text(), "cd");
        assert_eq!(heads(&b), vec![0, 2]);
    }

    #[test]
    fn typing_over_a_forward_span_replaces_it() {
        let mut b = span("abc", 0, 3);
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text(), "X");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn typing_over_a_backward_span_replaces_it() {
        // Orientation must not matter: "bcd" selected backward still replaces,
        // and the caret lands past the inserted text.
        let mut b = span("abcde", 4, 1); // anchor=4, head=1 -> span [1,4) = "bcd"
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text(), "aXe");
        assert_eq!(head(&b), 2); // min(1)+len("X")
    }

    #[test]
    fn newline_over_a_span_replaces_it() {
        let mut b = span("abc", 0, 3);
        run(&mut b, Action::InsertNewline);
        assert_eq!(b.text(), "\n");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn typing_over_multiple_spans_replaces_each() {
        // "ab cd": select "ab" [0,2) and "cd" [3,5); typing 'X' replaces both.
        let mut b = Buffer::from_str("ab cd");
        b.set_selections(
            vec![
                Selection {
                    id: 0,
                    start: 0,
                    end: 2,
                    reversed: false,
                    goal: SelectionGoal::None,
                },
                Selection {
                    id: 1,
                    start: 3,
                    end: 5,
                    reversed: false,
                    goal: SelectionGoal::None,
                },
            ],
            0,
        );
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text(), "X X");
        assert_eq!(heads(&b), vec![1, 3]);
    }

    // ---- external text injection (paste / IME): InsertText / InsertTexts -

    #[test]
    fn insert_text_at_a_bare_cursor_inserts_and_advances() {
        let mut b = at("ab", 1);
        run(&mut b, Action::InsertText("XY".to_string()));
        assert_eq!(b.text(), "aXYb");
        assert_eq!(head(&b), 3);
    }

    #[test]
    fn insert_text_over_a_span_replaces_it() {
        let mut b = span("abc", 0, 3);
        run(&mut b, Action::InsertText("XY".to_string()));
        assert_eq!(b.text(), "XY");
        assert_eq!(head(&b), 2);
    }

    #[test]
    fn insert_text_inserts_at_every_cursor() {
        let mut b = cursors("abcde", &[1, 3]);
        run(&mut b, Action::InsertText("XY".to_string()));
        assert_eq!(b.text(), "aXYbcXYde");
        assert_eq!(heads(&b), vec![3, 7]);
    }

    #[test]
    fn insert_empty_text_over_a_span_deletes_it() {
        // Exactly the delete cut relies on (Zed `cut_common` -> `insert("")`).
        let mut b = span("abcde", 1, 4); // "bcd" selected
        run(&mut b, Action::InsertText(String::new()));
        assert_eq!(b.text(), "ae");
        assert_eq!(head(&b), 1);
        assert!(b.primary_resolved().is_empty());
    }

    #[test]
    fn insert_texts_distributes_one_slice_per_cursor() {
        let mut b = cursors("ab", &[0, 1]);
        run(
            &mut b,
            Action::InsertTexts(vec!["X".to_string(), "Y".to_string()]),
        );
        assert_eq!(b.text(), "XaYb");
        assert_eq!(heads(&b), vec![1, 3]);
    }

    #[test]
    fn insert_texts_count_mismatch_inserts_whole_at_each() {
        // 1 slice, 2 cursors: Zed `do_paste`'s mismatch branch pastes the whole
        // clipboard string at every cursor.
        let mut b = cursors("ab", &[0, 1]);
        run(&mut b, Action::InsertTexts(vec!["P".to_string()]));
        assert_eq!(b.text(), "PaPb");
        assert_eq!(heads(&b), vec![1, 3]);
    }

    #[test]
    fn insert_texts_count_mismatch_joins_slices_with_newline() {
        // More slices than cursors: the "whole" text is the slices joined by '\n'
        // (Zed's clipboard string), inserted at the single cursor.
        let mut b = at("xy", 1);
        run(
            &mut b,
            Action::InsertTexts(vec!["a".to_string(), "b".to_string()]),
        );
        assert_eq!(b.text(), "xa\nby");
        assert_eq!(head(&b), 4); // past "a\nb"
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
        let s = "e\u{0301}";
        let mut b = at(s, 0);
        run(&mut b, mv(Char(Direction::Right), false, 1));
        assert_eq!(head(&b), s.len()); // past the cluster
    }

    #[test]
    fn char_right_does_not_split_a_flag_emoji() {
        let s = "🇺🇸";
        let mut b = at(s, 0);
        run(&mut b, mv(Char(Direction::Right), false, 1));
        assert_eq!(head(&b), s.len());
    }

    #[test]
    fn backspace_deletes_the_whole_previous_grapheme() {
        let s = "e\u{0301}";
        let mut b = at(s, s.len());
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text(), "");
        assert_eq!(head(&b), 0);
    }

    // ---- vertical motion ------------------------------------------------

    #[test]
    fn vertical_down_then_up_keeps_column() {
        let mut b = at("abc\ndef", 1);
        run(&mut b, mv(Char(Direction::Down), false, 1));
        assert_eq!(head(&b), 5); // line1 col1
        run(&mut b, mv(Char(Direction::Up), false, 1));
        assert_eq!(head(&b), 1); // back to line0 col1
    }

    #[test]
    fn vertical_goal_column_clamps_then_restores() {
        // "abcd\nef\nghij": the middle line "ef" is shorter; the goal column
        // survives it so the third line restores column 3.
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
        let mut b = at(" foo", 0);
        run(&mut b, mv(WordStart(Direction::Right), false, 1));
        assert_eq!(head(&b), 1); // start of "foo"
    }

    #[test]
    fn word_left_skips_trailing_punctuation() {
        let mut b = at("bar.", 4);
        run(&mut b, mv(WordStart(Direction::Left), false, 1));
        assert_eq!(head(&b), 0); // start of "bar"
    }

    // ---- line edges -----------------------------------------------------

    #[test]
    fn line_edges() {
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

    #[test]
    fn page_move_uses_supplied_visible_row_count() {
        let mut b = at("a\nb\nc\nd", 0);
        run(
            &mut b,
            Action::MovePage {
                direction: Direction::Down,
                extend: false,
                rows: 2,
            },
        );
        assert_eq!(head(&b), 4); // line 2, col 0
    }

    #[test]
    fn shift_page_move_extends() {
        let mut b = at("a\nb\nc\nd", 0);
        run(
            &mut b,
            Action::MovePage {
                direction: Direction::Down,
                extend: true,
                rows: 2,
            },
        );
        assert_eq!(span_of(&b), (0, 4));
    }

    // ---- blank-line leaps ----------------------------------------------

    #[test]
    fn blank_line_down_and_up() {
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
        let mut b = at("abc\ndef\n", 0);
        run(&mut b, mv(BlankLine(Direction::Down), false, 1));
        assert_eq!(head(&b), 0); // no real blank line below -> no-op
    }

    #[test]
    fn blank_line_down_still_finds_a_real_trailing_blank_line() {
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
        let mut b = at("()", 0);
        run(&mut b, mv(MatchingBracket, false, 2));
        assert_eq!(head(&b), 1); // partner, not back to the start
        let mut b3 = at("()", 0);
        run(&mut b3, mv(MatchingBracket, false, 3));
        assert_eq!(head(&b3), 1);
    }

    #[test]
    fn matching_bracket_from_inside_a_pair() {
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
        assert_eq!(span_of(&b), (0, 1));
        run(&mut b, mv(Char(Direction::Right), true, 1));
        assert_eq!(span_of(&b), (0, 2));
        assert!(!b.primary_resolved().reversed);
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
        assert_eq!(head(&b), 4);
        assert!(b.primary_resolved().is_empty());
    }

    #[test]
    fn select_all_selects_the_whole_buffer() {
        let mut b = at("abc\ndef", 4);
        run(&mut b, Action::SelectAll);
        assert_eq!(span_of(&b), (0, b.len()));
        assert!(!b.primary_resolved().reversed);
    }

    // ---- undo / redo through the executor and history ------------------

    #[test]
    fn undo_reverts_an_edit() {
        let mut b = at("abc", 0);
        let mut h = History::new();
        apply(Action::InsertChar('X'), &mut b, &mut h);
        assert_eq!(b.text(), "Xabc");
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(b.text(), "abc");
        assert_eq!(head(&b), 0);
    }

    #[test]
    fn redo_reapplies_an_undone_edit() {
        let mut b = at("abc", 0);
        let mut h = History::new();
        apply(Action::InsertChar('X'), &mut b, &mut h);
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(b.text(), "abc");
        assert!(h.redo(&mut b)); // redo is an engine capability (KEYMAP: not yet bound)
        assert_eq!(b.text(), "Xabc");
        assert_eq!(head(&b), 1);
    }

    #[test]
    fn redo_action_reapplies_an_undone_edit() {
        // The bound verb: Action::Redo flows through the executor like Action::Undo.
        let mut b = at("abc", 0);
        let mut h = History::new();
        apply(Action::InsertChar('X'), &mut b, &mut h);
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(b.text(), "abc");
        apply(Action::Redo, &mut b, &mut h);
        assert_eq!(b.text(), "Xabc"); // text restored
        assert_eq!(head(&b), 1); // and the after-selection
    }

    #[test]
    fn redo_action_at_top_of_stack_is_a_noop() {
        let mut b = at("abc", 0);
        let mut h = History::new();
        apply(Action::Redo, &mut b, &mut h); // nothing was undone
        assert_eq!(b.text(), "abc");
        assert_eq!(head(&b), 0);
    }

    // ---- multicursor ----------------------------------------------------

    #[test]
    fn two_cursors_insert_at_both() {
        let mut b = cursors("abcde", &[1, 3]);
        run(&mut b, Action::InsertChar('X'));
        assert_eq!(b.text(), "aXbcXde");
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
        let mut b = cursors("abc", &[1, 2]);
        run(&mut b, Action::DeleteBackward);
        assert_eq!(b.text(), "c");
        assert_eq!(count(&b), 1);
        assert_eq!(head(&b), 0);
    }

    #[test]
    fn esc_collapses_multicursor_to_the_primary() {
        let mut b = Buffer::from_str("abcde");
        b.set_selections(
            vec![
                Selection {
                    id: 0,
                    start: 1,
                    end: 2,
                    reversed: false,
                    goal: SelectionGoal::None,
                },
                Selection {
                    id: 1,
                    start: 3,
                    end: 4,
                    reversed: false,
                    goal: SelectionGoal::None,
                },
            ],
            1,
        );
        run(&mut b, Action::CollapseSelection);
        assert_eq!(count(&b), 1);
        assert_eq!(head(&b), 4); // primary head
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
        assert_eq!(count(&b), 1);
    }

    // ---- find -----------------------------------------------------------

    #[test]
    fn find_moves_to_occurrence_then_repeats_both_ways() {
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
        let mut b = cursors("axbx", &[0, 2]);
        let mut h = History::new();
        apply(Action::FindChar { ch: 'x' }, &mut b, &mut h);
        assert_eq!(count(&b), 2);
        assert_eq!(heads(&b), vec![1, 3]);
    }

    #[test]
    fn find_with_no_match_for_one_cursor_leaves_that_cursor_put() {
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
        assert_eq!(span_of(&b), (2, 4)); // "aa"
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        assert_eq!(span_of(&b), (2, 6)); // "aa b"

        // Ctrl+Z steps back through the expansion levels; text never changed.
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(span_of(&b), (2, 4));
        apply(Action::Undo, &mut b, &mut h);
        assert_eq!(head(&b), 2);
        assert!(b.primary_resolved().is_empty());
        assert_eq!(b.text(), "a aa b");
    }

    #[test]
    fn expand_at_outermost_does_not_commit_history() {
        let mut b = at("abc", 1);
        let mut h = History::new();
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h); // word "abc"
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h); // outermost: no-op
        assert!(h.undo(&mut b)); // undoes the word selection
        assert!(!h.undo(&mut b)); // root: nothing more
    }

    #[test]
    fn expand_outermost_backward_selection_is_a_strict_noop() {
        // A right-to-left whole-word selection must be a true no-op at the
        // outermost level: no history entry and no orientation flip.
        let mut b = Buffer::from_str("abc");
        let id = b.primary_id();
        b.set_selections(
            vec![Selection {
                id,
                start: 0,
                end: 3,
                reversed: true, // head at 0 (extended leftward)
                goal: SelectionGoal::None,
            }],
            id,
        );
        let mut h = History::new();
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        assert_eq!(span_of(&b), (0, 3));
        assert!(b.primary_resolved().reversed); // orientation unchanged
        assert!(!h.undo(&mut b)); // nothing committed: at the root
    }

    #[test]
    fn expand_normalizes_overlap_with_a_sibling_cursor() {
        // Two cursors; expanding the primary into a word swallows the sibling.
        let mut b = Buffer::from_str("a aa b");
        b.set_selections(vec![Selection::cursor(0, 2), Selection::cursor(1, 3)], 0);
        let mut h = History::new();
        apply(Action::Expand(Expansion::Enclosing), &mut b, &mut h);
        assert_eq!(count(&b), 1);
        assert_eq!(span_of(&b), (2, 4)); // "aa"
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
