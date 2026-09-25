//! `run_collecting` against `run`, on a real slide.
//!
//! `docs/dev/VIEWER.md` §3.5 needs object geometry out of a run, and the way that
//! goes wrong is not a crash — it is a set of objects that is *nearly* right.
//! One dropped seam duplicate, one detection kept whose candidate died, one
//! outline traced from the wrong region index, and the viewer draws something
//! that looks entirely plausible. `ROADMAP.md` §5 is about exactly this, and
//! these are the assertions that make the failure visible.
//!
//! The first two need no fixture from QuPath, because they are internal
//! consistency claims rather than parity ones:
//!
//! - collecting must not change **any** count the run reports, which is the
//!   claim that the collection is a read of what the pipeline already did and
//!   not a second, differently-parameterised pass;
//! - the number of detections must equal [`Outcome::objects`], which is the
//!   count *after* seam duplicates are removed — so an object whose candidate
//!   lost must be gone from the geometry too, not merely uncounted.
//!
//! The third is the identity `contour` is built on, carried end to end: every
//! outline the pipeline produces must enclose exactly the area the pipeline
//! measured for that object. That ties three separate things together — the
//! trace, the region map it read, and the filter's own pixel count — and it is
//! checked on real nuclei rather than on ASCII shapes.

#![expect(
    clippy::cast_possible_truncation,
    reason = "a pixel count round-tripped through the f32 measurement column"
)]

use std::path::Path;

use qupath_pipeline::{Detections, Params, Region, run, run_collecting, shoelace_area_x2};
use qupath_slide::Slide;

/// The fixture, which is not in the repository. Returns `None` so the suite
/// stays green on a checkout without it — and says so, failing instead, under
/// `QUPATH_RUST_REQUIRE_DATA`, as the other integration tests do.
fn slide() -> Option<Slide> {
    let path = Path::new("../../data/CMU-1-Small-Region.svs");
    if !path.exists() {
        qupath_testing::skipped("data/CMU-1-Small-Region.svs not present");
        return None;
    }
    Some(Slide::open(path).expect("open fixture"))
}

/// A region small enough to run quickly and large enough to be tiled, so the
/// seam resolution the second assertion is about actually has something to do.
fn params() -> (Region, Params) {
    let region = Region {
        x: 0,
        y: 0,
        width: 1024,
        height: 1024,
    };
    let params = Params {
        tile_preferred: 512,
        tile_max: 640,
        ..Params::default()
    };
    (region, params)
}

#[test]
fn the_two_entry_points_agree_on_every_count() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();

    let plain = run(&slide, region, params).expect("run");
    let (collected, detections) = run_collecting(&slide, region, params).expect("run_collecting");

    // Every count, field by field, rather than a spot check on `objects`. The
    // failure this is guarding against is a collection path that quietly runs a
    // stage differently, and that would show first in whichever count nobody
    // thought to compare.
    assert_eq!(collected.tiles, plain.tiles);
    assert_eq!(collected.pixels, plain.pixels);
    assert_eq!(collected.markers, plain.markers);
    assert_eq!(collected.labelled, plain.labelled);
    assert_eq!(collected.regions, plain.regions);
    assert_eq!(collected.candidates, plain.candidates);
    assert_eq!(collected.merged, plain.merged);
    assert_eq!(collected.nuclei, plain.nuclei);
    assert_eq!(collected.objects, plain.objects);
    assert_eq!(collected.duplicates, plain.duplicates);

    assert_eq!(
        detections.len() as u64,
        collected.objects,
        "the geometry and the count disagree about how many objects there are"
    );
    assert_eq!(detections.level(), params.level);
}

#[test]
fn the_seam_duplicates_are_removed_from_the_geometry_too() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();
    let (outcome, detections) = run_collecting(&slide, region, params).expect("run_collecting");

    // The assertion above would pass trivially if this region had no seam
    // duplicates at all, so the premise is asserted rather than assumed — the
    // same shape as `a_scripted_pan_stays_inside_the_budget` asserting that its
    // budget is actually exceeded.
    assert!(
        outcome.duplicates > 0,
        "this region resolved no duplicates, so the removal is untested; \
         make the region larger or the tiles smaller"
    );
    assert_eq!(detections.len() as u64, outcome.objects);

    // And what is left holds no two objects with the same bounding box, which
    // is the observable form of a duplicate that survived: the same nucleus
    // found by two tiles lands in the same place.
    let mut boxes: Vec<_> = detections
        .bounds()
        .iter()
        .map(|b| (b.x, b.y, b.width, b.height))
        .collect();
    boxes.sort_unstable();
    let before = boxes.len();
    boxes.dedup();
    assert_eq!(before, boxes.len(), "two detections share a bounding box");
}

#[test]
fn every_outline_encloses_the_area_the_filter_measured() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();
    let (_, detections) = run_collecting(&slide, region, params).expect("run_collecting");
    assert!(!detections.is_empty(), "the fixture produced no objects");

    let areas = detections.measurements().column(detections.area());
    for (object, &area) in areas.iter().enumerate() {
        let ring = detections.outline(object);
        assert!(
            ring.len() >= 4,
            "object {object} has a {}-vertex outline, which cannot enclose anything",
            ring.len()
        );
        // Exact, not approximate: the outline follows pixel corners, so twice
        // the shoelace area *is* twice the pixel count. A half-pixel anywhere in
        // the trace breaks this by the object's perimeter, not by a rounding
        // step, so it fails loudly.
        // The area column is `f32` per §4 and holds a pixel count, so the
        // comparison is exact below 2^24 and this is a whole nucleus away from
        // that. Rounded rather than truncated so the assertion fails on a real
        // disagreement instead of on the last bit of a float.
        assert_eq!(
            shoelace_area_x2(ring),
            (f64::from(area).round() as i64) * 2,
            "object {object} outlines {} pixels but was measured as {area}",
            shoelace_area_x2(ring) / 2,
        );
        // Clockwise in image coordinates, which is what `contour` documents and
        // what a caller filling the ring depends on.
        assert!(
            shoelace_area_x2(ring) > 0,
            "object {object} runs anticlockwise"
        );
    }
}

#[test]
fn every_outline_stays_inside_its_own_bounding_box() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();
    let (_, detections) = run_collecting(&slide, region, params).expect("run_collecting");

    // The tile origin is added to both the bounds and the vertices, in two
    // different places. A tile offset applied to one and not the other puts
    // every object of every tile but the first in the wrong place, and the count
    // assertions above cannot see it.
    for (object, bounds) in detections.bounds().iter().enumerate() {
        for vertex in detections.outline(object) {
            assert!(
                (bounds.x..=bounds.x + bounds.width).contains(&vertex[0])
                    && (bounds.y..=bounds.y + bounds.height).contains(&vertex[1]),
                "object {object} has vertex {vertex:?} outside bounds {bounds:?}"
            );
        }
    }
}

#[test]
fn the_footprint_is_the_one_the_documentation_claims() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();
    let (_, detections) = run_collecting(&slide, region, params).expect("run_collecting");

    // `detections`' module documentation quotes 322 bytes per object and 36.7
    // vertices per outline, and both came from this test rather than from an
    // estimate. They are what a caller sizes a region against, so the bounds
    // here are wide enough to survive a different fixture and tight enough that
    // a representation change — a `Vec` per outline, an `f64` column — trips
    // them.
    let vertices_per_object = detections.vertex_count() as f64 / detections.len() as f64;
    assert!(
        (5.0..60.0).contains(&vertices_per_object),
        "an outline averages {vertices_per_object:.1} vertices, which is not the \
         order the documentation claims"
    );
    let bytes = bytes(&detections);
    let per_object = bytes as f64 / detections.len() as f64;
    assert!(
        per_object < 400.0,
        "an object costs {per_object:.0} bytes against the 322 measured"
    );
    println!(
        "{} objects, {:.1} vertices each, {per_object:.0} bytes each",
        detections.len(),
        vertices_per_object
    );
}

/// What the arenas hold, which is everything that grows with the object count.
fn bytes(detections: &Detections) -> usize {
    let rows = detections.len();
    let geometry = rows * size_of::<Region>()
        + rows * size_of::<u32>()
        + detections.vertex_count() * size_of::<[u32; 2]>();
    let measurements = rows * detections.measurements().n_features() * size_of::<f32>();
    geometry + measurements
}
