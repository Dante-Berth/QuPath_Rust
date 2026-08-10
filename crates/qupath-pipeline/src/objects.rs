//! Detection's first object-level filter: the one that decides which basins
//! are candidate nuclei.
//!
//! Every stage before this works on pixels and keeps all of them. This is where
//! `WatershedCellDetection` starts throwing objects away:
//!
//! ```java
//! for (Roi r : rois) {
//!     ipToMeasure.setRoi(r);
//!     double mean = ipToMeasure.getStatistics().mean;
//!     if (mean <= threshold)
//!         continue;
//!     if (bpBackgroundMask != null) {
//!         bpBackgroundMask.setRoi(r);
//!         if (bpBackgroundMask.getStatistics().mean > 0)
//!             continue;
//!     }
//!     bp.fill(r);
//! }
//! ```
//!
//! # What is measured is not what was flooded
//!
//! `ipToMeasure` is `fpLoG.duplicate()`, taken **after** the background
//! subtraction and **before** the blur and the Laplacian. So the mean is over
//! background-subtracted optical density: neither the raw density, nor the
//! Laplacian of Gaussian the watershed ran on, nor anything else the pipeline
//! still has in hand at this point. Keeping that copy is the whole reason the
//! tile scratch carries a separate `to_measure` buffer.
//!
//! # Three conventions worth stating
//!
//! - **The mean test is `<=`**, so a region landing exactly on the threshold is
//!   dropped. The *second* filter pass, after the merge and split, tests `<` on
//!   the same quantity — the two are not the same comparison, and neither is a
//!   typo that can be tidied away.
//! - **The background-mask test rejects on a single overlapping pixel**, since a
//!   `ByteProcessor` mean over the region exceeds zero as soon as one does.
//! - **The area ImageJ reports is not the region's pixel count.** Statistics
//!   skip pixels outside `[-Float.MAX_VALUE, Float.MAX_VALUE]`, so an infinite
//!   or `NaN` pixel is counted by neither the sum nor `pixelCount`. It cannot
//!   change the mean, but it does change the area the filter in §1.5 will
//!   compare against 10–400 µm².
//!
//! All three are pinned by `parity/fixtures/ObjectFilters.json`.

/// Area and mean of one traced region, as ImageJ's `ImageStatistics` reports
/// them for a `FloatProcessor` with that region's mask set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegionStats {
    /// `ImageStatistics.pixelCount`: pixels of the region whose value is
    /// finite. Not the region's size when any pixel is infinite or `NaN`.
    pub area: u32,
    /// `ImageStatistics.mean`, or `NaN` when nothing was counted — which is
    /// Java's `0.0 / 0`, and which fails the threshold test as it should.
    pub mean: f64,
}

/// The mean-intensity and background-mask tests, with their scratch.
///
/// A tile pipeline filters once per tile; rebuilding these per tile is
/// allocation in the tile loop, a defect under §6 of the project guidelines.
#[derive(Debug, Default)]
pub struct ObjectFilter {
    stats: Vec<RegionStats>,
    sums: Vec<f64>,
    counts: Vec<u32>,
    masked: Vec<u32>,
    kept: Vec<bool>,
}

impl ObjectFilter {
    /// Creates a filter with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs both tests over every traced region, returning how many survive.
    ///
    /// `regions` is the region map from
    /// [`qupath_watershed::RegionTracer::trace_into`], `count` the number it
    /// returned, and `to_measure` QuPath's `ipToMeasure`. `background_mask` is
    /// the mask `estimateBackground` produced, non-zero where a region is too
    /// dark to be background; pass `None` where QuPath's is `null`.
    ///
    /// # Panics
    ///
    /// Panics if the buffers describe images of different sizes.
    pub fn apply(
        &mut self,
        to_measure: &[f32],
        regions: &[u32],
        count: u32,
        threshold: f64,
        background_mask: Option<&[f32]>,
    ) -> u32 {
        assert_eq!(
            to_measure.len(),
            regions.len(),
            "the measured image and the region map must be the same size"
        );
        if let Some(mask) = background_mask {
            assert_eq!(
                mask.len(),
                regions.len(),
                "the background mask and the region map must be the same size"
            );
        }

        let count = count as usize;
        self.sums.clear();
        self.sums.resize(count, 0.0);
        self.counts.clear();
        self.counts.resize(count, 0);
        self.masked.clear();
        self.masked.resize(count, 0);

        // One raster pass. Within a region this visits pixels in exactly the
        // order ImageJ walks its ROI's bounding rectangle, so the `f64` sum is
        // accumulated in the same order and comes out bit-identical rather than
        // merely close — float addition is not associative, and the mean is
        // compared against a threshold.
        for (at, &region) in regions.iter().enumerate() {
            if region == 0 {
                continue;
            }
            let index = region as usize - 1;
            let value = to_measure[at];
            // `v >= -Float.MAX_VALUE && v <= Float.MAX_VALUE`, which every
            // finite `f32` satisfies and no infinity or `NaN` does.
            if value.is_finite() {
                self.sums[index] += f64::from(value);
                self.counts[index] += 1;
            }
            if background_mask.is_some_and(|mask| mask[at] != 0.0) {
                self.masked[index] += 1;
            }
        }

        self.stats.clear();
        self.stats.extend(
            self.sums
                .iter()
                .zip(&self.counts)
                .map(|(&sum, &area)| RegionStats {
                    area,
                    mean: sum / f64::from(area),
                }),
        );

        self.kept.clear();
        self.kept.extend(
            self.stats
                .iter()
                .zip(&self.masked)
                .map(|(stats, &masked)| stats.mean > threshold && masked == 0),
        );

        u32::try_from(self.kept.iter().filter(|&&kept| kept).count())
            .expect("region count fits u32 by construction")
    }

    /// Whether each region survived, indexed from zero for region 1.
    #[must_use]
    pub fn kept(&self) -> &[bool] {
        &self.kept
    }

    /// Area and mean of each region, indexed from zero for region 1.
    #[must_use]
    pub fn stats(&self) -> &[RegionStats] {
        &self.stats
    }

    /// Writes QuPath's `bp` — 255 inside a surviving region, 0 elsewhere.
    ///
    /// This is the binary image the merge and the shape split consume, so it is
    /// the handover point to the rest of §1.5 rather than a debugging aid.
    ///
    /// # Panics
    ///
    /// Panics if `binary` and `regions` describe different images.
    pub fn write_binary(&self, regions: &[u32], binary: &mut [f32]) {
        assert_eq!(
            regions.len(),
            binary.len(),
            "the region map and the binary must be the same size"
        );
        for (out, &region) in binary.iter_mut().zip(regions) {
            let kept = region != 0 && self.kept[region as usize - 1];
            *out = if kept { 255.0 } else { 0.0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two regions side by side, so a test can give each its own mean.
    fn split(width: usize, height: usize) -> Vec<u32> {
        (0..width * height)
            .map(|i| if i % width < width / 2 { 1 } else { 2 })
            .collect()
    }

    #[test]
    fn the_mean_test_drops_a_region_sitting_on_the_threshold() {
        // `if (mean <= threshold) continue;`. Exactly on the line is out, and
        // 0.5 is exact in both f32 and f64 so the case is about the comparison
        // rather than about rounding.
        let regions = vec![1u32; 16];
        let values = vec![0.5f32; 16];
        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply(&values, &regions, 1, 0.5, None), 0);
        assert_eq!(filter.stats()[0].mean, 0.5);

        let mut raised = values;
        raised[0] = 0.75;
        assert_eq!(filter.apply(&raised, &regions, 1, 0.5, None), 1);
    }

    #[test]
    fn each_region_is_measured_over_its_own_pixels() {
        let (width, height) = (8, 4);
        let regions = split(width, height);
        let values: Vec<f32> = (0..width * height)
            .map(|i| if i % width < width / 2 { 1.0 } else { 0.0 })
            .collect();

        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply(&values, &regions, 2, 0.5, None), 1);
        assert_eq!(filter.stats()[0].mean, 1.0);
        assert_eq!(filter.stats()[1].mean, 0.0);
        assert_eq!(filter.kept(), [true, false]);
    }

    #[test]
    fn one_masked_pixel_rejects_a_whole_region() {
        // The ByteProcessor mean exceeds zero as soon as a single pixel does,
        // so this is an any-overlap test rather than a majority one.
        let (width, height) = (8, 4);
        let regions = split(width, height);
        let values = vec![1.0f32; width * height];
        let mut mask = vec![0.0f32; width * height];
        mask[width + 1] = 255.0;

        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply(&values, &regions, 2, 0.5, Some(&mask)), 1);
        assert_eq!(filter.kept(), [false, true]);

        // And with no mask at all, both survive — a port that rejects whenever
        // a mask exists rather than where it is set passes the case above.
        assert_eq!(filter.apply(&values, &regions, 2, 0.5, None), 2);
    }

    #[test]
    fn a_non_finite_pixel_leaves_the_area_and_the_mean_alone() {
        // ImageJ's statistics skip anything outside +/- Float.MAX_VALUE, in the
        // sum and in `pixelCount` alike.
        let regions = vec![1u32; 4];
        let values = vec![1.0f32, 1.0, f32::NAN, f32::INFINITY];
        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply(&values, &regions, 1, 0.5, None), 1);
        assert_eq!(filter.stats()[0].area, 2, "two of the four were counted");
        assert_eq!(filter.stats()[0].mean, 1.0);
    }

    #[test]
    fn a_region_of_nothing_countable_is_dropped() {
        // `sum / pixelCount` is Java's `0.0 / 0`, which is NaN, and NaN fails
        // the threshold test rather than passing it.
        let regions = vec![1u32; 4];
        let values = vec![f32::NAN; 4];
        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply(&values, &regions, 1, 0.5, None), 0);
        assert!(filter.stats()[0].mean.is_nan());
    }

    #[test]
    fn the_binary_carries_only_the_survivors() {
        let (width, height) = (8, 4);
        let regions = split(width, height);
        let values: Vec<f32> = (0..width * height)
            .map(|i| if i % width < width / 2 { 1.0 } else { 0.0 })
            .collect();

        let mut filter = ObjectFilter::new();
        filter.apply(&values, &regions, 2, 0.5, None);
        let mut binary = vec![0.0f32; width * height];
        filter.write_binary(&regions, &mut binary);

        assert_eq!(binary[0], 255.0);
        assert_eq!(binary[width - 1], 0.0);
        assert_eq!(
            binary.iter().filter(|&&v| v == 255.0).count(),
            width / 2 * height
        );
    }
}
