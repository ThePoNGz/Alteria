//! The changeset transaction layer — the spine every edit flows through.
//!
//! A [`ChangeSet`] is a Helix-style sequence of [`Op`]s that together partition
//! the *old* document end-to-end. Lengths are in **bytes**, matching the
//! byte-indexed [`rope::Rope`] the engine stores. Three pure operations make
//! undo and multicursor fall out naturally:
//!
//! - [`ChangeSet::apply`] mutates a rope.
//! - [`ChangeSet::invert`] produces the inverse changeset for undo.
//! - [`ChangeSet::map_pos`] maps a byte offset through the change.
//!
//! With Zed's byte-indexed rope there is no char↔byte bridge: ops are bytes and
//! the rope is bytes, so `apply` splices byte ranges directly via
//! [`rope::Rope::replace`] and `invert` recovers deleted text with
//! [`rope::Rope::slice`].

use rope::Rope;

/// One change operation. Lengths are in **bytes**.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Op {
    /// Keep the next `n` bytes unchanged.
    Retain(usize),
    /// Delete the next `n` bytes.
    Delete(usize),
    /// Insert this text at the current position.
    Insert(String),
}

/// Which side a position sticks to when it lands exactly on an insert boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Assoc {
    /// Stay to the left of inserted text (the insertion pushes in front).
    Before,
    /// Move to the right, past the inserted text.
    After,
}

/// A complete description of an edit over a document of `len_before` bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChangeSet {
    pub ops: Vec<Op>,
    pub len_before: usize,
}

impl ChangeSet {
    /// The identity changeset over a `len`-byte document (changes nothing).
    pub fn identity(len: usize) -> ChangeSet {
        ChangeSet {
            ops: if len == 0 {
                Vec::new()
            } else {
                vec![Op::Retain(len)]
            },
            len_before: len,
        }
    }

    /// Build a changeset from edits expressed in the **old** coordinate space.
    ///
    /// Each change is `(from, to, inserted_text)`: delete bytes `[from, to)` and
    /// insert `inserted_text` there. `changes` must be sorted by `from` and be
    /// non-overlapping (the caller guarantees this — for multicursor, sort and
    /// merge ranges first). A pure insert has `from == to`; a pure delete has an
    /// empty string.
    pub fn from_changes(len_before: usize, changes: &[(usize, usize, String)]) -> ChangeSet {
        debug_assert!(
            changes.windows(2).all(|w| w[0].1 <= w[1].0),
            "from_changes requires changes sorted by `from` and non-overlapping",
        );
        debug_assert!(
            changes
                .iter()
                .all(|&(from, to, _)| from <= to && to <= len_before),
            "from_changes requires from <= to <= len_before",
        );
        let mut ops = Vec::new();
        let mut last = 0usize;
        for (from, to, text) in changes {
            push_retain(&mut ops, from.saturating_sub(last));
            // Delete before Insert so that `map_pos` of a position inside a
            // replaced region clamps to the deletion's left edge (it reaches the
            // Delete first) rather than overshooting past the inserted text.
            push_delete(&mut ops, to.saturating_sub(*from));
            push_insert(&mut ops, text.clone());
            last = *to;
        }
        push_retain(&mut ops, len_before.saturating_sub(last));
        ChangeSet { ops, len_before }
    }

    /// The byte length of the document *after* this changeset is applied.
    pub fn len_after(&self) -> usize {
        self.ops
            .iter()
            .map(|op| match op {
                Op::Retain(n) => *n,
                Op::Delete(_) => 0,
                Op::Insert(s) => s.len(),
            })
            .sum()
    }

    /// True when applying this changeset would leave the document unchanged.
    pub fn is_identity(&self) -> bool {
        self.ops
            .iter()
            .all(|op| matches!(op, Op::Retain(_)) || matches!(op, Op::Insert(s) if s.is_empty()))
    }

    /// Bytes consumed from the old document (sum of `Retain` + `Delete`).
    /// A well-formed changeset consumes exactly `len_before`.
    fn consumed(&self) -> usize {
        self.ops
            .iter()
            .map(|op| match op {
                Op::Retain(n) | Op::Delete(n) => *n,
                Op::Insert(_) => 0,
            })
            .sum()
    }

    /// Apply this changeset to `text`, mutating it in place.
    ///
    /// Returns `false` and leaves `text` **untouched** if the changeset does not
    /// match the document (wrong length) or any op boundary is not a UTF-8 char
    /// boundary — it validates fully before mutating, so it never corrupts the
    /// rope and never half-applies a stale or malformed changeset.
    pub fn apply(&self, text: &mut Rope) -> bool {
        if !self.is_applicable(text) {
            return false;
        }
        // Validated up front, so this pass mutates all-or-nothing. `pos` indexes
        // the *live* (mutating) rope in bytes: `Retain`/`Insert` advance it;
        // `Delete` does not (the suffix shifts left into it). Each op is a
        // byte-range splice — no char conversion.
        let mut pos = 0usize;
        for op in &self.ops {
            match op {
                Op::Retain(n) => pos += n,
                Op::Delete(n) => text.replace(pos..pos + n, ""),
                Op::Insert(s) => {
                    text.replace(pos..pos, s);
                    pos += s.len();
                }
            }
        }
        true
    }

    /// True when this changeset can be applied to `text` without corrupting it:
    /// `len_before` matches the document, the ops consume exactly `len_before`
    /// bytes, and every op boundary falls on a char boundary of `text`.
    fn is_applicable(&self, text: &Rope) -> bool {
        if text.len() != self.len_before || self.consumed() != self.len_before {
            return false;
        }
        // `pos` walks the *original* `text` in bytes; an op boundary at `pos`
        // corresponds to the live byte position `apply` will splice at (the live
        // suffix from that position is the original suffix from `pos`), so
        // validating boundaries here is equivalent to validating the live splices.
        let mut pos = 0usize;
        for op in &self.ops {
            match op {
                Op::Retain(n) => pos += n,
                Op::Delete(n) => {
                    if !text.is_char_boundary(pos) || !text.is_char_boundary(pos + n) {
                        return false;
                    }
                    pos += n;
                }
                Op::Insert(_) => {
                    if !text.is_char_boundary(pos) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Produce the inverse changeset (post-image → pre-image) for undo.
    ///
    /// `original` must be the document **before** this changeset applied: a
    /// `Delete` carries no text, so its inverse `Insert` recovers the removed
    /// bytes from `original`.
    pub fn invert(&self, original: &Rope) -> ChangeSet {
        let mut ops = Vec::new();
        // `byte_pos` walks the *original* document.
        let mut byte_pos = 0usize;
        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    push_retain(&mut ops, *n);
                    byte_pos += n;
                }
                Op::Delete(n) => {
                    // Recover the removed bytes from the original to re-insert.
                    let end = byte_pos + n;
                    // A forward changeset's deletes must lie within `original`;
                    // a violation means `original` is not the pre-image (a caller
                    // bug). Surface it loudly in debug rather than silently
                    // dropping the recovered text (which would break undo), while
                    // the `if` keeps release builds panic-free.
                    debug_assert!(
                        end <= original.len(),
                        "invert: Delete extent {end} exceeds original length {}",
                        original.len()
                    );
                    if end <= original.len() {
                        push_insert(&mut ops, original.slice(byte_pos..end).to_string());
                    }
                    byte_pos += n;
                }
                Op::Insert(s) => {
                    // The forward insert added bytes absent from the original,
                    // so its inverse deletes them; `byte_pos` does not advance.
                    push_delete(&mut ops, s.len());
                }
            }
        }
        ChangeSet {
            ops,
            len_before: self.len_after(),
        }
    }

    /// Map a byte offset from the old coordinate space to the new one.
    pub fn map_pos(&self, pos: usize, assoc: Assoc) -> usize {
        // Walk the ops keeping two cursors in step: `old_pos` in the old doc,
        // `new_pos` in the new doc.
        let mut old_pos = 0usize;
        let mut new_pos = 0usize;
        for op in &self.ops {
            match op {
                Op::Retain(n) => {
                    if pos < old_pos + n {
                        return new_pos + (pos - old_pos);
                    }
                    old_pos += n;
                    new_pos += n;
                }
                Op::Delete(n) => {
                    if pos < old_pos + n {
                        // Inside a deletion: clamp to its left edge in new space.
                        return new_pos;
                    }
                    old_pos += n;
                }
                Op::Insert(s) => {
                    if pos == old_pos {
                        return new_pos
                            + match assoc {
                                Assoc::After => s.len(),
                                Assoc::Before => 0,
                            };
                    }
                    new_pos += s.len();
                }
            }
        }
        new_pos
    }
}

/// Append a `Retain(n)`, coalescing with a trailing `Retain`.
fn push_retain(ops: &mut Vec<Op>, n: usize) {
    if n == 0 {
        return;
    }
    if let Some(Op::Retain(last)) = ops.last_mut() {
        *last += n;
    } else {
        ops.push(Op::Retain(n));
    }
}

/// Append a `Delete(n)`, coalescing with a trailing `Delete`.
fn push_delete(ops: &mut Vec<Op>, n: usize) {
    if n == 0 {
        return;
    }
    if let Some(Op::Delete(last)) = ops.last_mut() {
        *last += n;
    } else {
        ops.push(Op::Delete(n));
    }
}

/// Append an `Insert(s)`, coalescing with a trailing `Insert`.
fn push_insert(ops: &mut Vec<Op>, s: String) {
    if s.is_empty() {
        return;
    }
    if let Some(Op::Insert(last)) = ops.last_mut() {
        last.push_str(&s);
    } else {
        ops.push(Op::Insert(s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(s: &str) -> Op {
        Op::Insert(s.to_string())
    }

    // ---- apply ----------------------------------------------------------

    #[test]
    fn apply_insert() {
        let mut r = Rope::from("abc");
        let cs = ChangeSet {
            ops: vec![Op::Retain(1), ins("X"), Op::Retain(2)],
            len_before: 3,
        };
        assert!(cs.apply(&mut r));
        assert_eq!(r.to_string(), "aXbc");
    }

    #[test]
    fn apply_delete() {
        let mut r = Rope::from("abc");
        let cs = ChangeSet {
            ops: vec![Op::Retain(1), Op::Delete(1), Op::Retain(1)],
            len_before: 3,
        };
        assert!(cs.apply(&mut r));
        assert_eq!(r.to_string(), "ac");
    }

    #[test]
    fn apply_mixed_replace() {
        let mut r = Rope::from("hello");
        // keep 'h', delete 'e', insert "XY", keep "llo"
        let cs = ChangeSet {
            ops: vec![Op::Retain(1), Op::Delete(1), ins("XY"), Op::Retain(3)],
            len_before: 5,
        };
        assert!(cs.apply(&mut r));
        assert_eq!(r.to_string(), "hXYllo");
    }

    #[test]
    fn apply_at_end() {
        let mut r = Rope::from("abc");
        let cs = ChangeSet {
            ops: vec![Op::Retain(3), ins("Z")],
            len_before: 3,
        };
        assert!(cs.apply(&mut r));
        assert_eq!(r.to_string(), "abcZ");
    }

    #[test]
    fn apply_rejects_stale_changeset() {
        let mut r = Rope::from("abc"); // 3 bytes
        let cs = ChangeSet {
            ops: vec![Op::Retain(5)],
            len_before: 5, // does not match the 3-byte doc
        };
        assert!(!cs.apply(&mut r));
        assert_eq!(r.to_string(), "abc"); // untouched
    }

    #[test]
    fn apply_multibyte_insert_and_delete_stay_char_aligned() {
        // "aé" = a(1) + é(2) = 3 bytes
        let mut r = Rope::from("aé");
        let cs = ChangeSet {
            ops: vec![Op::Retain(1), ins("X"), Op::Retain(2)],
            len_before: 3,
        };
        assert!(cs.apply(&mut r));
        assert_eq!(r.to_string(), "aXé");

        // delete the 2-byte 'é'
        let mut r2 = Rope::from("aé");
        let cs2 = ChangeSet {
            ops: vec![Op::Retain(1), Op::Delete(2)],
            len_before: 3,
        };
        assert!(cs2.apply(&mut r2));
        assert_eq!(r2.to_string(), "a");
    }

    #[test]
    fn apply_identity_is_noop() {
        let mut r = Rope::from("abc");
        assert!(ChangeSet::identity(3).apply(&mut r));
        assert_eq!(r.to_string(), "abc");

        let mut empty = Rope::from("");
        assert!(ChangeSet::identity(0).apply(&mut empty));
        assert_eq!(empty.to_string(), "");
    }

    // ---- invert (round-trip) -------------------------------------------

    fn assert_round_trips(original: &str, ops: Vec<Op>) {
        let pre = Rope::from(original);
        let cs = ChangeSet {
            len_before: pre.len(),
            ops,
        };
        let mut post = pre.clone();
        assert!(cs.apply(&mut post));

        let inverse = cs.invert(&pre);
        assert_eq!(inverse.len_before, cs.len_after());
        let mut restored = post.clone();
        assert!(inverse.apply(&mut restored));
        assert_eq!(restored.to_string(), original);
    }

    #[test]
    fn invert_insert_round_trips() {
        assert_round_trips("abc", vec![Op::Retain(1), ins("X"), Op::Retain(2)]);
    }

    #[test]
    fn invert_delete_round_trips() {
        assert_round_trips("abc", vec![Op::Retain(1), Op::Delete(1), Op::Retain(1)]);
    }

    #[test]
    fn invert_mixed_round_trips() {
        assert_round_trips(
            "hello",
            vec![Op::Retain(1), Op::Delete(1), ins("XY"), Op::Retain(3)],
        );
    }

    #[test]
    fn invert_multibyte_round_trips() {
        // delete the multi-byte 'é', insert an ascii — round trip must restore é
        assert_round_trips(
            "aébc",
            vec![Op::Retain(1), Op::Delete(2), ins("Z"), Op::Retain(2)],
        );
    }

    // ---- map_pos --------------------------------------------------------

    #[test]
    fn map_pos_shifts_after_insert() {
        // retain 2, insert "XY" (2 bytes), retain 3 ; len_before = 5
        let cs = ChangeSet {
            ops: vec![Op::Retain(2), ins("XY"), Op::Retain(3)],
            len_before: 5,
        };
        // a position past the insert shifts right by the inserted length
        assert_eq!(cs.map_pos(4, Assoc::After), 6);
        assert_eq!(cs.map_pos(0, Assoc::After), 0); // before the edit: unchanged
    }

    #[test]
    fn map_pos_assoc_breaks_the_insert_tie() {
        let cs = ChangeSet {
            ops: vec![Op::Retain(2), ins("XY"), Op::Retain(3)],
            len_before: 5,
        };
        assert_eq!(cs.map_pos(2, Assoc::Before), 2); // stays left of inserted text
        assert_eq!(cs.map_pos(2, Assoc::After), 4); // jumps past inserted text
    }

    #[test]
    fn map_pos_clamps_inside_a_deletion() {
        // retain 1, delete 3 (bytes [1,4)), retain 1 ; len_before = 5
        let cs = ChangeSet {
            ops: vec![Op::Retain(1), Op::Delete(3), Op::Retain(1)],
            len_before: 5,
        };
        assert_eq!(cs.map_pos(1, Assoc::After), 1); // left edge
        assert_eq!(cs.map_pos(2, Assoc::After), 1); // interior clamps left
        assert_eq!(cs.map_pos(3, Assoc::Before), 1); // interior clamps left
        assert_eq!(cs.map_pos(4, Assoc::After), 1); // right edge shifts to 1
    }

    #[test]
    fn map_pos_at_document_end() {
        let cs = ChangeSet {
            ops: vec![Op::Retain(3)],
            len_before: 3,
        };
        assert_eq!(cs.map_pos(3, Assoc::After), 3);
    }

    #[test]
    fn map_pos_through_identity_is_unchanged() {
        let cs = ChangeSet::identity(5);
        for p in 0..=5 {
            assert_eq!(cs.map_pos(p, Assoc::Before), p);
            assert_eq!(cs.map_pos(p, Assoc::After), p);
        }
    }

    // ---- from_changes / helpers ----------------------------------------

    #[test]
    fn from_changes_single_insert() {
        let cs = ChangeSet::from_changes(5, &[(2, 2, "XY".to_string())]);
        assert_eq!(cs.ops, vec![Op::Retain(2), ins("XY"), Op::Retain(3)]);
        assert_eq!(cs.len_before, 5);
    }

    #[test]
    fn from_changes_single_delete() {
        let cs = ChangeSet::from_changes(5, &[(1, 3, String::new())]);
        assert_eq!(cs.ops, vec![Op::Retain(1), Op::Delete(2), Op::Retain(2)]);
    }

    #[test]
    fn from_changes_two_inserts_for_multicursor() {
        let cs = ChangeSet::from_changes(5, &[(1, 1, "A".to_string()), (3, 3, "B".to_string())]);
        assert_eq!(
            cs.ops,
            vec![
                Op::Retain(1),
                ins("A"),
                Op::Retain(2),
                ins("B"),
                Op::Retain(2)
            ]
        );
    }

    #[test]
    fn from_changes_replace_applies_correctly() {
        let cs = ChangeSet::from_changes(5, &[(1, 3, "XYZ".to_string())]);
        let mut r = Rope::from("abcde");
        assert!(cs.apply(&mut r));
        assert_eq!(r.to_string(), "aXYZde");
    }

    #[test]
    fn from_changes_replace_emits_delete_before_insert() {
        let cs = ChangeSet::from_changes(5, &[(1, 3, "XYZ".to_string())]);
        assert_eq!(
            cs.ops,
            vec![Op::Retain(1), Op::Delete(2), ins("XYZ"), Op::Retain(2)]
        );
    }

    #[test]
    fn map_pos_inside_replace_clamps_to_deletion_start() {
        // Replace old bytes [1,3) with "XYZ".
        let cs = ChangeSet::from_changes(5, &[(1, 3, "XYZ".to_string())]);
        // A position inside (or at the left edge of) the replaced region clamps
        // to the deletion's left edge in the new space, regardless of Assoc —
        // it must not overshoot past the inserted text.
        assert_eq!(cs.map_pos(1, Assoc::Before), 1);
        assert_eq!(cs.map_pos(2, Assoc::Before), 1);
        assert_eq!(cs.map_pos(2, Assoc::After), 1);
        // A position after the replace shifts by the net delta (+1).
        assert_eq!(cs.map_pos(3, Assoc::After), 4);
    }

    #[test]
    fn apply_rejects_a_midcodepoint_boundary_untouched() {
        // "é" is 2 bytes; an op boundary at byte 1 falls inside the codepoint.
        // The rope is byte-true, so a splice there would corrupt UTF-8 — `apply`
        // validates char boundaries up front and cleanly rejects it.
        let mut r = Rope::from("é");
        let cs = ChangeSet {
            ops: vec![Op::Delete(1), Op::Retain(1)],
            len_before: 2,
        };
        assert!(!cs.apply(&mut r)); // cleanly rejected, never corrupts the rope
        assert_eq!(r.to_string(), "é"); // untouched
    }

    #[test]
    fn len_after_and_is_identity() {
        let cs = ChangeSet {
            ops: vec![Op::Retain(2), ins("XY"), Op::Delete(1)],
            len_before: 3,
        };
        assert_eq!(cs.len_after(), 4);
        assert!(!cs.is_identity());
        assert!(ChangeSet::identity(7).is_identity());
        assert!(ChangeSet::identity(0).is_identity());
    }
}
