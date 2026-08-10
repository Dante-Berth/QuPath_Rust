//! Streams slide tiles through the ported detection stages.
//!
//! This is what connects the crates: a region of a real slide is read tile by
//! tile, each tile is deconvolved to haematoxylin optical density, filtered,
//! and flooded, and only per-tile summaries come back. Before this existed the
//! ported components could not affect any end-to-end figure, because nothing
//! fed them slide pixels.
//!
//! # What this is not
//!
//! **It is not `WatershedCellDetection`, and its output is not detections.**
//! Every image-processing stage QuPath runs before the watershed is now here
//! and pinned against QuPath's own output — background subtraction by
//! reconstruction, the Laplacian of Gaussian, the regional maxima, the
//! labelling and the flood. What is absent is everything QuPath does *after*
//! the flood, which is where a basin becomes a cell:
//!
//! - **ROI tracing.** QuPath converts the label image to polygons with
//!   `getFilledPolygonROIs`, which fills interior holes. A basin with a hole is
//!   one object there and an annulus here.
//! - **The mean-intensity test.** Every candidate whose mean over the
//!   background-subtracted image is `<= threshold` (0.1 by default) is dropped.
//! - **The area filter.** Objects outside 10–400 µm² are dropped.
//! - **Merging and the shape split.** The survivors are dilated, intersected
//!   with the thresholded Laplacian, and split again by a distance-transform
//!   watershed.
//! - **Cell expansion and measurement.**
//!
//! Those five are why the count here is not QuPath's count, and all of them cut
//! it. The counts below are basins that passed no object-level test, and no
//! fixture pins them. `docs/ROADMAP.md` §1.5 is where they get ported.
//!
//! What this does establish is the path: real pixels, real decode, every ported
//! stage in QuPath's own order, at tile granularity, with memory that does not
//! grow with the region.
//!
//! # Memory
//!
//! Peak memory is bounded by the thread count, not the region size: each rayon
//! worker holds a fixed set of tile-sized buffers and nothing accumulates
//! across tiles. [`Outcome::peak_resident_bytes`] reports the high-water mark
//! so that a test can assert it, as §6 requires.

mod stain;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use qupath_filters::{Convolver, GaussianBlur, RankFilter, RankOp, Reconstructor};
use qupath_slide::{Slide, SlideError};
use qupath_watershed::{Connectivity, Labeller, Watershed};
use rayon::prelude::*;

pub use stain::Stains;

/// A rectangle of a pyramid level, in that level's pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Detection parameters.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// Pyramid level to read.
    pub level: usize,
    /// Radius of the background estimate, in pixels, or zero to skip it.
    ///
    /// The background is an **opening by reconstruction** — erode by this
    /// radius, then let the result grow back under the original image — and it
    /// is *subtracted* from the optical density rather than replacing it. That
    /// is `estimateBackground` followed by `Blitter.SUBTRACT`, and the shape it
    /// gives is a top hat: everything smaller than the kernel survives at full
    /// contrast and everything larger is flattened away.
    ///
    /// QuPath asks for 8 µm and divides by the pixel size. CMU-1 records
    /// `MPP = 0.4990`, which puts it at 16.03 px; [`Params::default`] uses that.
    pub background_radius: f64,
    /// Background level above which a region is masked out of the estimate.
    ///
    /// `estimateBackground` finds pixels whose *eroded* value exceeds this,
    /// dilates that mask by twice the radius, and sets the background estimate
    /// to negative infinity there so the reconstruction refills it from its
    /// surroundings. QuPath's brightfield default is 2.0, which on H&E is
    /// darker than tissue gets except on folds and ink.
    ///
    /// QuPath also uses the same mask to *discard* nuclei sitting on it. That
    /// half is an object-level test and belongs with the others in §1.5; only
    /// the effect on the image is here.
    pub max_background: f32,
    /// Gaussian blur sigma in pixels, or zero to skip the blur.
    ///
    /// QuPath asks for sigma in microns — 1.5 by default — and divides by the
    /// slide's pixel size, so the pixel value depends on the slide. CMU-1
    /// records `MPP = 0.4990`, which puts detection at 3.01 px on it.
    /// [`Params::default`] uses that. Nothing reads the pixel size off the
    /// slide yet, so a different slide needs this set by hand.
    pub sigma: f64,
    /// Value below which a pixel is background and is never flooded.
    ///
    /// This applies to the **Laplacian of Gaussian**, not to optical density,
    /// now that the Laplacian is in the pipeline. QuPath thresholds the same
    /// image at zero — `SimpleThresholding.thresholdAbove(fpLoG, 0.0)`, and
    /// `Watershed.doWatershed(fpLoG, labels, 0, false)` — so zero is the value
    /// that corresponds to what detection does, and the previous 0.12 was an
    /// optical-density threshold that no longer means anything here.
    pub threshold: f32,
    /// Flood connectivity.
    pub connectivity: Connectivity,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            level: 0,
            // QuPath's defaults in microns, at CMU-1's 0.4990 um/px.
            //
            // QuPath's own numbers are 16.0 and 3.0 rather than 16.03 and
            // 3.006, because it resamples to `requestedPixelSizeMicrons` (0.5)
            // before measuring anything. This pipeline reads level 0 as it is,
            // so it converts against the pixel size it actually has — see
            // DIVERGENCES.md D-005.
            background_radius: 8.0 / 0.4990,
            max_background: 2.0,
            sigma: 1.5 / 0.4990,
            // What QuPath thresholds its own LoG at. A nucleus is a positive
            // peak of the Laplacian and its surroundings are negative, so zero
            // is where the object boundary falls rather than a tuned constant.
            threshold: 0.0,
            // `Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false)` — the
            // `false` is `conn8`. Four, not eight, and not the same as the
            // 8-connected neighbourhood the maxima test uses one line earlier.
            connectivity: Connectivity::Four,
        }
    }
}

/// The value below which a pixel of the Laplacian of Gaussian cannot be a
/// regional maximum at all: `findRegionalMaxima(fpLoG, 0.001f, false)`.
///
/// Distinct from [`Params::threshold`], which gates the flood, and from the
/// zero that gates the labelling. All three are separate numbers in QuPath and
/// a port that shares one between them passes most tests.
const MAXIMA_THRESHOLD: f32 = 0.001;

/// Wall-clock nanoseconds attributed to each stage, summed over all tiles.
///
/// These are per-thread sums, so they exceed the wall-clock total by roughly
/// the thread count; the ratios between them are what the numbers are for.
#[derive(Debug, Default)]
struct StageClocks {
    decode: AtomicU64,
    deconvolve: AtomicU64,
    background: AtomicU64,
    blur: AtomicU64,
    laplacian: AtomicU64,
    maxima: AtomicU64,
    label: AtomicU64,
    flood: AtomicU64,
}

/// Time spent in each stage, in seconds, summed across threads.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stages {
    /// Reading and JPEG-decoding tiles.
    pub decode: f64,
    /// RGB to haematoxylin optical density.
    pub deconvolve: f64,
    /// Estimating and subtracting the background.
    pub background: f64,
    /// The Gaussian blur.
    pub blur: f64,
    /// The 3x3 Laplacian that completes the Laplacian of Gaussian.
    pub laplacian: f64,
    /// The regional-maximum transform, which is the prominence of every pixel.
    pub maxima: f64,
    /// Labelling the surviving maxima into markers.
    pub label: f64,
    /// The watershed flood.
    pub flood: f64,
}

impl Stages {
    /// Total across every stage.
    #[must_use]
    pub fn total(&self) -> f64 {
        self.decode
            + self.deconvolve
            + self.background
            + self.blur
            + self.laplacian
            + self.maxima
            + self.label
            + self.flood
    }
}

/// What a run produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Tiles processed.
    pub tiles: usize,
    /// Pixels processed, counting padding on edge tiles.
    pub pixels: u64,
    /// Markers seeded, summed over tiles.
    pub markers: u64,
    /// Pixels that ended up carrying a label.
    pub labelled: u64,
    /// Wall-clock seconds for the whole run.
    pub elapsed: f64,
    /// Per-stage totals, summed across threads.
    pub stages: Stages,
    /// Process high-water resident memory after the run, if readable.
    pub peak_resident_bytes: Option<u64>,
}

/// Anything that can stop a run.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The slide could not be read.
    #[error(transparent)]
    Slide(#[from] SlideError),

    /// The requested region is not inside the level.
    #[error(
        "region {x},{y} {width}x{height} is outside level {level} ({level_width}x{level_height})"
    )]
    RegionOutOfBounds {
        /// Pyramid level index.
        level: usize,
        /// Requested origin.
        x: u32,
        /// Requested origin.
        y: u32,
        /// Requested size.
        width: u32,
        /// Requested size.
        height: u32,
        /// The level's width.
        level_width: u32,
        /// The level's height.
        level_height: u32,
    },
}

/// Per-worker scratch, allocated once per rayon job and reused for every tile
/// that job handles. Allocation in the tile loop is a defect under §6.
struct Scratch {
    rgb: Vec<u8>,
    /// Haematoxylin optical density, then the same buffer with the background
    /// subtracted and the blur applied in place.
    density: Vec<f32>,
    /// The background estimate: an erosion of `density` grown back under it.
    background: Vec<f32>,
    /// Non-empty only where a region exceeded [`Params::max_background`], which
    /// on most tiles is never, so it usually stays at zero capacity.
    background_mask: Vec<f32>,
    /// The Laplacian of Gaussian: written from `density`, and the image every
    /// stage after it reads. Separate because the convolution reads a
    /// neighbourhood and so cannot write over what it is still reading.
    log: Vec<f32>,
    /// How far each pixel of `log` stands above the saddle that reaches it.
    prominence: Vec<f32>,
    labels: Vec<f32>,
    blur: GaussianBlur,
    filter: RankFilter,
    convolver: Convolver,
    reconstructor: Reconstructor,
    labeller: Labeller,
    flooder: Watershed,
}

impl Scratch {
    fn new() -> Self {
        Self {
            rgb: Vec::new(),
            density: Vec::new(),
            background: Vec::new(),
            background_mask: Vec::new(),
            log: Vec::new(),
            prominence: Vec::new(),
            labels: Vec::new(),
            blur: GaussianBlur::new(),
            filter: RankFilter::new(),
            convolver: Convolver::new(),
            reconstructor: Reconstructor::new(),
            labeller: Labeller::new(),
            flooder: Watershed::new(),
        }
    }
}

/// The kernel `WatershedCellDetection` convolves with after its Gaussian blur,
/// completing the Laplacian of Gaussian.
///
/// It sums to exactly zero, which is the branch of ImageJ's `Convolver` that
/// leaves the kernel unnormalised — see `qupath_filters::kernel_scale`.
const LAPLACIAN: [f32; 9] = [0.0, -1.0, 0.0, -1.0, 4.0, -1.0, 0.0, -1.0, 0.0];

/// Runs the pipeline over `region`, returning summary counts and timings.
///
/// Tiles are processed in parallel at tile granularity, per §6. Nothing
/// accumulates across tiles, so peak memory is set by the thread count rather
/// than by the size of the region.
///
/// # Errors
///
/// Fails if the level does not exist, the region leaves it, or a tile cannot
/// be decoded.
pub fn run(slide: &Slide, region: Region, params: Params) -> Result<Outcome, PipelineError> {
    let level = slide
        .levels()
        .get(params.level)
        .ok_or_else(|| SlideError::NoSuchLevel {
            path: slide.path().to_path_buf(),
            level: params.level,
            available: slide.levels().len(),
        })?
        .clone();

    if region.width == 0
        || region.height == 0
        || region.x.saturating_add(region.width) > level.width
        || region.y.saturating_add(region.height) > level.height
    {
        return Err(PipelineError::RegionOutOfBounds {
            level: params.level,
            x: region.x,
            y: region.y,
            width: region.width,
            height: region.height,
            level_width: level.width,
            level_height: level.height,
        });
    }

    // Every tile the region touches. Tiles are the unit of parallelism and the
    // unit of memory.
    let first_column = region.x / level.tile_width;
    let last_column = (region.x + region.width - 1) / level.tile_width;
    let first_row = region.y / level.tile_height;
    let last_row = (region.y + region.height - 1) / level.tile_height;

    let tiles: Vec<(u32, u32)> = (first_row..=last_row)
        .flat_map(|row| (first_column..=last_column).map(move |column| (column, row)))
        .collect();

    let clocks = StageClocks::default();
    let markers = AtomicU64::new(0);
    let labelled = AtomicU64::new(0);
    let width = level.tile_width as usize;

    // CMU-1 carries no stain estimate of its own, so QuPath falls back to these
    // defaults and this matches it. A slide that does carry one would need
    // `Stains::from_two` with the values from its metadata.
    let stains = Stains::h_and_e();

    let started = Instant::now();
    let outcomes: Result<Vec<()>, PipelineError> = tiles
        .par_iter()
        .map_init(Scratch::new, |scratch, &(column, row)| {
            let mark = Instant::now();
            slide.read_tile_into(params.level, column, row, &mut scratch.rgb)?;
            clocks.decode.fetch_add(nanos(mark), Ordering::Relaxed);

            let mark = Instant::now();
            stains.haematoxylin_into(&scratch.rgb, &mut scratch.density);
            clocks.deconvolve.fetch_add(nanos(mark), Ordering::Relaxed);

            // Stage order follows `WatershedCellDetection`: the background is
            // estimated and subtracted from the detection image *before* the
            // Laplacian of Gaussian, not after it.
            let mark = Instant::now();
            if params.background_radius > 0.0 {
                subtract_background(scratch, width, params);
            }
            clocks.background.fetch_add(nanos(mark), Ordering::Relaxed);

            let mark = Instant::now();
            if params.sigma > 0.0 {
                scratch.blur.blur(&mut scratch.density, width, params.sigma);
            }
            clocks.blur.fetch_add(nanos(mark), Ordering::Relaxed);

            // The line after the blur in QuPath, completing the Laplacian of
            // Gaussian. Written into its own buffer rather than in place: the
            // convolution reads a 3x3 neighbourhood and cannot overwrite what
            // it is still reading, and `apply_into` then copies nothing.
            let mark = Instant::now();
            scratch.log.clear();
            scratch.log.resize(scratch.density.len(), 0.0);
            scratch.convolver.apply_into(
                &scratch.density,
                &mut scratch.log,
                width,
                &LAPLACIAN,
                3,
                3,
            );
            clocks.laplacian.fetch_add(nanos(mark), Ordering::Relaxed);

            let mut labels = std::mem::take(&mut scratch.labels);
            let seeded = seed_markers(scratch, &mut labels, width, &clocks);
            markers.fetch_add(u64::from(seeded), Ordering::Relaxed);
            scratch.labels = labels;

            let mark = Instant::now();
            scratch.flooder.flood(
                &scratch.log,
                &mut scratch.labels,
                width,
                params.threshold,
                params.connectivity,
            );
            clocks.flood.fetch_add(nanos(mark), Ordering::Relaxed);

            labelled.fetch_add(
                scratch.labels.iter().filter(|&&l| l != 0.0).count() as u64,
                Ordering::Relaxed,
            );
            Ok(())
        })
        .collect();
    outcomes?;

    let elapsed = started.elapsed().as_secs_f64();

    Ok(Outcome {
        tiles: tiles.len(),
        pixels: tiles.len() as u64 * (width * level.tile_height as usize) as u64,
        markers: markers.load(Ordering::Relaxed),
        labelled: labelled.load(Ordering::Relaxed),
        elapsed,
        stages: Stages {
            decode: seconds(&clocks.decode),
            deconvolve: seconds(&clocks.deconvolve),
            background: seconds(&clocks.background),
            blur: seconds(&clocks.blur),
            laplacian: seconds(&clocks.laplacian),
            maxima: seconds(&clocks.maxima),
            label: seconds(&clocks.label),
            flood: seconds(&clocks.flood),
        },
        peak_resident_bytes: peak_resident_bytes(),
    })
}

fn nanos(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "nanosecond totals are far below f64's exact integer range"
)]
fn seconds(counter: &AtomicU64) -> f64 {
    counter.load(Ordering::Relaxed) as f64 / 1e9
}

/// Turns the Laplacian of Gaussian in `scratch.log` into markers in `labels`,
/// returning how many there are.
///
/// The pair of calls detection makes:
///
/// ```java
/// ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false);
/// ImageProcessor ipNucleusLabels = RoiLabeling.labelImage(ipTemp, 0, false);
/// ```
///
/// The first is a prominence: zero everywhere except at a maximum that stands
/// above the highest saddle reaching it. The second keeps the pixels where that
/// is strictly positive and numbers their 4-connected components.
///
/// Three thresholds appear in two lines and none of them is the same number:
/// `0.001` gates which pixels may be maxima, `0` gates which survive, and
/// [`Params::threshold`] gates the flood afterwards.
fn seed_markers(
    scratch: &mut Scratch,
    labels: &mut Vec<f32>,
    width: usize,
    clocks: &StageClocks,
) -> u32 {
    let mark = Instant::now();
    scratch.reconstructor.regional_maxima_into(
        &scratch.log,
        &mut scratch.prominence,
        width,
        MAXIMA_THRESHOLD,
    );
    clocks.maxima.fetch_add(nanos(mark), Ordering::Relaxed);

    let mark = Instant::now();
    labels.clear();
    labels.resize(scratch.log.len(), 0.0);
    let found =
        scratch
            .labeller
            .label_into(&scratch.prominence, labels, width, 0.0, Connectivity::Four);
    clocks.label.fetch_add(nanos(mark), Ordering::Relaxed);
    found
}

/// Estimates the background and subtracts it from `scratch.density`, in place.
///
/// This is `estimateBackground` followed by `Blitter.SUBTRACT`, which together
/// are a top hat by reconstruction:
///
/// 1. erode the optical density by `background_radius`;
/// 2. where the *eroded* image exceeds `max_background`, dilate that mask by
///    twice the radius and drop the estimate to negative infinity there;
/// 3. let the estimate grow back under the original image by geodesic dilation;
/// 4. subtract.
///
/// Step 3 is what makes it an opening *by reconstruction* rather than a plain
/// opening: a nucleus that the erosion removed does not come back, but the
/// slowly varying tissue around it returns to its exact original level instead
/// of to the dilation's approximation of it. QuPath made this optional in
/// v0.4.0 and left it on by default.
///
/// **The subtraction is the part that is easy to get wrong.** Replacing the
/// image with its opening — which is what a morphological opening reads like —
/// keeps the background and discards the nuclei. The subtraction keeps exactly
/// what the opening threw away, which is the nuclei.
fn subtract_background(scratch: &mut Scratch, width: usize, params: Params) {
    let radius = params.background_radius;

    scratch.background.clear();
    scratch.background.resize(scratch.density.len(), 0.0);
    scratch.filter.apply_into(
        &scratch.density,
        &mut scratch.background,
        width,
        radius,
        RankOp::Min,
    );

    // Regions too dark to be background at all. QuPath only allocates the mask
    // if some pixel exceeds the limit, and on tissue that is usually never, so
    // the common path is this scan and nothing else.
    if params.max_background > 0.0
        && scratch
            .background
            .iter()
            .any(|&value| value > params.max_background)
    {
        scratch.background_mask.clear();
        scratch.background_mask.extend(
            scratch
                .background
                .iter()
                .map(|&value| f32::from(u8::from(value > params.max_background))),
        );
        scratch.filter.apply(
            &mut scratch.background_mask,
            width,
            radius * 2.0,
            RankOp::Max,
        );
        for (estimate, &masked) in scratch.background.iter_mut().zip(&scratch.background_mask) {
            if masked != 0.0 {
                *estimate = f32::NEG_INFINITY;
            }
        }
    }

    scratch
        .reconstructor
        .reconstruct(&mut scratch.background, &scratch.density, width);

    for (value, &estimate) in scratch.density.iter_mut().zip(&scratch.background) {
        *value -= estimate;
    }
}

/// Process high-water resident set size, from `/proc/self/status`.
///
/// `VmHWM` is the peak, not the current value, so it survives the buffers being
/// dropped and is what §6's memory ceiling should be asserted against.
#[must_use]
pub fn peak_resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the marker chain the tile loop runs, on a hand-built surface.
    fn markers(values: &[f32], width: usize) -> (u32, Vec<f32>) {
        let mut scratch = Scratch::new();
        scratch.log.extend_from_slice(values);
        let mut labels = vec![0.0f32; values.len()];
        let found = seed_markers(&mut scratch, &mut labels, width, &StageClocks::default());
        (found, labels)
    }

    #[test]
    fn a_single_peak_seeds_one_marker() {
        let width = 9;
        let mut values = vec![0.0f32; width * 9];
        values[4 * width + 4] = 1.0;
        let (found, labels) = markers(&values, width);
        assert_eq!(found, 1);
        assert_eq!(labels[4 * width + 4], 1.0);
    }

    #[test]
    fn a_plateau_seeds_exactly_one_marker() {
        // A strict local-maximum test finds nothing on a plateau, and the
        // stages upstream produce plateaus by construction. Seeding per
        // candidate pixel instead would give 25 markers for this one object.
        let width = 9;
        let mut values = vec![0.0f32; width * 9];
        for y in 2..7 {
            for x in 2..7 {
                values[y * width + x] = 1.0;
            }
        }
        let (found, labels) = markers(&values, width);
        assert_eq!(found, 1, "a plateau is one maximum, not one per pixel");
        assert_eq!(labels.iter().filter(|&&l| l != 0.0).count(), 25);
    }

    #[test]
    fn a_terrace_seeds_no_marker_at_all() {
        // The regression this stage exists to prevent, and the one the pipeline
        // had before it was ported: a shelf on the side of a taller block is
        // `>=` all its neighbours over its whole interior, so a plain maximum
        // test — however it groups the candidates — reports a second marker on
        // the same object. Prominence is what tells the two apart.
        let (width, height) = (21, 11);
        let mut values = vec![0.0f32; width * height];
        for y in 2..height - 2 {
            for x in 2..width / 2 {
                values[y * width + x] = 0.5;
            }
            for x in width / 2..width - 2 {
                values[y * width + x] = 1.0;
            }
        }
        let (found, labels) = markers(&values, width);
        assert_eq!(
            found, 1,
            "the block is a maximum; the terrace beside it is not"
        );
        assert_eq!(labels[5 * width + 4], 0.0, "no marker on the terrace");
        assert_eq!(labels[5 * width + 14], 1.0, "one on the block");
    }

    #[test]
    fn two_separated_peaks_seed_two_markers() {
        let width = 11;
        let mut values = vec![0.0f32; width * 5];
        values[2 * width + 2] = 1.0;
        values[2 * width + 8] = 1.0;
        assert_eq!(markers(&values, width).0, 2);
    }

    #[test]
    fn background_below_the_threshold_seeds_nothing() {
        // `findRegionalMaxima(fpLoG, 0.001f, ...)`: below that, not a candidate.
        let width = 9;
        let mut values = vec![0.0f32; width * 9];
        values[4 * width + 4] = 0.0005;
        assert_eq!(markers(&values, width).0, 0);
    }

    #[test]
    fn a_top_hat_keeps_what_the_opening_discards() {
        // The other half of this change. A nucleus narrower than the background
        // radius survives the subtraction at full contrast, while the slowly
        // varying tissue it sits on is flattened to zero. Replacing the image
        // with its opening — rather than subtracting it — does exactly the
        // opposite, and looks entirely plausible while doing so.
        let (width, height) = (41, 41);
        let mut scratch = Scratch::new();
        scratch.density = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                // A broad ramp of tissue, plus one small dark nucleus.
                let tissue = 0.2 + 0.002 * (x + y) as f32;
                let nucleus = usize::abs_diff(x, 20) < 3 && usize::abs_diff(y, 20) < 3;
                if nucleus { tissue + 1.0 } else { tissue }
            })
            .collect();

        let params = Params {
            background_radius: 8.0,
            ..Params::default()
        };
        let before = scratch.density.clone();
        subtract_background(&mut scratch, width, params);

        assert!(
            scratch.density[20 * width + 20] > 0.9,
            "the nucleus keeps its contrast: {}",
            scratch.density[20 * width + 20]
        );
        assert!(
            scratch.density[5 * width + 5].abs() < 1e-6,
            "the tissue ramp is flattened: {}",
            scratch.density[5 * width + 5]
        );
        assert!(
            before[5 * width + 5] > 0.2,
            "and it was not flat to begin with"
        );
    }

    #[test]
    fn peak_memory_is_readable() {
        // The §6 ceiling test depends on this working; if /proc is absent the
        // ceiling cannot be asserted and that must be visible.
        assert!(
            peak_resident_bytes().is_some_and(|bytes| bytes > 0),
            "VmHWM unreadable; the memory ceiling cannot be checked here"
        );
    }
}
