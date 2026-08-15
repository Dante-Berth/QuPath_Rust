//! Decoded tiles, kept under a hard byte budget.
//!
//! `docs/VIEWER.md` §4 names texture memory as one of the two things that will
//! bite: a viewer that uploads every tile it sees and never lets go dies within
//! about ten seconds of panning. The budget belongs here rather than in the
//! render loop, and it is a **hard** budget — [`TileCache::bytes`] never exceeds
//! [`TileCache::budget`] after any operation, which is the property
//! `docs/VIEWER.md` §6 asks to be asserted on a scripted pan.
//!
//! Generic over the value so the library needs no GPU: the window stores a
//! `wgpu::BindGroup` per tile and the tests store nothing at all, and both go
//! through the same eviction arithmetic.
//!
//! # Cost
//!
//! Eviction scans every resident entry to find the least recently used one.
//! That is O(n) per eviction with n bounded by `budget / tile_bytes` — about a
//! thousand entries at a 256 MB budget and 256 KB tiles — and a pan evicts a
//! few tens of entries per frame. A heap ordered by use would be asymptotically
//! better and is not worth the code at this size; `ROADMAP.md` §5's rule about
//! measuring before optimising cuts both ways.

use std::collections::HashMap;

use crate::coords::TileIndex;

/// Decoded tiles, evicted least-recently-used first to stay under a budget.
#[derive(Debug)]
pub struct TileCache<T> {
    budget: usize,
    bytes: usize,
    /// Monotonic use counter. Wrapping it would take 2^64 touches.
    clock: u64,
    entries: HashMap<TileIndex, Entry<T>>,
    evicted: u64,
}

#[derive(Debug)]
struct Entry<T> {
    value: T,
    bytes: usize,
    used: u64,
}

impl<T> TileCache<T> {
    /// An empty cache holding at most `budget` bytes.
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            clock: 0,
            entries: HashMap::new(),
            evicted: 0,
        }
    }

    /// The byte budget this cache was built with.
    #[must_use]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Bytes currently resident. Never exceeds [`TileCache::budget`].
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// How many tiles are resident.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is resident.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many tiles have been evicted since this cache was built.
    ///
    /// A pan that keeps revisiting the same region should not raise this once
    /// the working set fits; if it climbs steadily at a fixed viewport the
    /// budget is smaller than one frame needs.
    #[must_use]
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Whether `tile` is resident, without counting as a use.
    ///
    /// This is what the render loop asks before deciding whether to request a
    /// tile. It deliberately does not touch the entry: a tile that is merely
    /// *asked about* every frame must not be kept alive by the asking.
    #[must_use]
    pub fn contains(&self, tile: TileIndex) -> bool {
        self.entries.contains_key(&tile)
    }

    /// The value for `tile`, without counting as a use.
    ///
    /// Paired with [`TileCache::touch`]: the render loop touches while it still
    /// has `&mut` and reads back with this while the render pass holds the
    /// cache immutably.
    #[must_use]
    pub fn peek(&self, tile: TileIndex) -> Option<&T> {
        self.entries.get(&tile).map(|entry| &entry.value)
    }

    /// Marks `tile` as used now, returning whether it was resident.
    #[must_use]
    pub fn touch(&mut self, tile: TileIndex) -> bool {
        self.clock += 1;
        let clock = self.clock;
        if let Some(entry) = self.entries.get_mut(&tile) {
            entry.used = clock;
            true
        } else {
            false
        }
    }

    /// Stores `tile`, evicting least-recently-used entries to stay in budget.
    ///
    /// Returns whether it was stored. A single tile larger than the whole
    /// budget is **refused** rather than stored over budget or allowed to evict
    /// everything and still not fit — a caller that hits this has a budget too
    /// small to draw anything, and silently emptying the cache every frame
    /// would hide that.
    pub fn insert(&mut self, tile: TileIndex, value: T, bytes: usize) -> bool {
        if bytes > self.budget {
            return false;
        }

        // Replacing an entry frees its bytes first, or a re-decode of the same
        // tile would double-count and evict its own neighbours.
        if let Some(previous) = self.entries.remove(&tile) {
            self.bytes -= previous.bytes;
        }

        while self.bytes + bytes > self.budget {
            let Some(&oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(tile, _)| tile)
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.bytes;
                self.evicted += 1;
            }
        }

        self.clock += 1;
        self.bytes += bytes;
        self.entries.insert(
            tile,
            Entry {
                value,
                bytes,
                used: self.clock,
            },
        );
        true
    }

    /// Drops everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 256x256 RGBA, which is what a real tile costs on the GPU.
    const TILE: usize = 256 * 256 * 4;

    fn tile(column: u32, row: u32) -> TileIndex {
        TileIndex::new(column, row, 0)
    }

    #[test]
    fn an_empty_cache_holds_nothing() {
        let cache: TileCache<()> = TileCache::new(TILE * 4);
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
        assert!(!cache.contains(tile(0, 0)));
    }

    #[test]
    fn resident_bytes_never_exceed_the_budget() {
        // The property the whole module exists for. Four tiles fit; the fifth
        // must push one out rather than take the cache over.
        let mut cache = TileCache::new(TILE * 4);
        for column in 0..64 {
            assert!(cache.insert(tile(column, 0), (), TILE));
            assert!(
                cache.bytes() <= cache.budget(),
                "after {} inserts: {} bytes against a {} budget",
                column + 1,
                cache.bytes(),
                cache.budget()
            );
        }
        assert_eq!(cache.len(), 4);
        assert_eq!(cache.evicted(), 60);
    }

    #[test]
    fn the_least_recently_used_tile_is_the_one_that_goes() {
        // Not the oldest *inserted* — the oldest *used*. A viewer holds a few
        // tiles on screen across many frames while others scroll past, and
        // evicting by insertion order would throw away exactly the ones being
        // looked at.
        let mut cache = TileCache::new(TILE * 3);
        for column in 0..3 {
            assert!(cache.insert(tile(column, 0), column, TILE));
        }

        // Use the oldest two, leaving tile 2 as the least recently used.
        assert!(cache.touch(tile(0, 0)));
        assert!(cache.touch(tile(1, 0)));

        assert!(cache.insert(tile(3, 0), 3, TILE));
        assert!(cache.contains(tile(0, 0)));
        assert!(cache.contains(tile(1, 0)));
        assert!(!cache.contains(tile(2, 0)), "the used tiles were evicted");
        assert!(cache.contains(tile(3, 0)));
    }

    #[test]
    fn asking_whether_a_tile_is_resident_does_not_keep_it_alive() {
        // `contains` is called for every visible tile every frame, including
        // for tiles that are about to be requested. If that counted as a use,
        // eviction order would be driven by the question rather than the answer
        // and the cache would keep whatever was asked about last.
        let mut cache = TileCache::new(TILE * 2);
        assert!(cache.insert(tile(0, 0), (), TILE));
        assert!(cache.insert(tile(1, 0), (), TILE));

        for _ in 0..10 {
            assert!(cache.contains(tile(0, 0)));
            assert!(cache.peek(tile(0, 0)).is_some());
        }
        assert!(cache.touch(tile(1, 0)));

        // Tile 0 was asked about ten times and used never, so it is the one
        // that goes.
        assert!(cache.insert(tile(2, 0), (), TILE));
        assert!(!cache.contains(tile(0, 0)));
        assert!(cache.contains(tile(1, 0)));
    }

    #[test]
    fn re_inserting_a_tile_does_not_double_count_its_bytes() {
        // A tile can be decoded twice — evicted, then requested again while a
        // stale copy is still in flight. Counting the second copy on top of the
        // first would leak budget until the cache held nothing at all.
        let mut cache = TileCache::new(TILE * 4);
        for _ in 0..20 {
            assert!(cache.insert(tile(0, 0), (), TILE));
        }
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes(), TILE);
        assert_eq!(cache.evicted(), 0);
    }

    #[test]
    fn a_tile_bigger_than_the_budget_is_refused_rather_than_emptying_the_cache() {
        let mut cache = TileCache::new(TILE * 2);
        assert!(cache.insert(tile(0, 0), (), TILE));
        assert!(!cache.insert(tile(1, 0), (), TILE * 3));
        assert!(cache.contains(tile(0, 0)), "the refused insert evicted");
        assert_eq!(cache.bytes(), TILE);
    }

    #[test]
    fn a_pan_across_a_level_holds_its_working_set() {
        // The shape of the real workload: a window's worth of tiles moves one
        // column at a time across a level. With a budget a few frames deep, the
        // tiles still on screen must survive the pan — otherwise every frame
        // re-decodes what the last one drew, which is the thrash the budget is
        // supposed to prevent rather than cause.
        const ACROSS: u32 = 5;
        const DOWN: u32 = 4;
        let frame = (ACROSS * DOWN) as usize;

        let mut cache = TileCache::new(TILE * frame * 3);
        let mut decoded = 0_u32;
        for step in 0..40 {
            for row in 0..DOWN {
                for column in step..step + ACROSS {
                    let tile = tile(column, row);
                    if !cache.touch(tile) {
                        decoded += 1;
                        assert!(cache.insert(tile, (), TILE));
                    }
                }
            }
            assert!(cache.bytes() <= cache.budget());
        }

        // 5x4 for the first frame, then one fresh column of 4 per step.
        assert_eq!(decoded, ACROSS * DOWN + 39 * DOWN);
    }
}
