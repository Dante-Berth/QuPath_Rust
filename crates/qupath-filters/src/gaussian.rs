//! ImageJ's separable Gaussian blur, as `WatershedCellDetection` applies it.
//!
//! Detection blurs the deconvolved optical density before taking its Laplacian
//! and finding maxima:
//!
//! ```text
//! fpLoG.blurGaussian(sigma);
//! fpLoG.convolve(new float[]{0, -1, 0, -1, 4, -1, 0, -1, 0}, 3, 3);
//! ```
//!
//! Without it the maxima are noise-driven, which is part of why the pipeline's
//! marker count is an order of magnitude above QuPath's cell count.
//!
//! # This is not a textbook Gaussian
//!
//! Four things about ImageJ's kernel are not derivable from the formula, and
//! `parity/fixtures/GaussianBlur.json` pins each of them:
//!
//! - **The accuracy is 0.0002**, not the 0.002 sitting in the same method.
//!   `blurGaussian(ip, sigma)` picks between them by processor type, and a
//!   `FloatProcessor` — which is what detection blurs — takes the tighter one.
//!   The looser value shortens the kernel by about a fifth.
//! - **The tail is a parabola, not a Gaussian.** Truncating a Gaussian leaves a
//!   step at the cutoff, so past a break point ImageJ replaces the values with
//!   `(kRadius - r)² · slope²`, which reaches zero exactly at the first
//!   out-of-kernel sample and so has a continuous first derivative.
//! - **The break point is searched for, not computed.** It scans inwards from
//!   the edge for the smallest `sqrt(kernel[r]) / (kRadius - r)` and stops at
//!   the first value that fails to improve, then starts the fit two samples
//!   further out.
//! - **Edges extend by replication, applied through the tail sum.** Rather than
//!   clamping the index per tap, [`convolve_line`] adds `kernSum[i]` — the
//!   kernel mass beyond sample `i`, *including* the part the truncation
//!   dropped — multiplied by the edge value. That also folds the truncation
//!   error back in, so the kernel behaves as though it were infinite.
//!
//! # Large sigma
//!
//! Above `sigma = 8.5` ImageJ stops convolving directly. It downscales the line
//! by `floor(sigma / 4)`, convolves with a correspondingly narrower Gaussian,
//! and upscales again, correcting sigma for the blur the two resamplings
//! themselves contribute.
//!
//! Detection does not reach this on default parameters: QuPath's default sigma
//! is 1.5 µm, which needs a pixel size below 0.176 µm to cross 8.5 — finer than
//! a 40× scan. It is ported anyway because sigma is a user-facing parameter and
//! raising it is the documented remedy for fragmented nuclei, so the threshold
//! is one dialog box away. Leaving it unported would have meant a silent
//! divergence at a value nothing in the code marks as special.
//!
//! # Determinism
//!
//! Lines are independent within a pass, and each output pixel is accumulated by
//! one line in a fixed order, so the result does not depend on how the work is
//! divided. §5 requires that be tested rather than asserted, and
//! `blurring_is_independent_of_how_lines_are_split` does.
//!
//! # Performance
//!
//! The interior of the convolution carries [`LANES`] output pixels at once
//! rather than ImageJ's one. That is worth 2.3x, in isolation and in place,
//! because ImageJ's `result +=` is a loop-carried dependency and the interior
//! therefore ran at the latency of a multiply-add rather than its throughput.
//! The lanes read almost the same taps, so independence is bought without extra
//! loads.
//!
//! **It is not an approximation.** Every lane accumulates the taps a single
//! accumulator would give that pixel, in the same order, so the output is
//! bit-identical — `widening_the_interior_loop_changes_nothing` asserts it
//! against [`GaussianBlur::blur_narrow`], and the parity fixture still matches
//! ImageJ exactly.
//!
//! Transposing the tile so the column pass ran contiguously was tried first and
//! was *slower* at every size. The column pass already copies each column into
//! a contiguous buffer before convolving it, so the taps were never strided;
//! only the copy-in and the store were, and two transposes cost more than they
//! saved. `docs/benchmarks/README.md` records it.

/// Accuracy `blurGaussian` uses for a `FloatProcessor`.
///
/// `ImageProcessor.blurGaussian(double)` chooses `0.002` for `ByteProcessor`
/// and `ColorProcessor` and this for everything else. Detection blurs a
/// `FloatProcessor`.
const FLOAT_ACCURACY: f64 = 0.0002;

/// Samples added on each side when upscaling, ImageJ's `UPSCALE_K_RADIUS`.
const UPSCALE_K_RADIUS: usize = 2;

/// Smallest sigma ImageJ will convolve with after downscaling.
const MIN_DOWNSCALED_SIGMA: f64 = 4.0;

/// Output pixels carried at once through the interior of the convolution.
///
/// Eight `f32` is one AVX2 register. The value only has to be enough to hide
/// the accumulator's latency and small enough that the lanes stay in registers;
/// `docs/benchmarks/README.md` records what it was worth.
const LANES: usize = 8;

/// Which lines a pass walks, and how it steps along and between them.
///
/// ImageJ's `blur1Direction` derives these from an `xDirection` flag. Naming
/// them keeps the two passes one body rather than two, and keeps the direction
/// out of the convolution itself.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    /// Points per line.
    length: usize,
    /// Number of lines.
    lines: usize,
    /// Step between consecutive points of a line.
    point_inc: usize,
    /// Step between the starts of consecutive lines.
    line_inc: usize,
}

impl Geometry {
    /// Rows of a `width` x `height` image: contiguous, one line per row.
    const fn rows(width: usize, height: usize) -> Self {
        Self {
            length: width,
            lines: height,
            point_inc: 1,
            line_inc: width,
        }
    }

    /// Columns of a `width` x `height` image, walked with a stride.
    const fn columns(width: usize, height: usize) -> Self {
        Self {
            length: height,
            lines: width,
            point_inc: width,
            line_inc: 1,
        }
    }
}

/// A reusable Gaussian blur.
///
/// Holds every buffer the blur needs, so calling it per tile allocates nothing
/// after the first tile — allocation in a tile loop is a defect under §6. Hold
/// one per worker thread; the blur itself is single-threaded, because the
/// pipeline's unit of parallelism is the tile.
#[derive(Debug, Default, Clone)]
pub struct GaussianBlur {
    /// One side of the normalised kernel, centre first.
    kernel: Vec<f32>,
    /// `kern_sum[i]` is the kernel mass beyond sample `i`, truncation included.
    kern_sum: Vec<f32>,
    /// The line being read, copied out so the output can be written in place.
    cache1: Vec<f32>,
    /// The convolved line, used only when downscaling.
    cache2: Vec<f32>,
    downscale_kernel: Vec<f32>,
    upscale_kernel: Vec<f32>,
}

impl GaussianBlur {
    /// A blur with no buffers allocated yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Blurs `pixels` in place, matching `FloatProcessor.blurGaussian(sigma)`.
    ///
    /// # Panics
    ///
    /// If `width` is zero or does not divide `pixels.len()`.
    pub fn blur(&mut self, pixels: &mut [f32], width: usize, sigma: f64) {
        self.blur_with(pixels, width, sigma, sigma, FLOAT_ACCURACY);
    }

    /// Blurs with independent sigmas and an explicit accuracy.
    ///
    /// ImageJ's `blurFloat`. There is no region of interest: QuPath reaches
    /// this through `ImageProcessor.blurGaussian`, whose first statement is
    /// `resetRoi()`, so the whole image is always both read and written and the
    /// `extraLines` handling around a partial ROI cannot be reached.
    ///
    /// # Panics
    ///
    /// If `width` is zero or does not divide `pixels.len()`.
    pub fn blur_with(
        &mut self,
        pixels: &mut [f32],
        width: usize,
        sigma_x: f64,
        sigma_y: f64,
        accuracy: f64,
    ) {
        assert!(width > 0, "width must be positive");
        assert_eq!(
            pixels.len() % width,
            0,
            "{} pixels is not a whole number of rows of {width}",
            pixels.len()
        );
        let height = pixels.len() / width;
        if height == 0 {
            return;
        }

        if sigma_x > 0.0 {
            self.blur_lines::<LANES>(pixels, Geometry::rows(width, height), sigma_x, accuracy);
        }
        if sigma_y > 0.0 {
            self.blur_lines::<LANES>(pixels, Geometry::columns(width, height), sigma_y, accuracy);
        }
    }

    /// Blurs with one output pixel in flight at a time.
    ///
    /// The implementation this crate had before the interior loop was widened,
    /// retained only so the A/B harness measures against the real previous
    /// version rather than a reimplementation of it. Bit-identical to
    /// [`Self::blur`] — `widening_the_interior_loop_changes_nothing` asserts
    /// it, and the harness asserts it again on every timed iteration.
    ///
    /// # Panics
    ///
    /// If `width` is zero or does not divide `pixels.len()`.
    #[doc(hidden)]
    pub fn blur_narrow(&mut self, pixels: &mut [f32], width: usize, sigma: f64) {
        assert!(width > 0, "width must be positive");
        assert_eq!(pixels.len() % width, 0, "not a whole number of rows");
        let height = pixels.len() / width;
        if height == 0 || sigma <= 0.0 {
            return;
        }
        self.blur_lines::<1>(pixels, Geometry::rows(width, height), sigma, FLOAT_ACCURACY);
        self.blur_lines::<1>(
            pixels,
            Geometry::columns(width, height),
            sigma,
            FLOAT_ACCURACY,
        );
    }

    /// ImageJ's `blur1Direction`, without the ROI and progress reporting.
    ///
    /// The direction is carried entirely by `geometry`, so the same body serves
    /// rows, strided columns, and rows of a transposed image.
    fn blur_lines<const LANES: usize>(
        &mut self,
        pixels: &mut [f32],
        geometry: Geometry,
        sigma: f64,
        accuracy: f64,
    ) {
        let Geometry {
            length,
            lines,
            point_inc,
            line_inc,
        } = geometry;

        let downscaling = sigma > 2.0 * MIN_DOWNSCALED_SIGMA + 0.5;
        let reduce_by = if downscaling {
            downscale_factor(sigma).min(length)
        } else {
            1
        };

        // Downscaling and upscaling each blur a little on their own — variance
        // 1/3 and 1/4 in downscaled pixels — and those add to the total, so the
        // Gaussian actually convolved is narrower than the one asked for.
        let sigma_gauss = if downscaling {
            #[expect(
                clippy::cast_precision_loss,
                reason = "reduce_by is a small line-length divisor"
            )]
            let squared = (reduce_by * reduce_by) as f64;
            (sigma * sigma / squared - 1.0 / 3.0 - 1.0 / 4.0).sqrt()
        } else {
            sigma
        };

        // With no ROI the whole line is read, so ImageJ's `maxLength` and
        // `newLength` coincide; it distinguishes them only to bound the kernel
        // before knowing the read range.
        let line_length = if downscaling {
            length.div_ceil(reduce_by) + 2 * (UPSCALE_K_RADIUS + 1)
        } else {
            length
        };

        fill_gaussian_kernel(
            &mut self.kernel,
            &mut self.kern_sum,
            sigma_gauss,
            accuracy,
            line_length,
        );

        // Input point corresponding to cache index 0; negative by construction.
        #[expect(
            clippy::cast_possible_wrap,
            reason = "line lengths are far below isize::MAX"
        )]
        let unscaled0 = -(((UPSCALE_K_RADIUS + 1) * reduce_by) as isize);

        let Self {
            kernel,
            kern_sum,
            cache1,
            cache2,
            downscale_kernel,
            upscale_kernel,
            ..
        } = self;

        cache1.clear();
        cache1.resize(line_length, 0.0);
        if downscaling {
            cache2.clear();
            cache2.resize(line_length, 0.0);
            *downscale_kernel = make_downscale_kernel(reduce_by);
            *upscale_kernel = make_upscale_kernel(reduce_by);
        }

        let taps = (&kernel[..], &kern_sum[..]);
        for index in 0..lines {
            let line = Line {
                start: index * line_inc,
                step: point_inc,
                reduce_by,
                unscaled0,
            };
            if downscaling {
                downscale_line(pixels, cache1, downscale_kernel, line, length);
                // The convolution runs in downscaled coordinates, writing to a
                // separate cache; the outermost sample each side is padding for
                // the upscale and is not written.
                convolve_line::<LANES>(cache1, cache2, taps, 1, line_length - 1, 0, 1);
                upscale_line(cache2, pixels, upscale_kernel, line, length);
            } else {
                // Copied out because the convolution writes back over the line
                // it is still reading.
                for (i, slot) in cache1.iter_mut().enumerate() {
                    *slot = pixels[line.start + i * point_inc];
                }
                convolve_line::<LANES>(cache1, pixels, taps, 0, length, line.start, point_inc);
            }
        }
    }
}

/// `floor(sigma / 4)`, ImageJ's downscaling factor, floored at one.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sigma is positive here and the quotient is a small line divisor"
)]
fn downscale_factor(sigma: f64) -> usize {
    ((sigma / MIN_DOWNSCALED_SIGMA).floor() as usize).max(1)
}

/// ImageJ's `makeGaussianKernel`, into reusable buffers.
///
/// Fills `kernel` with one side of the normalised kernel — centre at index 0,
/// decaying outwards — and `kern_sum` with the running mass beyond each sample.
///
/// `kern_sum` is what makes the edge handling work. It starts at
/// `0.5 + 0.5·kernel[0]/sum`, which is the mass of the half-kernel including
/// half the centre tap, and each entry subtracts one more sample. Because the
/// normalising `sum` covers only the samples that exist while the running total
/// starts from the mass of a *complete* half-kernel, whatever the truncation
/// dropped stays in `kern_sum` and reappears at the edges.
fn fill_gaussian_kernel(
    kernel: &mut Vec<f32>,
    kern_sum: &mut Vec<f32>,
    sigma: f64,
    accuracy: f64,
    max_radius: usize,
) {
    // Too small a maxRadius would make the tail sum inaccurate.
    let max_radius = max_radius.max(50);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sigma and accuracy are bounded; the radius is a small count"
    )]
    let wanted = (sigma * (-2.0 * accuracy.ln()).sqrt()).ceil() as usize + 1;
    let k_radius = wanted.min(max_radius);

    kernel.clear();
    kernel.reserve(k_radius);
    for i in 0..k_radius {
        #[expect(
            clippy::cast_precision_loss,
            reason = "the kernel radius is a small count"
        )]
        let i = i as f64;
        // Left-to-right as Java evaluates it, including the two divisions.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "ImageJ stores the kernel as float; this is that cast"
        )]
        kernel.push((-0.5 * i * i / sigma / sigma).exp() as f32);
    }

    // The parabolic tail. Skipped for a kernel of 3 or less, where there is no
    // room for it, and when the radius was cut short by `max_radius`, where the
    // kernel is being truncated deliberately rather than for accuracy.
    if k_radius < max_radius && k_radius > 3 {
        let mut sqrt_slope = f64::MAX;
        let mut r = k_radius;
        while r > k_radius / 2 {
            r -= 1;
            #[expect(
                clippy::cast_precision_loss,
                reason = "the kernel radius is a small count"
            )]
            let distance = (k_radius - r) as f64;
            let a = f64::from(kernel[r]).sqrt() / distance;
            if a < sqrt_slope {
                sqrt_slope = a;
            } else {
                break;
            }
        }
        // Two samples further out than where the scan stopped: the sample it
        // stopped on is the one that failed to improve, so it is kept.
        for (r1, tap) in kernel.iter_mut().enumerate().skip(r + 2) {
            let distance = k_radius - r1;
            #[expect(
                clippy::cast_precision_loss,
                reason = "the squared radius is exact in f64 at this size"
            )]
            let squared = (distance * distance) as f64;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "ImageJ stores the kernel as float; this is that cast"
            )]
            let value = (squared * sqrt_slope * sqrt_slope) as f32;
            *tap = value;
        }
    }

    let sum = if k_radius < max_radius {
        let mut sum = f64::from(kernel[0]);
        for value in &kernel[1..k_radius] {
            sum += 2.0 * f64::from(*value);
        }
        sum
    } else {
        // A kernel cut short by `max_radius` is missing too much of itself for
        // its own samples to normalise it, so the analytic integral is used.
        sigma * (2.0 * std::f64::consts::PI).sqrt()
    };

    let mut running = 0.5 + 0.5 * f64::from(kernel[0]) / sum;
    kern_sum.clear();
    kern_sum.reserve(k_radius);
    for value in kernel.iter_mut() {
        let normalised = f64::from(*value) / sum;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "ImageJ stores the kernel as float; this is that cast"
        )]
        let narrowed = normalised as f32;
        *value = narrowed;
        running -= normalised;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "ImageJ stores the tail sum as float; this is that cast"
        )]
        let narrowed = running as f32;
        kern_sum.push(narrowed);
    }
}

/// Where one line sits in the pixel array, and how it is resampled.
///
/// Bundles the four values ImageJ threads through `downscaleLine` and
/// `upscaleLine` as separate arguments. They always travel together and are
/// always the same for both calls on a line, so passing them as one keeps the
/// two impossible to disagree.
#[derive(Debug, Clone, Copy)]
struct Line {
    /// Pixel index of the line's first point, ImageJ's `pixel0`.
    start: usize,
    /// Step to the next point: 1 along a row, the width down a column.
    step: usize,
    /// Downscaling factor, ImageJ's `reduceBy`. One when convolving directly.
    reduce_by: usize,
    /// Input point corresponding to cache index 0; negative by construction.
    unscaled0: isize,
}

/// ImageJ's `convolveLine`: symmetric kernel, edges extended by replication.
///
/// Accumulation is `f32` throughout and in ImageJ's order — centre tap first,
/// then outwards in `k`, with the two mirrored samples summed before being
/// scaled. Reordering it would change the result in the last bits.
///
/// `readFrom` and `readTo` are absent because ImageJ's body never uses them;
/// they only document which part of `input` the caller must have filled.
fn convolve_line<const LANES: usize>(
    input: &[f32],
    output: &mut [f32],
    (kernel, kern_sum): (&[f32], &[f32]),
    write_from: usize,
    write_to: usize,
    point0: usize,
    point_inc: usize,
) {
    let length = input.len();
    // Out-of-edge samples are replaced by the nearest edge sample.
    let first = input[0];
    let last = input[length - 1];
    let kern0 = kernel[0];
    let k_radius = kernel.len();

    let first_part = k_radius.min(length);
    let mut i = write_from;
    let mut p = point0 + write_from * point_inc;

    // While the sum would reach below index 0.
    while i < first_part {
        let mut result = input[i] * kern0;
        result += kern_sum[i] * first;
        if i + k_radius > length {
            result += kern_sum[length - i - 1] * last;
        }
        for k in 1..k_radius {
            let mut v = 0.0f32;
            if i >= k {
                v += input[i - k];
            }
            if i + k < length {
                v += input[i + k];
            }
            result += kernel[k] * v;
        }
        output[p] = result;
        i += 1;
        p += point_inc;
    }

    // ImageJ's `length - kRadius` can go negative, which makes its loop test
    // fail immediately; saturating at zero does the same, since `i` is never
    // negative.
    let end_inside = length.saturating_sub(k_radius).min(write_to);

    // The easy case: every tap is inside the line, so there are no bounds
    // decisions and the taps can be read straight through.
    //
    // `LANES` output pixels are carried at once. Each one accumulates exactly
    // the sequence the single-pixel loop below would give it — centre tap
    // first, then k ascending — so this is a bit-identical rewrite and not a
    // reassociation. What it buys is that the lanes are independent of each
    // other, which the single accumulator is not: `result` feeds the next
    // multiply-add, so the loop runs at the latency of an FMA rather than its
    // throughput, and the strided store cannot overlap with anything either.
    while i + LANES <= end_inside {
        let mut acc = [0.0f32; LANES];
        for (lane, slot) in acc.iter_mut().enumerate() {
            *slot = input[i + lane] * kern0;
        }
        for k in 1..k_radius {
            let tap = kernel[k];
            let (low, high) = (&input[i - k..i - k + LANES], &input[i + k..i + k + LANES]);
            for (lane, slot) in acc.iter_mut().enumerate() {
                *slot += tap * (low[lane] + high[lane]);
            }
        }
        for (lane, &value) in acc.iter().enumerate() {
            output[p + lane * point_inc] = value;
        }
        i += LANES;
        p += LANES * point_inc;
    }

    // The remainder, and the whole interior when LANES is 1.
    while i < end_inside {
        let mut result = input[i] * kern0;
        for k in 1..k_radius {
            result += kernel[k] * (input[i - k] + input[i + k]);
        }
        output[p] = result;
        i += 1;
        p += point_inc;
    }

    // While the sum would reach past the end. The `>=` here against the `>` in
    // the first loop is ImageJ's, and it decides whether the truncated tail is
    // added when the kernel ends exactly on the last sample.
    while i < write_to {
        let mut result = input[i] * kern0;
        if i < k_radius {
            result += kern_sum[i] * first;
        }
        if i + k_radius >= length {
            result += kern_sum[length - i - 1] * last;
        }
        for k in 1..k_radius {
            let mut v = 0.0f32;
            if i >= k {
                v += input[i - k];
            }
            if i + k < length {
                v += input[i + k];
            }
            result += kernel[k] * v;
        }
        output[p] = result;
        i += 1;
        p += point_inc;
    }
}

/// ImageJ's `downscaleLine`: reduce a line by `reduce_by` into `cache`.
///
/// Each input sample contributes to three output samples, so the loop walks the
/// input once and scatters into the cache rather than gathering per output —
/// which is why the cache is accumulated with `+=` and why the entry two ahead
/// is assigned, not added: that assignment is what initialises it.
fn downscale_line(pixels: &[f32], cache: &mut [f32], kernel: &[f32], line: Line, length: usize) {
    let reduce_by = line.reduce_by;
    #[expect(
        clippy::cast_possible_wrap,
        reason = "image indices are far below isize::MAX"
    )]
    let (pixel0, point_inc_signed, last, new_length) = (
        line.start as isize,
        line.step as isize,
        length as isize - 1,
        cache.len() as isize,
    );
    let mut p = pixel0 + point_inc_signed * (line.unscaled0 - (reduce_by * 3 / 2) as isize);
    let p_last = pixel0 + point_inc_signed * last;

    for out in -1..=new_length {
        let (mut sum0, mut sum1, mut sum2) = (0.0f32, 0.0f32, 0.0f32);
        for x in 0..reduce_by {
            // Out-of-line samples are replaced by the edge samples.
            #[expect(
                clippy::cast_sign_loss,
                reason = "clamped into [pixel0, p_last], both non-negative"
            )]
            let v = pixels[p.clamp(pixel0, p_last) as usize];
            sum0 += v * kernel[x + 2 * reduce_by];
            sum1 += v * kernel[x + reduce_by];
            sum2 += v * kernel[x];
            p += point_inc_signed;
        }
        #[expect(
            clippy::cast_sign_loss,
            reason = "each index is guarded non-negative by its condition"
        )]
        {
            if out > 0 {
                cache[(out - 1) as usize] += sum0;
            }
            if out >= 0 && out < new_length {
                cache[out as usize] += sum1;
            }
            if out + 1 < new_length {
                cache[(out + 1) as usize] = sum2;
            }
        }
    }
}

/// ImageJ's `upscaleLine`: expand `cache` back over the full-resolution line.
fn upscale_line(cache: &[f32], pixels: &mut [f32], kernel: &[f32], line: Line, write_to: usize) {
    let reduce_by = line.reduce_by;
    let mut p = line.start;
    for out in 0..write_to {
        #[expect(
            clippy::cast_possible_wrap,
            reason = "image indices are far below isize::MAX"
        )]
        let shifted = out as isize - line.unscaled0 + reduce_by as isize - 1;
        #[expect(
            clippy::cast_sign_loss,
            reason = "unscaled0 is negative, so `shifted` is positive"
        )]
        let (index, phase) = (
            (shifted / reduce_by as isize) as usize,
            (shifted % reduce_by as isize) as usize,
        );
        let x = reduce_by - 1 - phase;
        // Left-to-right, as Java sums it.
        pixels[p] = cache[index - 2] * kernel[x]
            + cache[index - 1] * kernel[x + reduce_by]
            + cache[index] * kernel[x + 2 * reduce_by]
            + cache[index + 1] * kernel[x + 3 * reduce_by];
        p += line.step;
    }
}

/// ImageJ's `makeDownscaleKernel`, running from -1.5 to +1.5 downscaled pixels.
///
/// Preserves norm and first moment with a fixed second moment, which plain
/// linear interpolation does not.
fn make_downscale_kernel(unit_length: usize) -> Vec<f32> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "the reduction factor is a small count"
    )]
    let unit = unit_length as f64;
    let mid = unit_length * 3 / 2;
    let mut kernel = vec![0.0f32; 3 * unit_length];

    let mut set = |i: usize, v: f64| {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "ImageJ stores the kernel as float; this is that cast"
        )]
        let v = v as f32;
        kernel[mid - i] = v;
        kernel[mid + i] = v;
    };

    for i in 0..=unit_length / 2 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "the reduction factor is a small count"
        )]
        let x = i as f64 / unit;
        set(i, (0.75 - x * x) / unit);
    }
    for i in unit_length / 2 + 1..(unit_length * 3).div_ceil(2) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "the reduction factor is a small count"
        )]
        let x = i as f64 / unit;
        set(i, (0.125 + 0.5 * (x - 1.0) * (x - 2.0)) / unit);
    }
    kernel
}

/// ImageJ's `makeUpscaleKernel`: four unit squares convolved together.
///
/// Runs from -2 to +2 downscaled pixels, so index 0 is the open end of the
/// interval and stays zero.
fn make_upscale_kernel(unit_length: usize) -> Vec<f32> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "the reduction factor is a small count"
    )]
    let unit = unit_length as f64;
    let mid = 2 * unit_length;
    let mut kernel = vec![0.0f32; 4 * unit_length];

    let mut set = |i: usize, v: f64| {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "ImageJ stores the kernel as float; this is that cast"
        )]
        let v = v as f32;
        kernel[mid + i] = v;
        kernel[mid - i] = v;
    };

    for i in 0..unit_length {
        #[expect(
            clippy::cast_precision_loss,
            reason = "the reduction factor is a small count"
        )]
        let x = i as f64 / unit;
        set(i, 2.0 / 3.0 - x * x * (1.0 - 0.5 * x));
    }
    for i in unit_length..2 * unit_length {
        #[expect(
            clippy::cast_precision_loss,
            reason = "the reduction factor is a small count"
        )]
        let x = i as f64 / unit;
        set(i, (2.0 - x) * (2.0 - x) * (2.0 - x) / 6.0);
    }
    kernel
}

/// One side of ImageJ's Gaussian kernel and its running tail sum.
///
/// Returns `(kernel, kern_sum)` — exactly `makeGaussianKernel`'s two rows.
/// Exposed so the parity test can compare the kernel itself: when blurred
/// pixels disagree, this says whether the kernel or the convolution is at
/// fault, which the output alone does not.
///
/// `accuracy` is [`FLOAT_ACCURACY`] for the blur QuPath runs; `max_radius` is
/// the line length, and is raised to 50 internally if smaller.
#[must_use]
pub fn gaussian_kernel(sigma: f64, accuracy: f64, max_radius: usize) -> (Vec<f32>, Vec<f32>) {
    let (mut kernel, mut kern_sum) = (Vec::new(), Vec::new());
    fill_gaussian_kernel(&mut kernel, &mut kern_sum, sigma, accuracy, max_radius);
    (kernel, kern_sum)
}

/// Blurs `pixels` in place, allocating its buffers for this call alone.
///
/// Convenience for a one-off; a tile loop should hold a [`GaussianBlur`].
///
/// # Panics
///
/// If `width` is zero or does not divide `pixels.len()`.
pub fn blur_gaussian(pixels: &mut [f32], width: usize, sigma: f64) {
    GaussianBlur::new().blur(pixels, width, sigma);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blurs and returns, so cases read as one expression.
    fn blurred(pixels: &[f32], width: usize, sigma: f64) -> Vec<f32> {
        let mut out = pixels.to_vec();
        blur_gaussian(&mut out, width, sigma);
        out
    }

    #[test]
    fn a_constant_image_stays_constant() {
        // The kernel sums to one and the edges extend by replication, so
        // nothing about a flat field should change. This is the single
        // cheapest check that the normalisation and the edge handling agree:
        // if `kern_sum` did not carry the truncated tail, the border would sag.
        for sigma in [0.5, 1.5, 3.0, 6.0, 9.0, 12.0] {
            let out = blurred(&vec![0.75f32; 40 * 40], 40, sigma);
            for (index, value) in out.iter().enumerate() {
                assert!(
                    (value - 0.75).abs() < 1e-5,
                    "sigma {sigma}: pixel {index} became {value}, not 0.75"
                );
            }
        }
    }

    #[test]
    fn an_impulse_keeps_its_mass() {
        // Blurring conserves the integral, so a spike spreads without losing
        // weight. A kernel normalised against the wrong sum fails here.
        for sigma in [1.0, 3.0, 9.0] {
            let mut pixels = vec![0.0f32; 81 * 81];
            pixels[40 * 81 + 40] = 1.0;
            let out = blurred(&pixels, 81, sigma);
            let mass: f64 = out.iter().map(|&v| f64::from(v)).sum();
            assert!(
                (mass - 1.0).abs() < 1e-4,
                "sigma {sigma}: mass became {mass}, not 1"
            );
        }
    }

    #[test]
    fn an_impulse_stays_centred_and_symmetric() {
        // Catches a kernel indexed off by one, and an x/y transposition, both
        // of which conserve mass and so pass the test above.
        let width = 41;
        let mut pixels = vec![0.0f32; width * width];
        pixels[20 * width + 20] = 1.0;
        let out = blurred(&pixels, width, 2.0);

        let peak = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .expect("non-empty");
        assert_eq!(peak.0, 20 * width + 20, "the peak moved");

        for offset in 1..=6 {
            let left = out[20 * width + 20 - offset];
            let right = out[20 * width + 20 + offset];
            let up = out[(20 - offset) * width + 20];
            let down = out[(20 + offset) * width + 20];
            assert_eq!(left, right, "asymmetric across x at offset {offset}");
            assert_eq!(up, down, "asymmetric across y at offset {offset}");
            assert!(
                (f64::from(left) - f64::from(up)).abs() < 1e-9,
                "x and y disagree at offset {offset}: {left} against {up}"
            );
        }
    }

    #[test]
    fn a_wider_sigma_spreads_further() {
        let width = 61;
        let mut pixels = vec![0.0f32; width * width];
        pixels[30 * width + 30] = 1.0;

        let mut previous = f32::INFINITY;
        for sigma in [1.0, 2.0, 4.0] {
            let peak = blurred(&pixels, width, sigma)[30 * width + 30];
            assert!(peak < previous, "sigma {sigma} did not lower the peak");
            previous = peak;
        }
    }

    #[test]
    fn the_downscaled_path_agrees_with_the_direct_one_at_the_threshold() {
        // ImageJ switches to downscaling above sigma 8.5. The two paths are
        // different code, so they are only as consistent as ImageJ made them —
        // this pins how close that is, which a fixture at one sigma cannot say.
        // Forcing the direct path needs the accuracy left alone, so the
        // comparison is between `blur` at 8.6 and the same kernel construction
        // just below the threshold.
        let width = 64;
        let mut pixels = vec![0.0f32; width * width];
        pixels[32 * width + 32] = 1.0;

        let below = blurred(&pixels, width, 8.4);
        let above = blurred(&pixels, width, 8.6);
        let worst = below
            .iter()
            .zip(&above)
            .map(|(a, b)| f64::from(a - b).abs())
            .fold(0.0f64, f64::max);
        assert!(
            worst < 2e-4,
            "the two paths disagree by {worst} across sigma 8.5"
        );
    }

    #[test]
    fn blurring_is_independent_of_how_lines_are_split() {
        // §5: a parallel reduction must be deterministic. Lines are
        // independent here, so splitting the image into row bands and blurring
        // each band's rows separately must reproduce the x pass exactly. This
        // is what would fail if the blur ever accumulated across lines.
        let width = 48;
        let height = 32;
        let pixels: Vec<f32> = (0..width * height)
            .map(|i| ((i * 2654435761) % 1000) as f32 / 400.0 - 0.5)
            .collect();

        let mut whole = pixels.clone();
        let mut blur = GaussianBlur::new();
        blur.blur_with(&mut whole, width, 3.0, 0.0, FLOAT_ACCURACY);

        for bands in [1usize, 2, 3, 5, 8] {
            let mut split = pixels.clone();
            let rows_per_band = height.div_ceil(bands);
            for band in split.chunks_mut(rows_per_band * width) {
                GaussianBlur::new().blur_with(band, width, 3.0, 0.0, FLOAT_ACCURACY);
            }
            assert_eq!(
                split.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                whole.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "{bands} bands gave a different x pass"
            );
        }
    }

    #[test]
    fn widening_the_interior_loop_changes_nothing() {
        // Carrying eight output pixels at once is an optimisation that must not
        // be an approximation: each lane accumulates the same taps in the same
        // order, so the claim is identical, not close. Sizes that are not
        // multiples of eight are the ones a mishandled remainder gets wrong,
        // and short lines skip the widened loop altogether.
        let cases = [
            (64usize, 64usize, 3.0f64),
            (37, 23, 1.5),
            (23, 37, 1.5),
            (11, 9, 4.0),
            (1, 40, 2.0),
            (40, 1, 2.0),
            (8, 8, 0.5),
            (9, 17, 0.5),
            (33, 32, 0.5),
            (64, 64, 9.0),
            (65, 31, 12.0),
        ];

        let mut blur = GaussianBlur::new();
        for (width, height, sigma) in cases {
            let pixels: Vec<f32> = (0..width * height)
                .map(|i| ((i * 2654435761usize) % 1000) as f32 / 400.0 - 0.5)
                .collect();

            let mut wide = pixels.clone();
            blur.blur(&mut wide, width, sigma);
            let mut narrow = pixels;
            blur.blur_narrow(&mut narrow, width, sigma);

            assert_eq!(
                wide.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                narrow.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "{width}x{height} sigma {sigma}: the two widths disagree"
            );
        }
    }

    #[test]
    fn a_reused_blur_matches_a_fresh_one() {
        // The buffers are reused across calls, so a stale entry would show up
        // only on the second call — and only for a shape that leaves one.
        let mut reused = GaussianBlur::new();
        let cases = [
            (64usize, 3.0f64),
            (11, 4.0),
            (37, 1.5),
            (64, 9.0),
            (23, 0.5),
        ];

        for &(width, sigma) in &cases {
            let pixels: Vec<f32> = (0..width * width)
                .map(|i| ((i * 40503) % 977) as f32 / 400.0 - 0.5)
                .collect();

            let mut theirs = pixels.clone();
            GaussianBlur::new().blur(&mut theirs, width, sigma);
            let mut ours = pixels;
            reused.blur(&mut ours, width, sigma);

            assert_eq!(
                ours.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                theirs.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "reuse changed the result at width {width}, sigma {sigma}"
            );
        }
    }

    #[test]
    fn the_tail_correction_touches_only_the_outermost_taps() {
        // The parabolic tail is the part a textbook Gaussian gets wrong, and
        // the break-point search is what decides how much of the kernel it
        // covers. For sigma 3 at this accuracy ImageJ replaces exactly one
        // tap — everything inside stays the plain Gaussian, and asserting both
        // halves pins the search as well as the fit. A correction that ran too
        // far inwards would still look like a smooth decaying kernel.
        let sigma = 3.0;
        let (kernel, kern_sum) = gaussian_kernel(sigma, FLOAT_ACCURACY, 64);
        assert_eq!(kernel.len(), 14, "the radius for sigma 3 moved");

        // Recovering the normaliser from the centre tap, which is exp(0) = 1
        // before normalisation.
        let sum = 1.0 / f64::from(kernel[0]);
        let gaussian = |r: usize| {
            #[expect(clippy::cast_precision_loss, reason = "the radius is a small count")]
            let r = r as f64;
            (-0.5 * r * r / (sigma * sigma)).exp() / sum
        };

        for (r, &tap) in kernel.iter().enumerate().take(13) {
            let relative = (f64::from(tap) - gaussian(r)).abs() / gaussian(r);
            assert!(
                relative < 1e-6,
                "tap {r} is {tap} but the Gaussian is {} — the fit reached too far in",
                gaussian(r)
            );
        }

        // The outermost tap is the fitted one: below the Gaussian it replaced,
        // and positioned so the parabola hits zero at the first tap outside.
        let last = f64::from(kernel[13]);
        assert!(
            last < gaussian(13),
            "the outermost tap {last} was not pulled below the Gaussian {}",
            gaussian(13)
        );
        // Which leaves essentially no mass beyond the kernel.
        let beyond = f64::from(*kern_sum.last().expect("non-empty"));
        assert!(
            beyond.abs() < 1e-9,
            "the parabola left {beyond} of mass past the kernel"
        );
    }

    #[test]
    fn the_tail_sum_is_the_mass_beyond_each_sample() {
        // `kern_sum[i]` is what the edge extension multiplies the edge value
        // by, so it has to be exactly the kernel mass past sample i. It starts
        // at half the kernel less half the centre tap — the centre is shared
        // between the two sides — and decreases from there.
        for sigma in [1.0, 2.0, 3.0, 6.0] {
            let (kernel, kern_sum) = gaussian_kernel(sigma, FLOAT_ACCURACY, 64);

            let total =
                f64::from(kernel[0]) + 2.0 * kernel[1..].iter().map(|&v| f64::from(v)).sum::<f64>();
            assert!(
                (total - 1.0).abs() < 1e-6,
                "sigma {sigma}: the kernel sums to {total}"
            );

            let expected = 0.5 - 0.5 * f64::from(kernel[0]);
            assert!(
                (f64::from(kern_sum[0]) - expected).abs() < 1e-6,
                "sigma {sigma}: the tail sum starts at {}, not {expected}",
                kern_sum[0]
            );

            for i in 1..kern_sum.len() {
                assert!(
                    kern_sum[i] <= kern_sum[i - 1],
                    "sigma {sigma}: the tail sum rose at {i}"
                );
            }
            // Never negative by more than rounding: a negative tail sum would
            // subtract signal at the edges instead of extending it.
            assert!(
                f64::from(*kern_sum.last().expect("non-empty")) > -1e-9,
                "sigma {sigma}: the tail sum went negative"
            );
        }
    }

    #[test]
    fn a_line_shorter_than_the_kernel_still_blurs() {
        // convolveLine's interior loop never runs here and the first loop
        // covers the whole line, with both edge corrections active at once.
        let out = blurred(&[1.0f32; 5 * 5], 5, 6.0);
        for value in out {
            assert!(
                (value - 1.0).abs() < 1e-5,
                "a 5x5 constant became {value} under sigma 6"
            );
        }
    }
}
