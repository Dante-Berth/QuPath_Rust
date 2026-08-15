//! Where the radius-16 erosion's time goes, and what the same work costs when
//! written as nothing but vectorisable slice maxima.
//!
//! ROADMAP §4.7 wrote the erosion off — "the erosion is not the problem", 12% of
//! the background stage — on figures taken from the 4000x4000 profiled region,
//! which is four tiles on a twenty-thread pool. On a grid large enough to fill
//! the pool it is **28%** of pipeline stage time and the single largest stage,
//! and it is the largest at one thread too. So it is worth knowing what it is
//! made of before anyone writes off or rewrites it again.
//!
//! The filter decomposes the disc into nested rectangles and, for each output
//! row, folds the source rows it needs into a column accumulator and takes a
//! horizontal maximum per rectangle. This times the whole thing against the
//! floor for its vertical half alone: one `max` per source row per output pixel,
//! over contiguous slices, which is what the fold compiles to and nothing less
//! than the decomposition can cost.
#![allow(missing_docs, reason = "scratch probe")]

use std::hint::black_box;
use std::time::Instant;

use qupath_filters::{RankFilter, RankOp};
use qupath_pipeline::{Params, Region, Stains};
use qupath_slide::Slide;

/// The vertical half's floor: fold `rows` source rows into an accumulator for
/// every output row, which is what a disc of `2 * rows + 1` rows costs before
/// any horizontal maximum is taken.
fn fold_floor(source: &[f32], width: usize, rows: usize) -> Vec<f32> {
    let height = source.len() / width;
    let mut out = vec![f32::NEG_INFINITY; source.len()];
    let mut column = vec![f32::NEG_INFINITY; width];
    for y in 0..height {
        column.fill(f32::NEG_INFINITY);
        for dy in y.saturating_sub(rows)..=(y + rows).min(height - 1) {
            let row = &source[dy * width..(dy + 1) * width];
            for (acc, &value) in column.iter_mut().zip(row) {
                if value > *acc {
                    *acc = value;
                }
            }
        }
        out[y * width..(y + 1) * width].copy_from_slice(&column);
    }
    out
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let side: u32 = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(1920);
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
    let mut density = Vec::new();
    Stains::h_and_e().haematoxylin_into(&rgb, &mut density);

    let width = side as usize;
    let megapixels = density.len() as f64 / 1e6;
    let repetitions = 7;

    println!(
        "{side}x{side} = {megapixels:.2} Mpx, one real tile, single-threaded,\n\
         minimum of {repetitions} interleaved repetitions\n"
    );
    println!("{:>28} {:>10} {:>10}", "", "s", "ns/px");

    let mut out = vec![0.0f32; density.len()];
    let mut filter = RankFilter::new().with_parallel_rows(false);
    // Warm the plan and the buffers.
    filter.apply_into(
        &density,
        &mut out,
        width,
        params.background_radius,
        RankOp::Min,
    );

    let mut whole = f64::INFINITY;
    let mut floor = f64::INFINITY;
    for _ in 0..repetitions {
        let mark = Instant::now();
        filter.apply_into(
            black_box(&density),
            black_box(&mut out),
            width,
            params.background_radius,
            RankOp::Min,
        );
        whole = whole.min(mark.elapsed().as_secs_f64());

        let mark = Instant::now();
        let folded = fold_floor(black_box(&density), width, 16);
        floor = floor.min(mark.elapsed().as_secs_f64());
        black_box(folded);
    }

    let per_pixel = |seconds: f64| seconds / (megapixels * 1e6) * 1e9;
    println!(
        "{:>28} {whole:>10.4} {:>10.2}",
        "erosion, radius 16.03",
        per_pixel(whole)
    );
    println!(
        "{:>28} {floor:>10.4} {:>10.2}",
        "33 folded rows alone",
        per_pixel(floor)
    );
    println!("\n{:>28} {:>21.2}x", "the filter costs", whole / floor);
    println!(
        "\nThe second row is the vertical half of the decomposition and nothing\n\
         else: the same 33 source rows per output row, folded into an\n\
         accumulator by the same comparison, with no horizontal maximum and no\n\
         negated copy. It is a floor rather than an alternative — the filter has\n\
         to do at least this — so the ratio is how much of the stage sits in the\n\
         part a rewrite could address."
    );
}
