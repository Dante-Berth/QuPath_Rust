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

use crate::coords::{SlidePoint, TileIndex};

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
    /// Asks for a level at least as wide as the view needs on screen, so the
    /// chosen level is never *below* screen resolution — that would be visibly
    /// soft. [`Slide::level_for_width`] returns the coarsest level meeting
    /// that, which is the cheapest one that still looks right.
    #[must_use]
    pub fn level(self, slide: &Slide) -> &Level {
        let base = &slide.levels()[0];
        // How wide level 0 would be on screen at this scale. Clamped to at
        // least 1 so a degenerate scale still names a level rather than
        // underflowing the cast.
        let on_screen = (f64::from(base.width) * self.scale).max(1.0);
        // Saturating rather than wrapping: a very zoomed-in view asks for more
        // than u32 can hold, and the answer there is level 0, which is what
        // `level_for_width` returns for any width past the widest level.
        let needed = if on_screen >= f64::from(u32::MAX) {
            u32::MAX
        } else {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "the branch above excludes everything u32 cannot hold"
            )]
            {
                on_screen as u32
            }
        };
        slide.level_for_width(needed)
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
