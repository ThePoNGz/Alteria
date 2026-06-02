//! The undo/redo timeline — one shared stack over Zed's clock + `UndoMap`.
//!
//! Text edits delegate their inverse to [`text::Buffer`]: each edit is a
//! timestamped op, undo emits an undo op that flips fragment visibility in the
//! `UndoMap`, and **real redo is undo-of-undo** (`Buffer::redo`). Selection-only
//! expansion steps (`I`/`O`/`U`/`P`, per `KEYMAP.md`) interleave on the *same*
//! timeline as entries that carry no text op — so `Ctrl+Z` walks back through
//! expansions and edits alike (KEYMAP: "one shared timeline").
//!
//! Every entry stores the [`Selections`] to restore on undo (`before`) and redo
//! (`after`). They are anchors, so once the text op is reversed they resolve to
//! the right offsets for free.
//!
//! **Invariant:** the engine drives `text::Buffer` undo/redo *only* through the
//! `Edit` entries here, in this timeline's order. Selection-only steps never
//! touch the `text::Buffer` stacks, so the engine's `Edit` entries stay in 1:1
//! lockstep with Zed's undo/redo stacks (undo grouping is disabled, so each edit
//! is exactly one transaction).

use crate::buffer::Buffer;
use crate::selection::Selections;
use text::TransactionId;

/// What an undo entry does to the text. The selection is always restored.
#[derive(Clone, Debug)]
enum Step {
    /// A text transaction in the `text::Buffer` op log (undo/redo via `UndoMap`).
    Edit(TransactionId),
    /// A selection-only step (an expansion): no text change.
    SelectionOnly,
}

/// One step on the shared timeline.
#[derive(Clone, Debug)]
struct Entry {
    step: Step,
    /// The selection before the step (restored on undo).
    before: Selections,
    /// The selection after the step (restored on redo).
    after: Selections,
}

/// The undo/redo history: two stacks of [`Entry`]s.
#[derive(Clone, Debug)]
pub struct History {
    undo_stack: Vec<Entry>,
    redo_stack: Vec<Entry>,
}

impl Default for History {
    fn default() -> Self {
        History::new()
    }
}

impl History {
    /// A fresh, empty history.
    pub fn new() -> Self {
        History {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    /// Record a text edit. `transaction` is the id `text::Buffer::edit` produced;
    /// `before`/`after` are the selection on each side. Clears the redo branch.
    pub fn record_edit(
        &mut self,
        transaction: TransactionId,
        before: Selections,
        after: Selections,
    ) {
        self.undo_stack.push(Entry {
            step: Step::Edit(transaction),
            before,
            after,
        });
        self.redo_stack.clear();
    }

    /// Record a selection-only step (an expansion). Clears the redo branch.
    pub fn record_selection(&mut self, before: Selections, after: Selections) {
        self.undo_stack.push(Entry {
            step: Step::SelectionOnly,
            before,
            after,
        });
        self.redo_stack.clear();
    }

    /// Step back one entry: reverse its text op (if any) and restore the
    /// selection that preceded it. A no-op at the root (returns `false`); never
    /// panics.
    pub fn undo(&mut self, buf: &mut Buffer) -> bool {
        let Some(entry) = self.undo_stack.pop() else {
            return false;
        };
        if let Step::Edit(tx) = &entry.step {
            // The timeline's Edit entries stay in lockstep with text::Buffer's
            // undo stack (no grouping), so the undone transaction must match.
            let undone = buf.undo();
            debug_assert_eq!(undone, Some(*tx), "undo out of sync with text::Buffer");
        }
        buf.selection = entry.before.clone();
        self.redo_stack.push(entry);
        true
    }

    /// Step forward one entry: re-apply its text op (if any) and restore the
    /// selection that followed it. A no-op when nothing was undone (returns
    /// `false`); never panics.
    pub fn redo(&mut self, buf: &mut Buffer) -> bool {
        let Some(entry) = self.redo_stack.pop() else {
            return false;
        };
        if let Step::Edit(tx) = &entry.step {
            let redone = buf.redo();
            debug_assert_eq!(redone, Some(*tx), "redo out of sync with text::Buffer");
        }
        buf.selection = entry.after.clone();
        self.undo_stack.push(entry);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive an edit the way the executor does: snapshot the selection, apply
    /// the edit, move the caret, then record the (before, after) pair.
    fn typed(buf: &mut Buffer, history: &mut History, range: std::ops::Range<usize>, s: &str) {
        let before = buf.selection.clone();
        let caret = range.start + s.len();
        let tx = buf.edit(vec![(range, s.to_string())]);
        buf.set_cursor(caret);
        let after = buf.selection.clone();
        history.record_edit(tx.expect("edit produced a transaction"), before, after);
    }

    /// Drive a selection-only step (an expansion): record (before, after) with
    /// no text change.
    fn expanded(buf: &mut Buffer, history: &mut History, start: usize, end: usize) {
        let before = buf.selection.clone();
        buf.set_selections(
            vec![crate::selection::Selection {
                id: buf.primary_id(),
                start,
                end,
                reversed: false,
                goal: crate::selection::SelectionGoal::None,
            }],
            buf.primary_id(),
        );
        let after = buf.selection.clone();
        history.record_selection(before, after);
    }

    #[test]
    fn undo_at_root_is_a_safe_noop() {
        let mut h = History::new();
        let mut buf = Buffer::from_str("abc");
        assert!(!h.undo(&mut buf));
        assert_eq!(buf.text(), "abc");
        assert!(!h.redo(&mut buf));
    }

    #[test]
    fn undo_reverts_an_insert_and_restores_selection() {
        let mut buf = Buffer::from_str("abc");
        let mut h = History::new();
        typed(&mut buf, &mut h, 0..0, "X");
        assert_eq!(buf.text(), "Xabc");
        assert_eq!(buf.primary_resolved().head(), 1);

        assert!(h.undo(&mut buf));
        assert_eq!(buf.text(), "abc");
        assert_eq!(buf.primary_resolved().head(), 0);
    }

    #[test]
    fn redo_reapplies_text_and_selection() {
        let mut buf = Buffer::from_str("abc");
        let mut h = History::new();
        typed(&mut buf, &mut h, 0..0, "X");
        h.undo(&mut buf);
        assert_eq!(buf.text(), "abc");

        assert!(h.redo(&mut buf));
        assert_eq!(buf.text(), "Xabc");
        assert_eq!(buf.primary_resolved().head(), 1);
        // Nothing left to redo.
        assert!(!h.redo(&mut buf));
    }

    #[test]
    fn multiple_edits_undo_and_redo_in_order() {
        let mut buf = Buffer::from_str("ab");
        let mut h = History::new();
        typed(&mut buf, &mut h, 0..0, "X"); // "Xab"
        typed(&mut buf, &mut h, 3..3, "Y"); // "XabY"
        assert_eq!(buf.text(), "XabY");

        assert!(h.undo(&mut buf));
        assert_eq!(buf.text(), "Xab");
        assert!(h.undo(&mut buf));
        assert_eq!(buf.text(), "ab");
        assert!(!h.undo(&mut buf)); // root

        assert!(h.redo(&mut buf));
        assert_eq!(buf.text(), "Xab");
        assert!(h.redo(&mut buf));
        assert_eq!(buf.text(), "XabY");
    }

    #[test]
    fn expansion_step_undo_restores_selection_without_changing_text() {
        let mut buf = Buffer::from_str("hello");
        let mut h = History::new();
        buf.set_cursor(2);
        expanded(&mut buf, &mut h, 0, 5); // "select" the whole word, text untouched
        assert_eq!(
            (buf.primary_resolved().min(), buf.primary_resolved().max()),
            (0, 5)
        );

        assert!(h.undo(&mut buf));
        assert_eq!(buf.text(), "hello"); // text untouched
        assert_eq!(buf.primary_resolved().head(), 2); // selection reverted

        assert!(h.redo(&mut buf));
        assert_eq!(
            (buf.primary_resolved().min(), buf.primary_resolved().max()),
            (0, 5)
        );
    }

    #[test]
    fn edits_and_expansions_share_one_timeline() {
        // expand (selection-only) then type over it: undo walks back through both.
        let mut buf = Buffer::from_str("foo");
        let mut h = History::new();
        expanded(&mut buf, &mut h, 0, 3); // select "foo"
        typed(&mut buf, &mut h, 0..3, "x"); // replace -> "x"
        assert_eq!(buf.text(), "x");

        assert!(h.undo(&mut buf)); // undo the edit: text back, expanded span restored
        assert_eq!(buf.text(), "foo");
        assert_eq!(
            (buf.primary_resolved().min(), buf.primary_resolved().max()),
            (0, 3)
        );

        assert!(h.undo(&mut buf)); // undo the expansion: selection collapses
        assert_eq!(buf.text(), "foo");
        assert_eq!(buf.primary_resolved().head(), 0);

        assert!(!h.undo(&mut buf)); // root
    }
}
