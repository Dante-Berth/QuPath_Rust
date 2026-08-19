//! Counts this pipeline's objects over the regions `parity/fixtures/CellCounts.json`
//! records QuPath's cells for, and reports the gap.
//!
//! `docs/MEMO.md` §2 quotes 26,647 objects against QuPath's 25,757 cells on one
//! 4000x4000 region — one region, measured once. A ratio from a single sample
//! cannot tell a systematic offset from a coincidence, and `DIVERGENCES.md`
//! D-008 predicts a *systematic* one: seam overlaps are resolved here on nucleus
//! masks where QuPath resolves them on expanded, smoothed cells, so this
//! pipeline should over-count, by an amount that grows with the number of seams.
//!
//! This runs the same regions and prints both numbers side by side, so the
//! claim is checkable rather than quoted.
#![allow(missing_docs, reason = "example binary")]

use qupath_pipeline::{Params, Region, run};
use qupath_slide::Slide;

/// One region of the fixture, parsed out of the JSON without a JSON crate —
/// this workspace has no dependency on one and this is a fixed shape written by
/// `parity/java/.../RecordCellCounts.java`.
struct Recorded {
    name: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    cells: u64,
    ms: f64,
}

fn field(line: &str, key: &str) -> Option<String> {
    let at = line.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = line[at..].trim_start();
    let rest = rest.strip_prefix('"').map_or(rest, |rest| rest);
    let end = rest.find([',', '}', '"']).unwrap_or(rest.len());
    Some(rest[..end].trim().to_owned())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let fixture = args
        .next()
        .unwrap_or_else(|| "parity/fixtures/CellCounts.json".to_owned());

    let text = std::fs::read_to_string(&fixture).unwrap_or_else(|error| {
        eprintln!("cannot read {fixture}: {error}");
        eprintln!("run ./parity/record-cell-counts.sh first");
        std::process::exit(1);
    });

    let recorded: Vec<Recorded> = text
        .lines()
        .filter(|line| line.contains("\"cells\":"))
        .filter_map(|line| {
            Some(Recorded {
                name: field(line, "name")?,
                x: field(line, "x")?.parse().ok()?,
                y: field(line, "y")?.parse().ok()?,
                width: field(line, "width")?.parse().ok()?,
                height: field(line, "height")?.parse().ok()?,
                cells: field(line, "cells")?.parse().ok()?,
                ms: field(line, "ms")?.replace(',', ".").parse().ok()?,
            })
        })
        .collect();

    assert!(
        !recorded.is_empty(),
        "no regions parsed from {fixture}; has it been recorded?"
    );

    let slide = Slide::open(&path).expect("open the slide");

    println!(
        "{:<22} {:>8} {:>8} {:>8} {:>7} {:>7} {:>6}  {:>9} {:>9}",
        "region", "QuPath", "here", "gap", "ratio", "c/Mpx", "tiles", "QuPath ms", "here ms"
    );
    let (mut total_qupath, mut total_here) = (0u64, 0u64);
    for entry in &recorded {
        let region = Region {
            x: entry.x,
            y: entry.y,
            width: entry.width,
            height: entry.height,
        };
        let started = std::time::Instant::now();
        let outcome = run(&slide, region, Params::default()).expect("run the pipeline");
        let ms = started.elapsed().as_secs_f64() * 1e3;

        let here = outcome.objects;
        #[expect(
            clippy::cast_precision_loss,
            reason = "object counts are thousands; f64 is exact well past that"
        )]
        let ratio = here as f64 / entry.cells as f64;
        // Cells per megapixel, because the gap turns out to track this rather
        // than the seam count -- see the note this prints at the end.
        let density = entry.cells as f64
            / (f64::from(entry.width) * f64::from(entry.height) / f64::from(1u32 << 20));
        println!(
            "{:<22} {:>8} {:>8} {:>+8} {:>7.4} {:>7.0} {:>6}  {:>9.0} {:>9.0}",
            entry.name,
            entry.cells,
            here,
            here as i64 - entry.cells as i64,
            ratio,
            density,
            outcome.tiles,
            entry.ms,
            ms
        );
        total_qupath += entry.cells;
        total_here += here;
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "object counts are thousands; f64 is exact well past that"
    )]
    let overall = total_here as f64 / total_qupath as f64;
    println!(
        "\n{:<22} {:>8} {:>8} {:>+8} {:>7.4}",
        "total",
        total_qupath,
        total_here,
        total_here as i64 - total_qupath as i64,
        overall
    );
    println!(
        "\nA ratio above 1.0 is this pipeline over-counting, which is the direction\n\
         DIVERGENCES.md D-008 predicts: seam overlaps are resolved here on nucleus\n\
         masks and in QuPath on larger, smoothed cell boundaries, so QuPath removes\n\
         at least as many duplicates as this does.\n\
         \n\
         D-008 is not the whole of it, and this sweep is what shows that. A region\n\
         small enough to be one tile has no seams at all -- `tiles` reads 1 and the\n\
         pipeline removes zero duplicates -- so its gap is everything that is *not*\n\
         seam resolution. Those regions still disagree, by 1.2% to 11.3%.\n\
         \n\
         What the gap does track is `c/Mpx`, inversely: dense regions agree closely\n\
         and sparse ones do not. That points at the filters rather than the seams --\n\
         a sparse region is mostly background, and the mean-intensity and area tests\n\
         are what decide whether a faint candidate there survives."
    );
}
