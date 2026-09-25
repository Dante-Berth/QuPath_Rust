//! Colouring objects by a measurement — what QuPath calls a *measurement map*.
//!
//! `docs/dev/VIEWER.md` §3.5b. Three pieces, all pure: a colour table, the rule
//! that turns a value and a display range into an entry of it, and the range
//! itself. The overlay knows none of this — it hands out a row index per
//! segment ([`crate::Overlay::owned_segments_into`]) and a caller looks the
//! colour up here.
//!
//! # The table and the index rule are QuPath's
//!
//! [`INFERNO`] is QuPath's default colour map, built from QuPath's own
//! `colormaps/Inferno.tsv` (matplotlib's, released CC0) the way
//! `ColorMaps.DefaultColorMap` builds it: each channel rounded to a byte, then
//! resampled to 256 entries with Java's truncating integer arithmetic. [`index`]
//! is `ColorMaps.getInd`. Both are pinned against colours recorded from
//! QuPath's own `ColorMap.getColor` by `tests/parity_colour_map.rs` — a reading
//! of the Java is not accepted as a check here, and the recording is what makes
//! "QuPath's colours" a claim rather than an intention.
//!
//! Inferno and not viridis because it is what QuPath shows by default
//! (`ColorMaps.preferredDefaultColorMaps` lists it first). Both are perceptually
//! uniform; both start dark, which is the price of being so, and on a
//! haematoxylin-dark nucleus the lowest values are the hardest to see.
//!
//! # The range is not QuPath's, deliberately
//!
//! QuPath's `MeasurementMapper` starts its display range at the data's minimum
//! and maximum. One outlier then compresses every other object into a few
//! entries of the table — a single segmentation artefact of ten times the
//! usual area turns the whole field one colour. [`Range::robust`] takes the
//! 2nd and 98th percentiles instead, so the colours spread over the objects
//! actually present and the few beyond either end are drawn in the end colours,
//! which is what QuPath's own index rule does with a value outside the range.
//! The legend states the range, so what an end colour means is on screen.
//!
//! # A missing value is grey, and QuPath draws nothing for it
//!
//! QuPath's mapper returns `null` for `NaN`. Here a row with no value is drawn
//! in [`NO_VALUE`], a grey no entry of [`INFERNO`] comes near, so an object
//! the chosen column says nothing about is still visibly an object — and
//! visibly not a low value, which is what drawing it in the first colour
//! would claim.

use crate::compose::pack;

/// Entries in [`INFERNO`]: QuPath's `DefaultColorMap.nColors`.
pub const ENTRIES: usize = 256;

/// QuPath's Inferno colour map, packed as [`crate::compose::pack`] packs.
///
/// Generated from `qupath-core/src/main/resources/colormaps/Inferno.tsv` by
/// the arithmetic in `ColorMaps.DefaultColorMap`'s constructor, and checked
/// entry by entry against QuPath's own output (see the module documentation).
pub const INFERNO: [u32; ENTRIES] = [
    0x00_0004, 0x00_0004, 0x01_0005, 0x01_0107, 0x01_0109, 0x02_010b, 0x02_020d, 0x02_020f,
    0x03_0211, 0x04_0313, 0x04_0316, 0x05_0418, 0x06_041a, 0x07_051c, 0x08_051e, 0x09_0621,
    0x0a_0723, 0x0b_0725, 0x0c_0828, 0x0d_082a, 0x0f_092c, 0x10_092f, 0x11_0a31, 0x13_0a33,
    0x14_0b36, 0x15_0b38, 0x17_0b3b, 0x18_0c3d, 0x1a_0c40, 0x1b_0c42, 0x1d_0c44, 0x1e_0c47,
    0x20_0c49, 0x22_0c4b, 0x23_0c4e, 0x25_0c50, 0x27_0c52, 0x28_0b54, 0x2a_0b56, 0x2c_0b58,
    0x2e_0b5a, 0x30_0a5b, 0x31_0a5d, 0x33_0a5e, 0x35_0a60, 0x37_0961, 0x38_0962, 0x3a_0963,
    0x3c_0964, 0x3d_0965, 0x3f_0966, 0x41_0a67, 0x43_0a68, 0x44_0a68, 0x46_0a69, 0x48_0b6a,
    0x49_0b6a, 0x4b_0c6b, 0x4c_0c6b, 0x4e_0d6c, 0x50_0d6c, 0x51_0e6c, 0x53_0e6d, 0x54_0f6d,
    0x56_0f6d, 0x58_106e, 0x59_106e, 0x5b_116e, 0x5c_126e, 0x5e_126e, 0x60_136e, 0x61_136e,
    0x63_146e, 0x64_156e, 0x66_156e, 0x68_166e, 0x69_166e, 0x6b_176e, 0x6c_186e, 0x6e_186e,
    0x70_196e, 0x71_196e, 0x73_1a6e, 0x74_1a6e, 0x76_1b6e, 0x77_1c6d, 0x79_1c6d, 0x7b_1d6d,
    0x7c_1d6d, 0x7e_1e6d, 0x7f_1e6c, 0x81_1f6c, 0x83_206c, 0x84_206b, 0x86_216b, 0x87_216b,
    0x89_226a, 0x8b_226a, 0x8c_2369, 0x8e_2369, 0x8f_2469, 0x91_2568, 0x92_2568, 0x94_2667,
    0x96_2667, 0x97_2766, 0x99_2766, 0x9a_2865, 0x9c_2964, 0x9e_2964, 0x9f_2a63, 0xa1_2a63,
    0xa2_2b62, 0xa4_2c61, 0xa5_2c60, 0xa7_2d60, 0xa8_2e5f, 0xaa_2e5e, 0xac_2f5e, 0xad_305d,
    0xaf_305c, 0xb0_315b, 0xb2_325a, 0xb3_325a, 0xb5_3359, 0xb6_3458, 0xb8_3557, 0xb9_3556,
    0xbb_3655, 0xbc_3754, 0xbd_3853, 0xbf_3952, 0xc0_3a51, 0xc1_3a50, 0xc3_3b4f, 0xc4_3c4e,
    0xc6_3d4d, 0xc7_3e4c, 0xc8_3f4b, 0xca_404a, 0xcb_4149, 0xcc_4248, 0xce_4347, 0xcf_4446,
    0xd0_4545, 0xd2_4644, 0xd3_4743, 0xd4_4842, 0xd5_4a41, 0xd7_4b3f, 0xd8_4c3e, 0xd9_4d3d,
    0xda_4e3c, 0xdb_503b, 0xdd_513a, 0xde_5238, 0xdf_5337, 0xe0_5536, 0xe1_5635, 0xe2_5734,
    0xe3_5933, 0xe4_5a31, 0xe5_5c30, 0xe6_5d2f, 0xe7_5e2e, 0xe8_602d, 0xe9_612b, 0xea_632a,
    0xeb_6429, 0xeb_6628, 0xec_6726, 0xed_6925, 0xee_6a24, 0xef_6c23, 0xef_6e21, 0xf0_6f20,
    0xf1_711f, 0xf1_731d, 0xf2_741c, 0xf3_761b, 0xf3_7819, 0xf4_7918, 0xf5_7b17, 0xf5_7d15,
    0xf6_7e14, 0xf6_8013, 0xf7_8212, 0xf7_8410, 0xf8_850f, 0xf8_870e, 0xf8_890c, 0xf9_8b0b,
    0xf9_8c0a, 0xf9_8e09, 0xfa_9008, 0xfa_9207, 0xfa_9407, 0xfb_9606, 0xfb_9706, 0xfb_9906,
    0xfb_9b06, 0xfb_9d07, 0xfc_9f07, 0xfc_a108, 0xfc_a309, 0xfc_a50a, 0xfc_a60c, 0xfc_a80d,
    0xfc_aa0f, 0xfc_ac11, 0xfc_ae12, 0xfc_b014, 0xfc_b216, 0xfc_b418, 0xfb_b61a, 0xfb_b81d,
    0xfb_ba1f, 0xfb_bc21, 0xfb_be23, 0xfa_c026, 0xfa_c228, 0xfa_c42a, 0xfa_c62d, 0xf9_c72f,
    0xf9_c932, 0xf9_cb35, 0xf8_cd37, 0xf8_cf3a, 0xf7_d13d, 0xf7_d340, 0xf6_d543, 0xf6_d746,
    0xf5_d949, 0xf5_db4c, 0xf4_dd4f, 0xf4_df53, 0xf4_e156, 0xf3_e35a, 0xf3_e55d, 0xf2_e661,
    0xf2_e865, 0xf2_ea69, 0xf1_ec6d, 0xf1_ed71, 0xf1_ef75, 0xf1_f179, 0xf2_f27d, 0xf2_f482,
    0xf3_f586, 0xf3_f68a, 0xf4_f88e, 0xf5_f992, 0xf6_fa96, 0xf8_fb9a, 0xf9_fc9d, 0xfc_ffa4,
];

/// The colour of an object whose value is missing (`NaN`).
///
/// Mid grey: off the colour map's hue path entirely, and neither of its ends.
pub const NO_VALUE: u32 = pack(128, 128, 128);

/// Which entry of a 256-colour table `value` maps to, for a display range.
///
/// `ColorMaps.getInd`: the fraction of the way from `low` to `high`, times the
/// number of entries (256, not 255 — so the top entry is reached a half-step
/// early), rounded half up as Java's `Math.round` rounds, and clamped. A value
/// below the range takes the first entry and one above it the last. A range of
/// no width sends everything to the first entry, which is what the Java's
/// division by zero comes to.
///
/// Computed in `f64` from `f32` inputs, as QuPath widens its `float`
/// measurements to `double` before the same arithmetic.
///
/// **One deliberate difference.** Java casts the rounded `long` to `int`, so a
/// value more than about eight million range-widths outside the range wraps
/// and lands on an arbitrary entry — `+∞` on the *first*, which the recording
/// shows. This clamps instead, so anything above the range is the last colour.
/// No display range and measurement column met in practice come near it, and
/// QuPath's own mapper replaces an infinity before it gets here.
#[must_use]
pub fn index(value: f64, low: f64, high: f64) -> usize {
    let (min, max) = (low.min(high), low.max(high));
    // Stated rather than left to the arithmetic: dividing by a zero width gives
    // an infinity, which clamps to the *last* entry here and wraps to the first
    // in the Java. The recording says first.
    if max.partial_cmp(&min) != Some(std::cmp::Ordering::Greater) {
        return 0;
    }
    let scaled = (value - min) / (max - min) * ENTRIES as f64;
    // Java's Math.round: nearest, ties towards positive infinity, NaN to 0.
    let floor = scaled.floor();
    let rounded = if scaled - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    let index = if rounded.is_nan() {
        0
    } else {
        // Clamped in floating point, so the cast below is of a value in range.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=255 immediately before the cast"
        )]
        {
            rounded.clamp(0.0, (ENTRIES - 1) as f64) as usize
        }
    };
    // QuPath's reversed range: a range given high-to-low reads the table
    // backwards. Nothing here builds one, but the rule is kept whole.
    if low > high {
        ENTRIES - 1 - index
    } else {
        index
    }
}

/// A display range: the values the two ends of the colour map stand for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    /// The value drawn in the first colour, and everything below it.
    pub low: f32,
    /// The value drawn in the last colour, and everything above it.
    pub high: f32,
}

/// The lower percentile [`Range::robust`] takes. See the module documentation.
pub const LOW_PERCENTILE: f64 = 2.0;

/// The upper percentile [`Range::robust`] takes.
pub const HIGH_PERCENTILE: f64 = 98.0;

impl Range {
    /// The 2nd to 98th percentile of the finite values in `values`, or `None`
    /// when there are none.
    ///
    /// Nearest rank on the sorted finite values: the value at position
    /// `round(p / 100 * (n - 1))`. `NaN` and the infinities are left out, so a
    /// column with missing rows is ranged over the rows it has. Linear in the
    /// column (two selections), and meant to be computed once when the column
    /// or the run changes, not per frame.
    #[must_use]
    pub fn robust(values: &[f32]) -> Option<Self> {
        let mut finite: Vec<f32> = values.iter().copied().filter(|v| v.is_finite()).collect();
        if finite.is_empty() {
            return None;
        }
        let rank = |percentile: f64, n: usize| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a fraction of n - 1, so within 0..n"
            )]
            let rank = (percentile / 100.0 * (n - 1) as f64).round() as usize;
            rank.min(n - 1)
        };
        let n = finite.len();
        let (lower, upper) = (rank(LOW_PERCENTILE, n), rank(HIGH_PERCENTILE, n));
        let high = *finite.select_nth_unstable_by(upper, f32::total_cmp).1;
        // The lower rank is in the part `select_nth` left below `upper`.
        let low = *finite[..=upper]
            .select_nth_unstable_by(lower, f32::total_cmp)
            .1;
        Some(Self { low, high })
    }

    /// The colour `value` is drawn in: [`NO_VALUE`] for `NaN`, and otherwise
    /// the [`INFERNO`] entry [`index`] picks.
    #[must_use]
    pub fn colour(self, value: f32) -> u32 {
        if value.is_nan() {
            return NO_VALUE;
        }
        INFERNO[index(f64::from(value), f64::from(self.low), f64::from(self.high))]
    }

    /// One colour per row of `values`, written into `out`.
    ///
    /// `out` is cleared first. Per row rather than per visible object because
    /// the drawing code looks colours up by row, and a column of 150,000 is
    /// 600 kB computed once, where a lookup per segment per frame would redo
    /// the arithmetic sixty times a second for nothing.
    pub fn colours_into(self, values: &[f32], out: &mut Vec<u32>) {
        out.clear();
        out.extend(values.iter().map(|&value| self.colour(value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ends_and_the_middle_of_the_range() {
        assert_eq!(index(0.0, 0.0, 1.0), 0);
        assert_eq!(index(1.0, 0.0, 1.0), 255);
        // 0.5 * 256 = 128 exactly.
        assert_eq!(index(0.5, 0.0, 1.0), 128);
        // Below and above clamp to the ends rather than wrapping.
        assert_eq!(index(-3.0, 0.0, 1.0), 0);
        assert_eq!(index(40.0, 0.0, 1.0), 255);
        // Half up, as Java rounds: 1.5 / 256 of the way is entry 2, and a hair
        // under it is entry 1.
        assert_eq!(index(1.5 / 256.0, 0.0, 1.0), 2);
        assert_eq!(index(1.49 / 256.0, 0.0, 1.0), 1);
        // No width: everything to the first entry, as the Java's NaN does.
        assert_eq!(index(7.0, 7.0, 7.0), 0);
        assert_eq!(index(f64::NAN, 0.0, 1.0), 0);
    }

    #[test]
    fn the_range_ignores_missing_values_and_outliers() {
        // One hundred ordinary values and one enormous one. Min–max would put
        // the top of the range at 10,000 and every ordinary object in the
        // first four entries of the table; the percentiles do not.
        let mut values: Vec<f32> = (1..=100).map(|v| v as f32).collect();
        values.push(10_000.0);
        values.push(f32::NAN);
        values.push(f32::INFINITY);
        let range = Range::robust(&values).expect("finite values");
        // 101 finite values: ranks round(0.02 * 100) = 2 and round(0.98 * 100) = 98.
        assert_eq!(
            range,
            Range {
                low: 3.0,
                high: 99.0
            }
        );
        assert_eq!(Range::robust(&[f32::NAN]), None);
        assert_eq!(Range::robust(&[]), None);
        assert_eq!(
            Range::robust(&[4.0]),
            Some(Range {
                low: 4.0,
                high: 4.0
            })
        );
    }

    #[test]
    fn a_missing_value_is_grey_and_not_the_first_colour() {
        let range = Range {
            low: 0.0,
            high: 1.0,
        };
        assert_eq!(range.colour(f32::NAN), NO_VALUE);
        assert_ne!(range.colour(0.0), NO_VALUE);
        assert_ne!(INFERNO[0], NO_VALUE);

        let mut out = Vec::new();
        range.colours_into(&[0.0, f32::NAN, 1.0], &mut out);
        assert_eq!(out, [INFERNO[0], NO_VALUE, INFERNO[255]]);
        // Grey is not on the map at all, so it cannot be read as a value.
        assert!(!INFERNO.contains(&NO_VALUE));
    }

    #[test]
    fn the_table_runs_dark_to_light() {
        // A perceptually uniform sequential map rises in lightness; a table
        // pasted in the wrong order, or with channels swapped, does not rise
        // in this proxy for it. Compared sixteen entries apart, because
        // rounding each channel to a byte makes neighbouring dark entries dip
        // by a unit of this weighted sum (0x270c52 then 0x280b54).
        let luma = |c: u32| {
            let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            2126 * r + 7152 * g + 722 * b
        };
        for (from, to) in INFERNO.iter().zip(&INFERNO[16..]) {
            assert!(luma(*to) > luma(*from), "{from:06x} then {to:06x}");
        }
        assert_eq!(INFERNO[0], pack(0, 0, 4));
        assert_eq!(INFERNO[255], pack(252, 255, 164));
    }
}
