//! Detected objects drawn over the slide.
//!
//! `docs/dev/VIEWER.md` §3.5. Objects live in **slide coordinates** — level 0, the
//! canonical frame of §5 — because that is the only frame that survives the
//! viewer changing level under them. A detection run reads one level and returns
//! outlines in that level's pixels; converting once on the way in means a pan
//! that crosses a level boundary does not have to reinterpret geometry it has
//! already drawn twenty times.
//!
//! # What this knows about the pipeline
//!
//! Nothing. [`Overlay::push_level_ring`] takes lattice corners and a downsample,
//! which is what `qupath_pipeline::Detections` happens to hold, and would take
//! them just as happily from a mask traced somewhere else or from a file. The
//! library still depends on `qupath_slide` alone, which `docs/dev/VIEWER.md` §7
//! settled and this had no reason to unsettle.
//!
//! # Outlines, not fills
//!
//! An outline is a line strip and a fill is a scan conversion, and at the sizes
//! that matter — a few thousand nuclei on screen — the outline is both cheaper
//! and the thing that lets you see the slide underneath it. QuPath defaults the
//! same way and puts the fill behind a toggle. Nothing here forecloses a fill;
//! the rings are closed and wound clockwise, which is what a fill rule needs.
//!
//! # Culling, and the number that justifies doing it the simple way
//!
//! Two rejections, in this order, both per object and both from the bounding
//! box alone:
//!
//! - **off screen**, against [`Viewport::bounds`];
//! - **too small to see**, against a caller-supplied floor in screen pixels.
//!
//! The second is not an optimisation dressed up as a feature. Zoomed out far
//! enough, a nucleus is a third of a pixel; drawing forty thousand
//! sub-pixel rings produces a grey smear that is slower to compute than the
//! tiles underneath it and tells you less than they do.
//!
//! The scan is **linear** over the bounding boxes, with no spatial index. That
//! is a deliberate non-decision rather than an oversight: `ROADMAP.md` §5 and
//! `M-HOTPATH` both say to profile before building the clever thing, and
//! `examples/render.rs` reports what the pass actually costs so the question
//! can be settled with a number instead of an instinct. The boxes are one
//! contiguous `Vec<[f32; 4]>` and the test is four comparisons, which is the
//! shape a linear scan has to have before its cost means anything.
//!
//! # Why the coordinates are `f32`
//!
//! `ROADMAP.md` §5 asks for the float width of anything stored to be a decided
//! number rather than a default. Slide coordinates on the largest whole-slide
//! images run to a few hundred thousand pixels; `f32` holds every integer below
//! 16.7 million exactly and resolves 0.006 px at 46,000, which is CMU-1's long
//! edge. The viewport works in `f64` and the screen is `f32`, so this is the
//! width the geometry is *heading for* anyway, and storing it wider would double
//! the largest structure in the overlay to buy precision that the last multiply
//! throws away.

use crate::coords::ScreenPoint;
use crate::viewport::Viewport;

/// Objects to draw over the slide, in slide (level-0) coordinates.
///
/// Rows are the caller's to align: one [`Overlay::push_level_ring`] is one
/// object, an empty ring included, so an overlay built from a
/// `qupath_pipeline::Detections` shares its row indices and a later selection
/// can be a row number rather than a search.
#[derive(Debug, Clone, Default)]
pub struct Overlay {
    /// `[x, y, width, height]` per object, in slide coordinates. Its own array
    /// rather than a field of a per-object struct, because culling reads this
    /// and nothing else and wants it contiguous.
    bounds: Vec<[f32; 4]>,
    /// One arena for every ring. Object `i` occupies
    /// `vertices[starts[i]..starts[i + 1]]`.
    vertices: Vec<[f32; 2]>,
    /// `len() + 1` entries, so the last object needs no special case.
    starts: Vec<u32>,
}

impl Overlay {
    /// An empty overlay.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bounds: Vec::new(),
            vertices: Vec::new(),
            starts: vec![0],
        }
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

    /// Total vertices across every ring.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// What the overlay costs in memory, which is what a caller budgets a
    /// region against.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bounds.len() * size_of::<[f32; 4]>()
            + self.vertices.len() * size_of::<[f32; 2]>()
            + self.starts.len() * size_of::<u32>()
    }

    /// Bounding box of every object, `[x, y, width, height]` in slide
    /// coordinates.
    #[must_use]
    pub fn bounds(&self) -> &[[f32; 4]] {
        &self.bounds
    }

    /// The ring of one object, in slide coordinates.
    ///
    /// Closed implicitly: the last vertex joins the first, and is not repeated.
    ///
    /// # Panics
    ///
    /// Panics if `object` is past the end.
    #[must_use]
    pub fn outline(&self, object: usize) -> &[[f32; 2]] {
        let start = self.starts[object] as usize;
        let end = self.starts[object + 1] as usize;
        &self.vertices[start..end]
    }

    /// The box every object fits inside, or `None` when there are none.
    ///
    /// What a "zoom to the detections" control needs, and what a caller checks
    /// when the overlay appears to be somewhere the slide is not.
    #[must_use]
    pub fn extent(&self) -> Option<[f32; 4]> {
        let first = self.bounds.first()?;
        let (mut left, mut top) = (first[0], first[1]);
        let (mut right, mut bottom) = (first[0] + first[2], first[1] + first[3]);
        for box_ in &self.bounds[1..] {
            left = left.min(box_[0]);
            top = top.min(box_[1]);
            right = right.max(box_[0] + box_[2]);
            bottom = bottom.max(box_[1] + box_[3]);
        }
        Some([left, top, right - left, bottom - top])
    }

    /// Appends one object, converting a ring in level coordinates to slide
    /// coordinates.
    ///
    /// `ring` is lattice corners in the pixels of the level a detection run
    /// read, and `downsample` is that level's — `Level::downsample`, never a
    /// ratio of widths re-derived at the call site, which is §5's rule.
    ///
    /// An empty ring appends an object with a zero-sized box, so rows stay
    /// aligned with whatever produced them.
    ///
    /// # Panics
    ///
    /// Panics if the arena would exceed `u32::MAX` vertices.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "slide coordinates are f32 by decision; see the module documentation"
    )]
    pub fn push_level_ring(&mut self, ring: &[[u32; 2]], downsample: f64) {
        let (mut left, mut top) = (f32::INFINITY, f32::INFINITY);
        let (mut right, mut bottom) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for vertex in ring {
            let x = (f64::from(vertex[0]) * downsample) as f32;
            let y = (f64::from(vertex[1]) * downsample) as f32;
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
            self.vertices.push([x, y]);
        }
        self.bounds.push(if ring.is_empty() {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [left, top, right - left, bottom - top]
        });
        self.starts
            .push(u32::try_from(self.vertices.len()).expect("vertex count exceeds u32"));
    }

    /// Releases the slack the arenas have after a run.
    pub fn shrink_to_fit(&mut self) {
        self.bounds.shrink_to_fit();
        self.vertices.shrink_to_fit();
        self.starts.shrink_to_fit();
    }

    /// Writes the screen-space line segments of every visible object into `out`,
    /// and returns how many objects contributed.
    ///
    /// `out` is cleared and refilled, so a caller drawing every frame allocates
    /// once. Segments come in pairs — `out[0]` to `out[1]` is one line — which
    /// is a `LineList` vertex buffer on the GPU path and a loop over
    /// `chunks_exact(2)` on the CPU one. That is the same shape the tiles have:
    /// everything up to the draw list is shared, and only the last step differs.
    ///
    /// `min_screen_px` drops any object whose bounding box is under that many
    /// screen pixels on **both** axes. Zero draws everything.
    ///
    /// Segments are not clipped to the window. A caller that cannot cope with a
    /// coordinate outside it should clip; [`crate::compose::Framebuffer`] does.
    pub fn segments_into(
        &self,
        view: Viewport,
        min_screen_px: f32,
        out: &mut Vec<ScreenPoint>,
    ) -> usize {
        out.clear();
        let (left, top, right, bottom) = view.bounds();
        // Hoisted out of the loop: `Viewport::to_screen` recomputes `bounds`
        // every call, and this runs once per vertex of every visible object.
        // The arithmetic below is that function's, not a second version of it.
        let scale = view.scale;
        let floor = f64::from(min_screen_px);

        #[expect(
            clippy::cast_possible_truncation,
            reason = "screen space is f32 by definition; see coords::ScreenPoint"
        )]
        let project = |vertex: &[f32; 2]| {
            ScreenPoint::new(
                ((f64::from(vertex[0]) - left) * scale) as f32,
                ((f64::from(vertex[1]) - top) * scale) as f32,
            )
        };

        let mut drawn = 0;
        for (object, box_) in self.bounds.iter().enumerate() {
            let (x0, y0) = (f64::from(box_[0]), f64::from(box_[1]));
            let (x1, y1) = (x0 + f64::from(box_[2]), y0 + f64::from(box_[3]));
            // Written as a rejection rather than an acceptance so a NaN box —
            // which nothing here produces, but a file might — falls out instead
            // of being drawn at an unpredictable place.
            if !(x1 >= left && x0 <= right && y1 >= top && y0 <= bottom) {
                continue;
            }
            if f64::from(box_[2]).max(f64::from(box_[3])) * scale < floor {
                continue;
            }

            let ring = self.outline(object);
            if ring.len() < 2 {
                continue;
            }
            // `n` pairs for `n` vertices: every edge including the one that
            // closes the ring. A ring drawn with `n - 1` edges leaves a gap that
            // is invisible on a nucleus and obvious on a rectangle.
            for (from, to) in ring
                .iter()
                .zip(ring.iter().cycle().skip(1))
                .take(ring.len())
            {
                out.push(project(from));
                out.push(project(to));
            }
            drawn += 1;
        }
        drawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::SlidePoint;

    /// A unit square at `(x, y)` in level coordinates.
    fn square(x: u32, y: u32) -> Vec<[u32; 2]> {
        vec![[x, y], [x + 1, y], [x + 1, y + 1], [x, y + 1]]
    }

    fn view(centre: (f64, f64), scale: f64) -> Viewport {
        Viewport {
            centre: SlidePoint::new(centre.0, centre.1),
            scale,
            width: 100,
            height: 100,
        }
    }

    #[test]
    fn a_level_ring_is_scaled_to_slide_coordinates() {
        // The conversion the whole module exists to do once rather than every
        // frame. A run at level 2 of an Aperio pyramid has a downsample of 4,
        // so a unit square at (10, 20) covers slide pixels 40..44 by 80..84.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&square(10, 20), 4.0);

        assert_eq!(overlay.len(), 1);
        assert_eq!(
            overlay.outline(0),
            &[[40.0, 80.0], [44.0, 80.0], [44.0, 84.0], [40.0, 84.0]]
        );
        assert_eq!(overlay.bounds()[0], [40.0, 80.0, 4.0, 4.0]);
        assert_eq!(overlay.extent(), Some([40.0, 80.0, 4.0, 4.0]));
    }

    #[test]
    fn rings_of_different_lengths_stay_separate() {
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&square(0, 0), 1.0);
        overlay.push_level_ring(&[], 1.0);
        overlay.push_level_ring(&[[5, 5], [9, 5], [5, 9]], 1.0);

        assert_eq!(overlay.len(), 3);
        assert_eq!(overlay.outline(0).len(), 4);
        assert!(overlay.outline(1).is_empty());
        assert_eq!(overlay.outline(2).len(), 3);
        // The empty one still holds a row, so an overlay built from a set of
        // detections can be indexed by the same number.
        assert_eq!(overlay.bounds()[1], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(overlay.vertex_count(), 7);
    }

    #[test]
    fn an_object_off_the_view_contributes_nothing() {
        // Culling, stated as the thing it must never get wrong: an object off
        // screen costs no vertices at all, and an object on screen is not
        // dropped because its centre happens to be outside.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&square(0, 0), 1.0); // slide 0..1
        overlay.push_level_ring(&square(500, 500), 1.0); // far away

        let mut out = Vec::new();
        let drawn = overlay.segments_into(view((0.5, 0.5), 1.0), 0.0, &mut out);
        assert_eq!(drawn, 1);
        assert_eq!(out.len(), 8, "one square is four segments, so eight points");
    }

    #[test]
    fn an_object_straddling_the_edge_is_still_drawn() {
        // The rejection is on overlap, not on containment. An outline half off
        // the window must still be emitted, or every object at the edge of the
        // view pops in as you pan onto it.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&[[0, 0], [40, 0], [40, 40], [0, 40]], 1.0);

        // A view of slide 50..150, so the object's right edge just reaches it.
        let mut out = Vec::new();
        assert_eq!(
            overlay.segments_into(view((100.0, 20.0), 1.0), 0.0, &mut out),
            0
        );
        assert_eq!(
            overlay.segments_into(view((90.0, 20.0), 1.0), 0.0, &mut out),
            1
        );
    }

    #[test]
    fn the_ring_is_closed() {
        // `n` vertices give `n` segments, not `n - 1`. The missing edge is
        // invisible on a nucleus and obvious on anything with corners, which is
        // exactly the kind of defect that survives being looked at.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&square(0, 0), 1.0);

        let mut out = Vec::new();
        overlay.segments_into(view((0.5, 0.5), 10.0), 0.0, &mut out);
        assert_eq!(out.len(), 8);
        // The last segment ends where the first began.
        assert_eq!(out[7], out[0]);
    }

    #[test]
    fn a_sub_pixel_object_is_dropped_at_the_floor_and_kept_below_it() {
        // The second cull. A 4 px object at scale 0.25 is one screen pixel, so
        // a floor of 2 drops it and a floor of 0 does not — and the assertion
        // covers both directions, because a filter that drops everything also
        // passes the first half.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&[[0, 0], [4, 0], [4, 4], [0, 4]], 1.0);

        let mut out = Vec::new();
        assert_eq!(
            overlay.segments_into(view((2.0, 2.0), 0.25), 2.0, &mut out),
            0
        );
        assert!(out.is_empty());
        assert_eq!(
            overlay.segments_into(view((2.0, 2.0), 0.25), 0.0, &mut out),
            1
        );
        assert_eq!(
            overlay.segments_into(view((2.0, 2.0), 4.0), 2.0, &mut out),
            1
        );
    }

    #[test]
    fn projection_matches_the_viewport_the_tiles_use() {
        // The one assertion that keeps the overlay attached to the slide. This
        // module projects vertices itself rather than calling `to_screen` per
        // vertex, so the two arithmetics have to be shown to agree — a half
        // pixel here draws every nucleus beside the cell it belongs to, at
        // every zoom, and looks entirely plausible.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&[[3, 7], [11, 7], [11, 19], [3, 19]], 2.0);

        // The object covers slide 6..22 by 14..38, so every centre here has it
        // in view — the assertion that something was drawn is a guard against
        // this test passing by drawing nothing, not part of the claim.
        for (centre, scale) in [
            ((14.0, 26.0), 1.0),
            ((13.0, 29.0), 0.7),
            ((10.0, 20.0), 3.3),
            ((14.0, 26.0), 0.031),
        ] {
            let view = view(centre, scale);
            let mut out = Vec::new();
            overlay.segments_into(view, 0.0, &mut out);
            assert!(!out.is_empty(), "nothing drawn at {centre:?} scale {scale}");
            for (segment, vertex) in out.chunks_exact(2).zip(overlay.outline(0)) {
                let expected =
                    view.to_screen(SlidePoint::new(f64::from(vertex[0]), f64::from(vertex[1])));
                assert_eq!(segment[0], expected, "at {centre:?} scale {scale}");
            }
        }
    }

    #[test]
    fn out_is_reused_rather_than_grown() {
        // `segments_into` clears rather than appends, which is what makes it
        // safe to call every frame with the same buffer. A version that
        // appended would look correct for one frame and leak for the rest.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&square(0, 0), 1.0);

        let mut out = Vec::new();
        overlay.segments_into(view((0.5, 0.5), 1.0), 0.0, &mut out);
        let capacity = out.len();
        overlay.segments_into(view((0.5, 0.5), 1.0), 0.0, &mut out);
        assert_eq!(out.len(), capacity);
        // And a view that sees nothing empties it, rather than leaving the last
        // frame's segments to be drawn again.
        overlay.segments_into(view((9000.0, 9000.0), 1.0), 0.0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn an_empty_overlay_has_no_extent() {
        let overlay = Overlay::new();
        assert!(overlay.is_empty());
        assert_eq!(overlay.extent(), None);
        assert_eq!(overlay.bytes(), size_of::<u32>());
    }
}
