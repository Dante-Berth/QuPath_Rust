//! Checks a GeoJSON file *this project wrote* against QuPath's own reader.
//!
//! `docs/VIEWER.md` §8.8 listed this as the honest gap: both file formats had
//! been checked against a JSON parser and against the Java that writes them,
//! which is a stronger claim than "it validates" and a weaker one than "QuPath
//! opened it". This is the second claim, made once by a JVM and then asserted
//! without one.
//!
//! Three files are involved and each has one job:
//!
//! - `examples/shared/sample.rs` builds the overlay, and
//!   `examples/geojson_sample.rs` writes it to
//!   `parity/samples/detections.geojson`;
//! - `parity/java/GeoJsonImport.java` opens that file through
//!   `PathIO.readObjects` — the same entry point as *Open objects* in the GUI —
//!   and records what came back, into `parity/fixtures/GeoJsonImport.json`;
//! - this test asserts the recorded reading is the geometry the sample was
//!   built from.
//!
//! **The first assertion is what stops the chain rotting.** The committed
//! sample has to be exactly what `geojson::write` produces today; without that,
//! a change to the writer would leave the fixture describing QuPath's reading
//! of a file nothing produces any more, and every assertion below would keep
//! passing about the wrong thing.
//!
//! # What QuPath does that a Rust-side guess got wrong
//!
//! Two things, both found by running it:
//!
//! - a ring of eight vertices comes back with **nine** points. GeoJSON requires
//!   a linear ring to repeat its first position and QuPath keeps the repeat;
//! - an axis-aligned four-corner ring does not come back a polygon at all. The
//!   reader recognises it and builds a **Rectangle** ROI.
//!
//! Neither moves a coordinate — the areas below agree exactly — and both would
//! have failed an assertion written from the specification rather than from the
//! run.

use std::fmt::Write as _;

#[path = "../examples/shared/sample.rs"]
mod sample;

/// The sample as committed, and QuPath's reading of it.
const SAMPLE: &str = include_str!("../../../parity/samples/detections.geojson");
const FIXTURE: &str = include_str!("../../../parity/fixtures/GeoJsonImport.json");

/// `parity/README.md`'s tolerance for a scalar measurement.
///
/// Not tightened to what is observed. QuPath's ROIs hold their coordinates as
/// `f32`, so a centroid comes back rounded to about six significant figures
/// however exactly the geometry agrees, and a bound at the observed 5e-8 would
/// be measuring that rounding rather than the export. The observed margins are
/// printed instead, which is what makes one eroding towards the limit visible
/// before it fails.
const RELATIVE: f64 = 1e-5;

/// One `{ ... }` block per object QuPath read, in file order.
fn objects(fixture: &str) -> Vec<&str> {
    fixture
        .split("\n    {\n")
        .skip(1)
        .map(|block| block.split("\n    }").next().expect("an object block"))
        .collect()
}

/// The text of one field, up to the comma or newline that ends it.
fn text<'a>(block: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\": ");
    let at = block
        .find(&needle)
        .unwrap_or_else(|| panic!("no {key} in {block}"))
        + needle.len();
    let rest = &block[at..];
    let end = rest.find([',', '\n']).unwrap_or(rest.len());
    rest[..end].trim().trim_matches('"')
}

/// One numeric field.
fn number(block: &str, key: &str) -> f64 {
    text(block, key)
        .parse()
        .unwrap_or_else(|_| panic!("{key} is not a number in {block}"))
}

/// The measurement names and values, in the order QuPath lists them.
fn measurements(block: &str) -> Vec<(&str, f64)> {
    let open = block.find("\"measurements\": [").expect("measurements") + "\"measurements\": [".len();
    let list = &block[open..block[open..].find(']').expect("measurements end") + open];
    list.split("{\"name\": ")
        .skip(1)
        .map(|entry| {
            let name_end = entry[1..].find('"').expect("a measurement name") + 1;
            let value = entry
                .split("\"value\": ")
                .nth(1)
                .expect("a measurement value")
                .trim_end_matches(['}', ',', ' ']);
            (
                &entry[1..name_end],
                value.trim_matches('"').parse().unwrap_or(f64::NAN),
            )
        })
        .collect()
}

/// A vertex as the *file* holds it: two decimal places.
///
/// `geojson::write` rounds there, so this is what QuPath parsed and what the
/// geometry below has to be derived from. Computing from the unrounded overlay
/// instead would disagree by 6e-5 of an area on the object written at an SVS
/// downsample, and that disagreement would be this test's arithmetic rather
/// than the export's.
fn written_ring(ring: &[[f32; 2]]) -> Vec<[f64; 2]> {
    ring.iter()
        .map(|vertex| {
            [
                (f64::from(vertex[0]) * 100.0).round() / 100.0,
                (f64::from(vertex[1]) * 100.0).round() / 100.0,
            ]
        })
        .collect()
}

/// Shoelace area, and the area centroid JTS reports for a polygon.
fn area_and_centroid(ring: &[[f64; 2]]) -> (f64, f64, f64) {
    let (mut twice_area, mut x, mut y) = (0.0, 0.0, 0.0);
    for (at, vertex) in ring.iter().enumerate() {
        let next = ring[(at + 1) % ring.len()];
        let cross = vertex[0] * next[1] - next[0] * vertex[1];
        twice_area += cross;
        x += (vertex[0] + next[0]) * cross;
        y += (vertex[1] + next[1]) * cross;
    }
    let area = twice_area / 2.0;
    (area.abs(), x / (6.0 * area), y / (6.0 * area))
}

/// `[left, top, width, height]` of a ring.
fn bounds(ring: &[[f64; 2]]) -> [f64; 4] {
    let (mut left, mut top) = (f64::INFINITY, f64::INFINITY);
    let (mut right, mut bottom) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for vertex in ring {
        left = left.min(vertex[0]);
        top = top.min(vertex[1]);
        right = right.max(vertex[0]);
        bottom = bottom.max(vertex[1]);
    }
    [left, top, right - left, bottom - top]
}

/// What QuPath's reader makes of a ring: a Rectangle if it can, a Polygon
/// otherwise, with the point count each carries.
///
/// Recorded behaviour, not the specification — see this file's header.
fn expected_roi(ring: &[[f64; 2]]) -> (&'static str, usize) {
    let axis_aligned = ring.len() == 4
        && ring.iter().enumerate().all(|(at, vertex)| {
            let next = ring[(at + 1) % ring.len()];
            vertex[0] == next[0] || vertex[1] == next[1]
        });
    if axis_aligned {
        ("Rectangle", 4)
    } else {
        // The closing position GeoJSON requires, which QuPath keeps.
        ("Polygon", ring.len() + 1)
    }
}

/// Fails with the relative difference when two scalars disagree, and returns it.
#[track_caller]
fn agree(what: &str, ours: f64, qupath: f64) -> f64 {
    let relative = (ours - qupath).abs() / ours.abs().max(1.0);
    assert!(
        relative <= RELATIVE,
        "{what}: ours {ours}, QuPath {qupath}, relative {relative:.3e} over {RELATIVE:.0e}"
    );
    relative
}

/// The committed sample is what the writer produces.
///
/// Everything else here describes a file; this is what ties that file to the
/// code under test. If it fails after a deliberate change to `geojson::write`,
/// regenerate and re-record — in that order, because the recorder reads the
/// sample:
///
/// ```text
/// cargo run -p qupath-viewer --example geojson_sample
/// ./parity/record.sh
/// ```
#[test]
fn the_committed_sample_is_what_the_writer_produces() {
    let (text, written) = sample::write();
    assert_eq!(
        written,
        sample::overlay().len() - sample::SKIPPED.len(),
        "the writer skipped a different set of objects than the sample documents"
    );
    assert_eq!(
        text, SAMPLE,
        "parity/samples/detections.geojson is stale; regenerate it with \
         `cargo run -p qupath-viewer --example geojson_sample`, then re-record \
         the fixture with `./parity/record.sh`"
    );
}

/// QuPath read the geometry the sample was built from.
#[test]
fn qupath_reads_back_the_outlines_it_was_given() {
    let overlay = sample::overlay();
    let recorded = objects(FIXTURE);
    let kept: Vec<usize> = (0..overlay.len())
        .filter(|object| !sample::SKIPPED.contains(object))
        .collect();
    assert_eq!(
        recorded.len(),
        kept.len(),
        "QuPath read {} objects from a file holding {}",
        recorded.len(),
        kept.len()
    );

    let mut report = String::new();
    let mut worst = 0.0_f64;
    for (block, object) in recorded.iter().zip(&kept) {
        let ring = written_ring(overlay.outline(*object));
        let (area, centroid_x, centroid_y) = area_and_centroid(&ring);
        let box_ = bounds(&ring);
        let (roi, points) = expected_roi(&ring);

        // The objectType QuPath honoured. A spelling its reader does not know
        // still opens; the objects come back annotations, land in the
        // annotation tools, and nothing says so.
        assert_eq!(
            text(block, "detection"),
            "true",
            "object {object} came back as something other than a detection"
        );
        assert_eq!(
            text(block, "class"),
            "null",
            "object {object} arrived with a classification it was not given"
        );
        // Rings here are wound clockwise, which is the opposite of what the
        // GeoJSON specification asks of an exterior ring. JTS does not mind.
        assert_eq!(text(block, "valid"), "true", "object {object} is not a valid geometry");
        assert_eq!(text(block, "roi"), roi, "object {object} became a different ROI");
        assert_eq!(
            text(block, "points").parse::<usize>().expect("a vertex count"),
            points,
            "object {object} came back with a different vertex count"
        );

        for (what, ours, key) in [
            ("area", area, "area"),
            ("centroid x", centroid_x, "centroidX"),
            ("centroid y", centroid_y, "centroidY"),
            ("bounds x", box_[0], "boundsX"),
            ("bounds y", box_[1], "boundsY"),
            ("bounds width", box_[2], "boundsWidth"),
            ("bounds height", box_[3], "boundsHeight"),
        ] {
            worst = worst.max(agree(
                &format!("object {object} {what}"),
                ours,
                number(block, key),
            ));
        }
        let _ = writeln!(
            report,
            "  object {object}  {roi:<9} {points} points  area {area:>10.2}"
        );
    }
    println!("{report}{} objects  worst relative {worst:.3e}", kept.len());
}

/// The measurement columns arrive intact, on the objects they belong to.
///
/// Two claims, and the sample is built so that each can fail alone.
///
/// **The pairing.** This is what the two skipped rings are in the sample for.
/// Columns are indexed by object, so a writer that advanced them by written
/// feature instead would put object 5's area on object 3 — a number of the
/// right kind, on the wrong outline, which nothing downstream can detect.
///
/// **The value, asserted bit for bit and not within a tolerance.** QuPath
/// stores a detection's measurements as `float` —
/// `PathDetectionObject.createEmptyMeasurementList` asks for
/// `MeasurementListType.FLOAT` — and [`Measurement::values`](qupath_viewer) is
/// `f32`. `{}` on an `f32` writes the shortest decimal that reads back as the
/// same float, so the value QuPath ends up holding is the same 32 bits that
/// went in. That is a property of the format rather than an accident of a libm,
/// which is why it is a gate here and not a reported margin.
///
/// It is also the assertion that fails on the bug this file found: measurements
/// went through the *coordinate* writer, which snaps to QuPath's two-decimal
/// geometry grid, and a haematoxylin OD mean of 0.812345 was being exported as
/// 0.81.
#[test]
fn qupath_reads_each_measurement_against_the_object_it_belongs_to() {
    let overlay = sample::overlay();
    let (area, mean) = (sample::area(), sample::mean());
    let recorded = objects(FIXTURE);
    let kept: Vec<usize> = (0..overlay.len())
        .filter(|object| !sample::SKIPPED.contains(object))
        .collect();

    for (block, object) in recorded.iter().zip(&kept) {
        let found = measurements(block);
        assert_eq!(
            found.len(),
            2,
            "object {object} came back with {} measurements",
            found.len()
        );
        assert_eq!(found[0].0, qupath_pipeline::AREA);
        assert_eq!(found[1].0, qupath_pipeline::MEAN);

        // `write_measurement` puts `0` where a non-finite value would not
        // parse, so what QuPath reads back for object 2 is a present zero
        // rather than an absent measurement — which is `NaN` on the Java side,
        // and would come through this fixture as the string.
        let mean = if mean[*object].is_finite() {
            mean[*object]
        } else {
            0.0
        };
        for (what, ours, qupath) in [
            ("area", area[*object], found[0].1),
            ("mean", mean, found[1].1),
        ] {
            assert_eq!(
                f64::from(ours),
                qupath,
                "object {object} {what}: {ours} was written and QuPath read {qupath}"
            );
        }
    }
}
