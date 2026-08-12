//! Where the shape split's time goes, on the mask it actually meets.
//!
//! `EDM.toWatershed` is two things — `makeFloatEDM` and `MaximumFinder` in
//! `SEGMENTED` mode — and the pipeline's stage table times them together at
//! 13.9%, the largest stage after the background's two halves. Nothing in that
//! share says which half it is, and ROADMAP §5 has three entries about reading a
//! share as though it said why.
//!
//! The input is the **merged binary of a real tile**, because §5's other
//! standing entry is that a synthetic input can pick the wrong optimisation:
//! `MergeAndSplit.json`'s 96x96 field of random pixels puts nearly every
//! candidate within a pixel of a neighbour, and a tile of CMU-1 does not.
//!
//! # What this answered, and what it cost to find out
//!
//! The split is roughly **a quarter distance map and three quarters
//! segmentation**, and the segmentation is five stages of which none is a
//! hotspot: the maxima scan and the dilation are about a third each, the
//! grouping a sixth, the quantisation and the flattening the rest. Three
//! transformations were tried against it — a branchless interpolated height, a
//! branchless neighbourhood bitmask for the dilation, and a tree-shaped minimum
//! to shorten the height's dependency chain — and together they are worth
//! **1.02x**, which is this machine's noise floor. ROADMAP §4 has the table and
//! what it says about where the cost actually is.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "scratch probe")]

use std::hint::black_box;
use std::time::Instant;

use qupath_pipeline::{Params, Region};
use qupath_slide::Slide;
use qupath_watershed::{DistanceMap, Segmenter};

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
    let binary = tile.binary;
    let foreground = binary.iter().filter(|&&pixel| pixel != 0).count();

    let megapixels = binary.len() as f64 / 1e6;
    println!(
        "CMU-1 tile {side}x{side} = {megapixels:.2} Mpx, {} merged regions,\n\
         {:.1}% foreground\n",
        tile.merged,
        100.0 * foreground as f64 / binary.len() as f64
    );

    let repetitions = 9;
    let mut edm = vec![0.0f32; binary.len()];
    let mut mask = binary.clone();
    let mut distances = DistanceMap::new();
    let mut segmenter = Segmenter::new();

    let mut edm_best = f64::INFINITY;
    let mut segment_best = f64::INFINITY;
    for _ in 0..repetitions {
        mask.copy_from_slice(&binary);

        let mark = Instant::now();
        distances.make_float_edm(black_box(&binary), black_box(&mut edm), width);
        edm_best = edm_best.min(mark.elapsed().as_secs_f64());

        let mark = Instant::now();
        segmenter.segment(black_box(&edm), black_box(&mut mask), width);
        segment_best = segment_best.min(mark.elapsed().as_secs_f64());
    }

    let total = edm_best + segment_best;
    println!("{:>20} {:>10} {:>10} {:>9}", "", "best s", "ns/px", "share");
    for (label, seconds) in [
        ("makeFloatEDM", edm_best),
        ("findMaxima + AND", segment_best),
        ("split", total),
    ] {
        println!(
            "{label:>20} {seconds:>10.4} {:>10.2} {:>8.1}%",
            seconds / (megapixels * 1e6) * 1e9,
            100.0 * seconds / total
        );
    }

    println!(
        "\nMinimum of {repetitions} repetitions each, on the merged binary of a real\n\
         tile rather than on a generated one — ROADMAP §5. The two halves run\n\
         back to back in the pipeline and are timed as one stage there."
    );
}
