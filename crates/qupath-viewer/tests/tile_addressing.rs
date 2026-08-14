//! The golden check `docs/VIEWER.md` §6 asks for: the tile the viewer decides
//! to draw must hold the pixels `read_region_rgb` returns for the same region.
//!
//! This is the assertion that catches an off-by-one in tile addressing, and it
//! is on **bytes**, not on appearance. `read_region_rgb` is separately tested
//! in `qupath-slide` — including a seam test across tile boundaries — so it is
//! the reference here.
//!
//! Needs a slide on disk. Every test returns early without one rather than
//! failing, which is how `qupath-slide`'s own tests handle it; `data/` is not
//! committed. See `docs/test-data.md`.

use std::path::Path;

use qupath_slide::Slide;
use qupath_viewer::{SlidePoint, TileIndex, Viewport};

/// The small committed sample, if it is present.
fn slide() -> Option<Slide> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/CMU-1-Small-Region.svs");
    path.exists().then(|| Slide::open(&path).expect("open"))
}

#[test]
fn a_tile_holds_the_pixels_the_region_reader_returns() {
    let Some(slide) = slide() else {
        return;
    };
    let level = &slide.levels()[0];

    let mut tile_pixels = Vec::new();
    for (column, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let tile = TileIndex::new(column, row, 0);
        if !tile.is_within(level) {
            continue;
        }

        // What the viewer would draw: the tile, cropped to its real pixels so
        // no JPEG padding is included.
        let (visible_width, visible_height) = tile.visible_size(level);
        slide
            .read_tile_into(0, column, row, &mut tile_pixels)
            .expect("tile");

        // What the region reader says is at that position. A tile origin is a
        // tile index times a tile size, so it is an exact non-negative integer.
        let origin = tile.origin(level);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a tile origin is an exact non-negative integer by construction"
        )]
        let (origin_x, origin_y) = (origin.x as u32, origin.y as u32);
        let region = slide
            .read_region_rgb(0, origin_x, origin_y, visible_width, visible_height)
            .expect("region");

        // Row by row, because the tile is stored full-width with padding and
        // the region is packed to `visible_width`.
        for line in 0..visible_height as usize {
            let from_tile = line * level.tile_width as usize * 3;
            let from_region = line * visible_width as usize * 3;
            let span = visible_width as usize * 3;
            assert_eq!(
                &tile_pixels[from_tile..from_tile + span],
                &region[from_region..from_region + span],
                "tile {column},{row} row {line} disagrees with the region reader"
            );
        }
    }
}

#[test]
fn the_tiles_a_viewport_reports_are_the_ones_it_can_read() {
    // `visible_tiles` promises every tile it returns is readable. That is what
    // lets the render loop skip an `is_within` check, so it is worth asserting
    // against a real level table rather than only the synthetic ones in the
    // unit tests.
    let Some(slide) = slide() else {
        return;
    };

    let mut scratch = Vec::new();
    for scale in [1.0, 0.5, 0.1] {
        let level = Viewport {
            centre: SlidePoint::new(0.0, 0.0),
            scale,
            width: 512,
            height: 512,
        }
        .level(&slide);

        let view = Viewport {
            centre: SlidePoint::new(
                f64::from(level.width) * level.downsample / 2.0,
                f64::from(level.height) * level.downsample / 2.0,
            ),
            scale,
            width: 512,
            height: 512,
        };

        for tile in view.visible_tiles(level) {
            slide
                .read_tile_into(tile.level, tile.column, tile.row, &mut scratch)
                .unwrap_or_else(|error| {
                    panic!("scale {scale} reported unreadable {tile:?}: {error}")
                });
        }
    }
}

#[test]
fn zooming_out_never_picks_a_level_below_screen_resolution() {
    // The level choice must not be soft: the chosen level has to be at least as
    // wide on screen as the window shows, or the image is visibly blurry. This
    // is the property `level_for_width` is being trusted for.
    let Some(slide) = slide() else {
        return;
    };
    let base_width = f64::from(slide.levels()[0].width);

    for scale in [1.0, 0.75, 0.5, 0.25, 0.1, 0.05, 0.01] {
        let view = Viewport {
            centre: SlidePoint::new(0.0, 0.0),
            scale,
            width: 800,
            height: 600,
        };
        let level = view.level(&slide);
        let wanted = base_width * scale;

        // Either the level covers what the screen needs, or it is level 0 and
        // there is nothing finer to pick.
        assert!(
            f64::from(level.width) >= wanted || level.index == 0,
            "scale {scale} wanted {wanted:.0} px and got level {} at {} px",
            level.index,
            level.width
        );
    }
}
