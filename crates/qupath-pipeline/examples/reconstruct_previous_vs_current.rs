//! How long the raster passes should repeat before the queue takes over.
//!
//! `morphologicalReconstruction` alternates raster passes while one still moves
//! more than a tenth of the image, then hands the rest to a FIFO queue. That
//! tenth is a speed heuristic and nothing else: the reconstruction has a unique
//! fixed point, so every setting below computes the **same image** and only the
//! route differs. ROADMAP §1.4 established that by mutation — turning the queue
//! from FIFO to LIFO, and deleting the repeat loop entirely, both left every
//! recorded case bit-identical.
//!
//! It is measured **on a real tile** rather than a synthetic surface, because
//! the two disagree about the answer. A smooth synthetic image settles in four
//! raster passes and makes the repeat loop look free; a tile of CMU-1 takes
//! fourteen, because tissue gives the geodesic dilation long winding paths to
//! propagate along and a raster pass only carries it as far as a monotone path
//! reaches. Measuring this on the synthetic case would have picked the wrong
//! setting and reported a speedup that does not exist on slides.
//!
//! Per `docs/benchmarks/README.md` the settings run **interleaved in one
//! process**, the statistic is the **minimum** over repetitions, and every timed
//! iteration **asserts the output is bit-identical** to the reference.
#![allow(missing_docs, reason = "example binary")]

use std::time::Instant;

use qupath_filters::{RankFilter, RankOp, Reconstructor};
use qupath_pipeline::Stains;
use qupath_slide::Slide;

/// Repeat fractions to compare. Zero means no repeats at all: one forward pass,
/// one backward pass, then the queue, which is the textbook hybrid.
const FRACTIONS: [usize; 6] = [0, 2, 4, 10, 20, 50];

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let side: u32 = args.next().and_then(|arg| arg.parse().ok()).unwrap_or(2120);

    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    // The same pixels the background stage meets: haematoxylin optical density
    // of a real tile, and the radius-16 erosion of it as the marker.
    let rgb = slide
        .read_region_rgb(0, 27776, 5952, side, side)
        .expect("read region");
    let mut mask = Vec::new();
    Stains::h_and_e().haematoxylin_into(&rgb, &mut mask);

    let width = side as usize;
    let radius = 8.0 / 0.4990;
    let mut eroded = vec![0.0f32; mask.len()];
    RankFilter::new().apply_into(&mask, &mut eroded, width, radius, RankOp::Min);

    let mut reference = eroded.clone();
    Reconstructor::new().reconstruct(&mut reference, &mask, width);

    let repetitions = 7;
    let megapixels = mask.len() as f64 / 1e6;
    println!(
        "CMU-1 tile {side}x{side} = {megapixels:.2} Mpx, \
         minimum of {repetitions} interleaved repetitions\n"
    );
    println!(
        "{:>8} {:>8} {:>10} {:>9} {:>8} {:>8}",
        "fraction", "passes", "queued", "time s", "s/Mpx", "vs 1/10"
    );

    let mut best = vec![f64::INFINITY; FRACTIONS.len()];
    let mut passes = vec![0u32; FRACTIONS.len()];
    let mut queued = vec![0u64; FRACTIONS.len()];
    let mut working = eroded.clone();

    for _ in 0..repetitions {
        for (slot, &fraction) in FRACTIONS.iter().enumerate() {
            let mut reconstructor = Reconstructor::new().with_repeat_fraction(fraction);
            working.copy_from_slice(&eroded);

            let mark = Instant::now();
            reconstructor.reconstruct(&mut working, &mask, width);
            let elapsed = mark.elapsed().as_secs_f64();

            assert!(
                working == reference,
                "repeat fraction {fraction} computed a different image; \
                 the fixed point must not depend on the traversal"
            );

            best[slot] = best[slot].min(elapsed);
            passes[slot] = reconstructor.last_passes();
            queued[slot] = reconstructor.last_queued();
        }
    }

    let baseline = best[FRACTIONS.iter().position(|&f| f == 10).expect("10 listed")];
    for (slot, &fraction) in FRACTIONS.iter().enumerate() {
        let label = if fraction == 0 {
            "none".to_owned()
        } else {
            format!("1/{fraction}")
        };
        println!(
            "{label:>8} {:>8} {:>10} {:>9.4} {:>8.4} {:>7.2}x",
            passes[slot],
            queued[slot],
            best[slot],
            best[slot] / megapixels,
            baseline / best[slot],
        );
    }

    println!(
        "\nEvery row computed a bit-identical image; the assertion above is what\n\
         makes that a claim rather than an expectation. `1/10` is QuPath's own\n\
         setting and the baseline for the last column."
    );
}
