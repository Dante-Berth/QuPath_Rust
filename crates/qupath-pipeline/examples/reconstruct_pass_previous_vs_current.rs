//! A/B probe: QuPath's raster pass against one with a shorter dependency chain,
//! interleaved.
//!
//! The reconstruction is 46.5% of pipeline stage time and almost all of it is
//! `dilateAndCompare` — a real tile needs about fourteen raster passes, of which
//! thirteen run without the queue.
//!
//! # What ROADMAP §4.7 expected, and what was actually there
//!
//! The roadmap blamed the transliteration's index arithmetic: an `isize` step
//! carrying the direction, a `first_row` test, a `first_column` test, an
//! `Option` test for the queue, and a negative-index assertion, all inside the
//! inner loop. **Removing every one of them is worth 1.03x.** The loop was
//! already compiling to compare-and-blend with no bounds checks; the tests
//! predicted perfectly and cost nothing.
//!
//! The cost is the **loop-carried dependency**. Each pixel's result is the next
//! pixel's left neighbour, and QuPath's expression puts three dependent
//! compare-and-selects between them. Feeding a constant instead — which computes
//! the wrong answer, so it is a diagnostic and not a variant — runs the pass at
//! **0.62 ns/px against 3.87**. Six-sevenths of the stage is that one chain.
//!
//! `clip` distributes over `top`, so the term that does not involve the left
//! neighbour can be hoisted and two comparisons remain instead of three. That,
//! plus spelling the clip as the strict `<` that `MINSS` implements rather than
//! the negated `>=` that compiles to a compare and a blend, is the whole change.
//!
//! **The arithmetic is not touched, only reassociated.** The same five values,
//! the same tie order, so the two must agree **bit for bit** — and on the change
//! count, which is what the repeat loop reads. Every timed iteration asserts
//! both; `specialising_the_pass_changes_nothing` asserts them per pass over
//! every shape down to 1x1, and `reconstruct` falls back to the transliteration
//! on NaN, the one input where the reassociation is not an identity.
//!
//! Measured **on a real tile**, for the reason `reconstruct_previous_vs_current`
//! gives: a smooth synthetic surface settles in four passes and a tile of CMU-1
//! takes fourteen, so the synthetic case would measure the wrong thing.
//!
//! Per `docs/benchmarks/README.md` the two run interleaved in one process, and
//! the statistic is the minimum over repetitions.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "scratch probe")]

use std::hint::black_box;
use std::time::Instant;

use qupath_filters::{RankFilter, RankOp, Reconstructor};
use qupath_pipeline::Stains;
use qupath_slide::Slide;

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

    // The pixels the background stage meets: haematoxylin optical density of a
    // real tile, and the radius-16 erosion of it as the marker.
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
    let mut probe = Reconstructor::new();
    probe.reconstruct(&mut reference, &mask, width);
    let passes = probe.last_passes();

    let repetitions = 15;
    let megapixels = mask.len() as f64 / 1e6;
    println!(
        "CMU-1 tile {side}x{side} = {megapixels:.2} Mpx, {passes} raster passes,\n\
         minimum of {repetitions} interleaved repetitions\n"
    );

    // The pass on its own. A whole reconstruct also drains the queue, which is
    // unchanged code, so a figure taken from it understates what the pass did.
    let variants = [(0usize, true, "transliterated"), (1, false, "specialised")];
    let mut pass_best = [f64::INFINITY; 2];
    let mut pass_moved = [0usize; 2];
    let mut working = eroded.clone();

    for _ in 0..repetitions {
        for (slot, transliterated, _) in variants {
            let reconstructor = Reconstructor::new().with_transliterated_passes(transliterated);
            working.copy_from_slice(&eroded);

            // A forward pass over the erosion, then a backward pass over what it
            // leaves — the pair the repeat loop runs, on the state it runs on.
            let mark = Instant::now();
            let mut moved =
                reconstructor.raster_pass(black_box(&mut working), black_box(&mask), width, false);
            moved +=
                reconstructor.raster_pass(black_box(&mut working), black_box(&mask), width, true);
            pass_best[slot] = pass_best[slot].min(mark.elapsed().as_secs_f64() / 2.0);
            pass_moved[slot] = moved;
        }
        assert_eq!(
            pass_moved[0], pass_moved[1],
            "the two passes moved different numbers of pixels, so the repeat \
             loop would run a different number of passes"
        );
    }

    // And the whole reconstruction, so the share the pass is worth in place is
    // visible rather than inferred.
    let mut whole_best = [f64::INFINITY; 2];
    for _ in 0..repetitions {
        for (slot, transliterated, label) in variants {
            let mut reconstructor = Reconstructor::new().with_transliterated_passes(transliterated);
            working.copy_from_slice(&eroded);

            let mark = Instant::now();
            reconstructor.reconstruct(black_box(&mut working), black_box(&mask), width);
            let elapsed = mark.elapsed().as_secs_f64();

            assert!(
                working == reference,
                "the {label} pass reconstructed a different image"
            );
            assert_eq!(
                reconstructor.last_passes(),
                passes,
                "the {label} pass disagreed on the change count, so the repeat \
                 loop ran a different number of passes"
            );
            whole_best[slot] = whole_best[slot].min(elapsed);
        }
    }

    println!(
        "{:>16} {:>10} {:>10} {:>12} {:>12}",
        "", "one pass s", "ns/px", "reconstruct s", "queue+rest s"
    );
    for (slot, _, label) in variants {
        println!(
            "{label:>16} {:>10.4} {:>10.2} {:>12.4} {:>12.4}",
            pass_best[slot],
            pass_best[slot] / (megapixels * 1e6) * 1e9,
            whole_best[slot],
            whole_best[slot] - pass_best[slot] * f64::from(passes),
        );
    }
    println!(
        "\n{:>16} {:>10.2}x {:>23.2}x",
        "speedup",
        pass_best[0] / pass_best[1],
        whole_best[0] / whole_best[1],
    );
    println!(
        "\n{} of {} pixels moved in the timed pair.",
        pass_moved[1],
        mask.len(),
    );

    println!(
        "\nBoth rows computed a bit-identical image and needed the same number\n\
         of passes; the two assertions above are what make that a claim rather\n\
         than an expectation. The last column is everything `reconstruct` does\n\
         that is not a raster pass — chiefly the queue drain, which is unchanged\n\
         code and so is the ceiling on what specialising the pass can be worth."
    );
}
