//! The merge: what detection does to the surviving candidates before it splits
//! them again by shape.
//!
//! ```java
//! bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
//! if (mergeAll) {
//!     bp.filter(ImageProcessor.MAX);
//!     bp.copyBits(bpLoG, 0, 0, Blitter.AND);
//!     if (watershedPostProcess) {
//!         List<PolygonRoi> rois2 = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);
//!         bp.setValue(255);
//!         for (Roi r : rois2)
//!             bp.fill(r);
//!         new EDM().toWatershed(bp);
//!     }
//! }
//! ```
//!
//! Neither flag is reachable from the dialog. `mergeAll` is passed the literal
//! `true`, with the comment "always use 'merge all'", and `watershedPostProcess`
//! is the "Split by shape" checkbox, which defaults on. Both branches are taken
//! on a default run.
//!
//! This module is the first three lines. `toWatershed` is the shape split, and
//! is ported separately.
//!
//! # The dilation is not a dilation of the objects
//!
//! Read alone, `filter(MAX)` looks like it grows every candidate by a pixel. The
//! `AND` on the next line takes almost all of that back: the grown mask is
//! intersected with the thresholded Laplacian of Gaussian, which is where the
//! candidates came from in the first place. What survives the pair is the
//! original mask plus the pixels a candidate grew into **that the LoG already
//! called nucleus** — so the only lasting effect is to bridge gaps of one pixel
//! between two candidates sitting in the same positive LoG region.
//!
//! That is the whole point of the stage, and it is why the pair has to be ported
//! together. `gap-log-positive` and `gap-log-negative` in
//! `parity/fixtures/MergeAndSplit.json` are the same two blocks one pixel apart,
//! differing only in the LoG between them, and they come back as one object and
//! as two.
//!
//! # Three details that decide the answer
//!
//! - **`MAX` is the plain 3×3 maximum**, not the binary `DILATE` sharing its
//!   `filter(int)` entry point. On a 0/255 image the two happen to agree —
//!   `DILATE` tests `count >= binaryCount` with `binaryCount` defaulting to 1,
//!   which is the same "any neighbour is foreground" the maximum computes — so
//!   this is a distinction the fixture cannot make and the port does not rely
//!   on.
//! - **The threshold in the `AND` is `> 0.0`, strictly**, from
//!   `SimpleThresholding.thresholdAbove(fpLoG, 0.0)`. A `NaN` fails it, as it
//!   does in Java.
//! - **The holes are filled before the split, not after.** A ring the dilation
//!   closes up is solid by the time the distance map is computed, so its EDM has
//!   no interior zero and the split sees one blob rather than an annulus.
//!   `ring-closed-by-dilation` fails without it.
//!
//! A fourth was tried and is **unobservable**: whether the border is replicated,
//! as `ByteProcessor.getEdgePixel` does, or read as background, as the binary
//! `DILATE` path does. Both spellings pass every case, and they must — for a
//! maximum the replicated neighbour is a copy of a pixel already in the
//! neighbourhood, so it can never raise the result, and a background neighbour
//! is zero, so it cannot either. The clamp is kept because it is what ImageJ
//! does and because the *minimum* in `refineBoundary` reaches the same code with
//! the distinction live. Recorded here so the next reader does not spend the
//! hour, and because a mutation passing is not evidence that a convention holds.

use qupath_watershed::RegionTracer;

/// Foreground in the binary detection carries through the merge.
///
/// QuPath's `bp` holds nothing but 0 and 255: it is a fresh `ByteProcessor`
/// filled at `setValue(255)`. Every operation here preserves that, so the 3×3
/// maximum is a 3×3 logical OR and the `Blitter.AND` is a logical AND. The port
/// is specialised to that and would be wrong on a general byte image, which is
/// the reason to say so rather than to write a general maximum nobody calls.
pub const FOREGROUND: u8 = 255;

/// The merge, with its scratch.
///
/// A tile pipeline merges once per tile; rebuilding the buffers per tile is
/// allocation in the tile loop, a defect under §6 of the project guidelines.
#[derive(Debug, Default)]
pub struct Merger {
    /// `getPixelsCopy()`: the 3×3 maximum reads its neighbourhood from the
    /// image as it was, so the pass cannot run in place.
    source: Vec<u8>,
    /// The filled regions of the merged binary, which is how the holes get
    /// filled and how the merged objects get counted.
    regions: Vec<u32>,
}

impl Merger {
    /// Creates a merger with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Dilates `binary`, intersects it with the positive part of `log`, fills
    /// the holes of what is left, and returns how many regions that leaves.
    ///
    /// `binary` is the mask the intensity filter produced, holding 0 and
    /// [`FOREGROUND`] only, and is updated in place. `log` is the Laplacian of
    /// Gaussian the candidates were flooded on — QuPath thresholds it into a
    /// separate `bpLoG` and ANDs that, which is the same answer for one fewer
    /// image-sized buffer.
    ///
    /// The region map is left in [`Merger::regions`] for the split to consume.
    ///
    /// # Panics
    ///
    /// Panics if the buffers describe images of different sizes, or if `width`
    /// is zero or does not divide them.
    pub fn merge(
        &mut self,
        binary: &mut [u8],
        log: &[f32],
        width: usize,
        tracer: &mut RegionTracer,
    ) -> u32 {
        assert_eq!(
            binary.len(),
            log.len(),
            "the binary and the Laplacian must describe the same image"
        );
        assert!(width > 0, "width must be non-zero");
        assert_eq!(binary.len() % width, 0, "width must divide the buffer");

        self.source.clear();
        self.source.extend_from_slice(binary);
        dilate_and_mask(&self.source, log, binary, width);

        // `getFilledPolygonROIs(bp, FOUR_CONNECTED)` against `[127, inf)`, then
        // `bp.fill(r)` for each: every traced region's filled polygon is set to
        // foreground, which adds exactly the enclosed pixels.
        //
        // The trace below clears the region map itself, so zeroing it here was
        // the same 18 MB per 2120² tile twice over. See `fit` in `lib.rs`.
        crate::fit(&mut self.regions, binary.len(), 0);
        let found = tracer.trace_mask_into(binary, &mut self.regions, width);
        for (pixel, &region) in binary.iter_mut().zip(&self.regions) {
            if region != 0 {
                *pixel = FOREGROUND;
            }
        }
        found
    }

    /// The merged binary's filled regions, numbered from 1 in raster order of
    /// each region's first pixel, and zero for background.
    ///
    /// This is the map from *before* the fill loop wrote the holes back, so a
    /// hole carries its encloser's number here as it does everywhere else the
    /// tracer is used.
    #[must_use]
    pub fn regions(&self) -> &[u32] {
        &self.regions
    }
}

/// `bp.filter(MAX)` followed by `bp.copyBits(bpLoG, 0, 0, Blitter.AND)`, fused.
///
/// Fusing them is not an optimisation that changes the answer: the maximum of a
/// 0/255 neighbourhood is 255 exactly when one of the nine is, and ANDing 0/255
/// against 0/255 is a logical AND. It saves the image-sized `bpLoG` the Java
/// allocates and a second pass over the tile, which at 2048² and 20 threads is
/// the difference between two streams from DRAM and one.
///
/// The border is replicated, matching `ByteProcessor.getEdgePixel`: reading a
/// neighbour outside the image clamps the coordinate back onto it.
fn dilate_and_mask(source: &[u8], log: &[f32], out: &mut [u8], width: usize) {
    let height = source.len() / width;
    for y in 0..height {
        // The clamp `filter(MAX)` applies to the row above and below.
        let above = &source[y.saturating_sub(1) * width..][..width];
        let here = &source[y * width..][..width];
        let below = &source[(y + 1).min(height - 1) * width..][..width];
        let log_row = &log[y * width..][..width];
        let out_row = &mut out[y * width..][..width];

        for x in 0..width {
            // The same clamp along the row.
            let left = x.saturating_sub(1);
            let right = (x + 1).min(width - 1);
            let three = |row: &[u8]| row[left] | row[x] | row[right];
            let any = three(above) | three(here) | three(below);
            // `thresholdAbove(fpLoG, 0.0)` is `> 0.0`, strictly, and a NaN
            // fails it — which is what Java's comparison does too.
            out_row[x] = if any != 0 && log_row[x] > 0.0 {
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

    /// The 3×3 maximum, run on its own, so the fused pass can be checked
    /// against the operation it fuses rather than only end to end.
    fn dilate_only(source: &[u8], width: usize) -> Vec<u8> {
        let log = vec![1.0f32; source.len()];
        let mut out = vec![0u8; source.len()];
        dilate_and_mask(source, &log, &mut out, width);
        out
    }

    #[test]
    fn dilation_grows_a_single_pixel_into_its_eight_neighbours() {
        let mut source = vec![0u8; 25];
        source[12] = FOREGROUND;
        let out = dilate_only(&source, 5);
        let grown: Vec<usize> = (0..25).filter(|&i| out[i] != 0).collect();
        assert_eq!(grown, vec![6, 7, 8, 11, 12, 13, 16, 17, 18]);
    }

    /// A blob against the border keeps every pixel it has and grows inwards.
    ///
    /// This does *not* pin the replicated border: for a maximum, replicating and
    /// zero-padding give the same answer, and both pass this. It pins the
    /// weaker, still worth having, property that the edge is not simply skipped
    /// — `ByteProcessor.filter` handles its four border lines in a separate pass
    /// from the interior, and a port that forgets that pass leaves them
    /// untouched.
    #[test]
    fn a_corner_pixel_grows_only_inwards_and_loses_nothing_to_the_border() {
        let mut source = vec![0u8; 25];
        source[0] = FOREGROUND;
        let out = dilate_only(&source, 5);
        let grown: Vec<usize> = (0..25).filter(|&i| out[i] != 0).collect();
        assert_eq!(grown, vec![0, 1, 5, 6]);
    }

    #[test]
    fn the_mask_is_applied_after_the_dilation_not_before() {
        // One foreground pixel, and a LoG that is positive only where the
        // dilation would reach. Masking first would leave nothing.
        let mut source = vec![0u8; 9];
        source[4] = FOREGROUND;
        let mut log = vec![-1.0f32; 9];
        log[1] = 1.0;
        let mut out = vec![0u8; 9];
        dilate_and_mask(&source, &log, &mut out, 3);
        assert_eq!(out, vec![0, FOREGROUND, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_nan_in_the_laplacian_is_background() {
        let source = vec![FOREGROUND; 9];
        let mut log = vec![1.0f32; 9];
        log[4] = f32::NAN;
        let mut out = vec![0u8; 9];
        dilate_and_mask(&source, &log, &mut out, 3);
        assert_eq!(out[4], 0);
    }

    #[test]
    fn holes_closed_by_the_dilation_are_filled() {
        // A ring with a one-pixel gap in it. The dilation closes the gap, so
        // the interior becomes enclosed and the fill claims it.
        let width = 7;
        let mut binary = vec![0u8; width * 7];
        for x in 1..6 {
            binary[width + x] = FOREGROUND;
            binary[5 * width + x] = FOREGROUND;
        }
        for y in 1..6 {
            binary[y * width + 1] = FOREGROUND;
            binary[y * width + 5] = FOREGROUND;
        }
        let log = vec![1.0f32; binary.len()];
        let mut merger = Merger::new();
        let mut tracer = RegionTracer::new();
        assert_eq!(merger.merge(&mut binary, &log, width, &mut tracer), 1);
        assert!(binary.iter().all(|&p| p == FOREGROUND));
    }
}
