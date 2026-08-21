//! Writing detected objects out as GeoJSON, in the dialect QuPath reads.
//!
//! `docs/VIEWER.md` §8.2's last item, and a **deliberate revision of §0's
//! stated non-goal** rather than a gap being filled: §0 says nothing the
//! application finds is saved, editable or exported, and this changes the third
//! of those. It was confirmed wanted before it was written. The other two stand
//! — there is still no project format and no annotation editing here.
//!
//! # Why the format is copied exactly
//!
//! A file QuPath can open is worth much more than one that is merely similar,
//! and "similar" is the easy accident: GeoJSON is a published specification, so
//! writing something a validator accepts takes no effort at all and still lands
//! in a viewer that shows nothing. The shape below is from
//! `QuPathTypeAdapters.PathObjectTypeAdapter.write`, not from the GeoJSON
//! specification:
//!
//! ```text
//! {"type": "FeatureCollection", "features": [
//!   {"type": "Feature",
//!    "geometry": {"type": "Polygon", "coordinates": [[[x, y], ...]]},
//!    "properties": {"objectType": "detection", "measurements": {...}}}
//! ]}
//! ```
//!
//! Two details there are QuPath's and not GeoJSON's: `objectType` is the
//! camelCase spelling its reader wants — it warns on the older `object_type` —
//! and `measurements` is an object of name to number, which is the post-0.4.0
//! encoding rather than the array of `{name, value}` that came before.
//!
//! # Why there is no serde
//!
//! This workspace has no serialisation dependency and this is not a good enough
//! reason to acquire one. The output is one nesting of arrays and a flat map of
//! numbers, the escaping surface is a handful of key names this module writes
//! itself, and a derive would still need the field order and the naming above
//! specified by hand. [`escape`] exists for the one string a caller supplies.
//!
//! # Coordinates
//!
//! **Level-0 pixels**, which is what [`crate::Overlay`] holds and what QuPath
//! expects — its ROIs are in the full-resolution frame regardless of the level
//! anything was computed at. A file written in level coordinates opens without
//! complaint and puts every object in the top-left corner at the wrong size,
//! which is exactly the kind of wrong that looks like a working export.

use std::fmt::Write as _;

use crate::overlay::Overlay;

/// QuPath's `objectType` for a detected object.
///
/// The alternative is `"annotation"`, which is what QuPath calls something a
/// person drew. What this project produces is neither drawn nor editable, so
/// `detection` is the honest label and is also the one that keeps these objects
/// out of the annotation tools when the file is opened.
pub const DETECTION: &str = "detection";

/// A measurement to write alongside every object.
///
/// `values` is one number per object, in the overlay's row order — the
/// alignment [`Overlay`] documents, where one `push_level_ring` is one row.
#[derive(Debug, Clone, Copy)]
pub struct Measurement<'a> {
    /// The name QuPath will show the column under.
    pub name: &'a str,
    /// One value per object.
    pub values: &'a [f32],
}

/// Writes `overlay` as a GeoJSON `FeatureCollection`.
///
/// `measurements` may be empty; any whose length does not match the overlay is
/// **skipped rather than truncated**, because a measurement column silently cut
/// short would attach the wrong number to every object after the first missing
/// one, and a wrong measurement is worse than an absent one.
///
/// Objects with fewer than three vertices are skipped: a polygon needs three
/// corners, and [`Overlay`] admits empty rings on purpose so that its rows stay
/// aligned with whatever produced them. The count returned is how many features
/// were actually written, so a caller can report it rather than assuming
/// [`Overlay::len`].
#[must_use]
pub fn write(overlay: &Overlay, measurements: &[Measurement<'_>]) -> (String, usize) {
    let mut out = String::new();
    out.push_str("{\"type\":\"FeatureCollection\",\"features\":[");

    let usable: Vec<&Measurement<'_>> = measurements
        .iter()
        .filter(|column| column.values.len() == overlay.len())
        .collect();

    let mut written = 0usize;
    for object in 0..overlay.len() {
        let ring = overlay.outline(object);
        if ring.len() < 3 {
            continue;
        }
        if written > 0 {
            out.push(',');
        }
        written += 1;

        out.push_str("{\"type\":\"Feature\",\"geometry\":{\"type\":\"Polygon\",\"coordinates\":[[");
        for (at, vertex) in ring.iter().enumerate() {
            if at > 0 {
                out.push(',');
            }
            write_position(&mut out, vertex[0], vertex[1]);
        }
        // GeoJSON requires a linear ring's first and last positions to be
        // identical. `Overlay` closes implicitly — "the last vertex joins the
        // first, and is not repeated" — so the repeat is added here rather than
        // stored, which keeps the arena the size the drawing path wants.
        out.push(',');
        write_position(&mut out, ring[0][0], ring[0][1]);
        out.push_str("]]},\"properties\":{\"objectType\":\"");
        out.push_str(DETECTION);
        out.push('"');

        if !usable.is_empty() {
            out.push_str(",\"measurements\":{");
            for (at, column) in usable.iter().enumerate() {
                if at > 0 {
                    out.push(',');
                }
                out.push('"');
                escape(&mut out, column.name);
                out.push_str("\":");
                write_measurement(&mut out, column.values[object]);
            }
            out.push('}');
        }
        out.push_str("}}");
    }

    out.push_str("]}");
    (out, written)
}

/// Writes one `[x, y]` position.
fn write_position(out: &mut String, x: f32, y: f32) {
    out.push('[');
    write_coordinate(out, f64::from(x));
    out.push(',');
    write_coordinate(out, f64::from(y));
    out.push(']');
}

/// Writes a **coordinate** as JSON, with no exponent and no `NaN`.
///
/// JSON has no literal for a non-finite number, and a bare `NaN` is what a
/// hand-rolled writer emits by default — `serde_json` refuses it outright and
/// QuPath's own measurement writer substitutes the *string* `"NaN"`. Neither is
/// right for a coordinate, so this writes `0` and the caller never sees a file
/// that fails to parse halfway through. A non-finite coordinate cannot come out
/// of [`Overlay`], whose vertices are a product of integers and a downsample;
/// it is guarded because the alternative failure is a truncated file.
///
/// **Two decimal places, and that is QuPath's number rather than a tidy one.**
/// `GeometryTools.DEFAULT_FACTORY` is built on `new PrecisionModel(-0.01)`,
/// which in JTS is a grid size of 0.01 — every geometry QuPath holds is snapped
/// to hundredths of a pixel, so writing more digits than that stores precision
/// its reader will discard on the way in.
///
/// It is the *coordinate* rule, not a house style for numbers. Measurements go
/// through [`write_measurement`], which keeps all of them; see there for what
/// happened when one function served both.
#[expect(
    clippy::cast_possible_truncation,
    reason = "guarded to integral values below 1e15, which i64 holds exactly"
)]
fn write_coordinate(out: &mut String, value: f64) {
    if !value.is_finite() {
        out.push('0');
        return;
    }
    let rounded = (value * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 && rounded.abs() < 1e15 {
        // An integral value writes as `123` rather than `123.0`, which is what
        // QuPath's `DecimalFormat` produces and keeps the file smaller.
        let _ = write!(out, "{}", rounded as i64);
    } else {
        let text = format!("{rounded:.2}");
        out.push_str(text.trim_end_matches('0').trim_end_matches('.'));
    }
}

/// Writes a **measurement** as JSON, keeping every digit the value has.
///
/// Split from [`write_coordinate`] because one function served both and the
/// coordinate rule won. Two decimal places is right for a coordinate — QuPath's
/// own geometries are snapped to hundredths — and wrong for a measurement:
/// `Nucleus: Haematoxylin OD mean` runs from about 0 to 2, so rounding it there
/// leaves roughly two significant figures and about two hundred distinct values
/// for a number the pipeline's own [`ObjectFilter`](qupath_pipeline) thresholds
/// on. An exported file could not reproduce the run's own filtering decisions.
///
/// QuPath does not round: `QuPathTypeAdapters.MeasurementListTypeAdapter.write`
/// passes the value to `JsonWriter.value(Number)`, which writes the double out
/// in full.
///
/// **This was invisible at the one configuration everything is run at.** The
/// other column, `Nucleus: Area (px)`, is a count of labelled pixels, so at
/// level 0 it is an integer and two decimal places lose nothing at all. It only
/// becomes fractional at a level, where it is scaled by the square of the
/// downsample — and the mean beside it was quietly two digits the whole time.
///
/// Values are `f32` — [`Measurement::values`] — and `{}` on an `f32` is the
/// shortest decimal that reads back as the same float, so this writes what the
/// value actually holds rather than the wider `f64` its widening would print.
/// Non-finite goes to `0` for [`write_coordinate`]'s reason: a truncated file
/// is worse than a substituted number, and QuPath's own `"NaN"` string is not
/// something its reader requires here.
fn write_measurement(out: &mut String, value: f32) {
    if value.is_finite() {
        let _ = write!(out, "{value}");
    } else {
        out.push('0');
    }
}

/// Escapes a string into JSON, for the measurement names a caller supplies.
///
/// The control characters below `0x20` have to be escaped for the output to
/// parse at all, and a quote or backslash in a measurement name is the ordinary
/// way a hand-rolled writer produces a broken file.
pub fn escape(out: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control < ' ' => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square at a known place, in level coordinates with a downsample, so
    /// the test also sees the conversion `Overlay` does on the way in.
    fn square(overlay: &mut Overlay, x: u32, y: u32, size: u32, downsample: f64) {
        overlay.push_level_ring(
            &[[x, y], [x + size, y], [x + size, y + size], [x, y + size]],
            downsample,
        );
    }

    #[test]
    fn an_empty_overlay_writes_an_empty_collection() {
        let (json, count) = write(&Overlay::new(), &[]);
        assert_eq!(count, 0);
        assert_eq!(json, "{\"type\":\"FeatureCollection\",\"features\":[]}");
    }

    /// The whole point of the format: the field names are QuPath's, not
    /// GeoJSON's generic ones.
    #[test]
    fn a_feature_carries_qupaths_object_type() {
        let mut overlay = Overlay::new();
        square(&mut overlay, 10, 20, 4, 1.0);
        let (json, count) = write(&overlay, &[]);
        assert_eq!(count, 1);
        assert!(json.contains("\"type\":\"FeatureCollection\""));
        assert!(json.contains("\"type\":\"Feature\""));
        assert!(json.contains("\"type\":\"Polygon\""));
        assert!(
            json.contains("\"objectType\":\"detection\""),
            "camelCase is what QuPath's reader wants: {json}",
        );
        assert!(
            !json.contains("object_type"),
            "the snake_case spelling is the one QuPath warns about",
        );
    }

    /// GeoJSON requires the ring to close, and `Overlay` deliberately does not
    /// store the repeat. A file whose rings do not close is the kind that opens
    /// and then draws wrongly.
    #[test]
    fn the_ring_closes() {
        let mut overlay = Overlay::new();
        square(&mut overlay, 0, 0, 8, 1.0);
        let (json, _) = write(&overlay, &[]);
        let start = json.find("[[[").expect("coordinates") + 3;
        let end = json[start..].find("]]").expect("end of ring") + start;
        let ring: Vec<&str> = json[start..end].split("],[").collect();
        assert_eq!(
            ring.first().map(|s| s.trim_start_matches('[')),
            ring.last().map(|s| s.trim_end_matches(']')),
            "first and last positions must be identical: {ring:?}",
        );
        // Four corners plus the repeat.
        assert_eq!(ring.len(), 5);
    }

    /// Coordinates are level-0 pixels, so a ring detected at a downsample of 4
    /// must come out four times further from the origin. This is the check that
    /// separates a working export from one that puts everything in the corner
    /// at the wrong size.
    #[test]
    fn coordinates_are_level_zero_pixels() {
        let mut fine = Overlay::new();
        square(&mut fine, 100, 200, 10, 1.0);
        let mut coarse = Overlay::new();
        square(&mut coarse, 100, 200, 10, 4.0);
        let (fine_json, _) = write(&fine, &[]);
        let (coarse_json, _) = write(&coarse, &[]);
        assert!(fine_json.contains("[100,200]"), "{fine_json}");
        assert!(
            coarse_json.contains("[400,800]"),
            "a downsample of 4 multiplies the coordinates: {coarse_json}",
        );
    }

    #[test]
    fn measurements_are_written_per_object() {
        let mut overlay = Overlay::new();
        square(&mut overlay, 0, 0, 4, 1.0);
        square(&mut overlay, 20, 0, 4, 1.0);
        let areas = [12.5f32, 30.0];
        let (json, count) = write(
            &overlay,
            &[Measurement {
                name: "Area",
                values: &areas,
            }],
        );
        assert_eq!(count, 2);
        assert!(json.contains("\"measurements\":{\"Area\":12.5}"), "{json}");
        assert!(json.contains("\"measurements\":{\"Area\":30}"), "{json}");
    }

    /// A column of the wrong length is dropped rather than truncated: attaching
    /// the wrong number to an object is worse than attaching none.
    #[test]
    fn a_mismatched_measurement_column_is_skipped() {
        let mut overlay = Overlay::new();
        square(&mut overlay, 0, 0, 4, 1.0);
        square(&mut overlay, 20, 0, 4, 1.0);
        let short = [1.0f32];
        let (json, _) = write(
            &overlay,
            &[Measurement {
                name: "Area",
                values: &short,
            }],
        );
        assert!(!json.contains("measurements"), "{json}");
    }

    /// `Overlay` admits empty rings to keep its rows aligned, and a polygon
    /// needs three corners. The returned count is what was written, not
    /// `Overlay::len`.
    #[test]
    fn degenerate_objects_are_skipped_and_not_counted() {
        let mut overlay = Overlay::new();
        square(&mut overlay, 0, 0, 4, 1.0);
        overlay.push_level_ring(&[], 1.0);
        overlay.push_level_ring(&[[5, 5], [6, 6]], 1.0);
        let (json, count) = write(&overlay, &[]);
        assert_eq!(overlay.len(), 3);
        assert_eq!(count, 1, "only the square is a polygon");
        assert_eq!(json.matches("\"type\":\"Feature\"").count(), 1);
    }

    /// Features are separated by commas, and the separator being attached to
    /// the skipped objects rather than the written ones is how a file with a
    /// hole in it gets produced — `[a,,c]` parses nowhere.
    #[test]
    fn skipping_an_object_leaves_no_empty_slot() {
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&[], 1.0);
        square(&mut overlay, 0, 0, 4, 1.0);
        overlay.push_level_ring(&[], 1.0);
        square(&mut overlay, 20, 0, 4, 1.0);
        let (json, count) = write(&overlay, &[]);
        assert_eq!(count, 2);
        assert!(!json.contains(",,"), "{json}");
        assert!(!json.contains("[,"), "{json}");
        assert!(!json.contains(",]"), "{json}");
    }

    #[test]
    fn names_with_quotes_do_not_break_the_file() {
        let mut out = String::new();
        escape(&mut out, "a \"odd\\name\nhere");
        assert_eq!(out, "a \\\"odd\\\\name\\nhere");
    }

    #[test]
    fn non_finite_numbers_do_not_produce_invalid_json() {
        let mut out = String::new();
        write_coordinate(&mut out, f64::NAN);
        write_coordinate(&mut out, f64::INFINITY);
        write_measurement(&mut out, f32::NAN);
        write_measurement(&mut out, f32::NEG_INFINITY);
        assert_eq!(out, "0000", "JSON has no literal for any of them");
    }

    /// A measurement keeps its digits; a coordinate is snapped to QuPath's grid.
    ///
    /// One function wrote both, at the coordinate's two decimal places, and the
    /// column that would have shown it is the one that cannot: `Nucleus: Area
    /// (px)` is a pixel count, so at level 0 — the only configuration anything
    /// is ever run at — it is an integer and rounding it changes nothing. The
    /// mean optical density beside it runs from about 0 to 2 and was being
    /// exported with two significant figures.
    #[test]
    fn a_measurement_is_not_rounded_to_the_coordinate_grid() {
        let mut overlay = Overlay::new();
        square(&mut overlay, 10, 20, 6, 1.0);
        let od = [0.812_345_f32];
        let (json, _) = write(
            &overlay,
            &[Measurement {
                name: "Nucleus: Haematoxylin OD mean",
                values: &od,
            }],
        );
        assert!(
            json.contains("\"Nucleus: Haematoxylin OD mean\":0.812345"),
            "the measurement was rounded away: {json}"
        );

        // The coordinates in the same file still are snapped, which is the half
        // that has to keep matching `PrecisionModel(-0.01)`.
        let mut fractional = Overlay::new();
        fractional.push_level_ring(&[[1, 1], [3, 1], [3, 3]], 1.0 / 3.0);
        let (json, _) = write(&fractional, &[]);
        assert!(json.contains("[0.33,0.33]"), "coordinates moved off the grid: {json}");
    }

    /// The brace and bracket counts balancing is the cheap structural check
    /// that catches a missing close, which is what a hand-rolled writer gets
    /// wrong. Not a substitute for a parser — `parity` compares against
    /// QuPath — but it fails loudly on the mistake this file is most likely to
    /// make.
    #[test]
    fn the_output_is_balanced() {
        let mut overlay = Overlay::new();
        for at in 0..5 {
            square(&mut overlay, at * 20, 0, 6, 2.0);
        }
        let values = [1.0f32, 2.0, 3.0, 4.0, 5.0];
        let (json, _) = write(
            &overlay,
            &[Measurement {
                name: "Area",
                values: &values,
            }],
        );
        assert_eq!(json.matches('{').count(), json.matches('}').count());
        assert_eq!(json.matches('[').count(), json.matches(']').count());
    }
}
