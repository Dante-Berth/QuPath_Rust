//! What the tile grid alone does to the object count.
//!
//! ROADMAP §2 called tile seams "a correctness bug the current pipeline has"
//! and predicted they inflate every count. This measures the size of that
//! inflation by running the same region and the same stages over three grids
//! and changing nothing else:
//!
//! - **storage grid** — the slide's own 240 px TIFF tiles with no halo, which is
//!   what the pipeline did before this was fixed;
//! - **QuPath's grid** — `PREFERRED_TILE_SIZE` 2048 and `MAX_TILE_SIZE` 3072 at
//!   this slide's downsample, padded by `getTileOverlap`;
//! - **one tile** — the whole region in a single pass, which has no seam at all
//!   and so is the number the other two are trying to reproduce.
//!
//! The single-tile run is the reference here rather than an optimisation: with
//! no seam it needs no overlap resolution, so its count is free of both the
//! truncation the storage grid causes and the double-counting the halo causes.
#![allow(missing_docs, reason = "example binary")]

use qupath_pipeline::{Outcome, Params, Region, run};
use qupath_slide::Slide;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let region = Region {
        x: 27776,
        y: 5952,
        width: 4000,
        height: 4000,
    };

    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let defaults = Params::default();
    let grids = [
        (
            "storage grid, 240 px, no halo",
            Params {
                tile_preferred: 240,
                tile_max: 240,
                tile_overlap: 0,
                ..defaults
            },
        ),
        ("QuPath's grid, 2052/3078, 60 px halo", defaults),
        (
            "one tile, no seam",
            Params {
                tile_preferred: region.width.max(region.height),
                tile_max: region.width.max(region.height),
                tile_overlap: 0,
                ..defaults
            },
        ),
    ];

    println!(
        "region {},{} {}x{}\n",
        region.x, region.y, region.width, region.height
    );
    println!(
        "{:<38} {:>6} {:>9} {:>9} {:>7} {:>10} {:>8}",
        "grid", "tiles", "markers", "regions", "dup", "objects", "stage s"
    );

    let mut reference: Option<Outcome> = None;
    for (name, params) in grids {
        let outcome = match run(&slide, region, params) {
            Ok(outcome) => outcome,
            Err(error) => {
                eprintln!("{name}: {error}");
                std::process::exit(1);
            }
        };
        println!(
            "{name:<38} {:>6} {:>9} {:>9} {:>7} {:>10} {:>8.3}",
            outcome.tiles,
            outcome.markers,
            outcome.regions,
            outcome.duplicates,
            outcome.objects,
            outcome.stages.total(),
        );
        reference = Some(outcome);
    }

    if let Some(reference) = reference {
        println!(
            "\nAgainst the seamless run's {} objects, QuPath detects 25,757 cells for\n\
             this region on its own grid — which is the middle row here, not this\n\
             one. A halo cannot make tiling exact and is not meant to: both\n\
             reconstructions in this pipeline propagate without bound, so the\n\
             target is matching QuPath's grid rather than choosing a better one.",
            reference.objects
        );
    }
}
