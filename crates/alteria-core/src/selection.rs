//! The selection model: Zed's `Selection<T>` shape over byte-true [`Anchor`]s.
//!
//! Adopts Zed's `text::Selection<T> { id, start, end, reversed, goal }` shape
//! (`crates/text/src/selection.rs`): `start <= end` always, and `reversed`
//! records which end is the moving **head** (`reversed` ⇒ head at `start`).
//! `start == end` is a bare cursor. Positions are stored as **`Anchor`s** so a
//! selection rides edits made elsewhere for free; an action resolves them to
//! byte offsets against a [`BufferSnapshot`], does its math in offset space
//! (reusing the motion layer), then re-anchors.
//!
//! [`SelectionGoal`] mirrors the shape of Zed's enum but carries only the
//! **byte-column** variant Alteria needs (plan 003): rope columns are bytes and
//! pixel-x goals are deferred to the frontend.
//!
//! A [`Selections`] is the multicursor wrapper — `Vec<Selection<Anchor>>` with a
//! stable `primary` id. One edit applies to every selection; motions move every
//! head.

use text::Anchor;

/// The remembered goal for vertical motion (Zed's `SelectionGoal`). Only the
/// byte-`Column` variant is modelled; rope columns are bytes and pixel-x goals
/// are the frontend's concern (plan 003).
#[derive(Default, Copy, Clone, Debug, PartialEq, Eq)]
pub enum SelectionGoal {
    /// No goal — seed it from the current column on the next vertical motion.
    #[default]
    None,
    /// Keep this byte column across short lines.
    Column(u32),
}

impl SelectionGoal {
    /// The goal as an optional byte column, for the offset-space motion helpers.
    pub fn column(self) -> Option<u32> {
        match self {
            SelectionGoal::None => None,
            SelectionGoal::Column(c) => Some(c),
        }
    }

    /// Build a goal from an optional byte column (the inverse of [`column`]).
    ///
    /// [`column`]: SelectionGoal::column
    pub fn from_column(col: Option<u32>) -> Self {
        match col {
            Some(c) => SelectionGoal::Column(c),
            None => SelectionGoal::None,
        }
    }
}

/// A single cursor or selection span (Zed's `Selection<T>` shape).
///
/// `start <= end` always; `reversed` marks which end is the moving head. `T` is
/// [`Anchor`] in storage and `usize` (byte offset) in the resolved working form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection<T> {
    /// A stable identity, so a selection can be tracked through motions, edits,
    /// and merges (Zed assigns each selection an id).
    pub id: usize,
    /// The lower end of the span.
    pub start: T,
    /// The upper end of the span.
    pub end: T,
    /// `true` when the head is at `start` (the selection was extended leftward).
    pub reversed: bool,
    /// The vertical-motion goal carried with the head.
    pub goal: SelectionGoal,
}

impl<T: Copy> Selection<T> {
    /// The moving end (the cursor).
    pub fn head(&self) -> T {
        if self.reversed {
            self.start
        } else {
            self.end
        }
    }

    /// The fixed end (the anchor the head moved away from).
    pub fn tail(&self) -> T {
        if self.reversed {
            self.end
        } else {
            self.start
        }
    }
}

impl Selection<usize> {
    /// A bare cursor at `pos` with id `id` and no goal.
    pub fn cursor(id: usize, pos: usize) -> Self {
        Selection {
            id,
            start: pos,
            end: pos,
            reversed: false,
            goal: SelectionGoal::None,
        }
    }

    /// True when nothing is selected (`start == end`).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// The lower bound of the span (`start`, since `start <= end`).
    pub fn min(&self) -> usize {
        self.start
    }

    /// The upper bound of the span (`end`).
    pub fn max(&self) -> usize {
        self.end
    }

    /// Move the head to `head`, keeping the tail fixed, and set the new goal —
    /// reordering `start`/`end` and flipping `reversed` as needed (Zed
    /// `Selection::set_head`).
    pub fn set_head(&mut self, head: usize, goal: SelectionGoal) {
        let tail = self.tail();
        if head >= tail {
            self.start = tail;
            self.end = head;
            self.reversed = false;
        } else {
            self.start = head;
            self.end = tail;
            self.reversed = true;
        }
        self.goal = goal;
    }
}

/// Sort `selections` by position and merge any that overlap, returning the
/// surviving id for the selection that carried `primary_id` (or the range that
/// swallowed it). A merged span is normalized to forward orientation with no
/// goal (an M0 simplification, matching the pre-anchor model); standalone
/// selections keep their orientation, id, and goal.
///
/// Operates in **offset space** (`Selection<usize>`) because anchors only order
/// against a snapshot; the caller resolves first, then re-anchors the result.
pub fn normalize(selections: &mut Vec<Selection<usize>>, primary_id: usize) -> usize {
    if selections.len() <= 1 {
        return primary_id;
    }
    // The span the old primary covered, to re-find it after merging.
    let primary_span = selections
        .iter()
        .find(|s| s.id == primary_id)
        .map(|s| (s.min(), s.max()));

    selections.sort_by_key(|s| (s.min(), s.max()));

    let mut merged: Vec<Selection<usize>> = Vec::with_capacity(selections.len());
    for s in selections.drain(..) {
        match merged.last_mut() {
            Some(last) if overlaps(last, &s) => {
                // Grow the survivor to the union and keep it forward; prefer the
                // primary's id so the merged range stays primary if it ate it.
                let new_min = last.min().min(s.min());
                let new_max = last.max().max(s.max());
                let keep_id = if s.id == primary_id { s.id } else { last.id };
                last.id = keep_id;
                last.start = new_min;
                last.end = new_max;
                last.reversed = false;
                last.goal = SelectionGoal::None;
            }
            _ => merged.push(s),
        }
    }

    // Keep the primary: prefer the surviving identical id, else the range that
    // now contains the old primary's span, else the first.
    let new_primary = merged
        .iter()
        .find(|s| s.id == primary_id)
        .map(|s| s.id)
        .or_else(|| {
            primary_span.and_then(|(lo, hi)| {
                merged
                    .iter()
                    .find(|s| s.min() <= lo && hi <= s.max())
                    .map(|s| s.id)
            })
        })
        .unwrap_or_else(|| merged[0].id);

    *selections = merged;
    new_primary
}

/// True when two resolved selections share a position and should merge: two
/// bare cursors coincide, or two spans share interior. Touching end-to-start
/// does not merge; a bare cursor on a span's far edge is a distinct position.
fn overlaps(a: &Selection<usize>, b: &Selection<usize>) -> bool {
    a.min() == b.min() || (a.max() > b.min() && b.max() > a.min())
}

/// The multicursor set: anchored selections with one stable primary.
///
/// Anchors are resolved to offsets at the start of an action and rebuilt from
/// offsets at the end (see `buffer`), so this type stays a plain container —
/// all ordering/merging happens in offset space via [`normalize`].
#[derive(Clone, Debug)]
pub struct Selections {
    /// Every cursor/span, kept sorted and non-overlapping after each action.
    pub selections: Vec<Selection<Anchor>>,
    /// The id of the primary selection (the one motions/edits report against).
    primary_id: usize,
    /// The next id to hand out, so every selection gets a unique stable id.
    next_id: usize,
}

impl Selections {
    /// A set holding the single anchored `cursor`, made primary.
    pub fn single(cursor: Selection<Anchor>) -> Self {
        let primary_id = cursor.id;
        Selections {
            selections: vec![cursor],
            primary_id,
            next_id: primary_id + 1,
        }
    }

    /// The primary selection (falls back to the first if the id went missing).
    pub fn primary(&self) -> &Selection<Anchor> {
        self.selections
            .iter()
            .find(|s| s.id == self.primary_id)
            .unwrap_or(&self.selections[0])
    }

    /// The id of the primary selection.
    pub fn primary_id(&self) -> usize {
        self.primary_id
    }

    /// Hand out a fresh, unique selection id (for spawning a cursor).
    pub fn alloc_id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Replace the anchored selections wholesale (after re-anchoring), recording
    /// the new primary id. `next_id` is preserved so ids stay unique.
    pub fn replace(&mut self, selections: Vec<Selection<Anchor>>, primary_id: usize) {
        debug_assert!(!selections.is_empty(), "a Selections is never empty");
        self.next_id = self.next_id.max(
            selections
                .iter()
                .map(|s| s.id + 1)
                .max()
                .unwrap_or(self.next_id),
        );
        self.selections = selections;
        self.primary_id = primary_id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(id: usize, start: usize, end: usize, reversed: bool) -> Selection<usize> {
        Selection {
            id,
            start,
            end,
            reversed,
            goal: SelectionGoal::None,
        }
    }

    #[test]
    fn head_and_tail_follow_reversed() {
        let forward = sel(0, 2, 7, false);
        assert_eq!((forward.tail(), forward.head()), (2, 7));
        let backward = sel(0, 2, 7, true);
        assert_eq!((backward.tail(), backward.head()), (7, 2));
    }

    #[test]
    fn cursor_is_empty() {
        let c = Selection::cursor(0, 4);
        assert!(c.is_empty());
        assert_eq!((c.min(), c.max()), (4, 4));
    }

    #[test]
    fn set_head_extends_forward_then_flips_backward() {
        let mut s = Selection::cursor(0, 3);
        s.set_head(5, SelectionGoal::None);
        assert_eq!((s.start, s.end, s.reversed), (3, 5, false));
        // Pull the head back past the tail: the orientation flips.
        s.set_head(1, SelectionGoal::None);
        assert_eq!((s.start, s.end, s.reversed), (1, 3, true));
    }

    #[test]
    fn set_head_records_goal() {
        let mut s = Selection::cursor(0, 0);
        s.set_head(2, SelectionGoal::Column(9));
        assert_eq!(s.goal, SelectionGoal::Column(9));
    }

    #[test]
    fn goal_round_trips_through_column() {
        assert_eq!(
            SelectionGoal::from_column(Some(7)),
            SelectionGoal::Column(7)
        );
        assert_eq!(SelectionGoal::Column(7).column(), Some(7));
        assert_eq!(SelectionGoal::None.column(), None);
        assert_eq!(SelectionGoal::from_column(None), SelectionGoal::None);
    }

    #[test]
    fn normalize_single_is_noop() {
        let mut v = vec![sel(0, 7, 2, false)];
        assert_eq!(normalize(&mut v, 0), 0);
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn normalize_merges_overlapping_and_keeps_primary() {
        let mut v = vec![sel(0, 0, 3, false), sel(1, 2, 5, false)];
        let p = normalize(&mut v, 1);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].min(), v[0].max()), (0, 5));
        // The merged range carries the primary id (1 swallowed 0).
        assert_eq!(p, 1);
        assert_eq!(v[0].id, 1);
    }

    #[test]
    fn normalize_merges_duplicate_cursors() {
        let mut v = vec![Selection::cursor(0, 4), Selection::cursor(1, 4)];
        normalize(&mut v, 0);
        assert_eq!(v.len(), 1);
        assert!(v[0].is_empty() && v[0].min() == 4);
    }

    #[test]
    fn normalize_keeps_a_cursor_off_a_shared_edge() {
        // A bare cursor at 4 on the far edge of [2,4) stays distinct.
        let mut v = vec![sel(0, 2, 4, false), Selection::cursor(1, 4)];
        let p = normalize(&mut v, 1);
        assert_eq!(v.len(), 2);
        assert_eq!(p, 1);
    }

    #[test]
    fn normalize_keeps_distinct_cursors_sorted() {
        let mut v = vec![
            Selection::cursor(0, 9),
            Selection::cursor(1, 1),
            Selection::cursor(2, 5),
        ];
        let p = normalize(&mut v, 0);
        assert_eq!(v.iter().map(|s| s.min()).collect::<Vec<_>>(), vec![1, 5, 9]);
        assert_eq!(p, 0); // primary still the cursor that was at 9
    }
}
