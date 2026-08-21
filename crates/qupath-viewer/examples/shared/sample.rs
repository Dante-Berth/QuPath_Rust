//! The overlay the GeoJSON parity sample is written from.
//!
//! Shared, rather than built twice, because two copies is exactly how this
//! check would rot: `tests/parity_geojson_import.rs` asserts that the committed
//! sample is what the writer produces *today*, and a second copy of the
//! construction would let the writer and the recorded file drift apart while
//! both tests kept passing.
//!
//! Not an example of its own: cargo only auto-discovers `examples/*.rs`, so
//! this is reached by `#[path]`, the way `qupath-pipeline`'s probes reach their
//! shared tile.
#![allow(dead_code, reason = "the example and the test use different parts")]

use qupath_viewer::geojson::{self, Measurement};
use qupath_viewer::overlay::Overlay;

/// Where the sample is committed, relative to the repository root.
pub const PATH: &str = "parity/samples/detections.geojson";

/// The objects the writer skips, by their index in [`overlay`].
///
/// A polygon needs three corners, and [`Overlay`] admits shorter rings so that
/// its rows stay aligned with whatever produced them. These two are here so
/// that the *measurement* columns are read across a gap: rows are indexed by
/// object, so a skip that shifted them would put the wrong area on every object
/// after it, and QuPath is what reads those columns back.
pub const SKIPPED: [usize; 2] = [3, 4];

/// A closed ring's corners in level coordinates, as `push_level_ring` wants.
fn square(x: u32, y: u32, size: u32) -> Vec<[u32; 2]> {
    vec![[x, y], [x + size, y], [x + size, y + size], [x, y + size]]
}

/// An eight-sided blob, the shape a traced nucleus outline actually has.
fn blob(x: u32, y: u32) -> Vec<[u32; 2]> {
    vec![
        [x + 4, y],
        [x + 8, y + 2],
        [x + 9, y + 6],
        [x + 6, y + 9],
        [x + 2, y + 9],
        [x, y + 6],
        [x, y + 3],
        [x + 1, y + 1],
    ]
}

/// The seven objects, in the order they are written.
#[must_use]
pub fn overlay() -> Overlay {
    let mut overlay = Overlay::new();
    // 0: level 0, integer coordinates — the one configuration the application
    // is ever run at, and so the only one that was already covered.
    overlay.push_level_ring(&square(10, 20, 6), 1.0);
    // 1: level 2 of a 4x pyramid — 4096,8192 in slide coordinates. A file
    // written in *level* coordinates would put this in the corner instead.
    overlay.push_level_ring(&square(1024, 2048, 32), 4.0);
    // 2: a downsample that lands vertices on half pixels.
    overlay.push_level_ring(&square(3, 5, 7), 2.5);
    // 3: empty, and skipped by the writer.
    overlay.push_level_ring(&[], 1.0);
    // 4: two vertices, also skipped — a polygon needs three.
    overlay.push_level_ring(&[[100, 100], [104, 100]], 1.0);
    // 5: a traced outline, at a level.
    overlay.push_level_ring(&blob(600, 700), 2.0);
    // 6: the downsample a real SVS pyramid reports, which is a ratio of
    // dimensions and not a power of two. It puts a vertex at 8000.25 in the
    // file, where every round number above would not.
    overlay.push_level_ring(&blob(2000, 3000), 4.000_123);
    overlay
}

/// The nucleus areas, one row per object including the skipped two.
#[must_use]
pub fn area() -> Vec<f32> {
    vec![36.0, 16_384.0, 306.25, 0.0, 0.0, 232.0, 991.5]
}

/// The mean optical densities, one row per object.
///
/// Object 2's is `NaN`, and it is on an object the writer *keeps* on purpose.
/// The first version of this sample put it on the empty ring, so the object was
/// skipped and the case never reached the file at all — a test whose
/// interesting input is discarded before it is used. `write_number` puts `0`
/// where a bare `NaN` would not parse, and what QuPath reads back is what says
/// whether that survived.
#[must_use]
pub fn mean() -> Vec<f32> {
    vec![0.35, 0.812_345, f32::NAN, 0.5, 0.25, 1.25, 0.06]
}

/// The sample's text and how many features it holds.
#[must_use]
pub fn write() -> (String, usize) {
    let overlay = overlay();
    let (area, mean) = (area(), mean());
    geojson::write(
        &overlay,
        &[
            Measurement {
                name: qupath_pipeline::AREA,
                values: &area,
            },
            Measurement {
                name: qupath_pipeline::MEAN,
                values: &mean,
            },
        ],
    )
}
