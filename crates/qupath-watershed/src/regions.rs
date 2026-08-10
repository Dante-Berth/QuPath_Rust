//! Traced object regions, matching `RoiLabeling.getFilledPolygonROIs`.
//!
//! This is the first thing `WatershedCellDetection` does to the flooded label
//! image, and it is where a basin becomes a candidate object:
//!
//! ```java
//! ipNucleusLabels.setThreshold(0.5, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
//! List<PolygonRoi> rois = RoiLabeling.getFilledPolygonROIs(ipNucleusLabels, Wand.FOUR_CONNECTED);
//! ```
//!
//! QuPath gets polygons; this returns the filled mask of each one as a region
//! map, because the mask is what every later step consumes — the ROI is set on
//! an image and measured, or filled into a binary. A vertex list would pin the
//! tracing without pinning the fill, and the fill is where the surprises are.
//!
//! # Three conventions, none of them visible at the call site
//!
//! - **It does not trace labels.** The Wand is handed the threshold range
//!   `[0.5, inf)`, so it outlines whatever run of *labelled* pixels it can
//!   reach, not the pixels sharing one label. Two basins meeting with no
//!   watershed line between them come back as **one** region. The watershed
//!   normally leaves a line, so this is invisible until it does not — and a
//!   port that iterates label values passes every other case.
//! - **Holes are filled**, because the Wand returns an outer boundary only. A
//!   basin shaped like a ring is measured over its interior too, including
//!   pixels carrying no label at all.
//! - **A basin enclosed by another is discarded outright.** QuPath marks the
//!   *filled* polygon as completed, so a basin lying in another's hole is never
//!   traced and never becomes an object. It does not merge with its encloser;
//!   it stops existing.
//!
//! All three are pinned by `parity/fixtures/ObjectFilters.json`.
//!
//! # What "enclosed" means
//!
//! The Wand traces with `FOUR_CONNECTED`, so at a diagonal crossing it keeps the
//! foreground four-connected and lets the background pass through the corner.
//! Background is therefore **eight**-connected here, dual to the four-connected
//! regions, and a hole is a background run that cannot reach the image border
//! even diagonally. `diagonal-pinch` in the fixture is the case that stops this
//! being a guess: a ring broken by a two-pixel gap keeps its interior open and
//! is not filled, and the bridging pixel beyond the gap is its own region.

use crate::Connectivity;

/// A tracer that owns its working buffers, for repeated use.
///
/// A tile pipeline traces once per tile; rebuilding the buffers per tile is
/// allocation in the tile loop, a defect under §6 of the project guidelines.
#[derive(Debug, Default)]
pub struct RegionTracer {
    stack: Vec<u32>,
    /// Stamped with the region index rather than cleared per region, so the
    /// background flood costs one pass over each region's bounding box and no
    /// clearing between them.
    escaped: Vec<u32>,
}

impl RegionTracer {
    /// Creates a tracer with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Traces the filled regions of `lower <= value <= upper` into `regions`,
    /// returning how many were found.
    ///
    /// `regions` is fully overwritten: every pixel gets either its region's
    /// number, counting from 1 in the raster order of the first pixel traced,
    /// or zero. Detection calls this with `0.5` and infinity on the label image
    /// the watershed produced.
    ///
    /// A pixel that is `NaN` — or infinite, when `upper` is finite — fails both
    /// comparisons and is background, which is what QuPath's range test does.
    ///
    /// # Panics
    ///
    /// Panics if `values` and `regions` differ in length, if `width` is zero or
    /// does not divide that length, or if the image has more pixels than a
    /// `u32` can index.
    pub fn trace_into(
        &mut self,
        values: &[f32],
        regions: &mut [u32],
        width: usize,
        lower: f32,
        upper: f32,
    ) -> u32 {
        assert_eq!(
            values.len(),
            regions.len(),
            "values and regions must describe the same image"
        );
        assert!(width > 0, "width must be non-zero");
        assert_eq!(
            values.len() % width,
            0,
            "width must divide the buffer length"
        );
        assert!(
            u32::try_from(values.len()).is_ok(),
            "image has more pixels than a u32 index can address"
        );

        let height = values.len() / width;
        regions.fill(0);
        // Cleared rather than merely resized: a stamp left over from a previous
        // tile collides with this tile's low region numbers, and the failure is
        // a hole silently declared already-escaped.
        self.escaped.clear();
        self.escaped.resize(values.len(), 0);

        let mut found = 0u32;
        for start in 0..values.len() {
            if regions[start] != 0 || !selected(values[start], lower, upper) {
                continue;
            }
            found += 1;

            let (x, y) = (start % width, start / width);
            let mut bounds = Bounds {
                x0: x,
                x1: x,
                y0: y,
                y1: y,
            };

            // The region itself: four-connected, as `Wand.FOUR_CONNECTED` asks.
            regions[start] = found;
            self.stack.clear();
            self.stack.push(truncate(start));
            while let Some(index) = self.stack.pop() {
                let index = index as usize;
                let (x, y) = (index % width, index / width);
                bounds.extend(x, y);
                for &(dx, dy) in Connectivity::Four.offsets() {
                    let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
                        continue;
                    };
                    let neighbour = ny * width + nx;
                    if regions[neighbour] != 0 || !selected(values[neighbour], lower, upper) {
                        continue;
                    }
                    regions[neighbour] = found;
                    self.stack.push(truncate(neighbour));
                }
            }

            fill_enclosed(
                &mut self.stack,
                &mut self.escaped,
                regions,
                width,
                height,
                found,
                bounds,
            );
        }
        found
    }
}

/// Traces the filled regions of `lower <= value <= upper`, returning how many
/// were found.
///
/// See [`RegionTracer::trace_into`], which this calls; prefer that when tracing
/// more than one tile.
///
/// # Panics
///
/// Panics under the same conditions as [`RegionTracer::trace_into`].
pub fn filled_regions(
    values: &[f32],
    regions: &mut [u32],
    width: usize,
    lower: f32,
    upper: f32,
) -> u32 {
    RegionTracer::new().trace_into(values, regions, width, lower, upper)
}

/// QuPath's range test: `val >= threshLower && val <= threshHigher`.
fn selected(value: f32, lower: f32, upper: f32) -> bool {
    value >= lower && value <= upper
}

/// A region's bounding box, which is as far as its holes can reach.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

impl Bounds {
    fn extend(&mut self, x: usize, y: usize) {
        self.x0 = self.x0.min(x);
        self.x1 = self.x1.max(x);
        self.y0 = self.y0.min(y);
        self.y1 = self.y1.max(y);
    }
}

/// Adds to `regions` every pixel that region `index` encloses.
///
/// Works by the complement: flood the non-region pixels eight-connected inwards
/// from the border of the bounding box grown by one, and whatever the flood
/// fails to reach is enclosed. Growing by one is what makes the box border a
/// legitimate seed — those pixels are outside the region's bounding box and so
/// cannot belong to it — and clipping the growth at the image edge is what
/// makes a region touching the edge behave as though the outside were open,
/// which is what the traced polygon does.
///
/// Enclosed pixels are claimed whatever they are, which is how a basin lying in
/// another's hole is absorbed: it has not been traced yet when its encloser
/// fills, and its own turn in the scan never comes because it now carries a
/// region. Testing `== 0` is enough rather than excluding other regions
/// explicitly — an encloser's outline runs above whatever it encloses, so it is
/// always traced first and a hole pixel is never already claimed. Both spellings
/// were tried against the fixture and are indistinguishable on it.
fn fill_enclosed(
    stack: &mut Vec<u32>,
    escaped: &mut [u32],
    regions: &mut [u32],
    width: usize,
    height: usize,
    index: u32,
    bounds: Bounds,
) {
    let bx0 = bounds.x0.saturating_sub(1);
    let by0 = bounds.y0.saturating_sub(1);
    let bx1 = (bounds.x1 + 1).min(width - 1);
    let by1 = (bounds.y1 + 1).min(height - 1);

    stack.clear();
    let mut seed = |x: usize, y: usize, stack: &mut Vec<u32>| {
        let at = y * width + x;
        if regions[at] != index && escaped[at] != index {
            escaped[at] = index;
            stack.push(truncate(at));
        }
    };
    for x in bx0..=bx1 {
        seed(x, by0, stack);
        seed(x, by1, stack);
    }
    for y in by0..=by1 {
        seed(bx0, y, stack);
        seed(bx1, y, stack);
    }

    // Eight-connected, dual to the region's four: at a diagonal crossing the
    // Wand keeps the foreground joined and lets the background through.
    while let Some(at) = stack.pop() {
        let at = at as usize;
        let (x, y) = (at % width, at / width);
        for &(dx, dy) in Connectivity::Eight.offsets() {
            let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
                continue;
            };
            if nx < bx0 || nx > bx1 || ny < by0 || ny > by1 {
                continue;
            }
            let neighbour = ny * width + nx;
            if regions[neighbour] == index || escaped[neighbour] == index {
                continue;
            }
            escaped[neighbour] = index;
            stack.push(truncate(neighbour));
        }
    }

    for y in bounds.y0..=bounds.y1 {
        for x in bounds.x0..=bounds.x1 {
            let at = y * width + x;
            if regions[at] == 0 && escaped[at] != index {
                regions[at] = index;
            }
        }
    }
}

/// Offsets a coordinate, returning `None` outside the image.
fn shift(
    x: usize,
    y: usize,
    dx: i32,
    dy: i32,
    width: usize,
    height: usize,
) -> Option<(usize, usize)> {
    let nx = x.checked_add_signed(dx as isize)?;
    let ny = y.checked_add_signed(dy as isize)?;
    (nx < width && ny < height).then_some((nx, ny))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "callers assert the image fits a u32 index"
)]
fn truncate(index: usize) -> u32 {
    index as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(values: &[f32], width: usize) -> (u32, Vec<u32>) {
        let mut regions = vec![0u32; values.len()];
        let found = filled_regions(values, &mut regions, width, 0.5, f32::INFINITY);
        (found, regions)
    }

    /// Builds a label image from a picture, `.` being unlabelled.
    fn image(rows: &[&str]) -> (Vec<f32>, usize) {
        let width = rows[0].len();
        let values = rows
            .iter()
            .flat_map(|row| {
                row.chars().map(|c| match c {
                    '.' => 0.0,
                    d => d.to_digit(10).expect("digit") as f32,
                })
            })
            .collect();
        (values, width)
    }

    #[test]
    fn touching_labels_are_one_region() {
        // The Wand is given a threshold range, not a label, so two basins with
        // no watershed line between them trace as one object. A port that
        // iterates label values reports two here and passes everything else.
        let (values, width) = image(&["....", ".12.", ".12.", "...."]);
        assert_eq!(trace(&values, width).0, 1);
    }

    #[test]
    fn a_hole_is_filled_and_a_gap_is_not() {
        let (ring, width) = image(&[".....", ".111.", ".1.1.", ".111.", "....."]);
        let (found, regions) = trace(&ring, width);
        assert_eq!(found, 1);
        assert_eq!(regions[2 * width + 2], 1, "the hole is inside the outline");

        // The same ring opened at one side: its interior reaches the border, so
        // there is nothing enclosed to fill.
        let (broken, width) = image(&[".....", ".111.", ".1...", ".111.", "....."]);
        let (found, regions) = trace(&broken, width);
        assert_eq!(found, 1);
        assert_eq!(regions[2 * width + 2], 0, "an open interior is not a hole");
    }

    #[test]
    fn a_region_inside_a_hole_is_discarded() {
        // Not merged and not kept separately: QuPath marks the filled polygon
        // completed, so the enclosed basin's turn in the scan never comes.
        let (values, width) = image(&[
            ".......", ".11111.", ".1...1.", ".1.2.1.", ".1...1.", ".11111.", ".......",
        ]);
        let (found, regions) = trace(&values, width);
        assert_eq!(found, 1, "the enclosed basin is not an object of its own");
        assert_eq!(regions[3 * width + 3], 1, "it is absorbed by its encloser");
    }

    #[test]
    fn background_escapes_diagonally() {
        // Dual to the four-connected region: the Wand cuts the corner, so an
        // interior that can only leave by a diagonal step is still open. Using
        // four-connected background here fills this and is wrong.
        let (values, width) = image(&[".....", ".11..", ".1.1.", ".111.", "....."]);
        let (_, regions) = trace(&values, width);
        assert_eq!(
            regions[2 * width + 2],
            0,
            "the gap at the corner leaves the interior open"
        );
    }

    #[test]
    fn a_region_on_the_border_is_not_filled_inwards() {
        let (values, width) = image(&["11..", "1...", "....", "...."]);
        let (found, regions) = trace(&values, width);
        assert_eq!(found, 1);
        assert_eq!(regions[width + 1], 0, "outside the image is open");
    }

    #[test]
    fn regions_are_numbered_in_raster_order() {
        let (values, width) = image(&["...1", "....", "1..."]);
        let (found, regions) = trace(&values, width);
        assert_eq!(found, 2);
        assert_eq!(regions[3], 1);
        assert_eq!(regions[2 * width], 2);
    }

    #[test]
    fn a_reused_tracer_matches_a_fresh_one() {
        // The escaped stamps are the risk: left over from a previous tile they
        // collide with this tile's low region numbers.
        let (values, width) = image(&[
            ".......", ".11111.", ".1...1.", ".1...1.", ".11111.", "..1.1..", ".......",
        ]);
        let mut fresh = vec![0u32; values.len()];
        let expected = filled_regions(&values, &mut fresh, width, 0.5, f32::INFINITY);

        let mut tracer = RegionTracer::new();
        let (other, other_width) = image(&["111", "1.1", "111"]);
        let mut scratch = vec![0u32; other.len()];
        tracer.trace_into(&other, &mut scratch, other_width, 0.5, f32::INFINITY);

        let mut reused = vec![0u32; values.len()];
        let count = tracer.trace_into(&values, &mut reused, width, 0.5, f32::INFINITY);
        assert_eq!(count, expected);
        assert_eq!(reused, fresh);
    }
}
