//! Which object is under a point, and where an object is.
//!
//! `docs/dev/VIEWER.md` §3.5b. Selecting a detection by clicking it is a
//! question about **slide** coordinates: the click is converted once, by the
//! same [`Viewport::to_slide`](crate::Viewport::to_slide) the status bar's cursor
//! readout uses, and everything here is in level 0 — the frame
//! [`Overlay`] stores outlines in whatever level the run read. Nothing here
//! knows about the screen, the pyramid level being shown, or the interface's
//! scale factor, which is what keeps those three out of the answer.
//!
//! # A linear scan, and the number that says it is enough
//!
//! [`object_at`] tests every bounding box and runs the polygon test only on the
//! few whose box holds the point. There is no spatial index, and that is a
//! measurement rather than a shrug: `examples/pick_cost.rs` builds 150,000
//! nucleus-sized rings over a CMU-1-sized slide — the size of a whole-slide
//! run — and times the scan. The figure is in `docs/dev/VIEWER.md` §3.5b; it
//! is a small fraction of one frame, and it is paid once per *click*, not per
//! frame. The bounds are one contiguous `[f32; 4]` array (`Overlay`'s own
//! columnar choice), so the scan is four comparisons per object over memory
//! that prefetches. An index would be a second structure to keep in step with
//! the overlay for a cost nobody can perceive.
//!
//! # Inside, for a ring that follows pixel corners
//!
//! The rings a detection run produces follow pixel *corners*, so two nuclei
//! that touch share an edge exactly, and a click on that edge is a tie. The
//! crossing test below is half-open — an edge owns its lower endpoint and not
//! its upper one, and a point on a vertical edge belongs to the polygon on its
//! right — which gives every point of a shared edge to exactly one of the two
//! polygons. The tests hold a pair of touching squares to that.

use crate::coords::{ScreenPoint, SlidePoint};
use crate::overlay::Overlay;
use crate::viewport::Viewport;

/// Whether `(x, y)` is inside `ring`, by the even-odd rule.
///
/// `ring` is implicitly closed, as [`Overlay::outline`] returns it. Fewer than
/// three vertices enclose nothing. Half-open on both axes; see the module
/// documentation for why that matters on a pixel lattice.
#[must_use]
pub fn contains(ring: &[[f32; 2]], x: f64, y: f64) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut previous = ring[ring.len() - 1];
    for &vertex in ring {
        let (x0, y0) = (f64::from(previous[0]), f64::from(previous[1]));
        let (x1, y1) = (f64::from(vertex[0]), f64::from(vertex[1]));
        // An edge counts when it straddles the horizontal line through the
        // point, owning its lower end and not its upper one — so a horizontal
        // edge never counts and a vertex is never counted twice.
        if (y0 > y) != (y1 > y) {
            let crossing = x0 + (y - y0) * (x1 - x0) / (y1 - y0);
            if x < crossing {
                inside = !inside;
            }
        }
        previous = vertex;
    }
    inside
}

/// The object whose outline contains `point`, or `None` over empty slide.
///
/// Where outlines overlap — which a detection run's seam resolution prevents,
/// but an imported file need not — the one with the **smallest** bounding box
/// wins, so a click lands on the nucleus rather than on something drawn around
/// it; ties go to the lower row. See the module documentation for what this
/// costs at whole-slide sizes.
#[must_use]
pub fn object_at(overlay: &Overlay, point: SlidePoint) -> Option<usize> {
    let (x, y) = (point.x, point.y);
    let mut best: Option<(usize, f64)> = None;
    for (object, box_) in overlay.bounds().iter().enumerate() {
        let (left, top) = (f64::from(box_[0]), f64::from(box_[1]));
        let (width, height) = (f64::from(box_[2]), f64::from(box_[3]));
        // A rejection, so a NaN box falls out, as it does in the cull.
        if !(x >= left && x <= left + width && y >= top && y <= top + height) {
            continue;
        }
        if !contains(overlay.outline(object), x, y) {
            continue;
        }
        let size = width * height;
        if best.is_none_or(|(_, smallest)| size < smallest) {
            best = Some((object, size));
        }
    }
    best.map(|(object, _)| object)
}

/// The centre of mass of the area `ring` encloses, in slide coordinates.
///
/// The polygon (shoelace) centroid, not the mean of the vertices: a traced
/// outline has vertices wherever the boundary turns, so a nucleus with a
/// ragged side would have its vertex mean dragged towards that side. `None`
/// for a ring that encloses no area.
///
/// This is **this project's** number, computed from the unsmoothed outline the
/// run traced. QuPath reports a centroid of its own ROI, which for a cell
/// detection is smoothed; the two have not been compared.
#[must_use]
pub fn centroid(ring: &[[f32; 2]]) -> Option<SlidePoint> {
    if ring.len() < 3 {
        return None;
    }
    // Relative to the first vertex, so the products stay small: at CMU-1's
    // 46,000-pixel extent a raw `x * y` is 2e9, and the cancellation between
    // neighbouring terms is where a centroid loses its digits.
    let (ox, oy) = (f64::from(ring[0][0]), f64::from(ring[0][1]));
    let (mut twice_area, mut cx, mut cy) = (0.0, 0.0, 0.0);
    let mut previous = ring[ring.len() - 1];
    for &vertex in ring {
        let (x0, y0) = (f64::from(previous[0]) - ox, f64::from(previous[1]) - oy);
        let (x1, y1) = (f64::from(vertex[0]) - ox, f64::from(vertex[1]) - oy);
        let cross = x0 * y1 - x1 * y0;
        twice_area += cross;
        cx += (x0 + x1) * cross;
        cy += (y0 + y1) * cross;
        previous = vertex;
    }
    if twice_area == 0.0 {
        return None;
    }
    Some(SlidePoint::new(
        ox + cx / (3.0 * twice_area),
        oy + cy / (3.0 * twice_area),
    ))
}

/// Offsets, in screen pixels, at which [`highlight_into`] repeats a selected
/// outline.
///
/// The centre and its four neighbours: a line three pixels wide in a plus
/// shape, drawn as five one-pixel lines because both backends draw one-pixel
/// lines and nothing else — the GPU path as a `LineList`, the CPU path as a
/// DDA. Widening by repetition keeps the two on the one primitive they already
/// share rather than giving one of them a thick-line rule the other lacks.
pub const HIGHLIGHT_OFFSETS: [(f32, f32); 5] =
    [(0.0, 0.0), (-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)];

/// Writes the screen-space segments that draw `object` as selected into `out`.
///
/// `out` is cleared first. The outline is projected with
/// [`Viewport::to_screen`], the arithmetic `Overlay::segments_into` is tested
/// to agree with, and repeated at [`HIGHLIGHT_OFFSETS`] so the selection reads
/// as thicker than every other outline whatever colour those are.
///
/// Drawn whatever its size, unlike the overlay's own cull: a selected object
/// zoomed out to a speck is still the one selected, and a dot where it is says
/// more than nothing.
///
/// # Panics
///
/// Panics if `object` is past the end of `overlay`.
pub fn highlight_into(
    overlay: &Overlay,
    object: usize,
    view: Viewport,
    out: &mut Vec<ScreenPoint>,
) {
    out.clear();
    let ring = overlay.outline(object);
    if ring.len() < 2 {
        return;
    }
    let project = |vertex: &[f32; 2]| {
        view.to_screen(SlidePoint::new(f64::from(vertex[0]), f64::from(vertex[1])))
    };
    for (dx, dy) in HIGHLIGHT_OFFSETS {
        for (from, to) in ring
            .iter()
            .zip(ring.iter().cycle().skip(1))
            .take(ring.len())
        {
            let (from, to) = (project(from), project(to));
            out.push(ScreenPoint::new(from.x + dx, from.y + dy));
            out.push(ScreenPoint::new(to.x + dx, to.y + dy));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An axis-aligned rectangle in level coordinates, clockwise as the
    /// tracer winds them.
    fn rectangle(x: u32, y: u32, width: u32, height: u32) -> Vec<[u32; 2]> {
        vec![
            [x, y],
            [x + width, y],
            [x + width, y + height],
            [x, y + height],
        ]
    }

    #[test]
    fn a_point_inside_is_inside_and_one_outside_is_not() {
        // An L, so the bounding box holds points the polygon does not: the
        // notch at the top right is inside the box and outside the shape,
        // which is the case a box-only test gets wrong.
        let ring = [
            [0.0, 0.0],
            [4.0, 0.0],
            [4.0, 2.0],
            [10.0, 2.0],
            [10.0, 6.0],
            [0.0, 6.0],
        ];
        assert!(contains(&ring, 1.0, 1.0));
        assert!(contains(&ring, 8.0, 4.0));
        assert!(!contains(&ring, 8.0, 1.0), "the notch is outside");
        assert!(!contains(&ring, 11.0, 4.0));
        assert!(!contains(&ring, 5.0, -1.0));
    }

    #[test]
    fn a_shared_edge_belongs_to_exactly_one_of_two_touching_outlines() {
        // Two nuclei traced on the pixel lattice share their boundary exactly.
        // Every point along it — including the corners — must select one
        // object, never both and never neither.
        let left = [[0.0, 0.0], [5.0, 0.0], [5.0, 5.0], [0.0, 5.0]];
        let right = [[5.0, 0.0], [9.0, 0.0], [9.0, 5.0], [5.0, 5.0]];
        for step in 0..10 {
            let y = f64::from(step) * 0.5;
            let owners = u8::from(contains(&left, 5.0, y)) + u8::from(contains(&right, 5.0, y));
            // The top and bottom corners lie on the outer boundary too, and
            // the half-open rule gives the top one to both-or-neither in the
            // vertical direction; only the shared edge itself is asserted.
            if y > 0.0 && y < 5.0 {
                assert_eq!(owners, 1, "the shared edge at y = {y}");
            }
        }
    }

    #[test]
    fn object_at_finds_the_object_under_the_point_at_a_level() {
        // Level coordinates scaled by a downsample of 16 — pyramid level 2 of
        // an Aperio slide — so an object at level (10, 30) covers slide
        // 160..224 by 480..544. The two objects' x and y differ by hundreds of
        // pixels in both directions, so a point with its axes swapped lands on
        // neither, rather than on the other one by luck.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&rectangle(10, 30, 4, 4), 16.0);
        overlay.push_level_ring(&rectangle(60, 5, 4, 4), 16.0);

        assert_eq!(object_at(&overlay, SlidePoint::new(190.0, 500.0)), Some(0));
        assert_eq!(object_at(&overlay, SlidePoint::new(990.0, 110.0)), Some(1));
        // Swapped axes: empty slide.
        assert_eq!(object_at(&overlay, SlidePoint::new(500.0, 190.0)), None);
        // The level coordinate itself, as if the downsample were forgotten:
        // also empty slide.
        assert_eq!(object_at(&overlay, SlidePoint::new(11.0, 31.0)), None);
    }

    #[test]
    fn the_smallest_of_overlapping_objects_wins() {
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&rectangle(0, 0, 100, 100), 1.0);
        overlay.push_level_ring(&rectangle(40, 40, 10, 10), 1.0);
        assert_eq!(object_at(&overlay, SlidePoint::new(45.0, 45.0)), Some(1));
        assert_eq!(object_at(&overlay, SlidePoint::new(20.0, 70.0)), Some(0));
    }

    #[test]
    fn an_empty_ring_is_never_hit() {
        // `push_level_ring` keeps a row for an empty ring with a zero box at
        // the origin, so the origin is inside that box. It must not select it.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&[], 1.0);
        assert_eq!(object_at(&overlay, SlidePoint::new(0.0, 0.0)), None);
    }

    #[test]
    fn the_centroid_is_the_centre_of_area_not_of_vertices() {
        // A rectangle with extra vertices crowded along its left side: the
        // vertex mean moves left, the centre of area does not.
        let ring = [
            [0.0, 0.0],
            [10.0, 0.0],
            [10.0, 4.0],
            [0.0, 4.0],
            [0.0, 3.0],
            [0.0, 2.0],
            [0.0, 1.0],
        ];
        let centre = centroid(&ring).expect("a ring with area");
        assert!((centre.x - 5.0).abs() < 1e-9, "{centre:?}");
        assert!((centre.y - 2.0).abs() < 1e-9, "{centre:?}");
        // Far from the origin, where the products are large.
        let far: Vec<[f32; 2]> = ring
            .iter()
            .map(|v| [v[0] + 45_000.0, v[1] + 32_000.0])
            .collect();
        let centre = centroid(&far).expect("a ring with area");
        assert!((centre.x - 45_005.0).abs() < 1e-6, "{centre:?}");
        assert!((centre.y - 32_002.0).abs() < 1e-6, "{centre:?}");
        assert_eq!(centroid(&[[0.0, 0.0], [1.0, 1.0]]), None);
    }

    #[test]
    fn the_highlight_is_the_outline_where_the_overlay_draws_it() {
        // The unshifted copy of the highlight must be exactly the segments the
        // overlay draws for that object, or the selection is drawn beside the
        // thing selected.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&rectangle(3, 7, 8, 12), 2.0);
        let view = Viewport {
            centre: SlidePoint::new(13.0, 29.0),
            scale: 0.7,
            width: 100,
            height: 80,
        };
        let (mut plain, mut highlight) = (Vec::new(), Vec::new());
        overlay.segments_into(view, 0.0, &mut plain);
        highlight_into(&overlay, 0, view, &mut highlight);
        assert_eq!(highlight.len(), plain.len() * HIGHLIGHT_OFFSETS.len());
        assert_eq!(&highlight[..plain.len()], plain.as_slice());
        // And the copies are shifted by whole pixels, one per offset.
        assert_eq!(highlight[plain.len()].x, plain[0].x - 1.0);
        assert_eq!(highlight[plain.len()].y, plain[0].y);
    }
}
