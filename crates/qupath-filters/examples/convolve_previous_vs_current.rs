//! A/B probe: one output pixel in flight against eight, interleaved.
//!
//! The same defect the Gaussian blur had, in a different file. ImageJ's
//! `Convolver` accumulates one output pixel at a time and `sum +=` is a
//! loop-carried dependency, so the nine taps of the Laplacian run at the
//! latency of an `f64` add rather than its throughput. In the pipeline that
//! made a nine-tap stencil cost *more* than the separable twenty-nine-tap blur
//! ahead of it — 19.8% of stage time against 14.6% — which is the sort of ratio
//! that says the cost is a stall, not the arithmetic.
//!
//! Carrying eight output pixels at once makes the lanes independent. Adjacent
//! output pixels read taps that overlap almost entirely, so this costs no extra
//! loads; it only stops the chain.
//!
//! **The arithmetic is untouched.** Every lane accumulates exactly what the
//! single-pixel loop would give that pixel, in the same tap order, so the two
//! must agree **bit for bit** — and every timed iteration asserts it, so a
//! version that got fast by computing something else fails here rather than
//! reporting a speedup.
//!
//! Both run in one process on the same buffers, and the statistic is the
//! minimum over many repetitions, because this machine is not quiet.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "scratch probe")]

use std::hint::black_box;
use std::time::Instant;

use qupath_filters::Convolver;

/// The kernel `WatershedCellDetection` convolves with.
const LAPLACIAN: [f32; 9] = [0.0, -1.0, 0.0, -1.0, 4.0, -1.0, 0.0, -1.0, 0.0];

/// A wider kernel, to show the gain grows with the chain rather than being a
/// property of this one stencil.
const WIDE: [f32; 25] = [
    1.0, 0.0, -1.0, 0.0, 2.0, 0.0, -2.0, 0.0, 1.0, 0.0, 3.0, 0.0, -1.0, 0.0, -3.0, 1.0, 0.0, -1.0,
    0.0, 2.0, 0.0, -2.0, 0.0, 1.0, 0.0,
];

/// A tile of plausible blurred optical density.
///
/// Runs negative over the background, as a real stain amount does now that the
/// clamp at zero is gone. The convolution's cost is data-independent — the same
/// loads and arithmetic run whatever the values are — so this measures what a
/// real tile would.
fn tile(width: usize, height: usize) -> Vec<f32> {
    (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            let nucleus = (x / 8) % 3 == 0 && (y / 8) % 3 == 0;
            if nucleus {
                1.4 + ((x + y) % 7) as f32 / 40.0
            } else {
                -0.2 + ((x * 7 + y * 13) % 23) as f32 / 200.0
            }
        })
        .collect()
}

fn main() {
    let reps = 200;
    println!(
        "ImageJ Convolver, one output pixel in flight against eight, \
         minimum of {reps} interleaved repetitions\n"
    );
    println!(
        "{:>12}  {:>7}  {:>11}  {:>11}  {:>7}",
        "tile", "kernel", "one", "eight", "gain"
    );

    // 256x256 is the tile size CMU-1 stores and the pipeline processes; the
    // others bracket it. The 5x5 case is there to check the gain is not
    // specific to one stencil — and it lands *below* the 3x3 at the same size,
    // which is worth knowing: a longer chain is a longer stall, but twenty-five
    // taps against eight lanes also needs more live accumulators and taps than
    // three does, so the two effects pull against each other. The 3x3 the
    // pipeline actually runs is the case that matters.
    let cases: [(usize, &[f32], usize, usize, &str); 5] = [
        (128, &LAPLACIAN, 3, 3, "3x3"),
        (256, &LAPLACIAN, 3, 3, "3x3"),
        (512, &LAPLACIAN, 3, 3, "3x3"),
        (1024, &LAPLACIAN, 3, 3, "3x3"),
        (256, &WIDE, 5, 5, "5x5"),
    ];

    let mut convolver = Convolver::new();
    for (size, kernel, kw, kh, label) in cases {
        let source = tile(size, size);
        let (mut narrow, mut wide) = (vec![0.0f32; source.len()], vec![0.0f32; source.len()]);
        let (mut narrow_best, mut wide_best) = (f64::MAX, f64::MAX);

        for _ in 0..reps {
            let mark = Instant::now();
            convolver.apply_into_narrow(&source, black_box(&mut narrow), size, kernel, kw, kh);
            narrow_best = narrow_best.min(mark.elapsed().as_secs_f64());

            let mark = Instant::now();
            convolver.apply_into(&source, black_box(&mut wide), size, kernel, kw, kh);
            wide_best = wide_best.min(mark.elapsed().as_secs_f64());

            // Every iteration, not once at the end: a difference that appeared
            // only on some runs would be a reuse bug in the scratch buffers,
            // and checking once would miss it.
            assert!(
                narrow
                    .iter()
                    .zip(&wide)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{size}x{size} {label}: the two widths diverged"
            );
        }

        println!(
            "{:>9}²  {label:>7}  {:>9.3} ms  {:>9.3} ms  {:>6.2}x",
            size,
            narrow_best * 1e3,
            wide_best * 1e3,
            narrow_best / wide_best
        );
    }

    println!("\nOutput asserted bit-identical on every timed iteration.");
}
