//! The Bad-sector map: which sectors of a Source are known to be unreadable.

use std::collections::BTreeMap;
use std::ops::Range;

/// Set of unreadable sector ranges, kept merged and sorted.
///
/// Ranges are in logical sectors, half-open. Known-bad sectors are never re-read unless the
/// caller explicitly asks for retries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BadSectorMap {
    /// start -> end (exclusive); ranges never overlap or touch.
    ranges: BTreeMap<u64, u64>,
}

impl BadSectorMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks `sectors` as unreadable, merging with neighbouring ranges.
    pub fn mark(&mut self, sectors: Range<u64>) {
        if sectors.start >= sectors.end {
            return;
        }
        let mut start = sectors.start;
        let mut end = sectors.end;
        // Absorb a range that starts before `start` and reaches it.
        if let Some((&s, &e)) = self.ranges.range(..=start).next_back()
            && e >= start
        {
            start = s;
            end = end.max(e);
        }
        // Absorb every range that starts inside [start, end].
        let absorbed: Vec<u64> = self.ranges.range(start..=end).map(|(&s, _)| s).collect();
        for s in absorbed {
            if let Some(e) = self.ranges.remove(&s) {
                end = end.max(e);
            }
        }
        self.ranges.insert(start, end);
    }

    /// Removes `sectors` from the map (used when an explicit retry succeeds).
    pub fn clear(&mut self, sectors: Range<u64>) {
        if sectors.start >= sectors.end {
            return;
        }
        // Whole stored ranges (not clipped) that intersect `sectors`.
        let overlapping: Vec<(u64, u64)> = self
            .ranges
            .range(..sectors.end)
            .filter(|(_, e)| **e > sectors.start)
            .map(|(&s, &e)| (s, e))
            .collect();
        for (s, e) in overlapping {
            self.ranges.remove(&s);
            if s < sectors.start {
                self.ranges.insert(s, sectors.start);
            }
            if e > sectors.end {
                self.ranges.insert(sectors.end, e);
            }
        }
    }

    pub fn is_bad(&self, sector: u64) -> bool {
        self.ranges
            .range(..=sector)
            .next_back()
            .is_some_and(|(_, &e)| sector < e)
    }

    /// Bad ranges intersecting `sectors`, clipped to it, in ascending order.
    pub fn overlapping(&self, sectors: Range<u64>) -> impl Iterator<Item = Range<u64>> + '_ {
        let first = self
            .ranges
            .range(..=sectors.start)
            .next_back()
            .filter(|(_, e)| **e > sectors.start)
            .map(|(&s, _)| s)
            .unwrap_or(sectors.start);
        // `BTreeMap::range` panics on an inverted range; an empty query yields nothing.
        let hi = sectors.end.max(first);
        self.ranges
            .range(first..hi)
            .map(move |(&s, &e)| s.max(sectors.start)..e.min(sectors.end))
            .filter(|r| r.start < r.end)
    }

    /// Total number of bad sectors.
    pub fn bad_sectors(&self) -> u64 {
        self.ranges.iter().map(|(s, e)| e - s).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// All bad ranges in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = Range<u64>> + '_ {
        self.ranges.iter().map(|(&s, &e)| s..e)
    }
}

impl FromIterator<Range<u64>> for BadSectorMap {
    fn from_iter<I: IntoIterator<Item = Range<u64>>>(iter: I) -> Self {
        let mut map = Self::new();
        for r in iter {
            map.mark(r);
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(m: &BadSectorMap) -> Vec<Range<u64>> {
        m.iter().collect()
    }

    #[test]
    fn merges_overlapping_and_adjacent_ranges() {
        let mut m = BadSectorMap::new();
        m.mark(10..12);
        m.mark(20..22);
        m.mark(12..13); // adjacent to the first
        assert_eq!(ranges(&m), vec![10..13, 20..22]);
        m.mark(11..21); // bridges both
        assert_eq!(ranges(&m), vec![10..22]);
        assert_eq!(m.bad_sectors(), 12);
    }

    #[test]
    fn membership_and_clipped_overlap() {
        let m: BadSectorMap = [5..8, 100..101].into_iter().collect();
        assert!(!m.is_bad(4));
        assert!(m.is_bad(5));
        assert!(m.is_bad(7));
        assert!(!m.is_bad(8));
        assert_eq!(m.overlapping(6..101).collect::<Vec<_>>(), vec![6..8, 100..101]);
        assert_eq!(m.overlapping(8..100).count(), 0);
    }

    #[test]
    fn clear_splits_ranges() {
        let mut m: BadSectorMap = std::iter::once(0..10).collect();
        m.clear(3..5);
        assert_eq!(ranges(&m), vec![0..3, 5..10]);
        m.clear(0..10);
        assert!(m.is_empty());
    }

    #[test]
    fn empty_ranges_are_ignored() {
        let mut m = BadSectorMap::new();
        m.mark(5..5);
        m.clear(1..1);
        assert!(m.is_empty());
    }
}
