//! Revision-tree undo history.
//!
//! Each undoable step is a [`Transaction`] bundling the forward edit, its
//! inverse (for undo), and the [`Selection`] on each side. Revisions form a
//! tree (each knows its parent and children) rather than a linear stack, so
//! redo is a cheap later add; only the `Ctrl+Z` undo binding ships in M0.
//!
//! Selection-expansion steps (`I`/`O`/`U`/`P`) commit a [`Transaction`] whose
//! changeset is the identity but whose selections differ, so `undo` walks back
//! through expansion steps as well as text edits — one shared timeline.

use crate::buffer::Buffer;
use crate::selection::Selection;
use crate::transaction::ChangeSet;

/// One undoable step.
#[derive(Clone, Debug)]
pub struct Transaction {
    /// The edit as applied (post-image is reached by applying this).
    pub forward: ChangeSet,
    /// The inverse edit (applying it to the post-image restores the pre-image).
    pub inverse: ChangeSet,
    /// The selection before the step (restored on undo).
    pub selection_before: Selection,
    /// The selection after the step (restored on redo, once bound).
    pub selection_after: Selection,
}

/// A node in the revision tree.
#[derive(Clone, Debug)]
struct Revision {
    parent: Option<usize>,
    /// `None` only at the root.
    transaction: Option<Transaction>,
    /// Child revisions, newest last — redo-ready (binding deferred).
    children: Vec<usize>,
}

/// The undo history: a tree of revisions with a cursor at the current one.
#[derive(Clone, Debug)]
pub struct History {
    revisions: Vec<Revision>,
    current: usize,
}

impl Default for History {
    fn default() -> Self {
        History::new()
    }
}

impl History {
    /// A fresh history holding only the (empty) root revision.
    pub fn new() -> Self {
        History {
            revisions: vec![Revision {
                parent: None,
                transaction: None,
                children: Vec::new(),
            }],
            current: 0,
        }
    }

    /// The current revision index (root = 0). Exposed for tests.
    pub fn current(&self) -> usize {
        self.current
    }

    /// Append `tx` as a child of the current revision and advance to it.
    pub fn commit(&mut self, tx: Transaction) {
        let new_idx = self.revisions.len();
        let parent = self.current;
        self.revisions[parent].children.push(new_idx);
        self.revisions.push(Revision {
            parent: Some(parent),
            transaction: Some(tx),
            children: Vec::new(),
        });
        self.current = new_idx;
    }

    /// Undo the current revision: apply its inverse to `buf`, restore the
    /// selection that preceded it, and move to its parent. A no-op at the root
    /// (returns `false`); never panics.
    pub fn undo(&mut self, buf: &mut Buffer) -> bool {
        let rev = &self.revisions[self.current];
        let (Some(parent), Some(tx)) = (rev.parent, rev.transaction.as_ref()) else {
            return false; // at the root: nothing to undo
        };
        // Clone out what we need so the borrow ends before mutating `self`.
        let inverse = tx.inverse.clone();
        let selection_before = tx.selection_before.clone();
        if !inverse.apply(&mut buf.text) {
            return false; // refuse to desync on a non-applicable inverse
        }
        buf.selection = selection_before;
        self.current = parent;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::Range;
    use ropey::Rope;

    /// Build an edit transaction the way the executor will: forward changeset,
    /// its inverse against the pre-image, and the two selections.
    fn edit_tx(
        before: &Rope,
        from: usize,
        to: usize,
        ins: &str,
        sel_before: Selection,
        sel_after: Selection,
    ) -> Transaction {
        let forward = ChangeSet::from_changes(before.len_bytes(), &[(from, to, ins.to_string())]);
        let inverse = forward.invert(before);
        Transaction {
            forward,
            inverse,
            selection_before: sel_before,
            selection_after: sel_after,
        }
    }

    #[test]
    fn undo_at_root_is_a_safe_noop() {
        let mut h = History::new();
        let mut buf = Buffer::from_str("abc");
        assert!(!h.undo(&mut buf));
        assert_eq!(buf.text, "abc");
        assert_eq!(h.current(), 0);
    }

    #[test]
    fn undo_reverts_an_insert_and_restores_selection() {
        let mut buf = Buffer::from_str("abc");
        let before = buf.text.clone();
        let tx = edit_tx(&before, 0, 0, "X", Selection::at(0), Selection::at(1));
        // Apply the edit (as the executor would) and advance the cursor.
        tx.forward.apply(&mut buf.text);
        buf.selection = Selection::at(1);

        let mut h = History::new();
        h.commit(tx);
        assert_eq!(buf.text, "Xabc");

        assert!(h.undo(&mut buf));
        assert_eq!(buf.text, "abc");
        assert_eq!(buf.selection, Selection::at(0));
        assert_eq!(h.current(), 0);
    }

    #[test]
    fn multiple_edits_undo_in_reverse_order() {
        let mut buf = Buffer::from_str("ab");
        let mut h = History::new();

        let b1 = buf.text.clone();
        let t1 = edit_tx(&b1, 0, 0, "X", Selection::at(0), Selection::at(1));
        t1.forward.apply(&mut buf.text);
        h.commit(t1);

        let b2 = buf.text.clone(); // "Xab"
        let t2 = edit_tx(&b2, 3, 3, "Y", Selection::at(3), Selection::at(4));
        t2.forward.apply(&mut buf.text);
        h.commit(t2);

        assert_eq!(buf.text, "XabY");
        assert!(h.undo(&mut buf));
        assert_eq!(buf.text, "Xab");
        assert!(h.undo(&mut buf));
        assert_eq!(buf.text, "ab");
        assert!(!h.undo(&mut buf)); // back at root
    }

    #[test]
    fn expansion_step_undo_restores_selection_without_changing_text() {
        let mut buf = Buffer::from_str("hello");
        let expanded = Selection {
            ranges: vec![Range { anchor: 0, head: 5 }],
            primary: 0,
        };
        buf.selection = expanded.clone();

        let id = ChangeSet::identity(5);
        let mut h = History::new();
        h.commit(Transaction {
            forward: id.clone(),
            inverse: id,
            selection_before: Selection::at(2),
            selection_after: expanded,
        });

        assert!(h.undo(&mut buf));
        assert_eq!(buf.text, "hello"); // text untouched
        assert_eq!(buf.selection, Selection::at(2)); // selection reverted
    }
}
