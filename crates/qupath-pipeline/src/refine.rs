//! The boundary refinement: the stage between the shape split and the area
//! filter, and the one no plan in `docs/dev/ROADMAP.md` knew existed.
//!
//! ```java
//! if (refineBoundary && sigma > 1.5) {
//!     FloatProcessor fpBoundaryCleanup = (FloatProcessor)fpDetection.duplicate();
//!     fpBoundaryCleanup.blurGaussian(1);
//!     fpBoundaryCleanup.convolve(new float[]{0, -1, 0, -1, 4, -1, 0, -1, 0}, 3, 3);
//!     ByteProcessor bp2 = SimpleThresholding.thresholdAbove(fpBoundaryCleanup, 0.0);
//!     bp2.copyBits(bp, 0, 0, Blitter.MIN); // Remove everything not detected in bp
//!     bp.filter(ByteProcessor.MIN);
//!     bp.copyBits(bp2, 0, 0, Blitter.MAX);
//! }
//! ```
//!
//! QuPath's own comment says what it is for: the blur at the detection sigma
//! pushes the boundary of a thin nucleus outwards, and this walks it back by the
//! one pixel a smaller filter disagrees about.
//!
//! Both halves of the guard hold on a default brightfield run. `refineBoundary`
//! is a hard-coded `true` with a `TODO` beside it, and `sigma` is in **pixels** —
//! `sigmaMicrons / pixelSize`, which on CMU-1 is `1.5 / 0.4990 = 3.006`. See
//! [`SIGMA_FLOOR`].
//!
//! # What it actually computes
//!
//! Written out, with `bp` the mask on the way in:
//!
//! ```text
//! bp' = erode(bp)  OR  (bp AND cleanup > 0)
//! ```
//!
//! A pixel the erosion would remove — anything on a boundary — comes back if and
//! only if the sigma-1 Laplacian of Gaussian is positive there. So the interior
//! is untouched, the boundary is kept where the finer filter agrees it is
//! nucleus, and nothing can ever be *added*: the `MIN` against `bp` on the line
//! after the threshold is what enforces that last part, and without it the stage
//! would grow objects into whatever the finer filter liked.
//!
//! Both halves read `bp` as it was, so the two lines are fused into one pass
//! here for the same reason the merge's dilation and `AND` are: it saves an
//! image-sized `bp2` and a second stream over the tile.
//!
//! # The image it runs on is a third one
//!
//! `fpDetection` is the **raw** deconvolved channel — before the median, before
//! the background subtraction, before the blur. It is neither `fpLoG` nor
//! `ipToMeasure`. §1.5a had already found that the intensity filter measures an
//! image nothing else uses; this is a second such image, and keeping it is why
//! the tile scratch carries a `detection` buffer that nothing else reads.
//!
//! # Three details that decide the answer
//!
//! - **The erosion's border is replicated**, matching `ByteProcessor.getEdgePixel`
//!   — a neighbour outside the image is a copy of the nearest one inside. This is
//!   the same `filter(int)` entry point the merge's `MAX` uses, and where that
//!   one could not tell replication from zero-padding, this one can: a replicated
//!   neighbour can never raise a maximum, and it very much can raise a minimum.
//!   `all-foreground` in `parity/fixtures/RefineBoundary.json` is entirely
//!   foreground and comes back entirely foreground; zero-padding strips its outer
//!   ring, and so does the binary `ERODE` that shares `filter(int)` and reads
//!   outside as background.
//! - **`MIN` is the plain 3×3 minimum**, not that binary `ERODE`. On a 0/255
//!   image the two agree in the interior — `ERODE` removes a foreground pixel
//!   once `binaryCount` neighbours are background, and `binaryCount` defaults to
//!   1 — so only the border separates them, which is the point above.
//! - **The threshold is `> 0.0`, strictly**, from
//!   `SimpleThresholding.thresholdAbove`. A `NaN` fails it, as it does in Java.
//!   On a constant image this is a question about the last bit rather than a
//!   formality: the interior convolves to exactly zero, but the ring one pixel
//!   inside the border comes back at ±2.98e-08 — rounding left over from the
//!   blur's tail-sum edge handling — and `all-foreground` records exactly which
//!   of those went positive. Reproducing it needs the blur to be bit-identical at
//!   the edge, not merely close.
//!
//! # A property that has to hold and is asserted rather than assumed
//!
//! The refinement can only ever remove pixels: every one it keeps was already in
//! `bp`, by either branch. That is worth stating because it is what makes the
//! stage safe to run before the area filter — an object cannot grow past the
//! bounds the seam resolution already reasoned about.

use qupath_filters::{Convolver, GaussianBlur};

use crate::{FOREGROUND, LAPLACIAN};

/// The sigma the boundary cleanup blurs at: `blurGaussian(1)`, a literal.
///
/// Not scaled by the pixel size, where every other radius in detection is. The
/// stage is about a boundary that moved by one *pixel*, so a pixel is the unit.
pub const CLEANUP_SIGMA: f64 = 1.0;

/// The detection sigma above which the refinement runs, in pixels.
///
/// `if (refineBoundary && sigma > 1.5)`, where `sigma` has already been divided
/// by the pixel size. CMU-1's 3.006 clears it comfortably, so this fires on the
/// default run — but a slide coarse enough to put the detection sigma under 1.5
/// would skip the stage, and a port that always ran it would be wrong there.
pub const SIGMA_FLOOR: f64 = 1.5;

/// The boundary refinement, with its scratch.
///
/// A tile pipeline refines once per tile; rebuilding the buffers per tile is
/// allocation in the tile loop, a defect under §6 of the project guidelines.
#[derive(Debug)]
pub struct BoundaryRefiner {
    /// `getPixelsCopy()`: both the erosion and the restore read `bp` as it was,
    /// so the fused pass cannot run in place.
    source: Vec<u8>,
    blur: GaussianBlur,
    convolver: Convolver,
}

impl Default for BoundaryRefiner {
    /// Written out rather than derived, because a derived [`Convolver`] would
    /// silently parallelise its rows.
    fn default() -> Self {
        Self {
            source: Vec::new(),
            blur: GaussianBlur::new(),
            // Sequential rows: the pipeline calls this from inside one rayon job
            // per tile, and §6 forbids nesting. See `Scratch::new`.
            convolver: Convolver::new().with_parallel_rows(false),
        }
    }
}

impl BoundaryRefiner {
    /// Creates a refiner with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs the refinement over `binary`, in place.
    ///
    /// `detection` is QuPath's `fpDetection`, the raw deconvolved channel.
    /// `binary` is `bp` — the mask the shape split left, holding 0 and
    /// [`FOREGROUND`] only.
    ///
    /// `smoothed` and `cleanup` are image-sized scratch the caller lends for the
    /// duration. They are parameters rather than fields because by the time this
    /// stage runs the tile pipeline has two float buffers it is finished with,
    /// and a tile is ~18 MB of `f32` per buffer per worker; owning them here
    /// would add that to the peak for no reason. Their contents on the way in
    /// are ignored and on the way out are meaningless.
    ///
    /// # Panics
    ///
    /// Panics if the buffers describe images of different sizes, or if `width`
    /// is zero or does not divide them.
    pub fn refine(
        &mut self,
        detection: &[f32],
        binary: &mut [u8],
        width: usize,
        smoothed: &mut Vec<f32>,
        cleanup: &mut Vec<f32>,
    ) {
        assert_eq!(
            detection.len(),
            binary.len(),
            "the detection image and the binary must describe the same image"
        );
        assert!(width > 0, "width must be non-zero");
        assert_eq!(binary.len() % width, 0, "width must divide the buffer");

        // `fpDetection.duplicate()` — a fresh processor, so the region of
        // interest detection set on `fpDetection` does not come with it and both
        // filters below run over the whole tile. The same is true of `fpLoG` one
        // stage earlier, which is why this matches the main path.
        smoothed.clear();
        smoothed.extend_from_slice(detection);
        self.blur.blur(smoothed, width, CLEANUP_SIGMA);

        cleanup.clear();
        cleanup.resize(smoothed.len(), 0.0);
        self.convolver
            .apply_into(smoothed, cleanup, width, &LAPLACIAN, 3, 3);

        self.source.clear();
        self.source.extend_from_slice(binary);
        erode_and_restore(&self.source, cleanup, binary, width);
    }
}

/// `bp.filter(MIN)` and `bp.copyBits(bp2, MAX)`, fused, with `bp2` never built.
///
/// Fusing them changes no answer. `bp2` is `(cleanup > 0) AND source`, both
/// operands are 0/255, and `MIN`/`MAX` on a 0/255 image are a logical AND and a
/// logical OR — so the whole block is
/// `min3x3(source) | (source & (cleanup > 0))`, per pixel, from `source` alone.
///
/// The border is replicated, matching `ByteProcessor.getEdgePixel`: reading a
/// neighbour outside the image clamps the coordinate back onto it. Here that is
/// load-bearing rather than incidental — see the module comment.
fn erode_and_restore(source: &[u8], cleanup: &[f32], out: &mut [u8], width: usize) {
    let height = source.len() / width;
    for y in 0..height {
        // The clamp `filter(MIN)` applies to the row above and below.
        let above = &source[y.saturating_sub(1) * width..][..width];
        let here = &source[y * width..][..width];
        let below = &source[(y + 1).min(height - 1) * width..][..width];
        let cleanup_row = &cleanup[y * width..][..width];
        let out_row = &mut out[y * width..][..width];

        for x in 0..width {
            // The same clamp along the row. `ByteProcessor.filter` returns early
            // for a one-pixel-wide image and handles its four border lines in
            // passes separate from the interior; all of those paths clamp, so
            // one general loop agrees with every one of them.
            let left = x.saturating_sub(1);
            let right = (x + 1).min(width - 1);
            let three = |row: &[u8]| row[left] & row[x] & row[right];
            let eroded = three(above) & three(here) & three(below);
            // `bp2`: what the finer Laplacian calls nucleus, masked by `bp`. The
            // mask is what stops the refinement growing an object, and the
            // comparison is `> 0.0` strictly, so a NaN is background.
            let restored = here[x] != 0 && cleanup_row[x] > 0.0;
            out_row[x] = if eroded != 0 || restored {
                FOREGROUND
            } else {
                0
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fused pass on its own, against a cleanup image chosen by hand, so the
    /// two branches can be exercised without going through a blur.
    fn refined(source: &[u8], cleanup: &[f32], width: usize) -> Vec<u8> {
        let mut out = vec![0u8; source.len()];
        erode_and_restore(source, cleanup, &mut out, width);
        out
    }

    /// The convention the whole module turns on, and the one the merge's `MAX`
    /// could not see: a border pixel reads its missing neighbours as copies of
    /// itself, so an image that is entirely foreground erodes to nothing at all.
    /// Zero-padding the border strips the outer ring.
    #[test]
    fn an_entirely_foreground_image_survives_the_erosion() {
        let source = vec![FOREGROUND; 12 * 9];
        let cleanup = vec![-1.0f32; source.len()];
        assert_eq!(refined(&source, &cleanup, 12), source);
    }

    #[test]
    fn a_boundary_pixel_goes_where_the_finer_laplacian_disagrees() {
        // A 5x5 block with the cleanup negative everywhere: the erosion stands
        // and the block comes back 3x3.
        let width = 9;
        let mut source = vec![0u8; width * 9];
        for y in 2..7 {
            for x in 2..7 {
                source[y * width + x] = FOREGROUND;
            }
        }
        let cleanup = vec![-1.0f32; source.len()];
        let out = refined(&source, &cleanup, width);
        assert_eq!(out.iter().filter(|&&p| p != 0).count(), 9);
        assert_eq!(out[4 * width + 4], FOREGROUND);
        assert_eq!(out[2 * width + 2], 0, "the corner of the block is boundary");
    }

    #[test]
    fn a_boundary_pixel_stays_where_it_agrees() {
        // The same block with the cleanup positive: every pixel the erosion
        // removed is restored, so the block is unchanged.
        let width = 9;
        let mut source = vec![0u8; width * 9];
        for y in 2..7 {
            for x in 2..7 {
                source[y * width + x] = FOREGROUND;
            }
        }
        let cleanup = vec![1.0f32; source.len()];
        assert_eq!(refined(&source, &cleanup, width), source);
    }

    /// `bp2.copyBits(bp, 0, 0, Blitter.MIN)`. Without it the refinement grows
    /// into wherever the finer Laplacian happens to be positive, which on a real
    /// tile is most of the tissue.
    #[test]
    fn the_refinement_never_adds_a_pixel_the_binary_did_not_have() {
        let width = 7;
        let mut source = vec![0u8; width * 7];
        source[3 * width + 3] = FOREGROUND;
        // Positive everywhere, including everywhere the binary is empty.
        let cleanup = vec![1.0f32; source.len()];
        let out = refined(&source, &cleanup, width);
        assert_eq!(out, source, "one pixel in, one pixel out");
    }

    #[test]
    fn a_nan_in_the_cleanup_is_background() {
        let source = vec![FOREGROUND, FOREGROUND, 0, FOREGROUND];
        let mut cleanup = vec![1.0f32; 4];
        cleanup[0] = f32::NAN;
        // Pixel 0 has a background neighbour, so the erosion drops it, and a NaN
        // cleanup cannot bring it back.
        assert_eq!(refined(&source, &cleanup, 2)[0], 0);
    }

    /// The property the module comment claims: this stage is monotone downwards.
    #[test]
    fn the_refinement_only_ever_removes_pixels() {
        let width = 13;
        let mut source = vec![0u8; width * 11];
        let mut cleanup = vec![0.0f32; source.len()];
        // A deterministic scatter, so the case is reproducible without a
        // dependency on a random number generator.
        for (at, (pixel, value)) in source.iter_mut().zip(&mut cleanup).enumerate() {
            *pixel = if at % 3 == 0 || at % 7 == 0 {
                FOREGROUND
            } else {
                0
            };
            #[expect(clippy::cast_precision_loss, reason = "a small index, exact in f32")]
            let spread = ((at % 5) as f32) - 2.0;
            *value = spread;
        }
        let out = refined(&source, &cleanup, width);
        for (at, (&before, &after)) in source.iter().zip(&out).enumerate() {
            assert!(
                before != 0 || after == 0,
                "pixel {at} was background and came back foreground"
            );
        }
        assert!(
            out.iter().filter(|&&p| p != 0).count() < source.iter().filter(|&&p| p != 0).count(),
            "and this case should actually remove some"
        );
    }

    /// A one-pixel-wide image, which `ByteProcessor.filter` returns early for.
    #[test]
    fn a_single_column_erodes_along_its_length_only() {
        let source = vec![0, FOREGROUND, FOREGROUND, FOREGROUND, 0];
        let cleanup = vec![-1.0f32; 5];
        let out = refined(&source, &cleanup, 1);
        assert_eq!(out, vec![0, 0, FOREGROUND, 0, 0]);
    }

    #[test]
    fn a_one_pixel_image_is_its_own_neighbourhood() {
        let source = vec![FOREGROUND];
        let cleanup = vec![-1.0f32];
        assert_eq!(refined(&source, &cleanup, 1), vec![FOREGROUND]);
    }
}
