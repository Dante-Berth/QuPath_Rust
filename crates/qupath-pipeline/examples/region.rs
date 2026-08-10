//! Runs the tile pipeline over a slide region and reports where time goes.
//!
//! Defaults to the region `docs/profiles/README.md` profiled QuPath on, so the
//! two numbers describe the same pixels.
#![allow(missing_docs, reason = "example binary")]

use qupath_pipeline::{Params, Region, run};
use qupath_slide::Slide;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let parse = |arg: Option<String>, fallback: u32| {
        arg.and_then(|value| value.parse().ok()).unwrap_or(fallback)
    };
    let region = Region {
        x: parse(args.next(), 27776),
        y: parse(args.next(), 5952),
        width: parse(args.next(), 4000),
        height: parse(args.next(), 4000),
    };

    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let params = Params::default();
    let outcome = match run(&slide, region, params) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let megapixels = outcome.pixels as f64 / 1e6;
    println!(
        "{path}  level {}  region {},{} {}x{}",
        params.level, region.x, region.y, region.width, region.height
    );
    println!(
        "{} tiles, {megapixels:.1} Mpx, {} threads",
        outcome.tiles,
        rayon::current_num_threads()
    );
    println!(
        "\nwall clock: {:.3} s  ({:.1} Mpx/s)",
        outcome.elapsed,
        megapixels / outcome.elapsed
    );

    let stages = outcome.stages;
    let total = stages.total();
    println!("\nstage totals, summed across threads:");
    for (name, seconds) in [
        ("decode", stages.decode),
        ("deconvolve", stages.deconvolve),
        ("background", stages.background),
        ("  erode", stages.erode),
        ("  regrow", stages.regrow),
        ("gaussian blur", stages.blur),
        ("laplacian", stages.laplacian),
        ("maxima", stages.maxima),
        ("labelling", stages.label),
        ("watershed", stages.flood),
        ("tracing", stages.trace),
        ("object filter", stages.filter),
    ] {
        println!(
            "  {name:<13} {seconds:7.3} s  {:5.1}%",
            100.0 * seconds / total
        );
    }
    println!("  {:<13} {total:7.3} s", "total");

    println!(
        "\n{} markers seeded, {} pixels labelled ({:.1}% of the region)",
        outcome.markers,
        outcome.labelled,
        100.0 * outcome.labelled as f64 / outcome.pixels as f64
    );
    println!(
        "{} basins traced to candidates, {} passed the intensity filter ({:.1}% dropped)",
        outcome.regions,
        outcome.objects + outcome.duplicates,
        100.0 * (outcome.regions - outcome.objects - outcome.duplicates) as f64
            / outcome.regions as f64
    );
    println!(
        "{} of those were a neighbouring tile's copy of the same nucleus, leaving {}",
        outcome.duplicates, outcome.objects
    );
    if let Some(bytes) = outcome.peak_resident_bytes {
        println!("peak resident memory: {:.0} MB", bytes as f64 / 1e6);
    }
    println!(
        "\nNote: candidates, not cells. Every image stage QuPath runs before its\n\
         watershed is here and pinned against QuPath's own output, and so are the\n\
         tracing and the intensity filter that follow it. What is not here is the\n\
         merge and shape split, the area filter that runs on *its* objects, and\n\
         the tile seams of ROADMAP §2. All of those only ever remove objects, so\n\
         this count is still an upper bound on QuPath's rather than a match."
    );
}
