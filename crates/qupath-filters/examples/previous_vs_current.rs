//! A/B probe: the previous (ImageJ-incremental) filter against the current
//! (rectangle-decomposition) one, interleaved in one process.
//!
//! This machine is not quiet — load average is around 3 — and criterion's
//! medians move by up to 3x between runs at small radii as a result. Comparing
//! a benchmark taken now against one taken earlier is therefore not sound.
//! Interleaving the two implementations means both see the same machine, and
//! reporting the *minimum* over many repetitions estimates the uncontended cost
//! rather than the contended one.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "scratch probe")]

use std::hint::black_box;
use std::time::Instant;

use qupath_filters::{Kernel, RankFilter, RankOp};
use rayon::prelude::*;

/// The implementation this change replaces, verbatim from commit aa7b9eb.
mod previous {
    use super::{Kernel, ParallelSliceMut};
    use rayon::prelude::*;

    pub fn rank(pixels: &mut [f32], width: usize, radius: f64) {
        let height = pixels.len() / width;
        let kernel = Kernel::new(radius);
        let source = pixels.to_vec();
        let scan = Scan {
            source: &source,
            width,
            height,
            kernel: &kernel,
            sign: 1.0,
        };
        pixels
            .par_chunks_mut(width)
            .enumerate()
            .for_each(|(y, row)| filter_row(&scan, row, y));
    }

    struct Scan<'a> {
        source: &'a [f32],
        width: usize,
        height: usize,
        kernel: &'a Kernel,
        sign: f32,
    }

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
                        running = scan.area_extremum(x, y, 1).max(entering);
                    }
                }
            }
            full_calculation = false;
            *out_pixel = running * scan.sign;
        }
    }

    impl Scan<'_> {
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

    #[inline]
    fn clamp_coord(coord: usize, delta: i32, extent: usize) -> usize {
        let shifted = coord as isize + delta as isize;
        if shifted < 0 {
            0
        } else {
            (shifted as usize).min(extent - 1)
        }
    }
}

fn main() {
    let width = 512;
    let source: Vec<f32> = (0..width * width)
        .map(|i| ((i * 37) % 251) as f32)
        .collect();
    let reps = 200;

    println!("512x512 f32 MAX, minimum of {reps} interleaved repetitions, microseconds\n");
    println!(
        "{:>6}  {:>10}  {:>10}  {:>10}  {:>8}",
        "radius", "previous", "in-place", "into", "speedup"
    );
    println!(
        "{:>6}  {:>10}  {:>10}  {:>10}  {:>8}",
        "", "(1 copy)", "(1 copy)", "(0 copies)", "in-place"
    );

    for radius in [1.0f64, 2.0, 3.0, 5.0, 8.0, 10.0, 20.0] {
        let mut buf = source.clone();
        let mut out = vec![0.0f32; source.len()];
        let mut filter = RankFilter::new();
        // Warm the reusable buffers so the first timed call is not the one that
        // pays for them.
        filter.apply(&mut buf, width, radius, RankOp::Max);

        let (mut old_best, mut new_best, mut into_best) = (f64::MAX, f64::MAX, f64::MAX);
        let mut expected: Option<Vec<f32>> = None;

        for _ in 0..reps {
            buf.copy_from_slice(&source);
            let start = Instant::now();
            previous::rank(&mut buf, width, radius);
            old_best = old_best.min(start.elapsed().as_secs_f64() * 1e6);
            black_box(&buf);
            let from_previous = buf.clone();

            // In place, so one copy of the image happens inside the timed
            // region — the same shape as ImageJ's harness, which clones the
            // pixel array inside its own timed region.
            buf.copy_from_slice(&source);
            let start = Instant::now();
            filter.apply(&mut buf, width, radius, RankOp::Max);
            new_best = new_best.min(start.elapsed().as_secs_f64() * 1e6);
            black_box(&buf);

            // The two must agree exactly, or the timing is measuring different
            // work. MIN/MAX select an input value, so this is not a tolerance.
            assert_eq!(buf, from_previous, "implementations diverged at r={radius}");
            expected.get_or_insert_with(|| buf.clone());

            // Out of place: separate input and output tiles, nothing copied.
            let start = Instant::now();
            filter.apply_into(&source, &mut out, width, radius, RankOp::Max);
            into_best = into_best.min(start.elapsed().as_secs_f64() * 1e6);
            black_box(&out);
            assert_eq!(
                Some(&out),
                expected.as_ref(),
                "out-of-place diverged at r={radius}"
            );
        }

        println!(
            "{radius:>6}  {old_best:>10.1}  {new_best:>10.1}  {into_best:>10.1}  {:>7.2}x",
            old_best / new_best
        );
    }
}
