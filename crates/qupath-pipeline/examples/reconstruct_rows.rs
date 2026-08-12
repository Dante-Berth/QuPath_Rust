//! How much of each raster pass of the reconstruction does anything.
//!
//! Diagnostic only, in the shape ROADMAP §5 asks for: before writing a change
//! that skips settled rows, measure whether there are any. The stage is 26.2% of
//! pipeline time and fourteen full sweeps of a 2120 px tile; if the changes
//! after the first few passes live in a handful of rows, the sweeps are work
//! that happens and is not needed, and if they are spread over every row there
//! is nothing here.
//!
//! Reports, per pass: pixels moved, rows holding a moved pixel, and the rows a
//! conservative skip rule would still have to visit. That rule is the one a real
//! implementation could use — a row is visited if it moved in the previous pass,
//! if the row it reads moved in the previous pass, or if the row it reads moved
//! in this one.
#![allow(missing_docs, reason = "example binary")]

use qupath_filters::{RankFilter, RankOp, Reconstructor};
use qupath_pipeline::{Params, Region, Stains};
use qupath_slide::Slide;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let side: u32 = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(2120);
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
    let height = density.len() / width;
    let mut marker = vec![0.0f32; density.len()];
    RankFilter::new().apply_into(
        &density,
        &mut marker,
        width,
        params.background_radius,
        RankOp::Min,
    );

    // The pass sequence `Reconstructor::reconstruct` runs: one forward, then
    // alternating pairs while a pass still moves more than a tenth of the image.
    let reconstructor = Reconstructor::new();
    let pixels = density.len();
    // Every row is dirty before the first pass: the marker has just been eroded
    // and nothing has read it yet.
    let mut previous = vec![true; height];
    let mut snapshot = marker.clone();

    println!("{side}x{side}, {} px, one real tile\n", pixels);
    println!(
        "{:>5} {:>9} {:>12} {:>8} {:>8} {:>8}",
        "pass", "dir", "moved px", "% img", "rows", "visited"
    );

    let mut pass = 0u32;
    let mut changes;
    let mut reverse = false;
    loop {
        snapshot.copy_from_slice(&marker);
        changes = reconstructor.raster_pass(&mut marker, &density, width, reverse);
        pass += 1;

        let moved: Vec<bool> = marker
            .chunks(width)
            .zip(snapshot.chunks(width))
            .map(|(now, before)| now != before)
            .collect();

        // The skip rule, evaluated against what actually moved. A row is visited
        // if it moved last pass, if the row it reads moved last pass, or if the
        // row it reads moved in this one — the last clause is a sequential
        // dependency, so it is resolved by walking in the pass's own direction.
        let mut visited = vec![false; height];
        let order: Vec<usize> = if reverse {
            (0..height).rev().collect()
        } else {
            (0..height).collect()
        };
        for &row in &order {
            let upstream = if reverse {
                row.checked_add(1).filter(|next| *next < height)
            } else {
                row.checked_sub(1)
            };
            let reads_moved =
                upstream.is_some_and(|next| previous[next] || (visited[next] && moved[next]));
            visited[row] = previous[row] || reads_moved;
        }

        let rows = moved.iter().filter(|m| **m).count();
        let would_visit = visited.iter().filter(|v| **v).count();
        println!(
            "{pass:>5} {:>9} {changes:>12} {:>7.1}% {rows:>8} {would_visit:>8}",
            if reverse { "backward" } else { "forward" },
            100.0 * changes as f64 / pixels as f64,
        );

        previous = moved;
        // `reconstruct`'s own loop: stop once a forward pass moves a tenth or
        // less, since the queue takes it from there.
        if !reverse && pass > 1 && changes * 10 <= pixels {
            break;
        }
        if pass > 40 {
            break;
        }
        reverse = !reverse;
    }

    println!(
        "\n`rows` is how many of the {height} rows held a pixel that moved.\n\
         `visited` is how many a skip rule would still sweep. The two columns\n\
         are what decides whether skipping settled rows is worth writing: if\n\
         `visited` stays near {height} there is no work to remove."
    );
}
