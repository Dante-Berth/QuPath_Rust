//! Cell expansion: growing nucleus labels outward into the cells the seam
//! resolution actually compares.
//!
//! `WatershedCellDetection` reaches this through five lines:
//!
//! ```java
//! FloatProcessor fpEDM = new EDM().makeFloatEDM(bp, (byte)255, false);
//! fpEDM.multiply(-1);
//! double cellExpansionThreshold = -cellExpansion;
//! ImageProcessor ipLabelsCells = ipLabels.duplicate();
//! Watershed.doWatershed(fpEDM, ipLabelsCells, cellExpansionThreshold, false);
//! ```
//!
//! Every piece of that is already ported — the distance map, the flood — so this
//! module is the composition and the two constants that make it mean "grow each
//! nucleus by at most `cellExpansion` pixels, and split the contested space
//! along the midline".
//!
//! `cellExpansion` defaults to 5 µm and is divided by the pixel size before it
//! arrives, so on CMU-1 it is about 21.75 px and never an integer. It is never
//! zero on a default run, which is what makes `DIVERGENCES.md` D-008 real: the
//! 10% overlap test runs on these cells, not on the nuclei that seed them.
//!
//! # The three things that decide the answer
//!
//! - **The distance map's background is 255, not 0.** Every other EDM in this
//!   pipeline measures distance *inside* the objects; this one measures it
//!   *outward from* them. See [`DistanceMap::make_float_edm_from`]. Reusing the
//!   0-background call gives a map of the wrong region entirely and the cells
//!   come out as the gaps between nuclei.
//! - **The negation is what turns a distance into a priority.** The flood pops
//!   high values first, so negating makes it claim nearest pixels first, and
//!   that is what puts the boundary between two cells on the midline rather
//!   than wherever the queue happened to arrive.
//! - **The threshold is `-cell_expansion`, and the flood skips values `<=` it.**
//!   After the negation that reads as "further than `cell_expansion` from any
//!   nucleus", with the tie falling on the far side of what one would guess: a
//!   pixel at *exactly* `cell_expansion` has value `-cell_expansion`, which is
//!   not `>` the threshold, so it is **not** claimed. `exact-distance-boundary`
//!   in `parity/fixtures/CellExpansion.json` sits on that tie — 20 pixels at
//!   exactly 4.0 px, none of them claimed.
//!
//! # What a cell owns is not what the flood labelled
//!
//! The flood leaves a watershed line: where two labels meet, the pixel is
//! claimed by neither and stays zero. QuPath does not hand those labels to the
//! overlap test. It calls `RoiLabeling.labelsToFilledROIs`, which traces each
//! label **eight-connected** and *fills* the traced ring — so a cell owns the
//! polygon its label encloses, including the watershed line on its own side and
//! any interior pixel the flood missed. [`cell_areas`] is that count.
//!
//! Pinned by `parity/fixtures/CellExpansion.json`, recorded from a real QuPath.

use crate::{Connectivity, DistanceMap, RegionTracer, Watershed};

/// The value the distance map treats as background for cell expansion.
///
/// `(byte)255` at the call site. ImageJ sign-extends the mask byte and compares
/// it to the `int` argument, so `-1` matches `-1`; the mask is `u8` here and the
/// comparison is against `255`, which is the same set of pixels.
const EXPANSION_BACKGROUND: u8 = 255;

/// Grows each non-zero label in `labels` outward over `mask`'s background, by at
/// most `cell_expansion` pixels, splitting contested space along the midline.
///
/// `mask` is the 0/255 nucleus binary and `labels` the nucleus labels — the two
/// things the call site already holds. `labels` is expanded **in place**, which
/// is `ipLabels.duplicate()` plus the flood: the caller keeps its own copy of
/// the nucleus labels if it still needs them, exactly as QuPath does.
///
/// `edm` is scratch, sized like `mask`; it comes back holding the negated
/// distance map the flood ran on rather than being cleared, so a caller that
/// wants that surface can read it.
///
/// # Panics
///
/// Panics if `mask`, `labels` and `edm` do not describe the same image, or if
/// `width` is zero or does not divide their length.
pub fn expand_labels(
    mask: &[u8],
    labels: &mut [f32],
    edm: &mut [f32],
    width: usize,
    cell_expansion: f64,
) {
    assert_eq!(
        mask.len(),
        labels.len(),
        "the mask and the labels must describe the same image"
    );
    assert_eq!(
        mask.len(),
        edm.len(),
        "the mask and the distance map must describe the same image"
    );

    DistanceMap::new().make_float_edm_from(mask, edm, width, EXPANSION_BACKGROUND);

    // `fpEDM.multiply(-1)`. The flood pops high values first, so this is what
    // makes it expand outward in increasing distance.
    for distance in edm.iter_mut() {
        *distance = -*distance;
    }

    // `cellExpansionThreshold = -cellExpansion`, and the flood skips `<=` it.
    // The cast is the same narrowing QuPath does when the double threshold meets
    // the float image, and it is the only place the expansion's precision
    // matters: a fractional expansion is the normal case.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "QuPath compares a double threshold against float pixels; \
                  narrowing here reproduces that comparison rather than \
                  widening every pixel"
    )]
    let threshold = -cell_expansion as f32;

    Watershed::new().flood(edm, labels, width, threshold, Connectivity::Four);
}

/// The number of pixels each cell owns, indexed by label minus one.
///
/// This is `RoiLabeling.labelsToFilledROIs` followed by counting each polygon's
/// filled area, which is the number the 10% overlap test turns on. It is **not**
/// the count of pixels carrying that label: the polygon is traced and then
/// *filled*, so a cell also owns any pixel its own boundary encloses — the
/// watershed line on its own side, and anything the flood left unlabelled
/// inside it.
///
/// `nucleus_count` is `roisNuclei.size()` at the call site; labels above it are
/// ignored, matching the `val > 0 && val <= n` test. A label with no pixels
/// gives `None`, which is QuPath's null entry.
///
/// # Why this traces one label at a time
///
/// [`filled_regions`] renumbers what it finds in scan order, which is right
/// where the labelling *is* the numbering. Here it is not: a cell keeps the
/// label of the nucleus that seeded it, `rois[(int)val-1] = roi`, and the scan
/// order need not agree with it. So each label is traced against its own value
/// range and the count is stored at its own index.
///
/// # Panics
///
/// Panics if `width` is zero or does not divide `labels.len()`.
#[must_use]
pub fn cell_areas(labels: &[f32], width: usize, nucleus_count: usize) -> Vec<Option<u32>> {
    assert!(width > 0, "width must be non-zero");
    assert_eq!(
        labels.len() % width,
        0,
        "width must divide the buffer length"
    );

    let mut areas = vec![None; nucleus_count];
    let mut regions = vec![0u32; labels.len()];
    let mut tracer = RegionTracer::new();

    for (slot, area) in areas.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "labels are small positive integers held in f32;                       nucleus_count cannot reach f32's integer limit for an                       image this pipeline tiles"
        )]
        let label = (slot + 1) as f32;

        // `wand.autoOutline(x, y, val, val, ...)`: the region is the pixels
        // equal to this label, and the trace fills what the ring encloses.
        let found = tracer.trace_into(labels, &mut regions, width, label, label);
        if found == 0 {
            continue;
        }

        // A label the flood left in two disconnected pieces would trace as more
        // than one region. QuPath keeps only the one its scan reached first —
        // `rois[(int)val-1] = roi` overwrites, and the fill marks the rest
        // completed — so region 1, which is the first in raster order, is the
        // cell.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the count is bounded by the pixel count, which trace_into \
                      already asserts fits a u32 index"
        )]
        let owned = regions.iter().filter(|&&region| region == 1).count() as u32;
        *area = Some(owned);
    }

    areas
}
