//! The text buffer: Zed's [`text::Buffer`] (rope + Lamport clock + `UndoMap`)
//! plus the current [`Selections`].
//!
//! `Buffer` wraps Zed's `text::Buffer` — the byte-indexed `Rope`, the operation
//! log, and the anchor/clock model — so edits, undo/redo, and anchored
//! selections all ride the same machinery Zed uses. Reads go through a
//! [`BufferSnapshot`]; the well-tested motion layer keeps operating on the raw
//! [`Rope`] via [`BufferSnapshot::as_rope`], and the engine crosses the
//! anchor↔offset boundary only here ([`Buffer::resolved`] / [`Buffer::set_selections`]).
//!
//! Line endings are normalized to `\n` on construction (Zed's `Buffer::new`),
//! which is why the motion helpers can treat `\n` as the only break.

use std::ops::Range;
use std::time::Duration;

use rope::{Point, Rope};
use text::{Anchor, BufferId, BufferSnapshot, ReplicaId, ToOffset, TransactionId};

use crate::selection::{normalize, Selection, Selections};

/// The byte length of line `row`'s content, excluding its trailing line break
/// (`\n`, and a `\r` immediately before it for `\r\n` endings).
///
/// Rope counts lines by `\n` only, so [`Rope::line_len`] already stops before
/// the `\n`; this additionally trims a trailing `\r` so a `\r\n` line behaves
/// like its `\n` counterpart. Returns a **byte** column (rope columns are
/// bytes). Shared by `executor` and `find` so line-edge math is defined once.
pub(crate) fn line_content_len(text: &Rope, row: u32) -> u32 {
    let col = text.line_len(row);
    if col > 0 {
        // Peek the last byte of the line's content: a trailing '\r' is part of a
        // '\r\n' break and must not count toward the visible line length.
        let end = text.point_to_offset(Point::new(row, col));
        if text.reversed_chars_at(end).next() == Some('\r') {
            return col - 1;
        }
    }
    col
}

/// The count of navigable lines, excluding the phantom empty line that a
/// trailing newline produces (`"a\n"` is one navigable line, not two). That
/// phantom line is not somewhere the user can land, so blank-line leaps must
/// not treat it as a target — otherwise the same visible text would behave
/// differently with and without a trailing `\n`.
pub(crate) fn nav_line_count(text: &Rope) -> u32 {
    let rows = text.max_point().row; // last line index; +1 == total lines
    if ends_with_newline(text) {
        rows // the phantom trailing line is dropped
    } else {
        rows + 1
    }
}

/// True when the document ends in a `\n` (and so has a phantom trailing line).
pub(crate) fn ends_with_newline(text: &Rope) -> bool {
    text.reversed_chars_at(text.len()).next() == Some('\n')
}

/// A document (Zed `text::Buffer`) and where the cursor(s) currently are.
pub struct Buffer {
    /// Zed's buffer: owns the rope, the Lamport clock, the op log, and the
    /// `History` (undo/redo via `UndoMap`). Reads go through [`Buffer::snapshot`].
    inner: text::Buffer,
    /// The multicursor set, stored as anchors that ride edits for free.
    pub selection: Selections,
}

impl Buffer {
    /// Build a buffer from a string, with a single bare cursor at offset 0.
    ///
    /// Zed's `Buffer::new` normalizes `\r\n`/`\r` line endings to `\n` (plan
    /// 005 / A5). Undo grouping is disabled (`group_interval = 0`) so every edit
    /// is its own undo step, matching the per-keystroke history the engine and
    /// its tests expect; coalescing is a deliberate future concern.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        let mut inner = text::Buffer::new(
            ReplicaId::LOCAL,
            BufferId::new(1).expect("buffer id 1 is non-zero"),
            s,
        );
        inner.set_group_interval(Duration::ZERO);
        let cursor = {
            let snap = inner.snapshot();
            anchorize(snap, &Selection::cursor(0, 0))
        };
        Buffer {
            inner,
            selection: Selections::single(cursor),
        }
    }

    /// The current read-only snapshot (text + version + anchor resolution).
    pub fn snapshot(&self) -> &BufferSnapshot {
        self.inner.snapshot()
    }

    /// The underlying byte-indexed rope — what the motion layer reads.
    pub fn rope(&self) -> &Rope {
        self.inner.as_rope()
    }

    /// The whole document as a `String` (for rendering and tests).
    pub fn text(&self) -> String {
        self.inner.text()
    }

    /// The byte length of the document.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// True when the document is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Every selection resolved to byte offsets against the current snapshot —
    /// the working form the executor does its motion/edit math in.
    pub fn resolved(&self) -> Vec<Selection<usize>> {
        let snap = self.inner.snapshot();
        self.selection
            .selections
            .iter()
            .map(|s| resolve(snap, s))
            .collect()
    }

    /// The primary selection resolved to byte offsets.
    pub fn primary_resolved(&self) -> Selection<usize> {
        let snap = self.inner.snapshot();
        resolve(snap, self.selection.primary())
    }

    /// The id of the primary selection.
    pub fn primary_id(&self) -> usize {
        self.selection.primary_id()
    }

    /// Hand out a fresh selection id (for spawning a cursor).
    pub fn alloc_id(&mut self) -> usize {
        self.selection.alloc_id()
    }

    /// Replace the selection set from resolved offset selections: normalize
    /// (sort + merge overlaps in offset space), re-anchor against the current
    /// snapshot, and record the surviving primary id.
    pub fn set_selections(&mut self, mut resolved: Vec<Selection<usize>>, primary_id: usize) {
        let primary_id = normalize(&mut resolved, primary_id);
        let anchored = {
            let snap = self.inner.snapshot();
            resolved.iter().map(|s| anchorize(snap, s)).collect()
        };
        self.selection.replace(anchored, primary_id);
    }

    /// Collapse to a single bare cursor at `offset`, keeping the primary id.
    pub fn set_cursor(&mut self, offset: usize) {
        let id = self.selection.primary_id();
        self.set_selections(vec![Selection::cursor(id, offset)], id);
    }

    /// Apply `edits` (offset ranges → replacement text) as **one** undoable
    /// transaction and return its id, or `None` when there is nothing to do.
    /// Ranges must be sorted and non-overlapping (the executor guarantees it).
    /// Selection placement is the caller's job — anchors are re-set afterward.
    pub fn edit(&mut self, edits: Vec<(Range<usize>, String)>) -> Option<TransactionId> {
        if edits.is_empty() {
            return None;
        }
        self.inner.start_transaction();
        self.inner.edit(edits);
        self.inner.end_transaction().map(|(id, _)| id)
    }

    /// Undo the most recent text transaction (Zed's clock + `UndoMap`); returns
    /// the undone transaction id, or `None` at the root. Selection restoration
    /// is the history layer's job.
    pub fn undo(&mut self) -> Option<TransactionId> {
        self.inner.undo().map(|(id, _)| id)
    }

    /// Redo the most recently undone transaction (real redo = undo-of-undo via
    /// `UndoMap`); returns its id, or `None` when the redo stack is empty.
    pub fn redo(&mut self) -> Option<TransactionId> {
        self.inner.redo().map(|(id, _)| id)
    }
}

/// Resolve an anchored selection to byte offsets against `snap`.
fn resolve(snap: &BufferSnapshot, s: &Selection<Anchor>) -> Selection<usize> {
    let start = s.start.to_offset(snap);
    let end = s.end.to_offset(snap);
    debug_assert!(start <= end, "resolved selection must stay ordered");
    Selection {
        id: s.id,
        start,
        end,
        reversed: s.reversed,
        goal: s.goal,
    }
}

/// Re-anchor a resolved selection against `snap`. A bare cursor uses one anchor
/// for both ends; a span biases its `start` rightward and its `end` leftward
/// (`anchor_after`/`anchor_before`), so text typed just outside the span stays
/// outside it.
fn anchorize(snap: &BufferSnapshot, s: &Selection<usize>) -> Selection<Anchor> {
    let (start, end) = if s.start == s.end {
        let a = snap.anchor_before(s.start);
        (a, a)
    } else {
        (snap.anchor_after(s.start), snap.anchor_before(s.end))
    };
    Selection {
        id: s.id,
        start,
        end,
        reversed: s.reversed,
        goal: s.goal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_holds_the_text() {
        let buf = Buffer::from_str("abc");
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.text(), "abc");
    }

    #[test]
    fn from_str_starts_with_a_bare_cursor_at_zero() {
        let buf = Buffer::from_str("abc");
        assert_eq!(buf.selection.selections.len(), 1);
        let primary = buf.primary_resolved();
        assert!(primary.is_empty());
        assert_eq!(primary.head(), 0);
    }

    #[test]
    fn empty_buffer_has_zero_bytes_and_a_cursor_at_zero() {
        let buf = Buffer::from_str("");
        assert!(buf.is_empty());
        let p = buf.primary_resolved();
        assert!(p.is_empty() && p.head() == 0);
    }

    #[test]
    fn crlf_is_normalized_on_load() {
        // A5: `\r\n` and lone `\r` collapse to `\n` so the rope holds only `\n`.
        let buf = Buffer::from_str("a\r\nb\rc");
        assert_eq!(buf.text(), "a\nb\nc");
    }

    #[test]
    fn anchors_survive_an_edit_elsewhere() {
        // A cursor at offset 5 rides an insertion made before it: the stored
        // anchor resolves to the shifted offset without any manual mapping.
        let mut buf = Buffer::from_str("abcdef");
        buf.set_cursor(5);
        buf.edit(vec![(0..0, "XY".to_string())]);
        assert_eq!(buf.text(), "XYabcdef");
        assert_eq!(buf.primary_resolved().head(), 7); // 5 + 2 inserted
    }

    #[test]
    fn edit_undo_redo_round_trips_text() {
        let mut buf = Buffer::from_str("abc");
        let tx = buf.edit(vec![(0..0, "X".to_string())]);
        assert!(tx.is_some());
        assert_eq!(buf.text(), "Xabc");
        assert!(buf.undo().is_some());
        assert_eq!(buf.text(), "abc");
        assert!(buf.redo().is_some());
        assert_eq!(buf.text(), "Xabc");
    }
}
