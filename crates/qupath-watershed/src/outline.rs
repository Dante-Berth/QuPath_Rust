//! Object outlines, matching ImageJ's `Wand.autoOutline` in `FOUR_CONNECTED` mode.
//!
//! [`regions`](crate::regions) traces the same boundaries and keeps the *filled
//! mask*, because that is what every stage of the detection consumes. This keeps
//! the **vertex list**, which is what a `PathObject` carries: `ROADMAP.md`
//! §1.5b's step 4 is that the masks match and the polygons do not exist, and two
//! things need the polygon rather than what it encloses.
//!
//! - **Exporting geometry.** GeoJSON wants a ring of coordinates. A mask cannot
//!   be written as one without inventing an ordering.
//! - **`DIVERGENCES.md` D-008.** QuPath's seam resolution compares
//!   `geometry.getArea()` of a *smoothed* polygon, which is not the enclosed
//!   pixel count. Nothing can close that divergence until the polygon exists to
//!   be smoothed.
//!
//! Neither is done here. This is the contour, pinned against QuPath, and
//! nothing more.
//!
//! # The call site, which decides which trace this is
//!
//! `WatershedCellDetection` reaches the Wand three times and only the last
//! survives as object geometry — the outlines at its lines 798 and 831 are used
//! for a mean-intensity check and then filled back into a binary mask. Line 870
//! is the one:
//!
//! ```java
//! List<PolygonRoi> roisNuclei = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);
//! ```
//!
//! `FOUR_CONNECTED` is not `autoOutline`'s default. The overloads that take no
//! mode use the legacy one, which traces a different boundary, so the mode is a
//! recorded fact here rather than a parameter that looked reasonable —
//! `ROADMAP.md` §5's "check *which* algorithm QuPath calls" applied to an
//! argument instead of a class.
//!
//! # What the fixture settled
//!
//! `Wand.java`'s source is not in this checkout, only the compiled class, so
//! `parity/java/WandOutline.java` records what it does and
//! `parity_wand_outline.rs` checks this against that. Four conventions came out
//! of it, and three would have been guessed wrong:
//!
//! - **Vertices sit on pixel *corners*, not centres.** A single pixel at
//!   `(1, 1)` outlines as x `[2, 1, 1, 2]`, y `[2, 2, 1, 1]` — the corners of
//!   the unit square it occupies. A tracer working in pixel centres is half a
//!   pixel out everywhere.
//! - **Only corners are emitted.** A four-pixel horizontal bar is still four
//!   points; collinear runs carry no intermediate vertex.
//! - **A hole is filled, not traced.** A ring comes back as its outer rectangle,
//!   four points. Reproducing the hole would look *more* correct in isolation
//!   and be wrong against QuPath — the same convention [`regions`] documents,
//!   arrived at from the other side.
//! - **The interior stays on the walker's right.** Read off the recorded `ell`,
//!   whose six edges every one have a foreground pixel to the right of travel
//!   and a background one to the left. That invariant *is* the algorithm: each
//!   step takes the first turn — right, straight, left, back — that restores it,
//!   and it is also what makes a diagonal touch two regions rather than one.
//!
//! # Where the ring starts, and why that is compared up to rotation
//!
//! ImageJ's listing offset is **not** reproduced, and it is not a corner rule.
//! `single_pixel` and `square` are listed from the region's bottom-right corner,
//! which looks like one — but `ell` is listed from `(2, 3)`, which is not a
//! corner of its seed pixel at all. The offset falls out of where `Wand`'s own
//! internal scan happens to begin, and nothing in the recorded output determines
//! it from the outside.
//!
//! So the *cycle* is ported and the offset is not, and
//! `parity_wand_outline.rs` compares up to rotation while pinning the length,
//! the winding and every vertex. That is a real, named gap rather than a
//! tolerance: what consumes these is smoothing and an area, and neither depends
//! on which vertex is written first. If something later does, this is where to
//! look.

/// One traced outline: a closed ring of vertices on pixel corners.
///
/// The ring is **not** repeated at the end — `points` holds each vertex once,
/// and the last joins back to the first. That is `Wand.npoints` exactly, and it
/// matters for anything measuring the ring: a duplicated closing point is a
/// zero-length edge, which a shoelace area survives and a smoothing pass does
/// not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline {
    /// Vertices in walk order, on pixel corners, each appearing once.
    pub points: Vec<(i32, i32)>,
}

impl Outline {
    /// The number of vertices, which is ImageJ's `npoints`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// Whether the outline has no vertices, which a traced one never does.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Twice the signed area, by the shoelace formula.
    ///
    /// Integer, and deliberately not halved: on pixel corners every outline's
    /// doubled area is an exact integer, so this is the one form that needs no
    /// float. The sign carries the walk direction.
    ///
    /// This is **not** what D-008 compares — that is the area of a *smoothed*
    /// polygon, which this crate does not produce. For an unsmoothed outline of
    /// a filled region the magnitude does equal twice the pixel count, which is
    /// what `the_area_of_an_outline_is_its_pixel_count` checks.
    ///
    /// The sign is **positive** for every outline this traces. Keeping the
    /// interior on the right winds the ring clockwise on screen, and clockwise
    /// with `y` increasing downwards is a positive shoelace sum — the opposite
    /// of the textbook convention, because the textbook has `y` going up.
    #[must_use]
    pub fn double_signed_area(&self) -> i64 {
        let mut total: i64 = 0;
        for index in 0..self.points.len() {
            let (x0, y0) = self.points[index];
            let (x1, y1) = self.points[(index + 1) % self.points.len()];
            total += i64::from(x0) * i64::from(y1) - i64::from(x1) * i64::from(y0);
        }
        total
    }
}

/// Which way the boundary walker is facing.
///
/// Directions are in image coordinates, so `Down` increases `y`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Direction {
    Up,
    Right,
    Down,
    Left,
}

impl Direction {
    /// The step this direction takes, in corner coordinates.
    const fn step(self) -> (i32, i32) {
        match self {
            Self::Up => (0, -1),
            Self::Right => (1, 0),
            Self::Down => (0, 1),
            Self::Left => (-1, 0),
        }
    }

    /// The next direction clockwise, in image coordinates.
    const fn clockwise(self) -> Self {
        match self {
            Self::Up => Self::Right,
            Self::Right => Self::Down,
            Self::Down => Self::Left,
            Self::Left => Self::Up,
        }
    }

    /// The next direction anticlockwise, in image coordinates.
    const fn anticlockwise(self) -> Self {
        match self {
            Self::Up => Self::Left,
            Self::Left => Self::Down,
            Self::Down => Self::Right,
            Self::Right => Self::Up,
        }
    }

    /// The pixels either side of the unit edge leaving `at` in this direction.
    ///
    /// Returns `(left, right)` relative to the direction of travel. A corner
    /// `(x, y)` has four pixels around it — `(x-1, y-1)`, `(x, y-1)`,
    /// `(x-1, y)` and `(x, y)` — and which two touch the edge ahead depends on
    /// the facing.
    const fn sides(self, at: (i32, i32)) -> ((i32, i32), (i32, i32)) {
        let (x, y) = at;
        match self {
            Self::Up => ((x - 1, y - 1), (x, y - 1)),
            Self::Right => ((x, y - 1), (x, y)),
            Self::Down => ((x, y), (x - 1, y)),
            Self::Left => ((x - 1, y), (x - 1, y - 1)),
        }
    }
}

/// Every direction, in the order a turn is preferred.
///
/// Right first, then straight, then left, then back. Trying them in this order
/// and taking the first that keeps the invariant is what makes the walk hug the
/// boundary rather than cut a corner.
const TURN_ORDER: [fn(Direction) -> Direction; 4] = [
    Direction::clockwise,
    |facing| facing,
    Direction::anticlockwise,
    |facing| facing.clockwise().clockwise(),
];

/// Traces the outer boundary of the region containing `(seed_x, seed_y)`.
///
/// `inside` reports whether a pixel is foreground; it is called with
/// coordinates that may lie outside the raster, and must answer `false` there —
/// [`outline_mask`] is the usual caller and does exactly that. Taking a closure
/// rather than a slice is what lets the same walk serve a binary mask, a
/// threshold over a label image, and a single label, which are three different
/// callers in this pipeline.
///
/// The seed must be a foreground pixel that is the **first** of its region in
/// raster order, which is what `getFilledPolygonROIs`'s scan guarantees.
///
/// # The invariant, which is the whole algorithm
///
/// **The interior stays on the walker's right and the exterior on its left.**
/// That is not a convention chosen here; it was read off the recorded `ell`
/// case, whose six edges all have a foreground pixel on the right and a
/// background one on the left. Every step picks the first turn — right,
/// straight, left, back — that restores it.
///
/// Keeping the interior on the right is what makes a diagonal touch two regions
/// rather than one, because the walk turns *around* the shared corner instead of
/// crossing it. That is `FOUR_CONNECTED`, and `diagonal_touch` in the fixture is
/// what pins it.
///
/// # Panics
///
/// Panics if `inside(seed_x, seed_y)` is false, which is a caller that scanned
/// wrong rather than a shape this cannot trace.
pub fn outline_from<F>(seed_x: i32, seed_y: i32, inside: F) -> Outline
where
    F: Fn(i32, i32) -> bool,
{
    assert!(
        inside(seed_x, seed_y),
        "the seed pixel is not inside the region"
    );

    // The seed's top-left corner, facing down: the seed is then immediately on
    // the right and the pixel beyond the left edge is outside, because the seed
    // is its region's first pixel in raster order.
    let start = (seed_x, seed_y);
    let mut at = start;
    let mut facing = Direction::Down;
    let mut points: Vec<(i32, i32)> = Vec::new();
    // The pixel the walk is currently hugging. It starts as the seed, and every
    // step it may only move to a four-neighbour of itself.
    let mut hugging = (seed_x, seed_y);

    // Terminate on a repeated (corner, direction), not on returning to the
    // start. Those are not the same: the walk can pass through the starting
    // corner facing differently — a shape pinched to a point does exactly that —
    // and stopping there would cut the ring short. Every state has one
    // successor, so a repeat is the cycle closing.
    let mut visited = std::collections::HashSet::new();

    for _ in 0..MAX_STEPS {
        // Turning right is tried first, then straight, then left. The candidate
        // must keep the invariant *and* keep hugging a pixel four-connected to
        // the one just hugged — see `hugged` for why the second condition is
        // what `FOUR_CONNECTED` means here.
        let chosen = TURN_ORDER
            .iter()
            .map(|turn| turn(facing))
            .find(|&candidate| {
                let (left, right) = candidate.sides(at);
                inside(right.0, right.1)
                    && !inside(left.0, left.1)
                    && reachable(hugging, right, &inside)
            })
            .unwrap_or(facing);

        if !visited.insert((at, chosen)) {
            break;
        }
        // A vertex only where the direction changes. Collinear runs emit
        // nothing, which is why a four-pixel bar is four points and not six.
        if chosen != facing {
            points.push(at);
        }
        facing = chosen;
        hugging = chosen.sides(at).1;

        let (dx, dy) = facing.step();
        at = (at.0 + dx, at.1 + dy);
    }

    Outline { points }
}

/// Whether the walk may move from hugging `from` to hugging `to`.
///
/// Same pixel or a four-neighbour: always. **Diagonal: only if the corner they
/// share is filled** — that is, if one of the two pixels completing the 2x2
/// block is also inside, so the two are connected through it rather than only
/// at the point.
///
/// This is where `FOUR_CONNECTED` lives, and it is not where it looks like it
/// should. The obvious place is a turn table with one arm flipped for the
/// diagonal case, and that is wrong, because a concave corner and a shared
/// corner reach the *same* arm: at both, the invariant fails going right and
/// straight, so the walk turns left onto a diagonal pixel. The turn cannot tell
/// them apart. What can is whether the region continues around the corner —
/// filled at a concave corner, empty at a shared one.
///
/// Forbidding the empty case is what keeps the walk inside one region.
/// Seeding `diagonal_touch`'s lower pixel traced the *upper* one without it, and
/// the mask-side test could not have seen that: the scan had already completed
/// the upper region, so the two outlines merely came out duplicated rather than
/// obviously wrong.
fn reachable<F>(from: (i32, i32), to: (i32, i32), inside: &F) -> bool
where
    F: Fn(i32, i32) -> bool,
{
    if four_connected(from, to) {
        return true;
    }
    // Diagonal: the two pixels that complete the 2x2 block containing both.
    inside(from.0, to.1) || inside(to.0, from.1)
}

/// Whether two pixels are the same or four-neighbours.
const fn four_connected(from: (i32, i32), to: (i32, i32)) -> bool {
    let dx = (from.0 - to.0).abs();
    let dy = (from.1 - to.1).abs();
    dx + dy <= 1
}

/// A bound on the walk, so a caller's inconsistent `inside` cannot hang.
///
/// The walk visits each (corner, direction) at most once and stops on a repeat,
/// so this is unreachable for a closure that answers consistently. It is here
/// because `inside` is a closure this module cannot check.
const MAX_STEPS: usize = 1 << 26;

/// Traces every region of a binary mask, in the order a raster scan finds them.
///
/// This is `RoiLabeling.getFilledPolygonROIs`'s loop: scan rows top to bottom
/// and, within a row, left to right; trace from the first pixel of any region
/// not already completed; mark the traced region done so it is not traced again.
///
/// `mask` is row-major, `width` wide, and a non-zero byte is foreground. The
/// returned outlines are in scan order, which is the order QuPath's list has and
/// therefore the order object ids end up in.
///
/// # A region inside another's hole
///
/// It is **discarded**, not returned. QuPath marks the *filled* polygon as
/// completed — `bpCompleted.fill(roi)` — so a region lying in another's hole is
/// covered before the scan reaches it and never becomes an object. [`regions`]
/// documents the same convention from the mask side and `ObjectFilters.json`
/// pins it; this reproduces it by filling the traced ring rather than by
/// flooding the component, which is the difference that makes it happen.
///
/// # Panics
///
/// Panics if `mask.len()` is not a multiple of `width`, or `width` is zero.
#[must_use]
pub fn outline_mask(mask: &[u8], width: usize) -> Vec<Outline> {
    assert!(width > 0, "a mask needs a positive width");
    assert!(
        mask.len().is_multiple_of(width),
        "a {} byte mask is not a whole number of {width} wide rows",
        mask.len()
    );
    let height = mask.len() / width;

    let inside = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 {
            return false;
        }
        let (x, y) = (x as usize, y as usize);
        x < width && y < height && mask[y * width + x] != 0
    };

    let mut completed = vec![false; mask.len()];
    let mut outlines = Vec::new();

    for y in 0..height {
        for x in 0..width {
            let at = y * width + x;
            if completed[at] || mask[at] == 0 {
                continue;
            }
            let outline = outline_from(
                i32::try_from(x).expect("a mask narrower than i32::MAX"),
                i32::try_from(y).expect("a mask shorter than i32::MAX"),
                inside,
            );
            fill_ring(&outline, &mut completed, width, height);
            outlines.push(outline);
        }
    }
    outlines
}

/// Marks every pixel enclosed by `outline` as completed.
///
/// Stands in for `bpCompleted.fill(roi)`, and filling the *ring* rather than
/// flooding the component is what makes a region inside a hole disappear — see
/// [`outline_mask`].
///
/// Scanline parity over the ring's edges. The vertices are on pixel corners, so
/// pixel row `y` is tested along the horizontal line `y + 0.5`, which no vertex
/// lies on and no edge is collinear with — that is what makes a plain crossing
/// count correct here without the usual half-open tie-breaking.
fn fill_ring(outline: &Outline, completed: &mut [bool], width: usize, height: usize) {
    if outline.points.is_empty() {
        return;
    }
    let mut crossings: Vec<i32> = Vec::new();
    for y in 0..height {
        // Twice the scanline's y, so the comparison stays in integers: the line
        // is at `y + 0.5`, and `2y + 1` is odd where every vertex is even.
        let line = 2 * i32::try_from(y).expect("a mask shorter than i32::MAX") + 1;
        crossings.clear();

        for index in 0..outline.points.len() {
            let (x0, y0) = outline.points[index];
            let (_, y1) = outline.points[(index + 1) % outline.points.len()];
            if y0 == y1 {
                continue;
            }
            let (low, high) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            if 2 * low < line && line < 2 * high {
                // Only vertical edges can cross, and on a corner-aligned ring a
                // vertical edge has one x.
                crossings.push(x0);
            }
        }

        crossings.sort_unstable();
        for pair in crossings.chunks_exact(2) {
            let from = pair[0].max(0) as usize;
            let to = (pair[1].max(0) as usize).min(width);
            for x in from..to {
                completed[y * width + x] = true;
            }
        }
    }
}
