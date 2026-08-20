//! Milestone 3.4 against a real slide: what the pool delivers, and what the
//! cache costs over a pan.
//!
//! The pool and the cache carry their own unit tests over a fake source, which
//! is where cancellation and eviction order are pinned. What those cannot say
//! is whether the bytes are right and whether the budget holds against a real
//! level table — a fake tile is one pixel and a real one is 256x256 with
//! padding at the edges. Both assertions here are on **counts and bytes**, per
//! `ROADMAP.md` §5.
//!
//! Needs a slide on disk. Every test returns early without one rather than
//! failing, which is how `qupath-slide`'s own tests handle it; `data/` is not
//! committed. See `docs/test-data.md`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qupath_slide::Slide;
use qupath_viewer::loader::expand_to_rgba_into;
use qupath_viewer::{
    ScreenPoint, SlidePoint, SlideTiles, TileCache, TileIndex, TileLoader, Viewport,
};

/// The small committed sample, if it is present.
fn slide() -> Option<Slide> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/CMU-1-Small-Region.svs");
    path.exists().then(|| Slide::open(&path).expect("open"))
}

/// Waits for the *workers* to finish, failing rather than hanging.
///
/// Not `is_idle`, which also waits for the caller to take delivery: every
/// caller here drains after settling, so waiting for an empty channel first
/// would be waiting for itself.
fn settle(loader: &TileLoader) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while loader.pending() > 0 || loader.in_flight() > 0 {
        assert!(Instant::now() < deadline, "the loader never went idle");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn the_loader_delivers_the_pixels_a_direct_read_returns() {
    // The golden check for the loading path, and the counterpart to
    // `a_tile_holds_the_pixels_the_region_reader_returns` in
    // `tile_addressing.rs`: that one pins tile *addressing* against the region
    // reader, this one pins that going through a worker thread, a channel and
    // the RGBA expansion changes no byte.
    let (Some(loaded_from), Some(reference)) = (slide(), slide()) else {
        return;
    };
    let level = reference.levels()[0].clone();

    let source = Arc::new(SlideTiles::new(loaded_from));
    let loader = TileLoader::new(&source, 2);

    let wanted: Vec<TileIndex> = (0..level.rows.min(2))
        .flat_map(|row| (0..level.columns.min(2)).map(move |column| TileIndex::new(column, row, 0)))
        .collect();
    loader.request(wanted.clone());
    settle(&loader);

    let delivered = loader.drain();
    assert_eq!(delivered.len(), wanted.len(), "not every tile came back");

    let mut decoded = Vec::new();
    let mut expected = Vec::new();
    for loaded in delivered {
        let (width, height) = loaded.tile.visible_size(&level);
        assert_eq!((loaded.width, loaded.height), (width, height));
        assert_eq!(loaded.pixels.len(), (width * height * 4) as usize);

        reference
            .read_tile_into(0, loaded.tile.column, loaded.tile.row, &mut decoded)
            .expect("tile");
        expand_to_rgba_into(&decoded, level.tile_width, width, height, &mut expected);
        assert_eq!(
            loaded.pixels, expected,
            "{:?} came back from the pool changed",
            loaded.tile
        );
    }
}

#[test]
fn a_scripted_pan_stays_inside_the_budget() {
    // `docs/VIEWER.md` §6's cache test, written as it is stated there: pan a
    // scripted path and assert resident bytes never exceed the budget. The
    // budget here is deliberately smaller than the path's whole footprint, so
    // eviction actually runs rather than the test passing by never filling up.
    let Some(slide) = slide() else {
        return;
    };

    let mut view = Viewport {
        centre: SlidePoint::new(0.0, 0.0),
        scale: 1.0,
        width: 640,
        height: 480,
    };

    // A 640x480 window at scale 1 covers at most 4x3 tiles of 256x256, so a
    // 16-tile budget holds a frame and a little history: big enough that the
    // visible set never evicts itself, small enough that the pan overruns it.
    const TILE: usize = 256 * 256 * 4;
    let budget = 16 * TILE;
    let mut cache: TileCache<()> = TileCache::new(budget);
    let mut distinct = 0_u32;

    for step in 0..120 {
        view = view.pan(-12.0, if step % 20 == 19 { -160.0 } else { 0.0 });
        let level = view.level(&slide);
        for tile in view.visible_tiles(level) {
            if !cache.touch(tile) {
                distinct += 1;
                let (width, height) = tile.visible_size(level);
                assert!(cache.insert(tile, (), (width * height * 4) as usize));
            }
        }
        assert!(
            cache.bytes() <= cache.budget(),
            "step {step}: {} bytes resident against a {budget} budget",
            cache.bytes()
        );
    }

    // The path has to have been long enough to force eviction, or the
    // assertion above proved nothing.
    assert!(
        cache.evicted() > 0,
        "the pan never filled the cache: {distinct} distinct tiles over a {} tile budget",
        budget / TILE
    );
}

#[test]
fn a_stationary_view_decodes_each_tile_once() {
    // The other half of the budget: with room to spare, redrawing the same view
    // must cost nothing. A cache that re-decodes what it already holds is worse
    // than no cache, because it pays the bookkeeping too.
    let Some(slide) = slide() else {
        return;
    };

    let view = Viewport {
        centre: SlidePoint::new(512.0, 512.0),
        scale: 1.0,
        width: 512,
        height: 512,
    };
    let level = view.level(&slide).clone();
    let tiles = view.visible_tiles(&level);
    assert!(!tiles.is_empty());

    let source = Arc::new(SlideTiles::new(slide));
    let loader = TileLoader::new(&source, 4);
    let mut cache: TileCache<()> = TileCache::new(64 << 20);

    for _ in 0..30 {
        for loaded in loader.drain() {
            let bytes = loaded.pixels.len();
            assert!(cache.insert(loaded.tile, (), bytes));
        }
        let missing: Vec<TileIndex> = tiles
            .iter()
            .copied()
            .filter(|tile| !cache.contains(*tile))
            .collect();
        loader.request(missing);
        std::thread::sleep(Duration::from_millis(5));
    }
    settle(&loader);
    for loaded in loader.drain() {
        let bytes = loaded.pixels.len();
        assert!(cache.insert(loaded.tile, (), bytes));
    }

    assert_eq!(cache.len(), tiles.len(), "not every tile became resident");
    assert_eq!(
        loader.decoded(),
        tiles.len() as u64,
        "a resident tile was decoded again"
    );
    assert_eq!(cache.evicted(), 0);
}

#[test]
fn the_underlay_is_coarser_and_cheaper_than_the_level_it_backs() {
    // The underlay is drawn every frame a fine tile is missing, so it is only
    // affordable if it is much cheaper than the thing it stands in for. If it
    // ever cost more, keeping the old level while loading would be slower than
    // flashing the clear colour.
    let Some(slide) = slide() else {
        return;
    };
    if slide.levels().len() < 2 {
        return;
    }
    let base = &slide.levels()[0];
    let centre = SlidePoint::new(f64::from(base.width) / 2.0, f64::from(base.height) / 2.0);

    let mut saw_one = false;
    for scale in [4.0, 2.0, 1.0, 0.5, 0.25, 0.1, 0.02] {
        let view = Viewport {
            centre,
            scale,
            width: 1024,
            height: 768,
        };
        let chosen = view.level(&slide);
        let Some(underlay) = view.underlay_level(&slide) else {
            // Only legitimate when the view is already at the coarsest level.
            assert_eq!(chosen.index, slide.levels().len() - 1, "scale {scale}");
            continue;
        };
        saw_one = true;

        assert!(
            underlay.downsample > chosen.downsample,
            "scale {scale}: the underlay is not coarser than level {}",
            chosen.index
        );
        assert!(
            view.visible_tiles(underlay).len() <= view.visible_tiles(chosen).len(),
            "scale {scale}: the underlay costs more tiles than level {}",
            chosen.index
        );
    }
    assert!(saw_one, "no scale in the sweep needed an underlay");
}

#[test]
fn the_underlay_does_not_change_as_the_view_moves_within_a_level() {
    // The reason the *coarsest* level is the fallback rather than the next one
    // down: it is the same level at every zoom, so it stays resident across a
    // pan that evicts everything else. A fallback that changed with the view
    // would itself need loading exactly when the view is already behind.
    let Some(slide) = slide() else {
        return;
    };
    if slide.levels().len() < 2 {
        return;
    }
    let coarsest = slide.levels().last().expect("a level");

    let mut view = Viewport::fit(&slide, 1024, 768);
    let anchor = ScreenPoint::new(512.0, 384.0);
    for _ in 0..16 {
        view = view.zoom_about(1.4, anchor).pan(-30.0, 17.0);
        if let Some(underlay) = view.underlay_level(&slide) {
            assert_eq!(underlay.index, coarsest.index);
        }
    }
}
