//! Renders the application's own panels to PNGs, so they can be looked at.
//!
//! `docs/dev/VIEWER.md` §6 says appearance is checked by looking, and §8.8 recorded
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

    // And in an order other than the file's. The picture is of the control as
    // much as of the rows: a `ComboBox` sizes itself from its widest choice,
    // which inside a content-sized panel is §8.1c's trap, and a pane clipped
    // by it clips three widgets away from the change.
    app.order = chrome::Order::Name;
    render("chrome-project-sorted.png", &mut app);

    // And in an order that is one of the *project's* keys rather than one of
    // the four. The rows this key cannot speak for come first, which is the
    // half of the rule a study where every entry has the key never shows.
    app.order = chrome::Order::Metadata(String::from("block"));
    render("chrome-project-sorted-metadata.png", &mut app);
    app.order = chrome::Order::default();

    // And with the names masked, which is the state a blinded reader works in:
    // the rows wear their randomized names, they are shuffled, and the sort
    // combo has lost the three choices that would rank them by what is hidden.
    // The pictures are not masked and cannot be — a thumbnail of a slide is the
    // slide — which is worth being able to see rather than only read.
    //
    // Set directly rather than through `Command::MaskNames`, which the rest of
    // the application reaches through `App::run` and a `Window` this example
    // does not have — the same way `order` and `filter` are set above. The
    // shuffle is written out rather than drawn, because a picture whose row
    // order changes on every run is one nobody can read the diff of.
    app.mask_names = true;
    app.masked_order = vec![3, 1, 4, 2];
    render("chrome-project-masked.png", &mut app);
    app.mask_names = false;
    app.masked_order.clear();

    // And with a search narrowing on metadata rather than on a name, which is
    // the state the `|` is for: neither token is in a name, and the rows that
    // are left are the ones carrying both. A picture of it is the only thing
    // that says the count line and the rows agree about what a `key=value`
    // token left — the search tests assert on the predicate, not on the pane.
    app.filter = String::from("stain=h&e|block");
    render("chrome-project-search-metadata.png", &mut app);
    app.filter.clear();

    // And the metadata editor, which is the first thing in this application
    // that writes a user's typing into somebody else's file and so the one
    // most worth looking at before anyone does. Two pictures, because the
    // second is a state no ordinary run reaches on purpose: the refusal.
    //
    // Set directly for `mask_names`'s reason — the editor is opened by a
    // right-click, and a synthesised context menu would be a picture of
    // `egui`'s menu rather than of this.
    app.editing = Some(qupath_app::state::MetadataEditor {
        id: 1,
        name: String::from("CMU-1-Small-Region.svs"),
        rows: vec![
            (String::from("stain"), String::from("H&E")),
            (String::from("block"), String::from("A3")),
            (String::new(), String::new()),
        ],
        problem: None,
    });
    render("chrome-project-metadata.png", &mut app);

    // The refusal. This is the state the whole design of the write path is
    // about — the project changed underneath — and it is the one a picture is
    // worth most for, because it is reached only by two programs writing one
    // study and nobody sees it while building the feature.
    if let Some(editor) = &mut app.editing {
        editor.problem = Some(String::from(
            "The project changed on disk since it was opened, so this was not \
             written. Reopen the project and try again.",
        ));
    }
    render("chrome-project-metadata-refused.png", &mut app);
    app.editing = None;

    // And the list the combo drops down, which is the one part of the control
    // a still picture of it cannot show. The position is the closed box, read
    // off `chrome-project.png`.
    render_clicking(
        "chrome-project-sort-open.png",
        &mut app,
        egui::pos2(380.0, 204.0),
    );

    // And a row with the pointer over it, which §8.8 recorded as the part of
    // the browser that had been reasoned about and drawn and never seen. The
    // position is the first row, read off `chrome-project.png` the way the
    // combo's was.
    //
    // Two pictures, because the fill is attached to two different senses and
    // saying so is not the same as seeing it. The first row is openable and
    // takes `Sense::click()`; the last is an entry built by a transform this
    // cannot open, takes `Sense::hover()`, and is filled anyway — a row that
    // highlights under the pointer and then does nothing when pressed is the
    // one place in this pane where the fill could be a lie, and the picture is
    // how anybody would find out.
    render_hovering(
        "chrome-project-hover.png",
        &mut app,
        egui::pos2(360.0, 278.0),
    );
    render_hovering(
        "chrome-project-hover-unopenable.png",
        &mut app,
        egui::pos2(360.0, 440.0),
    );

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
    // A fourth row, and it is here for the *sort* rather than for a row state:
    // the other three are already in name order, so a picture of the browser
    // sorted by name was identical to one of it unsorted and said nothing
    // about either. This one is written third and sorts first, which is what
    // makes the two pictures differ.
    let block = directory.join("CMU-1 block 10.svs");
    std::fs::write(&block, b"").expect("a stand-in slide");

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
        // Before the `rotated` row and not after it. The rewrite below finds
        // the *last* `"builderType": "uri"` in the file, so the entry it turns
        // into a transform is whichever one is written last — adding this
        // fourth entry after `rotated` silently moved the greyed row onto it,
        // and both rows went on looking like rows. Nothing in the browser
        // could have said so; the picture did.
        Entry {
            path: &block,
            name: "CMU-1 block 10",
            width: 2220,
            height: 2967,
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
            let contents = match file.contents.rsplit_once("\"builderType\": \"uri\"") {
                Some((before, after)) => format!("{before}\"builderType\": \"rotated\"{after}"),
                None => file.contents.clone(),
            };
            with_metadata(&contents)
        } else {
            file.contents.clone()
        };
        std::fs::write(&path, contents).expect("write a project file");
    }
    project
}

/// Gives each entry a `metadata` map, which `project.rs` does not write.
///
/// Same shape as the `rotated` rewrite above and for the same reason: the state
/// being photographed is one only a project **QuPath** wrote can be in, and the
/// module that writes projects here deliberately writes no metadata against an
/// entry it has just created — there is nothing yet to record.
///
/// Two keys and not one, because the sort combo's popup is what these are for
/// and a menu with a single extra item does not show that the keys are sorted
/// and deduplicated. `stain` is on every entry and `block` on two, so the
/// popup also shows a key that not every row can answer — the `(Unassigned)`
/// case, which sorts those rows first.
///
/// Inserted after `"entryID"`, which every entry has exactly one of, rather
/// than at the end of an entry: the closing brace of an entry object is not
/// distinguishable from any other closing brace by text alone, and this is a
/// string edit rather than a parse.
fn with_metadata(project: &str) -> String {
    // In the order `project::files` writes the entries, which is the order the
    // ids are issued in.
    let maps = [
        r#"{"stain": "H&E", "block": "2"}"#,
        r#"{"stain": "CD8"}"#,
        r#"{"stain": "H&E", "block": "10"}"#,
        r#"{"stain": "CD8"}"#,
    ];
    // The randomized names go with them, and for a second reason: `project.rs`
    // writes `entry-<id>` there, which spells out the entry order, so the
    // browser refuses to mask a project it wrote. Replacing them with names
    // that are not derived from an id is what puts this project in the state a
    // project QuPath wrote is in — the only state the checkbox is live in.
    let random = [
        "8f14e45f-ea8b-4a92",
        "c9f0f895-fb98-4e7f",
        "45c48cce-2e2d-4fb1",
        "d3d94468-02a4-4d0d",
    ];
    let mut out = String::with_capacity(project.len() + 512);
    let mut rest = project;
    let mut written = 0;
    while let Some((before, after)) = rest.split_once("\"entryID\": ") {
        let Some(map) = maps.get(written) else { break };
        out.push_str(before);
        out.push_str(&format!("\"metadata\": {map},\n      \"entryID\": "));
        rest = after;
        written += 1;
    }
    out.push_str(rest);

    // The randomized names are *replaced* rather than inserted, because
    // `project.rs` already writes one per entry.
    let mut named = out;
    for (index, name) in random.iter().enumerate() {
        named = named.replace(
            &format!("\"randomizedName\": \"entry-{}\"", index + 1),
            &format!("\"randomizedName\": \"{name}\""),
        );
    }
    named
}

/// Writes `patch` into `texture` with its top-left corner at `(x, y)`.
///
/// `egui`'s partial texture update, which is how the font atlas grows: the
/// glyphs asked for on a later frame are appended and only their rectangle is
/// sent. Rows are copied whole, and a patch that would run past an edge is
/// clipped rather than wrapping — a wrapped row would put a glyph's right-hand
/// column at the left edge of the next line of the atlas, which draws as a
/// stray mark in a completely different letter.
fn paste(texture: &mut Texture, patch: &Texture, x: usize, y: usize) {
    let (stride, rows) = (texture.width as usize, texture.height as usize);
    let (patch_stride, patch_rows) = (patch.width as usize, patch.height as usize);
    let width = patch_stride.min(stride.saturating_sub(x));
    for row in 0..patch_rows.min(rows.saturating_sub(y)) {
        let from = row * patch_stride;
        let to = (y + row) * stride + x;
        texture.pixels[to..to + width].copy_from_slice(&patch.pixels[from..from + width]);
    }
}

/// Lays the interface out and rasterises it.
fn render(path: &str, app: &mut App) {
    render_pointing(path, app, None);
}

/// What the pointer does before the picture is taken.
///
/// Two gestures rather than a click and a boolean, because they are not a
/// gesture and a modifier of it: a hover is a `PointerMoved` and nothing else,
/// and a click is that move plus a press and a release. Writing the second as
/// the first with a flag set would let a caller ask for a press with the
/// pointer somewhere other than where it pressed, which is not a thing a hand
/// can do.
#[derive(Clone, Copy)]
enum Pointer {
    /// The pointer is moved here and left there.
    Hovering(egui::Pos2),
    /// The pointer is moved here, pressed and released.
    Clicking(egui::Pos2),
}

impl Pointer {
    /// Where the pointer ends up, which is what the settling frames hold it at.
    fn at(self) -> egui::Pos2 {
        match self {
            Self::Hovering(at) | Self::Clicking(at) => at,
        }
    }
}

/// The same, with the pointer moved to `at` before the frame is drawn.
///
/// For the fill under a hovered row, which `docs/dev/VIEWER.md` §8.8 recorded as
/// reasoned about and drawn and never seen. A hover is not a weaker click: the
/// row `egui` fills is the one the *pointer* is over on the frame being drawn,
/// so a click that has already been released still leaves the pointer there and
/// a picture of a click is a picture of a hover as well — which is exactly why
/// this could not be read off the sort combo's picture, where the pointer sits
/// on the combo rather than on a row.
fn render_hovering(path: &str, app: &mut App, at: egui::Pos2) {
    render_pointing(path, app, Some(Pointer::Hovering(at)));
}

/// The same, with a click delivered at `click` before the frame is drawn.
///
/// For the one part of a control an offscreen render cannot otherwise reach: a
/// `ComboBox`'s list only exists once the box has been pressed, so a picture of
/// the closed box says nothing about the popup — which is laid out against the
/// *screen* rather than against the panel, so the width the closed box was
/// given does not obviously bound it.
///
/// A press and a release in one frame, which is what `egui` reads as a click.
/// The position is in points and is read off the picture; a click that lands on
/// nothing renders the same frame as no click, so a wrong one shows up as a
/// picture with no popup in it rather than as a silent pass.
fn render_clicking(path: &str, app: &mut App, click: egui::Pos2) {
    render_pointing(path, app, Some(Pointer::Clicking(click)));
}

/// Lays the interface out with `pointer`'s gesture delivered, and rasterises it.
fn render_pointing(path: &str, app: &mut App, pointer: Option<Pointer>) {
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
    // The click goes on the *second* frame, because a press has to land on a
    // widget that already has a rectangle — the first frame is `egui` still
    // deciding where things are.
    let mut input = input;
    if let Some(pointer) = pointer {
        input.events.push(egui::Event::PointerMoved(pointer.at()));
        if let Pointer::Clicking(at) = pointer {
            input.events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            input.events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }
    let output = context.run(input.clone(), |context| {
        chrome::draw(context, app);
    });
    // And a third frame when something was clicked, because the popup a click
    // opens is drawn on the frame *after* the one that opened it. Without this
    // the picture is of the box being pressed and of no list at all, which is
    // the state the click was meant to leave behind.
    // The pressed frame's own deltas are kept whether or not a third frame
    // follows it, which is what `pressed` holds.
    // The settling path is taken for a click *or* for a modal, and the second
    // is not a special case of the first. `egui` fades a modal in exactly as it
    // fades a popup in, so the frame after one opens is a picture of it at 44%
    // opacity with the rows behind showing through — which reads as a
    // transparent dialog and is an animation a fifth of the way through. Found
    // in the first picture of the metadata editor, having already been found
    // once for the sort combo's dropdown and written down right here.
    // A hover settles for the same reason a click does, and it is the same
    // animation: `egui` interpolates a widget's background towards its hovered
    // colour rather than switching to it, so the frame the pointer arrives on
    // has the fill part of the way in. A picture of that is a picture of a
    // paler fill than the one a hand sees, and nothing about it looks wrong.
    let settling = pointer
        .map(Pointer::at)
        .or_else(|| app.editing.as_ref().map(|_| egui::pos2(-1.0, -1.0)));
    let (output, pressed) = match settling {
        Some(at) => {
            input.events.clear();
            // Only for a real gesture. The modal's settle uses a position off
            // the window, and moving the pointer there would take the hover
            // off whatever the picture is of.
            if pointer.is_some() {
                input.events.push(egui::Event::PointerMoved(at));
            }
            // Several frames, and time passing between them, because `egui`
            // **fades a popup in**: the frame after the click has it at 44%
            // opacity, which composites the rows behind it *through* it and
            // reads as a transparent menu. It is not one — it is a picture of
            // an animation a fifth of the way through. `predicted_dt` is what
            // the animation clock advances on, so the frames have to claim
            // time as well as be drawn.
            input.predicted_dt = 1.0 / 20.0;
            let mut opened = None;
            let mut deltas = Vec::new();
            for _ in 0..6 {
                let frame = context.run(input.clone(), |context| {
                    chrome::draw(context, app);
                });
                if let Some(previous) = opened.replace(frame) {
                    deltas.push(previous);
                }
                input.events.clear();
            }
            (opened.expect("a settled frame"), Some((output, deltas)))
        }
        None => (output, None),
    };
    // **Every** frame's texture deltas, and the third one is not optional.
    //
    // The font atlas is built as glyphs are first asked for, so a widget that
    // only exists on the third frame contributes glyphs that only that frame's
    // delta carries. Dropping it drew the popup with `P` and `E` missing from
    // two of its four choices — the two words no other widget on screen starts
    // with — and every other letter present, which reads as a clipped label
    // rather than as a missing texture. The module comment above records the
    // same failure for the *second* frame; this is its third instance and the
    // reason the chain is now over all of them rather than over two.
    let deltas: Vec<_> = first
        .textures_delta
        .set
        .into_iter()
        .chain(pressed.into_iter().flat_map(|(frame, rest)| {
            frame
                .textures_delta
                .set
                .into_iter()
                .chain(rest.into_iter().flat_map(|frame| frame.textures_delta.set))
        }))
        .chain(output.textures_delta.set)
        .collect();

    let mut frame = Framebuffer::new(WIDTH, HEIGHT);
    frame.clear(compose::CLEAR);

    let mut textures: Vec<(egui::TextureId, Texture)> = Vec::new();
    for (id, delta) in &deltas {
        let patch = to_texture(&delta.image);
        let at = textures.iter().position(|(existing, _)| existing == id);
        match (delta.pos, at) {
            // A **partial** update, which `egui` sends when the font atlas
            // grows: new glyphs are appended and only the new rectangle is
            // sent. This used to be skipped, on the reasoning that a render
            // from scratch has nothing for a patch to be applied to — and that
            // is wrong, because the whole-image set from the first frame is
            // exactly what it applies to.
            //
            // It cost the sort combo's popup two letters. `P` and `E` are the
            // only capitals on that pane that no *earlier* frame had asked for,
            // so `Project order` and `Entry ID` drew as `roject order` and
            // `ntry ID` with every other glyph present — which reads as a
            // clipped label, and sent a diagnosis after the widget's width for
            // an hour. One 35x11 patch, dropped in silence.
            (Some([x, y]), Some(at)) => paste(&mut textures[at].1, &patch, x, y),
            // A patch with nothing under it is the one case the old comment
            // was right about, and it is not reachable from here: `egui` sets
            // the whole atlas before it ever patches one. Skipped rather than
            // panicking, because a picture missing some glyphs is a better
            // failure for scratch tooling than no picture at all.
            (Some(_), None) => continue,
            (None, Some(at)) => textures[at].1 = patch,
            (None, None) => textures.push((*id, patch)),
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
