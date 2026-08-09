//! Circular-kernel MIN and MAX rank filters, matching ImageJ's `RankFilters`
//! as QuPath invokes it from `WatershedCellDetection`.
//!
//! These are grayscale morphological erosion and dilation. The profile
//! (`docs/profiles/README.md`) puts `RankFilters.getSideMax` and `getAreaMax`
//! at 11.6% of cell-detection self time, the largest cost after the watershed
//! queue.
//!
//! # This implementation is slower than ImageJ's
//!
//! Measured at 7.6× slower at radius 2 and 35.6× at radius 5
//! (`docs/benchmarks/README.md`). ImageJ maintains a running extremum as the
//! kernel slides, which is O(1) per pixel; this is the naive O(kernel area)
//! form, and it is single-threaded where ImageJ's is not.
//!
//! It is kept for the parity fixtures, which pin ImageJ's kernel geometry
//! exactly and are what a faster rewrite would be checked against. **Do not
//! put this on a hot path, and do not claim a speedup for it.** Replacing the
//! inner loop with a van Herk/Gil-Werman running extremum is the intended next
//! step.
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

    for y in 0..height {
        for x in 0..width {
            pixels[y * width + x] = extremum(&source, width, height, x, y, &kernel, op);
        }
    }
}

/// Returns the min or max over the kernel centred at `(x, y)`.
fn extremum(
    source: &[f32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    kernel: &Kernel,
    op: RankOp,
) -> f32 {
    let mut best = match op {
        RankOp::Min => f32::INFINITY,
        RankOp::Max => f32::NEG_INFINITY,
    };

    for dy in -kernel.radius_rows()..=kernel.radius_rows() {
        let half = kernel.half_width(dy);
        if half < 0 {
            continue;
        }
        let row = clamp_coord(y, dy, height);
        let base = row * width;

        // Iterating the clamped span directly lets the bounds check fall out of
        // the inner loop, which is what makes this autovectorisable.
        for dx in -half..=half {
            let column = clamp_coord(x, dx, width);
            let value = source[base + column];
            best = match op {
                RankOp::Min => best.min(value),
                RankOp::Max => best.max(value),
            };
        }
    }

    best
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
    #[should_panic(expected = "width must divide")]
    fn ragged_buffer_panics() {
        let mut pixels = vec![0.0f32; 7];
        rank(&mut pixels, 3, 1.0, RankOp::Max);
    }
}
