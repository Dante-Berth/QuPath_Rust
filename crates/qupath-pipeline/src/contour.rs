//! The boundary of a filled region, as a closed polygon.
//!
//! Every stage before this one works on a region *map*: a `u32` per pixel
//! naming which object covers it. That is the right shape for the pipeline —
//! the filters, the flood and the two traces all read and write it — and it is
//! the wrong shape for everything downstream. A viewer cannot draw a region
//! map, an export cannot serialise one, and `DIVERGENCES.md` D-008 is a claim
//! about vertices that nothing in this repository has been able to state.
//!
//! # Why a polygon and not the pixel mask
//!
//! [`crate::Candidate`] already carries a bitmask, and it is the right choice
//! there: `resolve_overlaps` counts shared pixels, and a mask counts them
//! directly. It is the wrong choice here for three reasons.
//!
//! - **QuPath stores a polygon.** `RoiLabeling.getFilledPolygonROIs` returns
//!   `PolygonRoi`, and every downstream thing QuPath does — the cell expansion,
//!   the smoothing D-008 is about, the GeoJSON export — is defined on vertices.
//!   A mask would have to be traced eventually anyway.
//! - **It is smaller where it matters.** A 30x30 nucleus is 900 bits of mask
//!   against roughly 40 vertices, and the gap widens with the object.
//! - **It draws.** A polygon is a line strip; a mask is a texture per object.
//!
//! # What the outline actually follows
//!
//! Pixel **corners**, not pixel centres. The polygon returned here is the crack
//! between the region and everything outside it, so a single pixel at the
//! origin comes back as the unit square `(0,0) (1,0) (1,1) (0,1)` rather than
//! as the degenerate point `(0,0)`.
//!
//! That is what makes [`shoelace_area_x2`] exactly the filled pixel count instead
//! of approximately it, which is the assertion this module is tested with and
//! is the same identity `overlaps.rs` relies on when it says counting shared
//! pixels *is* the intersection area rather than an estimate of it.
//!
//! # Chirality and the four-connected pinch
//!
//! The walk keeps the region on its right and prefers, at every corner, to turn
//! right before going straight before turning left. Two consequences, both
//! deliberate:
//!
//! - Vertices come out **clockwise** in image coordinates, where y runs down.
//!   [`shoelace_area_x2`] takes the absolute value, so nothing depends on this,
//!   but a caller feeding the ring to a fill rule does.
//! - At a corner where the region touches itself only diagonally, preferring
//!   the right turn walks *around* the pinch rather than through it — which
//!   treats the two pixels as unconnected, and that is `Wand.FOUR_CONNECTED`,
//!   the connectivity every trace in this pipeline uses. Going straight would
//!   be the eight-connected answer and would be wrong here.

use crate::objects::Extent;

/// Which way the walk is travelling along a crack, in image coordinates.
///
/// Named rather than an index because the turn rules below are the whole
/// algorithm, and `(direction + 1) % 4` reads as arithmetic rather than as
/// "turn right".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Right,
    Down,
    Left,
    Up,
}

impl Direction {
    /// The direction ninety degrees clockwise from this one.
    fn right(self) -> Self {
        match self {
            Self::Right => Self::Down,
            Self::Down => Self::Left,
            Self::Left => Self::Up,
            Self::Up => Self::Right,
        }
    }

    /// The direction ninety degrees anticlockwise from this one.
    fn left(self) -> Self {
        self.right().right().right()
    }

    /// One step along this direction, from a lattice corner.
    fn step(self, (x, y): (u32, u32)) -> (u32, u32) {
        match self {
            // Never underflows: the walk only ever leaves a corner along an
            // edge with an inside pixel beside it, and an inside pixel at
            // column or row zero puts the corner at 1 or more on the axis it
            // is about to decrease.
            Self::Right => (x + 1, y),
            Self::Down => (x, y + 1),
            Self::Left => (x - 1, y),
            Self::Up => (x, y - 1),
        }
    }
}

/// The region map and the label being traced, with the bounds checks that turn
/// "off the image" into "outside the region".
///
/// Off-image is genuinely outside for this purpose: an object touching the edge
/// of a tile has a boundary there, and the crack the walk follows runs along
/// the image border rather than falling off it.
struct Field<'a> {
    regions: &'a [u32],
    width: usize,
    height: usize,
    index: u32,
}

impl Field<'_> {
    /// Whether the pixel at these signed coordinates belongs to the region.
    fn inside(&self, x: i64, y: i64) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return false;
        };
        x < self.width && y < self.height && self.regions[y * self.width + x] == self.index
    }

    /// Whether the crack leaving `corner` in `direction` is a boundary edge —
    /// that is, whether it has the region on its right and not on its left.
    ///
    /// The four cases are the same statement rotated, and they are written out
    /// rather than folded into arithmetic because getting one of them backwards
    /// produces an outline that is plausible everywhere except at one kind of
    /// corner, which is exactly the bug that is invisible in a picture.
    fn is_boundary(&self, (x, y): (u32, u32), direction: Direction) -> bool {
        let (x, y) = (i64::from(x), i64::from(y));
        let (right, left) = match direction {
            // Travelling +x: the pixel below the crack is on the right.
            Direction::Right => ((x, y), (x, y - 1)),
            // Travelling +y: the pixel left of the crack is on the right.
            Direction::Down => ((x - 1, y), (x, y)),
            // Travelling -x: the pixel above the crack is on the right.
            Direction::Left => ((x - 1, y - 1), (x - 1, y)),
            // Travelling -y: the pixel right of the crack is on the right.
            Direction::Up => ((x, y - 1), (x - 1, y - 1)),
        };
        self.inside(right.0, right.1) && !self.inside(left.0, left.1)
    }
}

/// Appends the outline of region `index` to `out`, as lattice corners offset by
/// `origin`.
///
/// `extent` is that region's bounding box in the map's own coordinates, as
/// [`crate::ObjectFilter::extents_into`] produces it, and is used only to find
/// the first pixel — the walk itself is driven by the map. `origin` is where
/// the map sits in whatever frame the caller wants the answer in, which for the
/// pipeline is the tile's position in its level.
///
/// The ring is **not** closed by repeating the first vertex; the last vertex
/// joins the first. Nothing is appended for an empty region.
///
/// Returns how many vertices were appended.
///
/// # Panics
///
/// Panics if `width` does not divide the region map, or if `extent` claims a
/// non-empty region whose first row holds none of its pixels — both of which
/// mean the extent and the map disagree about what they describe.
pub fn trace_outline_into(
    regions: &[u32],
    width: usize,
    index: u32,
    extent: Extent,
    origin: (u32, u32),
    out: &mut Vec<[u32; 2]>,
) -> usize {
    assert!(width > 0, "width must be non-zero");
    assert_eq!(regions.len() % width, 0, "width must divide the region map");
    if extent.area == 0 {
        return 0;
    }

    let field = Field {
        regions,
        width,
        height: regions.len() / width,
        index,
    };

    // The topmost-leftmost pixel of the region: scan its first row only, which
    // `extents_into` guarantees holds at least one of its pixels. Its top-left
    // corner is the start, and it is the one corner of the whole walk whose 2x2
    // neighbourhood is known outright — three outside, one in — so the walk can
    // terminate on reaching it again without also comparing directions.
    let first = (extent.x..extent.x + extent.width)
        .find(|&column| field.inside(i64::from(column), i64::from(extent.y)))
        .expect("a region's bounding box starts on a row that contains it");
    let start = (first, extent.y);

    let before = out.len();
    // The incoming direction at that corner is always Up — the left edge of the
    // starting pixel, travelled upwards — so beginning here makes the first
    // corner a turn, and it is emitted like every other one rather than as a
    // special case.
    let mut direction = Direction::Up;
    let mut corner = start;
    loop {
        let next = if field.is_boundary(corner, direction.right()) {
            direction.right()
        } else if field.is_boundary(corner, direction) {
            direction
        } else if field.is_boundary(corner, direction.left()) {
            direction.left()
        } else {
            // A corner the walk reached along a boundary edge always has one
            // leaving it, so this is an invariant violation rather than a case.
            // It is a panic and not a fourth branch because the fourth branch
            // is "turn back", and taking it would spin here forever with
            // nothing on screen to say why.
            unreachable!("no boundary edge leaves {corner:?} arriving {direction:?}");
        };

        if next != direction {
            out.push([corner.0 + origin.0, corner.1 + origin.1]);
        }
        corner = next.step(corner);
        direction = next;

        if corner == start {
            break;
        }
    }

    out.len() - before
}

/// Twice the signed area a closed ring encloses, by the shoelace formula.
///
/// Kept as *twice* the area and as an integer because that is the form in which
/// it is exact: the vertices are lattice points, so the sum is an integer, and
/// halving it is the only step that could round. For a ring produced by
/// [`trace_outline_into`] its magnitude is exactly twice the number of pixels
/// the region covers, which is the identity the tests here assert and the one
/// `overlaps.rs` leans on.
///
/// Negative when the ring runs anticlockwise in image coordinates. Outlines
/// from this module run clockwise, so this comes back positive for them.
#[must_use]
pub fn shoelace_area_x2(ring: &[[u32; 2]]) -> i64 {
    if ring.len() < 3 {
        return 0;
    }
    let mut total: i64 = 0;
    for (current, next) in ring.iter().zip(ring.iter().cycle().skip(1)) {
        let (x0, y0) = (i64::from(current[0]), i64::from(current[1]));
        let (x1, y1) = (i64::from(next[0]), i64::from(next[1]));
        total += x0 * y1 - x1 * y0;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a region map from an ASCII picture, where `#` is region 1 and `.`
    /// is background, and returns it with its width.
    fn map(rows: &[&str]) -> (Vec<u32>, usize) {
        let width = rows[0].len();
        assert!(
            rows.iter().all(|row| row.len() == width),
            "every row must be the same length"
        );
        let regions = rows
            .iter()
            .flat_map(|row| row.bytes().map(|byte| u32::from(byte == b'#')))
            .collect();
        (regions, width)
    }

    /// The extent of region 1, computed the long way so the tests do not depend
    /// on `extents_into` being right.
    fn extent(regions: &[u32], width: usize) -> Extent {
        let (mut x0, mut y0, mut x1, mut y1, mut area) = (u32::MAX, u32::MAX, 0, 0, 0);
        for (at, &region) in regions.iter().enumerate() {
            if region != 1 {
                continue;
            }
            #[expect(clippy::cast_possible_truncation, reason = "test maps are tiny")]
            let (x, y) = ((at % width) as u32, (at / width) as u32);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
            area += 1;
        }
        if area == 0 {
            return Extent {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
                area: 0,
            };
        }
        Extent {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
            area,
        }
    }

    fn outline(rows: &[&str]) -> Vec<[u32; 2]> {
        let (regions, width) = map(rows);
        let mut out = Vec::new();
        trace_outline_into(
            &regions,
            width,
            1,
            extent(&regions, width),
            (0, 0),
            &mut out,
        );
        out
    }

    #[test]
    fn one_pixel_traces_its_own_unit_square() {
        // The smallest statement of "corners, not centres". A tracer that
        // followed pixel centres would return a single point here and an area
        // of zero, and would then be wrong by half a pixel everywhere.
        assert_eq!(
            outline(&[".....", ".#...", "....."]),
            vec![[1, 1], [2, 1], [2, 2], [1, 2]]
        );
    }

    #[test]
    fn the_enclosed_area_is_exactly_the_pixel_count() {
        // The identity this module exists to preserve, over shapes chosen so
        // that a rounding rule, an off-by-one in a span, or a missed concave
        // corner each break it. This is the whole test suite in one assertion:
        // an outline that is wrong anywhere encloses the wrong number of
        // pixels, and no tolerance is involved.
        let shapes: &[&[&str]] = &[
            &["#"],
            &["##", "##"],
            &["....", ".##.", ".##.", "...."],
            // Concave: an L, which has a left turn in it.
            &["#..", "#..", "###"],
            // A plus, which has four concave corners.
            &[".#.", "###", ".#."],
            // A staircase, which alternates turns on every step.
            &["#...", "##..", ".##.", "..##"],
            // Touching all four edges of the map, so the walk runs along the
            // border rather than in open space. A C rather than an O, because
            // an O has a hole and this traces the outer ring only — which is
            // sound here for the reason `Detections` records: every region
            // reaching this module has been through a hole-filling trace.
            &["####", "#...", "####"],
            // A one-pixel-wide comb, where the outline is longer than the area.
            &["#.#.#", "#####"],
        ];
        for rows in shapes {
            let (regions, width) = map(rows);
            let expected = extent(&regions, width).area;
            let ring = outline(rows);
            assert_eq!(
                shoelace_area_x2(&ring),
                i64::from(expected) * 2,
                "shape {rows:?} enclosed the wrong area with outline {ring:?}"
            );
        }
    }

    #[test]
    fn a_diagonal_pinch_is_walked_around_rather_than_through() {
        // `Wand.FOUR_CONNECTED`, which is the connectivity every trace in this
        // pipeline uses. These two pixels are one region only because the
        // tracer that produced the map said so; the outline must still treat
        // the corner between them as a pinch and go round it, giving the area
        // of two pixels rather than of a bowtie. Going straight through — the
        // eight-connected answer — would enclose the same two pixels with a
        // self-intersecting ring, and the shoelace of that is zero.
        //
        // Both pixels carry label 1 here, which four-connected labelling would
        // never produce; that is the point. The corner between them is the only
        // place in this module where three of the 2x2 tests are ambiguous, and
        // it needs a map that a well-formed pipeline will not hand it.
        let rows = &["#.", ".#"];
        let (regions, width) = map(rows);
        let mut ring = Vec::new();
        trace_outline_into(
            &regions,
            width,
            1,
            extent(&regions, width),
            (0, 0),
            &mut ring,
        );
        // Only the first pixel's square is reachable from its own top-left
        // corner without crossing the pinch, so the walk closes on one pixel.
        assert_eq!(shoelace_area_x2(&ring), 2);
        assert_eq!(ring, vec![[0, 0], [1, 0], [1, 1], [0, 1]]);
    }

    #[test]
    fn vertices_are_only_the_corners() {
        // A long straight edge must not emit a vertex per pixel. This is not
        // cosmetic: the overlay uploads these, and a 2000 px object would cost
        // 8000 vertices instead of the handful its shape needs.
        let ring = outline(&["####", "####", "####"]);
        assert_eq!(ring, vec![[0, 0], [4, 0], [4, 3], [0, 3]]);
    }

    #[test]
    fn the_origin_offsets_every_vertex() {
        // The tile's position in its level, carried through so the caller gets
        // level coordinates rather than tile ones.
        let (regions, width) = map(&[".#."]);
        let mut ring = Vec::new();
        trace_outline_into(
            &regions,
            width,
            1,
            extent(&regions, width),
            (1000, 2000),
            &mut ring,
        );
        assert_eq!(
            ring,
            vec![[1001, 2000], [1002, 2000], [1002, 2001], [1001, 2001]]
        );
    }

    #[test]
    fn an_empty_region_appends_nothing() {
        let (regions, width) = map(&["..", ".."]);
        let mut ring = vec![[9, 9]];
        let added = trace_outline_into(
            &regions,
            width,
            1,
            extent(&regions, width),
            (0, 0),
            &mut ring,
        );
        assert_eq!(added, 0);
        assert_eq!(ring, vec![[9, 9]]);
    }

    #[test]
    fn the_ring_runs_clockwise_in_image_coordinates() {
        // Recorded rather than left to chance: a caller filling the ring needs
        // to know, and the sign of the shoelace is the whole statement.
        assert!(shoelace_area_x2(&outline(&["##", "##"])) > 0);
    }

    #[test]
    fn a_neighbouring_region_is_not_traced() {
        // Two objects sharing an edge. Tracing region 1 must stop at the shared
        // crack rather than wandering into region 2, which is the failure a
        // tracer keyed on "non-zero" instead of on the label would have.
        let regions = vec![1, 1, 2, 2, 1, 1, 2, 2];
        let width = 4;
        let extent = extent(&regions, width);
        let mut ring = Vec::new();
        trace_outline_into(&regions, width, 1, extent, (0, 0), &mut ring);
        assert_eq!(ring, vec![[0, 0], [2, 0], [2, 2], [0, 2]]);
        assert_eq!(shoelace_area_x2(&ring), 8);

        // And region 2 comes back as its own square, offset.
        let mut ring = Vec::new();
        let extent = Extent {
            x: 2,
            y: 0,
            width: 2,
            height: 2,
            area: 4,
        };
        trace_outline_into(&regions, width, 2, extent, (0, 0), &mut ring);
        assert_eq!(ring, vec![[2, 0], [4, 0], [4, 2], [2, 2]]);
    }
}
