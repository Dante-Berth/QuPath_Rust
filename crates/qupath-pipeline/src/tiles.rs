//! How a region is broken into tiles for detection, and why it is not the
//! slide's own tile grid.
//!
//! This is a port of what `AbstractTileableDetectionPlugin.addRunnableTasks`
//! computes and hands to `RoiTools.computeTiledROIs`:
//!
//! ```java
//! int preferred = (int)(PREFERRED_TILE_SIZE * downsampleFactor);   // 2048
//! int max       = (int)(MAX_TILE_SIZE * downsampleFactor);         // 3072
//! Collection<? extends ROI> pathROIs = RoiTools.computeTiledROIs(
//!         parentROI, sizePreferred, sizeMax, false, getTileOverlap(imageData, params));
//! ```
//!
//! # The three things the grid does that a reading of "tiles overlap" does not
//!
//! - **A region no larger than `max` in both directions is not tiled at all.**
//!   `computeTiledROIs` returns the parent ROI itself, unpadded. Detection then
//!   runs once over the whole thing and no seam exists to resolve.
//! - **Tiles are grown by the overlap on all four sides**, not offset by it, and
//!   the result is intersected with the parent. So the core grid still partitions
//!   the region exactly, every tile reads a halo on each side that has a
//!   neighbour, and no tile reads outside the region — the outermost edges keep
//!   the same replicate-at-the-border behaviour a single-tile run would give.
//! - **The core side is `ceil(width / nx)`, not the preferred size**, because
//!   `fixedSize` is `false`. Tiles are then centred on the region, which is why
//!   the first core can start half a pixel before it and be clipped back.
//!
//! # Why the halo cannot make tiling exact
//!
//! Two stages of this pipeline are morphological reconstructions — the
//! background estimate of §1.4b and the regional maxima of §1.4 — and a geodesic
//! dilation propagates without bound. No finite halo reproduces a single-tile
//! run exactly. QuPath has the same property and accepts it, so **the target is
//! matching QuPath's grid, not choosing a better one**; a halo picked from the
//! largest kernel radius would be a different answer from QuPath's, not a more
//! correct one.

use crate::Region;

/// `AbstractTileableDetectionPlugin.PREFERRED_TILE_SIZE`, before the downsample.
pub const PREFERRED_TILE_SIZE: u32 = 2048;

/// `AbstractTileableDetectionPlugin.MAX_TILE_SIZE`, before the downsample.
pub const MAX_TILE_SIZE: u32 = 3072;

/// One processing tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// The rectangle actually read and processed: [`Tile::core`] grown by the
    /// overlap on every side and clipped to the region.
    pub padded: Region,
    /// The part of the region this tile is the interior of. The cores of a grid
    /// partition the region exactly, with no gap and no overlap.
    ///
    /// QuPath resolves overlapping detections by area rather than by which core
    /// they land in, so this is the grid's geometry rather than an ownership
    /// rule — see [`crate::resolve_overlaps`].
    pub core: Region,
}

impl Tile {
    /// Offset of the core's origin inside the padded rectangle.
    #[must_use]
    pub fn core_offset(&self) -> (u32, u32) {
        (self.core.x - self.padded.x, self.core.y - self.padded.y)
    }
}

/// The overlap `WatershedCellDetection.getTileOverlap` asks for, in pixels.
///
/// ```java
/// double expansionMicrons = 10.0;
/// double cellExpansion = params.getDoubleParameterValue("cellExpansionMicrons");
/// if (cellExpansion > 0)
///     expansionMicrons += params.getDoubleParameterValue("cellExpansionMicrons");
/// int overlap = (int)(expansionMicrons / pxSize * 2.0);
/// ```
///
/// The 10 µm is a constant floor and the cell expansion is *added* to it, not
/// compared against it; with the 5 µm default that is 15 µm, doubled, which on
/// CMU-1's 0.4990 µm/px is 60 px. Nothing here depends on any kernel radius,
/// which is worth stating because the halo a filter would want is a different
/// number and this is the one QuPath uses.
#[must_use]
pub fn tile_overlap(pixel_size_microns: f64, cell_expansion_microns: f64) -> u32 {
    let mut expansion = 10.0;
    if cell_expansion_microns > 0.0 {
        expansion += cell_expansion_microns;
    }
    if !pixel_size_microns.is_finite() {
        // QuPath's fallback when the slide carries no pixel size.
        return if cell_expansion_microns > 0.0 { 25 } else { 10 };
    }
    // Java's `(int)` truncates toward zero; the value is positive here.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a tile overlap in pixels is small and positive"
    )]
    let overlap = (expansion / pixel_size_microns * 2.0) as u32;
    overlap
}

/// Breaks `region` into the tiles QuPath would detect over.
///
/// `preferred` and `max` are already multiplied by the downsample, as
/// `addRunnableTasks` does before calling `computeTiledROIs`.
///
/// # Panics
///
/// Panics if `preferred` is zero.
#[must_use]
pub fn tile_grid(region: Region, preferred: u32, max: u32, overlap: u32) -> Vec<Tile> {
    assert!(preferred > 0, "the preferred tile size must be positive");

    // `if (pathArea == null || (bounds.getWidth() <= sizeMax.width &&
    //      bounds.getHeight() <= sizeMax.height)) return singletonList(parentROI);`
    //
    // The parent ROI itself, with no halo: a region this small is detected in
    // one pass and has no seam.
    if region.width <= max && region.height <= max {
        return vec![Tile {
            padded: region,
            core: region,
        }];
    }

    let width = i64::from(region.width);
    let height = i64::from(region.height);
    let preferred = i64::from(preferred);
    let overlap = i64::from(overlap);

    let nx = ceil_div(width, preferred);
    let ny = ceil_div(height, preferred);
    // `fixedSize` is false, so the tile is sized to divide the region evenly
    // rather than kept at the preferred size.
    let w = ceil_div(width, nx);
    let h = ceil_div(height, ny);

    // `xMin = (int)(bounds.getCenterX() - (nx * w * .5));` — the grid is centred
    // on the region, so it can start just outside it when `nx * w` overshoots.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java truncates the same expression to int here"
    )]
    let x_min = (f64::from(region.x) + width as f64 / 2.0 - (nx * w) as f64 / 2.0) as i64;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Java truncates the same expression to int here"
    )]
    let y_min = (f64::from(region.y) + height as f64 / 2.0 - (ny * h) as f64 / 2.0) as i64;

    let left = i64::from(region.x);
    let top = i64::from(region.y);
    let right = left + width;
    let bottom = top + height;

    let mut tiles = Vec::with_capacity(usize::try_from(nx * ny).unwrap_or(0));
    for yi in 0..ny {
        for xi in 0..nx {
            let core_x = x_min + xi * w;
            let core_y = y_min + yi * h;
            // Every rectangle is intersected with the parent, so both the core
            // and the padded rect are clipped to the region.
            let core = clip(core_x, core_y, w, h, left, top, right, bottom);
            let padded = clip(
                core_x - overlap,
                core_y - overlap,
                w + overlap * 2,
                h + overlap * 2,
                left,
                top,
                right,
                bottom,
            );
            // `.filter(t -> !t.isEmpty() && t.isArea())`
            if let (Some(core), Some(padded)) = (core, padded) {
                tiles.push(Tile { padded, core });
            }
        }
    }
    tiles
}

/// `(int)Math.ceil(a / b)` for the positive values this grid deals in.
fn ceil_div(a: i64, b: i64) -> i64 {
    (a + b - 1) / b
}

/// Intersects a rectangle with the region, or `None` if nothing is left.
#[expect(
    clippy::too_many_arguments,
    reason = "a rectangle and the region clipping it, both as loose coordinates \
              so the caller can pass values that have not been range-checked yet"
)]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clipped to the region, whose coordinates are u32 by construction"
)]
fn clip(
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
) -> Option<Region> {
    let x0 = x.max(left);
    let y0 = y.max(top);
    let x1 = (x + width).min(right);
    let y1 = (y + height).min(bottom);
    (x1 > x0 && y1 > y0).then(|| Region {
        x: x0 as u32,
        y: y0 as u32,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(x: u32, y: u32, width: u32, height: u32) -> Region {
        Region {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn the_overlap_is_sixty_pixels_on_cmu_1() {
        // (10 + 5) / 0.4990 * 2 = 60.12, truncated. This is the number the whole
        // seam question turns on, so it is asserted rather than derived at a
        // call site.
        assert_eq!(tile_overlap(0.4990, 5.0), 60);
    }

    #[test]
    fn the_cell_expansion_is_added_to_the_floor_not_maxed_with_it() {
        // A reading of "10 µm, or the cell expansion if larger" gives 20 px here
        // and 60 px is correct.
        assert_eq!(tile_overlap(0.4990, 0.0), 40);
        assert_eq!(tile_overlap(0.4990, 5.0), 60);
        assert_eq!(tile_overlap(0.4990, 25.0), 140);
    }

    #[test]
    fn a_region_within_the_maximum_is_one_unpadded_tile() {
        // `computeTiledROIs` returns the parent ROI itself. Not a padded tile,
        // and not a 1x1 grid of them: there is no seam, so there is no halo.
        let whole = region(100, 200, 3000, 3000);
        let tiles = tile_grid(whole, 2052, 3078, 60);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].padded, whole);
        assert_eq!(tiles[0].core, whole);
    }

    #[test]
    fn the_profiled_region_is_four_tiles_of_two_thousand() {
        // What QuPath actually does to the region every count in the roadmap is
        // measured on: 2x2, cores of 2000, each read with a 60 px halo on the
        // sides that have a neighbour.
        let tiles = tile_grid(region(27776, 5952, 4000, 4000), 2052, 3078, 60);
        assert_eq!(tiles.len(), 4);

        for tile in &tiles {
            assert_eq!(tile.core.width, 2000);
            assert_eq!(tile.core.height, 2000);
        }
        // Top-left: halo on the right and bottom only, because the left and top
        // edges are the region's own.
        assert_eq!(tiles[0].core, region(27776, 5952, 2000, 2000));
        assert_eq!(tiles[0].padded, region(27776, 5952, 2060, 2060));
        // Bottom-right: halo on the left and top only.
        assert_eq!(tiles[3].core, region(29776, 7952, 2000, 2000));
        assert_eq!(tiles[3].padded, region(29716, 7892, 2060, 2060));
    }

    #[test]
    fn the_cores_partition_the_region_exactly() {
        // The halo is the only overlap. If the cores overlapped too, every
        // object in the seam would be counted twice before any resolution ran.
        let whole = region(0, 0, 10_000, 7_000);
        let tiles = tile_grid(whole, 2052, 3078, 60);
        let covered: u64 = tiles
            .iter()
            .map(|tile| u64::from(tile.core.width) * u64::from(tile.core.height))
            .sum();
        assert_eq!(covered, 10_000 * 7_000);

        for tile in &tiles {
            assert!(tile.padded.x <= tile.core.x);
            assert!(tile.padded.y <= tile.core.y);
            assert!(tile.padded.x + tile.padded.width >= tile.core.x + tile.core.width);
            assert!(tile.padded.y + tile.padded.height >= tile.core.y + tile.core.height);
        }
    }

    #[test]
    fn every_tile_stays_inside_the_region() {
        // The padded rect is intersected with the parent, so the outermost
        // edges see exactly what a single-tile run would see.
        let whole = region(27776, 5952, 4000, 4000);
        for tile in tile_grid(whole, 2052, 3078, 60) {
            assert!(tile.padded.x >= whole.x);
            assert!(tile.padded.y >= whole.y);
            assert!(tile.padded.x + tile.padded.width <= whole.x + whole.width);
            assert!(tile.padded.y + tile.padded.height <= whole.y + whole.height);
        }
    }

    #[test]
    fn an_uneven_region_still_partitions() {
        // `nx * w` overshoots the width here, so the centred grid starts before
        // the region and the first core is clipped. The cores must still tile it
        // exactly.
        let whole = region(11, 7, 5000, 4001);
        let tiles = tile_grid(whole, 2052, 3078, 60);
        let covered: u64 = tiles
            .iter()
            .map(|tile| u64::from(tile.core.width) * u64::from(tile.core.height))
            .sum();
        assert_eq!(covered, 5000 * 4001);
    }
}
