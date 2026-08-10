//! ImageJ's `Convolver`, as `WatershedCellDetection` applies it to complete the
//! Laplacian of Gaussian.
//!
//! Detection blurs the deconvolved optical density and then takes its discrete
//! Laplacian:
//!
//! ```text
//! fpLoG.blurGaussian(sigma);
//! fpLoG.convolve(new float[]{0, -1, 0, -1, 4, -1, 0, -1, 0}, 3, 3);
//! ```
//!
//! `FloatProcessor.convolve` snapshots the pixels and hands them to a *fresh*
//! `Convolver`, so `normalize` sits at its field default of `true`. Neither fact
//! is visible at the call site and both decide the answer.
//!
//! # Three things a nine-tap stencil does not tell you
//!
//! `parity/fixtures/LaplacianConvolution.json` pins each of them.
//!
//! - **The Laplacian is the kernel normalisation skips.** [`kernel_scale`]
//!   divides by the kernel sum — except that `{0,-1,0,-1,4,-1,0,-1,0}` sums to
//!   exactly zero, and ImageJ leaves the scale at 1.0 rather than dividing. A
//!   port that always normalises divides by zero here; one that never
//!   normalises passes this kernel and fails every other. The fixture records
//!   both branches.
//! - **It correlates rather than convolves.** The kernel index advances with
//!   the tap offset and is never flipped, so `kernel[0]` meets the *top-left*
//!   neighbour. The Laplacian is symmetric and cannot show this, which is why
//!   the fixture carries an asymmetric kernel that can.
//! - **Each tap of an edge pixel is clamped individually.** This is not the
//!   blur's convention two files away, which extends the image by folding a
//!   tail sum into the kernel; here the coordinate itself is clamped into the
//!   image, per tap.
//!
//! # Where the arithmetic rounds
//!
//! ImageJ accumulates into a `double`, but the product it accumulates is
//! `float * float` — Java evaluates that in `f32` and rounds *before* widening
//! it. So the port must round each product to `f32` too, and narrow only once
//! at the store:
//!
//! ```text
//! sum += pixels2[offset + u] * kernel[i++];   // f32 product, f64 accumulator
//! pixels[x + y * width] = (float) (sum * scale);
//! ```
//!
//! Accumulating in full `f64` — the reading a port naturally reaches for — is a
//! different answer. It happens not to matter for the Laplacian, whose taps are
//! `0`, `±1` and `4` and whose products are therefore exact, but it matters for
//! every other kernel and the fixture carries three that show it.
//!
//! # Determinism
//!
//! Output rows are independent and each output pixel is one accumulation in a
//! fixed tap order, so the result cannot depend on how the rows are divided.
//! §5 asks for that to be tested rather than asserted, and
//! `convolution_is_independent_of_thread_count` does.

use rayon::prelude::*;

/// Output pixels carried at once through the interior of a row.
///
/// Matches the blur's lane count and for the same reason: enough independent
/// accumulators to hide the latency of an `f64` add, few enough that they stay
/// in registers. `docs/benchmarks/README.md` records what it was worth.
const LANES: usize = 8;

/// ImageJ's `Convolver.getScale`: the reciprocal of the kernel sum, or 1.0.
///
/// The zero-sum case is the one detection takes. ImageJ guards the division
/// with `if (sum != 0.0)` and otherwise leaves the scale at 1.0, so the
/// Laplacian is applied unscaled rather than producing infinities.
///
/// The sum accumulates in `f64` over `f32` coefficients, which widen exactly.
#[must_use]
pub fn kernel_scale(kernel: &[f32]) -> f64 {
    let sum: f64 = kernel.iter().map(|&k| f64::from(k)).sum();
    if sum == 0.0 { 1.0 } else { 1.0 / sum }
}

/// Convolves `pixels` in place with `kernel`, matching `FloatProcessor.convolve`.
///
/// `pixels` is row-major with the given `width`. Reads outside the image clamp
/// to the nearest edge pixel, per tap.
///
/// This allocates a copy of `pixels` on every call, because the convolution
/// reads a neighbourhood and cannot overwrite what it is still reading. Callers
/// filtering many tiles should hold a [`Convolver`], which does not.
///
/// # Panics
///
/// Panics if `width` is zero or does not divide `pixels.len()`, if either
/// kernel dimension is even, or if `kernel.len()` is not
/// `kernel_width * kernel_height`.
pub fn convolve(
    pixels: &mut [f32],
    width: usize,
    kernel: &[f32],
    kernel_width: usize,
    kernel_height: usize,
) {
    Convolver::new().apply(pixels, width, kernel, kernel_width, kernel_height);
}

/// A convolver that owns its scratch space, for repeated application.
///
/// In-place convolution needs an untouched copy of the source to read while the
/// output is written over the original; holding that copy here means a tile loop
/// allocates once rather than once per tile. [`Convolver::apply_into`] avoids
/// the copy entirely by writing to a separate output, which is what the pipeline
/// does.
#[derive(Debug)]
pub struct Convolver {
    /// Holds the caller's pixels during an in-place [`Convolver::apply`].
    /// Unused by [`Convolver::apply_into`].
    staging: Vec<f32>,
    /// Whether to spread rows across rayon. See
    /// [`Convolver::with_parallel_rows`].
    parallel_rows: bool,
}

impl Default for Convolver {
    /// Written out rather than derived, because a derived `bool` default would
    /// silently make the convolution sequential.
    fn default() -> Self {
        Self {
            staging: Vec::new(),
            parallel_rows: true,
        }
    }
}

impl Convolver {
    /// Creates a convolver with no scratch space yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Chooses whether rows are spread across rayon, which they are by default.
    ///
    /// A caller already inside a rayon job should turn this off; see
    /// [`crate::RankFilter::with_parallel_rows`] for why nesting is
    /// both a §6 violation and slower here.
    #[must_use]
    pub fn with_parallel_rows(mut self, parallel: bool) -> Self {
        self.parallel_rows = parallel;
        self
    }

    /// Convolves `pixels` in place, reusing this convolver's buffer.
    ///
    /// Semantics are identical to [`convolve`]; only the allocation differs.
    ///
    /// # Panics
    ///
    /// As [`convolve`].
    pub fn apply(
        &mut self,
        pixels: &mut [f32],
        width: usize,
        kernel: &[f32],
        kernel_width: usize,
        kernel_height: usize,
    ) {
        self.staging.clear();
        self.staging.extend_from_slice(pixels);
        let staging = std::mem::take(&mut self.staging);
        apply_into(
            &staging,
            pixels,
            width,
            kernel,
            kernel_width,
            kernel_height,
            true,
            self.parallel_rows,
        );
        self.staging = staging;
    }

    /// Convolves `source` into `out`, allocating and copying nothing.
    ///
    /// `source` and `out` must be the same length and must not be the same
    /// buffer; use [`Convolver::apply`] to convolve in place.
    ///
    /// # Panics
    ///
    /// As [`convolve`], and additionally if `source` and `out` differ in length.
    pub fn apply_into(
        &mut self,
        source: &[f32],
        out: &mut [f32],
        width: usize,
        kernel: &[f32],
        kernel_width: usize,
        kernel_height: usize,
    ) {
        apply_into(
            source,
            out,
            width,
            kernel,
            kernel_width,
            kernel_height,
            true,
            self.parallel_rows,
        );
    }

    /// As [`Convolver::apply_into`], with one output pixel in flight at a time.
    ///
    /// The implementation this crate had before the interior loop was widened,
    /// retained only so the A/B harness measures against the real previous
    /// version rather than a reimplementation of it. Bit-identical to
    /// [`Self::apply_into`] — `widening_the_interior_loop_changes_nothing`
    /// asserts it, and the harness asserts it again on every timed iteration.
    ///
    /// # Panics
    ///
    /// As [`Convolver::apply_into`].
    #[doc(hidden)]
    pub fn apply_into_narrow(
        &mut self,
        source: &[f32],
        out: &mut [f32],
        width: usize,
        kernel: &[f32],
        kernel_width: usize,
        kernel_height: usize,
    ) {
        apply_into(
            source,
            out,
            width,
            kernel,
            kernel_width,
            kernel_height,
            false,
            self.parallel_rows,
        );
    }
}

/// The convolution itself. Free function so both entry points share one body.
#[expect(
    clippy::too_many_arguments,
    reason = "the shared body of three public entry points, each of which fixes \
              one of the trailing flags"
)]
fn apply_into(
    source: &[f32],
    out: &mut [f32],
    width: usize,
    kernel: &[f32],
    kernel_width: usize,
    kernel_height: usize,
    wide: bool,
    parallel_rows: bool,
) {
    assert!(width > 0, "width must be non-zero");
    assert_eq!(
        source.len(),
        out.len(),
        "source and output must describe the same image"
    );
    assert_eq!(
        source.len() % width,
        0,
        "width must divide the buffer length"
    );
    // ImageJ throws on an even kernel dimension rather than rounding one down.
    assert!(
        kernel_width % 2 == 1 && kernel_height % 2 == 1,
        "kernel width and height must be odd, got {kernel_width}x{kernel_height}"
    );
    assert_eq!(
        kernel.len(),
        kernel_width * kernel_height,
        "kernel length must be its width times its height"
    );

    let height = source.len() / width;
    if height == 0 {
        return;
    }

    let scan = Scan {
        source,
        width,
        height,
        kernel,
        half_width: kernel_width / 2,
        half_height: kernel_height / 2,
        scale: kernel_scale(kernel),
        wide,
    };

    // Rows are independent: each reads the untouched source and writes only its
    // own output row. Granularity is a row, not a pixel, per §6. No accumulation
    // crosses rows, so the split cannot change the result.
    if parallel_rows {
        out.par_chunks_mut(width)
            .enumerate()
            .for_each(|(y, out)| scan.row(out, y));
    } else {
        out.chunks_mut(width)
            .enumerate()
            .for_each(|(y, out)| scan.row(out, y));
    }
}

/// The image geometry and kernel one row of the convolution works against.
struct Scan<'a> {
    source: &'a [f32],
    width: usize,
    height: usize,
    kernel: &'a [f32],
    /// ImageJ's `uc`: taps either side of centre, horizontally.
    half_width: usize,
    /// ImageJ's `vc`: taps either side of centre, vertically.
    half_height: usize,
    /// `1 / sum(kernel)`, or 1.0 when the sum is zero. See [`kernel_scale`].
    scale: f64,
    /// Whether the interior carries [`LANES`] output pixels at once. False only
    /// for the A/B harness, which measures against the previous version.
    wide: bool,
}

impl Scan<'_> {
    /// Convolves the single output row `y`.
    ///
    /// ImageJ tests `edgePixel` per pixel and branches inside the tap loop. The
    /// same split is done once per row here: a row is either entirely edge — in
    /// which case every tap needs clamping — or it has an interior span whose
    /// taps are all in bounds, with a clamped margin either side.
    fn row(&self, out: &mut [f32], y: usize) {
        // ImageJ: xedge = width - uc, yedge = height - vc.
        let x_edge = self.width.saturating_sub(self.half_width);
        let y_edge = self.height.saturating_sub(self.half_height);

        if y < self.half_height || y >= y_edge {
            for (x, value) in out.iter_mut().enumerate() {
                *value = self.clamped(x, y);
            }
            return;
        }

        // `start..end` is exactly the negation of ImageJ's
        // `x < uc || x >= xedge`, clamped into the row. Both ends need the
        // clamp: on an image narrower than the kernel, `half_width` can exceed
        // the width outright and `x_edge` falls at or below it, which leaves the
        // interior empty — the intended answer, since every pixel is then an
        // edge pixel.
        let start = self.half_width.min(self.width);
        let end = x_edge.max(start);

        for (x, value) in out[..start].iter_mut().enumerate() {
            *value = self.clamped(x, y);
        }
        if self.wide {
            if end > start {
                self.interior_span(&mut out[start..end], start, y);
            }
        } else {
            for (offset, value) in out[start..end].iter_mut().enumerate() {
                *value = self.interior(start + offset, y);
            }
        }
        for (offset, value) in out[end..].iter_mut().enumerate() {
            *value = self.clamped(end + offset, y);
        }
    }

    /// The interior of one row, [`LANES`] output pixels at a time.
    ///
    /// Each lane keeps its own accumulator and receives the taps in the order a
    /// single accumulator would give that pixel, so the result is
    /// **bit-identical** to [`Scan::interior`] — this widens the loop, it does
    /// not reassociate the sum, which would not be allowed.
    ///
    /// The reason it is worth doing is the one the blur hit: `sum +=` is a
    /// loop-carried dependency, so a single accumulator runs the taps at the
    /// latency of an `f64` add rather than its throughput. Nine taps chained
    /// that way cost more than the separable 29-tap blur before it. Independent
    /// lanes read adjacent taps, so the independence costs no extra loads.
    fn interior_span(&self, out: &mut [f32], x_start: usize, y: usize) {
        let span = 2 * self.half_width + 1;
        let len = out.len();
        let mut chunks = out.chunks_exact_mut(LANES);

        for (chunk, index) in chunks.by_ref().zip(0..) {
            let x0 = x_start + index * LANES;
            let mut acc = [0.0f64; LANES];
            let mut taps = self.kernel.iter();

            for v in 0..=2 * self.half_height {
                // ImageJ: offset = x + (y + v) * width, with v running from -vc.
                let row = (y + v - self.half_height) * self.width;
                for u in 0..span {
                    let coefficient = *taps.next().expect("kernel length checked on entry");
                    let start = row + x0 + u - self.half_width;
                    let source = &self.source[start..start + LANES];
                    // Fixed-length and contiguous, so this is one vector
                    // multiply-add per lane group with no bounds checks.
                    for (sum, &pixel) in acc.iter_mut().zip(source) {
                        *sum += f64::from(pixel * coefficient);
                    }
                }
            }

            for (value, &sum) in chunk.iter_mut().zip(acc.iter()) {
                *value = self.narrow(sum);
            }
        }

        // The tail, one pixel at a time. Fewer than LANES of them per row.
        let remainder = chunks.into_remainder();
        let tail_start = x_start + len - remainder.len();
        for (offset, value) in remainder.iter_mut().enumerate() {
            *value = self.interior(tail_start + offset, y);
        }
    }

    /// One output pixel whose every tap is inside the image.
    fn interior(&self, x: usize, y: usize) -> f32 {
        let mut sum = 0.0f64;
        let mut taps = self.kernel.iter();
        for v in 0..=2 * self.half_height {
            // ImageJ: offset = x + (y + v) * width, with v running from -vc.
            let row = (y + v - self.half_height) * self.width;
            let start = row + x - self.half_width;
            let span = &self.source[start..start + 2 * self.half_width + 1];
            for &pixel in span {
                let coefficient = *taps.next().expect("kernel length checked on entry");
                // The product is f32 in Java and rounds before it widens.
                sum += f64::from(pixel * coefficient);
            }
        }
        self.narrow(sum)
    }

    /// One output pixel with at least one tap outside the image.
    fn clamped(&self, x: usize, y: usize) -> f32 {
        let mut sum = 0.0f64;
        let mut taps = self.kernel.iter();
        for v in 0..=2 * self.half_height {
            let sy = self.clamp_y(y as isize + v as isize - self.half_height as isize);
            for u in 0..=2 * self.half_width {
                let sx = self.clamp_x(x as isize + u as isize - self.half_width as isize);
                let coefficient = *taps.next().expect("kernel length checked on entry");
                sum += f64::from(self.source[sy * self.width + sx] * coefficient);
            }
        }
        self.narrow(sum)
    }

    /// ImageJ's `getPixel` clamp: into the image, per coordinate.
    fn clamp_x(&self, x: isize) -> usize {
        x.clamp(0, self.width as isize - 1) as usize
    }

    /// As [`Scan::clamp_x`], vertically.
    fn clamp_y(&self, y: isize) -> usize {
        y.clamp(0, self.height as isize - 1) as usize
    }

    /// Scales and narrows once, as ImageJ's `(float)(sum * scale)` does.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "narrowing to f32 once at the store is what ImageJ does"
    )]
    fn narrow(&self, sum: f64) -> f32 {
        (sum * self.scale) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel `WatershedCellDetection` passes.
    const LAPLACIAN: [f32; 9] = [0.0, -1.0, 0.0, -1.0, 4.0, -1.0, 0.0, -1.0, 0.0];

    #[test]
    fn a_zero_sum_kernel_is_not_normalised() {
        // The branch detection takes. Dividing by the sum here would be a
        // division by zero, and the guard is the only thing preventing it.
        assert_eq!(kernel_scale(&LAPLACIAN), 1.0);
        assert!(kernel_scale(&LAPLACIAN).is_finite());
    }

    #[test]
    fn a_summing_kernel_is_normalised_by_its_sum() {
        assert_eq!(kernel_scale(&[1.0; 9]), 1.0 / 9.0);
        assert_eq!(
            kernel_scale(&[0.0, -1.0, 0.0, -1.0, 5.0, -1.0, 0.0, -1.0, 0.0]),
            1.0
        );
    }

    #[test]
    fn a_constant_image_is_annihilated_including_its_edges() {
        // Only true if the edge taps clamp to the same constant. An edge rule
        // that reads zero outside would leave a bright border.
        let mut pixels = vec![0.75f32; 12 * 10];
        convolve(&mut pixels, 12, &LAPLACIAN, 3, 3);
        assert!(
            pixels.iter().all(|&v| v == 0.0),
            "a zero-sum kernel must annihilate a constant image everywhere"
        );
    }

    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the scale is compared at f32 precision, as the output is"
    )]
    fn an_impulse_returns_the_kernel_unflipped() {
        // Correlation, not convolution: kernel[0] meets the top-left neighbour.
        // An asymmetric kernel is the only way to see the difference.
        let kernel: [f32; 9] = [1.0, 2.0, 3.0, -1.0, 4.0, -2.0, 0.0, 5.0, -2.0];
        let width = 7;
        let mut pixels = vec![0.0f32; width * 7];
        pixels[3 * width + 3] = 1.0;
        convolve(&mut pixels, width, &kernel, 3, 3);

        // The impulse at (3,3) contributes kernel[i] to the pixel it is read
        // from, so the output around (3,3) is the kernel reversed in both axes.
        let scale = kernel_scale(&kernel) as f32;
        for dy in 0..3usize {
            for dx in 0..3usize {
                let out = pixels[(2 + dy) * width + 2 + dx];
                let expected = kernel[(2 - dy) * 3 + (2 - dx)] * scale;
                assert!(
                    (out - expected).abs() < 1e-6,
                    "at ({dx},{dy}): {out} != {expected}"
                );
            }
        }
    }

    /// The definition, applied literally, with clamped reads and ImageJ's
    /// rounding. Slow, obviously correct, independent of the row splitting and
    /// the interior fast path.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "narrowing to f32 once at the store is what ImageJ does"
    )]
    fn reference(pixels: &[f32], width: usize, kernel: &[f32], kw: usize, kh: usize) -> Vec<f32> {
        let height = pixels.len() / width;
        let (uc, vc) = (kw as isize / 2, kh as isize / 2);
        let scale = kernel_scale(kernel);
        let mut out = vec![0.0f32; pixels.len()];

        for y in 0..height as isize {
            for x in 0..width as isize {
                let mut sum = 0.0f64;
                let mut i = 0;
                for v in -vc..=vc {
                    for u in -uc..=uc {
                        let sx = (x + u).clamp(0, width as isize - 1) as usize;
                        let sy = (y + v).clamp(0, height as isize - 1) as usize;
                        sum += f64::from(pixels[sy * width + sx] * kernel[i]);
                        i += 1;
                    }
                }
                out[y as usize * width + x as usize] = (sum * scale) as f32;
            }
        }
        out
    }

    #[test]
    fn matches_the_literal_definition_over_many_shapes() {
        // The interior fast path, the row split and the edge clamp all have to
        // agree with the definition at once. Images narrower and shorter than
        // the kernel are where the interior span goes empty, which is where an
        // off-by-one in the split survives every larger case.
        let kernels: [(&[f32], usize, usize); 4] = [
            (&LAPLACIAN, 3, 3),
            (&[1.0; 9], 3, 3),
            (&[1.0, 2.0, 3.0, -1.0, 4.0, -2.0, 0.0, 5.0, -2.0], 3, 3),
            (
                &[
                    1.0, 0.0, -1.0, 0.0, 2.0, 0.0, -2.0, 0.0, 1.0, 0.0, 3.0, 0.0, -1.0, 0.0, -3.0,
                ],
                5,
                3,
            ),
        ];

        for &(width, height) in &[(1, 1), (1, 9), (9, 1), (2, 2), (3, 6), (13, 7), (32, 32)] {
            let pixels: Vec<f32> = (0..width * height)
                .map(|i| ((i * 37) % 101) as f32 / 40.0 - 0.5)
                .collect();
            for &(kernel, kw, kh) in &kernels {
                let mut actual = pixels.clone();
                convolve(&mut actual, width, kernel, kw, kh);
                assert_eq!(
                    actual,
                    reference(&pixels, width, kernel, kw, kh),
                    "{kw}x{kh} kernel on {width}x{height}"
                );
            }
        }
    }

    #[test]
    fn widening_the_interior_loop_changes_nothing() {
        // The lanes exist to break the accumulator's dependency chain, not to
        // reassociate the sum. Each lane must receive the taps in the same order
        // a single accumulator would, so the two paths are bit-identical rather
        // than merely close — an assertion the parity fixture then repeats
        // against ImageJ.
        //
        // Widths are chosen around LANES so that the chunked span, its
        // remainder, and an interior shorter than one chunk are all exercised:
        // an interior of 1..width-1 has width-2 pixels, so width 10 divides
        // evenly and the others do not.
        let kernels: [(&[f32], usize, usize); 3] = [
            (&LAPLACIAN, 3, 3),
            (&[1.0, 2.0, 3.0, -1.0, 4.0, -2.0, 0.0, 5.0, -2.0], 3, 3),
            (
                &[
                    1.0, 0.0, -1.0, 0.0, 2.0, 0.0, -2.0, 0.0, 1.0, 0.0, 3.0, 0.0, -1.0, 0.0, -3.0,
                ],
                5,
                3,
            ),
        ];

        for width in [3usize, 5, 7, 9, 10, 11, 16, 17, 23, 64] {
            let pixels: Vec<f32> = (0..width * 12)
                .map(|i| ((i * 37) % 101) as f32 / 40.0 - 0.5)
                .collect();
            for &(kernel, kw, kh) in &kernels {
                let mut wide = pixels.clone();
                convolve(&mut wide, width, kernel, kw, kh);

                let mut narrow = vec![0.0f32; pixels.len()];
                Convolver::new().apply_into_narrow(&pixels, &mut narrow, width, kernel, kw, kh);

                assert_eq!(wide, narrow, "{kw}x{kh} kernel on width {width}");
            }
        }
    }

    #[test]
    fn convolution_is_independent_of_thread_count() {
        // §5 requires parallel work to be reproducible. Each output pixel is one
        // accumulation in a fixed tap order and rows never interact, so the
        // result must be bit-identical however rayon splits the work.
        let width = 64;
        let source: Vec<f32> = (0..width * 64).map(|i| ((i * 31) % 97) as f32).collect();

        let mut expected = source.clone();
        convolve(&mut expected, width, &LAPLACIAN, 3, 3);

        for threads in [1usize, 2, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("thread pool");
            let mut pixels = source.clone();
            pool.install(|| convolve(&mut pixels, width, &LAPLACIAN, 3, 3));
            assert_eq!(pixels, expected, "diverged with {threads} threads");
        }
    }

    #[test]
    fn convolving_into_a_buffer_matches_convolving_in_place() {
        let width = 24;
        let source: Vec<f32> = (0..width * 20).map(|i| ((i * 29) % 83) as f32).collect();
        let mut convolver = Convolver::new();

        let mut in_place = source.clone();
        convolver.apply(&mut in_place, width, &LAPLACIAN, 3, 3);

        let mut out = vec![0.0f32; source.len()];
        convolver.apply_into(&source, &mut out, width, &LAPLACIAN, 3, 3);

        assert_eq!(out, in_place);
    }

    #[test]
    fn a_reused_convolver_matches_a_fresh_one() {
        // The point of `Convolver` is that the buffer survives between calls;
        // stale contents leaking into a later result would be the failure mode.
        let width = 16;
        let mut convolver = Convolver::new();
        for len in [width * 16, width * 4, width * 16] {
            let source: Vec<f32> = (0..len).map(|i| ((i * 13) % 71) as f32).collect();

            let mut reused = source.clone();
            convolver.apply(&mut reused, width, &LAPLACIAN, 3, 3);

            let mut fresh = source.clone();
            convolve(&mut fresh, width, &LAPLACIAN, 3, 3);

            assert_eq!(reused, fresh, "{len} pixels");
        }
    }

    #[test]
    #[should_panic(expected = "must be odd")]
    fn an_even_kernel_dimension_panics() {
        let mut pixels = vec![0.0f32; 16];
        convolve(&mut pixels, 4, &[1.0; 4], 2, 2);
    }

    #[test]
    #[should_panic(expected = "kernel length")]
    fn a_mismatched_kernel_length_panics() {
        let mut pixels = vec![0.0f32; 16];
        convolve(&mut pixels, 4, &[1.0; 8], 3, 3);
    }
}
