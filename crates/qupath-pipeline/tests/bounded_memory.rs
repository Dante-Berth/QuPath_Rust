//! §6: "There is no code path that materializes a full-resolution slide into a
//! `Vec<u8>` ... Add a test that asserts peak RSS stays under the ceiling on a
//! large fixture."
//!
//! The property that matters is not an absolute number — that depends on the
//! allocator and the thread count — but that peak memory is set by **the tile
//! size and the thread count**, and does not grow with the region.
//!
//! # Why this test was rewritten
//!
//! It used to compare a 1000x1000 region against a 5000x5000 one. Once the
//! pipeline started tiling the way QuPath does, that stopped measuring what it
//! claimed: 1000x1000 is below `tile_max` and so runs as a **single untiled
//! pass**, while 5000x5000 is a 3x3 grid of much larger tiles. The peak rose
//! 939 MB between them and essentially none of that was the region size — it was
//! the tile going from 1 Mpx to 3 Mpx and from one worker to nine.
//!
//! So both runs now use the **same tile geometry** and differ only in how many
//! tiles that geometry produces, which is the claim being made. The tile size is
//! set explicitly rather than left at the default, so the test states the
//! footprint it is asserting instead of inheriting one.

use std::path::Path;

use qupath_pipeline::{Params, Region, peak_resident_bytes, run};
use qupath_slide::Slide;

/// Tile size for both runs. Smaller than the default 2052 so the test stays
/// quick and its ceiling stays modest; the property under test does not depend
/// on which size is chosen, only on both runs sharing it.
const TILE: u32 = 512;

/// Bytes of scratch each in-flight tile needs per pixel of its padded rectangle.
///
/// The per-worker buffers in `Scratch`: three for the RGB tile, eight `f32`
/// images (density, detection, background, to_measure, log, prominence, labels,
/// and the decode staging), two `u32` region maps (the tracer's and the
/// merger's), one `u8` binary, and the shape split's own scratch, which is the
/// largest single item at about 17 bytes a pixel. Rounded up, and the assertion
/// below carries a further factor for allocator slack and the region maps'
/// growth, so this is a design statement rather than a fitted constant.
const BYTES_PER_TILE_PIXEL: u64 = 66;

/// How much the high-water mark may rise when the region grows four-fold at a
/// fixed tile size and a fixed worker count.
///
/// Not zero, and the reason is worth stating: the candidates that reach a tile
/// seam are held with their pixel masks until every tile is done, because
/// resolving a duplicate needs both copies. That set grows with the region's
/// total seam length, so it is the one thing here that legitimately does scale —
/// see `qupath_pipeline::resolve_overlaps`. It is a few tens of megabytes at this tile
/// size; anything approaching the allowance below is a buffer that should have
/// been per-tile.
const ALLOWED_GROWTH_BYTES: u64 = 192 * 1_024 * 1_024;

fn slide() -> Option<Slide> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/CMU-1.svs");
    path.exists()
        .then(|| Slide::open(&path).expect("open slide"))
}

/// The parameters both runs share: an explicit tile size, so that the only
/// difference between them is how many tiles the region contains.
fn params() -> Params {
    Params {
        tile_preferred: TILE,
        tile_max: TILE,
        ..Params::default()
    }
}

#[test]
fn peak_memory_does_not_grow_with_the_region() {
    let Some(slide) = slide() else {
        qupath_testing::skipped("data/CMU-1.svs not present");
        return;
    };

    let origin = (27776, 5952);
    let region = |side| Region {
        x: origin.0,
        y: origin.1,
        width: side,
        height: side,
    };

    // Both sides must contain at least as many tiles as there are workers, or
    // the comparison measures the thread count coming up to strength rather than
    // the region growing.
    let small = run(&slide, region(4000), params()).expect("small region");
    let after_small = peak_resident_bytes().expect("VmHWM");

    let large = run(&slide, region(8000), params()).expect("large region");
    let after_large = peak_resident_bytes().expect("VmHWM");

    // The runs have to have done something, or the memory claim is vacuous.
    assert!(
        large.markers > small.markers && small.markers > 0,
        "no work done: {} markers small, {} large",
        small.markers,
        large.markers
    );
    assert!(
        large.tiles >= small.tiles * 3,
        "the large region should contain far more tiles: {} vs {}",
        large.tiles,
        small.tiles
    );
    // The point of the rewrite: same tile geometry on both sides, so a peak
    // that moves is the region and not the tile.
    assert!(
        small.tiles >= rayon::current_num_threads(),
        "both runs must saturate the pool ({} tiles, {} workers) or the peak \
         moves because more workers ran, not because the region grew",
        small.tiles,
        rayon::current_num_threads()
    );

    let growth = after_large.saturating_sub(after_small);
    assert!(
        growth <= ALLOWED_GROWTH_BYTES,
        "peak memory grew {:.0} MB when the region grew {:.1}x at a fixed tile \
         size; that is a buffer scaling with the region, not with the tile",
        growth as f64 / 1e6,
        large.pixels as f64 / small.pixels as f64
    );

    // The ceiling the design actually claims: one padded tile's worth of scratch
    // per worker that touches a tile, and nothing else that scales.
    let padded = u64::from(TILE + 2 * params().tile_overlap);
    let workers = rayon::current_num_threads() as u64;
    let ceiling = padded * padded * BYTES_PER_TILE_PIXEL * workers.min(large.tiles as u64)
        // Slack for the allocator, the process image and the slide's own state.
        + 512 * 1_024 * 1_024;
    assert!(
        after_large <= ceiling,
        "peak resident memory {:.0} MB exceeds the {:.0} MB the tile size and \
         {workers} workers account for",
        after_large as f64 / 1e6,
        ceiling as f64 / 1e6
    );

    println!(
        "tile {TILE}+{}px halo, {workers} workers: \
         {} tiles -> {:.0} MB;  {} tiles -> {:.0} MB;  growth {:.0} MB, ceiling {:.0} MB",
        params().tile_overlap,
        small.tiles,
        after_small as f64 / 1e6,
        large.tiles,
        after_large as f64 / 1e6,
        growth as f64 / 1e6,
        ceiling as f64 / 1e6,
    );
}
