//! Hierarchical (bucket) priority queue for the watershed flood.
//!
//! QuPath uses `PriorityQueue<PixelWithValue>`, ordered by value descending and
//! breaking ties on an insertion counter so that plateaus flood FIFO. Profiling
//! puts `siftDownComparable` and `compareTo` at 29.4% of cell-detection self
//! time (`docs/profiles/README.md`), and QuPath's own source carries the
//! comment "profiling indicates that the many comparisons are the slowest part
//! of the algorithm".
//!
//! That ordering does not need a comparison heap. Values are quantised into a
//! fixed number of levels, each holding a FIFO of packed pixel indices, and the
//! flood always pops from the highest non-empty level:
//!
//! - **No comparator.** Priority is the bucket index; there is nothing to sift.
//! - **No allocation per pixel.** A pixel is one `u32`, not a ~32-byte heap
//!   object, so a queued pixel costs 4 bytes instead of 32.
//! - **FIFO tie-breaking is structural** rather than bought with a counter
//!   field, which is what QuPath's `count` exists to emulate.
//!
//! Operations are O(1) amortised, against O(log n) for the heap.

/// Number of quantisation levels. The watershed input is an EDM, whose useful
/// dynamic range is small; 4096 levels keeps the quantisation error far below
/// the spacing between distinct EDM values while the level array stays under
/// 32 KB and therefore L1-resident.
pub const LEVELS: usize = 4096;

/// A pixel queued for flooding, packed as `y * width + x`.
pub type PixelIndex = u32;

/// Priority queue keyed on a quantised pixel value, FIFO within a level.
///
/// Pops in descending value order, matching QuPath's `PixelWithValue`
/// comparator, with insertion order preserved for equal values.
pub struct HierarchicalQueue {
    /// One FIFO per level; `levels[i]` holds pixels quantised to level `i`.
    levels: Vec<Vec<PixelIndex>>,
    /// Read cursor per level, so popping is a bump rather than a `remove(0)`.
    heads: Vec<usize>,
    /// Highest level index that may be non-empty. Never increases while
    /// flooding, because the flood proceeds from high values to low.
    top: usize,
    /// Number of pixels currently queued.
    len: usize,
    /// Value mapped to the highest level.
    max_value: f32,
    /// Value mapped to level zero.
    min_value: f32,
}

impl HierarchicalQueue {
    /// Creates a queue quantising values over `[min_value, max_value]`.
    ///
    /// Values outside the range are clamped. `capacity` pre-sizes the level
    /// FIFOs to avoid reallocation during the flood.
    #[must_use]
    pub fn new(min_value: f32, max_value: f32, capacity: usize) -> Self {
        // A degenerate range would make every value quantise to the same level,
        // which is correct but slow; it is not an error.
        let per_level = (capacity / LEVELS).max(4);
        Self {
            levels: (0..LEVELS).map(|_| Vec::with_capacity(per_level)).collect(),
            heads: vec![0; LEVELS],
            top: LEVELS - 1,
            len: 0,
            max_value,
            min_value,
        }
    }

    /// Maps a value to its level: `max_value` to `LEVELS - 1`, `min_value` to 0.
    #[must_use]
    fn level_of(&self, value: f32) -> usize {
        let span = self.max_value - self.min_value;
        if span <= 0.0 {
            return LEVELS - 1;
        }
        let t = (value - self.min_value) / span;
        // `t` is clamped before the cast, so the truncation is bounded.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to [0, LEVELS-1] immediately above the cast"
        )]
        let level = (t.clamp(0.0, 1.0) * (LEVELS - 1) as f32) as usize;
        level.min(LEVELS - 1)
    }

    /// Queues `pixel` at the level for `value`.
    pub fn push(&mut self, pixel: PixelIndex, value: f32) {
        let level = self.level_of(value);
        self.levels[level].push(pixel);
        self.len += 1;
        // Flooding descends, but the initial seeding is in raster order, so a
        // push can land above the current cursor.
        if level > self.top {
            self.top = level;
        }
    }

    /// Removes and returns the highest-valued queued pixel, FIFO within a level.
    pub fn pop(&mut self) -> Option<PixelIndex> {
        if self.len == 0 {
            return None;
        }
        // Walk down to the highest non-empty level. Each level is passed at
        // most once per flood, so this loop is O(LEVELS) in total, not per pop.
        loop {
            let head = self.heads[self.top];
            if head < self.levels[self.top].len() {
                self.heads[self.top] = head + 1;
                self.len -= 1;
                return Some(self.levels[self.top][head]);
            }
            // Level exhausted; release it and descend.
            self.levels[self.top].clear();
            self.heads[self.top] = 0;
            if self.top == 0 {
                return None;
            }
            self.top -= 1;
        }
    }

    /// Returns the number of queued pixels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no pixels are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pops_in_descending_value_order() {
        let mut q = HierarchicalQueue::new(0.0, 10.0, 16);
        q.push(1, 3.0);
        q.push(2, 9.0);
        q.push(3, 5.0);
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn equal_values_pop_fifo() {
        // This is the property QuPath buys with its `count` field: plateaus must
        // flood in insertion order, or the watershed lines land differently.
        let mut q = HierarchicalQueue::new(0.0, 1.0, 16);
        for pixel in 0..8 {
            q.push(pixel, 0.5);
        }
        for expected in 0..8 {
            assert_eq!(q.pop(), Some(expected));
        }
    }

    #[test]
    fn interleaved_push_and_pop_keeps_order() {
        // The flood pushes neighbours while draining, so pushes below the
        // cursor must still be seen.
        let mut q = HierarchicalQueue::new(0.0, 10.0, 16);
        q.push(1, 8.0);
        assert_eq!(q.pop(), Some(1));
        q.push(2, 4.0);
        q.push(3, 6.0);
        assert_eq!(q.pop(), Some(3));
        assert_eq!(q.pop(), Some(2));
        assert!(q.is_empty());
    }

    #[test]
    fn values_outside_the_range_are_clamped() {
        let mut q = HierarchicalQueue::new(0.0, 1.0, 16);
        q.push(1, -5.0);
        q.push(2, 5.0);
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), Some(1));
    }

    #[test]
    fn len_tracks_push_and_pop() {
        let mut q = HierarchicalQueue::new(0.0, 1.0, 16);
        assert!(q.is_empty());
        q.push(1, 0.5);
        q.push(2, 0.5);
        assert_eq!(q.len(), 2);
        q.pop();
        assert_eq!(q.len(), 1);
        q.pop();
        assert!(q.is_empty());
    }

    #[test]
    fn degenerate_range_still_pops_everything_fifo() {
        let mut q = HierarchicalQueue::new(3.0, 3.0, 8);
        q.push(1, 3.0);
        q.push(2, 3.0);
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), None);
    }
}
