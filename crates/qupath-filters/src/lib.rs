//! Circular-kernel MIN and MAX rank filters, matching ImageJ's `RankFilters`
//! as QuPath invokes it from `WatershedCellDetection`.
//!
//! These are grayscale morphological erosion and dilation. The profile
//! (`docs/profiles/README.md`) puts `RankFilters.getSideMax` and `getAreaMax`
//! at 11.6% of cell-detection self time, the largest cost after the watershed
//! queue.
//!
//! # Performance
//!
//! Measured at 1.90× ImageJ at radius 2 and 1.04× (parity) at radius 5, both
//! threaded, on the same 512×512 workload (`docs/benchmarks/README.md`).
//!
//! The algorithm is ImageJ's, not a better one: a lazy incremental extremum
//! that rescans the kernel only when the column leaving it held the extremum,
//! making the common case O(kernel height) rather than O(kernel area). The
//! first version of this crate scanned the full kernel per pixel and was
//! 7.6–35.6× *slower* than ImageJ; that history is kept in the benchmark notes
//! because it is the clearest evidence in this repository that the algorithm,
//! not the language, is what decides.
//!
//! The margin over ImageJ therefore comes from layout and per-operation cost,
//! and it narrows as the radius grows.
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

mod kernel;

pub use kernel::Kernel;

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
/// # Panics
///
/// Panics if `width` is zero or does not divide `pixels.len()`.
pub fn rank(pixels: &mut [f32], width: usize, radius: f64, op: RankOp) {
    assert!(width > 0, "width must be non-zero");
    assert_eq!(
        pixels.len() % width,
        0,
        "width must divide the buffer length"
    );

    let height = pixels.len() / width;
    let kernel = Kernel::new(radius);

    // The filter reads a neighbourhood, so it cannot be done in place: a
    // partially-written row would feed back into later pixels.
    let source = pixels.to_vec();

    // MIN is MAX with the sign flipped, as in ImageJ. Running one code path
    // keeps the incremental logic below in a single place.
    let sign = match op {
        RankOp::Min => -1.0,
        RankOp::Max => 1.0,
    };

    // Rows are independent: each reads the untouched `source` and writes only
    // its own output row, so this parallelises without synchronisation. ImageJ
    // threads the same way, and comparing a single-threaded port against a
    // threaded original would not be a like-for-like measurement.
    //
    // Granularity is a row, not a pixel, per §6 of the guidelines. No float
    // reduction crosses rows, so the result does not depend on how rayon splits
    // the work — MIN and MAX select an existing value rather than accumulating.
    let scan = Scan {
        source: &source,
        width,
        height,
        kernel: &kernel,
        sign,
    };
    pixels
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, row)| filter_row(&scan, row, y));
}

/// The image geometry and kernel a row scan works against.
///
/// These five values travel together through every helper below; grouping them
/// keeps the incremental logic readable and the signatures short.
struct Scan<'a> {
    source: &'a [f32],
    width: usize,
    height: usize,
    kernel: &'a Kernel,
    /// `1.0` for MAX, `-1.0` for MIN. Flipping the sign lets one code path
    /// serve both, as ImageJ does.
    sign: f32,
}

/// Filters one row, carrying the running extremum across `x`.
///
/// This is ImageJ's lazy incremental scheme. A full area scan happens once per
/// row; each step right then costs one column scan in the common case:
///
/// - if the entering column's extremum beats the running one, it *is* the new
///   extremum, and nothing else need be read;
/// - otherwise the leaving column decides. If its extremum is below the running
///   one, the running extremum is still somewhere inside the kernel and stands;
/// - only when the leaving column actually held the extremum must the area be
///   rescanned.
///
/// So the per-pixel cost is O(kernel height), not O(kernel area), and the
/// rescan is rare on real images. This is the difference the benchmark measures.
///
/// `out` is the single output row for `y`, not the whole image.
fn filter_row(scan: &Scan<'_>, out: &mut [f32], y: usize) {
    let mut running = f32::NEG_INFINITY;
    let mut full_calculation = true;

    for (x, out_pixel) in out.iter_mut().enumerate() {
        if full_calculation {
            running = scan.area_extremum(x, y, 0);
        } else {
            let entering = scan.side_extremum(x, y, true);
            if entering >= running {
                running = entering;
            } else {
                let leaving = scan.side_extremum(x, y, false);
                if leaving >= running {
                    // The extremum is leaving the kernel, so it has to be found
                    // again. `entering` seeds the scan, and the rightmost column
                    // is skipped because it is already accounted for.
                    running = scan.area_extremum(x, y, 1).max(entering);
                }
            }
        }
        // After the first pixel of a row the incremental path is valid.
        full_calculation = false;

        *out_pixel = running * scan.sign;
    }
}

impl Scan<'_> {
    /// Scans the whole kernel at `(x, y)`, ignoring `ignore_right` columns at
    /// the right edge.
    fn area_extremum(&self, x: usize, y: usize, ignore_right: i32) -> f32 {
        let mut best = f32::NEG_INFINITY;

        for dy in -self.kernel.radius_rows()..=self.kernel.radius_rows() {
            let half = self.kernel.half_width(dy);
            if half < 0 {
                continue;
            }
            let base = clamp_coord(y, dy, self.height) * self.width;
            for dx in -half..=(half - ignore_right) {
                let value = self.source[base + clamp_coord(x, dx, self.width)] * self.sign;
                if value > best {
                    best = value;
                }
            }
        }

        best
    }

    /// Scans the single column entering the kernel (`right`) or leaving it.
    ///
    /// Entering at `x` means the column at `x + half_width`; leaving means the
    /// one at `x - half_width - 1`, which was inside the kernel at `x - 1`.
    fn side_extremum(&self, x: usize, y: usize, right: bool) -> f32 {
        let mut best = f32::NEG_INFINITY;

        for dy in -self.kernel.radius_rows()..=self.kernel.radius_rows() {
            let half = self.kernel.half_width(dy);
            if half < 0 {
                continue;
            }
            let base = clamp_coord(y, dy, self.height) * self.width;
            let dx = if right { half } else { -half - 1 };
            let value = self.source[base + clamp_coord(x, dx, self.width)] * self.sign;
            if value > best {
                best = value;
            }
        }

        best
    }
}

/// Offsets `coord` by `delta`, clamping to `[0, extent)` as ImageJ does.
#[inline]
fn clamp_coord(coord: usize, delta: i32, extent: usize) -> usize {
    let shifted = coord as isize + delta as isize;
    if shifted < 0 {
        0
    } else {
        (shifted as usize).min(extent - 1)
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
    #[should_panic(expected = "width must divide")]
    fn ragged_buffer_panics() {
        let mut pixels = vec![0.0f32; 7];
        rank(&mut pixels, 3, 1.0, RankOp::Max);
    }
}
