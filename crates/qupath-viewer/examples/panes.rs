//! Renders §8.2's new pieces to PNGs, so they can be looked at.
//!
//! `docs/VIEWER.md` §6 says appearance is checked by looking, and that nobody
//! had looked until the README images. This is the same treatment for the
//! overview thumbnail and the display transforms: no window, no GPU, and the
//! output on disk to open.
//!
//! ```text
//! cargo run -p qupath-viewer --example panes --release -- data/CMU-1.svs
//! ```
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::fs::File;
use std::io::BufWriter;

use qupath_slide::Slide;
use qupath_viewer::compose::{self, Framebuffer};
use qupath_viewer::overview::{self, Overview};
use qupath_viewer::{Display, SlidePoint, Stain, Viewport};

/// H&E haematoxylin, from `qupath_pipeline::stain::Stains::h_and_e()`.
const HAEMATOXYLIN: [f64; 3] = [
    1.838_330_820_296_71,
    0.033_906_046_767_148_236,
    -0.759_830_046_177_923,
];
/// The eosin row of the same matrix.
const EOSIN: [f64; 3] = [
    -1.373_320_834_675_587,
    0.771_884_590_358_511_3,
    1.214_963_204_441_977_8,
];

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
    let base = &slide.levels()[0];
    println!("{path}: {}x{}", base.width, base.height);

    // The overview picture itself, at the size QuPath would use, from the
    // coarsest level — which is what makes a thumbnail cheap.
    let overview = Overview::fit(base.width, base.height).expect("a real slide");
    let (width, height) = overview.size();
    println!(
        "  overview {width}x{height} at max length {}",
        overview::MAX_LENGTH
    );

    let coarsest = slide.levels().last().expect("a level");
    let thumbnail = slide
        .read_region_rgb(coarsest.index, 0, 0, coarsest.width, coarsest.height)
        .expect("read the coarsest level");

    // Three views, so the rectangle can be seen to move and shrink. The first
    // is `fit`, where QuPath draws no rectangle at all because everything is
    // visible.
    let views = [
        ("fit", Viewport::fit(&slide, 1024, 768)),
        (
            "zoomed",
            Viewport {
                centre: SlidePoint::new(
                    f64::from(base.width) * 0.32,
                    f64::from(base.height) * 0.45,
                ),
                scale: 0.05,
                width: 1024,
                height: 768,
            },
        ),
        (
            "detail",
            Viewport {
                centre: SlidePoint::new(27776.0, 5952.0),
                scale: 0.5,
                width: 1024,
                height: 768,
            },
        ),
    ];

    for (name, view) in views {
        // Draw the overview at 4x so the rectangle is actually visible in a
        // PNG — at 150 px a zoomed-in view is a two-pixel mark, which is the
        // right size on screen and useless to look at in a file.
        let zoom = 4u32;
        let mut frame = Framebuffer::new(width * zoom, height * zoom);
        let mut rgba = Vec::with_capacity(thumbnail.len() / 3 * 4);
        for pixel in thumbnail.chunks_exact(3) {
            rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
        #[expect(clippy::cast_precision_loss, reason = "sizes are small")]
        frame.draw_tile(
            (0.0, 0.0, (width * zoom) as f32, (height * zoom) as f32),
            &rgba,
            coarsest.width,
            coarsest.height,
        );

        let covers = overview.covers_everything(view);
        if !covers {
            let rect = overview.visible_rect(view);
            #[expect(clippy::cast_precision_loss, reason = "zoom is small")]
            let zoom = zoom as f32;
            draw_rect(
                &mut frame,
                [
                    rect[0] * zoom,
                    rect[1] * zoom,
                    rect[2] * zoom,
                    rect[3] * zoom,
                ],
                compose::pack(200, 0, 0),
            );
        }
        let out = format!("overview-{name}.png");
        write_frame(&out, &frame);
        println!(
            "  {out}: scale {:.4}, {}",
            view.scale,
            if covers {
                "whole slide visible, no rectangle (QuPath's rule)".to_owned()
            } else {
                let rect = overview.visible_rect(view);
                format!(
                    "rectangle [{:.1},{:.1} {:.1}x{:.1}]",
                    rect[0], rect[1], rect[2], rect[3]
                )
            },
        );
    }

    // And the display transforms, over a region with real tissue in it, so the
    // stain channels can be compared against the colour they came from.
    let (x, y, size) = (27776u32, 5952u32, 1024u32);
    if x + size <= base.width && y + size <= base.height {
        let region = slide
            .read_region_rgb(0, x, y, size, size)
            .expect("read the detail region");
        let mut rgba = Vec::with_capacity(region.len() / 3 * 4);
        for pixel in region.chunks_exact(3) {
            rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }

        let displays = [
            ("original", Display::new()),
            (
                "brightened",
                Display::range(30.0, 220.0).expect("a real range"),
            ),
            (
                "haematoxylin",
                Display::new().with_stain(Stain {
                    coefficients: HAEMATOXYLIN,
                    background: [255.0, 255.0, 255.0],
                    full: 1.5,
                }),
            ),
            (
                "eosin",
                Display::new().with_stain(Stain {
                    coefficients: EOSIN,
                    background: [255.0, 255.0, 255.0],
                    full: 1.5,
                }),
            ),
            // The two ends of the panel's "Full black" slider, which is the
            // stain channels' contrast control and the one thing the panel adds
            // that §8.5's transform did not already have a picture of. At 0.3
            // everything above a faint density saturates to black; at 3.0 the
            // whole slide is pale. Both are what the control is *for*, and
            // neither is a bug — but a slider whose two ends looked the same
            // would be, and that is what this is here to show.
            (
                "haematoxylin-full-0.3",
                Display::new().with_stain(Stain {
                    coefficients: HAEMATOXYLIN,
                    background: [255.0, 255.0, 255.0],
                    full: 0.3,
                }),
            ),
            (
                "haematoxylin-full-3.0",
                Display::new().with_stain(Stain {
                    coefficients: HAEMATOXYLIN,
                    background: [255.0, 255.0, 255.0],
                    full: 3.0,
                }),
            ),
        ];
        for (name, display) in displays {
            let mut frame = Framebuffer::new(size, size);
            #[expect(clippy::cast_precision_loss, reason = "size is 1024")]
            frame.draw_tile_with(
                (0.0, 0.0, size as f32, size as f32),
                &rgba,
                size,
                size,
                display,
            );
            let out = format!("display-{name}.png");
            write_frame(&out, &frame);
            println!("  {out}");
        }
    }
}

/// A one-pixel rectangle outline, which is what `ImageOverview` draws.
fn draw_rect(frame: &mut Framebuffer, rect: [f32; 4], colour: u32) {
    use qupath_viewer::ScreenPoint;
    let [x, y, width, height] = rect;
    let corners = [
        ScreenPoint::new(x, y),
        ScreenPoint::new(x + width, y),
        ScreenPoint::new(x + width, y + height),
        ScreenPoint::new(x, y + height),
    ];
    let mut segments = Vec::new();
    for at in 0..4 {
        segments.push(corners[at]);
        segments.push(corners[(at + 1) % 4]);
    }
    frame.draw_segments(&segments, colour);
}

fn write_frame(path: &str, frame: &Framebuffer) {
    let mut rgba = Vec::new();
    frame.to_rgba(&mut rgba);
    let file = File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(BufWriter::new(file), frame.width(), frame.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(&rgba)
        .expect("png data");
}
