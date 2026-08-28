//! ImageJ's Euclidean distance map, which is the surface the shape split runs
//! on.
//!
//! `WatershedCellDetection` reaches this through one line:
//!
//! ```java
//! new EDM().toWatershed(bp);
//! ```
//!
//! which is
//!
//! ```java
//! FloatProcessor floatEdm = makeFloatEDM(ip, 0, false);
//! ByteProcessor maxIp = maxFinder.findMaxima(floatEdm, MAXFINDER_TOLERANCE,
//!         ImageProcessor.NO_THRESHOLD, MaximumFinder.SEGMENTED, false, true);
//! if (maxIp != null) ip.copyBits(maxIp, 0, 0, Blitter.AND);
//! ```
//!
//! This module is `makeFloatEDM`. Every foreground pixel gets its distance to
//! the nearest background pixel.
//!
//! # It is exact, and it is not a chamfer
//!
//! The obvious port of a distance map is a two-pass chamfer, which propagates
//! *distances* and is approximate by a few percent. ImageJ propagates the
//! **coordinates of the nearest background point** instead and computes the true
//! Euclidean distance from them, so the result is exact wherever the propagation
//! finds the right point. A chamfer would look right, would be wrong by a
//! percent or two, and would move the saddle between two touching nuclei by a
//! pixel — which changes where the split cuts and therefore how many objects
//! come out. The fixture compares bit for bit and would report it, but only
//! because the fixture exists.
//!
//! The propagation is four sweeps: rows top to bottom, each row scanned left to
//! right and then right to left, and then the same with the rows bottom to top.
//! Each sweep carries a per-column buffer of the best background point found so
//! far, and each pixel considers three candidates — the one above it, the one
//! before it in the row, and the one diagonally back.
//!
//! # Two consequences worth stating
//!
//! - **An all-foreground image does not produce infinities.** A pixel that never
//!   sees a background point keeps `distSqr` at `Integer.MAX_VALUE`, which is
//!   cast to `float` and then square-rooted, so it comes out at about 46341
//!   rather than at infinity. It is a saturation, not a distance, and anything
//!   downstream that compares EDM values is comparing against it.
//! - **The exactness is per-point, not global.** Four sweeps propagating three
//!   candidates each is not guaranteed to find the true nearest background point
//!   for every pixel of every shape; ImageJ's own comment concedes as much. This
//!   is not corrected here. Matching QuPath includes matching this.
//!
//! Pinned by `parity/fixtures/MergeAndSplit.json`, whose `edm` array is
//! `EDM.makeFloatEDM(merged, 0, false)` recorded from a real ImageJ.

/// No nearest background point recorded yet for this column.
///
/// ImageJ's `NO_POINT`, which is `-1` and so cannot collide with a packed
/// coordinate: both halves of a real point are non-negative.
const NO_POINT: i32 = -1;

/// The largest image dimension a packed coordinate can address.
///
/// ImageJ packs a point as `x | y << 16` in a signed 32-bit int, so both
/// coordinates must fit 16 bits. QuPath's own tiles are 2048 plus a halo and
/// nowhere near this; the check exists because the failure mode without it is a
/// silently wrong distance rather than a crash.
const MAX_DIMENSION: usize = 0xffff;

/// A distance map that owns its working buffers, for repeated use.
///
/// A tile pipeline builds one map per tile; rebuilding the buffers per tile is
/// allocation in the tile loop, a defect under §6 of the project guidelines.
#[derive(Debug, Default)]
pub struct DistanceMap {
    /// The nearest background point per column, for the left-to-right scan.
    forward: Vec<i32>,
    /// The same, for the right-to-left scan. ImageJ keeps the two apart because
    /// a point that is nearest when approaching from the left is usually not the
    /// one that is nearest approaching from the right.
    backward: Vec<i32>,
}

impl DistanceMap {
    /// Creates a distance map with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes into `out` each pixel's distance to the nearest zero pixel of
    /// `mask`.
    ///
    /// This is `EDM.makeFloatEDM(ip, 0, false)`, which is the call the nucleus
    /// split makes. `edgesAreBackground` stays fixed at `false`, because a port
    /// of the branches nothing calls is a port nothing checks.
    ///
    /// The background *value* is no longer fixed: cell expansion calls the same
    /// ImageJ method with `(byte)255`, which inverts which pixels the distance
    /// is measured from. See [`Self::make_float_edm_from`], which this delegates
    /// to.
    ///
    /// `false` for the edges is the load-bearing one: the image border is **not**
    /// background, so a blob running off the edge keeps growing distances
    /// towards it rather than being cut back to zero there. On a tiled pipeline
    /// that is what makes the halo necessary and the seam resolution real.
    ///
    /// # Panics
    ///
    /// Panics if the buffers describe images of different sizes, if `width` is
    /// zero or does not divide them, or if either dimension exceeds
    /// `MAX_DIMENSION`.
    pub fn make_float_edm(&mut self, mask: &[u8], out: &mut [f32], width: usize) {
        self.make_float_edm_from(mask, out, width, 0);
    }

    /// Writes into `out` each pixel's distance to the nearest pixel of `mask`
    /// equal to `background`.
    ///
    /// This is `EDM.makeFloatEDM(ip, background, false)`. Detection reaches it
    /// with two different values and they mean opposite things:
    ///
    /// - **`0`**, from the shape split. The mask is 0/255 with the objects at
    ///   255, so distance is measured *inside* each object, outward from its
    ///   boundary. That is the surface the maximum finder splits.
    /// - **`255`**, from cell expansion. The same 0/255 mask, but now the
    ///   objects themselves are the background, so the map is zero *on* the
    ///   nuclei and measures distance *outward into the space between them*.
    ///   That is the surface the expansion floods.
    ///
    /// Reusing the `0` call for the second would produce a distance map of the
    /// wrong region entirely — cells that are the gaps between nuclei rather
    /// than the nuclei grown. Pinned by `single-nucleus` in
    /// `parity/fixtures/CellExpansion.json`, whose `edm` is zero exactly on the
    /// nucleus.
    ///
    /// # The comparison is equality, and the sign does not matter here
    ///
    /// ImageJ loads the mask byte with `baload`, which sign-extends, and
    /// compares it to the `int` argument with `if_icmpeq`. So `(byte)255`
    /// arrives as `-1` and a mask pixel of 255 loads as `-1`: they match. The
    /// mask is `u8` here and the comparison is against `255` directly, which is
    /// the same set of pixels. It is equality either way — not a threshold, so
    /// a mask holding some third value belongs to neither side and is treated
    /// as foreground.
    ///
    /// # Panics
    ///
    /// As [`Self::make_float_edm`].
    pub fn make_float_edm_from(
        &mut self,
        mask: &[u8],
        out: &mut [f32],
        width: usize,
        background: u8,
    ) {
        assert_eq!(
            mask.len(),
            out.len(),
            "the mask and the distance map must describe the same image"
        );
        assert!(width > 0, "width must be non-zero");
        assert_eq!(mask.len() % width, 0, "width must divide the buffer");
        let height = mask.len() / width;
        assert!(
            width <= MAX_DIMENSION && height <= MAX_DIMENSION,
            "a {width}x{height} image cannot be addressed by ImageJ's packed points"
        );

        // Foreground starts at infinity and is lowered; background stays zero
        // and is never visited.
        for (distance, &pixel) in out.iter_mut().zip(mask) {
            *distance = if pixel == background { 0.0 } else { f32::MAX };
        }

        self.forward.clear();
        self.forward.resize(width, NO_POINT);
        self.backward.clear();
        self.backward.resize(width, NO_POINT);
        for y in 0..height {
            self.line(mask, out, width, y, background);
        }

        // The buffers are reset, not reused: the second pair of sweeps runs the
        // other way and a point remembered from the first would be behind it.
        self.forward.fill(NO_POINT);
        self.backward.fill(NO_POINT);
        for y in (0..height).rev() {
            self.line(mask, out, width, y, background);
        }

        for distance in out.iter_mut() {
            // `FloatProcessor.sqrt()`, which is `(float)Math.sqrt(v)` for
            // positive v and 0 for the rest. Java computes in double and rounds
            // to float; that agrees with a single-precision square root bit for
            // bit, because binary64's 53 bits exceed the 2x24+2 a square root
            // needs for the double rounding to be harmless.
            *distance = distance.sqrt();
        }
    }

    /// One row, scanned left to right and then right to left.
    fn line(&mut self, mask: &[u8], out: &mut [f32], width: usize, y: usize, background: u8) {
        let row = y * width;

        let mut previous = NO_POINT;
        let mut diagonal = NO_POINT;
        for x in 0..width {
            let next_diagonal = self.forward[x];
            if mask[row + x] == background {
                self.forward[x] = pack(x, y);
            } else {
                let squared = nearest(&mut self.forward, previous, diagonal, x, y);
                if out[row + x] > squared {
                    out[row + x] = squared;
                }
            }
            previous = self.forward[x];
            diagonal = next_diagonal;
        }

        let mut previous = NO_POINT;
        let mut diagonal = NO_POINT;
        for x in (0..width).rev() {
            let next_diagonal = self.backward[x];
            if mask[row + x] == background {
                self.backward[x] = pack(x, y);
            } else {
                let squared = nearest(&mut self.backward, previous, diagonal, x, y);
                if out[row + x] > squared {
                    out[row + x] = squared;
                }
            }
            previous = self.backward[x];
            diagonal = next_diagonal;
        }
    }
}

/// ImageJ's `x | y << 16`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "both coordinates are checked against MAX_DIMENSION"
)]
fn pack(x: usize, y: usize) -> i32 {
    (x as i32) | ((y as i32) << 16)
}

/// The squared distance from `(x, y)` to the nearest of three candidate
/// background points, which also becomes this column's remembered point.
///
/// The three are the point remembered for this column on the previous row, the
/// one from the previous pixel of this row, and the one from the diagonal. The
/// order of the tests is ImageJ's and is not arbitrary: on a tie the earlier
/// candidate wins, because each test is a strict `<`.
///
/// `points[x]` is left holding whichever candidate won, so the next row inherits
/// it. Note that the column's own previous point is the initial `nearest` and is
/// therefore what a pixel with no better candidate keeps — including when it is
/// [`NO_POINT`].
fn nearest(points: &mut [i32], previous: i32, diagonal: i32, x: usize, y: usize) -> f32 {
    // Not zero: a pixel that never sees a background point keeps this, and it
    // is what makes an all-foreground image saturate rather than stay infinite.
    let mut squared = i32::MAX;

    let column = points[x];
    let mut best = column;
    if column != NO_POINT {
        let distance = distance_squared(column, x, y);
        if distance < squared {
            squared = distance;
        }
    }
    // The `!=` guards are ImageJ's and skip a candidate already considered
    // rather than merely being an optimisation: without them a tie between two
    // spellings of the same point would still take the second branch and change
    // which point is remembered.
    if diagonal != column && diagonal != NO_POINT {
        let distance = distance_squared(diagonal, x, y);
        if distance < squared {
            best = diagonal;
            squared = distance;
        }
    }
    if previous != diagonal && previous != NO_POINT {
        let distance = distance_squared(previous, x, y);
        if distance < squared {
            best = previous;
            squared = distance;
        }
    }

    points[x] = best;
    // `(float)distSqr`. At `i32::MAX` this rounds up to 2147483648.0, exactly as
    // Java's cast does.
    #[expect(
        clippy::cast_precision_loss,
        reason = "matching Java's (float) cast, saturation included"
    )]
    let squared = squared as f32;
    squared
}

/// The squared distance from a packed point to `(x, y)`, in `i32` as Java has
/// it.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "both coordinates are checked against MAX_DIMENSION"
)]
fn distance_squared(point: i32, x: usize, y: usize) -> i32 {
    let px = point & 0xffff;
    let py = (point >> 16) & 0xffff;
    let dx = x as i32 - px;
    let dy = y as i32 - py;
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edm(mask: &[u8], width: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; mask.len()];
        DistanceMap::new().make_float_edm(mask, &mut out, width);
        out
    }

    #[test]
    fn background_is_zero_and_a_lone_foreground_pixel_is_one() {
        let mut mask = vec![0u8; 25];
        mask[12] = 255;
        let out = edm(&mask, 5);
        assert_eq!(out[12], 1.0);
        assert_eq!(out[11], 0.0);
    }

    /// The distance across a bar is exact, not a chamfer approximation: the
    /// centre of a 5-wide bar is 3 from the outside, and the diagonal
    /// neighbours of a corner are sqrt(2) rather than the 1.351 a 3-4 chamfer
    /// would give.
    #[test]
    fn distances_are_euclidean_rather_than_chamfered() {
        let width = 9;
        let mut mask = vec![0u8; width * 9];
        for y in 2..7 {
            for x in 2..7 {
                mask[y * width + x] = 255;
            }
        }
        let out = edm(&mask, width);
        assert_eq!(out[4 * width + 4], 3.0);
        // The block's own corner is 1 from the background, and its inward
        // diagonal neighbour is 2 — both exact.
        assert_eq!(out[2 * width + 2], 1.0);
        assert_eq!(out[3 * width + 3], 2.0);
    }

    /// A pixel equidistant from background in two directions gets the smaller
    /// of the two, which is what makes the ridge of a rectangle a plateau
    /// rather than a slope.
    #[test]
    fn a_bar_has_a_flat_ridge() {
        let width = 11;
        let mut mask = vec![0u8; width * 5];
        for x in 1..10 {
            mask[2 * width + x] = 255;
        }
        let out = edm(&mask, width);
        for x in 2..9 {
            assert_eq!(out[2 * width + x], 1.0, "at x = {x}");
        }
    }

    /// The image border is not background, so a blob running off the edge is
    /// not cut back to zero there. This is `edgesAreBackground = false`, and it
    /// is why a tiled pipeline needs its halo.
    #[test]
    fn the_image_border_is_not_background() {
        let width = 5;
        let mut mask = vec![0u8; width * 5];
        for x in 0..width {
            mask[x] = 255;
            mask[width + x] = 255;
        }
        let out = edm(&mask, width);
        // Row 0 is foreground with background below it at row 2, so its
        // distance is 2 — it is not 1, which is what treating the edge above as
        // background would give.
        assert_eq!(out[2], 2.0);
    }

    /// An image with no background at all saturates at ImageJ's
    /// `sqrt((float)Integer.MAX_VALUE)` instead of staying infinite.
    #[test]
    fn an_all_foreground_image_saturates_rather_than_returning_infinity() {
        let out = edm(&[255u8; 16], 4);
        for &distance in &out {
            assert!(distance.is_finite());
            assert_eq!(distance, 2147483648.0f32.sqrt());
        }
    }
}
