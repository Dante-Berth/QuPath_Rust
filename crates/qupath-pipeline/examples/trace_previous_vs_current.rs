//! A/B probe: the region tracer against its previous spelling, interleaved.
//!
//! The tracer runs three times per tile — once on the flooded labels, once
//! inside the merge, once on the refined binary — and those three stages are
//! 6.7%, 6.0% and 4.4% of pipeline stage time. ROADMAP §4.6 pointed the first
//! optimisation pass at the bounding-box half, on the grounds that the hole test
//! walks 13.7 M bounding-box pixels against a 17.8 M image.
//!
//! What changed: **the closing scan is skipped when the flood has already
//! accounted for the box.** The flood counts the pixels it reaches inside the
//! box, the region knows its own area, and when the two add up there is nothing
//! left to find. On a real tile that fires for 26,976 of 27,308 regions — a
//! nucleus without a hole no longer pays for its bounding box twice.
//!
//! The change is invisible in the output by construction, and the harness
//! asserts it: the region maps and the region counts must match on every timed
//! iteration, on the real label image and on the real merged binary.
//!
//! **A second change was tried here and reverted.** Popping a flood stack
//! recovers `x` and `y` from the offset with a hardware division, about 4 M of
//! them per tile; carrying packed coordinates instead removes every one and is
//! worth **1.00x**. The row is kept out of the table because the code is gone,
//! but the measurement is the fourth of its kind in this repository and is
//! recorded in ROADMAP §4.6.
//!
//! Measured on a tile of CMU-1 rather than on a generated image, per ROADMAP §5,
//! and interleaved in one process with the minimum over repetitions, per
//! `docs/benchmarks/README.md`.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "scratch probe")]

use std::hint::black_box;
use std::time::Instant;

use qupath_pipeline::{Params, Region};
use qupath_slide::Slide;
use qupath_watershed::RegionTracer;

#[path = "shared/tile.rs"]
mod tile;

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

    let params = Params::default();
    let region = Region {
        x: 27776,
        y: 5952,
        width: side,
        height: side,
    };
    let width = side as usize;
    let tile = tile::Tile::build(&slide, region, params);

    let megapixels = tile.labels.len() as f64 / 1e6;
    let repetitions = 9;
    println!(
        "CMU-1 tile {side}x{side} = {megapixels:.2} Mpx, minimum of {repetitions}\n\
         interleaved repetitions\n"
    );

    let variants = [(0usize, true, "previous"), (1, false, "current")];
    let mut labels_best = [f64::INFINITY; 2];
    let mut mask_best = [f64::INFINITY; 2];
    let mut maps: [Vec<u32>; 2] = Default::default();
    let mut counts = [0u32; 2];
    let mut regions = vec![0u32; tile.labels.len()];

    for _ in 0..repetitions {
        // The first trace: the flooded label image, `[0.5, inf)`.
        for (slot, scan, _) in variants {
            let mut tracer = RegionTracer::new().with_unconditional_scan(scan);
            let mark = Instant::now();
            counts[slot] = tracer.trace_into(
                black_box(&tile.labels),
                black_box(&mut regions),
                width,
                0.5,
                f32::INFINITY,
            );
            labels_best[slot] = labels_best[slot].min(mark.elapsed().as_secs_f64());
            maps[slot].clear();
            maps[slot].extend_from_slice(&regions);
        }
        for slot in 1..2 {
            assert_eq!(
                counts[0], counts[slot],
                "variant {slot} found a different number of regions"
            );
            assert!(
                maps[0] == maps[slot],
                "variant {slot} built a different region map"
            );
        }

        // The second: the merged binary, which is what the merge and the nuclei
        // stages trace.
        for (slot, scan, _) in variants {
            let mut tracer = RegionTracer::new().with_unconditional_scan(scan);
            let mark = Instant::now();
            counts[slot] =
                tracer.trace_mask_into(black_box(&tile.binary), black_box(&mut regions), width);
            mask_best[slot] = mask_best[slot].min(mark.elapsed().as_secs_f64());
            maps[slot].clear();
            maps[slot].extend_from_slice(&regions);
        }
        for slot in 1..2 {
            assert_eq!(
                counts[0], counts[slot],
                "variant {slot} found a different number of regions on the binary"
            );
            assert!(
                maps[0] == maps[slot],
                "variant {slot} built a different region map on the binary"
            );
        }
    }

    println!(
        "{:>12} {:>12} {:>12} {:>12} {:>10}",
        "", "labels s", "binary s", "both s", "vs previous"
    );
    for (slot, _, label) in variants {
        let both = labels_best[slot] + mask_best[slot];
        println!(
            "{label:>12} {:>12.4} {:>12.4} {:>12.4} {:>9.2}x",
            labels_best[slot],
            mask_best[slot],
            both,
            (labels_best[0] + mask_best[0]) / both,
        );
    }

    println!(
        "\nBoth rows produced identical region maps and identical region counts\n\
         on every timed iteration; the assertions above are what\n\
         make that a claim rather than an expectation."
    );
}
