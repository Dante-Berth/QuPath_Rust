//! Prints a slide's pyramid and samples a region, as a smoke test.
#![allow(missing_docs, reason = "example binary")]

use std::time::Instant;

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

    println!("{path}");
    println!(
        "{:>5}  {:>13}  {:>11}  {:>9}  {:>10}",
        "level", "size", "tiles", "tile", "downsample"
    );
    for level in slide.levels() {
        println!(
            "{:>5}  {:>13}  {:>11}  {:>9}  {:>9.1}x",
            level.index,
            format!("{}x{}", level.width, level.height),
            format!("{}x{}", level.columns, level.rows),
            format!("{}x{}", level.tile_width, level.tile_height),
            level.downsample
        );
    }

    if let Ok(description) = slide.description(0) {
        let first = description.split('|').next().unwrap_or("").trim();
        println!("\nlevel 0: {first}");
    }

    // The region the JFR profile used, so the pixels are comparable with
    // docs/profiles/README.md.
    let (x, y, width, height) = (27776u32, 5952u32, 4000u32, 4000u32);
    let start = Instant::now();
    match slide.read_region_rgb(0, x, y, width, height) {
        Ok(region) => {
            let elapsed = start.elapsed().as_secs_f64();
            let mean = region.iter().map(|&v| f64::from(v)).sum::<f64>() / region.len() as f64;
            let dark = region.iter().filter(|&&v| v < 128).count();
            println!(
                "\nregion {x},{y} {width}x{height} at level 0: {:.3} s ({:.1} MB/s decoded)",
                elapsed,
                region.len() as f64 / 1e6 / elapsed
            );
            println!(
                "  mean channel value {mean:.1}, {:.1}% of samples below 128",
                100.0 * dark as f64 / region.len() as f64
            );
        }
        Err(error) => eprintln!("region read failed: {error}"),
    }
}
