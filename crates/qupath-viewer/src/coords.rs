//! The four coordinate spaces a viewer works in, kept apart by the type system.
//!
//! `docs/dev/VIEWER.md` §4 calls mixing these "the source of essentially every
//! viewer bug", and §5 says to write the types before the second space appears
//! rather than after. There are four:
//!
//! - [`SlidePoint`] — level 0 pixels, the canonical frame. Every other space is
//!   defined against this one, and anything stored long-term is stored here.
//! - [`LevelPoint`] — pixels within one pyramid level, which is what
//!   [`qupath_slide::Slide::read_region_into`] takes.
//! - [`TileIndex`] — a tile's column and row within a level, which is what
//!   [`qupath_slide::Slide::read_tile_into`] takes.
//! - [`ScreenPoint`] — window pixels, `f32` because that is what reaches the GPU.
//!
//! Conversions carry the [`Level`] explicitly and go through the methods here,
//! never through arithmetic at a call site. `level.downsample` is the only
//! scale factor; it is computed by `qupath-slide` from the level widths, so
//! re-deriving it from widths at a call site is both redundant and a chance to
//! get it wrong.

use qupath_slide::Level;

/// A point in level-0 pixels — the canonical frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlidePoint {
    /// Horizontal position in level-0 pixels.
    pub x: f64,
    /// Vertical position in level-0 pixels.
    pub y: f64,
}

impl SlidePoint {
    /// A point in level-0 pixels.
    #[must_use]
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Where this point falls within `level`.
    ///
    /// Dividing by `downsample` is the whole of the conversion; the point of
    /// routing it through here is that the result is typed as belonging to
    /// `level` and cannot be handed to a different one by accident.
    #[must_use]
    pub fn to_level(self, level: &Level) -> LevelPoint {
        LevelPoint {
            x: self.x / level.downsample,
            y: self.y / level.downsample,
            level: level.index,
        }
    }
}

/// A point within one pyramid level, in that level's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelPoint {
    /// Horizontal position in this level's pixels.
    pub x: f64,
    /// Vertical position in this level's pixels.
    pub y: f64,
    /// Which level, matching [`Level::index`].
    pub level: usize,
}

impl LevelPoint {
    /// Back to level-0 pixels.
    ///
    /// # Panics
    ///
    /// Panics if `level` is not the level this point was produced for. That is
    /// the mistake this module exists to catch, so it is loud rather than a
    /// silently wrong coordinate.
    #[must_use]
    pub fn to_slide(self, level: &Level) -> SlidePoint {
        assert_eq!(
            self.level, level.index,
            "LevelPoint belongs to level {} but was converted with level {}",
            self.level, level.index
        );
        SlidePoint {
            x: self.x * level.downsample,
            y: self.y * level.downsample,
        }
    }

    /// The tile containing this point.
    ///
    /// Truncating toward zero is correct only for points inside the level, so
    /// negatives clamp to tile 0 rather than wrapping through a negative cast.
    ///
    /// # Panics
    ///
    /// Panics if `level` is not the level this point was produced for.
    #[must_use]
    pub fn tile(self, level: &Level) -> TileIndex {
        assert_eq!(
            self.level, level.index,
            "LevelPoint belongs to level {} but was converted with level {}",
            self.level, level.index
        );
        let column = (self.x / f64::from(level.tile_width)).floor().max(0.0);
        let row = (self.y / f64::from(level.tile_height)).floor().max(0.0);
        TileIndex {
            // Saturating: a point past the level's right edge names a tile past
            // the last one, and `TileIndex::is_within` is what rejects it.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "clamped to [0, u32::MAX] on the lines above, and floored"
            )]
            column: column.min(f64::from(u32::MAX)) as u32,
            #[expect(
                clippy::cast_possible_truncation,
                reason = "clamped to [0, u32::MAX] on the lines above, and floored"
            )]
            row: row.min(f64::from(u32::MAX)) as u32,
            level: self.level,
        }
    }
}

/// A tile's position in a level's tile grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileIndex {
    /// Tile column, 0-based.
    pub column: u32,
    /// Tile row, 0-based.
    pub row: u32,
    /// Which level, matching [`Level::index`].
    pub level: usize,
}

impl TileIndex {
    /// A tile position.
    #[must_use]
    pub fn new(column: u32, row: u32, level: usize) -> Self {
        Self { column, row, level }
    }

    /// Whether this tile exists in `level`.
    ///
    /// [`qupath_slide::Slide::read_tile_into`] rejects out-of-range tiles, so
    /// this is what a caller uses to avoid asking in the first place.
    #[must_use]
    pub fn is_within(self, level: &Level) -> bool {
        self.level == level.index && self.column < level.columns && self.row < level.rows
    }

    /// This tile's top-left corner in the level's pixels.
    #[must_use]
    pub fn origin(self, level: &Level) -> LevelPoint {
        LevelPoint {
            x: f64::from(self.column) * f64::from(level.tile_width),
            y: f64::from(self.row) * f64::from(level.tile_height),
            level: self.level,
        }
    }

    /// This tile's size in the level's pixels, cropped at the level edge.
    ///
    /// TIFF stores edge tiles full-sized with padding and
    /// [`qupath_slide::Slide::read_tile_into`] returns them that way, so a
    /// viewer drawing a tile quad must not draw the padding. This is the
    /// cropped extent to draw; see [`crate::coords`] and `docs/dev/VIEWER.md` §1.
    #[must_use]
    pub fn visible_size(self, level: &Level) -> (u32, u32) {
        let origin_x = self.column * level.tile_width;
        let origin_y = self.row * level.tile_height;
        (
            level.tile_width.min(level.width.saturating_sub(origin_x)),
            level.tile_height.min(level.height.saturating_sub(origin_y)),
        )
    }
}

/// A point in window pixels.
///
/// `f32` because this is the space that reaches the GPU, and because window
/// coordinates never need more precision than that — unlike [`SlidePoint`],
/// where level 0 of a real slide is 46000 px across and `f32` would start
/// losing whole pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPoint {
    /// Horizontal position in window pixels, 0 at the left edge.
    pub x: f32,
    /// Vertical position in window pixels, 0 at the top edge.
    pub y: f32,
}

impl ScreenPoint {
    /// A point in window pixels.
    #[must_use]
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A level shaped like a real Aperio one, without needing a slide on disk.
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

    #[test]
    fn level_0_is_the_identity() {
        let level = level(0, 46000, 32914, 1.0);
        let point = SlidePoint::new(27776.0, 5952.0);
        let on_level = point.to_level(&level);
        assert_eq!(on_level.x, 27776.0);
        assert_eq!(on_level.y, 5952.0);
    }

    #[test]
    fn a_round_trip_through_a_level_returns_the_point() {
        // The conversion that matters: whatever the downsample, going down and
        // back must land where it started, or panning across a level change
        // would drift.
        for (index, downsample) in [(0, 1.0_f64), (1, 4.0), (2, 16.0), (3, 32.0)] {
            let width = (46000.0 / downsample).round();
            #[expect(clippy::cast_possible_truncation, reason = "small exact powers of two")]
            let level = level(index, width as u32, 32914, downsample);
            let point = SlidePoint::new(27776.0, 5952.0);
            let back = point.to_level(&level).to_slide(&level);
            assert!(
                (back.x - point.x).abs() < 1e-9 && (back.y - point.y).abs() < 1e-9,
                "downsample {downsample} lost the point: {point:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn a_downsampled_level_halves_the_coordinate() {
        let level = level(2, 11500, 8228, 4.0);
        let on_level = SlidePoint::new(4000.0, 800.0).to_level(&level);
        assert_eq!(on_level.x, 1000.0);
        assert_eq!(on_level.y, 200.0);
        assert_eq!(on_level.level, 2);
    }

    #[test]
    #[should_panic(expected = "belongs to level")]
    fn converting_with_the_wrong_level_panics() {
        // The whole point of the newtypes: this is the bug class §4 names, and
        // it must be loud rather than a silently wrong coordinate.
        let first = level(1, 11500, 8228, 4.0);
        let second = level(2, 2875, 2057, 16.0);
        let _ = SlidePoint::new(4000.0, 800.0)
            .to_level(&first)
            .to_slide(&second);
    }

    #[test]
    fn a_point_maps_to_the_tile_containing_it() {
        let level = level(0, 46000, 32914, 1.0);
        // Exactly on a tile boundary belongs to the tile starting there.
        assert_eq!(
            LevelPoint {
                x: 256.0,
                y: 512.0,
                level: 0
            }
            .tile(&level),
            TileIndex::new(1, 2, 0)
        );
        // One pixel short belongs to the previous tile.
        assert_eq!(
            LevelPoint {
                x: 255.9,
                y: 511.9,
                level: 0
            }
            .tile(&level),
            TileIndex::new(0, 1, 0)
        );
    }

    #[test]
    fn a_tile_origin_round_trips_to_its_own_index() {
        // Origin -> tile must be a fixed point, or tile addressing drifts by one
        // somewhere in the grid rather than everywhere, which is the hardest
        // version of this bug to see.
        let level = level(0, 46000, 32914, 1.0);
        for (column, row) in [(0, 0), (1, 0), (0, 1), (37, 52), (179, 128)] {
            let tile = TileIndex::new(column, row, 0);
            assert_eq!(
                tile.origin(&level).tile(&level),
                tile,
                "tile {column},{row}"
            );
        }
    }

    #[test]
    fn negative_coordinates_clamp_to_the_first_tile() {
        // Panning past the top-left corner produces negatives, and `as u32` on
        // a negative float is 0 in Rust but only because of saturating casts —
        // the clamp is explicit so it does not depend on that.
        let level = level(0, 46000, 32914, 1.0);
        let tile = LevelPoint {
            x: -900.0,
            y: -1.0,
            level: 0,
        }
        .tile(&level);
        assert_eq!(tile, TileIndex::new(0, 0, 0));
    }

    #[test]
    fn tiles_outside_the_level_are_rejected() {
        let level = level(0, 1000, 500, 1.0);
        assert_eq!(level.columns, 4);
        assert_eq!(level.rows, 2);
        assert!(TileIndex::new(3, 1, 0).is_within(&level));
        assert!(!TileIndex::new(4, 0, 0).is_within(&level));
        assert!(!TileIndex::new(0, 2, 0).is_within(&level));
        // Right index, wrong level, is not within either.
        assert!(!TileIndex::new(0, 0, 1).is_within(&level));
    }

    #[test]
    fn an_edge_tile_reports_only_its_real_pixels() {
        // 1000 px across is 3 full tiles and a 232 px remainder. Drawing the
        // full 256 would draw JPEG padding, which is `docs/dev/VIEWER.md` §1's
        // "decide once, in step 2" — decided here.
        let level = level(0, 1000, 500, 1.0);
        assert_eq!(TileIndex::new(0, 0, 0).visible_size(&level), (256, 256));
        assert_eq!(TileIndex::new(3, 0, 0).visible_size(&level), (232, 256));
        assert_eq!(TileIndex::new(0, 1, 0).visible_size(&level), (256, 244));
        assert_eq!(TileIndex::new(3, 1, 0).visible_size(&level), (232, 244));
    }
}
