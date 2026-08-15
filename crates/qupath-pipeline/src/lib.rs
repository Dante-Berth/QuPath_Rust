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
//! Every image-processing stage QuPath runs before the watershed is here and
//! pinned against QuPath's own output — background subtraction by
//! reconstruction, the Laplacian of Gaussian, the regional maxima, the
//! labelling and the flood — and so now are the first two things it does
//! afterwards: tracing each basin to a hole-filled region, and dropping the
//! ones whose mean intensity or background overlap disqualifies them.
//!
//! and so is everything between that filter and the nuclei QuPath goes on to
//! measure: the merge, the shape split, the boundary refinement, and the area
//! filter that runs on the objects those leave.
//!
//! **ROI vertices** used to be on that list and are not any more: [`contour`]
//! traces a filled region to the polygon that bounds it, and
//! [`run_collecting`] hands back [`Detections`] — outlines, bounding boxes and
//! two measurements, columnar per §4. [`run`] is unchanged and still returns
//! counts only, because it is the one that has to keep §6's memory bound.
//!
//! What is still absent:
//!
//! - **Cell expansion and measurement.** Neither changes the nucleus count.
//! - **Boundary smoothing.** QuPath interpolates and simplifies each polygon
//!   before anything else sees it; these outlines are the raw pixel boundary.
//!   That is half of what `DIVERGENCES.md` D-008 is about, and having the
//!   vertices is what makes it possible to look at for the first time.
//!
//! So [`Outcome::objects`] is now a count of the same objects QuPath's
//! `roisNuclei` holds after its area filter — see `docs/ROADMAP.md` §1.5 for
//! what that is worth and what it is not.
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

mod contour;
mod detections;
mod merge;
mod objects;
mod overlaps;
mod refine;
mod stain;
mod tiles;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use qupath_filters::{Convolver, GaussianBlur, RankFilter, RankOp, Reconstructor};
use qupath_slide::{Slide, SlideError};
use qupath_watershed::{Connectivity, DistanceMap, Labeller, RegionTracer, Segmenter, Watershed};
use rayon::prelude::*;

pub use contour::{shoelace_area_x2, trace_outline_into};
pub use detections::{AREA, Detections, MEAN};
pub use merge::{FOREGROUND, Merger};
pub use objects::{Extent, ObjectFilter, RegionStats};
pub use overlaps::{Candidate, resolve_overlaps};
pub use refine::{BoundaryRefiner, CLEANUP_SIGMA, SIGMA_FLOOR};
pub use stain::Stains;
pub use tiles::{MAX_TILE_SIZE, PREFERRED_TILE_SIZE, Tile, tile_grid, tile_overlap};

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
    /// Mean intensity a traced candidate must **exceed** to survive.
    ///
    /// This is QuPath's `threshold` parameter, 0.1 by default, and it is a
    /// different number from [`Params::threshold`] above: that one gates the
    /// flood and QuPath hardcodes it to zero, while this one gates objects.
    ///
    /// The mean is taken over `ipToMeasure` — the optical density *after* the
    /// background subtraction and *before* the blur and the Laplacian — across
    /// the candidate's filled region. The test is `mean <= threshold` drops, so
    /// a candidate landing exactly on this value does not survive.
    pub intensity_threshold: f64,
    /// Flood connectivity.
    pub connectivity: Connectivity,
    /// Preferred side of a processing tile, in pixels.
    ///
    /// Smallest area an object may have and survive, in pixels, or zero to
    /// disable the test.
    ///
    /// QuPath asks for 10 µm² and divides by the **squared** pixel size, which
    /// on CMU-1's 0.4990 is 40.16 px². The area is
    /// [`RegionStats::area`] — ImageJ's `pixelCount`, which skips non-finite
    /// pixels — rather than the region's size.
    ///
    /// Disabling this **and** [`Params::max_area`] disables the second mean test
    /// with them; see [`ObjectFilter::apply_area`].
    pub min_area: f64,
    /// Largest area an object may have and survive, in pixels, or zero to
    /// disable the test. QuPath's 400 µm², which is 1606.42 px² on CMU-1.
    pub max_area: f64,
    /// `AbstractTileableDetectionPlugin.PREFERRED_TILE_SIZE * downsampleFactor`.
    /// **This is not the slide's TIFF tile size**, which on CMU-1 is 240 px:
    /// detection tiles are its own grid, an order of magnitude larger, and
    /// processing the TIFF's tiles instead puts a seam every 240 px where QuPath
    /// has one every 2000.
    pub tile_preferred: u32,
    /// Largest region processed in one pass, in pixels.
    ///
    /// `MAX_TILE_SIZE * downsampleFactor`. A region no larger than this in both
    /// directions is **not tiled at all** — `computeTiledROIs` hands back the
    /// parent ROI itself — so it has no seam and no halo.
    pub tile_max: u32,
    /// Halo read around every processing tile, in pixels.
    ///
    /// `WatershedCellDetection.getTileOverlap`, which is
    /// `(10 µm + cellExpansion) / pixelSize * 2` — 60 px on CMU-1. It is derived
    /// from the cell expansion, **not** from any filter's kernel radius, and the
    /// tile is grown by it on all four sides rather than offset by it.
    pub tile_overlap: u32,
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
            // QuPath's `threshold` parameter. Nothing scales it: it is an
            // optical density, so it is independent of the pixel size.
            intensity_threshold: 0.1,
            // 10 and 400 µm² over CMU-1's 0.4990² µm² per pixel. Note the
            // *squared* pixel size: an area converts differently from a radius,
            // and using the radius conversion here would be out by 2x.
            min_area: 10.0 / (0.4990 * 0.4990),
            max_area: 400.0 / (0.4990 * 0.4990),
            // `Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false)` — the
            // `false` is `conn8`. Four, not eight, and not the same as the
            // 8-connected neighbourhood the maxima test uses one line earlier.
            connectivity: Connectivity::Four,
            // 2048 and 3072 at CMU-1's downsample of 0.5/0.4990 = 1.00200,
            // truncated as Java's `(int)` cast does.
            tile_preferred: 2052,
            tile_max: 3078,
            // (10 + 5) / 0.4990 * 2 = 60.12, truncated.
            tile_overlap: 60,
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
    /// The radius-16 erosion inside the background stage, which is rayon-parallel
    /// over rows.
    erode: AtomicU64,
    /// The geodesic dilation inside the background stage, which is not.
    regrow: AtomicU64,
    blur: AtomicU64,
    laplacian: AtomicU64,
    maxima: AtomicU64,
    label: AtomicU64,
    flood: AtomicU64,
    trace: AtomicU64,
    filter: AtomicU64,
    merge: AtomicU64,
    split: AtomicU64,
    refine: AtomicU64,
    /// The second trace, over the refined binary, and the area filter over it.
    nuclei: AtomicU64,
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
    /// The erosion within [`Stages::background`]. Parallel over rows, so this
    /// is wall time with the row work already spread across threads.
    pub erode: f64,
    /// The geodesic dilation within [`Stages::background`]. Strictly sequential:
    /// each raster pass reads the neighbour it just wrote.
    pub regrow: f64,
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
    /// Tracing each basin to a hole-filled region.
    pub trace: f64,
    /// The mean-intensity and background-mask tests over those regions.
    pub filter: f64,
    /// The merge: a 3x3 maximum, the intersection with the thresholded
    /// Laplacian, and the hole fill that follows them.
    pub merge: f64,
    /// The shape split: the Euclidean distance map and the segmentation
    /// `MaximumFinder` derives from it.
    pub split: f64,
    /// The boundary refinement: a second Gaussian and a second Laplacian, at
    /// sigma 1, over the raw detection channel.
    pub refine: f64,
    /// Tracing the refined binary to nuclei, and the area filter over them.
    pub nuclei: f64,
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
            + self.trace
            + self.filter
            + self.merge
            + self.split
            + self.refine
            + self.nuclei
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
    /// Basins traced to hole-filled candidate regions, summed over tiles.
    ///
    /// Lower than [`Outcome::markers`] wherever two basins met with no
    /// watershed line between them, or one enclosed another.
    pub regions: u64,
    /// Candidates surviving the mean-intensity and background-mask tests,
    /// summed over tiles.
    ///
    /// QuPath never counts these; it fills them straight into `bp`. The count
    /// is here because §1.5b's table needs to know what each stage removes, and
    /// a stage's effect is only measurable against the stage before it.
    pub candidates: u64,
    /// Regions the merge leaves — QuPath's `rois2` — summed over tiles.
    ///
    /// Lower than [`Outcome::candidates`] wherever the dilation bridged two
    /// candidates the thresholded Laplacian already joined.
    pub merged: u64,
    /// Objects after the shape split and the boundary refinement, summed over
    /// tiles: QuPath's `roisNuclei`, before its area filter runs on them.
    pub nuclei: u64,
    /// Nuclei surviving the area filter, after duplicates across tile seams
    /// have been resolved.
    ///
    /// This is the object set QuPath goes on to measure and convert to
    /// detections. What separates it from a cell count is the tail — ROI
    /// vertices, cell expansion, measurements — none of which changes how many
    /// there are.
    pub objects: u64,
    /// Candidates a neighbouring tile had a better copy of, removed by
    /// [`resolve_overlaps`]. Zero when the region fits in one tile.
    pub duplicates: u64,
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
    /// One decoded TIFF tile, reused while assembling `rgb`. A processing tile
    /// spans many of them and does not line up with their grid.
    decoded: Vec<u8>,
    /// Haematoxylin optical density, then the same buffer with the background
    /// subtracted and the blur applied in place.
    density: Vec<f32>,
    /// QuPath's `fpDetection`: the haematoxylin optical density as the
    /// deconvolution left it, before the background subtraction and the blur.
    ///
    /// A third image, and the last stage in the tile is the only thing that
    /// reads it — `refineBoundary` duplicates `fpDetection` and filters it at
    /// sigma 1. It cannot be recovered from `to_measure` and the background
    /// estimate: adding the estimate back is not the bit pattern the subtraction
    /// started from.
    detection: Vec<f32>,
    /// The background estimate: an erosion of `density` grown back under it.
    background: Vec<f32>,
    /// Non-empty only where a region exceeded [`Params::max_background`], which
    /// on most tiles is never, so it usually stays at zero capacity.
    background_mask: Vec<f32>,
    /// QuPath's `ipToMeasure`: `density` copied once the background has been
    /// subtracted and before the blur touches it. The object filter measures
    /// this and nothing else measures it, which is why it is a separate buffer
    /// rather than a stage the pipeline could read back.
    to_measure: Vec<f32>,
    /// The Laplacian of Gaussian: written from `density`, and the image every
    /// stage after it reads. Separate because the convolution reads a
    /// neighbourhood and so cannot write over what it is still reading.
    log: Vec<f32>,
    /// How far each pixel of `log` stands above the saddle that reaches it.
    prominence: Vec<f32>,
    labels: Vec<f32>,
    /// Which traced candidate each pixel belongs to, holes included. Reused for
    /// the nuclei once the merge, the split and the refinement have run.
    regions: Vec<u32>,
    /// QuPath's `bp`: 255 inside an object, 0 outside. Every stage from the
    /// merge onwards reads and writes this rather than a float image.
    binary: Vec<u8>,
    /// Bounding box and filled area of each traced candidate. Populated only
    /// when the region is tiled, since only a seam can need it.
    extents: Vec<Extent>,
    /// One outline while it is being traced, reused for every object of every
    /// tile. A `Vec` per object would be one allocation per detection.
    ring: Vec<[u32; 2]>,
    blur: GaussianBlur,
    filter: RankFilter,
    convolver: Convolver,
    reconstructor: Reconstructor,
    labeller: Labeller,
    flooder: Watershed,
    tracer: RegionTracer,
    objects: ObjectFilter,
    merger: Merger,
    distances: DistanceMap,
    segmenter: Segmenter,
    refiner: BoundaryRefiner,
}

thread_local! {
    /// One scratch per worker thread, reused across every tile that thread ever
    /// handles and across runs.
    ///
    /// **Not `map_init`**, which is what this used to be. `map_init` calls its
    /// initialiser once per rayon *job*, not once per thread, and rayon splits
    /// into more jobs when there are more items — so peak memory grew with the
    /// number of tiles rather than with the thread count. That was invisible
    /// while a tile was 256x256 and 2 MB of scratch; at QuPath's 2048 px tiles
    /// each one is ~45 MB and it is the difference between a bounded pipeline
    /// and an unbounded one. A thread-local is one per OS thread by
    /// construction, which is exactly the bound §6 asks for.
    static SCRATCH: std::cell::RefCell<Scratch> = std::cell::RefCell::new(Scratch::new());
}

impl Scratch {
    fn new() -> Self {
        Self {
            rgb: Vec::new(),
            decoded: Vec::new(),
            density: Vec::new(),
            detection: Vec::new(),
            background: Vec::new(),
            background_mask: Vec::new(),
            to_measure: Vec::new(),
            log: Vec::new(),
            prominence: Vec::new(),
            labels: Vec::new(),
            regions: Vec::new(),
            binary: Vec::new(),
            extents: Vec::new(),
            ring: Vec::new(),
            blur: GaussianBlur::new(),
            // Sequential rows: this scratch is already inside one rayon job per
            // tile, and §6 forbids nesting. Nesting here also re-enters the
            // thread-local scratch above, because rayon is free to resume
            // another tile's job on this thread while the inner filter blocks.
            filter: RankFilter::new().with_parallel_rows(false),
            convolver: Convolver::new().with_parallel_rows(false),
            reconstructor: Reconstructor::new(),
            labeller: Labeller::new(),
            flooder: Watershed::new(),
            tracer: RegionTracer::new(),
            objects: ObjectFilter::new(),
            merger: Merger::new(),
            distances: DistanceMap::new(),
            segmenter: Segmenter::new(),
            refiner: BoundaryRefiner::new(),
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
/// [`run_collecting`] is the same run with the objects kept.
///
/// # Errors
///
/// Fails if the level does not exist, the region leaves it, or a tile cannot
/// be decoded.
pub fn run(slide: &Slide, region: Region, params: Params) -> Result<Outcome, PipelineError> {
    let (outcome, _) = run_inner(slide, region, params, false)?;
    Ok(outcome)
}

/// The same run as [`run`], keeping the objects it found.
///
/// Every count in the returned [`Outcome`] is identical to what [`run`] gives
/// for the same arguments — collecting changes nothing about what is detected,
/// and `the_two_entry_points_agree_on_every_count` asserts it rather than
/// asserting it in a comment. [`Detections::len`] equals [`Outcome::objects`],
/// duplicates across tile seams already removed.
///
/// # Memory
///
/// This is the one function in the crate whose output grows with the region,
/// which is why it is a separate entry point rather than a flag — see
/// `detections`' module documentation. Budget roughly **250 bytes per object**
/// and ask for a region rather than a slide.
///
/// # Errors
///
/// Fails if the level does not exist, the region leaves it, or a tile cannot
/// be decoded.
pub fn run_collecting(
    slide: &Slide,
    region: Region,
    params: Params,
) -> Result<(Outcome, Detections), PipelineError> {
    let (outcome, detections) = run_inner(slide, region, params, true)?;
    Ok((
        outcome,
        detections.expect("run_inner returns detections when asked for them"),
    ))
}

fn run_inner(
    slide: &Slide,
    region: Region,
    params: Params,
    collect: bool,
) -> Result<(Outcome, Option<Detections>), PipelineError> {
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

    // The detection grid, which is **not** the slide's TIFF tile grid: QuPath
    // tiles at 2048 px and pads by the cell expansion, where the TIFF stores
    // 240 px tiles. Processing the storage grid put a seam every 240 px and is
    // what ROADMAP §2 is about.
    let tiles = tile_grid(
        region,
        params.tile_preferred,
        params.tile_max,
        params.tile_overlap,
    );

    let clocks = StageClocks::default();
    let markers = AtomicU64::new(0);
    let labelled = AtomicU64::new(0);
    let regions = AtomicU64::new(0);
    let candidates = AtomicU64::new(0);
    let merged = AtomicU64::new(0);
    let nuclei = AtomicU64::new(0);
    let objects = AtomicU64::new(0);
    let pixels = AtomicU64::new(0);

    // CMU-1 carries no stain estimate of its own, so QuPath falls back to these
    // defaults and this matches it. A slide that does carry one would need
    // `Stains::from_two` with the values from its metadata.
    let stains = Stains::h_and_e();

    // The padded rectangles, so a worker can tell whether one of its candidates
    // reaches a seam without knowing anything else about the grid.
    let padded: Vec<Region> = tiles.iter().map(|tile| tile.padded).collect();

    let started = Instant::now();
    let outcomes: Result<Vec<TileHarvest>, PipelineError> = tiles
        .par_iter()
        .enumerate()
        .map(|(index, tile)| {
            SCRATCH.with_borrow_mut(|scratch| {
                let rect = tile.padded;
                let width = rect.width as usize;
                pixels.fetch_add(
                    u64::from(rect.width) * u64::from(rect.height),
                    Ordering::Relaxed,
                );

                let mark = Instant::now();
                slide.read_region_into(
                    params.level,
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    &mut scratch.rgb,
                    &mut scratch.decoded,
                )?;
                clocks.decode.fetch_add(nanos(mark), Ordering::Relaxed);

                let mark = Instant::now();
                stains.haematoxylin_into(&scratch.rgb, &mut scratch.density);
                // `fpLoG = (FloatProcessor)fpDetection.duplicate()`, the other
                // way round: the working image is the copy here and the
                // untouched one is kept, because `fpDetection` outlives every
                // stage that modifies `fpLoG` and is read once at the very end
                // by `refineBoundary`.
                if params.sigma > SIGMA_FLOOR {
                    scratch.detection.clear();
                    scratch.detection.extend_from_slice(&scratch.density);
                }
                clocks.deconvolve.fetch_add(nanos(mark), Ordering::Relaxed);

                // Stage order follows `WatershedCellDetection`: the background is
                // estimated and subtracted from the detection image *before* the
                // Laplacian of Gaussian, not after it.
                let mark = Instant::now();
                let mut background_masked = false;
                if params.background_radius > 0.0 {
                    background_masked = subtract_background(scratch, width, params, &clocks);
                }
                // `ipToMeasure = fpLoG.duplicate()` when a background was
                // subtracted, and `ipToMeasure = fpDetection` when none was. Taking
                // the copy here covers both: `density` is the subtracted image in
                // the first case and the untouched optical density in the second.
                // Timed with the stage that produced it; it is one tile memcpy.
                scratch.to_measure.clear();
                scratch.to_measure.extend_from_slice(&scratch.density);
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
                // `apply_into` writes every pixel of its output, edges included.
                fit(&mut scratch.log, scratch.density.len(), 0.0);
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

                // `getFilledPolygonROIs(ipNucleusLabels, Wand.FOUR_CONNECTED)`
                // against a threshold of 0.5: basins become candidate objects, with
                // their holes filled and anything they enclose absorbed.
                let mark = Instant::now();
                // The trace clears the region map itself, so zeroing it here was
                // the same 18 MB twice.
                fit(&mut scratch.regions, scratch.labels.len(), 0);
                let traced = scratch.tracer.trace_into(
                    &scratch.labels,
                    &mut scratch.regions,
                    width,
                    0.5,
                    f32::INFINITY,
                );
                clocks.trace.fetch_add(nanos(mark), Ordering::Relaxed);
                regions.fetch_add(u64::from(traced), Ordering::Relaxed);

                let mark = Instant::now();
                let background = if background_masked {
                    Some(scratch.background_mask.as_slice())
                } else {
                    None
                };
                let kept = scratch.objects.apply(
                    &scratch.to_measure,
                    &scratch.regions,
                    traced,
                    params.intensity_threshold,
                    background,
                );
                clocks.filter.fetch_add(nanos(mark), Ordering::Relaxed);
                candidates.fetch_add(u64::from(kept), Ordering::Relaxed);

                let found = finish_objects(scratch, width, params, &clocks);
                merged.fetch_add(u64::from(found.merged), Ordering::Relaxed);
                nuclei.fetch_add(u64::from(found.nuclei), Ordering::Relaxed);
                objects.fetch_add(u64::from(found.kept), Ordering::Relaxed);

                // Everything above is per-tile and final. What is left is the one
                // thing a tile cannot decide alone: whether a neighbour found the
                // same nucleus. Only objects reaching another tile's rectangle
                // can, so only those get a mask built for them.
                Ok(harvest(
                    scratch,
                    index,
                    tile,
                    &padded,
                    width,
                    found.nuclei,
                    collect.then_some(params.level),
                ))
            })
        })
        .collect();
    let mut harvests = outcomes?;

    // `ParallelDetectionTileManager.postprocess`: every tile is done, so the
    // duplicates in the overlap bands can now be resolved. The tiles' candidate
    // lists are concatenated in tile order and each tile's offset into the
    // result is kept, so a dead candidate can be traced back to the detection
    // it came from.
    let mut seam_candidates: Vec<Candidate> = Vec::new();
    let mut offsets = Vec::with_capacity(harvests.len());
    for harvest in &mut harvests {
        offsets.push(seam_candidates.len());
        seam_candidates.append(&mut harvest.seam);
    }
    let duplicates = resolve_overlaps(&padded, &mut seam_candidates);

    let detections = collect.then(|| {
        let mut all = Detections::new(params.level);
        for (harvest, &offset) in harvests.iter().zip(&offsets) {
            let Some(tile) = harvest.detected.as_ref() else {
                continue;
            };
            // Everything survives unless a seam candidate says otherwise, which
            // is the same statement `Outcome::objects` makes by subtracting
            // `duplicates` from a total.
            let mut keep = vec![true; tile.len()];
            for (at, &row) in harvest.seam_rows.iter().enumerate() {
                if !seam_candidates[offset + at].alive() {
                    keep[row as usize] = false;
                }
            }
            all.append_kept(tile, &keep);
        }
        all.shrink_to_fit();
        all
    });

    let elapsed = started.elapsed().as_secs_f64();

    Ok((
        Outcome {
            tiles: tiles.len(),
            pixels: pixels.load(Ordering::Relaxed),
            markers: markers.load(Ordering::Relaxed),
            labelled: labelled.load(Ordering::Relaxed),
            regions: regions.load(Ordering::Relaxed),
            candidates: candidates.load(Ordering::Relaxed),
            merged: merged.load(Ordering::Relaxed),
            nuclei: nuclei.load(Ordering::Relaxed),
            objects: objects.load(Ordering::Relaxed) - u64::from(duplicates),
            duplicates: u64::from(duplicates),
            elapsed,
            stages: Stages {
                decode: seconds(&clocks.decode),
                deconvolve: seconds(&clocks.deconvolve),
                background: seconds(&clocks.background),
                erode: seconds(&clocks.erode),
                regrow: seconds(&clocks.regrow),
                blur: seconds(&clocks.blur),
                laplacian: seconds(&clocks.laplacian),
                maxima: seconds(&clocks.maxima),
                label: seconds(&clocks.label),
                flood: seconds(&clocks.flood),
                trace: seconds(&clocks.trace),
                filter: seconds(&clocks.filter),
                merge: seconds(&clocks.merge),
                split: seconds(&clocks.split),
                refine: seconds(&clocks.refine),
                nuclei: seconds(&clocks.nuclei),
            },
            peak_resident_bytes: peak_resident_bytes(),
        },
        detections,
    ))
}

/// Gives `buffer` exactly `len` entries without rewriting the ones it already
/// has.
///
/// Every caller below hands the buffer straight to a stage that writes every
/// entry of it before anything reads one, so the zero-fill these used to do was
/// a memset nothing observed — 95 MB of it per 2120² tile across the five, on a
/// pipeline §4.8 measures as bound by memory bandwidth. Tiles are all one size
/// but for the grid's last row and column, so after a worker's first tile this
/// does nothing at all.
///
/// The length is exact rather than merely sufficient, so an index past the end
/// still panics instead of landing in the previous tile's leftovers. Each call
/// site names the stage that overwrites the buffer, and each of those stages
/// has a test that a poisoned buffer comes back the same as a zeroed one; the
/// convention is what this trades the memset for, so it is pinned rather than
/// trusted.
pub(crate) fn fit<T: Copy>(buffer: &mut Vec<T>, len: usize, fill: T) {
    if buffer.len() < len {
        buffer.resize(len, fill);
    } else {
        buffer.truncate(len);
    }
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
    // `label_into` writes every pixel before it reads one: the threshold test
    // sets each to a pending marker or to zero.
    fit(labels, scratch.log.len(), 0.0);
    let found =
        scratch
            .labeller
            .label_into(&scratch.prominence, labels, width, 0.0, Connectivity::Four);
    clocks.label.fetch_add(nanos(mark), Ordering::Relaxed);
    found
}

/// What one tile's run of the stages after the intensity filter produced.
struct Objects {
    /// Regions the merge left: QuPath's `rois2`.
    merged: u32,
    /// Objects the shape split and the boundary refinement left, which is what
    /// the area filter is handed: QuPath's `roisNuclei`.
    nuclei: u32,
    /// How many of those survived the area filter.
    kept: u32,
}

/// Everything `WatershedCellDetection` does between the intensity filter and
/// the nuclei it goes on to measure.
///
/// ```java
/// bp.filter(ImageProcessor.MAX);
/// bp.copyBits(bpLoG, 0, 0, Blitter.AND);
/// ... fill holes ...
/// new EDM().toWatershed(bp);
/// ... refineBoundary ...
/// List<PolygonRoi> roisNuclei = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);
/// ... area filter ...
/// ```
///
/// The tile has three float images it is finished with by the time this runs —
/// `log` once the merge's intersection has read it, `prominence` once the
/// labelling has, `labels` once the trace has — so the distance map and the
/// refinement's two working images come out of those rather than out of three
/// more allocations. At 2120 px tiles and 20 workers that is the difference
/// between the peak this pipeline claims and 1 GB more of it.
fn finish_objects(
    scratch: &mut Scratch,
    width: usize,
    params: Params,
    clocks: &StageClocks,
) -> Objects {
    // `bp`: a fresh ByteProcessor with every surviving candidate filled.
    let mark = Instant::now();
    // `write_binary` writes every pixel: a region's number or zero.
    fit(&mut scratch.binary, scratch.regions.len(), 0);
    scratch
        .objects
        .write_binary(&scratch.regions, &mut scratch.binary);

    let merged = scratch.merger.merge(
        &mut scratch.binary,
        &scratch.log,
        width,
        &mut scratch.tracer,
    );
    clocks.merge.fetch_add(nanos(mark), Ordering::Relaxed);

    // `new EDM().toWatershed(bp)`. The Laplacian has been read for the last
    // time by the merge above, so its buffer becomes the distance map.
    let mark = Instant::now();
    scratch
        .distances
        .make_float_edm(&scratch.binary, &mut scratch.log, width);
    scratch
        .segmenter
        .segment(&scratch.log, &mut scratch.binary, width);
    clocks.split.fetch_add(nanos(mark), Ordering::Relaxed);

    // `if (refineBoundary && sigma > 1.5)`. `refineBoundary` is a hard-coded
    // `true`, so the sigma is the whole of the condition.
    let mark = Instant::now();
    if params.sigma > SIGMA_FLOOR {
        let mut smoothed = std::mem::take(&mut scratch.log);
        scratch.refiner.refine(
            &scratch.detection,
            &mut scratch.binary,
            width,
            &mut smoothed,
            &mut scratch.prominence,
        );
        scratch.log = smoothed;
    }
    clocks.refine.fetch_add(nanos(mark), Ordering::Relaxed);

    // `getFilledPolygonROIs(bp, FOUR_CONNECTED)` again, on the refined binary,
    // and then the area filter over what it finds.
    let mark = Instant::now();
    // As above: the trace clears the region map itself.
    fit(&mut scratch.regions, scratch.binary.len(), 0);
    let nuclei = scratch
        .tracer
        .trace_mask_into(&scratch.binary, &mut scratch.regions, width);
    let kept = scratch.objects.apply_area(
        &scratch.to_measure,
        &scratch.regions,
        nuclei,
        params.intensity_threshold,
        params.min_area,
        params.max_area,
    );
    clocks.nuclei.fetch_add(nanos(mark), Ordering::Relaxed);

    Objects {
        merged,
        nuclei,
        kept,
    }
}

/// What one tile hands back to the caller after every stage has run on it.
///
/// Two things, and only the first is ever needed: the candidates a neighbouring
/// tile might have found a second copy of, and — when the caller asked for them
/// — the objects themselves.
struct TileHarvest {
    /// Objects reaching another tile's rectangle, for [`resolve_overlaps`].
    seam: Vec<Candidate>,
    /// The row of [`TileHarvest::detected`] each entry of
    /// [`TileHarvest::seam`] came from, in the same order.
    ///
    /// Empty when nothing was collected. This is the only thing tying a dead
    /// candidate back to the detection it must delete, and the ordering is what
    /// makes it a `u32` rather than a map: both lists are built in one pass.
    seam_rows: Vec<u32>,
    /// The tile's objects, when [`run_collecting`] asked for them.
    detected: Option<Detections>,
}

/// Builds the objects of one tile that another tile could also have found, and
/// optionally every object it found at all.
///
/// A candidate matters to [`resolve_overlaps`] only if its bounding box reaches
/// some other tile's rectangle. On this grid the overlap bands are 60 px of a
/// 2000 px core, so this keeps a few percent of the objects and leaves the rest
/// without a mask — which is the difference between carrying pixel sets for
/// every detection in the region and carrying them for the seams.
///
/// `collect` is the level to record detections in, or `None` to record none. It
/// is a level rather than a `bool` because [`Detections`] is meaningless without
/// knowing which pyramid level its coordinates are in.
///
/// Returns nothing at all for a single-tile run that asked for no detections,
/// where there is neither a seam nor anything to keep.
fn harvest(
    scratch: &mut Scratch,
    index: usize,
    tile: &Tile,
    padded: &[Region],
    width: usize,
    count: u32,
    collect: Option<usize>,
) -> TileHarvest {
    let seams_exist = padded.len() > 1;
    let mut detected = collect.map(Detections::new);
    if !seams_exist && detected.is_none() {
        return TileHarvest {
            seam: Vec::new(),
            seam_rows: Vec::new(),
            detected: None,
        };
    }

    let mut extents = std::mem::take(&mut scratch.extents);
    scratch
        .objects
        .extents_into(&scratch.regions, width, count, &mut extents);
    let mut ring = std::mem::take(&mut scratch.ring);

    let origin = (tile.padded.x, tile.padded.y);
    let mut seam = Vec::new();
    let mut seam_rows = Vec::new();
    for (at, extent) in extents.iter().enumerate() {
        if !scratch.objects.kept()[at] || extent.area == 0 {
            continue;
        }
        let bounds = Region {
            x: origin.0 + extent.x,
            y: origin.1 + extent.y,
            width: extent.width,
            height: extent.height,
        };
        let region_index = u32::try_from(at + 1).expect("region count fits u32");

        // Traced before the seam test, because a collected object is kept
        // whether or not it reaches one and the two lists must stay aligned.
        let row = detected.as_mut().map(|detected| {
            ring.clear();
            contour::trace_outline_into(
                &scratch.regions,
                width,
                region_index,
                *extent,
                origin,
                &mut ring,
            );
            // The mean the area filter already computed, not a second pass over
            // the pixels: a detection's measurement and the reason it survived
            // cannot then disagree.
            detected.push(bounds, &ring, extent.area, scratch.objects.stats()[at].mean)
        });

        let reaches_a_seam = seams_exist
            && padded.iter().enumerate().any(|(other, rect)| {
                other != index && overlaps::intersect(bounds, *rect).is_some()
            });
        if !reaches_a_seam {
            continue;
        }
        seam.push(Candidate::new(
            u32::try_from(index).expect("tile count fits u32"),
            region_index,
            *extent,
            &scratch.regions,
            width,
            origin,
        ));
        if let Some(row) = row {
            seam_rows.push(u32::try_from(row).expect("object count fits u32"));
        }
    }

    scratch.extents = extents;
    scratch.ring = ring;
    TileHarvest {
        seam,
        seam_rows,
        detected,
    }
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
///
/// Returns whether `scratch.background_mask` was populated, which is QuPath's
/// `bpBackgroundMask != null`. The object filter reads that mask, so it is
/// cleared on every call rather than only when it is rebuilt: leaving a
/// previous tile's mask behind would reject this tile's candidates at
/// coordinates that mean nothing here.
fn subtract_background(
    scratch: &mut Scratch,
    width: usize,
    params: Params,
    clocks: &StageClocks,
) -> bool {
    let radius = params.background_radius;
    scratch.background_mask.clear();

    let mark = Instant::now();
    // The rank filter writes every pixel of its output.
    fit(&mut scratch.background, scratch.density.len(), 0.0);
    scratch.filter.apply_into(
        &scratch.density,
        &mut scratch.background,
        width,
        radius,
        RankOp::Min,
    );
    clocks.erode.fetch_add(nanos(mark), Ordering::Relaxed);

    // Regions too dark to be background at all. QuPath only allocates the mask
    // if some pixel exceeds the limit, and on tissue that is usually never, so
    // the common path is this scan and nothing else.
    let masked = params.max_background > 0.0
        && scratch
            .background
            .iter()
            .any(|&value| value > params.max_background);
    if masked {
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

    let mark = Instant::now();
    scratch
        .reconstructor
        .reconstruct(&mut scratch.background, &scratch.density, width);
    clocks.regrow.fetch_add(nanos(mark), Ordering::Relaxed);

    for (value, &estimate) in scratch.density.iter_mut().zip(&scratch.background) {
        *value -= estimate;
    }
    masked
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
        subtract_background(&mut scratch, width, params, &StageClocks::default());

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
