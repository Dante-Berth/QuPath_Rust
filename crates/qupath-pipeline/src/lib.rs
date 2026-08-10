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
//! The stages QuPath runs that are not yet ported — `findRegionalMaxima`,
//! thresholding, ROI tracing and measurement — are absent, and the marker rule
//! here is a plain regional-maximum test with no prominence filter at all, so
//! it over-segments. The counts below are flooded basins, not cells, and no
//! fixture pins them.
//!
//! The Laplacian of Gaussian *is* complete as of the Laplacian port: both the
//! blur and the 3x3 convolution that follows it are pinned bit-identical
//! against ImageJ. What remains between here and detections is how the maxima
//! are chosen — QuPath uses
//! `MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false)`
//! followed by a 4-connected `labelImage`, which is an h-maxima transform, and
//! that prominence test is exactly what this pipeline lacks.
//!
//! What it does establish is the path: real pixels, real decode, the ported
//! filter and flood, at tile granularity, with memory that does not grow with
//! the region. `docs/ROADMAP.md` lists what each remaining stage needs.
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

use qupath_filters::{Convolver, GaussianBlur, RankFilter, RankOp};
use qupath_slide::{Slide, SlideError};
use qupath_watershed::{Connectivity, Watershed};
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
    /// Radius of the morphological opening applied before the blur, in pixels.
    ///
    /// QuPath derives its rank-filter radii from the user's cell-size
    /// parameters; this stands in for that until the detection front end is
    /// ported. It sits where QuPath's median filter sits — before the Laplacian
    /// of Gaussian, not after it — so that the maxima are found on a clean
    /// second derivative rather than on a smeared one.
    pub open_radius: f64,
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
            open_radius: 2.0,
            // 1.5 microns at CMU-1's 0.4990 um/px, which is what QuPath would
            // use on this slide with its own defaults.
            sigma: 1.5 / 0.4990,
            // What QuPath thresholds its own LoG at. A nucleus is a positive
            // peak of the Laplacian and its surroundings are negative, so zero
            // is where the object boundary falls rather than a tuned constant.
            threshold: 0.0,
            connectivity: Connectivity::Eight,
        }
    }
}

/// Wall-clock nanoseconds attributed to each stage, summed over all tiles.
///
/// These are per-thread sums, so they exceed the wall-clock total by roughly
/// the thread count; the ratios between them are what the numbers are for.
#[derive(Debug, Default)]
struct StageClocks {
    decode: AtomicU64,
    deconvolve: AtomicU64,
    filter: AtomicU64,
    blur: AtomicU64,
    laplacian: AtomicU64,
    maxima: AtomicU64,
    flood: AtomicU64,
}

/// Time spent in each stage, in seconds, summed across threads.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stages {
    /// Reading and JPEG-decoding tiles.
    pub decode: f64,
    /// RGB to haematoxylin optical density.
    pub deconvolve: f64,
    /// The rank-filter opening.
    pub filter: f64,
    /// The Gaussian blur.
    pub blur: f64,
    /// The 3x3 Laplacian that completes the Laplacian of Gaussian.
    pub laplacian: f64,
    /// Finding markers.
    pub maxima: f64,
    /// The watershed flood.
    pub flood: f64,
}

impl Stages {
    /// Total across every stage.
    #[must_use]
    pub fn total(&self) -> f64 {
        self.decode
            + self.deconvolve
            + self.filter
            + self.blur
            + self.laplacian
            + self.maxima
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
    density: Vec<f32>,
    opened: Vec<f32>,
    /// The Laplacian of Gaussian: written from `opened`, and the image every
    /// stage after it reads. Separate because the convolution reads a
    /// neighbourhood and so cannot write over what it is still reading.
    log: Vec<f32>,
    labels: Vec<f32>,
    candidates: Vec<bool>,
    stack: Vec<u32>,
    blur: GaussianBlur,
    filter: RankFilter,
    convolver: Convolver,
    flooder: Watershed,
}

impl Scratch {
    fn new() -> Self {
        Self {
            rgb: Vec::new(),
            density: Vec::new(),
            opened: Vec::new(),
            log: Vec::new(),
            labels: Vec::new(),
            candidates: Vec::new(),
            stack: Vec::new(),
            blur: GaussianBlur::new(),
            filter: RankFilter::new(),
            convolver: Convolver::new(),
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

            // Stage order follows `WatershedCellDetection`: its rank filter runs
            // on the detection image *before* the Laplacian of Gaussian, not
            // after it, so the maxima are found on a clean second derivative.
            let mark = Instant::now();
            scratch.opened.clear();
            scratch.opened.resize(scratch.density.len(), 0.0);
            // Morphological opening: erode then dilate, which removes specks
            // smaller than the kernel while leaving nuclei intact.
            scratch.filter.apply_into(
                &scratch.density,
                &mut scratch.opened,
                width,
                params.open_radius,
                RankOp::Min,
            );
            scratch
                .filter
                .apply(&mut scratch.opened, width, params.open_radius, RankOp::Max);
            clocks.filter.fetch_add(nanos(mark), Ordering::Relaxed);

            let mark = Instant::now();
            if params.sigma > 0.0 {
                scratch.blur.blur(&mut scratch.opened, width, params.sigma);
            }
            clocks.blur.fetch_add(nanos(mark), Ordering::Relaxed);

            // The line after the blur in QuPath, completing the Laplacian of
            // Gaussian. Written into its own buffer rather than in place: the
            // convolution reads a 3x3 neighbourhood and cannot overwrite what
            // it is still reading, and `apply_into` then copies nothing.
            let mark = Instant::now();
            scratch.log.clear();
            scratch.log.resize(scratch.opened.len(), 0.0);
            scratch.convolver.apply_into(
                &scratch.opened,
                &mut scratch.log,
                width,
                &LAPLACIAN,
                3,
                3,
            );
            clocks.laplacian.fetch_add(nanos(mark), Ordering::Relaxed);

            let mark = Instant::now();
            scratch.labels.clear();
            scratch.labels.resize(scratch.log.len(), 0.0);
            let seeded = seed_maxima(
                &scratch.log,
                &mut scratch.labels,
                width,
                params.threshold,
                &mut scratch.candidates,
                &mut scratch.stack,
            );
            clocks.maxima.fetch_add(nanos(mark), Ordering::Relaxed);
            markers.fetch_add(seeded, Ordering::Relaxed);

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
            filter: seconds(&clocks.filter),
            blur: seconds(&clocks.blur),
            laplacian: seconds(&clocks.laplacian),
            maxima: seconds(&clocks.maxima),
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

/// Labels regional maxima above `threshold`, returning how many were found.
///
/// A pixel is a candidate when no neighbour is strictly greater; candidates are
/// then grouped into 8-connected components, and each component seeds one
/// marker. Grouping is the whole point. A *strict* local-maximum test — every
/// neighbour strictly smaller — finds **nothing at all** on this data, because
/// the morphological opening upstream creates plateaus by construction: on one
/// tissue tile it gives 0 strict maxima against 15,751 candidates. Seeding per
/// candidate instead would give thousands of basins per nucleus.
///
/// **This is not what QuPath does.** `WatershedCellDetection` calls
/// `MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false)` and
/// then `RoiLabeling.labelImage(ipTemp, 0, false)`. That is an h-maxima
/// transform: it removes the candidate maxima, fills the holes back in by
/// geodesic dilation under the original image, and keeps only those whose
/// *prominence* — how far they stand above the highest saddle reaching them —
/// is above zero. That prominence test is what stops one nucleus producing
/// several markers, and this function has no equivalent, so it over-segments.
///
/// Two differences to carry over when it is ported: QuPath labels the survivors
/// **4-connected**, where the grouping below is 8-connected; and it runs the
/// whole transform inside the processor's ROI, where the Laplacian on the line
/// before it runs over the whole image.
///
/// The counts this produces are therefore basins, not cells, and nothing pins
/// them against QuPath — see the crate documentation and `docs/ROADMAP.md`.
fn seed_maxima(
    values: &[f32],
    labels: &mut [f32],
    width: usize,
    threshold: f32,
    candidates: &mut Vec<bool>,
    stack: &mut Vec<u32>,
) -> u64 {
    let height = values.len() / width;
    if width < 3 || height < 3 {
        return 0;
    }

    candidates.clear();
    candidates.resize(values.len(), false);

    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let index = y * width + x;
            let centre = values[index];
            if centre <= threshold {
                continue;
            }
            let dominant = NEIGHBOURS.iter().all(|&(dx, dy)| {
                let neighbour = index
                    .wrapping_add_signed(dy * width as isize)
                    .wrapping_add_signed(dx);
                values[neighbour] <= centre
            });
            candidates[index] = dominant;
        }
    }

    // One marker per connected component of candidates.
    let mut next_label = 1.0f32;
    let mut found = 0u64;
    for start in 0..values.len() {
        if !candidates[start] {
            continue;
        }

        stack.clear();
        stack.push(u32::try_from(start).unwrap_or(u32::MAX));
        candidates[start] = false;
        labels[start] = next_label;

        while let Some(index) = stack.pop() {
            let index = index as usize;
            let (x, y) = (index % width, index / width);
            if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
                continue;
            }
            for &(dx, dy) in &NEIGHBOURS {
                let neighbour = index
                    .wrapping_add_signed(dy * width as isize)
                    .wrapping_add_signed(dx);
                if candidates[neighbour] {
                    candidates[neighbour] = false;
                    labels[neighbour] = next_label;
                    stack.push(u32::try_from(neighbour).unwrap_or(u32::MAX));
                }
            }
        }

        next_label += 1.0;
        found += 1;
    }

    found
}

/// The eight neighbours, as `(dx, dy)`.
const NEIGHBOURS: [(isize, isize); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

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

    #[test]
    fn a_single_peak_seeds_one_marker() {
        let width = 9;
        let mut values = vec![0.0f32; width * 9];
        values[4 * width + 4] = 1.0;
        let mut labels = vec![0.0f32; values.len()];
        let (mut candidates, mut stack) = (Vec::new(), Vec::new());
        assert_eq!(
            seed_maxima(
                &values,
                &mut labels,
                width,
                0.1,
                &mut candidates,
                &mut stack
            ),
            1
        );
        assert_eq!(labels[4 * width + 4], 1.0);
    }

    #[test]
    fn a_plateau_seeds_exactly_one_marker() {
        // The failure this replaced: a strict local-maximum test finds nothing
        // on a plateau, and the morphological opening upstream produces
        // plateaus everywhere. Seeding per candidate pixel would give 25.
        let width = 9;
        let mut values = vec![0.0f32; width * 9];
        for y in 2..7 {
            for x in 2..7 {
                values[y * width + x] = 1.0;
            }
        }
        let mut labels = vec![0.0f32; values.len()];
        let (mut candidates, mut stack) = (Vec::new(), Vec::new());
        assert_eq!(
            seed_maxima(
                &values,
                &mut labels,
                width,
                0.1,
                &mut candidates,
                &mut stack
            ),
            1,
            "a plateau is one maximum, not one per pixel"
        );
        assert_eq!(labels.iter().filter(|&&l| l != 0.0).count(), 25);
    }

    #[test]
    fn two_separated_peaks_seed_two_markers() {
        let width = 11;
        let mut values = vec![0.0f32; width * 5];
        values[2 * width + 2] = 1.0;
        values[2 * width + 8] = 1.0;
        let mut labels = vec![0.0f32; values.len()];
        let (mut candidates, mut stack) = (Vec::new(), Vec::new());
        assert_eq!(
            seed_maxima(
                &values,
                &mut labels,
                width,
                0.1,
                &mut candidates,
                &mut stack
            ),
            2
        );
    }

    #[test]
    fn background_below_the_threshold_seeds_nothing() {
        let width = 9;
        let mut values = vec![0.0f32; width * 9];
        values[4 * width + 4] = 0.05;
        let mut labels = vec![0.0f32; values.len()];
        let (mut candidates, mut stack) = (Vec::new(), Vec::new());
        assert_eq!(
            seed_maxima(
                &values,
                &mut labels,
                width,
                0.1,
                &mut candidates,
                &mut stack
            ),
            0
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
