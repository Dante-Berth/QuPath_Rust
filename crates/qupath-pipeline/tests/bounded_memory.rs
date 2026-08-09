//! §6: "There is no code path that materializes a full-resolution slide into a
//! `Vec<u8>` ... Add a test that asserts peak RSS stays under the ceiling on a
//! large fixture."
//!
//! The property that matters is not an absolute number — that depends on the
//! allocator and the thread count — but that peak memory is set by the thread
//! count and the tile size, and **does not grow with the region**. So this runs
//! a small region, records the high-water mark, then runs a region 25 times
//! larger in the same process and asserts the mark barely moves.
//!
//! Skipped where the slide is absent; slides are untracked because they are
//! hundreds of megabytes. See `docs/test-data.md`.

use std::path::Path;

use qupath_pipeline::{Params, Region, peak_resident_bytes, run};
use qupath_slide::Slide;

/// Absolute ceiling for the whole process. The 5000x5000 region below would be
/// 75 MB as RGB and 100 MB as `f32`, and several stages would each need their
/// own copy; a pipeline that buffered the region rather than streaming it would
/// exceed this even before the watershed.
const CEILING_BYTES: u64 = 1_024 * 1_024 * 1_024;

/// How much the high-water mark may rise when the region grows 25-fold.
const ALLOWED_GROWTH_BYTES: u64 = 128 * 1_024 * 1_024;

fn slide() -> Option<Slide> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/CMU-1.svs");
    path.exists()
        .then(|| Slide::open(&path).expect("open slide"))
}

#[test]
fn peak_memory_does_not_grow_with_the_region() {
    let Some(slide) = slide() else {
        eprintln!("skipping: data/CMU-1.svs not present");
        return;
    };

    let params = Params::default();
    let origin = (27776, 5952);

    let small = run(
        &slide,
        Region {
            x: origin.0,
            y: origin.1,
            width: 1000,
            height: 1000,
        },
        params,
    )
    .expect("small region");
    let after_small = peak_resident_bytes().expect("VmHWM");

    let large = run(
        &slide,
        Region {
            x: origin.0,
            y: origin.1,
            width: 5000,
            height: 5000,
        },
        params,
    )
    .expect("large region");
    let after_large = peak_resident_bytes().expect("VmHWM");

    // The run has to have done something, or the memory claim is vacuous.
    assert!(
        large.markers > small.markers && small.markers > 0,
        "no work done: {} markers small, {} large",
        small.markers,
        large.markers
    );
    // 25x by area as requested, a little less once both are rounded out to
    // whole tiles.
    assert!(
        large.pixels >= small.pixels * 15,
        "the large region should be far bigger: {} vs {} pixels",
        large.pixels,
        small.pixels
    );

    let growth = after_large.saturating_sub(after_small);
    assert!(
        growth <= ALLOWED_GROWTH_BYTES,
        "peak memory grew {:.0} MB when the region grew {:.0}x; \
         that is a buffer scaling with the region, not with the tile",
        growth as f64 / 1e6,
        large.pixels as f64 / small.pixels as f64
    );
    assert!(
        after_large <= CEILING_BYTES,
        "peak resident memory {:.0} MB exceeds the {:.0} MB ceiling",
        after_large as f64 / 1e6,
        CEILING_BYTES as f64 / 1e6
    );

    println!(
        "small {:.1} Mpx -> {:.0} MB;  large {:.1} Mpx -> {:.0} MB;  growth {:.0} MB",
        small.pixels as f64 / 1e6,
        after_small as f64 / 1e6,
        large.pixels as f64 / 1e6,
        after_large as f64 / 1e6,
        growth as f64 / 1e6
    );
}
