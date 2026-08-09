//! Colour deconvolution: RGB to stain optical density.
//!
//! Cell detection does not run on RGB. QuPath's `WatershedCellDetection`
//! separates the haematoxylin and eosin contributions first and detects nuclei
//! on the haematoxylin channel, because nuclei are what haematoxylin stains.
//!
//! The transform is Ruifrok and Johnston's: convert each channel to optical
//! density, `OD = -log10(I / 255)`, then project the OD vector onto the inverse
//! of the stain matrix. The default H&E vectors below are QuPath's own
//! (`ColorDeconvolutionStains`), so a slide with no stain estimate stored uses
//! the same numbers on both sides.
//!
//! **Not yet parity-checked.** No fixture pins this against QuPath's
//! `ColorDeconvolution` output. The vectors and the formula come from QuPath's
//! source, and the arithmetic is straightforward, but "read from the source"
//! is not the same as "recorded from a run" — which is the standard the rest of
//! this repository holds itself to. `docs/ROADMAP.md` carries the fixture as a
//! prerequisite for any detection-parity claim.

/// A stain matrix and its inverse, ready to project optical density.
#[derive(Debug, Clone, Copy)]
pub struct Stains {
    /// Row-major inverse of the normalised stain matrix.
    inverse: [[f32; 3]; 3],
}

/// QuPath's default haematoxylin vector for H&E.
const HAEMATOXYLIN: [f64; 3] = [0.651, 0.701, 0.290];
/// QuPath's default eosin vector for H&E.
const EOSIN: [f64; 3] = [0.269, 0.568, 0.778];

impl Default for Stains {
    fn default() -> Self {
        Self::h_and_e()
    }
}

impl Stains {
    /// The default H&E stains, with the third vector taken as the cross product
    /// of the first two — the "residual" QuPath uses when only two are known.
    #[must_use]
    pub fn h_and_e() -> Self {
        let first = normalise(HAEMATOXYLIN);
        let second = normalise(EOSIN);
        let third = normalise(cross(first, second));
        Self {
            inverse: invert([first, second, third]),
        }
    }

    /// Haematoxylin optical density for one pixel.
    #[must_use]
    pub fn haematoxylin(&self, red: u8, green: u8, blue: u8) -> f32 {
        let od = [
            optical_density(red),
            optical_density(green),
            optical_density(blue),
        ];
        let row = &self.inverse[0];
        (od[0] * row[0] + od[1] * row[1] + od[2] * row[2]).max(0.0)
    }

    /// Converts an RGB tile to a haematoxylin optical-density image.
    ///
    /// `out` is reused across tiles, so this allocates nothing in a tile loop.
    pub fn haematoxylin_into(&self, rgb: &[u8], out: &mut Vec<f32>) {
        out.clear();
        out.reserve(rgb.len() / 3);
        out.extend(
            rgb.chunks_exact(3)
                .map(|pixel| self.haematoxylin(pixel[0], pixel[1], pixel[2])),
        );
    }
}

/// `-log10(intensity / 255)`, clamped so that a black pixel stays finite.
///
/// QuPath clamps the intensity to at least 1 before the logarithm; without it a
/// zero-valued channel gives infinity and poisons everything downstream.
#[inline]
fn optical_density(value: u8) -> f32 {
    let intensity = f32::from(value).max(1.0);
    -(intensity / 255.0).log10()
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
/// Falls back to the identity if the matrix is singular, which for normalised
/// stain vectors means two stains coincide.
#[expect(
    clippy::cast_possible_truncation,
    reason = "stain vectors are small constants; f32 is the pixel working type"
)]
fn invert(m: [[f64; 3]; 3]) -> [[f32; 3]; 3] {
    let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);

    if determinant.abs() < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }

    let mut result = [[0.0f32; 3]; 3];
    for (row, out_row) in result.iter_mut().enumerate() {
        for (column, out) in out_row.iter_mut().enumerate() {
            // Cofactor of (row, column), which is the transpose of the adjugate
            // and therefore exactly the (Mᵀ)⁻¹ wanted above.
            let (r0, r1) = ((row + 1) % 3, (row + 2) % 3);
            let (c0, c1) = ((column + 1) % 3, (column + 2) % 3);
            let cofactor = m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
            *out = (cofactor / determinant) as f32;
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
    fn amounts(stains: &Stains, density: [f32; 3]) -> [f32; 3] {
        let mut out = [0.0f32; 3];
        for (row, value) in stains.inverse.iter().zip(out.iter_mut()) {
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
        let stains = Stains::h_and_e();

        for (index, vector) in [HAEMATOXYLIN, EOSIN].into_iter().enumerate() {
            let unit = normalise(vector);
            let amount = 0.7f32;
            #[expect(clippy::cast_possible_truncation, reason = "test constants")]
            let density = [
                unit[0] as f32 * amount,
                unit[1] as f32 * amount,
                unit[2] as f32 * amount,
            ];

            let recovered = amounts(&stains, density);
            assert!(
                (recovered[index] - amount).abs() < 1e-4,
                "stain {index}: expected {amount}, got {recovered:?}"
            );
            for (other, &value) in recovered.iter().enumerate() {
                if other != index {
                    assert!(
                        value.abs() < 1e-4,
                        "stain {index} leaked {value} into channel {other}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_haematoxylin_colour_reads_stronger_than_the_eosin_colour() {
        // The same property seen through the 8-bit path the pipeline uses.
        let stains = Stains::h_and_e();

        let as_rgb = |vector: [f64; 3]| {
            let channel = |v: f64| {
                #[expect(clippy::cast_possible_truncation, reason = "clamped to 0..=255")]
                let byte = (255.0 * 10f64.powf(-v)).clamp(0.0, 255.0) as u8;
                byte
            };
            (channel(vector[0]), channel(vector[1]), channel(vector[2]))
        };

        let (hr, hg, hb) = as_rgb(normalise(HAEMATOXYLIN));
        let (er, eg, eb) = as_rgb(normalise(EOSIN));

        let on_haem = stains.haematoxylin(hr, hg, hb);
        let on_eosin = stains.haematoxylin(er, eg, eb);
        assert!(
            on_haem > 0.5 && on_haem > on_eosin * 4.0,
            "haematoxylin pixel gave {on_haem}, eosin pixel {on_eosin}"
        );
    }

    #[test]
    fn a_black_pixel_stays_finite() {
        // Zero intensity would give an infinite optical density and poison
        // every filter downstream; QuPath clamps, and so does this.
        let stains = Stains::h_and_e();
        assert!(stains.haematoxylin(0, 0, 0).is_finite());
    }
}
