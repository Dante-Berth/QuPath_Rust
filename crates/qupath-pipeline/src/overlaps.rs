//! Resolving detections that two tiles both found, which is what makes a tiled
//! run's object count mean anything.
//!
//! A tile is padded by [`crate::Params::tile_overlap`] on every side that has a
//! neighbour, so a nucleus sitting in that band is detected **twice** — once by
//! each tile, and in general at two different sizes, because one of the tiles
//! may have clipped it at the far edge of its halo. `ParallelTileObject`
//! resolves that after every tile is done:
//!
//! ```java
//! double intersectionArea = intersection.getArea();
//! double secondArea = secondGeometry.getArea();
//! double threshold = 0.1;
//! if (firstArea >= secondArea) {
//!     if (intersectionArea / secondArea > threshold) {
//!         second.removeChildObject(secondObject);
//!     }
//! } else {
//!     if (intersectionArea / firstArea > threshold) {
//!         first.removeChildObject(firstObject);
//!         break;
//!     }
//! }
//! ```
//!
//! # The four things this rule is not
//!
//! - **It is not centroid ownership.** ROADMAP §2 proposed keeping "only objects
//!   whose centroid falls in the unpadded core", which is a different algorithm
//!   with different output: it drops an object whose centroid lands just outside
//!   the core even when that tile saw it whole and the neighbour clipped it.
//!   QuPath keeps the **larger** of the two, which is what makes a halo work at
//!   all — the tile that saw the whole nucleus is the one that wins.
//! - **It is not a merge.** The loser is deleted outright. QuPath's own comment
//!   on `getTileOverlap` says so, and says why: merged objects would need
//!   measurements recomputed, which the plugin cannot do generically.
//! - **The 10% is relative to the object being removed**, not to the union or to
//!   the larger one. Two nuclei that genuinely abut across a seam overlap by a
//!   few percent of the smaller and both survive; the same nucleus found twice
//!   overlaps by nearly all of it and one goes.
//! - **The comparison is restricted to the band.** Only objects whose bounding
//!   box meets the two tiles' shared rectangle are compared, so an object deep
//!   inside a core is never a candidate for removal.
//!
//! # Why a pixel mask rather than a polygon
//!
//! QuPath intersects JTS geometries and takes the area. These polygons are the
//! Wand's outlines of filled pixel runs, so the polygon area is exactly the
//! filled pixel count and the intersection's area is exactly the count of pixels
//! both cover. Counting shared pixels is therefore the same number, not an
//! approximation of it, and it needs no geometry library.

use crate::Region;
use crate::objects::Extent;

/// The fraction of the removed object that must be covered before it is
/// removed: `double threshold = 0.1;`.
const OVERLAP_THRESHOLD: f64 = 0.1;

/// One surviving candidate, in the region's coordinates.
///
/// Built only for candidates that reach a tile seam. Everything deeper inside a
/// core cannot conflict with anything, so it never gets a mask.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Which tile of the grid found it.
    pub tile: u32,
    /// Bounding box in the region's coordinates.
    pub bounds: Region,
    /// `geometry.getArea()`: pixels the filled region covers.
    pub area: u32,
    /// One bit per pixel of [`Candidate::bounds`], row-major, set where the
    /// filled region covers it.
    mask: Vec<u64>,
    /// Cleared when another tile's copy of this object won.
    alive: bool,
}

impl Candidate {
    /// Builds a candidate from one tile's region map.
    ///
    /// `extent` is in the tile's coordinates and `origin` is where the tile
    /// sits in the region, so the result is in the region's coordinates.
    ///
    /// # Panics
    ///
    /// Panics if `extent` leaves the tile.
    #[must_use]
    pub fn new(
        tile: u32,
        index: u32,
        extent: Extent,
        regions: &[u32],
        width: usize,
        origin: (u32, u32),
    ) -> Self {
        let (w, h) = (extent.width as usize, extent.height as usize);
        let mut mask = vec![0u64; (w * h).div_ceil(64)];
        for row in 0..h {
            let line = (extent.y as usize + row) * width + extent.x as usize;
            for column in 0..w {
                if regions[line + column] == index {
                    let bit = row * w + column;
                    mask[bit / 64] |= 1 << (bit % 64);
                }
            }
        }
        Self {
            tile,
            bounds: Region {
                x: origin.0 + extent.x,
                y: origin.1 + extent.y,
                width: extent.width,
                height: extent.height,
            },
            area: extent.area,
            mask,
            alive: true,
        }
    }

    /// Whether this candidate survived resolution.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.alive
    }

    /// Whether the pixel at a region coordinate is inside the filled region.
    fn covers(&self, x: u32, y: u32) -> bool {
        let column = (x - self.bounds.x) as usize;
        let row = (y - self.bounds.y) as usize;
        let bit = row * self.bounds.width as usize + column;
        self.mask[bit / 64] & (1 << (bit % 64)) != 0
    }

    /// Pixels both candidates cover: `firstGeometry.intersection(second).getArea()`.
    #[must_use]
    pub fn intersection_area(&self, other: &Self) -> u32 {
        let Some(shared) = intersect(self.bounds, other.bounds) else {
            return 0;
        };
        let mut count = 0;
        for y in shared.y..shared.y + shared.height {
            for x in shared.x..shared.x + shared.width {
                if self.covers(x, y) && other.covers(x, y) {
                    count += 1;
                }
            }
        }
        count
    }
}

/// Removes every detection a neighbouring tile found a better copy of,
/// returning how many went.
///
/// `tiles` are the padded rectangles of the grid, indexed by
/// [`Candidate::tile`]. Pairs of tiles that do not touch are never compared, and
/// within a pair only candidates reaching the shared rectangle are.
pub fn resolve_overlaps(tiles: &[Region], candidates: &mut [Candidate]) -> u32 {
    let mut removed = 0;

    for i in 0..tiles.len() {
        for j in i + 1..tiles.len() {
            let Some(band) = intersect(tiles[i], tiles[j]) else {
                continue;
            };

            // `if (this.getROI().getBoundsX() > pto.getROI().getBoundsX() ||
            //      this.getROI().getBoundsY() > pto.getROI().getBoundsY())`
            // — `first` is the tile further right or further down. This only
            // decides which copy wins when the two areas are exactly equal.
            let (first, second) = if tiles[j].x > tiles[i].x || tiles[j].y > tiles[i].y {
                (index(j), index(i))
            } else {
                (index(i), index(j))
            };

            let in_band = |candidate: &Candidate| {
                candidate.alive && intersect(candidate.bounds, band).is_some()
            };

            let firsts: Vec<usize> = (0..candidates.len())
                .filter(|&at| candidates[at].tile == first && in_band(&candidates[at]))
                .collect();
            let seconds: Vec<usize> = (0..candidates.len())
                .filter(|&at| candidates[at].tile == second && in_band(&candidates[at]))
                .collect();

            for &fo in &firsts {
                if !candidates[fo].alive {
                    continue;
                }
                for &so in &seconds {
                    if !candidates[so].alive {
                        continue;
                    }
                    // `if (!firstRegion.intersects(...)) continue;`
                    if intersect(candidates[fo].bounds, candidates[so].bounds).is_none() {
                        continue;
                    }
                    let shared = candidates[fo].intersection_area(&candidates[so]);
                    if shared == 0 {
                        continue;
                    }

                    let shared = f64::from(shared);
                    let (first_area, second_area) = (candidates[fo].area, candidates[so].area);
                    if first_area >= second_area {
                        if shared / f64::from(second_area) > OVERLAP_THRESHOLD {
                            candidates[so].alive = false;
                            removed += 1;
                        }
                    } else if shared / f64::from(first_area) > OVERLAP_THRESHOLD {
                        candidates[fo].alive = false;
                        removed += 1;
                        // `break;` — this object is gone, so it cannot lose
                        // again to a later one.
                        break;
                    }
                }
            }
        }
    }

    removed
}

/// A tile's position in the grid, as [`Candidate::tile`] records it.
fn index(at: usize) -> u32 {
    u32::try_from(at).expect("a grid has far fewer tiles than u32 can count")
}

/// The shared rectangle of two regions, or `None` if they do not meet.
#[must_use]
pub fn intersect(a: Region, b: Region) -> Option<Region> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then(|| Region {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid rectangle of pixels as one candidate, placed in region
    /// coordinates.
    fn block(tile: u32, x: u32, y: u32, width: u32, height: u32) -> Candidate {
        let bits = (width * height) as usize;
        let mut mask = vec![0u64; bits.div_ceil(64)];
        for bit in 0..bits {
            mask[bit / 64] |= 1 << (bit % 64);
        }
        Candidate {
            tile,
            bounds: Region {
                x,
                y,
                width,
                height,
            },
            area: width * height,
            mask,
            alive: true,
        }
    }

    fn region(x: u32, y: u32, width: u32, height: u32) -> Region {
        Region {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn the_same_nucleus_found_twice_is_resolved_to_one() {
        let tiles = [region(0, 0, 60, 40), region(40, 0, 60, 40)];
        let mut candidates = [block(0, 45, 10, 10, 10), block(1, 45, 10, 10, 10)];
        assert_eq!(resolve_overlaps(&tiles, &mut candidates), 1);
        assert_eq!(candidates.iter().filter(|c| c.alive()).count(), 1);
    }

    #[test]
    fn the_larger_copy_is_the_one_kept() {
        // The whole point of the halo: one tile clipped the nucleus at the edge
        // of its overlap and the other saw all of it. Keeping the truncated copy
        // would be worse than not having tiled at all.
        let tiles = [region(0, 0, 60, 40), region(40, 0, 60, 40)];
        let mut candidates = [block(0, 50, 10, 6, 10), block(1, 50, 10, 12, 10)];
        assert_eq!(resolve_overlaps(&tiles, &mut candidates), 1);
        assert!(candidates[1].alive(), "the complete copy must survive");
        assert!(!candidates[0].alive(), "the truncated copy must go");
    }

    #[test]
    fn two_nuclei_that_merely_abut_both_survive() {
        // A 10% overlap of the smaller is the line. These share one column of a
        // ten-wide object, which is under it.
        let tiles = [region(0, 0, 60, 40), region(40, 0, 60, 40)];
        let mut candidates = [block(0, 45, 10, 10, 10), block(1, 54, 10, 10, 10)];
        assert_eq!(resolve_overlaps(&tiles, &mut candidates), 0);
        assert!(candidates.iter().all(Candidate::alive));
    }

    #[test]
    fn an_overlap_of_exactly_a_tenth_is_kept() {
        // `intersectionArea / secondArea > threshold` is strict, so landing
        // exactly on a tenth survives. Ten of a hundred pixels shared.
        let tiles = [region(0, 0, 60, 40), region(40, 0, 60, 40)];
        let mut candidates = [block(0, 45, 10, 10, 10), block(1, 54, 10, 10, 10)];
        assert_eq!(candidates[0].intersection_area(&candidates[1]), 10);
        assert_eq!(resolve_overlaps(&tiles, &mut candidates), 0);
    }

    #[test]
    fn objects_from_the_same_tile_are_never_compared() {
        // Within a tile the tracer already decided what is one object; two
        // regions there cannot overlap, and a resolver that compared them would
        // eat legitimate detections.
        let tiles = [region(0, 0, 60, 40), region(40, 0, 60, 40)];
        let mut candidates = [block(0, 45, 10, 10, 10), block(0, 45, 10, 10, 10)];
        assert_eq!(resolve_overlaps(&tiles, &mut candidates), 0);
    }

    #[test]
    fn a_candidate_away_from_the_seam_is_untouched() {
        let tiles = [region(0, 0, 60, 40), region(40, 0, 60, 40)];
        let mut candidates = [block(0, 5, 5, 10, 10), block(1, 80, 5, 10, 10)];
        assert_eq!(resolve_overlaps(&tiles, &mut candidates), 0);
        assert!(candidates.iter().all(Candidate::alive));
    }

    #[test]
    fn the_mask_is_the_region_not_its_bounding_box() {
        // Two L-shapes sharing a bounding box but no pixel. Comparing bounding
        // boxes would delete one of them.
        let mut left = block(0, 10, 10, 4, 4);
        let mut right = block(1, 10, 10, 4, 4);
        // Left keeps the top row, right keeps the bottom row.
        left.mask = vec![0b1111];
        left.area = 4;
        right.mask = vec![0b1111 << 12];
        right.area = 4;
        assert_eq!(left.intersection_area(&right), 0);

        let tiles = [region(0, 0, 30, 30), region(0, 0, 30, 30)];
        let mut candidates = [left, right];
        assert_eq!(resolve_overlaps(&tiles, &mut candidates), 0);
    }

    #[test]
    fn a_candidate_removed_once_does_not_count_twice() {
        // Three tiles meeting at a corner all find the same nucleus. Two copies
        // go, not three, and the count must not double-report the loser.
        let tiles = [
            region(0, 0, 60, 60),
            region(40, 0, 60, 60),
            region(0, 40, 60, 60),
        ];
        let mut candidates = [
            block(0, 45, 45, 10, 10),
            block(1, 45, 45, 10, 10),
            block(2, 45, 45, 10, 10),
        ];
        assert_eq!(resolve_overlaps(&tiles, &mut candidates), 2);
        assert_eq!(candidates.iter().filter(|c| c.alive()).count(), 1);
    }
}
