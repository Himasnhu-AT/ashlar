//! An app-owned multi-selection for [`VirtualList`](crate::virtual_list::VirtualList) and
//! [`VirtualTable`](crate::virtual_list::VirtualTable): a cursor, an anchor and a set of rows.
//!
//! Keep it in the app's view state (it serializes) and pass it with `.selection(&mut sel)`.
//! Click selects one row, Shift-click and Shift+arrows extend from the anchor, ⌘-click (Ctrl-click
//! off macOS) toggles one row, ⌘A selects every row.
//!
//! The set is stored as sorted, disjoint ranges, so "select all" on ten million rows costs one
//! range, not ten million entries.

use std::ops::Range;

/// A selection of row indices. See the [module docs](self).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Selection {
    /// Sorted, disjoint, non-adjacent, non-empty half-open ranges.
    ranges: Vec<(usize, usize)>,
    anchor: Option<usize>,
    cursor: Option<usize>,
}

impl Selection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `row` is selected (O(log ranges)).
    pub fn contains(&self, row: usize) -> bool {
        let i = self.ranges.partition_point(|r| r.1 <= row);
        self.ranges.get(i).is_some_and(|r| r.0 <= row)
    }
    /// Number of selected rows.
    pub fn len(&self) -> usize {
        self.ranges.iter().fold(0usize, |n, r| n.saturating_add(r.1.saturating_sub(r.0)))
    }
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
    /// The row keyboard navigation moves from (the last row clicked or moved to).
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }
    /// The row Shift ranges start from.
    pub fn anchor(&self) -> Option<usize> {
        self.anchor
    }
    /// The selected rows as sorted ranges.
    pub fn ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.ranges.iter().map(|r| r.0..r.1)
    }
    /// The selected rows, ascending.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.ranges().flatten()
    }

    /// Select only `row` (a plain click or arrow key): it becomes the anchor and the cursor.
    pub fn select_only(&mut self, row: usize) {
        self.ranges.clear();
        self.ranges.push((row, row.saturating_add(1)));
        self.anchor = Some(row);
        self.cursor = Some(row);
    }
    /// Toggle `row` (⌘/Ctrl-click): it becomes the anchor and the cursor.
    pub fn toggle(&mut self, row: usize) {
        if self.contains(row) {
            self.remove(row..row.saturating_add(1));
        } else {
            self.insert(row..row.saturating_add(1));
        }
        self.anchor = Some(row);
        self.cursor = Some(row);
    }
    /// Select the rows from the anchor to `row` (Shift-click, Shift+arrows), replacing the
    /// previous selection; the anchor stays, the cursor moves to `row`.
    pub fn extend_to(&mut self, row: usize) {
        let anchor = self.anchor.unwrap_or(row);
        let (lo, hi) = if anchor <= row { (anchor, row) } else { (row, anchor) };
        self.ranges.clear();
        self.ranges.push((lo, hi.saturating_add(1)));
        self.anchor = Some(anchor);
        self.cursor = Some(row);
    }
    /// Select rows `0..rows` (⌘A); the cursor and anchor stay.
    pub fn select_all(&mut self, rows: usize) {
        self.ranges.clear();
        if rows > 0 {
            self.ranges.push((0, rows));
        }
    }
    /// Deselect everything.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    /// Drop rows at or past `rows` (call when the model shrinks).
    pub fn clamp(&mut self, rows: usize) {
        self.remove(rows..usize::MAX);
        self.anchor = self.anchor.filter(|a| *a < rows);
        self.cursor = self.cursor.filter(|c| *c < rows);
    }

    /// Add a range of rows.
    pub fn insert(&mut self, rows: Range<usize>) {
        if rows.is_empty() {
            return;
        }
        let (mut lo, mut hi) = (rows.start, rows.end);
        // ranges touching or overlapping [lo, hi) merge into it
        let first = self.ranges.partition_point(|r| r.1 < lo);
        let last = self.ranges.partition_point(|r| r.0 <= hi);
        if let Some(r) = self.ranges.get(first..last) {
            if let (Some(a), Some(b)) = (r.first(), r.last()) {
                lo = lo.min(a.0);
                hi = hi.max(b.1);
            }
            self.ranges.splice(first..last, [(lo, hi)]);
        }
    }
    /// Remove a range of rows.
    pub fn remove(&mut self, rows: Range<usize>) {
        if rows.is_empty() {
            return;
        }
        let (lo, hi) = (rows.start, rows.end);
        let first = self.ranges.partition_point(|r| r.1 <= lo);
        let last = self.ranges.partition_point(|r| r.0 < hi);
        let Some(hit) = self.ranges.get(first..last) else { return };
        let mut keep = Vec::with_capacity(2);
        if let Some(a) = hit.first().filter(|a| a.0 < lo) {
            keep.push((a.0, lo));
        }
        if let Some(b) = hit.last().filter(|b| b.1 > hi) {
            keep.push((hi, b.1));
        }
        self.ranges.splice(first..last, keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_merge_split_and_count() {
        let mut s = Selection::new();
        s.insert(5..8);
        s.insert(1..3);
        s.insert(3..5);
        assert_eq!(s.ranges().collect::<Vec<_>>(), vec![1..8]);
        s.remove(4..6);
        assert_eq!(s.ranges().collect::<Vec<_>>(), vec![1..4, 6..8]);
        assert!(s.contains(3) && !s.contains(4) && s.contains(7) && !s.contains(8));
        assert_eq!(s.len(), 5);
        s.toggle(5);
        assert_eq!(s.ranges().collect::<Vec<_>>(), vec![1..4, 5..8]);
        s.toggle(5);
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![1, 2, 3, 6, 7]);
        s.remove(0..usize::MAX);
        assert!(s.is_empty());
    }

    #[test]
    fn extend_keeps_anchor_and_select_all_is_one_range() {
        let mut s = Selection::new();
        s.select_only(10);
        s.extend_to(7);
        assert_eq!((s.anchor(), s.cursor(), s.len()), (Some(10), Some(7), 4));
        s.extend_to(12);
        assert_eq!(s.ranges().collect::<Vec<_>>(), vec![10..13]);
        s.select_all(10_000_000);
        assert_eq!((s.len(), s.ranges().count()), (10_000_000, 1));
        s.clamp(5);
        assert_eq!((s.len(), s.anchor(), s.cursor()), (5, None, None));
        s.insert(usize::MAX - 1..usize::MAX);
        s.toggle(usize::MAX);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Selection>(&json).unwrap(), s);
    }
}
