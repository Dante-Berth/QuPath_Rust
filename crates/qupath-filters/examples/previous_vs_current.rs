//! A/B probe: one output pixel in flight against eight, interleaved.
//!
//! ImageJ's `convolveLine` accumulates one output pixel at a time, and the
//! accumulator is a loop-carried dependency: each `result +=` waits on the
//! previous one, so the interior runs at the latency of a multiply-add rather
//! than its throughput, and nothing else can overlap with it.
//!
//! Carrying eight output pixels at once makes the lanes independent of one
//! another. The taps they read overlap almost entirely, so this costs no extra
//! loads — it only stops the chain.
//!
//! **The arithmetic is untouched.** Every lane accumulates exactly what the
//! single-pixel loop would give that pixel: centre tap first, then k ascending,
//! same kernel, same order. This is a rewrite, not a reassociation, so the two
//! must agree **bit for bit** — and every timed iteration asserts it, so a
//! version that got fast by computing something else fails here rather than
//! reporting a speedup.
//!
//! Transposing the image so the column pass runs contiguously was tried first
//! and was **slower** at every size — 0.82x to 0.98x. The column pass already
//! copies each column into a contiguous buffer before convolving it, so only
//! the copy-in and the store were strided, and two full transposes cost more
//! than they saved. `docs/benchmarks/README.md` records it.
//!
//! Both run in one process on the same buffers, and the statistic is the
//! minimum over many repetitions, because this machine is not quiet.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "scratch probe")]

use std::hint::black_box;
use std::time::Instant;

use qupath_filters::GaussianBlur;

/// A tile of plausible deconvolved optical density.
///
/// Runs negative over the background, as a real stain amount does now that the
/// clamp at zero is gone. The blur's cost is data-independent — the same loads
/// and arithmetic run whatever the values are — so this measures what a real
/// tile would.
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
        "ImageJ Gaussian blur, one output pixel in flight against eight, \
         minimum of {reps} interleaved repetitions\n"
    );
    println!(
        "{:>12}  {:>6}  {:>11}  {:>11}  {:>7}",
        "tile", "sigma", "one", "eight", "gain"
    );

    // 256x256 is the tile size CMU-1 stores and the pipeline processes; the
    // others bracket it. Sigma 6 is there because the gain should grow with the
    // kernel — a longer chain is a longer stall — and a single sigma would not
    // show that.
    let cases = [
        (128usize, 3.0f64),
        (256, 3.0),
        (256, 6.0),
        (512, 3.0),
        (1024, 3.0),
    ];

    let mut blur = GaussianBlur::new();
    for (size, sigma) in cases {
        let source = tile(size, size);
        let (mut narrow, mut wide) = (source.clone(), source.clone());
        let (mut narrow_best, mut wide_best) = (f64::MAX, f64::MAX);

        for _ in 0..reps {
            narrow.copy_from_slice(&source);
            let mark = Instant::now();
            blur.blur_narrow(black_box(&mut narrow), size, sigma);
            narrow_best = narrow_best.min(mark.elapsed().as_secs_f64());

            wide.copy_from_slice(&source);
            let mark = Instant::now();
            blur.blur(black_box(&mut wide), size, sigma);
            wide_best = wide_best.min(mark.elapsed().as_secs_f64());

            // Every iteration, not once at the end: a difference that appeared
            // only on some runs would be a reuse bug in the scratch buffers,
            // and checking once would miss it.
            assert!(
                narrow
                    .iter()
                    .zip(&wide)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{size}x{size} sigma {sigma}: the two widths diverged"
            );
        }

        println!(
            "{:>9}²  {sigma:>6}  {:>9.3} ms  {:>9.3} ms  {:>6.2}x",
            size,
            narrow_best * 1e3,
            wide_best * 1e3,
            narrow_best / wide_best
        );
    }

    println!("\nOutput asserted bit-identical on every timed iteration.");
}
