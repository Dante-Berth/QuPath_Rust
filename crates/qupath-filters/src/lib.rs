//! Circular-kernel MIN and MAX rank filters, matching ImageJ's `RankFilters`
//! as QuPath invokes it from `WatershedCellDetection`.
//!
//! These are grayscale morphological erosion and dilation. The profile
//! (`docs/profiles/README.md`) puts `RankFilters.getSideMax` and `getAreaMax`
//! at 11.6% of cell-detection self time, the largest cost after the watershed
//! queue.
//!
//! # Algorithm
//!
//! The disc is decomposed into a union of nested rectangles (see [`plan`]), and
//! each rectangle is filtered separably: a vertical pass folding whole source
//! rows into a column accumulator, then a horizontal windowed maximum along
//! that accumulator. Because the rectangle heights are nested, one accumulator
//! grown outwards from the centre serves every rectangle, so a source row is
//! read once per output row rather than once per kernel row.
//!
//! The consequence is that the per-pixel cost stops tracking the kernel and
//! every inner loop becomes a slice-to-slice maximum over contiguous `f32`,
//! which the compiler vectorises without any `unsafe`. `docs/benchmarks/README.md`
//! records what that is worth, and the two earlier implementations it replaced
//! — a naive area scan, then ImageJ's own incremental scheme — because the
//! progression is the clearest evidence in this repository that the algorithm,
//! not the language, is what decides.
//!
//! # Reuse
//!
//! [`rank`] allocates a source copy per call, because filtering in place needs
//! somewhere to keep the pixels it is still reading. A tile pipeline should
//! hold a [`RankFilter`] and call [`RankFilter::apply_into`] with separate
//! input and output tiles, which copies nothing and allocates nothing —
//! allocation in the tile loop is a defect under §6 of the project guidelines.
//!
//! ```
//! use qupath_filters::{rank, RankOp};
//!
//! // A single bright pixel dilates into the kernel shape.
//! let mut pixels = vec![0.0f32; 25];
//! pixels[12] = 100.0;
//! rank(&mut pixels, 5, 1.0, RankOp::Max);
//! assert_eq!(pixels[6], 100.0, "the 3x3 neighbourhood is filled");
//! assert_eq!(pixels[0], 0.0, "the corner is outside a radius-1 kernel");
//! ```

use rayon::prelude::*;

mod convolve;
mod gaussian;
mod kernel;
mod morphology;
mod plan;

pub use convolve::{Convolver, convolve, kernel_scale};
pub use gaussian::{GaussianBlur, blur_gaussian, gaussian_kernel};
pub use kernel::Kernel;
pub use morphology::{Reconstructor, find_regional_maxima, morphological_reconstruction};

use plan::Plan;

/// Which rank statistic the filter computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankOp {
    /// Grayscale erosion: the minimum over the kernel.
    Min,
    /// Grayscale dilation: the maximum over the kernel.
    Max,
}

/// Applies a circular-kernel rank filter to `pixels`, in place.
///
/// `pixels` is row-major with the given `width`. Coordinates outside the image
/// are clamped to the nearest edge pixel, as ImageJ does — the filter never
/// invents a background value, so a bright object touching the border stays
/// bright rather than being eroded by imaginary zeros.
///
/// This allocates a copy of `pixels` on every call. Callers filtering many
/// tiles should use [`RankFilter`], which does not.
///
/// # Panics
///
/// Panics if `width` is zero or does not divide `pixels.len()`.
pub fn rank(pixels: &mut [f32], width: usize, radius: f64, op: RankOp) {
    RankFilter::new().apply(pixels, width, radius, op);
}

/// A rank filter that owns its scratch space, for repeated application.
///
/// The filter reads a neighbourhood, so it cannot overwrite a buffer it is
/// still reading: filtering in place needs a copy of the source, and holding
/// that copy here means a tile loop allocates once rather than once per tile.
/// [`RankFilter::apply_into`] sidesteps the copy entirely by writing to a
/// separate output. The decomposed kernel is cached alongside the buffers, so
/// repeated calls at one radius do not rebuild it.
#[derive(Debug)]
pub struct RankFilter {
    /// Holds the caller's pixels during an in-place [`RankFilter::apply`], so
    /// that the filter has an untouched image to read while it overwrites the
    /// original. Unused by [`RankFilter::apply_into`].
    staging: Vec<f32>,
    /// The negated source for MIN, which has nowhere else to live. MAX does not
    /// use it, and so copies nothing.
    negated: Vec<f32>,
    /// Cached decomposition, keyed on the radius that produced it.
    plan: Option<(f64, Plan)>,
    /// Whether to spread rows across rayon. See
    /// [`RankFilter::with_parallel_rows`].
    parallel_rows: bool,
}

impl Default for RankFilter {
    /// Written out rather than derived, because a derived `bool` default would
    /// silently make the filter sequential.
    fn default() -> Self {
        Self {
            staging: Vec::new(),
            negated: Vec::new(),
            plan: None,
            parallel_rows: true,
        }
    }
}

impl RankFilter {
    /// Creates a filter with no scratch space yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Chooses whether rows are spread across rayon, which they are by default.
    ///
    /// **A caller that is already inside a rayon job should turn this off.**
    /// §6 forbids nesting `par_iter`, and this is why: the tile pipeline runs
    /// one job per tile, and a nested parallel filter lets rayon resume some
    /// *other* tile's job on this thread while this one is still in the middle
    /// of its own. Anything the outer job was holding — a thread-local scratch
    /// buffer, a `RefCell` borrow — is then re-entered. It also contends for the
    /// same cores the outer level already saturates: measured on a 2120 px tile,
    /// the erosion is faster sequentially inside a four-tile run than it is
    /// nested, 0.014 s against 0.051 s.
    #[must_use]
    pub fn with_parallel_rows(mut self, parallel: bool) -> Self {
        self.parallel_rows = parallel;
        self
    }

    /// Applies the filter to `pixels`, in place, reusing this filter's buffers.
    ///
    /// Semantics are identical to [`rank`]; only the allocation behaviour
    /// differs. In-place operation costs one copy of `pixels`, because the
    /// filter reads a neighbourhood and cannot overwrite what it is still
    /// reading. A tile pipeline that already has separate input and output
    /// tiles should call [`RankFilter::apply_into`], which avoids that copy.
    ///
    /// # Panics
    ///
    /// Panics if `width` is zero or does not divide `pixels.len()`.
    pub fn apply(&mut self, pixels: &mut [f32], width: usize, radius: f64, op: RankOp) {
        self.staging.clear();
        self.staging.extend_from_slice(pixels);
        let staging = std::mem::take(&mut self.staging);
        self.apply_into(&staging, pixels, width, radius, op);
        self.staging = staging;
    }

    /// Filters `source` into `out`, reusing this filter's buffers.
    ///
    /// `source` and `out` must be the same length and must not be the same
    /// buffer; use [`RankFilter::apply`] to filter a buffer in place. For MAX
    /// this copies nothing at all — the filter reads the caller's `source`
    /// directly and writes straight to `out`.
    ///
    /// # Panics
    ///
    /// Panics if `width` is zero, does not divide the length, or if `source`
    /// and `out` differ in length.
    pub fn apply_into(
        &mut self,
        source: &[f32],
        out: &mut [f32],
        width: usize,
        radius: f64,
        op: RankOp,
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

        let height = source.len() / width;
        if height == 0 {
            return;
        }

        if self
            .plan
            .as_ref()
            .is_none_or(|(cached, _)| *cached != radius)
        {
            self.plan = Some((radius, Plan::for_radius(radius)));
        }

        // MIN is MAX on the negated image, as in ImageJ, which runs one code
        // path with a flipped sign. Negation of an `f32` is exact, so this
        // cannot perturb a result. MAX needs no negated copy and so reads the
        // caller's buffer directly.
        let source = match op {
            RankOp::Max => source,
            RankOp::Min => {
                self.negated.clear();
                self.negated.reserve(source.len());
                self.negated.extend(source.iter().map(|value| -value));
                &self.negated
            }
        };

        let plan = &self.plan.as_ref().expect("plan set immediately above").1;
        let scan = Scan {
            source,
            width,
            height,
            plan,
            negate_output: matches!(op, RankOp::Min),
        };

        // Rows are independent: each reads the untouched source and writes only
        // its own output row, so this parallelises without synchronisation.
        // ImageJ threads the same way, and comparing a single-threaded port
        // against a threaded original would not be a like-for-like measurement.
        //
        // Granularity is a row, not a pixel, per §6. No float reduction crosses
        // rows, so the result does not depend on how rayon splits the work —
        // MIN and MAX select an existing value rather than accumulating.
        //
        // `for_each_init` builds the per-row scratch once per rayon job rather
        // than once per row. The scratch is O(width); the megabyte-scale buffer
        // is `self.source`, which is reused across calls.
        let scratch = || Scratch::new(width, plan.max_half_width());
        if self.parallel_rows {
            out.par_chunks_mut(width)
                .enumerate()
                .for_each_init(scratch, |scratch, (y, out)| {
                    filter_row(&scan, scratch, out, y)
                });
        } else {
            let mut scratch = scratch();
            out.chunks_mut(width)
                .enumerate()
                .for_each(|(y, out)| filter_row(&scan, &mut scratch, out, y));
        }
    }
}

/// The image geometry and decomposed kernel a row filter works against.
struct Scan<'a> {
    source: &'a [f32],
    width: usize,
    height: usize,
    plan: &'a Plan,
    /// Set for MIN, where the source was negated on the way in and the output
    /// has to be negated on the way out.
    negate_output: bool,
}

impl Scan<'_> {
    /// The source row at `y`, which must be in bounds.
    fn row(&self, y: usize) -> &[f32] {
        &self.source[y * self.width..(y + 1) * self.width]
    }
}

/// Row-sized working buffers, reused across every row a rayon job handles.
struct Scratch {
    /// Vertical maximum over the rows currently folded in, one entry per column.
    column: Vec<f32>,
    /// The column accumulator with its edges replicated, for the block scheme.
    extended: Vec<f32>,
    /// Per-block running maximum from the block start.
    prefix: Vec<f32>,
    /// Per-block running maximum to the block end.
    suffix: Vec<f32>,
}

impl Scratch {
    fn new(width: usize, max_half_width: usize) -> Self {
        let extended = width + 2 * max_half_width;
        Self {
            column: vec![f32::NEG_INFINITY; width],
            extended: vec![f32::NEG_INFINITY; extended],
            prefix: vec![f32::NEG_INFINITY; extended],
            suffix: vec![f32::NEG_INFINITY; extended],
        }
    }
}

/// Folds `value` into a running maximum.
///
/// Written as `value > acc` rather than [`f32::max`] deliberately: a `NaN`
/// compares false against everything, so it is ignored and never becomes the
/// running maximum. `f32::max` would return the other operand and let a `NaN`
/// input propagate differently. Every accumulator below starts at negative
/// infinity so that this holds uniformly.
#[inline]
fn fold(acc: &mut f32, value: f32) {
    if value > *acc {
        *acc = value;
    }
}

/// Filters one row: `out` is the single output row for `y`.
///
/// The column accumulator holds the vertical maximum over rows
/// `y - grown ..= y + grown`. Rectangles are visited shortest first, so the
/// accumulator only ever grows, and each source row is folded in once.
fn filter_row(scan: &Scan<'_>, scratch: &mut Scratch, out: &mut [f32], y: usize) {
    let Scratch {
        column,
        extended,
        prefix,
        suffix,
    } = scratch;

    out.fill(f32::NEG_INFINITY);
    column.fill(f32::NEG_INFINITY);
    fold_row(column, scan.row(y));

    // Rows already folded in. Once these stick at the image edge the clamped
    // read repeats, and folding the same row again cannot change the maximum,
    // so it is skipped.
    let (mut lowest, mut highest) = (y, y);

    for rect in scan.plan.rects() {
        for distance in 1..=rect.half_height {
            let above = y.saturating_sub(distance);
            if above != lowest {
                fold_row(column, scan.row(above));
                lowest = above;
            }
            let below = (y + distance).min(scan.height - 1);
            if below != highest {
                fold_row(column, scan.row(below));
                highest = below;
            }
        }
        horizontal_max(column, out, rect.half_width, extended, prefix, suffix);
    }

    if scan.negate_output {
        for value in out.iter_mut() {
            *value = -*value;
        }
    }
}

/// `column[x] = max(column[x], row[x])` for every column.
///
/// Both slices are contiguous and equal-length, so this compiles to a plain
/// vectorised maximum — the reason the decomposition is worth having.
#[inline]
fn fold_row(column: &mut [f32], row: &[f32]) {
    for (acc, &value) in column.iter_mut().zip(row) {
        fold(acc, value);
    }
}

/// `out[x] = max(out[x], max over dx in [-half_width, half_width] of
/// column[clamp(x + dx)])`.
fn horizontal_max(
    column: &[f32],
    out: &mut [f32],
    half_width: usize,
    extended: &mut [f32],
    prefix: &mut [f32],
    suffix: &mut [f32],
) {
    // Two ways to do this, and the cheaper one depends on the window. Shifting
    // the row costs `2 * half_width + 1` passes; the block scheme below costs
    // four regardless of width. They cross at `half_width == 2` (five passes
    // against four), so 3 is the first width where the block scheme is clearly
    // ahead — and small windows dominate at the radii cell detection uses.
    if half_width < 3 {
        for dx in -(half_width as isize)..=(half_width as isize) {
            fold_shifted(out, column, dx);
        }
    } else {
        horizontal_max_blocks(column, out, half_width, extended, prefix, suffix);
    }
}

/// `out[x] = max(out[x], column[clamp(x + dx)])`.
///
/// Split into three ranges so the middle one — almost the whole row — is a
/// contiguous slice against a contiguous slice with no clamping arithmetic.
fn fold_shifted(out: &mut [f32], column: &[f32], dx: isize) {
    let width = column.len();
    let shift = dx.unsigned_abs();

    if shift >= width {
        // The shift moves every read past an edge, so the clamped value is the
        // same for the whole row. Only reachable on rows narrower than the
        // kernel, but that is exactly where the general case below would index
        // out of range.
        let value = if dx < 0 { column[0] } else { column[width - 1] };
        for acc in out.iter_mut() {
            fold(acc, value);
        }
        return;
    }

    // `x + dx` is inside the row for x in [lo, hi), reading from `start`;
    // outside it the clamped read is the first or last pixel.
    let (lo, hi, start) = if dx < 0 {
        (shift, width, 0)
    } else {
        (0, width - shift, shift)
    };

    for acc in &mut out[..lo] {
        fold(acc, column[0]);
    }
    for (acc, &value) in out[lo..hi].iter_mut().zip(&column[start..]) {
        fold(acc, value);
    }
    for acc in &mut out[hi..] {
        fold(acc, column[width - 1]);
    }
}

/// van Herk / Gil-Werman windowed maximum: O(1) per pixel, whatever the width.
///
/// The row is cut into blocks exactly as long as the window. Within a block,
/// `prefix[e]` is the maximum from the block start to `e` and `suffix[e]` the
/// maximum from `e` to the block end. Any window of that length spans at most
/// two adjacent blocks, so `max(suffix[left], prefix[right])` is exactly the
/// window maximum and never reaches beyond it — which is why the block length
/// must equal the window length and not merely exceed it.
///
/// Edge clamping is handled by replicating the first and last pixel into an
/// extended row, so the scan itself has no boundary cases.
fn horizontal_max_blocks(
    column: &[f32],
    out: &mut [f32],
    half_width: usize,
    extended: &mut [f32],
    prefix: &mut [f32],
    suffix: &mut [f32],
) {
    let width = column.len();
    let span = 2 * half_width + 1;
    let len = width + 2 * half_width;

    let extended = &mut extended[..len];
    extended[..half_width].fill(column[0]);
    extended[half_width..half_width + width].copy_from_slice(column);
    extended[half_width + width..].fill(column[width - 1]);

    let prefix = &mut prefix[..len];
    let suffix = &mut suffix[..len];
    for start in (0..len).step_by(span) {
        let end = (start + span).min(len);

        let mut running = f32::NEG_INFINITY;
        for (acc, &value) in prefix[start..end].iter_mut().zip(&extended[start..end]) {
            fold(&mut running, value);
            *acc = running;
        }

        let mut running = f32::NEG_INFINITY;
        for (acc, &value) in suffix[start..end]
            .iter_mut()
            .zip(&extended[start..end])
            .rev()
        {
            fold(&mut running, value);
            *acc = running;
        }
    }

    for ((acc, &left), &right) in out
        .iter_mut()
        .zip(&suffix[..width])
        .zip(&prefix[2 * half_width..])
    {
        let mut best = left;
        fold(&mut best, right);
        fold(acc, best);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_dilates_an_impulse_into_the_kernel() {
        let mut pixels = vec![0.0f32; 81];
        pixels[40] = 50.0; // centre of a 9x9
        rank(&mut pixels, 9, 2.0, RankOp::Max);
        assert_eq!(
            pixels.iter().filter(|&&v| v > 0.0).count(),
            Kernel::new(2.0).area()
        );
    }

    #[test]
    fn min_erodes_a_single_dark_pixel() {
        let mut pixels = vec![10.0f32; 81];
        pixels[40] = 0.0;
        rank(&mut pixels, 9, 2.0, RankOp::Min);
        assert_eq!(
            pixels.iter().filter(|&&v| v == 0.0).count(),
            Kernel::new(2.0).area()
        );
    }

    #[test]
    fn a_flat_image_is_unchanged() {
        let mut pixels = vec![7.0f32; 36];
        rank(&mut pixels, 6, 3.0, RankOp::Max);
        assert!(pixels.iter().all(|&v| v == 7.0));
    }

    #[test]
    fn edges_clamp_rather_than_darkening() {
        // A uniformly bright image must stay bright at the border: clamping
        // reads the edge pixel, it does not read an imaginary zero.
        let mut pixels = vec![5.0f32; 25];
        rank(&mut pixels, 5, 2.0, RankOp::Min);
        assert!(pixels.iter().all(|&v| v == 5.0));
    }

    #[test]
    fn min_and_max_bracket_the_input() {
        let source: Vec<f32> = (0..64).map(|i| (i % 7) as f32).collect();
        let mut mins = source.clone();
        let mut maxs = source.clone();
        rank(&mut mins, 8, 2.0, RankOp::Min);
        rank(&mut maxs, 8, 2.0, RankOp::Max);
        for i in 0..source.len() {
            assert!(mins[i] <= source[i], "min must not exceed the input");
            assert!(maxs[i] >= source[i], "max must not fall below the input");
        }
    }

    /// The definition, applied literally: max over every kernel offset with
    /// clamped reads. Slow, obviously correct, and independent of everything
    /// the fast path does.
    fn reference(pixels: &[f32], width: usize, radius: f64, op: RankOp) -> Vec<f32> {
        let height = pixels.len() / width;
        let kernel = Kernel::new(radius);
        let sign = if matches!(op, RankOp::Min) { -1.0 } else { 1.0 };
        let mut out = vec![0.0f32; pixels.len()];

        for y in 0..height {
            for x in 0..width {
                let mut best = f32::NEG_INFINITY;
                for dy in -kernel.radius_rows()..=kernel.radius_rows() {
                    let half = kernel.half_width(dy);
                    if half < 0 {
                        continue;
                    }
                    let sy = (y as isize + dy as isize).clamp(0, height as isize - 1) as usize;
                    for dx in -half..=half {
                        let sx = (x as isize + dx as isize).clamp(0, width as isize - 1) as usize;
                        fold(&mut best, pixels[sy * width + sx] * sign);
                    }
                }
                out[y * width + x] = best * sign;
            }
        }
        out
    }

    #[test]
    fn matches_the_literal_definition_over_many_shapes() {
        // The decomposition, the block scheme and the edge clamping all have to
        // agree with the definition at once. Non-square images and radii on
        // both sides of the block-scheme threshold are the cases where an
        // off-by-one in any of the three would survive the smaller tests above.
        for &(width, height) in &[(1, 9), (9, 1), (13, 7), (7, 13), (32, 32), (5, 5)] {
            let pixels: Vec<f32> = (0..width * height)
                .map(|i| ((i * 37) % 101) as f32 - 50.0)
                .collect();
            for radius in [0.0, 1.0, 1.5, 2.0, 2.5, 3.0, 5.0, 8.0, 15.0] {
                for op in [RankOp::Min, RankOp::Max] {
                    let mut actual = pixels.clone();
                    rank(&mut actual, width, radius, op);
                    assert_eq!(
                        actual,
                        reference(&pixels, width, radius, op),
                        "{op:?} radius {radius} on {width}x{height}"
                    );
                }
            }
        }
    }

    #[test]
    fn nan_inputs_are_ignored_rather_than_propagated() {
        // Not pinned against ImageJ — no recorded fixture contains NaN — but
        // pinned here because the accumulator's comparison order decides it,
        // and the previous implementation ignored NaN the same way.
        let mut pixels = vec![1.0f32; 25];
        pixels[12] = f32::NAN;
        rank(&mut pixels, 5, 1.0, RankOp::Max);
        assert!(
            pixels.iter().all(|&v| v == 1.0),
            "a NaN must not become the maximum"
        );
    }

    #[test]
    fn result_is_independent_of_thread_count() {
        // §5 requires parallel work to be reproducible. MIN/MAX select an
        // existing value rather than accumulating, and rows never interact, so
        // the result must be bit-identical however rayon splits the work.
        let width = 64;
        let source: Vec<f32> = (0..width * 64).map(|i| ((i * 31) % 97) as f32).collect();

        let mut reference = source.clone();
        rank(&mut reference, width, 3.0, RankOp::Max);

        for threads in [1usize, 2, 8] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("thread pool");
            let mut pixels = source.clone();
            pool.install(|| rank(&mut pixels, width, 3.0, RankOp::Max));
            assert_eq!(pixels, reference, "diverged with {threads} threads");
        }
    }

    #[test]
    fn a_reused_filter_matches_a_fresh_one() {
        // The point of `RankFilter` is that the buffers survive between calls;
        // stale contents leaking into a later result would be the failure mode.
        let width = 16;
        let mut filter = RankFilter::new();
        for radius in [2.0, 5.0, 2.0, 1.0] {
            for len in [width * 16, width * 4, width * 16] {
                let source: Vec<f32> = (0..len).map(|i| ((i * 13) % 71) as f32).collect();

                let mut reused = source.clone();
                filter.apply(&mut reused, width, radius, RankOp::Max);

                let mut fresh = source.clone();
                rank(&mut fresh, width, radius, RankOp::Max);

                assert_eq!(reused, fresh, "radius {radius}, {len} pixels");
            }
        }
    }

    #[test]
    fn filtering_into_a_buffer_matches_filtering_in_place() {
        // `apply_into` is the path that copies nothing for MAX, so it is also
        // the path where a missing copy would show up as reading half-written
        // data. Both ops, both sides of the block-scheme threshold.
        let width = 24;
        let source: Vec<f32> = (0..width * 20).map(|i| ((i * 29) % 83) as f32).collect();
        let mut filter = RankFilter::new();

        for radius in [1.0, 2.0, 5.0, 9.0] {
            for op in [RankOp::Min, RankOp::Max] {
                let mut in_place = source.clone();
                filter.apply(&mut in_place, width, radius, op);

                let mut out = vec![0.0f32; source.len()];
                filter.apply_into(&source, &mut out, width, radius, op);

                assert_eq!(out, in_place, "{op:?} radius {radius}");
            }
        }
    }

    #[test]
    #[should_panic(expected = "same image")]
    fn mismatched_output_length_panics() {
        let mut out = vec![0.0f32; 8];
        RankFilter::new().apply_into(&[1.0; 9], &mut out, 3, 1.0, RankOp::Max);
    }

    #[test]
    #[should_panic(expected = "width must divide")]
    fn ragged_buffer_panics() {
        let mut pixels = vec![0.0f32; 7];
        rank(&mut pixels, 3, 1.0, RankOp::Max);
    }

    #[test]
    fn a_poisoned_output_is_fully_overwritten() {
        // The pipeline stopped zeroing the buffer it hands this between tiles,
        // because the filter writes every pixel of it. That is a convention, so
        // it is pinned rather than trusted: an output arriving full of a
        // previous tile's values has to come back the same as one arriving
        // zeroed.
        let width = 9;
        let source: Vec<f32> = (0..width * 7).map(|i| (i % 13) as f32 - 4.0).collect();
        for op in [RankOp::Min, RankOp::Max] {
            let mut zeroed = vec![0.0f32; source.len()];
            let mut poisoned = vec![f32::NAN; source.len()];
            let mut filter = RankFilter::new();
            filter.apply_into(&source, &mut zeroed, width, 3.5, op);
            filter.apply_into(&source, &mut poisoned, width, 3.5, op);
            assert_eq!(zeroed, poisoned, "{op:?} left a pixel of the output unwritten");
        }
    }
}
