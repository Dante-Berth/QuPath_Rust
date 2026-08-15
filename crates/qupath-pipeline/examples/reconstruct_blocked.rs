//! Whether the reconstruction's raster passes are worth blocking for cache.
//!
//! ROADMAP §4.8 names cache-blocking as one of the two honest answers to the
//! pipeline being memory-bandwidth-bound, and §4.7 leaves the reconstruction at
//! 26.2% of stage time: thirteen sweeps of a 2120 px tile, reading a marker and
//! a mask and writing the marker, none of which fits in any cache.
//!
//! The raster phase is a **heuristic**. The queue that follows it is a complete
//! algorithm on its own, and the fixed point does not depend on the traversal
//! (ROADMAP §1.4, mutation-checked), so any sequence of valid dilation updates
//! ahead of the queue is legal — including one that sweeps a strip of rows
//! several times while it is still in L2 rather than the whole tile once.
//!
//! This probe measures whether that is faster, before anything is written into
//! the library. Both variants end at the same fixed point and the probe asserts
//! it pixel for pixel; a variant that got fast by settling somewhere else fails.
#![allow(missing_docs, reason = "example binary")]

use std::time::{Duration, Instant};

use qupath_filters::{RankFilter, RankOp, Reconstructor};
use qupath_pipeline::{Params, Region, Stains};
use qupath_slide::Slide;

/// One blocked raster phase: strips of `strip` rows, each swept `rounds` times
/// in each direction while it is hot, top to bottom and then bottom to top.
///
/// Strips overlap by one row so a value can cross the seam: the row above a
/// strip is the last row of the strip before it, already settled, and the
/// forward pass reads it. Every update is the same clipped dilation the full
/// pass applies, so the marker only ever rises and never passes the mask.
fn blocked_phase(
    reconstructor: &Reconstructor,
    marker: &mut [f32],
    mask: &[f32],
    width: usize,
    strip: usize,
    rounds: usize,
) -> usize {
    let height = marker.len() / width;
    let mut changes = 0;

    let mut top = 0;
    while top < height {
        let bottom = (top + strip).min(height);
        let from = top.saturating_sub(1) * width;
        let to = bottom * width;
        for _ in 0..rounds {
            changes +=
                reconstructor.raster_pass(&mut marker[from..to], &mask[from..to], width, false);
            changes +=
                reconstructor.raster_pass(&mut marker[from..to], &mask[from..to], width, true);
        }
        top = bottom;
    }

    let mut bottom = height;
    while bottom > 0 {
        let top = bottom.saturating_sub(strip);
        let to = (bottom + 1).min(height) * width;
        let from = top * width;
        for _ in 0..rounds {
            changes +=
                reconstructor.raster_pass(&mut marker[from..to], &mask[from..to], width, true);
            changes +=
                reconstructor.raster_pass(&mut marker[from..to], &mask[from..to], width, false);
        }
        bottom = top;
    }

    changes
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let side: u32 = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(2120);
    let repetitions: usize = std::env::args()
        .nth(3)
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let params = Params::default();
    let region = Region {
        x: 27776,
        y: 5952,
        width: side,
        height: side,
    };
    let rgb = slide
        .read_region_rgb(0, region.x, region.y, region.width, region.height)
        .expect("read region");
    let stains = Stains::h_and_e();
    let mut density = Vec::new();
    stains.haematoxylin_into(&rgb, &mut density);

    let width = side as usize;
    let mut eroded = vec![0.0f32; density.len()];
    RankFilter::new().apply_into(
        &density,
        &mut eroded,
        width,
        params.background_radius,
        RankOp::Min,
    );

    let pixels = density.len();
    let mut reconstructor = Reconstructor::new();

    // The reference: `reconstruct` exactly as the pipeline calls it.
    let mut expected = eroded.clone();
    reconstructor.reconstruct(&mut expected, &density, width);
    let reference_passes = reconstructor.last_passes();
    let reference_queued = reconstructor.last_queued();

    println!(
        "{side}x{side}, {pixels} px. Reference: {reference_passes} passes, \
         {reference_queued} queued.\n"
    );
    println!(
        "{:>8} {:>7} {:>8} {:>12} {:>10} {:>8}",
        "strip", "rounds", "sweeps", "raster ms", "queued", "total ms"
    );

    let mut baseline = Duration::MAX;
    let mut working = eroded.clone();
    for _ in 0..repetitions {
        working.copy_from_slice(&eroded);
        let mark = Instant::now();
        reconstructor.reconstruct(&mut working, &density, width);
        baseline = baseline.min(mark.elapsed());
        assert_eq!(working, expected, "the reference moved between repetitions");
    }
    println!(
        "{:>8} {:>7} {:>8} {:>12} {:>10} {:>8.1}",
        "whole",
        "-",
        reference_passes,
        "-",
        reference_queued,
        baseline.as_secs_f64() * 1e3,
    );

    // Every blocked variant runs its own raster phase and then hands the result
    // to `reconstruct` with the repeats turned off, which is one forward pass,
    // the queue-filling backward pass, and the drain.
    let mut settle = Reconstructor::new().with_repeat_fraction(0);
    for strip in [32usize, 64, 128, 256, 512] {
        for rounds in [1usize, 2, 3] {
            let mut best = Duration::MAX;
            let mut best_raster = Duration::MAX;
            let mut sweeps = 0;
            let mut queued = 0;
            for _ in 0..repetitions {
                working.copy_from_slice(&eroded);
                let mark = Instant::now();
                let mut phases = 0;
                loop {
                    let changes =
                        blocked_phase(&reconstructor, &mut working, &density, width, strip, rounds);
                    phases += 1;
                    if changes * 10 <= pixels || phases > 8 {
                        break;
                    }
                }
                let raster = mark.elapsed();
                settle.reconstruct(&mut working, &density, width);
                let total = mark.elapsed();
                best = best.min(total);
                best_raster = best_raster.min(raster);
                sweeps = phases * rounds * 4;
                queued = settle.last_queued();
                assert_eq!(
                    working, expected,
                    "blocked phase reached a different fixed point"
                );
            }
            println!(
                "{strip:>8} {rounds:>7} {sweeps:>8} {:>12.1} {queued:>10} {:>8.1}",
                best_raster.as_secs_f64() * 1e3,
                best.as_secs_f64() * 1e3,
            );
        }
    }

    println!(
        "\nMinimum of {repetitions}, interleaved with nothing else, output asserted\n\
         identical to the reference on every repetition. `sweeps` counts strip\n\
         passes, which each touch a fraction of the tile — it is not comparable\n\
         to the reference's whole-tile pass count."
    );
}
