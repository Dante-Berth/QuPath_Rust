//! Renders the application's own panels to PNGs, so they can be looked at.
//!
//! `docs/VIEWER.md` §6 says appearance is checked by looking, and §8.8 recorded
//! that the panels were the part of this project nobody had looked at: the
//! transforms had offscreen renders, the application launched, but the panes'
//! own layout and colours had only ever been reasoned about.
//!
//! No window and no GPU. `egui` lays out into meshes, `Framebuffer::draw_mesh`
//! rasterises them — the same call the application makes on a machine with no
//! adapter (§3.4b), so what comes out here is what that machine draws.
//!
//! ```text
//! cargo run -p qupath-app --example chrome --release
//! ```
//!
//! Scratch tooling, not part of the application.
#![allow(missing_docs, reason = "example binary")]

use std::fs::File;
use std::io::BufWriter;

use qupath_app::chrome;
use qupath_app::state::App;
use qupath_viewer::compose::{self, Framebuffer, MeshVertex, Texture};

const WIDTH: u32 = 1100;
const HEIGHT: u32 = 760;

fn main() {
    // Every panel open at once, which is both the densest layout and the one
    // §8.1c found a bug in. The slide is left unopened deliberately: the empty
    // states are what a first-time user meets, and they are the easiest thing
    // in an interface to never look at.
    let mut app = App::new(None);
    app.show_metadata = true;
    app.show_appearance = true;
    app.show_overview = true;
    render("chrome-panels.png", &mut app);

    app.show_analysis = true;
    app.show_help = true;
    render("chrome-everything.png", &mut app);

    // And with a slide, which is the state the controls exist for: the empty
    // panels above are what a first-time user meets, and these are what
    // everyone else does. The path is optional so the example still runs on a
    // machine with no slides — the empty states are the half worth having by
    // default.
    if let Some(path) = std::env::args().nth(1) {
        let mut app = App::new(None);
        app.show_metadata = true;
        app.show_appearance = true;
        app.show_overview = true;
        app.open(std::path::PathBuf::from(path));
        render("chrome-slide.png", &mut app);

        app.appearance.channel = qupath_app::state::Channel::Haematoxylin;
        render("chrome-slide-stain.png", &mut app);
    }
}

/// Lays the interface out and rasterises it.
fn render(path: &str, app: &mut App) {
    let context = egui::Context::default();
    chrome::style(&context);

    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            #[expect(clippy::cast_precision_loss, reason = "a window size")]
            egui::vec2(WIDTH as f32, HEIGHT as f32),
        )),
        ..Default::default()
    };

    // Twice: the first pass builds the font atlas and lets `egui` settle the
    // panel widths it derives from content, and a screenshot of the first frame
    // of any `egui` application is a picture of it still deciding.
    // Both frames' texture deltas are kept. The font atlas is created on the
    // first frame and only *changes* on later ones, so taking the second
    // frame's deltas alone leaves nothing to draw with — which renders as an
    // empty window of flat background, and is what this example did until it
    // was looked at.
    let first = context.run(input.clone(), |context| {
        chrome::draw(context, app);
    });
    let output = context.run(input, |context| {
        chrome::draw(context, app);
    });
    let deltas: Vec<_> = first
        .textures_delta
        .set
        .into_iter()
        .chain(output.textures_delta.set)
        .collect();

    let mut frame = Framebuffer::new(WIDTH, HEIGHT);
    frame.clear(compose::CLEAR);

    let mut textures: Vec<(egui::TextureId, Texture)> = Vec::new();
    for (id, delta) in &deltas {
        // Whole-image sets only. `egui` sends partial updates when the font
        // atlas grows, and this renders one frame from scratch, so there is
        // nothing for a patch to be applied to.
        if delta.pos.is_some() {
            continue;
        }
        let texture = to_texture(&delta.image);
        match textures.iter().position(|(existing, _)| existing == id) {
            Some(at) => textures[at].1 = texture,
            None => textures.push((*id, texture)),
        }
    }

    // The overview thumbnail is a texture the *application* owns rather than
    // one `egui` manages, so it arrives through `set_overview` in `render`
    // rather than in a texture delta. This example does not call `render`, so
    // it registers the picture itself under the same id.
    if let Some(thumbnail) = app.open.as_ref().and_then(|open| open.overview()) {
        let (width, height) = thumbnail.overview.size();
        textures.push((
            egui::TextureId::User(chrome::OVERVIEW_TEXTURE),
            Texture {
                width,
                height,
                pixels: thumbnail
                    .pixels
                    .chunks_exact(4)
                    .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
                    .collect(),
            },
        ));
    }

    let mut vertices: Vec<MeshVertex> = Vec::new();
    for primitive in context.tessellate(output.shapes, output.pixels_per_point) {
        let egui::epaint::Primitive::Mesh(mesh) = &primitive.primitive else {
            continue;
        };
        let Some(at) = textures.iter().position(|(id, _)| *id == mesh.texture_id) else {
            continue;
        };
        vertices.clear();
        vertices.extend(mesh.vertices.iter().map(|vertex| MeshVertex {
            position: [vertex.pos.x, vertex.pos.y],
            uv: [vertex.uv.x, vertex.uv.y],
            colour: vertex.color.to_array(),
        }));
        let clip = primitive.clip_rect;
        frame.draw_mesh(
            (clip.min.x, clip.min.y, clip.max.x, clip.max.y),
            &vertices,
            &mesh.indices,
            &textures[at].1,
        );
    }

    write_frame(path, &frame);
    println!("  {path}");
}

/// One `egui` image as the framebuffer's texture.
fn to_texture(image: &egui::ImageData) -> Texture {
    match image {
        egui::ImageData::Color(colour) => Texture {
            #[expect(clippy::cast_possible_truncation, reason = "an atlas dimension")]
            width: colour.width() as u32,
            #[expect(clippy::cast_possible_truncation, reason = "an atlas dimension")]
            height: colour.height() as u32,
            pixels: colour.pixels.iter().map(|pixel| pixel.to_array()).collect(),
        },
    }
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
