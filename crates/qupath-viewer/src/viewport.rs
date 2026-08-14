//! What the window is looking at, and which tiles that needs.
//!
//! Pure arithmetic over a [`Slide`]'s level table — no GPU, no window, no
//! pixels read. That is deliberate: `docs/VIEWER.md` §6 says the viewport math
//! is the part that can carry real assertions, because the rest of a viewer's
//! output is an appearance and this repository's standing constraint is to
//! assert on counts and recorded output rather than on how something looks.
//!
//! # Level selection
//!
//! [`Viewport::level`] is the first caller of [`Slide::level_for_width`], which
//! has existed unused since `qupath-slide` was written. `ROADMAP.md` §0 puts
//! "do less work" above every other optimisation, and choosing the level is
//! how a viewer does that: at 20x zoomed out, reading level 0 decodes 400x the
//! pixels the window can show.

use qupath_slide::{Level, Slide};

use crate::coords::{ScreenPoint, SlidePoint, TileIndex};

/// What the window is looking at: a centre in slide coordinates and a scale.
///
/// `scale` is screen pixels per level-0 pixel, so 1.0 shows the slide at full
/// resolution and 0.25 shows it a quarter size. It is the reciprocal of the
/// downsample a viewer wants, which is why [`Viewport::level`] inverts it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// Centre of the view, in level-0 pixels.
    pub centre: SlidePoint,
    /// Screen pixels per level-0 pixel.
    pub scale: f64,
    /// Window width in pixels.
    pub width: u32,
    /// Window height in pixels.
    pub height: u32,
}

impl Viewport {
    /// A viewport showing the whole of `slide`'s level 0, centred, in a window
    /// of `width` by `height`.
    ///
    /// The scale is whichever axis is more constrained, so the whole slide
    /// fits rather than being cropped on one side.
    #[must_use]
    pub fn fit(slide: &Slide, width: u32, height: u32) -> Self {
        let base = &slide.levels()[0];
        let scale = f64::from(width) / f64::from(base.width);
        let vertical = f64::from(height) / f64::from(base.height);
        Self {
            centre: SlidePoint::new(f64::from(base.width) / 2.0, f64::from(base.height) / 2.0),
            scale: scale.min(vertical),
            width,
            height,
        }
    }

    /// Where a screen point falls on the slide.
    ///
    /// The inverse of [`Viewport::to_screen`], and the two are tested against
    /// each other — a viewer that disagrees with itself here zooms away from
    /// the cursor instead of towards it.
    #[must_use]
    pub fn to_slide(self, point: ScreenPoint) -> SlidePoint {
        let (left, top, _, _) = self.bounds();
        SlidePoint::new(
            left + f64::from(point.x) / self.scale,
            top + f64::from(point.y) / self.scale,
        )
    }

    /// Where a slide point lands in the window.
    ///
    /// May fall outside the window; culling is [`Viewport::visible_tiles`]'s
    /// job, not this one's.
    #[must_use]
    pub fn to_screen(self, point: SlidePoint) -> ScreenPoint {
        let (left, top, _, _) = self.bounds();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "screen space is f32 by definition; see coords::ScreenPoint"
        )]
        ScreenPoint::new(
            ((point.x - left) * self.scale) as f32,
            ((point.y - top) * self.scale) as f32,
        )
    }

    /// Moves the view by a screen-space drag.
    ///
    /// The sign is the one a grab gesture wants: dragging right moves the
    /// slide right, which means the centre moves *left*.
    #[must_use]
    pub fn pan(self, dx: f64, dy: f64) -> Self {
        Self {
            centre: SlidePoint::new(
                self.centre.x - dx / self.scale,
                self.centre.y - dy / self.scale,
            ),
            ..self
        }
    }

    /// Zooms by `factor` about a fixed screen point.
    ///
    /// The slide point under `anchor` stays under `anchor`, which is what makes
    /// scroll-to-zoom feel attached to the cursor rather than to the window
    /// centre. `zoom_holds_the_anchor_still` is the test, and it is the whole
    /// reason this is a method rather than two lines in the event handler.
    ///
    /// `scale` is clamped to a sane range: past about 32 px per slide pixel
    /// there is nothing left to see, and below `1e-6` a whole slide is
    /// sub-pixel and the tile arithmetic loses meaning.
    #[must_use]
    pub fn zoom_about(self, factor: f64, anchor: ScreenPoint) -> Self {
        let target = self.to_slide(anchor);
        let scale = (self.scale * factor).clamp(1e-6, 32.0);
        // Solve for the centre that keeps `target` under `anchor` at the new
        // scale: anchor is (target - left) * scale, and left is centre minus
        // half the view, so centre = target - (anchor - half) / scale.
        let half_width = f64::from(self.width) / scale / 2.0;
        let half_height = f64::from(self.height) / scale / 2.0;
        Self {
            centre: SlidePoint::new(
                target.x - (f64::from(anchor.x) / scale - half_width),
                target.y - (f64::from(anchor.y) / scale - half_height),
            ),
            scale,
            ..self
        }
    }

    /// The view resized to a new window size, keeping the centre and scale.
    #[must_use]
    pub fn resize(self, width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            ..self
        }
    }

    /// Where a tile lands in the window, as `(x, y, width, height)` in screen
    /// pixels.
    ///
    /// Uses the tile's *visible* size, so the JPEG padding TIFF stores in edge
    /// tiles is neither drawn nor allowed to stretch the quad — the same
    /// decision [`TileIndex::visible_size`] records, carried through to the
    /// geometry.
    #[must_use]
    pub fn tile_rect(self, tile: TileIndex, level: &Level) -> (f32, f32, f32, f32) {
        let origin = tile.origin(level).to_slide(level);
        let (visible_width, visible_height) = tile.visible_size(level);
        let top_left = self.to_screen(origin);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "screen space is f32 by definition; see coords::ScreenPoint"
        )]
        let (width, height) = (
            (f64::from(visible_width) * level.downsample * self.scale) as f32,
            (f64::from(visible_height) * level.downsample * self.scale) as f32,
        );
        (top_left.x, top_left.y, width, height)
    }

    /// The extent of the view in level-0 pixels, as `(left, top, right, bottom)`.
    ///
    /// May fall outside the slide — panning past an edge is allowed, and
    /// [`Viewport::visible_tiles`] is what clips.
    #[must_use]
    pub fn bounds(self) -> (f64, f64, f64, f64) {
        let half_width = f64::from(self.width) / self.scale / 2.0;
        let half_height = f64::from(self.height) / self.scale / 2.0;
        (
            self.centre.x - half_width,
            self.centre.y - half_height,
            self.centre.x + half_width,
            self.centre.y + half_height,
        )
    }

    /// The level to read at this scale.
    ///
    /// The criterion is **resolution per pixel**, not extent: a level is good
    /// enough when one of its pixels covers no more than one screen pixel,
    /// which is `downsample <= 1 / scale`. The coarsest such level is the
    /// cheapest one that is not visibly soft.
    ///
    /// # The bug this had, which is worth keeping written down
    ///
    /// The first version asked [`Slide::level_for_width`] for `base.width *
    /// scale` and so demanded a level's *whole extent* cover the slide at
    /// screen scale. That is the same condition algebraically, but it fails on
    /// the pyramid's own rounding: at scale 0.2536 a 46000 px slide wants
    /// 11666 px, level 1 offers 11500, and the level is rejected over a **1.4%
    /// shortfall** — so the viewer jumped from level 2 straight to level 0 and
    /// decoded **221 tiles where 20 would do**, to show a window of glass.
    /// Zooming *out* reached the same scale at level 1, so the same view picked
    /// different levels depending on the direction of travel.
    ///
    /// Comparing downsamples directly is the honest form of the criterion, and
    /// it takes the tolerance with it: a level within `TOLERANCE` of ideal is
    /// not visibly soft, and refusing it costs an order of magnitude in tiles.
    /// `the_level_is_the_same_whichever_way_the_scale_is_reached` and
    /// `the_level_never_costs_more_tiles_than_the_one_below_it` pin both halves.
    #[must_use]
    pub fn level(self, slide: &Slide) -> &Level {
        /// How much coarser than ideal a level may be before it is rejected.
        ///
        /// Aperio pyramids are powers of two in practice, so the ideal
        /// downsample almost never lands on a level exactly; without slack the
        /// choice flips on floating-point noise. 5% is below what is visible
        /// and well under the 4x gap between adjacent levels, so it can never
        /// promote a level two steps too coarse.
        const TOLERANCE: f64 = 1.05;

        // One slide pixel per screen pixel is downsample 1/scale. Anything
        // coarser than that is being magnified, which is what looks soft.
        let ideal = if self.scale > 0.0 {
            1.0 / self.scale
        } else {
            f64::INFINITY
        };

        // The coarsest level that is not meaningfully softer than ideal. Levels
        // are ordered finest first, so this walks from the cheap end.
        slide
            .levels()
            .iter()
            .rfind(|level| level.downsample <= ideal * TOLERANCE)
            .unwrap_or(&slide.levels()[0])
    }

    /// Every tile of `level` the view touches, row-major.
    ///
    /// Clipped to the level, so panning past an edge yields fewer tiles rather
    /// than out-of-range ones — each returned tile satisfies
    /// [`TileIndex::is_within`] and can be passed straight to
    /// [`Slide::read_tile_into`].
    #[must_use]
    pub fn visible_tiles(self, level: &Level) -> Vec<TileIndex> {
        let (left, top, right, bottom) = self.bounds();

        // Into this level's pixels, then to tile indices. Clamping before the
        // cast keeps a far-off-slide viewport from naming absurd tiles.
        let to_column = |value: f64| {
            (value / level.downsample / f64::from(level.tile_width))
                .floor()
                .clamp(0.0, f64::from(level.columns.saturating_sub(1)))
        };
        let to_row = |value: f64| {
            (value / level.downsample / f64::from(level.tile_height))
                .floor()
                .clamp(0.0, f64::from(level.rows.saturating_sub(1)))
        };

        // Entirely off the slide in either axis means nothing is visible. This
        // has to be tested before clamping, which would pull an off-slide view
        // back onto the edge tile and draw it in the wrong place.
        let level_width = f64::from(level.width) * level.downsample;
        let level_height = f64::from(level.height) * level.downsample;
        if right < 0.0 || bottom < 0.0 || left >= level_width || top >= level_height {
            return Vec::new();
        }

        // `to_column` and `to_row` clamp into the level's own tile grid, so
        // every one of these is in range by construction.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped to [0, columns-1] / [0, rows-1] by the closures above"
        )]
        let (first_column, last_column, first_row, last_row) = (
            to_column(left) as u32,
            to_column(right) as u32,
            to_row(top) as u32,
            to_row(bottom) as u32,
        );

        let mut tiles = Vec::new();
        for row in first_row..=last_row {
            for column in first_column..=last_column {
                tiles.push(TileIndex::new(column, row, level.index));
            }
        }
        tiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(index: usize, width: u32, height: u32, downsample: f64) -> Level {
        Level {
            index,
            width,
            height,
            tile_width: 256,
            tile_height: 256,
            columns: width.div_ceil(256),
            rows: height.div_ceil(256),
            downsample,
        }
    }

    fn viewport(centre: (f64, f64), scale: f64, size: (u32, u32)) -> Viewport {
        Viewport {
            centre: SlidePoint::new(centre.0, centre.1),
            scale,
            width: size.0,
            height: size.1,
        }
    }

    #[test]
    fn bounds_are_centred_and_scale_inversely() {
        // Scale 1.0: a 800x600 window shows exactly 800x600 slide pixels.
        let view = viewport((1000.0, 1000.0), 1.0, (800, 600));
        let (left, top, right, bottom) = view.bounds();
        assert_eq!((left, top, right, bottom), (600.0, 700.0, 1400.0, 1300.0));

        // Half the scale shows twice the slide, same centre.
        let zoomed_out = viewport((1000.0, 1000.0), 0.5, (800, 600));
        let (left, top, right, bottom) = zoomed_out.bounds();
        assert_eq!((left, top, right, bottom), (200.0, 400.0, 1800.0, 1600.0));
    }

    #[test]
    fn visible_tiles_cover_exactly_the_view() {
        // A view of level-0 pixels 600..1400 spans tiles 2..5 (256 px each);
        // 700..1300 spans rows 2..5. Four columns by four rows.
        let base = level(0, 4096, 4096, 1.0);
        let tiles = viewport((1000.0, 1000.0), 1.0, (800, 600)).visible_tiles(&base);
        assert_eq!(tiles.len(), 4 * 4);
        assert!(tiles.contains(&TileIndex::new(2, 2, 0)));
        assert!(tiles.contains(&TileIndex::new(5, 5, 0)));
        assert!(!tiles.contains(&TileIndex::new(1, 2, 0)));
        assert!(!tiles.contains(&TileIndex::new(6, 2, 0)));
    }

    #[test]
    fn every_visible_tile_is_readable() {
        // The contract that matters to a caller: nothing here needs an
        // is_within check before being handed to read_tile_into. Swept across
        // scales and positions, including well past every edge.
        let base = level(0, 4096, 3000, 1.0);
        for scale in [4.0, 1.0, 0.5, 0.1, 0.01] {
            for centre in [(0.0, 0.0), (2000.0, 1500.0), (4096.0, 3000.0), (-500.0, -500.0), (9000.0, 9000.0)] {
                let view = viewport(centre, scale, (800, 600));
                for tile in view.visible_tiles(&base) {
                    assert!(
                        tile.is_within(&base),
                        "scale {scale} centre {centre:?} produced unreadable {tile:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_view_entirely_off_the_slide_sees_nothing() {
        let base = level(0, 4096, 3000, 1.0);
        // Far to the left, and far below.
        assert!(viewport((-100_000.0, 1500.0), 1.0, (800, 600)).visible_tiles(&base).is_empty());
        assert!(viewport((2000.0, 100_000.0), 1.0, (800, 600)).visible_tiles(&base).is_empty());
    }

    #[test]
    fn screen_and_slide_are_inverses() {
        // A viewer that disagrees with itself here zooms away from the cursor.
        let view = viewport((1000.0, 800.0), 0.35, (800, 600));
        for point in [(0.0, 0.0), (400.0, 300.0), (799.0, 599.0), (123.0, 456.0)] {
            let screen = ScreenPoint::new(point.0, point.1);
            let back = view.to_screen(view.to_slide(screen));
            assert!(
                (back.x - screen.x).abs() < 1e-3 && (back.y - screen.y).abs() < 1e-3,
                "{screen:?} round-tripped to {back:?}"
            );
        }
    }

    #[test]
    fn zoom_holds_the_anchor_still() {
        // The invariant that makes scroll-to-zoom feel attached to the cursor:
        // whatever slide point is under the anchor stays under it. Swept over
        // anchors and factors, in and out, including the window corners.
        let view = viewport((1000.0, 800.0), 0.25, (800, 600));
        for anchor in [(0.0, 0.0), (400.0, 300.0), (800.0, 600.0), (137.0, 42.0)] {
            let anchor = ScreenPoint::new(anchor.0, anchor.1);
            let before = view.to_slide(anchor);
            for factor in [1.1, 2.0, 8.0, 0.9, 0.5, 0.125] {
                let after = view.zoom_about(factor, anchor).to_slide(anchor);
                assert!(
                    (after.x - before.x).abs() < 1e-6 && (after.y - before.y).abs() < 1e-6,
                    "factor {factor} at {anchor:?} moved the slide point {before:?} -> {after:?}"
                );
            }
        }
    }

    #[test]
    fn zoom_clamps_rather_than_running_away() {
        // Scrolling forever must not produce a degenerate or infinite scale;
        // the tile arithmetic stops meaning anything at both ends.
        let mut view = viewport((1000.0, 800.0), 1.0, (800, 600));
        for _ in 0..200 {
            view = view.zoom_about(2.0, ScreenPoint::new(400.0, 300.0));
        }
        assert!(view.scale <= 32.0 && view.scale.is_finite(), "{}", view.scale);

        let mut view = viewport((1000.0, 800.0), 1.0, (800, 600));
        for _ in 0..200 {
            view = view.zoom_about(0.5, ScreenPoint::new(400.0, 300.0));
        }
        assert!(view.scale >= 1e-6 && view.scale.is_finite(), "{}", view.scale);
    }

    #[test]
    fn panning_moves_the_slide_with_the_drag() {
        // Drag right by 100 screen px at scale 0.5 moves the view 200 slide px
        // left, so the content follows the cursor.
        let view = viewport((1000.0, 800.0), 0.5, (800, 600));
        let panned = view.pan(100.0, 50.0);
        assert_eq!(panned.centre.x, 800.0);
        assert_eq!(panned.centre.y, 700.0);
        // And it is reversible, which a sign error in one axis would break.
        let back = panned.pan(-100.0, -50.0);
        assert!((back.centre.x - view.centre.x).abs() < 1e-9);
        assert!((back.centre.y - view.centre.y).abs() < 1e-9);
    }

    #[test]
    fn adjacent_tiles_are_placed_edge_to_edge() {
        // A gap or an overlap here is a visible seam grid on screen, and it is
        // the bug `tile_rect` is most likely to have: the placement comes from
        // the tile origin and the size from the tile extent, computed
        // separately, so they have to agree at every boundary.
        let base = level(0, 4096, 4096, 1.0);
        let view = viewport((1000.0, 1000.0), 0.5, (800, 600));
        for row in 0..3 {
            for column in 0..3 {
                let (x, y, width, height) = view.tile_rect(TileIndex::new(column, row, 0), &base);
                let (right_x, _, _, _) = view.tile_rect(TileIndex::new(column + 1, row, 0), &base);
                let (_, below_y, _, _) = view.tile_rect(TileIndex::new(column, row + 1, 0), &base);
                assert!(
                    (x + width - right_x).abs() < 1e-3,
                    "tile {column},{row} leaves a horizontal seam of {}",
                    right_x - (x + width)
                );
                assert!(
                    (y + height - below_y).abs() < 1e-3,
                    "tile {column},{row} leaves a vertical seam of {}",
                    below_y - (y + height)
                );
            }
        }
    }

    #[test]
    fn a_coarse_tile_is_drawn_larger_than_a_fine_one() {
        // A tile from a 4x-downsampled level covers 4x the slide, so at the
        // same scale it must be drawn 4x as wide. Getting this wrong is how a
        // level change makes the image jump.
        let view = viewport((2048.0, 2048.0), 0.25, (800, 600));
        let base = level(0, 4096, 4096, 1.0);
        let coarse = level(2, 1024, 1024, 4.0);
        let (_, _, fine_width, _) = view.tile_rect(TileIndex::new(1, 1, 0), &base);
        let (_, _, coarse_width, _) = view.tile_rect(TileIndex::new(1, 1, 2), &coarse);
        assert!(
            (coarse_width - fine_width * 4.0).abs() < 1e-3,
            "coarse tile {coarse_width} against fine {fine_width}"
        );
    }

    #[test]
    fn resizing_keeps_the_centre_and_shows_more() {
        let view = viewport((1000.0, 800.0), 0.5, (800, 600));
        let bigger = view.resize(1600, 900);
        assert_eq!(bigger.centre, view.centre);
        assert_eq!(bigger.scale, view.scale);
        let (left, _, right, _) = bigger.bounds();
        assert_eq!(right - left, 3200.0);
    }

    #[test]
    fn a_coarser_level_needs_fewer_tiles_for_the_same_view() {
        // The whole point of the pyramid: the same slide region costs 16x fewer
        // tiles at 4x downsample. If this ever comes out equal, level selection
        // is not reaching the tile math.
        let view = viewport((2048.0, 2048.0), 0.25, (800, 600));
        let base = level(0, 4096, 4096, 1.0);
        let coarse = level(2, 1024, 1024, 4.0);
        let fine_count = view.visible_tiles(&base).len();
        let coarse_count = view.visible_tiles(&coarse).len();
        assert!(
            coarse_count * 4 <= fine_count,
            "level 2 wanted {coarse_count} tiles against level 0's {fine_count}"
        );
    }
}
