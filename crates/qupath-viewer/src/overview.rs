//! Where the view is on the slide, as a small picture of the whole thing.
//!
//! `docs/VIEWER.md` §8.2. Ported from QuPath's `ImageOverview.java`, and like
//! [`crate::scalebar`] the portable part is the arithmetic rather than the
//! drawing: a `Canvas`, a `WritableImage` and a `PathIterator` there, and the
//! caller's business here. What this module owns is the two mappings that make
//! the picture mean something —
//!
//! - **slide to overview**, which places the rectangle showing what is on
//!   screen, and
//! - **overview to slide**, which turns a click into a centre to jump to.
//!
//! # Why this exists at all
//!
//! Zoomed in past a few hundred microns there is nothing on screen that says
//! *where on the slide you are*. The status bar gives level-0 coordinates,
//! which is a number rather than a position: `(27776, 5952)` does not tell
//! anyone they are looking at the second tissue section from the left. A
//! thumbnail with a rectangle on it is the smallest thing that answers the
//! question, which is why §8.2 put it at the top of the list rather than
//! judging it by its 271 lines.
//!
//! # The one invariant worth stating
//!
//! [`Overview::to_slide`] and [`Overview::visible_rect`] must agree: clicking
//! the centre of the drawn rectangle must not move the view. That is the same
//! shape of property as [`crate::viewport::Viewport::to_slide`] against
//! `to_screen`, and it is pinned the same way — a viewer that disagrees with
//! itself here jumps somewhere else every time it is asked to jump *here*.
//!
//! # What is not ported
//!
//! QuPath keeps two copies of the thumbnail — the server's and a smoothly
//! rescaled one — because its display can be changed by colour transforms and
//! because `drawImage` rescaling on the fly "produces a very low-quality
//! image". Both reasons are about JavaFX's drawing, not about the geometry, so
//! neither is reproduced. The caller holds whatever pixels it has;
//! [`Overview::size`] tells it how large to make them.

use crate::coords::{ScreenPoint, SlidePoint};
use crate::viewport::Viewport;

/// The longest an overview may be on either axis, in screen pixels.
///
/// QuPath's `ImageOverview.maxLength`, which is 150 and is a field rather than
/// a constant there only because nothing ever writes it.
pub const MAX_LENGTH: u32 = 150;

/// The colour of the visible-region rectangle, as `0xAARRGGBB`.
///
/// `Color.rgb(200, 0, 0, .8)` — red at four-fifths alpha, so the slide shows
/// through it rather than being replaced by it.
pub const RECT_COLOUR: u32 = 0xCC_C8_00_00;

/// The colour of the border drawn around the overview, as `0xAARRGGBB`.
///
/// `Color.rgb(64, 64, 64)`, opaque. The border is what separates a pale slide
/// from a pale panel; without it a mostly-glass thumbnail has no edge.
pub const BORDER_COLOUR: u32 = 0xFF_40_40_40;

/// How wide the border is, in screen pixels. QuPath's `g.setLineWidth(2)`.
pub const BORDER_WIDTH: f32 = 2.0;

/// A rectangle in overview pixels: `[x, y, width, height]`.
pub type Rect = [f32; 4];

/// The overview of one slide: how big the picture is, and what maps onto it.
///
/// Holds no pixels. The thumbnail is the caller's — `examples/dump` already
/// writes exactly the image this describes — and everything here is derived
/// from the slide's level-0 extent and the size the picture was given.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Overview {
    /// Level-0 width of the slide, in pixels.
    slide_width: u32,
    /// Level-0 height of the slide, in pixels.
    slide_height: u32,
    /// Width of the overview picture, in screen pixels.
    width: u32,
    /// Height of the overview picture, in screen pixels.
    height: u32,
}

impl Overview {
    /// The overview for a slide of `slide_width` by `slide_height`, fitted
    /// inside `max_length` on its longer axis.
    ///
    /// `None` for a degenerate slide. A zero extent makes every mapping here a
    /// division by zero, and there is no picture to draw of a slide with no
    /// pixels — the caller shows nothing, which is the honest answer and the
    /// same one [`crate::scalebar::scalebar`] gives for a slide with no pixel
    /// size.
    ///
    /// # Rounding, and why the picture can be one pixel off the ratio
    ///
    /// QuPath scales by `maxLength / max(width, height)` and *rounds* both
    /// axes, so a slide whose aspect ratio does not divide evenly gets a
    /// picture that is not exactly proportional — 150 by 113 for a 46000 by
    /// 32914 slide, where the exact ratio wants 107.3. Rounding rather than
    /// truncating is copied deliberately: the difference is at most half a
    /// pixel of aspect ratio and is invisible, where a truncated axis on a
    /// nearly-square slide loses a whole pixel of a 150-pixel picture.
    ///
    /// The short axis is clamped to at least one pixel, because a slide 46000
    /// by 100 rounds its short axis to zero and a zero-height picture is a
    /// division by zero downstream rather than a thin one.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "both axes are a rounded fraction of max_length, so within u32"
    )]
    pub fn new(slide_width: u32, slide_height: u32, max_length: u32) -> Option<Self> {
        if slide_width == 0 || slide_height == 0 || max_length == 0 {
            return None;
        }
        let longest = f64::from(slide_width.max(slide_height));
        let scale = f64::from(max_length) / longest;
        let width = (f64::from(slide_width) * scale).round().max(1.0) as u32;
        let height = (f64::from(slide_height) * scale).round().max(1.0) as u32;
        Some(Self {
            slide_width,
            slide_height,
            width,
            height,
        })
    }

    /// The overview for a slide at QuPath's [`MAX_LENGTH`].
    #[must_use]
    pub fn fit(slide_width: u32, slide_height: u32) -> Option<Self> {
        Self::new(slide_width, slide_height, MAX_LENGTH)
    }

    /// How large the picture is, in screen pixels: `(width, height)`.
    ///
    /// What the caller allocates a thumbnail to, and what it lays out.
    #[must_use]
    pub fn size(self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Overview pixels per level-0 pixel, on the x axis.
    ///
    /// Separate from [`Overview::scale_y`] because the rounding above means the
    /// two are not exactly equal, and using one for both puts the rectangle
    /// progressively out of place along the longer axis.
    #[must_use]
    pub fn scale_x(self) -> f64 {
        f64::from(self.width) / f64::from(self.slide_width)
    }

    /// Overview pixels per level-0 pixel, on the y axis.
    #[must_use]
    pub fn scale_y(self) -> f64 {
        f64::from(self.height) / f64::from(self.slide_height)
    }

    /// Where a slide point lands in the picture.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "overview coordinates are screen-sized and f32 by the same decision as ScreenPoint"
    )]
    pub fn to_overview(self, point: SlidePoint) -> ScreenPoint {
        ScreenPoint::new(
            (point.x * self.scale_x()) as f32,
            (point.y * self.scale_y()) as f32,
        )
    }

    /// Where a click on the picture falls on the slide.
    ///
    /// QuPath's `mouseViewerToLocation`, which is a proportion of the width
    /// rather than an inverse of the transform: `x / getWidth() *
    /// server.getWidth()`. The result is what the viewer's centre becomes, so
    /// this is a *centre*, not a corner — clicking the middle of the picture
    /// centres the view on the middle of the slide.
    ///
    /// Not clamped. A click outside the picture is the caller's to reject, and
    /// [`crate::viewport::Viewport::clamp_to_extent`] already pulls a centre
    /// back inside the slide, so clamping here would be the same rule applied
    /// twice in two places that could disagree.
    #[must_use]
    pub fn to_slide(self, point: ScreenPoint) -> SlidePoint {
        SlidePoint::new(
            f64::from(point.x) / f64::from(self.width) * f64::from(self.slide_width),
            f64::from(point.y) / f64::from(self.height) * f64::from(self.slide_height),
        )
    }

    /// The rectangle showing what `view` has on screen, in overview pixels.
    ///
    /// `[x, y, width, height]`, and it **may fall outside the picture** or be
    /// larger than it: zoomed out past [`Viewport::fit`] the window shows more
    /// than the slide, which is exactly when the rectangle stops being
    /// informative. The caller decides whether to draw it — see
    /// [`Overview::covers_everything`] — because clipping it here would silently
    /// turn "you are seeing all of it" into "you are seeing precisely the
    /// slide", which are different statements.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "overview coordinates are screen-sized and f32 by decision"
    )]
    pub fn visible_rect(self, view: Viewport) -> Rect {
        let (left, top, right, bottom) = view.bounds();
        let x = left * self.scale_x();
        let y = top * self.scale_y();
        [
            x as f32,
            y as f32,
            ((right - left) * self.scale_x()) as f32,
            ((bottom - top) * self.scale_y()) as f32,
        ]
    }

    /// Whether the view already contains the whole slide.
    ///
    /// QuPath skips the rectangle when the viewer is at zoom-to-fit, "in which
    /// case everything is visible". The test is against the slide's extent
    /// rather than against the scale [`Viewport::fit`] returns, because a
    /// window resize or a pan reaches the same condition without going through
    /// `fit` — comparing to a remembered scale would answer correctly only for
    /// views that arrived one particular way.
    #[must_use]
    pub fn covers_everything(self, view: Viewport) -> bool {
        let (left, top, right, bottom) = view.bounds();
        left <= 0.0
            && top <= 0.0
            && right >= f64::from(self.slide_width)
            && bottom >= f64::from(self.slide_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CMU-1's level-0 extent, which is what every ratio here is checked
    /// against.
    const CMU1: (u32, u32) = (46000, 32914);

    fn view(centre: (f64, f64), scale: f64) -> Viewport {
        Viewport {
            centre: SlidePoint::new(centre.0, centre.1),
            scale,
            width: 1024,
            height: 768,
        }
    }

    #[test]
    fn fits_the_long_axis_to_the_maximum() {
        let overview = Overview::fit(CMU1.0, CMU1.1).expect("real slide");
        let (width, height) = overview.size();
        assert_eq!(width, MAX_LENGTH, "the long axis is the one that is capped");
        // 32914 / 46000 * 150 = 107.3, which rounds to 107.
        assert_eq!(height, 107);
    }

    #[test]
    fn a_tall_slide_caps_its_height_instead() {
        let overview = Overview::fit(32914, 46000).expect("portrait slide");
        assert_eq!(overview.size(), (107, MAX_LENGTH));
    }

    /// The short axis of an extremely wide slide rounds to zero, and a
    /// zero-height picture divides by zero in `to_slide`.
    #[test]
    fn a_very_wide_slide_keeps_at_least_one_pixel() {
        let overview = Overview::fit(46000, 100).expect("wide slide");
        let (_, height) = overview.size();
        assert_eq!(height, 1);
        // The mapping still answers rather than producing NaN.
        assert!(overview.to_slide(ScreenPoint::new(75.0, 0.0)).y.is_finite());
    }

    #[test]
    fn a_degenerate_slide_has_no_overview() {
        assert!(Overview::fit(0, 32914).is_none());
        assert!(Overview::fit(46000, 0).is_none());
        assert!(Overview::new(46000, 32914, 0).is_none());
    }

    /// The invariant the module documentation states: clicking the centre of
    /// the drawn rectangle must not move the view.
    ///
    /// This is what catches an inverted or transposed mapping, and neither
    /// function alone can — `to_slide` on its own is defensible at any scale,
    /// exactly as §3.3's level choice was defensible frame by frame.
    #[test]
    fn clicking_the_rectangles_centre_does_not_move_the_view() {
        let overview = Overview::fit(CMU1.0, CMU1.1).expect("real slide");
        for centre in [(23000.0, 16457.0), (8000.0, 4000.0), (41000.0, 29000.0)] {
            let seen = view(centre, 0.25);
            let rect = overview.visible_rect(seen);
            let middle = ScreenPoint::new(rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
            let jumped = overview.to_slide(middle);
            assert!(
                (jumped.x - centre.0).abs() < 200.0 && (jumped.y - centre.1).abs() < 200.0,
                "clicking the middle of {rect:?} jumped to {jumped:?}, not {centre:?}",
            );
        }
    }

    /// A transposed mapping would pass a square-slide test and fail here.
    #[test]
    fn the_axes_are_not_swapped() {
        let overview = Overview::fit(CMU1.0, CMU1.1).expect("real slide");
        // A point far along x and near zero on y must stay that way.
        let mapped = overview.to_overview(SlidePoint::new(46000.0, 0.0));
        assert!(
            mapped.x > 140.0,
            "x should reach the right edge: {mapped:?}"
        );
        assert!(mapped.y.abs() < 1.0, "y should stay at the top: {mapped:?}");
    }

    /// Zooming in must shrink the rectangle, at every step rather than merely
    /// at the ends — the across-scales comparison §8.1d found necessary for the
    /// scalebar, for the same reason: an inverted scale gives a plausible
    /// rectangle at every individual scale.
    #[test]
    fn zooming_in_never_grows_the_rectangle() {
        let overview = Overview::fit(CMU1.0, CMU1.1).expect("real slide");
        let centre = (23000.0, 16457.0);
        let mut previous = f32::INFINITY;
        for step in 0..12 {
            let scale = 0.02 * 1.5_f64.powi(step);
            let rect = overview.visible_rect(view(centre, scale));
            assert!(
                rect[2] <= previous + 1e-3,
                "zooming to {scale} widened the rectangle from {previous} to {}",
                rect[2],
            );
            previous = rect[2];
        }
        assert!(
            previous < 5.0,
            "zoomed right in the rectangle should be a few pixels, not {previous}",
        );
    }

    /// At `Viewport::fit` the whole slide is on screen, which is the case
    /// QuPath draws no rectangle for.
    #[test]
    fn a_fitted_view_covers_everything() {
        let overview = Overview::fit(CMU1.0, CMU1.1).expect("real slide");
        // Viewport::fit's scale for a 1024x768 window on CMU-1, from §8.1d.
        let fitted = view((23000.0, 16457.0), 1024.0 / 46000.0);
        assert!(overview.covers_everything(fitted));
        // And zoomed in, it does not.
        assert!(!overview.covers_everything(view((23000.0, 16457.0), 0.25)));
    }

    /// The rectangle is allowed to leave the picture, and the caller is told
    /// rather than being handed a clipped rectangle that claims the view sits
    /// exactly on the slide's edge.
    #[test]
    fn panning_past_an_edge_leaves_the_picture() {
        let overview = Overview::fit(CMU1.0, CMU1.1).expect("real slide");
        let rect = overview.visible_rect(view((0.0, 0.0), 0.25));
        assert!(
            rect[0] < 0.0 && rect[1] < 0.0,
            "a view centred on the corner extends past it: {rect:?}",
        );
    }

    /// The two scales differ because both axes are rounded independently, and
    /// using one for both would drift along the longer axis. Pinned so that
    /// "simplify this to a single scale" is a change someone has to argue with.
    #[test]
    fn the_two_axis_scales_are_not_identical() {
        let overview = Overview::fit(CMU1.0, CMU1.1).expect("real slide");
        assert!(
            (overview.scale_x() - overview.scale_y()).abs() > f64::EPSILON,
            "CMU-1's rounded picture is not exactly proportional",
        );
    }
}
