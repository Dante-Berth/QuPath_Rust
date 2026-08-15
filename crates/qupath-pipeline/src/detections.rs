//! What a run found, rather than how many of them there were.
//!
//! [`crate::run`] returns [`crate::Outcome`], which is counts and timings, and
//! that was the right thing to return for every milestone up to this one: the
//! parity fixtures compare counts, and a pipeline that streams tiles through
//! bounded memory must not accumulate an object set by default. `docs/VIEWER.md`
//! §3.5 is the first caller that needs the objects themselves, and this is what
//! it gets.
//!
//! # Why this is a second entry point and not a flag
//!
//! `ROADMAP.md` §6 requires a documented memory ceiling and forbids any path
//! that materialises a whole slide. [`crate::run`] honours that outright:
//! nothing survives a tile except a handful of counters and the seam candidates,
//! so peak memory is set by the thread count. Collecting detections breaks that
//! property by construction — the output grows with the region — so it is a
//! different function with a different contract rather than a parameter that
//! quietly changes which contract you are under.
//!
//! The cost is **322 bytes per object**, measured on `CMU-1-Small-Region.svs`
//! at level 0 by `the_footprint_is_the_one_the_documentation_claims`: a 16-byte
//! bounding box, two measurements, and an outline that comes to 36.7 vertices
//! on average. The outline is five sixths of that, so the figure is really a
//! statement about nucleus perimeters and will fall on a coarser level. A
//! region is comfortable at this rate and a whole slide at level 0 is not —
//! CMU-1 would be tens of gigabytes. Ask for a region.
//!
//! # Columnar, per `ROADMAP.md` §4
//!
//! Measurements live in a [`Table`], which is this workspace's columnar store
//! and until now had no producer anywhere in it — it was written for exactly
//! this moment and then not reached for four milestones. Geometry is kept
//! **separate** from measurements, as §4 requires, because almost everything
//! touches one or the other and not both: culling to a viewport reads only
//! bounds, drawing reads only outlines, and filtering reads only columns.
//!
//! Outlines share one arena rather than being a `Vec` each. Several thousand
//! small `Vec`s is several thousand allocations and a pointer chase per object
//! to draw one, which is the shape §4 exists to reject.

use qupath_objects::{FeatureId, Table};

use crate::Region;

/// Filled area of the nucleus, in pixels of the level the run read.
///
/// Pixels and not µm²: this crate does not carry a pixel size, and QuPath's
/// `Nucleus: Area` is calibrated. Multiplying by the square of the level's
/// microns-per-pixel is the caller's job, and naming the column honestly is
/// cheaper than a unit that is right on one slide.
pub const AREA: &str = "Nucleus: Area (px)";

/// Mean of `ipToMeasure` over the nucleus — the background-subtracted
/// haematoxylin optical density.
///
/// This is the same number [`crate::ObjectFilter::apply_area`] thresholded, read
/// out of the filter rather than measured a second time, so a detection's mean
/// and the reason it survived cannot disagree.
pub const MEAN: &str = "Nucleus: Haematoxylin OD mean";

/// Detected objects and their geometry, in the coordinates of one pyramid level.
///
/// Rows are shared: object `i` has [`Detections::bounds`] `i`,
/// [`Detections::outline`] `i`, and row `i` of [`Detections::measurements`].
#[derive(Debug, Clone)]
pub struct Detections {
    level: usize,
    bounds: Vec<Region>,
    /// One arena for every outline. Object `i` occupies
    /// `vertices[starts[i]..starts[i + 1]]`.
    vertices: Vec<[u32; 2]>,
    /// `len() + 1` entries, so the last object's end needs no special case.
    starts: Vec<u32>,
    table: Table,
    area: FeatureId,
    mean: FeatureId,
}

impl Detections {
    /// An empty set for objects found at `level`.
    #[must_use]
    pub fn new(level: usize) -> Self {
        let mut table = Table::new();
        // Resolved once here rather than by name at every push, which is §4's
        // rule about string lookup in a loop.
        let area = table.add_feature(AREA);
        let mean = table.add_feature(MEAN);
        Self {
            level,
            bounds: Vec::new(),
            vertices: Vec::new(),
            starts: vec![0],
            table,
            area,
            mean,
        }
    }

    /// Which pyramid level the coordinates are in.
    ///
    /// Every bounding box and every vertex is in this level's pixels. Converting
    /// to level 0 is a multiply by that level's `downsample`, and doing it here
    /// would throw away the exactness the outline was traced with.
    #[must_use]
    pub fn level(&self) -> usize {
        self.level
    }

    /// How many objects there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bounds.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bounds.is_empty()
    }

    /// Bounding box of every object, in level coordinates.
    ///
    /// Returned as a slice rather than one at a time because culling to a
    /// viewport is a pass over exactly this and nothing else.
    #[must_use]
    pub fn bounds(&self) -> &[Region] {
        &self.bounds
    }

    /// The closed outline of one object, as lattice corners in level
    /// coordinates.
    ///
    /// The ring does not repeat its first vertex; the last joins the first.
    ///
    /// # Panics
    ///
    /// Panics if `object` is past the end.
    #[must_use]
    pub fn outline(&self, object: usize) -> &[[u32; 2]] {
        let start = self.starts[object] as usize;
        let end = self.starts[object + 1] as usize;
        &self.vertices[start..end]
    }

    /// The measurement table, one row per object.
    #[must_use]
    pub fn measurements(&self) -> &Table {
        &self.table
    }

    /// Mutable access to the measurements, for a caller adding its own columns.
    ///
    /// Adding a column is safe at any time; adding a *row* is not, because the
    /// row index is shared with the geometry above. Nothing here can prevent
    /// that, so it is said rather than enforced.
    pub fn measurements_mut(&mut self) -> &mut Table {
        &mut self.table
    }

    /// The id of the [`AREA`] column, resolved once at construction.
    #[must_use]
    pub fn area(&self) -> FeatureId {
        self.area
    }

    /// The id of the [`MEAN`] column, resolved once at construction.
    #[must_use]
    pub fn mean(&self) -> FeatureId {
        self.mean
    }

    /// Total vertices across every outline.
    ///
    /// The one number that says what an object set costs, since it dominates:
    /// bounds and measurements are fixed per object and the outline is not.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// Appends one object, and returns its row index.
    ///
    /// `outline` is copied into the shared arena. `area` is in pixels and `mean`
    /// is the optical density [`MEAN`] describes.
    ///
    /// # Panics
    ///
    /// Panics if the arena would exceed `u32::MAX` vertices.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "measurements are stored as f32 by §4; area below 2^24 is exact and a mean is a threshold input"
    )]
    pub fn push(&mut self, bounds: Region, outline: &[[u32; 2]], area: u32, mean: f64) -> usize {
        let row = self.table.push_object();
        self.table.set(row, self.area, area as f32);
        self.table.set(row, self.mean, mean as f32);
        self.bounds.push(bounds);
        self.vertices.extend_from_slice(outline);
        self.starts
            .push(u32::try_from(self.vertices.len()).expect("vertex count exceeds u32"));
        row.index()
    }

    /// Appends every object of `other` whose row `keep` marks true.
    ///
    /// This is how the tiles of a run are joined once `resolve_overlaps` has
    /// said which of them a neighbour found a better copy of. Only the two
    /// columns this module owns are carried across — a caller that added its own
    /// has added them to the wrong table.
    ///
    /// # Panics
    ///
    /// Panics if `keep` is not one entry per object of `other`.
    pub fn append_kept(&mut self, other: &Self, keep: &[bool]) {
        assert_eq!(
            keep.len(),
            other.len(),
            "keep must have one entry per object"
        );
        let (area, mean) = (
            other.table.column(other.area),
            other.table.column(other.mean),
        );
        for (object, &keep) in keep.iter().enumerate() {
            if !keep {
                continue;
            }
            let row = self.table.push_object();
            self.table.set(row, self.area, area[object]);
            self.table.set(row, self.mean, mean[object]);
            self.bounds.push(other.bounds[object]);
            self.vertices.extend_from_slice(other.outline(object));
            self.starts
                .push(u32::try_from(self.vertices.len()).expect("vertex count exceeds u32"));
        }
    }

    /// Releases the slack every arena has after a run.
    pub fn shrink_to_fit(&mut self) {
        self.bounds.shrink_to_fit();
        self.vertices.shrink_to_fit();
        self.starts.shrink_to_fit();
        self.table.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::cast_possible_truncation, reason = "loop counters below four")]

    use super::*;

    fn region(x: u32, y: u32) -> Region {
        Region {
            x,
            y,
            width: 2,
            height: 2,
        }
    }

    fn square(x: u32, y: u32) -> Vec<[u32; 2]> {
        vec![[x, y], [x + 2, y], [x + 2, y + 2], [x, y + 2]]
    }

    #[test]
    fn geometry_and_measurements_share_a_row_index() {
        // The invariant the whole type rests on, and the one a columnar store
        // makes it possible to get wrong: bounds, outline and measurements are
        // three parallel structures, and nothing in the type system ties them.
        let mut detections = Detections::new(2);
        for step in 0..3u32 {
            let row = detections.push(
                region(step * 10, step),
                &square(step * 10, step),
                4 + step,
                f64::from(step) / 4.0,
            );
            assert_eq!(row, step as usize);
        }

        assert_eq!(detections.len(), 3);
        assert_eq!(detections.level(), 2);
        for step in 0..3usize {
            assert_eq!(detections.bounds()[step].x, step as u32 * 10);
            assert_eq!(detections.outline(step)[0], [step as u32 * 10, step as u32]);
            let table = detections.measurements();
            assert_eq!(table.column(detections.area())[step], 4.0 + step as f32);
            assert_eq!(table.column(detections.mean())[step], step as f32 / 4.0);
        }
    }

    #[test]
    fn outlines_of_different_lengths_stay_separate() {
        // The arena's whole risk: one object's ring bleeding into the next when
        // the rings are not all the same length.
        let mut detections = Detections::new(0);
        detections.push(region(0, 0), &[[0, 0], [1, 0], [0, 1]], 1, 0.0);
        detections.push(region(5, 5), &[], 0, 0.0);
        detections.push(region(9, 9), &square(9, 9), 4, 1.0);

        assert_eq!(detections.outline(0), &[[0, 0], [1, 0], [0, 1]]);
        assert!(detections.outline(1).is_empty());
        assert_eq!(detections.outline(2), square(9, 9).as_slice());
        assert_eq!(detections.vertex_count(), 7);
    }

    #[test]
    fn appending_keeps_only_the_marked_rows() {
        // How a run joins its tiles once the seam duplicates are known. A
        // filtered append that carried the wrong row's outline would show up as
        // an object drawn in someone else's place, which is invisible in a
        // count and obvious in a picture.
        let mut tile = Detections::new(1);
        for step in 0..4u32 {
            tile.push(region(step, 0), &square(step, 0), step, f64::from(step));
        }

        let mut merged = Detections::new(1);
        merged.append_kept(&tile, &[true, false, true, false]);

        assert_eq!(merged.len(), 2);
        assert_eq!(merged.bounds()[0].x, 0);
        assert_eq!(merged.bounds()[1].x, 2);
        assert_eq!(merged.outline(1), square(2, 0).as_slice());
        assert_eq!(merged.measurements().column(merged.mean()), &[0.0, 2.0]);
        assert_eq!(merged.vertex_count(), 8);
    }

    #[test]
    fn an_empty_set_reports_itself_as_one() {
        let detections = Detections::new(0);
        assert!(detections.is_empty());
        assert_eq!(detections.len(), 0);
        assert_eq!(detections.vertex_count(), 0);
        assert_eq!(detections.measurements().len(), 0);
        // The two columns exist before any object does, so a caller reading a
        // column of an empty set gets an empty slice rather than a panic.
        assert_eq!(detections.measurements().n_features(), 2);
        assert!(
            detections
                .measurements()
                .column(detections.area())
                .is_empty()
        );
    }
}
