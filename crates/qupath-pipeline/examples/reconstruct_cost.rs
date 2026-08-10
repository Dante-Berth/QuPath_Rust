//! Where the background stage's time goes, as a function of tile size.
//!
//! ROADMAP §4.7 recorded the stage as "59% erosion / 40% reconstruction" and
//! pointed the next optimisation pass at the erosion. Both halves of that were
//! measured on 256 px tiles. This runs the same two operations on the tile sizes
//! the pipeline actually uses and reports the raster-pass count, because that is
//! the term nobody had looked at: a geodesic dilation propagates across the
//! whole tile, and one raster pass carries it only as far as a monotone path
//! reaches, so the number of passes grows with the tile's side while the erosion
//! stays linear in its area.
#![allow(missing_docs, reason = "example binary")]

use std::time::Instant;

use qupath_filters::{RankFilter, RankOp, Reconstructor};
use qupath_pipeline::{Region, Stains};
use qupath_slide::Slide;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let stains = Stains::h_and_e();
    let radius = 8.0 / 0.4990;

    println!(
        "{:>6} {:>10} {:>9} {:>9} {:>8} {:>10} {:>9}",
        "side", "Mpx", "erode s", "regrow s", "passes", "queued", "s/Mpx"
    );

    for side in [256u32, 512, 1024, 2048, 2120] {
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
        stains.haematoxylin_into(&rgb, &mut density);

        let width = side as usize;
        let mut background = vec![0.0f32; density.len()];
        let mut filter = RankFilter::new();
        let mut reconstructor = Reconstructor::new();

        // Warm the plan and the buffers, so neither is timed.
        filter.apply_into(&density, &mut background, width, radius, RankOp::Min);

        let mark = Instant::now();
        filter.apply_into(&density, &mut background, width, radius, RankOp::Min);
        let erode = mark.elapsed().as_secs_f64();

        let mark = Instant::now();
        reconstructor.reconstruct(&mut background, &density, width);
        let regrow = mark.elapsed().as_secs_f64();

        let megapixels = density.len() as f64 / 1e6;
        println!(
            "{side:>6} {megapixels:>10.2} {erode:>9.4} {regrow:>9.4} {:>8} {:>10} {:>9.4}",
            reconstructor.last_passes(),
            reconstructor.last_queued(),
            regrow / megapixels,
        );
    }

    println!(
        "\nBoth columns are single-threaded here except the erosion, which is\n\
         rayon-parallel over rows internally. `s/Mpx` is the reconstruction's\n\
         cost per megapixel: flat would mean it scales with area, rising means\n\
         it scales with the tile's side as well."
    );
}
