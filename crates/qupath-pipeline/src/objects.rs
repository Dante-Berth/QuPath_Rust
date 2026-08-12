//! Detection's two object-level filters: the one that decides which basins are
//! candidate nuclei, and the one that decides which of the objects the merge,
//! the split and the boundary refinement leave are nuclei at all.
//!
//! Every stage before the first works on pixels and keeps all of them. This is
//! where `WatershedCellDetection` starts throwing objects away:
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
//! and the second, once those candidates have been merged, split and refined:
//!
//! ```java
//! if (minArea > 0 || maxArea > 0) {
//!     bp.setValue(0);
//!     Iterator<PolygonRoi> iter = roisNuclei.iterator();
//!     while (iter.hasNext()) {
//!         Roi roiTemp = iter.next();
//!         ipToMeasure.setRoi(roiTemp);
//!         ImageStatistics stats = ImageStatistics.getStatistics(ipToMeasure, Measurements.AREA | Measurements.MEAN, null);
//!         double area = stats.pixelCount;
//!         if ((stats.mean < threshold) || (minArea > 0 && area < minArea) || (maxArea > 0 && area > maxArea)) {
//!             iter.remove();
//!             bp.fill(roiTemp);
//!         }
//!     }
//!     ipToMeasure.resetRoi();
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
//! tile scratch carries a separate `to_measure` buffer. **Both** passes measure
//! it, which is what makes their two comparisons worth telling apart: they are
//! the same quantity over different objects.
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
//!   compare against 10–400 µm². That filter is [`ObjectFilter::apply_area`],
//!   and this is the reason its area is not simply the region's pixel count.
//!
//! All three are pinned by `parity/fixtures/ObjectFilters.json`.
//!
//! # Three more, which belong to the second pass alone
//!
//! - **The whole block is guarded by `minArea > 0 || maxArea > 0`.** Turn both
//!   area limits off and the *mean* test disappears with them, so an object the
//!   first pass kept is never re-measured. QuPath's defaults are 10 and 400 µm²
//!   so the guard holds on a default run, but it is a guard on the mean test as
//!   much as on the area tests and a port that hoisted the mean out would differ
//!   wherever a user cleared both.
//! - **The mean test is `<`**, where the first pass tests `<=`. A candidate
//!   landing exactly on the threshold survives here and would not have survived
//!   there — and since it already survived there, the two comparisons only ever
//!   differ for an object whose mean *changed* between the passes, which is
//!   every object the merge or the split altered.
//! - **A region of nothing countable survives the mean test here and fails it
//!   there.** `sum / pixelCount` is `NaN` when the count is zero, `NaN > 0.1` is
//!   false and so is `NaN < 0.1`, so the first pass drops it and the second does
//!   not. It is then dropped by the area test instead, since a count of zero is
//!   below any positive minimum — but only because `minArea > 0`.

/// Where one traced region sits, and how many pixels it fills.
///
/// Distinct from [`RegionStats`] in the one way that matters: [`Extent::area`]
/// counts **every** pixel of the filled region, where `RegionStats::area` is
/// ImageJ's `pixelCount` and skips non-finite ones. Overlap resolution compares
/// `geometry.getArea()`, which is the polygon's own area and so is the former —
/// for a polygon traced around pixels the two agree exactly, and a region
/// carrying an infinity is where they part company.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    /// Left edge of the bounding box.
    pub x: u32,
    /// Top edge of the bounding box.
    pub y: u32,
    /// Width of the bounding box; zero if the region has no pixels.
    pub width: u32,
    /// Height of the bounding box.
    pub height: u32,
    /// Pixels the filled region covers.
    pub area: u32,
}

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

        self.measure(to_measure, regions, count, background_mask);

        self.kept.clear();
        self.kept.extend(
            self.stats
                .iter()
                .zip(&self.masked)
                .map(|(stats, &masked)| stats.mean > threshold && masked == 0),
        );

        self.survivors()
    }

    /// Detection's *second* object-level filter, over the objects the merge, the
    /// shape split and the boundary refinement leave.
    ///
    /// Same measurement, three different tests: an area window either side of
    /// the object, and a mean test that is `<` where the first pass is `<=`.
    /// `min_area` and `max_area` are in pixels — QuPath asks for µm² and divides
    /// by the *squared* pixel size — and either is disabled by passing zero or
    /// less, as QuPath disables it with `minArea > 0`.
    ///
    /// **Both disabled skips the mean test as well**, because the whole block
    /// sits inside `if (minArea > 0 || maxArea > 0)`. That is a strange place
    /// for a mean test to live and it is where QuPath put it.
    ///
    /// There is no background-mask test here; QuPath applies that one only in
    /// the first pass.
    ///
    /// # Panics
    ///
    /// Panics if the buffers describe images of different sizes.
    pub fn apply_area(
        &mut self,
        to_measure: &[f32],
        regions: &[u32],
        count: u32,
        threshold: f64,
        min_area: f64,
        max_area: f64,
    ) -> u32 {
        assert_eq!(
            to_measure.len(),
            regions.len(),
            "the measured image and the region map must be the same size"
        );

        self.measure(to_measure, regions, count, None);

        self.kept.clear();
        if min_area <= 0.0 && max_area <= 0.0 {
            // `if (minArea > 0 || maxArea > 0)` is false, so nothing in the
            // block runs — the mean test included.
            self.kept.resize(count as usize, true);
            return self.survivors();
        }
        self.kept.extend(self.stats.iter().map(|stats| {
            // `iter.remove()` on any of the three, so surviving is the negation
            // of all three. `NaN < threshold` is false, which is the opposite of
            // what the first pass's `NaN > threshold` does with the same region.
            let area = f64::from(stats.area);
            let too_faint = stats.mean < threshold;
            let too_small = min_area > 0.0 && area < min_area;
            let too_large = max_area > 0.0 && area > max_area;
            !(too_faint || too_small || too_large)
        }));

        self.survivors()
    }

    /// Area and mean of every region, and how many of its pixels the background
    /// mask claims. Shared by both filter passes, which measure identically and
    /// differ only in what they do with the answer.
    fn measure(
        &mut self,
        to_measure: &[f32],
        regions: &[u32],
        count: u32,
        background_mask: Option<&[f32]>,
    ) {
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
    }

    fn survivors(&self) -> u32 {
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

    /// Bounding box and filled area of every region, indexed from zero for
    /// region 1.
    ///
    /// Only needed when a region is tiled, since it is what
    /// [`crate::resolve_overlaps`] compares across a seam; a single-tile run
    /// never calls it and pays nothing for it.
    ///
    /// # Panics
    ///
    /// Panics if `width` does not divide the region map.
    pub fn extents_into(&self, regions: &[u32], width: usize, count: u32, out: &mut Vec<Extent>) {
        assert!(width > 0, "width must be non-zero");
        assert_eq!(regions.len() % width, 0, "width must divide the region map");

        out.clear();
        out.resize(
            count as usize,
            Extent {
                x: u32::MAX,
                y: u32::MAX,
                width: 0,
                height: 0,
                area: 0,
            },
        );

        for (at, &region) in regions.iter().enumerate() {
            if region == 0 {
                continue;
            }
            let extent = &mut out[region as usize - 1];
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a tile's pixel count fits u32; the tracer asserts it"
            )]
            let (x, y) = ((at % width) as u32, (at / width) as u32);
            // `width` and `height` hold the exclusive far edge until the pass
            // ends, then become extents; a region's first pixel in raster order
            // fixes `y`, so only `x` needs a running minimum.
            extent.x = extent.x.min(x);
            extent.y = extent.y.min(y);
            extent.width = extent.width.max(x + 1);
            extent.height = extent.height.max(y + 1);
            extent.area += 1;
        }

        for extent in out.iter_mut() {
            if extent.area == 0 {
                *extent = Extent {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                    area: 0,
                };
            } else {
                extent.width -= extent.x;
                extent.height -= extent.y;
            }
        }
    }

    /// Writes QuPath's `bp` — [`crate::FOREGROUND`] inside a surviving region,
    /// 0 elsewhere.
    ///
    /// This is the binary image the merge and the shape split consume, so it is
    /// the handover point to the rest of §1.5 rather than a debugging aid.
    ///
    /// There is no counterpart for [`ObjectFilter::apply_area`], and QuPath does
    /// have one — it fills each rejected object back out of `bp` as it goes. The
    /// only thing that reads `bp` afterwards is the cell expansion, which is not
    /// ported, and writing a mask nothing reads is work this pipeline would do
    /// on every tile forever.
    ///
    /// # Panics
    ///
    /// Panics if `binary` and `regions` describe different images.
    pub fn write_binary(&self, regions: &[u32], binary: &mut [u8]) {
        assert_eq!(
            regions.len(),
            binary.len(),
            "the region map and the binary must be the same size"
        );
        for (out, &region) in binary.iter_mut().zip(regions) {
            let kept = region != 0 && self.kept[region as usize - 1];
            *out = if kept { crate::FOREGROUND } else { 0 };
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
        let mut binary = vec![0u8; width * height];
        filter.write_binary(&regions, &mut binary);

        assert_eq!(binary[0], crate::FOREGROUND);
        assert_eq!(binary[width - 1], 0);
        assert_eq!(
            binary.iter().filter(|&&v| v == crate::FOREGROUND).count(),
            width / 2 * height
        );
    }

    /// Three regions of 4, 8 and 12 pixels, all bright enough to pass the mean.
    fn sized(areas: [usize; 3]) -> (Vec<f32>, Vec<u32>) {
        let mut regions = Vec::new();
        for (at, &area) in areas.iter().enumerate() {
            regions.extend(std::iter::repeat_n(u32::try_from(at + 1).unwrap(), area));
        }
        (vec![1.0f32; regions.len()], regions)
    }

    #[test]
    fn the_area_window_is_inclusive_at_both_ends() {
        // `area < minArea` and `area > maxArea` drop, so an object landing
        // exactly on either limit survives.
        let (values, regions) = sized([4, 8, 12]);
        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply_area(&values, &regions, 3, 0.5, 4.0, 12.0), 3);
        assert_eq!(filter.kept(), [true, true, true]);

        assert_eq!(filter.apply_area(&values, &regions, 3, 0.5, 5.0, 11.0), 1);
        assert_eq!(filter.kept(), [false, true, false]);
    }

    #[test]
    fn either_area_limit_can_be_disabled_on_its_own() {
        let (values, regions) = sized([4, 8, 12]);
        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply_area(&values, &regions, 3, 0.5, 0.0, 8.0), 2);
        assert_eq!(filter.kept(), [true, true, false]);

        assert_eq!(filter.apply_area(&values, &regions, 3, 0.5, 8.0, 0.0), 2);
        assert_eq!(filter.kept(), [false, true, true]);
    }

    #[test]
    fn disabling_both_area_limits_disables_the_mean_test_with_them() {
        // The whole block is `if (minArea > 0 || maxArea > 0)`, so an object
        // far below the threshold is never re-measured.
        let regions = vec![1u32; 8];
        let values = vec![0.0f32; 8];
        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply_area(&values, &regions, 1, 0.5, 0.0, 0.0), 1);
        // And with a limit set, the same object goes on the mean alone.
        assert_eq!(filter.apply_area(&values, &regions, 1, 0.5, 1.0, 0.0), 0);
    }

    #[test]
    fn the_second_mean_test_keeps_what_the_first_would_drop() {
        // `<` here, `<=` there. The same region on the same threshold.
        let regions = vec![1u32; 16];
        let values = vec![0.5f32; 16];
        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply(&values, &regions, 1, 0.5, None), 0);
        assert_eq!(filter.apply_area(&values, &regions, 1, 0.5, 1.0, 0.0), 1);
    }

    #[test]
    fn a_region_of_nothing_countable_is_dropped_by_the_area_not_the_mean() {
        // The mirror of `a_region_of_nothing_countable_is_dropped`: `NaN < t` is
        // false, so the mean test passes it here, and the area of zero is what
        // removes it — which needs `minArea > 0` to be the case.
        let regions = vec![1u32; 4];
        let values = vec![f32::NAN; 4];
        let mut filter = ObjectFilter::new();
        assert_eq!(filter.apply_area(&values, &regions, 1, 0.5, 1.0, 0.0), 0);
        assert_eq!(
            filter.apply_area(&values, &regions, 1, 0.5, 0.0, 100.0),
            1,
            "with no minimum there is nothing left to reject it"
        );
    }

    #[test]
    fn a_poisoned_binary_is_fully_overwritten() {
        // The pipeline hands this the previous tile's mask without clearing it.
        // `write_binary` writes every pixel — foreground or zero — and this is
        // what says so.
        let regions = split(6, 4);
        let values = vec![1.0f32; regions.len()];
        let mut filter = ObjectFilter::new();
        filter.apply(&values, &regions, 2, 0.5, None);
        let mut zeroed = vec![0u8; regions.len()];
        let mut poisoned = vec![17u8; regions.len()];
        filter.write_binary(&regions, &mut zeroed);
        filter.write_binary(&regions, &mut poisoned);
        assert_eq!(zeroed, poisoned, "a pixel of the binary was left unwritten");
    }
}
