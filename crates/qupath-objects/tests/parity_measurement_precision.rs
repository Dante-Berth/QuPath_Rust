//! Parity against recorded QuPath output.
//!
//! Asserts that storing a value in a [`Table`] column retains exactly the bits
//! QuPath's `DOUBLE`/`FLOAT` measurement lists retain — see `DIVERGENCES.md`
//! D-001. The fixture is recorded from a real QuPath build by
//! `parity/record.sh`; regenerate it there rather than editing it by hand.

use qupath_objects::Table;

/// The recorded fixture, parsed without pulling in a JSON dependency.
///
/// Each entry is `(input_bits, qupath_double_bits, qupath_general_bits)` as
/// raw IEEE-754 `f64` bit patterns.
fn fixture() -> Vec<(u64, u64, u64)> {
    let text = include_str!("../../../parity/fixtures/MeasurementPrecision.json");

    let hex = |line: &str| -> u64 {
        let quoted = line
            .rsplit(':')
            .next()
            .expect("field has a value")
            .trim()
            .trim_end_matches(',')
            .trim_matches('"');
        u64::from_str_radix(quoted, 16).expect("value is a hex bit pattern")
    };

    let mut cases = Vec::new();
    let (mut input, mut general, mut double) = (None, None, None);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\"input_bits\"") {
            input = Some(hex(trimmed));
        } else if trimmed.starts_with("\"GENERAL\"") {
            general = Some(hex(trimmed));
        } else if trimmed.starts_with("\"DOUBLE\"") {
            double = Some(hex(trimmed));
        } else if trimmed.starts_with("\"FLOAT\"") {
            let float = hex(trimmed);
            let d = double.take().expect("DOUBLE precedes FLOAT in the fixture");
            assert_eq!(
                d, float,
                "fixture records DOUBLE and FLOAT diverging, which breaks D-001"
            );
            cases.push((
                input.take().expect("input_bits precedes the stored values"),
                d,
                general.take().expect("GENERAL precedes FLOAT"),
            ));
        }
    }
    assert!(!cases.is_empty(), "fixture parsed to zero cases");
    cases
}

/// Java's `(float)` narrowing and Rust's `as f32` must agree, including the
/// round-to-nearest-even tie behaviour and NaN/infinity handling.
#[test]
fn f32_column_reproduces_qupath_double_list_bit_exactly() {
    for (input_bits, qupath_bits, _) in fixture() {
        let input = f64::from_bits(input_bits);

        let mut table = Table::new();
        let feature = table.add_feature("x");
        let row = table.push_object();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the truncation under test: QuPath narrows to f32 too"
        )]
        table.set(row, feature, input as f32);

        let stored = f64::from(table.value(row, feature));
        let expected = f64::from_bits(qupath_bits);

        if expected.is_nan() {
            assert!(
                stored.is_nan(),
                "QuPath stored NaN for input {input}, we stored {stored}"
            );
        } else {
            assert_eq!(
                stored.to_bits(),
                qupath_bits,
                "input {input}: QuPath stored {expected} ({qupath_bits:016x}), \
                 we stored {stored} ({:016x})",
                stored.to_bits()
            );
        }
    }
}

/// Guards the claim in D-001 that `GENERAL` is the one list type we do *not*
/// match bit-for-bit. If this stops holding, the divergence note is stale.
#[test]
fn general_list_retains_precision_that_f32_columns_cannot() {
    let mut differed = 0_usize;
    for (input_bits, double_bits, general_bits) in fixture() {
        assert_eq!(
            general_bits, input_bits,
            "GENERAL is documented to store the input unchanged"
        );
        if general_bits != double_bits {
            differed += 1;
        }
    }
    assert!(
        differed > 0,
        "no fixture case distinguishes GENERAL from DOUBLE, so the fixture \
         cannot detect a regression in D-001"
    );
}

/// The absent-measurement contract QuPath callers depend on.
#[test]
fn absent_measurement_matches_recorded_qupath_behaviour() {
    let recorded = include_str!("../../../parity/fixtures/MeasurementPrecision.json");
    assert!(
        recorded.contains("\"absent_measurement_returns\": \"NaN\""),
        "fixture no longer records absent measurements as NaN"
    );

    let mut table = Table::new();
    let row = table.push_object();
    assert!(table.get(row, "not a real feature").is_nan());
}
