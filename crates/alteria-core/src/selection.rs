//! The selection model: byte-offset ranges, multicursor-ready.
//!
//! Every cursor is a [`Range`] of byte offsets. `anchor == head` is a bare
//! cursor (no span selected); otherwise the half-open span between them is
//! selected. A [`Selection`] is a non-empty set of ranges with one primary.

/// A single cursor or selection span, as byte offsets into the buffer.
///
/// `anchor` is the fixed end (where a selection was initiated); `head` is the
/// moving end. The span is `[min, max)`. `anchor == head` is a bare cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Range {
    pub anchor: usize,
    pub head: usize,
    /// The remembered visual column for vertical motion (Zed's
    /// `SelectionGoal::Column`). `Some(col)` lets up/down keep their column
    /// across short lines; it is `None` until a vertical motion sets it and is
    /// reset by horizontal motion and edits. A *byte* column (rope columns are
    /// bytes); pixel-x goals are deferred to the frontend.
    pub goal: Option<u32>,
}

impl Range {
    /// A bare cursor at `pos` (`anchor == head == pos`), with no goal column.
    pub fn cursor(pos: usize) -> Self {
        Range {
            anchor: pos,
            head: pos,
            goal: None,
        }
    }

    /// A range from `anchor` to `head` with no goal column. The terse
    /// constructor existing call sites use, so adding `goal` doesn't force every
    /// `Range { .. }` literal to spell the field out.
    pub fn new(anchor: usize, head: usize) -> Self {
        Range {
            anchor,
            head,
            goal: None,
        }
    }

    /// True when nothing is selected (`anchor == head`).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The lower bound of the span.
    pub fn min(&self) -> usize {
        self.anchor.min(self.head)
    }

    /// The upper bound of the span.
    pub fn max(&self) -> usize {
        self.anchor.max(self.head)
    }

    /// True when the two ranges share a position and should merge.
    ///
    /// Two bare cursors at the same offset overlap; a cursor exactly at the
    /// far edge of a span does not (it is a distinct position). Spans overlap
    /// only when they share interior — touching end-to-start does not merge.
    pub fn overlaps(&self, other: &Range) -> bool {
        self.min() == other.min() || (self.max() > other.min() && other.max() > self.min())
    }
}

/// A set of one or more ranges, one of which is primary. One edit applies to
/// every range; motions move every head.
#[derive(Clone, PartialEq, Debug)]
pub struct Selection {
    pub ranges: Vec<Range>,
    pub primary: usize,
}

impl Selection {
    /// A single bare cursor at `pos`.
    pub fn at(pos: usize) -> Self {
        Selection {
            ranges: vec![Range::cursor(pos)],
            primary: 0,
        }
    }

    /// The primary range — the one motions/edits report against.
    pub fn primary(&self) -> Range {
        self.ranges[self.primary]
    }

    /// Sort ranges by position, merge overlaps, and keep `primary` pointing at
    /// the (possibly merged) range that contains the old primary. A merged
    /// range is normalized to forward orientation (M0 simplification).
    pub fn normalize(&mut self) {
        if self.ranges.len() <= 1 {
            return;
        }
        let primary = self.ranges[self.primary];

        let mut sorted = self.ranges.clone();
        sorted.sort_by_key(|r| (r.min(), r.max()));

        let mut merged: Vec<Range> = Vec::with_capacity(sorted.len());
        for r in sorted {
            match merged.last_mut() {
                Some(last) if last.overlaps(&r) => {
                    let new_min = last.min().min(r.min());
                    let new_max = last.max().max(r.max());
                    *last = Range::new(new_min, new_max);
                }
                _ => merged.push(r),
            }
        }

        // Keep the primary: prefer the surviving identical range (so a bare
        // cursor sharing an edge with a span isn't reassigned to the span), and
        // only fall back to containment for a primary that was merged away.
        let new_primary = merged
            .iter()
            .position(|r| *r == primary)
            .or_else(|| {
                merged
                    .iter()
                    .position(|r| r.min() <= primary.min() && primary.max() <= r.max())
            })
            .unwrap_or(0);

        self.ranges = merged;
        self.primary = new_primary;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_cursor_is_empty() {
        let c = Range::cursor(4);
        assert!(c.is_empty());
        assert_eq!(c.min(), 4);
        assert_eq!(c.max(), 4);
    }

    #[test]
    fn min_max_independent_of_orientation() {
        let forward = Range::new(2, 7);
        let backward = Range::new(7, 2);
        assert_eq!((forward.min(), forward.max()), (2, 7));
        assert_eq!((backward.min(), backward.max()), (2, 7));
        assert!(!forward.is_empty());
        assert!(!backward.is_empty());
    }

    #[test]
    fn identical_cursors_overlap() {
        assert!(Range::cursor(3).overlaps(&Range::cursor(3)));
    }

    #[test]
    fn distinct_cursors_do_not_overlap() {
        assert!(!Range::cursor(3).overlaps(&Range::cursor(5)));
    }

    #[test]
    fn interior_overlapping_spans_overlap() {
        let a = Range::new(0, 3);
        let b = Range::new(2, 5);
        assert!(a.overlaps(&b));
        assert!(b.overlaps(&a));
    }

    #[test]
    fn touching_spans_do_not_overlap() {
        let a = Range::new(0, 2);
        let b = Range::new(2, 4);
        assert!(!a.overlaps(&b));
        assert!(!b.overlaps(&a));
    }

    #[test]
    fn cursor_inside_span_overlaps_but_at_far_edge_does_not() {
        let span = Range::new(0, 3);
        assert!(Range::cursor(2).overlaps(&span));
        assert!(span.overlaps(&Range::cursor(2)));
        // A cursor exactly at the far edge is a distinct position.
        assert!(!Range::cursor(3).overlaps(&span));
    }

    #[test]
    fn normalize_single_range_is_noop() {
        let mut s = Selection {
            ranges: vec![Range::new(7, 2)],
            primary: 0,
        };
        let before = s.clone();
        s.normalize();
        assert_eq!(s, before);
    }

    #[test]
    fn normalize_merges_overlapping_and_preserves_primary() {
        let mut s = Selection {
            ranges: vec![Range::new(0, 3), Range::new(2, 5)],
            primary: 1,
        };
        s.normalize();
        assert_eq!(s.ranges.len(), 1);
        assert_eq!(s.ranges[0].min(), 0);
        assert_eq!(s.ranges[0].max(), 5);
        // The merged range still contains the old primary span [2,5].
        let p = s.primary();
        assert!(p.min() <= 2 && 5 <= p.max());
    }

    #[test]
    fn normalize_merges_duplicate_cursors() {
        let mut s = Selection {
            ranges: vec![Range::cursor(4), Range::cursor(4)],
            primary: 0,
        };
        s.normalize();
        assert_eq!(s.ranges.len(), 1);
        assert_eq!(s.ranges[0], Range::cursor(4));
    }

    #[test]
    fn normalize_keeps_a_zero_width_primary_off_a_shared_edge() {
        // A bare primary cursor at 4 sits on the far edge of an earlier span
        // [2,4); it must stay the primary, not be reassigned to the span.
        let mut s = Selection {
            ranges: vec![Range::new(2, 4), Range::cursor(4)],
            primary: 1,
        };
        s.normalize();
        assert_eq!(s.ranges.len(), 2); // they touch but do not overlap
        assert_eq!(s.primary(), Range::cursor(4));
    }

    #[test]
    fn normalize_keeps_distinct_cursors_sorted() {
        let mut s = Selection {
            ranges: vec![Range::cursor(9), Range::cursor(1), Range::cursor(5)],
            primary: 0, // the cursor at 9
        };
        s.normalize();
        assert_eq!(s.ranges.len(), 3);
        assert_eq!(
            s.ranges,
            vec![Range::cursor(1), Range::cursor(5), Range::cursor(9)]
        );
        // primary tracked the cursor that was at 9 (now last).
        assert_eq!(s.primary(), Range::cursor(9));
    }
}
