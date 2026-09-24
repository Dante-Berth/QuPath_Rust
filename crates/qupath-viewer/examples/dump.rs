//! Writes what the viewer would show, as a PNG, without opening a window.
//!
//! Two jobs. It is the reference `docs/dev/VIEWER.md` §3.2 says to check the
//! on-screen tile against — same slide, same level, same pixels, through a path
//! that has no GPU in it — so a colour or orientation bug can be localised to
//! the render code rather than the read code. And it is how to look at a slide
//! on a machine with no display.
//!
//! ```text
//! cargo run -p qupath-viewer --example dump -- data/CMU-1.svs
//! ```
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::fs::File;
use std::io::BufWriter;

use qupath_slide::Slide;
use qupath_viewer::{SlidePoint, Viewport};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let out = args.next().unwrap_or_else(|| "viewer-dump.png".to_owned());

    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    // A window-sized view of the whole slide, which is what the viewer opens
    // with. `fit` picks the scale; `level` picks the cheapest level that still
    // covers it.
    let (width, height) = (1024u32, 768u32);
    let view = Viewport::fit(&slide, width, height);
    let level = view.level(&slide);

    println!("{path}");
    println!(
        "  whole slide at {:.4} px/px -> level {} ({}x{}, {:.1}x downsample)",
        view.scale, level.index, level.width, level.height, level.downsample
    );
    println!(
        "  {} tiles to cover the view, against {} at level 0",
        view.visible_tiles(level).len(),
        view.visible_tiles(&slide.levels()[0]).len()
    );

    // Read the whole chosen level. This is a dump, not the render loop — the
    // viewer reads tiles, but for a still image the region reader is simpler
    // and is the tested path.
    let pixels = match slide.read_region_rgb(level.index, 0, 0, level.width, level.height) {
        Ok(pixels) => pixels,
        Err(error) => {
            eprintln!("read failed: {error}");
            std::process::exit(1);
        }
    };

    write_png(&out, &pixels, level.width, level.height);
    println!("  wrote {out} ({}x{})", level.width, level.height);

    // And the detail view: the region the JFR profile used, at full resolution,
    // so there is something to compare against docs/profiles/README.md and
    // against what the window shows at scale 1.0.
    let (x, y, size) = (27776u32, 5952u32, 1024u32);
    let base = &slide.levels()[0];
    if x + size <= base.width && y + size <= base.height {
        match slide.read_region_rgb(0, x, y, size, size) {
            Ok(detail) => {
                let detail_out = out.replace(".png", "-detail.png");
                write_png(&detail_out, &detail, size, size);
                println!("  wrote {detail_out} ({size}x{size} at level 0, from {x},{y})");
            }
            Err(error) => eprintln!("detail read failed: {error}"),
        }
    }

    // What the viewport would ask for when zoomed in, so the level choice is
    // visible as numbers as well as pixels.
    println!("\n  scale   level   size          tiles in view");
    for scale in [1.0, 0.5, 0.25, 0.1, 0.05, 0.01] {
        let zoomed = Viewport {
            centre: SlidePoint::new(f64::from(base.width) / 2.0, f64::from(base.height) / 2.0),
            scale,
            width,
            height,
        };
        let chosen = zoomed.level(&slide);
        println!(
            "  {:>5.2}   {:>5}   {:>13}  {:>4}",
            scale,
            chosen.index,
            format!("{}x{}", chosen.width, chosen.height),
            zoomed.visible_tiles(chosen).len()
        );
    }
}

fn write_png(path: &str, rgb: &[u8], width: u32, height: u32) {
    let file = File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(rgb)
        .expect("png data");
}
