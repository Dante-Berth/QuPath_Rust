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
use std::path::PathBuf;

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

    // The project browser, in the three states a row can be in: a slide that is
    // there, one that is not, and an entry this cannot open at all. All three
    // are reachable from an ordinary project and none of them is reachable
    // without a project on disk, which is why this writes one.
    let mut app = App::new(None);
    app.show_metadata = true;
    app.show_overview = true;
    app.open_project(sample_project());
    render("chrome-project.png", &mut app);

    // And with the search narrowing it, which is the state a fifty-slide study
    // is worked in and the one an empty search box says nothing about: the
    // count line changes, and the rows that are left keep their own pictures.
    app.filter = String::from("jp2k");
    render("chrome-project-search.png", &mut app);
    app.filter.clear();

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
        app.open(PathBuf::from(path));
        render("chrome-slide.png", &mut app);

        app.appearance.channel = qupath_app::state::Channel::Haematoxylin;
        render("chrome-slide-stain.png", &mut app);
    }
}

/// Writes a project to a temporary directory and returns its `.qpproj`.
///
/// Written with `project::files` rather than by hand, so the picture is of a
/// project this application would actually produce — the same reason
/// `parity/samples/` is generated rather than committed as text.
///
/// The three rows are chosen for the three states:
///
/// - a slide that **is** there. The file is created empty; nothing here opens
///   it, and the browser's mark is `Path::is_file`;
/// - a slide that **is not**. A study directory copied without its images is
///   the ordinary way this happens, and it is the state a browser exists to
///   make visible before fifty clicks do;
/// - an entry this **cannot open**, which needs one of the sixteen builder
///   types that wrap another builder. There is no way to write one with
///   `project.rs`, because writing one is exactly what it refuses to do, so the
///   discriminator is edited into the file afterwards. That is a picture of a
///   file QuPath writes and this reads, which is what the row is for.
fn sample_project() -> std::path::PathBuf {
    use qupath_viewer::project::{self, Entry, Timestamps};

    let directory = std::env::temp_dir().join("qupath-rust-chrome-project");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a temporary project directory");

    // A real slide if the repository has one, because the row's picture is read
    // from the slide itself and a stand-in of zero bytes is a row with an empty
    // frame — which is the *third* row's state, not this one's. Falls back to
    // the stand-in so the example still runs on a machine with no data.
    let real = PathBuf::from("data/CMU-1-Small-Region.svs");
    let here = if real.is_file() {
        std::path::absolute(&real).unwrap_or(real)
    } else {
        let here = directory.join("CMU-1-Small-Region.svs");
        std::fs::write(&here, b"").expect("a stand-in slide");
        here
    };
    let gone = directory.join("CMU-1, JP2K.svs");
    let rotated = directory.join("CMU-1 rotated.svs");

    let entries = [
        Entry {
            path: &here,
            name: "CMU-1 small region",
            width: 2220,
            height: 2967,
        },
        Entry {
            path: &gone,
            name: "CMU-1, JP2K",
            width: 46_000,
            height: 32_914,
        },
        Entry {
            path: &rotated,
            name: "CMU-1, rotated 90°",
            width: 32_914,
            height: 46_000,
        },
    ];

    let project = directory.join("project.qpproj");
    for file in project::files(&project, &entries, Timestamps::now()) {
        let path = directory.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("an entry directory");
        }
        // The last entry's builder is rewritten to one QuPath registers and
        // `project.rs` will not write. `rsplit_once` so it lands on the last
        // occurrence and the two above it are untouched.
        let contents = if file.path == "project.qpproj" {
            match file.contents.rsplit_once("\"builderType\": \"uri\"") {
                Some((before, after)) => format!("{before}\"builderType\": \"rotated\"{after}"),
                None => file.contents.clone(),
            }
        } else {
            file.contents.clone()
        };
        std::fs::write(&path, contents).expect("write a project file");
    }
    project
}

/// Lays the interface out and rasterises it.
fn render(path: &str, app: &mut App) {
    // The browser's row pictures are made on a thread and arrive over a
    // channel, so an offscreen render has to wait for them the way the window
    // does — by asking again. The application asks once per frame and fills the
    // list in as they land; a still picture has one frame, so it waits.
    while app
        .project
        .as_ref()
        .is_some_and(|project| project.pictures.working())
    {
        app.take_pictures();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    app.take_pictures();

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

    // The browser's rows, under the same ids `App::take_pictures` uploads them
    // to. Same reason as the overview above: these are the application's own
    // textures rather than ones `egui` manages, so no delta carries them.
    if let Some(project) = &app.project {
        for index in 0..project.stored.images.len() {
            let Some(picture) = project.pictures.ready(index) else {
                continue;
            };
            let (width, height) = picture.overview.size();
            textures.push((
                egui::TextureId::User(chrome::PROJECT_TEXTURE + index as u64),
                Texture {
                    width,
                    height,
                    pixels: picture
                        .pixels
                        .chunks_exact(4)
                        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
                        .collect(),
                },
            ));
        }
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
