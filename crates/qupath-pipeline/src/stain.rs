//! Colour deconvolution: RGB to stain optical density.
//!
//! Cell detection does not run on RGB. QuPath's `WatershedCellDetection`
//! separates the haematoxylin and eosin contributions first and detects nuclei
//! on the haematoxylin channel, because nuclei are what haematoxylin stains.
//!
//! The transform is Ruifrok and Johnston's: convert each channel to optical
//! density, `OD = -log10(I / max)`, then project the OD vector onto the inverse
//! of the stain matrix.
//!
//! Per pixel that is three `log10` calls and a dot product, but the result
//! depends on the three channel bytes and nothing else, so it is computed once
//! per byte value into a table instead — as QuPath does in `makeODLUT`. See
//! [`Stains::stain_into`].
//!
//! Pinned by `parity/fixtures/ColourDeconvolution.json`, recorded from
//! `ColorTransformer.getTransformedPixels` — the call `IJTools.colorDeconvolve`
//! makes on QuPath's behalf during a detection run. Before that fixture existed
//! this module used the **DAB** vector under the name `EOSIN`, so it had been
//! performing H-DAB deconvolution and calling it H&E. It produced a perfectly
//! plausible single-channel image throughout.

/// A stain matrix and its inverse, ready to project optical density.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stains {
    /// Deconvolution coefficients, `[stain][channel]`.
    ///
    /// This is the **transpose** of QuPath's `getMatrixInverse()`. QuPath reads
    /// that matrix as `invMat[channel][stain]` — down a column — so holding the
    /// transpose puts one stain's three coefficients in a contiguous row, which
    /// is the order a per-pixel dot product wants. See `invert`.
    coefficients: [[f64; 3]; 3],
    /// Per-channel white point, QuPath's `maxRed`, `maxGreen`, `maxBlue`.
    max_values: [f64; 3],
}

/// QuPath's default haematoxylin vector, from Ruifrok and Johnston's paper.
const HAEMATOXYLIN: [f64; 3] = [0.65, 0.70, 0.29];
/// QuPath's default eosin vector.
///
/// **Not** Ruifrok and Johnston's `[0.07, 0.99, 0.11]`, which QuPath carries
/// commented out beside this one. QuPath uses the AMIDA13 estimate, and using
/// the paper's value instead is a silent 20% error on the haematoxylin channel.
const EOSIN: [f64; 3] = [0.2159, 0.8012, 0.5581];
/// QuPath's default DAB vector, from Ruifrok and Johnston's paper.
///
/// Close enough to [`EOSIN`] in shape to be mistaken for it, and far enough to
/// change every value downstream.
const DAB: [f64; 3] = [0.27, 0.57, 0.78];

/// QuPath's default white point: 8-bit full scale on every channel.
const DEFAULT_MAX: [f64; 3] = [255.0, 255.0, 255.0];

impl Default for Stains {
    fn default() -> Self {
        Self::h_and_e()
    }
}

impl Stains {
    /// The default H&E stains, matching
    /// `ColorDeconvolutionStains.makeDefaultColorDeconvolutionStains(H_E)`.
    #[must_use]
    pub fn h_and_e() -> Self {
        Self::from_two(HAEMATOXYLIN, EOSIN, DEFAULT_MAX)
    }

    /// The default H-DAB stains, matching QuPath's `H_DAB`.
    #[must_use]
    pub fn h_dab() -> Self {
        Self::from_two(HAEMATOXYLIN, DAB, DEFAULT_MAX)
    }

    /// Builds stains from two measured vectors and a per-channel white point.
    ///
    /// The third vector is the "residual" QuPath synthesises when only two
    /// stains are known: the cross product of the first two, normalised.
    ///
    /// This is the constructor to use for a slide that carries its own stain
    /// estimate in its metadata. QuPath prefers those to the defaults whenever
    /// they are present, and only falls back to [`Self::h_and_e`] otherwise, so
    /// deconvolving such a slide with the defaults is not what QuPath would do.
    #[must_use]
    pub fn from_two(first: [f64; 3], second: [f64; 3], max_values: [f64; 3]) -> Self {
        let first = normalise(first);
        let second = normalise(second);
        let third = normalise(cross(first, second));
        Self {
            coefficients: invert([first, second, third]),
            max_values,
        }
    }

    /// Optical density of one stain for one pixel.
    ///
    /// `stain` is 0-based: 0 is haematoxylin for both default stain sets, 1 is
    /// eosin or DAB, 2 the residual.
    ///
    /// The result is **not** clamped at zero. QuPath's `deconvolve` does not
    /// clamp it either, and a stain amount is legitimately negative wherever
    /// the observed colour sits outside the cone the stain vectors span — which
    /// is most of the background on a real slide. Clamping here would flatten
    /// that to a plateau of exact zeros and change where the blur and the
    /// maximum finder downstream put their peaks.
    ///
    /// # Panics
    ///
    /// If `stain` is not 0, 1 or 2.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "QuPath accumulates in double and stores float; this is that cast"
    )]
    pub fn stain(&self, stain: usize, red: u8, green: u8, blue: u8) -> f32 {
        assert!(stain < 3, "stain index {stain} out of range");
        let row = &self.coefficients[stain];
        let density = [
            optical_density(red, self.max_values[0]),
            optical_density(green, self.max_values[1]),
            optical_density(blue, self.max_values[2]),
        ];
        (density[0] * row[0] + density[1] * row[1] + density[2] * row[2]) as f32
    }

    /// Haematoxylin optical density for one pixel — stain 0.
    #[must_use]
    pub fn haematoxylin(&self, red: u8, green: u8, blue: u8) -> f32 {
        self.stain(0, red, green, blue)
    }

    /// The deconvolution coefficients, `[stain][channel]`.
    ///
    /// The transpose of QuPath's `getMatrixInverse()`, per `invert`. Exposed
    /// so the parity test can compare the matrix itself: when a per-pixel
    /// comparison fails, this says whether the vectors, the inversion or the
    /// optical density is at fault, which the pixel values alone do not.
    #[must_use]
    pub fn coefficients(&self) -> [[f64; 3]; 3] {
        self.coefficients
    }

    /// The per-channel white point, QuPath's `maxValues`.
    ///
    /// Exposed for the same reason [`Self::coefficients`] is, and for one more:
    /// the viewer's display path duplicates this arithmetic rather than
    /// depending on this crate (`docs/dev/VIEWER.md` §7), so the numbers it copies
    /// have to be *readable* from here. A background typed as `[255.0; 3]`
    /// because that is what it usually is would be right for the default stains
    /// and silently wrong for a slide carrying its own.
    #[must_use]
    pub fn max_values(&self) -> [f64; 3] {
        self.max_values
    }

    /// Converts an RGB tile to a haematoxylin optical-density image.
    ///
    /// `out` is reused across tiles, so this allocates nothing in a tile loop.
    pub fn haematoxylin_into(&self, rgb: &[u8], out: &mut Vec<f32>) {
        self.stain_into(0, rgb, out);
    }

    /// Converts an RGB tile to one stain's optical-density image.
    ///
    /// Equivalent to [`Self::stain`] per pixel, and **bit-identical** to it:
    /// the table below holds the same products, formed by the same multiply,
    /// and they are summed in the same order. The A/B example asserts that.
    ///
    /// # Panics
    ///
    /// If `stain` is not 0, 1 or 2.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "QuPath accumulates in double and stores float; this is that cast"
    )]
    pub fn stain_into(&self, stain: usize, rgb: &[u8], out: &mut Vec<f32>) {
        let table = self.table(stain);
        out.clear();
        out.reserve(rgb.len() / 3);
        out.extend(rgb.chunks_exact(3).map(|pixel| {
            (table[0][pixel[0] as usize]
                + table[1][pixel[1] as usize]
                + table[2][pixel[2] as usize]) as f32
        }));
    }

    /// One stain's optical density per channel byte, coefficient folded in.
    ///
    /// The deconvolved value depends on the three channel bytes and nothing
    /// else, so all 256 densities per channel can be computed once and read
    /// back per pixel — which is what QuPath does too, in `makeODLUT`. That
    /// replaces three `log10` calls per pixel with three loads from 6 KB of
    /// L1-resident table.
    ///
    /// Folding the row's coefficient into the table as it is built removes the
    /// three multiplies as well, and is what keeps this bit-identical rather
    /// than merely close: `stain` computes `OD * c` and sums, and so does this,
    /// with the same values in the same order.
    fn table(&self, stain: usize) -> [[f64; 256]; 3] {
        assert!(stain < 3, "stain index {stain} out of range");
        let row = &self.coefficients[stain];
        std::array::from_fn(|channel| {
            let white = self.max_values[channel];
            let coefficient = row[channel];
            #[expect(
                clippy::cast_possible_truncation,
                reason = "index is 0..256 by construction, so the cast is exact"
            )]
            std::array::from_fn(|value| optical_density(value as u8, white) * coefficient)
        })
    }
}

/// `max(0, -log10(max(value, 1) / white))`, QuPath's `makeOD`.
///
/// Both clamps are QuPath's. The inner one keeps a black pixel finite, since
/// zero intensity would give an infinite density and poison every filter
/// downstream. The outer one only bites when `white` is below full scale, which
/// the defaults are not, but a slide carrying its own white point can be.
#[inline]
fn optical_density(value: u8, white: f64) -> f64 {
    let intensity = f64::from(value).max(1.0);
    (-(intensity / white).log10()).max(0.0)
}

fn normalise(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length <= 0.0 {
        return v;
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Builds the matrix that takes an optical-density vector to stain amounts.
///
/// `m` holds the stain vectors as rows. A pixel's optical density is the sum of
/// each stain's vector scaled by how much of that stain is present:
///
/// ```text
/// d = c0 * s0 + c1 * s1 + c2 * s2  =  Mᵀ c
/// ```
///
/// so recovering `c` needs `(Mᵀ)⁻¹`, which is `(M⁻¹)ᵀ` — **not** `M⁻¹`. Getting
/// that transpose wrong is not a visible error: it still produces a smooth
/// single-channel image that looks like a stain separation, but it projects
/// onto the wrong axis. Here it drove the haematoxylin channel to zero across
/// a whole tissue region, which the pipeline only noticed because it counts
/// what it found.
///
/// QuPath arrives at the same place from the other side: it stores the plain
/// `M⁻¹` and transposes at the point of use, reading `invMat[channel][stain]`.
/// The parity fixture records `M⁻¹` as QuPath returns it, so the test below
/// transposes it back to compare.
///
/// Falls back to the identity if the matrix is singular, which for normalised
/// stain vectors means two stains coincide.
fn invert(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);

    if determinant.abs() < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }

    let mut result = [[0.0f64; 3]; 3];
    for (row, out_row) in result.iter_mut().enumerate() {
        for (column, out) in out_row.iter_mut().enumerate() {
            // Cofactor of (row, column), which is the transpose of the adjugate
            // and therefore exactly the (Mᵀ)⁻¹ wanted above.
            let (r0, r1) = ((row + 1) % 3, (row + 2) % 3);
            let (c0, c1) = ((column + 1) % 3, (column + 2) % 3);
            *out = (m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]) / determinant;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_has_no_stain() {
        // Optical density of white is zero, so every stain reads zero.
        let stains = Stains::h_and_e();
        assert!(stains.haematoxylin(255, 255, 255).abs() < 1e-6);
    }

    /// Applies the deconvolution to a raw optical-density vector, skipping the
    /// 8-bit round trip so the test measures the matrix and not quantisation.
    fn amounts(stains: &Stains, density: [f64; 3]) -> [f64; 3] {
        let mut out = [0.0f64; 3];
        for (row, value) in stains.coefficients.iter().zip(out.iter_mut()) {
            *value = density[0] * row[0] + density[1] * row[1] + density[2] * row[2];
        }
        out
    }

    #[test]
    fn a_pure_stain_recovers_as_that_stain_alone() {
        // The defining property: optical density that is exactly 0.7 units of
        // haematoxylin must come back as 0.7 haematoxylin and nothing else.
        // This is what fails when the inverse is not transposed — and it fails
        // quietly, still producing a plausible-looking single-channel image.
        for (stains, second) in [(Stains::h_and_e(), EOSIN), (Stains::h_dab(), DAB)] {
            for (index, vector) in [HAEMATOXYLIN, second].into_iter().enumerate() {
                let unit = normalise(vector);
                let amount = 0.7f64;
                let density = [unit[0] * amount, unit[1] * amount, unit[2] * amount];

                let recovered = amounts(&stains, density);
                assert!(
                    (recovered[index] - amount).abs() < 1e-9,
                    "stain {index}: expected {amount}, got {recovered:?}"
                );
                for (other, &value) in recovered.iter().enumerate() {
                    if other != index {
                        assert!(
                            value.abs() < 1e-9,
                            "stain {index} leaked {value} into channel {other}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_black_pixel_stays_finite() {
        // Zero intensity would give an infinite optical density and poison
        // every filter downstream; QuPath clamps, and so does this.
        let stains = Stains::h_and_e();
        assert!(stains.haematoxylin(0, 0, 0).is_finite());
    }

    #[test]
    fn the_table_agrees_with_the_scalar_path_exactly() {
        // `stain_into` claims to be bit-identical to `stain`, not merely close,
        // and the parity fixture only exercises the scalar path — so without
        // this the table could drift from what QuPath was compared against.
        //
        // Every one of the 16.7 million distinct pixel values would be the
        // exhaustive check; this sweeps each channel across its whole range
        // against varied values in the other two, which covers every table
        // entry many times over.
        let mut rgb = Vec::new();
        for value in 0..=255u8 {
            for other in [0u8, 1, 37, 128, 199, 254, 255] {
                rgb.extend_from_slice(&[value, other, other.wrapping_add(53)]);
                rgb.extend_from_slice(&[other, value, other.wrapping_add(53)]);
                rgb.extend_from_slice(&[other, other.wrapping_add(53), value]);
            }
        }

        for stains in [Stains::h_and_e(), Stains::h_dab()] {
            for stain in 0..3 {
                let mut out = Vec::new();
                stains.stain_into(stain, &rgb, &mut out);
                assert_eq!(out.len(), rgb.len() / 3);

                for (pixel, expected) in rgb.chunks_exact(3).zip(&out) {
                    let scalar = stains.stain(stain, pixel[0], pixel[1], pixel[2]);
                    assert_eq!(
                        scalar.to_bits(),
                        expected.to_bits(),
                        "stain {stain} at rgb({}, {}, {}): table gave {expected}, \
                         scalar gave {scalar}",
                        pixel[0],
                        pixel[1],
                        pixel[2]
                    );
                }
            }
        }
    }

    #[test]
    fn h_and_e_is_not_h_dab() {
        // These differ only in the second vector, and confusing the two is the
        // bug the parity fixture caught. A pixel exists that separates them.
        let (h_e, h_dab) = (Stains::h_and_e(), Stains::h_dab());
        assert!(
            (h_e.haematoxylin(120, 90, 160) - h_dab.haematoxylin(120, 90, 160)).abs() > 1e-3,
            "H&E and H-DAB gave the same haematoxylin value"
        );
    }
}
