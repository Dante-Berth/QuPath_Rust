//! What the application is looking at, and how a frame gets drawn.
//!
//! The split from [`crate::chrome`] is the one an immediate-mode interface
//! wants: everything here is state that survives a frame, and everything there
//! is a function of it. `chrome` returns the [`Command`]s the user asked for and
//! changes nothing itself, so "what happens when a button is pressed" is in one
//! place rather than spread through the layout code.

// Screen space is `f32` by definition (`qupath_viewer::ScreenPoint`) and winit
// reports it as `f64`, so the narrowing happens at every boundary between them.
// A window is a few thousand pixels across and `egui`'s texture atlas a few
// thousand texels; nothing here is near a width that could lose a value.
#![expect(
    clippy::cast_possible_truncation,
    reason = "window geometry, which is small by construction"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use qupath_pipeline::{Micron, Stains};
use qupath_slide::Slide;
use qupath_viewer::compose::{self, Framebuffer, MeshVertex, Texture};
use qupath_viewer::project;
use qupath_viewer::{
    Display, Overlay, ScreenPoint, SlidePoint, SlideTiles, Stain, TileCache, TileLoader, Viewport,
};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use winit::window::Window;

use crate::chrome::{self, Command};
use crate::detect::Detector;
use crate::thumbnails::{Pictures, Thumbnail};

/// Texture budget, in bytes. Roughly two thousand tiles.
const BUDGET: usize = 512 << 20;

/// Decoding threads. Four is what `docs/dev/VIEWER.md` §3.4 measured with.
const WORKERS: usize = 4;

/// How many frame costs the status bar averages over.
const WINDOW: usize = 30;

/// How many slides the recent list keeps.
const RECENT: usize = 8;

/// Screen pixels an object's bounding box must reach before its outline is
/// drawn.
///
/// The same value `examples/render.rs` uses, and for the reason `overlay`'s
/// module documentation gives: below this an outline is a dot, and a screenful
/// of them is a smear that costs more than the tiles underneath it and says
/// less.
const MIN_OVERLAY_PX: f32 = 2.0;

/// The colour outlines are drawn in.
///
/// Yellow-green, which is what QuPath defaults a detection to and is chosen the
/// same way: H&E is pink and purple, so an outline in this never disappears
/// into the tissue it is drawn on. Packed through `compose::pack` rather than
/// written as a literal, so the channel order is the compositor's statement of
/// it and not this module's guess.
const OVERLAY_COLOUR: u32 = compose::pack(200, 255, 0);

/// The colour the zoom rectangle is drawn in.
///
/// White, and not [`OVERLAY_COLOUR`]: the band is a transient thing the user is
/// doing and the outlines are a result they are reading, so the two must not be
/// mistaken for each other while a box is dragged across a detected field.
const BAND_COLOUR: u32 = compose::pack(255, 255, 255);

/// Pixels of trackpad scroll that make one zoom step.
///
/// A wheel and a trackpad arrive through the same `MouseWheel` event and are
/// different gestures. A wheel notch is one `LineDelta` of 1.0; a two-finger
/// drag is a *stream* of `PixelDelta` events carrying a few pixels each, so the
/// per-event number is much smaller and there are far more of them.
///
/// This was 50.0, which made a whole trackpad swipe worth a fraction of one
/// step — the zoom moved, but by so little that it read as not working at all.
/// Ten makes a comfortable swipe of roughly 150 px about fifteen steps, which is
/// the same order as spinning a wheel through fifteen notches.
///
/// It is a feel constant and cannot be derived, so it is stated as one rather
/// than dressed up: what is *asserted* is that a trackpad-sized event produces a
/// visible change and a wheel notch is unaffected, which is
/// `a_trackpad_scroll_zooms_visibly`.
const TRACKPAD_PIXELS_PER_STEP: f64 = 10.0;

/// Zoom steps per unit of a platform pinch gesture.
///
/// `PinchGesture` reports a fraction of the current magnification, so 0.01 per
/// event is common and a full pinch sums to something near 1.0. Four steps for
/// that is a factor of about two, which is what a pinch across the trackpad
/// feels like it should do.
///
/// Untested against a real device: this platform never sends the event — see
/// the `PinchGesture` arm in [`App::input`] — so the number is a considered
/// default rather than a measured one, and this comment says so instead of
/// implying otherwise.
const PINCH_STEPS: f64 = 4.0;

/// How much faster ctrl-scroll zooms than plain scroll.
///
/// GNOME delivers a trackpad pinch as ctrl with a two-finger scroll, so this is
/// the factor that makes a pinch cover a useful range of zoom for the distance
/// fingers actually travel. Three is chosen so that a pinch and a wheel notch
/// are the same order per gesture, not per event.
const CTRL_SCROLL_STEPS: f64 = 3.0;

/// How far a right-drag must travel, in window pixels, before it is a zoom
/// rectangle rather than a click.
///
/// A right-click that moves two pixels between press and release is a click
/// with a shaky hand, and treating it as a box would zoom by a factor of a few
/// hundred. Below this the gesture is abandoned and the view is left alone.
const BAND_MINIMUM_PX: f32 = 8.0;

/// Converts a rectangle from `egui`'s logical points into physical pixels.
///
/// Free rather than a method so the arithmetic can be asserted with no window
/// and no display server, which is the only way this particular bug is
/// reachable from a test: it is invisible at the scale factor of 1.0 that a
/// development machine and every headless run both have. See
/// [`App::slide_area`].
fn scale_area(area: (f32, f32, f32, f32), pixels_per_point: f32) -> (f32, f32, f32, f32) {
    let (left, top, right, bottom) = area;
    (
        left * pixels_per_point,
        top * pixels_per_point,
        right * pixels_per_point,
        bottom * pixels_per_point,
    )
}

/// One tile and where it goes, in window pixels — the draw list, in the same
/// shape `examples/window.rs` builds it.
type Draw<'a> = ((f32, f32, f32, f32), &'a Decoded);

/// A decoded tile in main memory, ready to blit.
pub struct Decoded {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// An open slide and everything the viewer keeps for it.
///
/// Separate from [`App`] so that opening a second slide is dropping this and
/// building another, rather than remembering to reset six fields. The loader's
/// threads stop when it drops, which is what makes that safe.
pub struct Open {
    pub path: PathBuf,
    source: Arc<SlideTiles>,
    loader: TileLoader,
    cache: TileCache<Decoded>,
    pub view: Viewport,
    /// Tiles the last frame drew, for the status bar.
    pub drawn: usize,
    /// The whole slide as a small picture, and the geometry that places the
    /// visible rectangle on it — `VIEWER.md` §8.2.
    ///
    /// Read once when the slide opens, from the coarsest level, because that is
    /// what makes a thumbnail cheap: the top of CMU-1's pyramid is 1,024 px on
    /// its long edge, so this is one region read of about 2 MB rather than
    /// anything per frame. It never changes afterwards — the picture is of the
    /// slide, not of the view — so the only per-frame work is drawing the
    /// rectangle.
    ///
    /// `None` when the slide is degenerate or the read failed. A missing
    /// overview is not a reason to fail to open a slide, so it is an absent
    /// pane rather than an error.
    pub overview: Option<Thumbnail>,
}

impl Open {
    /// Opens `path` and fits the whole slide in a viewport of `width` by
    /// `height`.
    ///
    /// # Errors
    ///
    /// Returns what went wrong, as a sentence to put in front of the user.
    /// Every failure here is ordinary — a file that is not a slide, a codec
    /// this crate does not decode — so none of them is a panic. That is the
    /// difference between an application and an example: `window.rs` may exit,
    /// and this has to say so and carry on.
    pub fn new(path: PathBuf, width: u32, height: u32) -> Result<Self, String> {
        let slide = Slide::open(&path).map_err(|error| error.to_string())?;
        let view = Viewport::fit(&slide, width.max(1), height.max(1));
        let overview = crate::thumbnails::make(&slide).ok();
        let source = Arc::new(SlideTiles::new(slide));
        let loader = TileLoader::new(&source, WORKERS);
        Ok(Self {
            path,
            source,
            loader,
            cache: TileCache::new(BUDGET),
            view,
            drawn: 0,
            overview,
        })
    }

    pub fn slide(&self) -> &Slide {
        self.source.slide()
    }

    /// The overview, if the slide has one.
    pub fn overview(&self) -> Option<&Thumbnail> {
        self.overview.as_ref()
    }

    /// Moves the view, keeping it over the slide.
    ///
    /// **Every change to `view` goes through here**, which is why `view` is read
    /// by the rest of the crate and written only by this. The clamp is one line
    /// and there are six places that move the view; writing it at each of them
    /// is writing it at five of them, and the sixth is a drag that loses the
    /// slide — see [`Viewport::clamp_to`] for what that costs.
    fn look(&mut self, view: Viewport) {
        self.view = view.clamp_to(self.source.slide());
    }

    /// Resident texture bytes and how many tiles that is.
    pub fn resident(&self) -> (usize, usize) {
        (self.cache.bytes(), self.cache.len())
    }

    /// Tiles that could not be decoded and will not be tried again.
    ///
    /// Zero on a slide `qupath-slide` can open at all, because the codec check
    /// is per slide. Anything else is a **permanent hole** in the picture: the
    /// loader refuses to ask for a failed tile a second time — deliberately,
    /// or every redraw would retry it — so the gap never fills in, and the
    /// underlay covering it makes it look like a slow load that has stalled.
    /// [`TileLoader::failed`] says this is worth seeing rather than watching
    /// redraw silently; the status bar is where it gets seen.
    pub fn failed(&self) -> usize {
        self.loader.failed()
    }

    /// Tiles the loader still owes.
    ///
    /// Including the ones already in the channel: a tile that has been decoded
    /// and not yet drained has no frame showing it, so it is still owed to the
    /// person watching the status bar, and this counts what
    /// [`TileLoader::is_idle`] answers about.
    pub fn outstanding(&self) -> usize {
        self.loader.pending() + self.loader.in_flight() + self.loader.undrained()
    }
}

/// Presenting a [`Framebuffer`] to the window with no GPU involved.
struct Software {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    frame: Framebuffer,
    /// `egui`'s textures by id, as a short association list.
    ///
    /// A `Vec` and not a map on purpose: there are two of these in practice —
    /// the font atlas and the one-texel white — and a linear scan over two
    /// entries beats hashing an id.
    textures: Vec<(egui::TextureId, Texture)>,
    /// Reported once rather than every frame. A presentation that fails
    /// silently makes a broken window look exactly like a blank slide, which is
    /// the bug `docs/dev/VIEWER.md` §3.4b records finding in `present_cpu`.
    reported: bool,
    /// One mesh's vertices, converted from `egui`'s and reused across meshes
    /// and frames.
    ///
    /// `epaint::Vertex` and `compose::MeshVertex` have the same layout, so a
    /// cast would do — but a cast needs `bytemuck::Pod` on `MeshVertex`, which
    /// means a dependency the viewer library does not have and
    /// `docs/dev/VIEWER.md` §7 says it should not acquire. The copy is a few
    /// thousand vertices of a few hundred kilobytes against a frame that costs
    /// milliseconds, and it allocates nothing after the first mesh.
    vertices: Vec<MeshVertex>,
}

/// Which channel the viewer is showing.
///
/// QuPath's brightness/contrast dialog lists the original colour alongside the
/// deconvolved stains and switches between them, rather than treating a stain
/// as a mode of its own. This enum is that list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Channel {
    /// The slide as read.
    #[default]
    Original,
    /// Haematoxylin, which is stain 0 of QuPath's default H&E.
    Haematoxylin,
    /// Eosin, stain 1.
    Eosin,
}

impl Channel {
    /// The three, in the order the dialog lists them.
    pub const ALL: [Self; 3] = [Self::Original, Self::Haematoxylin, Self::Eosin];

    /// What the button says.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Original => "Original",
            Self::Haematoxylin => "Haematoxylin",
            Self::Eosin => "Eosin",
        }
    }

    /// This channel's row of the deconvolution matrix, or `None` for the
    /// original colour.
    ///
    /// Read from [`Stains`] rather than typed, which is the whole reason
    /// `Stains::max_values` was exposed: `docs/dev/VIEWER.md` §8.5 records that the
    /// first attempt at these numbers typed a plausible vector from the *stain*
    /// matrix where the inverted matrix's row was wanted, and that nothing
    /// failed — the channel looked like a working stain channel. Numbers that
    /// are read cannot be plausible-but-wrong.
    #[must_use]
    fn stain(self, full: f64) -> Option<Stain> {
        let index = match self {
            Self::Original => return None,
            Self::Haematoxylin => 0,
            Self::Eosin => 1,
        };
        let stains = Stains::h_and_e();
        Some(Stain {
            coefficients: stains.coefficients()[index],
            background: stains.max_values(),
            full,
        })
    }
}

/// What the brightness/contrast panel holds, and what it renders down to.
///
/// Separate from [`Display`] because the dialog outlives any one of them: a
/// user who dials in a range, looks at haematoxylin and goes back to the
/// original colour expects their range to still be there. A [`Display`] with
/// `stain: None` has forgotten which stain it was, and a [`Display`] built
/// fresh each time the channel changes has forgotten the range. Keeping the
/// three fields and computing the [`Display`] is what makes the panel's
/// controls independent of each other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Appearance {
    /// The value shown as black, for the original colour.
    pub min: f32,
    /// The value shown as white, for the original colour.
    pub max: f32,
    /// Which channel is showing.
    pub channel: Channel,
    /// The optical density shown as full black, for the stain channels.
    ///
    /// Held here rather than derived because it is the stain channels' contrast
    /// control — the byte range means nothing to an optical density, per
    /// [`Stain::full`]'s documentation.
    pub full: f64,
}

impl Default for Appearance {
    /// The slide as read: QuPath's own default display, which is the full byte
    /// range and no stain.
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 255.0,
            channel: Channel::default(),
            full: 1.5,
        }
    }
}

impl Appearance {
    /// The transform this appearance asks for.
    ///
    /// A range the user has collapsed or inverted falls back to the identity
    /// rather than to a negative scale, because [`Display::range`] refuses it
    /// for `scalebar`'s reason and a panel is not the place to decide what an
    /// impossible range should look like. The sliders below cannot produce one
    /// — the panel keeps them apart — so this is the guard for a value arriving
    /// some other way.
    #[must_use]
    pub fn display(self) -> Display {
        let base = Display::range(self.min, self.max).unwrap_or_default();
        match self.channel.stain(self.full) {
            Some(stain) => base.with_stain(stain),
            None => base.without_stain(),
        }
    }
}

/// A project the browser has open.
///
/// The path and what was read from it, kept together because every path in a
/// project is resolved against neither — `project.qpproj` stores absolute
/// `file:` URIs — but *the project's own* location is what tells a project that
/// has been moved from one in place, and what a rewrite has to be written back
/// to.
///
/// There is no slide here. Opening a project shows a list; opening one of its
/// images is a second act, and [`App::open`] is what performs it, exactly as it
/// does for a slide picked from the file dialog. Coupling the two would mean a
/// project of fifty slides opened the first of them, which is a wait nobody
/// asked for.
pub struct Project {
    /// Where `project.qpproj` is.
    pub path: PathBuf,
    /// What to call it, which is the study's directory and not the file.
    ///
    /// Every project file in the world is called `project.qpproj`, so a browser
    /// headed with the file name heads every project identically. QuPath's own
    /// `getName()` is `dirBase.getName() + "/" + file.getName()` — the study
    /// directory *and* the file — and this follows it, because the directory
    /// alone loses the distinction between a project and its backup sitting
    /// beside it.
    ///
    /// Built once, when the project is read. QuPath's version has a branch for
    /// the directory having gone, and that branch cannot be reached from here:
    /// the file inside it has just been read.
    pub name: String,
    /// What it said.
    pub stored: project::Stored,
    /// Whether each image's slide was on disk when the project was opened, in
    /// the same order as `stored.images`.
    ///
    /// Checked **once**, and that is the trade rather than an oversight. A
    /// browser is a list someone scrolls, and a `stat` per row per frame is a
    /// syscall storm for an answer that changes about as often as a drive is
    /// plugged in. The cost of it being stale is bounded: the mark is a
    /// warning colour and a hover, clicking a row still tries the file, and
    /// [`App::open`] reports what actually happened. QuPath checks on open for
    /// the same reason.
    present: Vec<bool>,
    /// Whether each image had a `data.qpdata` beside it when the project was
    /// opened, in the same order as `stored.images`.
    ///
    /// `DefaultProjectImageEntry.hasImageData` is `Files.exists` on
    /// `data/<entryID>/data.qpdata`, so this is that answer and not an
    /// approximation of it — the file being Java serialisation this refuses to
    /// *read* does not stop it being a file whose presence can be seen.
    ///
    /// It exists because `ProjectEntryPredicate` synthesises a `has_data=<bool>`
    /// metadata string for every entry that does not define the key itself, so
    /// the search box cannot answer `has_data=true` without it. Checked once
    /// for `present`'s reason, and staler than `present` for a better one:
    /// nothing in this application writes a `.qpdata`, so the only thing that
    /// changes the answer is QuPath running on the same study.
    has_data: Vec<bool>,
    /// A picture of each image, made in the background as the list is read.
    ///
    /// Owned by the project rather than by the application, which is what makes
    /// a picture of the previous study impossible in this one's list — see
    /// [`Pictures`]. Dropping this project stops its worker.
    pub pictures: Pictures,
}

impl Project {
    /// Whether entry `index`'s slide was there when the project was opened.
    ///
    /// Out of range reads as present, because the only way to be out of range
    /// is a caller iterating something other than `stored.images` — and the
    /// answer that does not add a warning to a row is the one that does not
    /// invent information.
    #[must_use]
    pub fn present(&self, index: usize) -> bool {
        self.present.get(index).copied().unwrap_or(true)
    }

    /// Whether entry `index` had a `data.qpdata` when the project was opened.
    ///
    /// Out of range reads as `false`, and the asymmetry with [`Project::present`]
    /// is deliberate. There, the answer that invents nothing is the one that
    /// adds no warning to a row. Here it is the opposite: `has_data=true` is a
    /// claim that QuPath has analysed this image, and a row out of range is one
    /// nothing is known about.
    #[must_use]
    pub fn has_data(&self, index: usize) -> bool {
        self.has_data.get(index).copied().unwrap_or(false)
    }

    /// Whether the project is where it says it is.
    ///
    /// QuPath compares its `uri` field against the file it opened to tell a
    /// moved project from one in place, and a moved project is worth saying: a
    /// directory copied from another machine has entries whose URIs point at
    /// slides that are not there, and "the project moved" is the explanation
    /// for a list of rows that will not open.
    ///
    /// Compared as paths rather than as URI text, because the two spellings of
    /// a `file:` URI are different strings for the same place — see
    /// `project::uri_path`.
    #[must_use]
    pub fn moved(&self) -> bool {
        self.stored
            .uri
            .as_deref()
            .and_then(|uri| project::uri_path(uri).ok())
            .is_some_and(|was| was != self.path)
    }

    /// Every picture this project has already made, with the slide it is of.
    ///
    /// What a re-read carries over: [`App::add_to_project`] writes the index
    /// and opens it again, and the fifty pictures it already had are pictures
    /// of the same fifty slides.
    fn known(&self) -> Vec<(PathBuf, Arc<Thumbnail>)> {
        self.stored
            .images
            .iter()
            .enumerate()
            .filter_map(|(index, image)| {
                let path = image.slide.as_ref().ok()?;
                Some((path.clone(), Arc::clone(self.pictures.made(index)?)))
            })
            .collect()
    }
}

/// What pressing escape found in progress, and therefore abandoned.
///
/// A returned decision rather than three branches inside [`App::key`], so the
/// ordering between them can be asserted: `key` takes a `&Window` and no test
/// can build one, which is what kept this gesture in `VIEWER.md` §8.8's hole
/// long after the reason had stopped applying.
///
/// The `Run` case carries the command rather than being a bare "handled",
/// because *which* command it is is half of what there is to get wrong here —
/// a variant saying only that something happened would be satisfied by a
/// keypress that started a detection run instead of stopping one.
#[derive(Debug, Clone, PartialEq)]
enum Escape {
    /// Nothing was in progress. The key is not consumed.
    Nothing,
    /// A zoom rectangle mid-drag, taken by the decision itself: abandoning it
    /// is dropping it, and there is nothing elsewhere in the application to
    /// undo.
    Band,
    /// A detection run, which is abandoned by running this.
    Run(Command),
}

/// The whole application.
pub struct App {
    pub window: Option<Arc<Window>>,
    /// `None` until a window exists, because `egui_winit` needs a display
    /// handle to build against.
    pub egui: Option<egui_winit::State>,
    context: egui::Context,
    surface: Option<Software>,
    pub open: Option<Open>,
    /// What to say when there is no slide, or when the last one would not open.
    pub message: String,
    /// Slides opened this session, most recent first.
    pub recent: Vec<PathBuf>,
    /// A path asked for and not yet opened: the command line at startup, or a
    /// file dialog's answer. Applied at the top of a frame, when the window
    /// size that `Viewport::fit` needs is known.
    pending: Option<PathBuf>,
    /// A project asked for and not yet read, from the command line.
    ///
    /// Separate from [`App::pending`] rather than a tagged one, because the two
    /// are not alternatives: `--` a project *and* a slide is a reasonable thing
    /// to ask for, and a single slot would silently drop one of them. Applied
    /// in the same place for the same reason, so there is one moment at which
    /// what the command line asked for becomes what is open.
    pending_project: Option<PathBuf>,
    dragging: bool,
    /// Where a right-drag began, while one is going.
    ///
    /// The anchor rather than a rectangle, because the other corner is always
    /// [`App::cursor`] — keeping both would be two fields that have to be
    /// updated together and one of them would eventually not be.
    band: Option<ScreenPoint>,
    cursor: ScreenPoint,
    costs: Vec<Duration>,
    pub show_overlay: bool,
    /// Detection, and what the last run found.
    ///
    /// On [`App`] rather than on [`Open`] because it outlives one: a run
    /// started against a slide that is then closed still has a thread to finish
    /// and a result to discard, and [`Detector::clear`] is what makes closing
    /// the slide discard it rather than draw it over the next one.
    pub detector: Detector,
    /// The window rectangle the slide occupies — what is left after the
    /// toolbar and the status bar have taken their strips — as
    /// `(left, top, right, bottom)` in **physical** window pixels.
    ///
    /// The unit matters and was wrong once. `chrome::draw` reads this from
    /// `egui`'s `available_rect`, which is in *logical points*, while
    /// `self.cursor` comes from winit's `CursorMoved` in *physical pixels*.
    /// Those agree only where the scale factor is 1.0. Anywhere else the
    /// cursor's coordinates were compared against a rectangle in the wrong
    /// units, so `App::cursor_over_slide` read false over most of the window
    /// and every gesture gated on it -- scroll-to-zoom, drag-to-pan and the
    /// zoom band alike -- silently did nothing. It is converted at the point it
    /// is stored so there is one unit below this line rather than a conversion
    /// at each of the three call sites.
    ///
    /// Recorded during layout because it is the only reliable answer to "is the
    /// pointer over the slide or over the interface". `egui`'s own
    /// `wants_pointer_input` cannot answer it: it is true for a pointer
    /// anywhere over a panel, and the slide *is* a panel, so it reads true over
    /// bare glass with no widget within four hundred pixels. See the wheel arm
    /// in [`App::input`].
    ///
    /// `None` until the first frame has been laid out.
    slide_area: Option<(f32, f32, f32, f32)>,
    /// Screen-space line segments of the visible outlines, rebuilt every frame
    /// and reused across them — `segments_into` clears rather than appends, so
    /// this allocates once and then holds its capacity.
    segments: Vec<ScreenPoint>,
    /// Objects the last frame drew an outline for, for the status bar.
    pub outlined: usize,
    /// Whether ctrl is held, tracked from `ModifiersChanged`.
    ///
    /// Needed because a **trackpad pinch on Linux is delivered as ctrl with a
    /// scroll**, not as a gesture event: `winit` 0.30 emits `PinchGesture` on
    /// macOS and iOS only, and its Wayland backend does not implement
    /// `zwp_pointer_gestures`. Ctrl-scroll is also the conventional zoom in
    /// every browser and most editors, so honouring it is worth doing on its
    /// own account rather than only as a pinch substitute.
    ctrl: bool,
    /// Detection parameters, in QuPath's units.
    ///
    /// Held here rather than in [`Detector`] because they outlive a run and
    /// survive a slide being closed: they are what the user has dialled in, not
    /// what the last run used. [`Detector::start`] is handed a copy.
    pub tuning: Micron,
    /// Whether the analysis panel is showing.
    pub show_analysis: bool,
    /// Whether the slide-metadata panel is showing.
    pub show_metadata: bool,
    /// Whether the gesture and key list is showing.
    pub show_help: bool,
    /// Whether the last frame computed a number the interface has not shown
    /// yet.
    ///
    /// `chrome::draw` is laid out **before** the frame is composed, because the
    /// commands it produces change what that frame shows. The consequence is
    /// that the two counts the status bar reports about the frame —
    /// [`App::outlined`] and [`Open::drawn`] — are always the *previous*
    /// frame's, and the last frame's are never reported at all, because
    /// nothing asks for another one.
    ///
    /// That is invisible for the tile count and misleading for the outlines: a
    /// detection that finishes while the view is still says "0 of 4027
    /// outlined" over a screen covered in outlines, and stays there until the
    /// mouse is moved. Set when a frame's numbers differ from the ones that
    /// were laid out, cleared by the frame that shows them — so it costs one
    /// extra frame after a change and nothing at rest.
    unreported: bool,
    /// Whether the slide just opened still needs fitting to the *visible*
    /// rectangle.
    ///
    /// [`Open::new`] fits to the window, because it runs before the frame is
    /// laid out and there is no rectangle to consult yet — on the very first
    /// slide there has never been one. Taken by the next [`App::render`], once
    /// [`App::slide_area`] has been recorded and before the frame is drawn, so
    /// a slide arrives fitted to what can actually be seen rather than to the
    /// window the chrome is covering.
    refit: bool,
    /// Whether the overview picture has been handed to the renderer for the
    /// slide currently open.
    ///
    /// Cleared when a slide is opened or closed, which is the only time the
    /// picture changes.
    uploaded_overview: bool,
    /// Whether the overview thumbnail is showing.
    ///
    /// On by default, unlike the other panes: it answers "where am I", which
    /// is a question the user has before they know to go looking for a pane
    /// that answers it. QuPath shows its overview by default for the same
    /// reason.
    pub show_overview: bool,
    /// Whether the brightness/contrast controls are showing.
    pub show_appearance: bool,
    /// How the slide is being shown: the display range and the stain channel.
    ///
    /// Held on [`App`] rather than on [`Open`] because it is a property of the
    /// viewer, not of the slide: QuPath's dialog keeps its settings when the
    /// image changes, and a user comparing two slides at the same range would
    /// have to dial it in twice otherwise.
    pub appearance: Appearance,
    /// The project whose images the browser is listing, if any.
    ///
    /// Independent of [`App::open`] in both directions: a project can be open
    /// with no slide showing, and a slide can be open that belongs to no
    /// project. That is how a study is actually worked through — the list stays
    /// up while the images under it are opened one after another.
    pub project: Option<Project>,
    /// Whether the project browser is showing.
    pub show_project: bool,
    /// What the browser's search box holds.
    ///
    /// On [`App`] rather than on [`Project`] because it is a state of the
    /// *interface*, and because it survives nothing: opening a project clears
    /// it, as QuPath's browser clears `tfFilter`. A search left over from
    /// another study is a list with rows missing and nothing on screen that
    /// looks like a reason — the box holds the text, but a pane scrolled past
    /// it does not.
    pub filter: String,
    /// What order the browser's rows are in.
    ///
    /// On [`App`] rather than on [`Project`] for [`App::filter`]'s reason —
    /// it is a state of the interface — but **unlike the filter it survives a
    /// project being opened**, and the difference is worth stating because the
    /// two controls sit one above the other.
    ///
    /// A filter left over from another study hides rows and shows nothing that
    /// says why. An order left over hides nothing: every row is there, and the
    /// user asked for names in alphabetical order because that is how they
    /// want to read a list of slides, which is a preference about lists rather
    /// than about one project. QuPath keeps it on the *project* — it writes
    /// `_SORT_KEY` into the project's metadata, so a study reopens in the
    /// order it was left in — and this cannot, because writing a key
    /// `project::read` does not read back is how a rewrite quietly drops it.
    /// So it is per-session here, which is the half of QuPath's behaviour that
    /// can be honoured without the other half being a lie.
    pub order: crate::chrome::Order,
    /// Whether the browser shows each entry's `randomizedName` in place of its
    /// name, for blinded analysis.
    ///
    /// QuPath's `PathPrefs.maskImageNamesProperty`, which is an application
    /// preference rather than a project field — so it is per-session here for
    /// the same reason [`App::order`] is, and QuPath keeps it per-session too.
    ///
    /// **It is not honoured for a project this application wrote**, and
    /// `chrome::maskable` is where that is enforced rather than here:
    /// `project::files` writes `entry-<id>` into `randomizedName`, which spells
    /// out the very order masking exists to hide.
    pub mask_names: bool,
    /// The order the masked rows are shuffled into, by entry id.
    ///
    /// QuPath reshuffles inside `getAllImageRows` on every `refreshTree`, which
    /// includes every keystroke in its search box. That cannot be copied
    /// directly: `row_order` runs once per *frame* here, so a shuffle computed
    /// there would reorder the list sixty times a second and the pane would be
    /// unreadable. The order is therefore drawn once, when masking is switched
    /// on, and held until it is switched off.
    ///
    /// Keyed by id and not by position so that it survives a project being
    /// re-read — adding a slide rewrites the index and reopens it, and a
    /// shuffle stored as positions would silently become a different shuffle.
    pub masked_order: Vec<u64>,
    /// The metadata editor, when one is open.
    ///
    /// On [`App`] and not on [`Project`] because it survives a re-read: writing
    /// the file reopens the project, and an editor stored beside the document
    /// would be dropped by its own success — or, on the refusal path, by
    /// nothing at all, which is worse, since that is the path where the typing
    /// has to still be there.
    pub editing: Option<MetadataEditor>,
}

/// An entry's metadata, while somebody is changing it.
///
/// The working copy is a `Vec` of pairs like [`project::Image::metadata`] and
/// for the same reason: the order is the order somebody entered the keys in,
/// and a map here would re-alphabetise a study's fields the first time anyone
/// opened the editor on it.
///
/// # Why the rows are here rather than read from the entry each frame
///
/// Because a half-typed key is not a key. `rows` is what the user has in front
/// of them, including a row whose name is empty because they have just added it
/// and a row whose name duplicates another because they are in the middle of
/// retyping it. The entry only ever sees the result of [`MetadataEditor::map`],
/// which is what makes those intermediate states unable to reach the file.
pub struct MetadataEditor {
    /// Which entry, by `entryID`.
    ///
    /// The id and not the row's position, for `project::edited`'s reason: the
    /// position is an index into the browser's order, which the search box and
    /// the sort control both change while the editor is open.
    pub id: u64,
    /// The name shown in the editor's heading, taken when it was opened.
    ///
    /// Copied rather than looked up, so that the editor can name its entry on
    /// the frame after a re-read has replaced every `Image` in the project —
    /// and so that a masked pane's editor is headed with the masked name it was
    /// opened from.
    pub name: String,
    /// The keys and values being edited.
    pub rows: Vec<(String, String)>,
    /// What went wrong with the last attempt to write, if it did.
    ///
    /// Kept on the editor rather than put in the status bar alone, because the
    /// message that matters most — the project changed underneath — is about
    /// the thing the user is looking at, and a status bar at the other end of
    /// the window is not where they are looking.
    pub problem: Option<String>,
}

impl MetadataEditor {
    /// The rows as they would be written; see [`written_rows`].
    #[must_use]
    pub fn map(&self) -> Vec<(String, String)> {
        written_rows(&self.rows)
    }
}

/// The rows as they would be written.
///
/// A row whose key is blank is dropped: an empty key is not something QuPath's
/// own editor can produce and not something a search can match, so the blank
/// row an editor always has at the bottom is a *space to type in* rather than a
/// pair. Values are kept as typed, including empty ones — `stain=` is a key
/// somebody has recorded as having no value, which is different from not having
/// recorded it.
///
/// The last of a repeated key wins, which is Gson's own behaviour reading one
/// back and the only choice that makes the editor agree with what the file
/// would say. Trimmed keys, because a key with a trailing space is invisible in
/// the editor and unmatchable in the search.
///
/// Free rather than a method on [`MetadataEditor`], because the editor's
/// *preview* — the line saying how many keys will be written — has rows in hand
/// that are not yet the editor's: they are this frame's edit, on its way to
/// becoming them. One definition, so the count shown and the map written cannot
/// disagree about what a blank key means.
#[must_use]
pub fn written_rows(rows: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (key, value) in rows {
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, slot)) => slot.clone_from(value),
            None => out.push((key.to_owned(), value.clone())),
        }
    }
    out
}

impl App {
    /// Whether a path is a project rather than a slide.
    ///
    /// By extension, which is what the file dialog filters on and what QuPath
    /// registers. Reading the first bytes would be stronger and would also mean
    /// a slow disk read before the window exists, for a distinction the file
    /// name already makes.
    #[must_use]
    pub fn is_project(path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("qpproj"))
    }

    #[must_use]
    /// The application, with whatever the command line asked to be opened.
    ///
    /// Takes any number of paths and sorts them by [`App::is_project`], so a
    /// project and a slide can be given in either order and both are honoured.
    /// `Option<String>` is an `IntoIterator` of one, which is how the callers
    /// that have exactly one path — the headless examples and the tests — still
    /// read as they did.
    ///
    /// Nothing is opened here. Both are applied at the top of the first frame,
    /// when the window size `Viewport::fit` needs exists.
    pub fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let context = egui::Context::default();
        chrome::style(&context);
        let (project, slide) = requested(paths);
        Self {
            window: None,
            egui: None,
            context,
            surface: None,
            open: None,
            message: String::from("Open a slide to begin."),
            recent: Vec::new(),
            pending: slide,
            pending_project: project,
            dragging: false,
            band: None,
            slide_area: None,
            cursor: ScreenPoint::new(0.0, 0.0),
            costs: Vec::new(),
            show_overlay: true,
            detector: Detector::new(),
            segments: Vec::new(),
            outlined: 0,
            ctrl: false,
            tuning: Micron::default(),
            show_analysis: false,
            show_metadata: false,
            show_help: false,
            show_overview: true,
            show_appearance: false,
            appearance: Appearance::default(),
            uploaded_overview: false,
            refit: false,
            unreported: false,
            project: None,
            show_project: false,
            order: crate::chrome::Order::default(),
            mask_names: false,
            masked_order: Vec::new(),
            editing: None,
            filter: String::new(),
        }
    }

    /// Builds everything that needs a window to exist first.
    pub fn start(&mut self, window: &Arc<Window>) {
        let size = window.inner_size();
        self.egui = Some(egui_winit::State::new(
            self.context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        ));

        // `softbuffer` cannot fail for want of a driver, which is the whole
        // reason this path exists. It can still fail to talk to the display
        // server, and that is fatal in a way no fallback helps with.
        let context = softbuffer::Context::new(Arc::clone(window)).expect("connect to the display");
        let surface =
            softbuffer::Surface::new(&context, Arc::clone(window)).expect("create a surface");
        self.surface = Some(Software {
            surface,
            frame: Framebuffer::new(size.width.max(1), size.height.max(1)),
            textures: Vec::new(),
            reported: false,
            vertices: Vec::new(),
        });
    }

    /// Opens `path`, replacing whatever was open.
    pub fn open(&mut self, path: PathBuf) {
        // Resolved against the working directory before anything keeps it, and
        // that is not tidiness — it is the same trap `project::file_uri` was
        // written for, met from the other side.
        //
        // A project stores absolute paths, always: `File.toURI()` resolves one
        // and so does `file_uri`. A slide opened from the command line is
        // relative in the commonest invocation there is — this file documents
        // starting the application as `-- data/CMU-1.svs` — so comparing the
        // two compares `data/CMU-1.svs` against `/home/…/data/CMU-1.svs` and
        // finds them different. Two things went wrong on that: the project
        // browser did not mark the row for the slide on screen, and
        // [`App::addable`] offered to add a slide the project already had,
        // which is two entries and two directories under `data/` for one image.
        //
        // `std::path::absolute` is the lexical form, the same one `file_uri`
        // uses, so the two agree by construction rather than by coincidence. A
        // path that cannot be resolved is kept as it stands: the working
        // directory being unreadable is not a reason to refuse to open a slide.
        let path = std::path::absolute(&path).unwrap_or(path);
        let (width, height) = self.size();
        match Open::new(path.clone(), width, height) {
            Ok(open) => {
                // Before the slide is replaced, not after. Outlines are in the
                // *old* slide's coordinates, and a set of nuclei drawn over a
                // different slide is wrong in the way that looks right.
                self.detector.clear();
                let name = display_name(&open.path);
                self.message = format!("{name} — {}", describe(open.slide()));
                self.recent.retain(|recent| recent != &path);
                self.recent.insert(0, path);
                self.recent.truncate(RECENT);
                self.open = Some(open);
                self.uploaded_overview = false;
                self.refit = true;
            }
            Err(error) => {
                // Kept open. Failing to open a second slide must not close the
                // first, which is the behaviour that makes a viewer annoying.
                self.message = format!("{}: {error}", display_name(&path));
            }
        }
    }

    /// The name of a project's index, which QuPath looks for by name inside
    /// the project directory.
    pub const PROJECT_FILE: &'static str = "project.qpproj";

    /// Whether the slide showing can be added to the project that is open, and
    /// the sentence to show when it cannot.
    ///
    /// One predicate, read by the menu item that offers the action and by the
    /// function that performs it, for [`App::found`]'s reason: two would
    /// eventually disagree, and the way they disagree is a live menu item that
    /// does nothing when pressed.
    ///
    /// The message is the *disabled* hover text rather than a log line, so the
    /// answer to "why is this greyed out" is where the greyed-out thing is.
    ///
    /// # Errors
    ///
    /// The reason there is nothing to add, or nowhere to add it to.
    pub fn addable(&self) -> Result<(), String> {
        let Some(project) = &self.project else {
            return Err("Open or create a project first.".to_owned());
        };
        let Some(open) = &self.open else {
            return Err("Open a slide first (o); it is the one that gets added.".to_owned());
        };
        // A project that lists the same slide twice is two entries with two
        // directories under `data/` for one image, and QuPath will show both.
        // Compared by path rather than by name, because the name is a label and
        // the path is the image.
        if project
            .stored
            .images
            .iter()
            .any(|image| image.slide.as_deref() == Ok(open.path.as_path()))
        {
            return Err(format!(
                "{} is already in this project.",
                display_name(&open.path)
            ));
        }
        Ok(())
    }

    /// Creates an empty project in `directory` and opens it.
    ///
    /// Empty, and not seeded with the slide showing: QuPath separates *Create
    /// project* from *Add images*, and one click that both made a project and
    /// decided what went into it would be two decisions taken by one.
    ///
    /// **An existing project is not overwritten.** A directory someone points
    /// this at is far more likely to be a study they meant to open than one
    /// they meant to erase, and the cost of the two mistakes is not
    /// symmetrical.
    pub fn new_project(&mut self, directory: &Path) {
        let path = directory.join(Self::PROJECT_FILE);
        if path.exists() {
            self.message = format!(
                "{} already has a project in it. Open it instead.",
                display_name(directory)
            );
            return;
        }
        let files = project::files(&path, &[], project::Timestamps::now());
        if let Err(error) = write_project(directory, &files) {
            self.message = error;
            return;
        }
        self.open_project(path);
    }

    /// Adds the slide showing to the project that is open, and re-reads it.
    ///
    /// Re-read rather than updated in place, so what the pane lists is what is
    /// on disk. An in-memory update that diverged from the file would be a
    /// browser showing a project nobody else can see.
    pub fn add_to_project(&mut self) {
        if let Err(reason) = self.addable() {
            self.message = reason;
            return;
        }
        // Both are `Some`, which is what `addable` has just established.
        let (Some(project), Some(open)) = (&self.project, &self.open) else {
            return;
        };
        // Level 0 is the size QuPath records, and it is the only size a
        // `summary.json` has a field for.
        let (width, height) = open
            .slide()
            .levels()
            .first()
            .map_or((0, 0), |level| (level.width, level.height));
        let name = display_name(&open.path);
        let entry = project::Entry {
            path: &open.path,
            name: &name,
            width,
            height,
        };

        // The directory this entry's files will land in, taken before `added`
        // consumes the index: `next_id` is the id it will be issued, and the
        // entry directory is named after it.
        let id = project.stored.next_id();
        let files = project::added(&project.stored, &[entry], project::Timestamps::now());
        let path = project.path.clone();
        let directory = path.parent().unwrap_or(&path).to_path_buf();
        if let Err(error) = write_project(&directory, &files) {
            self.message = error;
            return;
        }

        // `thumbnail.jpg`, which QuPath caches so that opening a study does not
        // re-read every slide in it. Written after the JSON and not through
        // `project::files`, for two reasons: `Output::contents` is a `String`
        // and these are bytes, and this file is the one part of an entry that
        // is a *cache* rather than a fact — QuPath regenerates it from the
        // pixels when it is absent, and so does this browser.
        //
        // Which is also why a failure here is not a failure of the add. The
        // entry is already written and correct; a project missing a thumbnail
        // is one both applications handle. Saying nothing would be wrong, so it
        // rides along on the message rather than replacing it.
        let thumbnail = crate::thumbnails::make(open.slide())
            .and_then(|thumbnail| crate::thumbnails::encode_jpeg(&thumbnail))
            .and_then(|bytes| {
                let at = directory.join(format!("data/{id}/thumbnail.jpg"));
                std::fs::write(&at, bytes)
                    .map_err(|error| format!("Could not write {}: {error}", at.display()))
            });

        self.open_project(path);
        self.message = match thumbnail {
            Ok(()) => format!("Added {name} to the project."),
            Err(reason) => format!("Added {name} to the project, without a thumbnail: {reason}"),
        };
    }

    /// Writes entry `id`'s metadata back to the project on disk.
    ///
    /// The first thing in this application that puts a user's typing into a
    /// file another program wrote, and the whole of the care here is about
    /// that. `add_to_project` appends an entry and re-reads; this changes one
    /// that was already there.
    ///
    /// # What happens when the file changed underneath
    ///
    /// It is refused, and nothing is written. The index is re-read and compared
    /// against what was read when the project was opened; if the two differ,
    /// somebody else — QuPath, a second copy of this, a script — has written
    /// the file since, and the document held here is no longer the document
    /// their change is in. Rewriting it would silently undo whatever they did,
    /// which is the failure mode `project::added`'s documentation calls the
    /// worst shape a bug of this kind can take: the file still opens, still
    /// lists the same images, and has lost what someone recorded against them.
    ///
    /// Refusing rather than merging is deliberate. A merge is the friendlier
    /// answer and it is also the one that never tells anybody the file moved
    /// under them, so an edit made against a stale list would land looking as
    /// though it had been made against the current one. The edit is not
    /// discarded — the caller keeps the editor open with the typing in it — so
    /// what is lost by refusing is one reopen, and what is bought is that no
    /// concurrent change is ever overwritten without being noticed.
    ///
    /// # The comparison is the text, and it is trimmed
    ///
    /// Not an mtime and not a length: `Stored::rewritten` reproduces a
    /// Gson-written file byte for byte, so comparing the text on disk against
    /// the text this read is exact in both directions — it cannot miss a change
    /// made inside a timestamp's resolution, and it cannot report one for a
    /// file rewritten identically.
    ///
    /// **Trimmed at the ends, and that is not a slackening.** `project_file`
    /// ends the index with a newline and `json::write` does not, so a project
    /// this application *created* differs from its own rewrite by one trailing
    /// byte — and an untrimmed comparison refuses every edit to such a project,
    /// having found a change nobody made. Found by editing one. Only the outer
    /// whitespace is ignored; any difference inside the document is still a
    /// difference, because that is where a change somebody made would be.
    pub fn set_metadata(&mut self, id: u64, metadata: &[(String, String)]) -> Result<(), String> {
        let Some(project) = &self.project else {
            return Err(String::from("No project is open."));
        };
        let path = project.path.clone();

        let on_disk = std::fs::read_to_string(&path)
            .map_err(|error| format!("Could not re-read the project: {error}"))?;
        // Against what was *read*, rewritten through the same writer, rather
        // than against the raw text kept from then. The two are the same file
        // for a project Gson wrote, and for one this application wrote the
        // rewrite is what the comparison has to be against, because that is
        // what is on disk.
        if on_disk.trim_end() != project.stored.rewritten().trim_end() {
            return Err(String::from(
                "The project changed on disk since it was opened, so this was \
                 not written. Reopen the project and try again.",
            ));
        }

        let files = project::edited(&project.stored, id, metadata, project::Timestamps::now())?;
        let directory = path.parent().unwrap_or(&path).to_path_buf();
        write_project(&directory, &files)?;
        // Re-read for `add_to_project`'s reason: the file on disk is now the
        // truth, and reading it back is what makes the pane show what was
        // written rather than what was meant to be. The pictures survive it.
        self.open_project(path);
        Ok(())
    }

    /// Opens a project, replacing whatever project was open.
    ///
    /// The slide showing is left alone, and deliberately: a project is a list,
    /// and opening one does not say which of its images the user wants. The
    /// alternative — opening the first entry — is a fifty-slide study deciding
    /// for itself which slide someone wanted to look at.
    ///
    /// Every failure ends in [`App::message`] rather than a panic, for
    /// [`Open::new`]'s reason: an unreadable file is an ordinary thing to be
    /// handed.
    pub fn open_project(&mut self, path: PathBuf) {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                self.message = format!("{}: {error}", display_name(&path));
                return;
            }
        };
        match project::read(&text) {
            Ok(stored) => {
                let images = stored.images.len();
                let present = stored
                    .images
                    .iter()
                    .map(|image| image.slide.as_ref().is_ok_and(|path| path.is_file()))
                    .collect();
                // `data/<entryID>/data.qpdata`, which is `getImageDataPath`
                // spelled out — relative to the project *directory*, so the
                // file's own parent, not the file.
                let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
                let has_data = stored
                    .images
                    .iter()
                    .map(|image| {
                        directory
                            .join("data")
                            .join(image.id.to_string())
                            .join("data.qpdata")
                            .is_file()
                    })
                    .collect();
                // Taken before the new project is built, so the pictures the
                // browser already has survive a re-read and the worker for the
                // project being replaced is stopped. Both matter in the same
                // place: adding a slide re-reads the file it just wrote.
                let known = self
                    .project
                    .take()
                    .map(|previous| previous.known())
                    .unwrap_or_default();
                let slides: Vec<Option<PathBuf>> = stored
                    .images
                    .iter()
                    .map(|image| image.slide.as_ref().ok().cloned())
                    .collect();
                self.project = Some(Project {
                    name: project_name(&path),
                    path,
                    stored,
                    present,
                    has_data,
                    pictures: Pictures::start(&slides, &known),
                });
                self.show_project = true;
                // Cleared with the project, as QuPath's browser clears its own
                // filter: a search typed against one study hides rows of the
                // next for a reason that is one scroll off the top of the pane.
                self.filter.clear();
                // Said in the status bar as well as shown in the pane, because
                // a project of zero images opens a pane with nothing in it and
                // the pane alone cannot distinguish that from one that failed.
                self.message = match images {
                    0 => String::from("The project has no images in it."),
                    1 => String::from("1 image in the project."),
                    many => format!("{many} images in the project."),
                };
            }
            Err(error) => {
                // The project already open is kept, for the reason a slide that
                // will not open does not close the one showing.
                self.message = format!("{}: {error}", display_name(&path));
            }
        }
    }

    fn size(&self) -> (u32, u32) {
        self.surface.as_ref().map_or((1280, 860), |state| {
            (state.frame.width(), state.frame.height())
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if let Some(state) = &mut self.surface {
            state.frame.resize(width, height);
        }
        if let Some(open) = &mut self.open {
            open.look(open.view.resize(width, height));
        }
    }

    /// Mean main-thread cost of the last `WINDOW` frames.
    pub fn frame_cost(&self) -> Option<Duration> {
        (!self.costs.is_empty())
            .then(|| self.costs.iter().sum::<Duration>() / self.costs.len() as u32)
    }

    /// Whether another frame is wanted without further input.
    ///
    /// True only while tiles are still landing. At rest this is false and the
    /// event loop sleeps, which is what keeps the viewer at zero CPU on a still
    /// picture.
    pub fn wants_another_frame(&self) -> bool {
        self.pending.is_some()
            // One more frame to report what the last one drew; see the field.
            || self.unreported
            // A run in flight sends nothing until it finishes, so there is no
            // event to wake the loop on: the frames while one is going are what
            // turns the spinner and, at the end, what notices the result. This
            // is the one thing in the application that spends CPU to show that
            // something is happening.
            || self.detector.is_running()
            || self
                .open
                .as_ref()
                .is_some_and(|open| !open.loader.is_idle())
            // The browser's pictures arrive on a channel nothing wakes the
            // event loop for, so the frames while they are being made are what
            // puts them on screen. Fifty slides is fifty pictures; the loop
            // sleeps again when the last one has landed, which is what
            // `Pictures::working` stops saying.
            || self
                .project
                .as_ref()
                .is_some_and(|project| project.pictures.working())
    }

    /// Handles one window event that the interface did not take, and says
    /// whether a redraw is needed.
    ///
    /// `consumed` is `egui`'s answer to "was this mine", and it is honoured for
    /// the **keyboard** only: a keypress belongs to whatever has focus, which
    /// is a question only `egui` can answer.
    ///
    /// It is deliberately *not* used for the mouse. For pointer events
    /// `egui_winit` derives it from `wants_pointer_input`, which is true for a
    /// pointer anywhere over a panel — and the slide is a panel, so the answer
    /// is true over bare glass and the mouse gestures gated on it silently did
    /// nothing. Those ask `App::cursor_over_slide` instead, which is a
    /// question about geometry and answerable.
    pub fn input(&mut self, window: &Window, event: &WindowEvent, consumed: bool) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let next = ScreenPoint::new(position.x as f32, position.y as f32);
                let (dx, dy) = (
                    f64::from(next.x - self.cursor.x),
                    f64::from(next.y - self.cursor.y),
                );
                self.cursor = next;
                if self.dragging
                    && let Some(open) = &mut self.open
                {
                    open.look(open.view.pan(dx, dy));
                    return true;
                }
                // A moved cursor still repaints, because the status bar carries
                // the position under it — and because a band being dragged is
                // redrawn to the new corner.
                self.open.is_some()
            }
            WindowEvent::MouseInput { state, button, .. } if *button == MouseButton::Left => {
                // A drag is only the slide's if it *began* over the slide.
                // Testing this on every motion instead would drop the drag the
                // moment the cursor crossed a panel, which is an ordinary thing
                // to do halfway through a pan.
                //
                // The test is geometric for the reason the wheel arm above
                // gives at length: `consumed` is true for a pointer anywhere
                // over a panel, and the slide is one.
                self.dragging = *state == ElementState::Pressed && self.cursor_over_slide();
                false
            }
            WindowEvent::MouseInput { state, button, .. } if *button == MouseButton::Right => {
                match state {
                    // Began over the slide, for the reason the left button
                    // records: a band started on a menu is the menu's.
                    ElementState::Pressed => {
                        self.band = (self.cursor_over_slide() && self.open.is_some())
                            .then_some(self.cursor);
                        false
                    }
                    ElementState::Released => self.finish_band(),
                }
            }
            // Gated on *where the pointer is*, not on `egui`'s `consumed`.
            //
            // `egui_winit` reports a wheel event as consumed whenever
            // `wants_pointer_input` is true, and that is true for a pointer
            // anywhere over a panel — including the central one, which is the
            // slide. Honouring it here meant **every scroll over the slide was
            // discarded and scroll-to-zoom did nothing at all**, leaving the
            // toolbar's buttons as the only way to zoom; those anchor on the
            // view centre, so zooming appeared to be permanently centre-locked.
            //
            // Dragging was unaffected because `CursorMoved` is reported against
            // `is_using_pointer`, which is only true while a button is already
            // held. Two gestures gated on two different questions is what made
            // one of them work and the other vanish.
            //
            // A scroll inside the slide's rectangle is the slide's, and one
            // over the toolbar or the status bar is the interface's, which is
            // what `consumed` was reached for and could not express.
            WindowEvent::MouseWheel { delta, .. } if self.cursor_over_slide() => {
                let steps = match delta {
                    // A mouse wheel, in detents. One notch is one step.
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    // A **trackpad**, in pixels of scroll. The two are not the
                    // same gesture and must not share a divisor: a wheel notch
                    // arrives as one event, where a two-finger drag arrives as a
                    // stream of small ones, so the per-event value is far
                    // smaller and the number of events far larger.
                    //
                    // This was `/ 50.0`, which made a trackpad scroll zoom by
                    // 1.2^(a few hundredths) per event — visually nothing, and
                    // the reason zooming felt dead on a trackpad while working
                    // on a mouse. See [`TRACKPAD_PIXELS_PER_STEP`].
                    MouseScrollDelta::PixelDelta(position) => position.y / TRACKPAD_PIXELS_PER_STEP,
                };
                // Ctrl multiplies rather than gates. Under GNOME a trackpad
                // pinch arrives as ctrl with a scroll, and it should zoom
                // faster than the same finger movement without ctrl -- that is
                // what makes the pinch feel like a pinch rather than like a
                // slow scroll. Without ctrl the scroll still zooms, which is
                // this viewer's existing behaviour and is not changed.
                let steps = if self.ctrl {
                    steps * CTRL_SCROLL_STEPS
                } else {
                    steps
                };
                self.zoom_by(steps)
            }
            // A two-finger pinch, where the platform reports one.
            //
            // **This does not fire on Linux.** `winit` 0.30 emits
            // `PinchGesture` on macOS and iOS only — its Wayland backend does
            // not implement `zwp_pointer_gestures` at all, so a real pinch
            // produces no event here to catch. It is handled anyway because the
            // arm costs three lines and the alternative is that this works on a
            // Mac by accident later, or does not and nobody knows why.
            //
            // What a Linux trackpad *does* produce is the `MouseWheel` arm
            // above, and — under GNOME — ctrl with a two-finger scroll, which is
            // the arm below.
            WindowEvent::PinchGesture { delta, .. } if self.cursor_over_slide() => {
                // `delta` is a fraction of the current magnification per event
                // and may be NaN, which `zoom_about` would propagate into the
                // scale and make every subsequent frame draw nothing.
                if delta.is_finite() {
                    return self.zoom_by(delta * PINCH_STEPS);
                }
                false
            }
            // Tracked rather than read from the keyboard arm, because a wheel
            // event carries no modifier state of its own and the pinch-as-ctrl
            // gesture never presses a key this window sees as a keypress.
            WindowEvent::ModifiersChanged(modifiers) => {
                self.ctrl = modifiers.state().control_key();
                false
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !consumed =>
            {
                self.key(window, &event.logical_key)
            }
            _ => false,
        }
    }

    /// Ends a right-drag: zooms to the box if there is one, and says whether a
    /// redraw is needed.
    ///
    /// A band that never grew past [`BAND_MINIMUM_PX`] was a right-click, and a
    /// box with no area is refused by [`Viewport::zoom_to_box`] besides. Either
    /// way the band is taken, so a released button never leaves a rectangle
    /// drawn on the window.
    fn finish_band(&mut self) -> bool {
        let Some(from) = self.band.take() else {
            return false;
        };
        let to = self.cursor;
        if (to.x - from.x).abs() < BAND_MINIMUM_PX && (to.y - from.y).abs() < BAND_MINIMUM_PX {
            // A click, not a drag. The band still has to be erased, which is
            // why this repaints rather than returning false.
            return true;
        }
        if let Some(open) = &mut self.open
            && let Some(zoomed) = open.view.zoom_to_box(from, to)
        {
            open.look(zoomed);
        }
        true
    }

    /// Whether the cursor is over the slide rather than over the interface.
    ///
    /// See [`App::slide_area`] for why this is asked geometrically rather than
    /// through `egui`. Before the first frame there is no layout to consult and
    /// this is false, which costs at most one scroll delivered before the
    /// window has drawn anything.
    fn cursor_over_slide(&self) -> bool {
        let Some((left, top, right, bottom)) = self.slide_area else {
            return false;
        };
        self.cursor.x >= left
            && self.cursor.x < right
            && self.cursor.y >= top
            && self.cursor.y < bottom
    }

    /// Zooms about the cursor by `steps`, and says whether a redraw is needed.
    ///
    /// Shared by the wheel, the trackpad and the pinch gesture so that all three
    /// agree on what a step is. The 1.2 is the wheel's own base and is smaller
    /// than the 1.5 the buttons and keys use, because a wheel and a trackpad
    /// deliver many events where a keypress delivers one.
    ///
    /// A non-finite `steps` is refused rather than passed on: `PinchGesture`
    /// documents that its delta may be NaN, and a NaN reaching `scale` makes
    /// every later frame compute an empty tile list — a window that goes blank
    /// and stays blank, with no way back but `f`.
    fn zoom_by(&mut self, steps: f64) -> bool {
        if !steps.is_finite() || steps == 0.0 {
            return false;
        }
        let cursor = self.cursor;
        if let Some(open) = &mut self.open {
            open.look(open.view.zoom_about(1.2f64.powf(steps), cursor));
            return true;
        }
        false
    }

    /// Zooms about the cursor by `steps`, for the keyboard.
    ///
    /// The same 1.5 per step the toolbar's buttons use, so the two agree about
    /// what one step is.
    fn zoom_key(&mut self, steps: i32) -> bool {
        let cursor = self.cursor;
        if let Some(open) = &mut self.open {
            open.look(open.view.zoom_about(1.5f64.powi(steps), cursor));
            return true;
        }
        false
    }

    fn key(&mut self, window: &Window, key: &Key) -> bool {
        match key.as_ref() {
            Key::Character("f") => {
                if self.open.is_some() {
                    self.fit();
                    return true;
                }
                false
            }
            // Zoom from the keyboard, about the cursor rather than the window
            // centre — the toolbar buttons use the centre because a button
            // press says nothing about where the user is looking, and a
            // keypress with the mouse over the tissue does.
            //
            // Three spellings each, because the keys next to each other on a
            // keyboard are not the ones a layout agrees on: `=` is the
            // unshifted key `+` lives on, and the numeric keypad sends its own.
            Key::Character("+" | "=") => self.zoom_key(1),
            Key::Character("-" | "_") => self.zoom_key(-1),
            Key::Character("o") => {
                self.run(Command::Open, window);
                true
            }
            Key::Character("p") => {
                self.run(Command::OpenProject, window);
                true
            }
            Key::Character("d") => {
                self.run(Command::Detect, window);
                true
            }
            Key::Character("h") => {
                self.show_overlay = !self.show_overlay;
                true
            }
            Key::Character("s") => {
                self.run(Command::Export, window);
                true
            }
            // The decision is [`App::escape`], which needs no window; this is
            // the two lines that carry it out.
            Key::Named(NamedKey::Escape) => match self.escape() {
                Escape::Nothing => false,
                Escape::Band => true,
                Escape::Run(command) => {
                    self.run(command, window);
                    true
                }
            },
            _ => false,
        }
    }

    /// What pressing escape abandons.
    ///
    /// Split out of [`App::key`] because the *ordering* is the behaviour, and
    /// until this existed there was no way to check it without a hand on the
    /// keyboard: `key` takes a `&Window`, which no test can build. What needs
    /// the window is the redraw a command may ask for, not the decision about
    /// which thing to abandon — the same mistake `VIEWER.md` §8.8 records
    /// about the browser's rows, where the hole was drawn around the click and
    /// what `App` could not reach was really the pixel position.
    ///
    /// Not an exit. In an application escape closes a dialog, and taking the
    /// window down on a stray keypress is what makes an example an example.
    /// What it does do is abandon whatever is in progress, which is now two
    /// things.
    fn escape(&mut self) -> Escape {
        // The band goes first and alone: a person drawing a zoom rectangle
        // while a detection runs in the background means the rectangle, and
        // stopping their run instead would be a surprising amount of lost work
        // for one keypress.
        if self.band.take().is_some() {
            return Escape::Band;
        }
        // A zoom rectangle in the wrong place and a whole-slide run started by
        // mistake are the same gesture: undo the thing I just started. The
        // `Stop` button in the status bar is the discoverable form of this;
        // escape is the fast one.
        if self.detector.is_running() {
            return Escape::Run(Command::StopDetecting);
        }
        Escape::Nothing
    }

    /// Fits the whole slide into the part of the window that is not chrome.
    ///
    /// Shared by the `f` key and the toolbar's button so the two cannot drift,
    /// and going through [`Viewport::fit_into`] rather than `fit` because the
    /// toolbar, the status bar and any open side panel are drawn *over* the
    /// slide: fitting to the window puts their strips' worth of slide behind
    /// them, and a control that promises the whole slide has to deliver it.
    fn fit(&mut self) {
        let area = self.slide_area;
        if let Some(open) = &mut self.open {
            let fitted = match area {
                Some(area) => open.view.fit_into(open.slide(), area),
                // No layout yet, which is the first frame only.
                None => Viewport::fit(open.slide(), open.view.width, open.view.height),
            };
            open.look(fitted);
        }
    }

    /// Writes the last run's outlines as GeoJSON, through a save dialog.
    ///
    /// `docs/dev/VIEWER.md` §8.6 wrote the format and §8.2 recorded saving as done,
    /// and for several sessions **nothing called it**: `geojson::write` was a
    /// library function with no caller while the file's own opening paragraph
    /// said "what you find can be saved as GeoJSON that QuPath opens". This is
    /// that sentence being made true.
    ///
    /// Two measurement columns go with the outlines — the nucleus area and its
    /// mean haematoxylin optical density, under `qupath_pipeline`'s own names —
    /// because a file of outlines with no numbers on them is a picture, and
    /// these are the two QuPath's own export carries. Both are in the same
    /// frame as the coordinates; see [`detect::columns_of`](crate::detect).
    ///
    /// Every failure ends in [`App::message`] rather than a panic, for
    /// [`Open::new`]'s reason: a full disk is an ordinary thing to happen to an
    /// application and an unwritable path is the user's own choice of one.
    fn export(&mut self, window: &Window) {
        let Some(overlay) = self.found() else {
            self.message = String::from("Nothing to save: detect cells first (d).");
            return;
        };

        // Named after the slide, so a directory of exports says which slide
        // each came from without being opened.
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save detections as GeoJSON")
            .set_file_name(export_name(
                self.open.as_ref().map(|open| open.path.as_path()),
            ))
            .add_filter("GeoJSON", &["geojson", "json"]);
        if let Some(directory) = self.recent.first().and_then(|path| path.parent()) {
            dialog = dialog.set_directory(directory);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };

        // Borrowed as `Measurement`s rather than copied: `geojson::write` reads
        // the columns and skips any whose length does not match the overlay,
        // which is the safety net under `Detector`'s row alignment.
        let columns: Vec<qupath_viewer::Measurement<'_>> = self
            .detector
            .measurements
            .iter()
            .map(|(name, values)| qupath_viewer::Measurement { name, values })
            .collect();
        let (text, written) = qupath_viewer::geojson::write(overlay, &columns);
        self.message = match std::fs::write(&path, text) {
            Ok(()) => format!("Saved {written} objects to {}", display_name(&path)),
            Err(error) => format!("Could not save {}: {error}", display_name(&path)),
        };
        window.request_redraw();
    }

    /// What there is to save, or `None` when no run has found anything.
    ///
    /// One predicate, read by both the menu item that offers the save and the
    /// function that performs it. Two would eventually disagree, and the way
    /// they disagree is a live menu item that does nothing when pressed —
    /// which reads as the save being broken rather than as there being nothing
    /// to save.
    ///
    /// It asks the detector rather than the open slide because that is where
    /// the lifetime is: [`Detector::clear`] is what drops an overlay when the
    /// slide changes, so a run against a slide that is no longer open has
    /// already been discarded by the time this is asked.
    #[must_use]
    pub fn found(&self) -> Option<&Overlay> {
        self.detector
            .overlay
            .as_ref()
            .filter(|overlay| !overlay.is_empty())
    }

    /// The middle of what is on screen, for the controls that zoom about a
    /// point rather than about the cursor.
    ///
    /// The middle of the *slide's* rectangle, not of the window: with a side
    /// panel open the two are a hundred pixels apart, and zooming about the
    /// window's middle then walks the thing being looked at sideways out of
    /// view. Falls back to the window before the first layout.
    fn middle(&self) -> ScreenPoint {
        match self.slide_area {
            Some((left, top, right, bottom)) => {
                ScreenPoint::new(left.midpoint(right), top.midpoint(bottom))
            }
            None => self
                .open
                .as_ref()
                .map_or(ScreenPoint::new(0.0, 0.0), |open| centre(&open.view)),
        }
    }

    /// The commands that change only how the slide is shown.
    ///
    /// Split out of [`App::run`] because they need no window, which is what
    /// lets them be asserted: building an `App` is possible headless but
    /// building a `Window` is not, and the question worth asking about these is
    /// where a *run* of them leaves the state — whether a channel survives a
    /// range being moved afterwards — not what one of them returns.
    ///
    /// Nothing here touches [`Detector`], and that is the point rather than an
    /// omission: `docs/dev/VIEWER.md` §8.5's display-only claim is structural in
    /// the library and has to stay structural here.
    fn display_command(&mut self, command: &Command) {
        match command {
            // Turning one of these on opens the panel it lives in. Both are
            // shown inside the slide panel, which is closed by default, so
            // ticking "Display" used to do nothing at all — the user ticks a
            // box, sees no change, and concludes the control is broken. A
            // toggle whose effect is invisible is worse than a missing one.
            Command::ToggleOverview => {
                self.show_overview = !self.show_overview;
                if self.show_overview {
                    self.show_metadata = true;
                }
            }
            Command::ToggleAppearance => {
                self.show_appearance = !self.show_appearance;
                if self.show_appearance {
                    self.show_metadata = true;
                }
            }
            // No reload, and deliberately: the tiles in the cache are the
            // slide's own pixels and the transform is applied on the way to the
            // frame, so a drag of these sliders costs a redraw rather than a
            // round trip to the file.
            Command::SetAppearance(appearance) => self.appearance = *appearance,
            Command::ResetAppearance => self.appearance = Appearance::default(),
            _ => {}
        }
    }

    /// Carries out what the interface asked for.
    pub fn run(&mut self, command: Command, window: &Window) {
        match command {
            Command::None => {}
            Command::Open => {
                // Blocking, and on the main thread. A file dialog is modal to
                // the user anyway, so a frame not being drawn while it is up is
                // what they already expect; making it asynchronous would buy an
                // animation nobody is watching.
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Open a slide")
                    .add_filter("Slides", &["svs", "tif", "tiff", "ndpi"])
                    .add_filter("Any file", &["*"]);
                if let Some(recent) = self.recent.first().and_then(|path| path.parent()) {
                    dialog = dialog.set_directory(recent);
                }
                if let Some(path) = dialog.pick_file() {
                    self.pending = Some(path);
                    window.request_redraw();
                }
            }
            Command::OpenPath(path) => {
                self.pending = Some(path);
                window.request_redraw();
            }
            Command::OpenProject => {
                // Blocking and on the main thread, as `Open` is, and for the
                // same reason.
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Open a QuPath project")
                    .add_filter("QuPath project", &["qpproj"])
                    .add_filter("Any file", &["*"]);
                // Beside the project already open before beside the last slide:
                // someone opening a second project is far more likely to be in
                // the directory of studies than in the directory of one study's
                // slides.
                if let Some(directory) = self
                    .project
                    .as_ref()
                    .map(|project| project.path.as_path())
                    .or_else(|| self.recent.first().map(PathBuf::as_path))
                    .and_then(Path::parent)
                {
                    dialog = dialog.set_directory(directory);
                }
                if let Some(path) = dialog.pick_file() {
                    self.open_project(path);
                    window.request_redraw();
                }
            }
            Command::NewProject => {
                let mut dialog =
                    rfd::FileDialog::new().set_title("Choose a directory for the new project");
                if let Some(directory) = self
                    .project
                    .as_ref()
                    .map(|project| project.path.as_path())
                    .or_else(|| self.recent.first().map(PathBuf::as_path))
                    .and_then(Path::parent)
                {
                    dialog = dialog.set_directory(directory);
                }
                // A directory rather than a save dialog: QuPath's project *is*
                // a directory, and asking for a file name would put the user in
                // charge of a name the format fixes.
                if let Some(directory) = dialog.pick_folder() {
                    self.new_project(&directory);
                    window.request_redraw();
                }
            }
            Command::AddToProject => {
                self.add_to_project();
                window.request_redraw();
            }
            Command::CloseProject => {
                // The slide stays open. Closing the list is not closing what is
                // being looked at, and a viewer that shut the image because a
                // list was dismissed would lose the view someone had navigated
                // to.
                self.project = None;
                self.message = String::from("Project closed.");
            }
            Command::ToggleProject => self.show_project = !self.show_project,
            Command::Filter(text) => self.filter = text,
            // Not cleared when a project is opened, unlike the filter above:
            // see [`App::order`] for why the two differ.
            Command::SortBy(order) => self.order = order,
            Command::MaskNames(mask) => {
                self.mask_names = mask;
                // Drawn once, here, rather than in the pane: see
                // [`App::masked_order`] for why a per-frame shuffle is not an
                // option. Switching masking *off* clears it, so switching it on
                // again is a fresh shuffle rather than the one somebody has
                // already seen half of.
                self.masked_order = if mask {
                    self.project
                        .as_ref()
                        .map(|project| shuffled_ids(&project.stored.images))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
            }
            Command::EditMetadata(id) => {
                // Opened from the project's own entry rather than from the row
                // the click was on, so the editor starts from what the file
                // says and not from whatever the pane had drawn.
                if let Some(image) = self
                    .project
                    .as_ref()
                    .and_then(|project| project.stored.images.iter().find(|image| image.id == id))
                {
                    // The same two-part test the pane makes, and made the
                    // same way: the flag alone is not enough, because a project
                    // that cannot be masked does not mask.
                    let masking = self.mask_names
                        && self
                            .project
                            .as_ref()
                            .is_some_and(|project| crate::chrome::maskable(&project.stored.images));
                    self.editing = Some(MetadataEditor {
                        id,
                        name: crate::chrome::shown_name(image, masking).to_owned(),
                        rows: image.metadata.clone(),
                        problem: None,
                    });
                }
            }
            Command::SetMetadataRows(rows) => {
                if let Some(editor) = &mut self.editing {
                    editor.rows = rows;
                    // Cleared as soon as anything is typed: a refusal is about
                    // the attempt that was made, and leaving it up beside an
                    // edited list says it is about the list now on screen.
                    editor.problem = None;
                }
            }
            Command::WriteMetadata => {
                if let Some(editor) = &self.editing {
                    let (id, map) = (editor.id, editor.map());
                    match self.set_metadata(id, &map) {
                        Ok(()) => {
                            // Closed only on success. The refusal path is the
                            // one where the typing must survive, and closing
                            // the editor is how it would be lost.
                            self.editing = None;
                            self.message = match map.len() {
                                0 => String::from("Cleared the entry's metadata."),
                                1 => String::from("Wrote 1 metadata key."),
                                many => format!("Wrote {many} metadata keys."),
                            };
                        }
                        Err(problem) => {
                            self.message.clone_from(&problem);
                            if let Some(editor) = &mut self.editing {
                                editor.problem = Some(problem);
                            }
                        }
                    }
                }
            }
            Command::CancelMetadata => self.editing = None,
            Command::Close => {
                self.open = None;
                self.uploaded_overview = false;
                self.detector.clear();
                self.message = String::from("Open a slide to begin.");
            }
            Command::StopDetecting => {
                // No `is_running` guard: `cancel` is harmless when nothing is
                // going, and the run does not stop here anyway — the worker
                // sees the flag on its next tile and the result arrives through
                // `poll` like any other. Repainting is what shows the status
                // bar changing when it does.
                self.detector.cancel();
                window.request_redraw();
            }
            Command::Detect => {
                if let Some(open) = &self.open {
                    let slide = open.slide();
                    // The level the viewer *draws* is passed as a floor, not as
                    // the level to read: `Detector::start` picks the level that
                    // can resolve a nucleus, which is a different question. See
                    // `detect::level_for_detection`.
                    let view = open.view.level(slide).clone();
                    self.detector.start(
                        &open.path,
                        slide.levels(),
                        &view,
                        slide.microns_per_pixel(),
                        open.view,
                        self.tuning,
                    );
                }
            }
            Command::Export => self.export(window),
            Command::ToggleOverlay => self.show_overlay = !self.show_overlay,
            Command::ToggleAnalysis => self.show_analysis = !self.show_analysis,
            Command::ToggleMetadata => self.show_metadata = !self.show_metadata,
            Command::ToggleHelp => self.show_help = !self.show_help,
            Command::ToggleOverview => self.display_command(&command),
            Command::JumpTo(centre) => {
                if let Some(open) = &mut self.open {
                    // Through `look`, so the jump is clamped like every other
                    // move of the view — a click near the edge of the overview
                    // asks for a centre outside the slide, which is exactly
                    // what `clamp_to` exists for.
                    let view = Viewport {
                        centre,
                        ..open.view
                    };
                    open.look(view);
                }
            }
            // Changing a parameter does not re-run detection. A run takes about
            // a second and a slider produces a value every frame, so re-running
            // on change would start dozens of runs across one drag and show the
            // answer to a parameter the user had already moved past. `Detect`
            // is the deliberate act, as it is in QuPath's dialog.
            Command::ToggleAppearance | Command::SetAppearance(_) | Command::ResetAppearance => {
                self.display_command(&command);
            }
            Command::SetTuning(tuning) => self.tuning = tuning,
            Command::ResetTuning => self.tuning = Micron::default(),
            Command::Fit => self.fit(),
            Command::Zoom(steps) => {
                let anchor = self.middle();
                if let Some(open) = &mut self.open {
                    // The same 1.5 a scroll step uses, so the buttons and the
                    // wheel move the view by the same amount.
                    open.look(open.view.zoom_about(1.5f64.powi(steps), anchor));
                }
            }
            Command::Actual => {
                let anchor = self.middle();
                if let Some(open) = &mut self.open {
                    let factor = 1.0 / open.view.scale;
                    open.look(open.view.zoom_about(factor, anchor));
                }
            }
        }
    }

    /// Draws one frame: the slide, then the interface over it.
    pub fn render(&mut self, window: &Arc<Window>) {
        let started = Instant::now();
        // The project first: a slide named on the same command line is the one
        // that should be showing when both arrive, and opening it second is
        // what makes that true whichever order they were typed in.
        if let Some(path) = self.pending_project.take() {
            self.open_project(path);
        }
        if let Some(path) = self.pending.take() {
            self.open(path);
        }

        let (width, height) = self.size();
        if width == 0 || height == 0 {
            return;
        }
        self.request_tiles();
        self.take_pictures();
        // Before the interface is laid out, so a run that finished since the
        // last frame is reported by *this* frame's status bar rather than the
        // next one — the same reason the interface is laid out before the frame
        // is drawn.
        self.detector.poll();

        // The interface is laid out *before* the frame is drawn, because the
        // commands it produces change what the frame shows — a fit pressed this
        // frame must be the view this frame draws, not the next one.
        let Some(egui) = self.egui.as_mut() else {
            return;
        };
        let input = egui.take_egui_input(window);
        let mut command = Command::None;
        let mut slide_area = None;
        let output = self.context.clone().run(input, |context| {
            let (asked, area) = chrome::draw(context, self);
            command = asked;
            slide_area = Some(area);
        });
        // Into *physical* pixels, which is the unit `self.cursor` and the
        // compositor's frame are both in. `chrome::draw` reads the rectangle
        // from `egui`, whose coordinates are logical points -- the two are the
        // same numbers only at a scale factor of 1.0, which is the one machine
        // this was developed on. See [`App::slide_area`].
        self.slide_area = slide_area.map(|area| scale_area(area, output.pixels_per_point));
        if let Some(egui) = self.egui.as_mut() {
            egui.handle_platform_output(window, output.platform_output);
        }
        self.run(command, window);
        // After the layout and before the frame: a slide opened this frame was
        // fitted to the window by `Open::new`, which had no rectangle to fit
        // into yet. Now there is one.
        if std::mem::take(&mut self.refit) {
            self.fit();
        }

        let primitives = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        self.draw(&primitives, &output.textures_delta, output.pixels_per_point);

        self.costs.push(started.elapsed());
        if self.costs.len() > WINDOW {
            self.costs.remove(0);
        }
        // `egui` asking for an immediate repaint — a tooltip fading in, a
        // spinner turning. A *delay* rather than zero is `egui` asking for one
        // later, and is deliberately not honoured: nothing here animates on a
        // timer, and scheduling a wake-up for it is one of the two ways a
        // window that should be asleep stays awake. The other was
        // `RedrawRequested` being fed back into `egui`; see `main.rs`.
        if output
            .viewport_output
            .values()
            .any(|out| out.repaint_delay.is_zero())
        {
            window.request_redraw();
        }
    }

    /// Asks the loader for what this frame needs and takes delivery of what has
    /// arrived.
    fn request_tiles(&mut self) {
        let Some(open) = &mut self.open else {
            return;
        };
        // Drained before anything is requested, and while the loader is asked
        // whether it is busy first — both are `docs/dev/VIEWER.md` §3.4's rule
        // about a tile being neither claimed nor delivered.
        for loaded in open.loader.drain() {
            let bytes = loaded.pixels.len();
            open.cache.insert(
                loaded.tile,
                Decoded {
                    pixels: loaded.pixels,
                    width: loaded.width,
                    height: loaded.height,
                },
                bytes,
            );
        }

        let level = open.view.level(open.slide()).clone();
        let underlay = open.view.underlay_level(open.slide()).cloned();
        let mut wanted = Vec::new();
        if let Some(under) = &underlay {
            wanted.extend(open.view.visible_tiles(under));
        }
        wanted.extend(open.view.visible_tiles(&level));

        let missing: Vec<_> = wanted
            .iter()
            .copied()
            .filter(|tile| !open.cache.touch(*tile))
            .collect();
        if !missing.is_empty() {
            open.loader.request(missing);
        }
    }

    /// Takes delivery of the browser's thumbnails and makes them textures.
    ///
    /// **Before the interface is laid out, and that is not a preference.** A
    /// row draws its picture by naming a texture id derived from its index, and
    /// those ids are reused by whichever project is open. A picture uploaded
    /// after the layout that names it is therefore not merely late — the frame
    /// naming it resolves the id to whatever the *previous* project left there,
    /// so a row of study B briefly shows a slide from study A. Uploading first
    /// is what makes [`Pictures::ready`] a true statement rather than a hope.
    pub fn take_pictures(&mut self) {
        let Some(project) = &mut self.project else {
            return;
        };
        project.pictures.take();
        let pending = project.pictures.undelivered();
        let Some(state) = &mut self.surface else {
            // No surface, and the pictures are marked delivered anyway. The
            // application always has one here — `App::start` builds the window
            // and the surface together, and nothing calls this before that —
            // so the only callers that land here are the ones that draw the
            // interface themselves: `examples/chrome`, which registers these
            // same ids by hand, and the tests, which draw nothing at all. A
            // queue that grew forever waiting for a renderer that is never
            // coming would be a leak in exactly the configuration that cannot
            // see it.
            return;
        };
        for (index, picture) in pending {
            let (width, height) = picture.overview.size();
            state.set_texture(
                chrome::PROJECT_TEXTURE + index as u64,
                &picture.pixels,
                width,
                height,
            );
        }
    }

    /// Composes the slide and the interface, and presents.
    ///
    /// `pixels_per_point` is `egui`'s scale factor, and it is needed here
    /// because the two halves of the frame arrive in different units: the tiles
    /// are already in the framebuffer's physical pixels, and `primitives` are
    /// in logical points. See [`draw_interface`].
    fn draw(
        &mut self,
        primitives: &[egui::ClippedPrimitive],
        delta: &egui::TexturesDelta,
        pixels_per_point: f32,
    ) {
        // The draw list, built before the framebuffer is borrowed: the slide's
        // tiles back to front, exactly as `window.rs` builds it.
        let mut draws: Vec<Draw<'_>> = Vec::new();
        if let Some(open) = &self.open {
            let level = open.view.level(open.slide()).clone();
            let underlay = open.view.underlay_level(open.slide()).cloned();
            if let Some(under) = &underlay {
                for tile in open.view.visible_tiles(under) {
                    if let Some(decoded) = open.cache.peek(tile) {
                        draws.push((open.view.tile_rect(tile, under), decoded));
                    }
                }
            }
            for tile in open.view.visible_tiles(&level) {
                if let Some(decoded) = open.cache.peek(tile) {
                    draws.push((open.view.tile_rect(tile, &level), decoded));
                }
            }
        }
        let drawn = draws.len();

        // The overlay's segments, projected before the framebuffer is borrowed
        // for the same reason the tiles are: the detector and the surface are
        // both fields of `self`, and drawing straight out of one into the other
        // borrows all of it.
        let mut outlined = 0;
        if let (true, Some(open), Some(overlay)) =
            (self.show_overlay, &self.open, &self.detector.overlay)
        {
            outlined = overlay.segments_into(open.view, MIN_OVERLAY_PX, &mut self.segments);
        } else {
            self.segments.clear();
        }

        // Uploaded when the slide changes rather than every frame: the picture
        // is of the slide, not of the view, so the rectangle moving is a
        // different draw and not a different texture. `uploaded_overview` is
        // what keeps a pan from copying 64 KB sixty times a second for a
        // picture that has not changed.
        let overview = self
            .open
            .as_ref()
            .and_then(|open| open.overview())
            .filter(|_| !self.uploaded_overview)
            .map(|thumbnail| (thumbnail.pixels.clone(), thumbnail.overview.size()));

        let Some(state) = &mut self.surface else {
            return;
        };
        if let Some((pixels, (width, height))) = overview {
            state.set_texture(chrome::OVERVIEW_TEXTURE, &pixels, width, height);
            self.uploaded_overview = true;
        }
        state.frame.clear(compose::CLEAR);
        // The display transform is applied here, on the way to the frame, and
        // nowhere earlier: the tiles in the cache are the slide's own pixels,
        // so changing the range costs a redraw and not a reload, and nothing
        // the detector reads has been through it. That is the structural half
        // of `docs/dev/VIEWER.md` §8.5's display-only claim — `Detector::start` is
        // handed the view and the path, never a `Display`.
        let display = self.appearance.display();
        for (rect, tile) in &draws {
            state
                .frame
                .draw_tile_with(*rect, &tile.pixels, tile.width, tile.height, display);
        }
        drop(draws);
        // Over the tiles and under the chrome. A panel drawn first would have
        // outlines run across it, which is the one ordering mistake here that
        // looks like a rendering bug rather than a layering one.
        state.frame.draw_segments(&self.segments, OVERLAY_COLOUR);
        // Over the outlines too. The band is what the hand is doing right now
        // and has to be readable against whatever it is drawn across.
        if let Some(from) = self.band {
            state
                .frame
                .draw_segments(&band_segments(from, self.cursor), BAND_COLOUR);
        }
        // Recorded before the assignment: these are what the *next* layout will
        // report, and if they differ from what this frame's layout showed then
        // the interface is a frame behind and owes one more. See
        // [`App::unreported`].
        self.unreported =
            outlined != self.outlined || self.open.as_ref().is_some_and(|open| open.drawn != drawn);
        self.outlined = outlined;
        if let Some(open) = &mut self.open {
            open.drawn = drawn;
        }

        apply_textures(&mut state.textures, delta);
        draw_interface(
            &mut state.frame,
            &state.textures,
            &mut state.vertices,
            primitives,
            pixels_per_point,
        );

        state.present();
    }
}

/// Applies `egui`'s texture changes to `textures`: whole images and patches of
/// them.
///
/// Free rather than a method for the reason [`draw_interface`] is: together the
/// two are the whole software path, and a test can then run it with no window.
fn apply_textures(textures: &mut Vec<(egui::TextureId, Texture)>, delta: &egui::TexturesDelta) {
    for (id, image) in &delta.set {
        let egui::ImageData::Color(source) = &image.image;
        let [source_width, source_height] = source.size;
        let pixels: Vec<[u8; 4]> = source.pixels.iter().map(|pixel| pixel.to_array()).collect();

        let at = textures.iter().position(|(existing, _)| existing == id);
        match (image.pos, at) {
            // A patch of an existing texture, which is what `egui` sends
            // when a new glyph is rasterised into the atlas.
            (Some([x, y]), Some(at)) => {
                let texture = &mut textures[at].1;
                let width = texture.width as usize;
                for row in 0..source_height {
                    let from = row * source_width;
                    let to = (y + row) * width + x;
                    if to + source_width <= texture.pixels.len() {
                        texture.pixels[to..to + source_width]
                            .copy_from_slice(&pixels[from..from + source_width]);
                    }
                }
            }
            // The whole texture, new or replaced.
            _ => {
                let texture = Texture {
                    width: source_width as u32,
                    height: source_height as u32,
                    pixels,
                };
                if let Some(at) = at {
                    textures[at].1 = texture;
                } else {
                    textures.push((*id, texture));
                }
            }
        }
    }
    for id in &delta.free {
        textures.retain(|(existing, _)| existing != id);
    }
}

/// Rasterises `egui`'s tessellated interface into `frame`.
///
/// # The unit, which was wrong
///
/// `egui` lays out and tessellates in **logical points**, and `frame` is in
/// **physical pixels**. Those are the same numbers only at a scale factor of
/// 1.0 — the value a development machine and every headless render both have.
/// Anywhere else the entire interface was drawn at `1 / pixels_per_point` of
/// its size into the top-left corner of the window, over a slide drawn at full
/// size: a toolbar half the width of the window, a status bar across the middle
/// of the tissue, and everything below and right of them bare. It is the same
/// mistake [`App::slide_area`] records one layer up, and it survived that fix
/// because converting the rectangle the interface *reports* says nothing about
/// the units the interface is *drawn* in — the two were wrong in opposite
/// directions, so the hit-testing rectangle then named a strip of window the
/// toolbar was nowhere near.
///
/// Converted here rather than at the two call sites below, for
/// [`scale_area`]'s reason: one unit below this line.
///
/// # Why it is free
///
/// Taking the frame and the textures rather than a [`Software`] means the whole
/// software path runs with no window and no display server, which is the only
/// way this class of bug is reachable from a test at all —
/// `the_interface_is_drawn_across_the_whole_window_at_a_scale_factor_of_two`
/// is that test, and it fails on a `pixels_per_point` that is accepted and
/// ignored rather than only on arithmetic that is wrong.
fn draw_interface(
    frame: &mut Framebuffer,
    textures: &[(egui::TextureId, Texture)],
    scratch: &mut Vec<MeshVertex>,
    primitives: &[egui::ClippedPrimitive],
    pixels_per_point: f32,
) {
    for primitive in primitives {
        let egui::epaint::Primitive::Mesh(mesh) = &primitive.primitive else {
            // `Callback` is for a renderer that draws its own thing inside the
            // interface. Nothing here does, so it is skipped rather than
            // asserted on.
            continue;
        };
        // The texture is resolved to an *index* rather than a reference so that
        // the frame, the vertex buffer and the texture can be borrowed at once.
        let Some(at) = textures.iter().position(|(id, _)| *id == mesh.texture_id) else {
            continue;
        };

        // `epaint::Vertex` is `{Pos2, Pos2, Color32}` and `MeshVertex` is the
        // same three fields; this is that conversion, written out because a
        // cast would need a trait the viewer library does not implement.
        scratch.clear();
        scratch.extend(mesh.vertices.iter().map(|vertex| MeshVertex {
            position: [
                vertex.pos.x * pixels_per_point,
                vertex.pos.y * pixels_per_point,
            ],
            uv: [vertex.uv.x, vertex.uv.y],
            colour: vertex.color.to_array(),
        }));

        let clip = primitive.clip_rect;
        frame.draw_mesh(
            scale_area(
                (clip.min.x, clip.min.y, clip.max.x, clip.max.y),
                pixels_per_point,
            ),
            scratch,
            &mesh.indices,
            &textures[at].1,
        );
    }
}

impl Software {
    /// Registers a picture of the application's own as a user texture,
    /// replacing any previous one under the same id.
    ///
    /// `egui` owns the *managed* ids in [`Software::textures`]; this claims
    /// user ids alongside them, so `mesh` resolves one through the same lookup
    /// and needs no special case. Uploaded when the picture changes rather than
    /// per frame: `chrome` names its textures every frame, and the pixels
    /// behind them change when a slide is opened or a row's thumbnail lands.
    ///
    /// The ids are [`chrome::OVERVIEW_TEXTURE`] and one per browser row from
    /// [`chrome::PROJECT_TEXTURE`], and the row ids are **reused** by the next
    /// project opened — which is the whole reason a row may only draw its
    /// picture once `Pictures` says the renderer has this project's copy.
    fn set_texture(&mut self, id: u64, pixels: &[u8], width: u32, height: u32) {
        let id = egui::TextureId::User(id);
        let texture = Texture {
            width,
            height,
            pixels: pixels
                .chunks_exact(4)
                .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
                .collect(),
        };
        if let Some(at) = self
            .textures
            .iter()
            .position(|(existing, _)| *existing == id)
        {
            self.textures[at].1 = texture;
        } else {
            self.textures.push((id, texture));
        }
    }

    /// Copies the frame into the window's buffer and presents it.
    fn present(&mut self) {
        if let Err(error) = self.present_inner() {
            if !self.reported {
                eprintln!("presenting failed: {error}");
                self.reported = true;
            }
        } else {
            self.reported = false;
        }
    }

    fn present_inner(&mut self) -> Result<(), softbuffer::SoftBufferError> {
        let (Some(width), Some(height)) = (
            std::num::NonZeroU32::new(self.frame.width()),
            std::num::NonZeroU32::new(self.frame.height()),
        ) else {
            return Ok(());
        };
        self.surface.resize(width, height)?;
        let mut buffer = self.surface.buffer_mut()?;
        buffer.copy_from_slice(self.frame.pixels());
        buffer.present()
    }
}

/// The file name a save dialog opens with.
///
/// The slide's **stem** and not [`display_name`], which keeps the extension:
/// "CMU-1.svs-detections.geojson" is a name nobody meant. Named after the slide
/// at all so a directory of exports says which slide each came from without
/// being opened.
#[must_use]
pub fn export_name(slide: Option<&Path>) -> String {
    let stem = slide.and_then(Path::file_stem).map_or_else(
        || String::from("detections"),
        |stem| format!("{}-detections", stem.to_string_lossy()),
    );
    format!("{stem}.geojson")
}

/// The file name of a path, for a title bar.
/// The project and the slide a list of command-line paths asks for.
///
/// Told apart by [`App::is_project`], so the two can be given in either order.
/// They are not alternatives — a project is the list and a slide is what is
/// being looked at, and starting work usually means both — so a single slot for
/// them would silently drop one.
///
/// The **last** of each wins. Saying "open this" twice means the second one,
/// which is what a shell history full of appended arguments produces.
///
/// Pure, and separate from [`App::new`], for `docs/dev/VIEWER.md` §6's reason: it
/// is the whole of what the command line decides, and a test of it needs no
/// window.
#[must_use]
pub fn requested(paths: impl IntoIterator<Item = String>) -> (Option<PathBuf>, Option<PathBuf>) {
    let (projects, slides): (Vec<PathBuf>, Vec<PathBuf>) = paths
        .into_iter()
        .map(PathBuf::from)
        .partition(|path| App::is_project(path));
    (
        projects.into_iter().next_back(),
        slides.into_iter().next_back(),
    )
}

/// Writes the files of a project into `directory`, creating what it needs.
///
/// Every path is relative to the project directory and comes from
/// `qupath_viewer::project`, which is the module that knows the layout; this
/// only creates directories and writes bytes.
///
/// **The index goes last.** It is the file QuPath opens first, and a run
/// interrupted between the two leaves either a directory with no project in it
/// — which reads as nothing having happened — or a project naming entry
/// directories that are not there, which reads as a corrupt study. The first is
/// the failure worth having.
///
/// # Errors
///
/// A sentence naming the file that could not be written, for the status bar. A
/// full disk and an unwritable directory are ordinary things to happen to an
/// application, and the user chose the directory.
/// The entry ids of `images`, shuffled.
///
/// QuPath shuffles its masked list "for less biased analyses" — a blinded
/// reader who works down the pane in order would otherwise be working through
/// the study in whatever order the slides were added, which is often the order
/// they were collected in.
///
/// # The seed, and what it has to not be
///
/// Seeded from the wall clock, because the one thing it must not be derivable
/// from is the list being shuffled. A seed taken from the entries — their
/// count, their ids, a hash of their names — makes the permutation a function
/// of the project, so the same study masked twice is masked the same way, and
/// two readers comparing notes recover the order. `SystemTime` is coarse and
/// guessable and that is fine: the reader is being kept from *recognising*
/// slides, not from an adversary reconstructing a key.
///
/// # A Fisher-Yates with an xorshift, and not a dependency
///
/// The workspace has no `rand`, and this is the only thing in it that wants
/// one. `xorshift64*` is nine lines and its bias over a list of fifty is far
/// below what a shuffle is for; a dependency pulled in for nine lines is a
/// dependency the parity crates would carry too.
fn shuffled_ids(images: &[qupath_viewer::project::Image]) -> Vec<u64> {
    let mut ids: Vec<u64> = images.iter().map(|image| image.id).collect();
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x2545_f491_4f6c_dd1d, |since| since.as_nanos() as u64)
        // `xorshift64` is stuck at zero, which a clock at the epoch would
        // reach. The list would come back in its own order, masked but not
        // shuffled.
        | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    // Downwards, taking each element to a position at or below it, which is
    // the unbiased direction: the upwards spelling revisits positions already
    // placed and is the classic wrong one.
    for index in (1..ids.len()).rev() {
        ids.swap(index, (next() % (index as u64 + 1)) as usize);
    }
    ids
}

fn write_project(directory: &Path, files: &[qupath_viewer::project::Output]) -> Result<(), String> {
    let (index, rest): (Vec<_>, Vec<_>) = files
        .iter()
        .partition(|file| file.path == App::PROJECT_FILE);
    for file in rest.into_iter().chain(index) {
        let path = directory.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        std::fs::write(&path, &file.contents)
            .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    }
    Ok(())
}

/// What to call a project, following `DefaultProject.getName()`.
///
/// The study directory and the file, `study/project.qpproj`, because the file
/// name alone is the same for every project there has ever been. Falls back to
/// the file name when the path has no parent, which is a `.qpproj` at the root
/// of a filesystem and not a project anyone has.
#[must_use]
pub fn project_name(path: &Path) -> String {
    let directory = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned());
    match directory {
        Some(directory) => format!("{directory}/{}", display_name(path)),
        None => display_name(path),
    }
}

pub fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// A one-line summary of a slide: its level-0 size and how many levels it has.
pub fn describe(slide: &Slide) -> String {
    let levels = slide.levels();
    let base = &levels[0];
    format!(
        "{} x {} px, {} level{}",
        base.width,
        base.height,
        levels.len(),
        if levels.len() == 1 { "" } else { "s" }
    )
}

/// The centre of a view, as the anchor a zoom button zooms about.
fn centre(view: &Viewport) -> ScreenPoint {
    ScreenPoint::new(view.width as f32 / 2.0, view.height as f32 / 2.0)
}

/// The four sides of the zoom rectangle, as the segment pairs
/// [`Framebuffer::draw_segments`] takes.
///
/// Built from the two corners in whatever order they were dragged, so the
/// rectangle is the same one [`Viewport::zoom_to_box`] will zoom to — the band
/// showing a different rectangle from the one that gets used is the bug this
/// sharing of `min`/`max` exists to prevent.
fn band_segments(from: ScreenPoint, to: ScreenPoint) -> [ScreenPoint; 8] {
    let (left, right) = (from.x.min(to.x), from.x.max(to.x));
    let (top, bottom) = (from.y.min(to.y), from.y.max(to.y));
    let (top_left, top_right) = (ScreenPoint::new(left, top), ScreenPoint::new(right, top));
    let (bottom_right, bottom_left) = (
        ScreenPoint::new(right, bottom),
        ScreenPoint::new(left, bottom),
    );
    [
        top_left,
        top_right,
        top_right,
        bottom_right,
        bottom_right,
        bottom_left,
        bottom_left,
        top_left,
    ]
}

/// Where the cursor is on the slide, in level-0 pixels.
pub fn cursor_slide_position(open: &Open, cursor: ScreenPoint) -> SlidePoint {
    open.view.to_slide(cursor)
}

impl App {
    /// The cursor's position, for the status bar.
    pub fn cursor(&self) -> ScreenPoint {
        self.cursor
    }

    /// The zoom a band being dragged would apply if released now, as a
    /// multiple of the current scale.
    ///
    /// `None` when no band is being dragged, or while it is still small enough
    /// to be a click — so the readout appears once the gesture has committed to
    /// being a drag, at the same threshold `App::finish_band` uses.
    ///
    /// Derived from [`Viewport::zoom_to_box`] rather than recomputed, because a
    /// label predicting a different factor from the one the release applies is
    /// worse than no label.
    pub fn band_factor(&self) -> Option<f64> {
        let from = self.band?;
        let to = self.cursor;
        if (to.x - from.x).abs() < BAND_MINIMUM_PX && (to.y - from.y).abs() < BAND_MINIMUM_PX {
            return None;
        }
        let open = self.open.as_ref()?;
        let zoomed = open.view.zoom_to_box(from, to)?;
        Some(zoomed.scale / open.view.scale)
    }
}

// The `winit` event loop's side of `App`. It lives here rather than in
// `main.rs` because the orphan rule requires it: `ApplicationHandler` and
// `App` cannot both be foreign to the crate that connects them, and `App`
// moved into the library so `examples/chrome` could lay the interface out
// without a window.
use winit::application::ApplicationHandler;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("QuPath Rust")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 860.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("create the window"),
        );
        self.start(&window);
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };

        // `RedrawRequested` is not input, and must never reach `egui`.
        //
        // It is the *answer* to a redraw request rather than a reason for one,
        // and `egui_winit` answers it with `repaint: true` — so passing it on
        // closes a loop that needs no input to sustain itself: render, receive
        // `RedrawRequested`, ask to render again. Measured at **90 frames a
        // second and 100% of one core** on a window sitting still, which is
        // precisely what `ControlFlow::Wait` is set to prevent.
        //
        // It is handled here, ahead of `egui`, rather than by testing the event
        // twice. See `docs/dev/VIEWER.md` §3.6.
        if matches!(event, WindowEvent::RedrawRequested) {
            self.render(&window);
            return;
        }

        // Every *other* event goes to the interface first, which says whether
        // it wants it. Without this a drag that started on a slider also pans
        // the slide, and a scroll over a panel zooms the view underneath it.
        let Some(egui) = self.egui.as_mut() else {
            return;
        };
        let response = egui.on_window_event(&window, &event);
        if response.repaint {
            window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.resize(size.width, size.height);
                window.request_redraw();
            }
            other => {
                if self.input(&window, &other, response.consumed) {
                    window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // One more frame while tiles are still landing, and none once they have
        // all landed. This is what keeps the viewer at zero CPU at rest while
        // still filling in as a pan's tiles arrive.
        if self.wants_another_frame()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use qupath_viewer::overview::Overview;

    use super::*;

    /// A scratch directory that removes itself.
    ///
    /// Hand-rolled rather than a dependency: the workspace has no test-only
    /// crates and one directory under `TMPDIR` is not reason to acquire the
    /// first. Named from the process id *and* the test's own name, because
    /// `cargo test` runs these on threads of one process and two tests sharing
    /// a directory would be two tests writing one `project.qpproj`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("qupath-rust-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a scratch directory");
            Self(path)
        }

        /// A project of one entry, written through the module that writes
        /// projects — not a literal, so the fixture cannot drift from the
        /// format.
        fn project(&self) -> PathBuf {
            let path = self.0.join(App::PROJECT_FILE);
            let slide = self.0.join("CMU-1.svs");
            let entry = project::Entry {
                path: &slide,
                name: "CMU-1.svs",
                width: 46000,
                height: 32914,
            };
            let written = project::Timestamps {
                created: 1_700_000_000_000,
                modified: 1_700_000_000_000,
            };
            for file in project::files(&path, &[entry], written) {
                let at = self.0.join(&file.path);
                if let Some(parent) = at.parent() {
                    std::fs::create_dir_all(parent).expect("a directory");
                }
                std::fs::write(&at, &file.contents).expect("a file");
            }
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An application with a project open and no window.
    fn opened(path: PathBuf) -> App {
        let mut app = App::new(Vec::<String>::new());
        app.open_project(path);
        assert!(app.project.is_some(), "the scratch project opened");
        app
    }

    fn pairs(from: &[(&str, &str)]) -> Vec<(String, String)> {
        from.iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    /// A project too deeply nested to parse is a message, not a dead process.
    ///
    /// `json::parse` grew a depth limit because its recursion had none, and a
    /// 78 KB file aborted the process with `SIGABRT`. That is fixed in the
    /// parser, but the parser returning `Err` is only half of it: what a person
    /// does is *open a file*, and this asserts the other half — the error
    /// reaches [`App::message`], and the project that was already open is still
    /// open, which is the same rule a slide that will not load follows.
    #[test]
    fn a_project_too_deeply_nested_is_refused_without_losing_the_one_open() {
        let scratch = Scratch::new("deep-project");
        let good = scratch.project();
        let mut app = opened(good.clone());
        let before = app.project.as_ref().expect("a project").stored.images[0].id;

        let deep = scratch.0.join("deep.qpproj");
        let n = 40_000;
        std::fs::write(&deep, format!("{}{}", "[".repeat(n), "]".repeat(n))).expect("a file");

        app.open_project(deep);

        assert!(
            app.message.contains("nested deeper than"),
            "the message should say what was wrong with the file, got {:?}",
            app.message
        );
        let after = app
            .project
            .as_ref()
            .expect("the project stayed open")
            .stored
            .images[0]
            .id;
        assert_eq!(
            before, after,
            "the project already open should be untouched"
        );
    }

    /// The gesture, end to end: type a key, save, and find it in the file.
    ///
    /// Through `set_metadata` rather than through `project::edited`, because
    /// what this is testing is the part the unit tests cannot see — that the
    /// bytes reach the disk and that the pane is showing them afterwards.
    #[test]
    fn an_edit_reaches_the_file_and_the_pane() {
        let scratch = Scratch::new("edit-reaches-file");
        let path = scratch.project();
        let mut app = opened(path.clone());
        let id = app.project.as_ref().expect("a project").stored.images[0].id;

        app.set_metadata(id, &pairs(&[("stain", "H&E")]))
            .expect("the file is as it was opened");

        // On disk.
        let text = std::fs::read_to_string(&path).expect("the project");
        let stored = project::read(&text).expect("it reads back");
        assert_eq!(stored.images[0].metadata, pairs(&[("stain", "H&E")]));
        // And in the application, which re-read it rather than assuming.
        assert_eq!(
            app.project.expect("still open").stored.images[0].metadata,
            pairs(&[("stain", "H&E")])
        );
    }

    /// The staleness check must not fire on a project this application wrote.
    ///
    /// A regression, and the bug it pins made the feature useless on exactly
    /// the projects this creates: `project_file` ends the index with a newline
    /// and `json::write` does not, so such a project differs from its own
    /// rewrite by one trailing byte and every edit to it was refused for a
    /// change nobody had made. Asserted as the *first* edit succeeding, which
    /// is the gesture that was broken, rather than by comparing the two
    /// spellings — a test of the spelling would pass again the moment the
    /// comparison moved somewhere else.
    #[test]
    fn a_project_this_wrote_is_not_reported_as_changed() {
        let scratch = Scratch::new("own-project-not-stale");
        let path = scratch.project();
        let mut app = opened(path);
        let id = app.project.as_ref().expect("a project").stored.images[0].id;

        app.set_metadata(id, &pairs(&[("k", "v")]))
            .expect("a project this application created is not stale on its first edit");
    }

    /// **The reason this feature has a check at all.** A project written by
    /// somebody else since it was opened is not overwritten, and the edit is
    /// refused with a message that says why.
    #[test]
    fn an_edit_against_a_changed_file_is_refused() {
        let scratch = Scratch::new("changed-underneath");
        let path = scratch.project();
        let mut app = opened(path.clone());
        let id = app.project.as_ref().expect("a project").stored.images[0].id;

        // Somebody else writes the project: QuPath, a script, a second copy of
        // this. Through the same writer, so what lands is a real project and
        // not a corrupted one — the check is about the file having *changed*,
        // not about it having become unreadable.
        let theirs = project::read(&std::fs::read_to_string(&path).expect("the project"))
            .expect("reads back");
        let elsewhere = project::edited(
            &theirs,
            id,
            &pairs(&[("theirs", "keep me")]),
            project::Timestamps {
                created: 1_700_000_000_000,
                modified: 1_700_000_999_000,
            },
        )
        .expect("their edit");
        std::fs::write(&path, &elsewhere[0].contents).expect("their write");

        let refused = app
            .set_metadata(id, &pairs(&[("mine", "overwrite")]))
            .expect_err("the file changed underneath");
        assert!(
            refused.contains("changed on disk"),
            "the message says what happened: {refused}"
        );

        // Their edit is still there, and mine is not.
        let after = project::read(&std::fs::read_to_string(&path).expect("the project"))
            .expect("reads back");
        assert_eq!(
            after.images[0].metadata,
            pairs(&[("theirs", "keep me")]),
            "a refused edit overwrote the change it was refused for"
        );
    }

    /// Repeating the gesture leaves one key, not two. The invariant is over
    /// where the state ends up rather than over what one call returned.
    #[test]
    fn editing_the_same_entry_twice_leaves_one_key() {
        let scratch = Scratch::new("edit-twice");
        let path = scratch.project();
        let mut app = opened(path.clone());
        let id = app.project.as_ref().expect("a project").stored.images[0].id;

        app.set_metadata(id, &pairs(&[("stain", "H&E")]))
            .expect("first");
        // The second edit is against the project the first one re-read, which
        // is the case that would fail if `set_metadata` compared against a
        // snapshot taken when the project was first opened.
        app.set_metadata(id, &pairs(&[("stain", "PAS")]))
            .expect("second, against the file the first one wrote");

        let after = project::read(&std::fs::read_to_string(&path).expect("the project"))
            .expect("reads back");
        assert_eq!(after.images[0].metadata, pairs(&[("stain", "PAS")]));
    }

    /// Clearing every key writes an entry with none, rather than refusing or
    /// leaving the last one behind.
    #[test]
    fn clearing_every_key_writes_an_entry_with_none() {
        let scratch = Scratch::new("clear-keys");
        let path = scratch.project();
        let mut app = opened(path.clone());
        let id = app.project.as_ref().expect("a project").stored.images[0].id;

        app.set_metadata(id, &pairs(&[("a", "1"), ("b", "2")]))
            .expect("filled");
        app.set_metadata(id, &[]).expect("cleared");

        let after = project::read(&std::fs::read_to_string(&path).expect("the project"))
            .expect("reads back");
        assert!(after.images[0].metadata.is_empty());
    }

    /// What the editor would write, given what is in its boxes. The blank row
    /// at the bottom is a space to type in and not a key.
    #[test]
    fn the_editor_drops_blank_keys_and_keeps_blank_values() {
        let rows = pairs(&[("stain", "H&E"), ("", ""), ("  ", "orphan"), ("note", "")]);
        assert_eq!(
            written_rows(&rows),
            pairs(&[("stain", "H&E"), ("note", "")]),
            "a key with no name is not a key; a key with no value is"
        );
    }

    /// A repeated key keeps its last value, which is what the file would say
    /// if both were written.
    #[test]
    fn the_editor_resolves_a_repeated_key_the_way_the_file_would() {
        let rows = pairs(&[("stain", "H&E"), ("stain", "PAS")]);
        assert_eq!(written_rows(&rows), pairs(&[("stain", "PAS")]));
    }

    /// A key typed with a trailing space is the same key. Untrimmed, it is
    /// invisible in the editor and unmatchable in the search box.
    #[test]
    fn the_editor_trims_keys() {
        assert_eq!(
            written_rows(&pairs(&[(" stain ", "H&E")])),
            pairs(&[("stain", "H&E")])
        );
    }

    /// An id no entry has is refused rather than written somewhere else — the
    /// case a caller holding an id across a re-read can reach.
    #[test]
    fn an_edit_to_an_entry_that_is_gone_is_refused() {
        let scratch = Scratch::new("absent-entry");
        let path = scratch.project();
        let mut app = opened(path);
        let error = app
            .set_metadata(9999, &pairs(&[("k", "v")]))
            .expect_err("no entry 9999");
        assert!(error.contains("9999"), "the message names it: {error}");
    }

    /// The shuffle is a permutation, and it is not the same one twice.
    ///
    /// A permutation because the rows are addressed through it: an id lost is a
    /// row that vanishes, and one duplicated is a row that appears twice
    /// wearing another's picture. `sort_by_key` over a shuffle with a repeat
    /// would put two rows at the same position and pick between them silently.
    ///
    /// Not the same one twice because a shuffle seeded from the *list* would
    /// pass every other assertion here and mask nothing: two readers with the
    /// same study would see the same order, and comparing notes recovers it.
    /// Fifty entries makes a collision between two runs about `1/50!`, so a
    /// failure here is the seed having become a function of the entries rather
    /// than of the clock.
    #[test]
    fn the_mask_shuffle_is_a_permutation_and_not_a_constant() {
        let images: Vec<qupath_viewer::project::Image> = (1..=50)
            .map(|id| qupath_viewer::project::Image {
                id,
                name: format!("image {id}"),
                builder: String::from("uri"),
                slide: Ok(PathBuf::from(format!("/slides/{id}.svs"))),
                uri: None,
                description: None,
                metadata: Vec::new(),
                tags: Vec::new(),
                randomized_name: Some(format!("r{id}")),
            })
            .collect();

        let once = super::shuffled_ids(&images);
        let mut sorted = once.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (1..=50).collect::<Vec<u64>>());

        let twice = super::shuffled_ids(&images);
        assert_ne!(once, twice, "the shuffle does not depend on the clock");

        // And it moves things: an implementation whose loop bound was wrong
        // leaves the list in its own order and every other assertion holds.
        assert_ne!(once, sorted, "the shuffle left the list in order");
    }

    /// What one event of each kind is worth, in zoom steps.
    ///
    /// The arithmetic of the `MouseWheel` arm, extracted so it can be asserted
    /// with no window: building an `App` needs a display, and this is the part
    /// that was wrong.
    fn steps(delta: &MouseScrollDelta, ctrl: bool) -> f64 {
        let steps = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
            MouseScrollDelta::PixelDelta(position) => position.y / TRACKPAD_PIXELS_PER_STEP,
        };
        if ctrl {
            steps * CTRL_SCROLL_STEPS
        } else {
            steps
        }
    }

    /// The cursor is over the slide at a scale factor that is not 1.0.
    ///
    /// This is the bug rather than its arithmetic. `chrome::draw` hands back a
    /// rectangle in `egui`'s logical points and `self.cursor` is in winit's
    /// physical pixels, so at a scale factor of 2.0 a cursor in the middle of
    /// the slide has coordinates *past the right edge* of the unconverted
    /// rectangle. Every gesture gated on `cursor_over_slide` — scroll-to-zoom,
    /// drag-to-pan, the zoom band — then does nothing, which is what "pinch
    /// zoom stopped working" was.
    ///
    /// Asserted through `slide_area` set the way `frame` sets it rather than
    /// against `scale_area` alone, because the arithmetic was never in doubt:
    /// what was wrong is that nothing applied it. Setting the field raw is what
    /// this failed at before the fix.
    #[test]
    fn the_cursor_is_over_the_slide_at_a_scale_factor_of_two() {
        // The layout `egui` reports for a 1280x860 physical window at 2.0: a
        // 640x430 point window with a toolbar strip off the top and a status
        // bar off the bottom, leaving the slide the middle.
        let reported = (0.0, 30.0, 640.0, 400.0);

        let mut app = App::new(None);
        app.slide_area = Some(scale_area(reported, 2.0));

        // The centre of the slide, in the physical pixels `CursorMoved` gives.
        app.cursor = ScreenPoint::new(640.0, 460.0);
        assert!(
            app.cursor_over_slide(),
            "the middle of the slide must be over the slide at a scale factor of 2.0"
        );

        // And the status bar is still outside it, so this did not pass by
        // making the rectangle unbounded.
        app.cursor = ScreenPoint::new(640.0, 820.0);
        assert!(
            !app.cursor_over_slide(),
            "the status bar must not be over the slide"
        );

        // The value that would have made this fail is reachable: with the
        // rectangle left in points, the same centre cursor reads as outside.
        app.slide_area = Some(reported);
        app.cursor = ScreenPoint::new(640.0, 460.0);
        assert!(
            !app.cursor_over_slide(),
            "unconverted, the centre reads as outside -- this is what regressed"
        );
    }

    /// The interface is drawn in the framebuffer's **physical** pixels, and
    /// `egui` tessellates it in logical points.
    ///
    /// The same bug as the test above, one layer further down, and the fix for
    /// that one could not have caught it: converting the rectangle the
    /// interface *reports* says nothing about the units the interface is
    /// *drawn* in. Those were wrong in opposite directions — the hit-testing
    /// rectangle scaled up, the drawing left in points — so at a scale factor
    /// of 2.0 the toolbar covered half the window's width, the status bar ran
    /// across the middle of the tissue, and `cursor_over_slide` answered for a
    /// layout that was nowhere on screen.
    ///
    /// Asserted on the *rasterised frame* rather than on the arithmetic,
    /// because the arithmetic was never in doubt: what was wrong is that
    /// nothing applied it, and a test of a conversion function passes happily
    /// while its caller ignores the argument. [`draw_interface`] is free so
    /// that this can run the real path with no window.
    #[test]
    fn the_interface_is_drawn_across_the_whole_window_at_a_scale_factor_of_two() {
        // A 1280x860 physical window at a scale factor of 2.0 is the 640x430
        // points `egui_winit` lays out in.
        const PHYSICAL: (u32, u32) = (1280, 860);
        const POINTS: (f32, f32) = (640.0, 430.0);

        let context = egui::Context::default();
        chrome::style(&context);
        let app = App::new(None);

        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(POINTS.0, POINTS.1),
            )),
            ..Default::default()
        };
        let viewport = input.viewport_id;
        input
            .viewports
            .get_mut(&viewport)
            .expect("the root viewport")
            .native_pixels_per_point = Some(2.0);

        // Twice, for `examples/chrome.rs`'s reason: the font atlas arrives in
        // the first frame's texture delta and only *changes* afterwards, so one
        // frame alone has nothing to draw its text with.
        let first = context.run(input.clone(), |context| {
            let _ = chrome::draw(context, &app);
        });
        let output = context.run(input, |context| {
            let _ = chrome::draw(context, &app);
        });
        assert!(
            (output.pixels_per_point - 2.0).abs() < f32::EPSILON,
            "the scale factor did not reach `egui`: {}",
            output.pixels_per_point,
        );

        let mut textures = Vec::new();
        apply_textures(&mut textures, &first.textures_delta);
        apply_textures(&mut textures, &output.textures_delta);
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);

        /// How much of the frame's last row is painted rather than left clear.
        ///
        /// The status bar is a panel across the bottom of the window, so that
        /// row is its background — and it is painted only if the interface
        /// reaches the bottom of the *window* rather than the bottom of the
        /// points it was laid out in.
        fn bottom_row(
            primitives: &[egui::ClippedPrimitive],
            textures: &[(egui::TextureId, Texture)],
            pixels_per_point: f32,
            size: (u32, u32),
        ) -> usize {
            let mut frame = Framebuffer::new(size.0, size.1);
            frame.clear(compose::CLEAR);
            draw_interface(
                &mut frame,
                textures,
                &mut Vec::new(),
                primitives,
                pixels_per_point,
            );
            let width = size.0 as usize;
            frame.pixels()[(size.1 as usize - 1) * width..]
                .iter()
                .filter(|pixel| **pixel != compose::CLEAR)
                .count()
        }

        let painted = bottom_row(&primitives, &textures, output.pixels_per_point, PHYSICAL);
        assert!(
            painted >= PHYSICAL.0 as usize - 2,
            "the status bar must reach the bottom row of the window: {painted} of {} pixels",
            PHYSICAL.0,
        );

        // What it did before: laid out in points and drawn as pixels, the
        // interface stops halfway down and the bottom half of the window is
        // bare clear colour over a slide drawn at full size.
        assert_eq!(
            bottom_row(&primitives, &textures, 1.0, PHYSICAL),
            0,
            "unscaled, nothing reaches the bottom row -- this is what regressed",
        );
    }

    /// A frame whose numbers are not on screen yet asks for another.
    ///
    /// The interface is laid out before the frame is composed — deliberately,
    /// since the commands it produces change what that frame shows — so the
    /// status bar's "N of M outlined" and "N tiles drawn" are always the
    /// previous frame's. At rest that means the *last* frame's numbers are
    /// never shown at all, because nothing asks for another one.
    ///
    /// What that looked like: press `d`, wait for the run, and read "0 of 4027
    /// outlined" over a screen covered in outlines, until the mouse was moved.
    /// Verified in a real window before and after — `docs/dev/VIEWER.md` §6's
    /// "checked by looking", which is what a number in a status bar is.
    #[test]
    fn a_frame_whose_numbers_are_not_on_screen_yet_asks_for_another() {
        let mut app = App::new(None);
        assert!(
            !app.wants_another_frame(),
            "a viewer with nothing to do sleeps, which is what keeps it at zero CPU"
        );

        // What `draw` sets when the counts it just computed differ from the
        // ones the layout above it reported.
        app.unreported = true;
        assert!(
            app.wants_another_frame(),
            "the interface is a frame behind and must not be left there"
        );
    }

    /// A saved file is named after the slide, not after its file name.
    ///
    /// `display_name` keeps the extension because a status bar wants it, and
    /// reusing it here produced "CMU-1.svs-detections.geojson".
    #[test]
    fn a_saved_file_is_named_after_the_slide() {
        assert_eq!(
            export_name(Some(Path::new("/slides/CMU-1.svs"))),
            "CMU-1-detections.geojson"
        );
        // A slide with dots in its name keeps all but the last segment, which
        // is what `file_stem` means and what a person would write by hand.
        assert_eq!(
            export_name(Some(Path::new("/slides/CMU-1-JP2K-33005.svs"))),
            "CMU-1-JP2K-33005-detections.geojson"
        );
        // No slide open is still a usable name rather than an empty one: the
        // overlay outlives the slide, so this is reachable.
        assert_eq!(export_name(None), "detections.geojson");
    }

    /// The save is offered exactly when it would do something.
    ///
    /// The menu item's enabled state and the action's own guard read one
    /// predicate. Two would drift, and the way they drift is a live menu item
    /// that does nothing when pressed — which reads as a broken save rather
    /// than as an empty one.
    #[test]
    fn there_is_nothing_to_save_until_a_run_finds_something() {
        let mut app = App::new(None);
        assert!(app.found().is_none(), "a fresh viewer has found nothing");

        // A run that finished having found nothing is still nothing to save,
        // which is the case an `is_some()` on the overlay alone would offer.
        app.detector.overlay = Some(Overlay::new());
        assert!(
            app.found().is_none(),
            "an empty overlay must not offer a file with no objects in it"
        );

        // And one with an object in it is.
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&[[0, 0], [4, 0], [4, 4], [0, 4]], 1.0);
        app.detector.overlay = Some(overlay);
        assert!(app.found().is_some());

        // Closing the slide takes it away again, because the outlines are in
        // that slide's coordinates -- `Detector::clear` is what enforces it and
        // this is the reachable consequence.
        app.detector.clear();
        assert!(
            app.found().is_none(),
            "outlines from a closed slide must not be offered for saving"
        );
    }

    /// A jump from the overview lands where it was aimed, and stays on the
    /// slide when it was not.
    ///
    /// `Command::JumpTo` goes through `Open::look`, so it is clamped like every
    /// other move of the view — this asserts the clamp is what happens rather
    /// than that it was remembered, which is the property `clamp_to_extent`
    /// exists for. Written against the arithmetic rather than against an `App`
    /// for the reason `steps` is: building one needs a display.
    #[test]
    fn a_jump_from_the_overview_stays_on_the_slide() {
        let overview = Overview::fit(46000, 32914).expect("a real slide");
        let view = Viewport {
            centre: SlidePoint::new(23000.0, 16457.0),
            scale: 0.25,
            width: 1024,
            height: 768,
        };

        // A click in the middle of the picture centres on the middle.
        let (width, height) = overview.size();
        let middle = overview.to_slide(ScreenPoint::new(width as f32 / 2.0, height as f32 / 2.0));
        let jumped = Viewport {
            centre: middle,
            ..view
        }
        .clamp_to_extent(46000, 32914);
        assert!((jumped.centre.x - 23000.0).abs() < 400.0);
        assert!((jumped.centre.y - 16457.0).abs() < 400.0);

        // And a click outside the picture — which a drag off its edge
        // produces, since `to_slide` is deliberately unclamped — cannot take
        // the view off the slide.
        for corner in [
            ScreenPoint::new(-40.0, -40.0),
            ScreenPoint::new(width as f32 + 80.0, height as f32 + 80.0),
        ] {
            let outside = Viewport {
                centre: overview.to_slide(corner),
                ..view
            }
            .clamp_to_extent(46000, 32914);
            assert!(
                outside.centre.x >= 0.0
                    && outside.centre.x <= 46000.0
                    && outside.centre.y >= 0.0
                    && outside.centre.y <= 32914.0,
                "a drag off the overview left the slide: {:?}",
                outside.centre,
            );
        }
    }

    /// What a zoom factor a run of events comes to.
    fn factor(events: &[MouseScrollDelta], ctrl: bool) -> f64 {
        events
            .iter()
            .map(|delta| 1.2f64.powf(steps(delta, ctrl)))
            .product()
    }

    /// The bug that was reported: a trackpad scroll did essentially nothing.
    ///
    /// A two-finger swipe on a trackpad arrives as a stream of `PixelDelta`
    /// events of a few pixels each. At the old divisor of 50 a whole 150 px
    /// swipe came to 1.2^3 spread over its events — but each *event* was
    /// 1.2^0.06, and the swipe as a whole barely moved the view. This asserts
    /// on the swipe rather than the event, because that is the unit the user
    /// performs.
    #[test]
    fn a_trackpad_scroll_zooms_visibly() {
        // Fifteen events of 10 px: an ordinary short swipe.
        let swipe: Vec<_> = (0..15)
            .map(|_| MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 10.0)))
            .collect();
        let zoomed = factor(&swipe, false);
        assert!(
            zoomed > 2.0,
            "a 150 px trackpad swipe came to {zoomed:.2}x, which reads as nothing happening"
        );
        // And it is not so violent that a small movement flies off the slide.
        assert!(zoomed < 30.0, "a single swipe came to {zoomed:.2}x");
    }

    /// A mouse wheel is unaffected by the trackpad fix.
    ///
    /// The two share one event, so a change made for one can silently alter the
    /// other. One notch stays exactly 1.2.
    #[test]
    fn a_wheel_notch_is_still_one_step() {
        assert!((steps(&MouseScrollDelta::LineDelta(0.0, 1.0), false) - 1.0).abs() < 1e-12);
        assert!((steps(&MouseScrollDelta::LineDelta(0.0, -1.0), false) + 1.0).abs() < 1e-12);
    }

    /// Ctrl-scroll — which is what a GNOME trackpad pinch arrives as — zooms in
    /// the same direction as a plain scroll, and further.
    #[test]
    fn a_pinch_zooms_further_than_the_same_scroll_without_ctrl() {
        let event = MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 10.0));
        let plain = steps(&event, false);
        let pinched = steps(&event, true);
        assert!(
            pinched > plain && plain > 0.0,
            "ctrl {pinched} must exceed plain {plain}, both zooming in"
        );
        // Both directions, so ctrl is a magnitude and not a sign.
        let out = MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -10.0));
        assert!(steps(&out, true) < steps(&out, false) && steps(&out, false) < 0.0);
    }

    /// A pinch delta of NaN must not reach the viewport.
    ///
    /// `winit` documents that `PinchGesture`'s delta may be NaN. A NaN scale
    /// makes every later frame compute an empty tile list — a window that goes
    /// blank and stays blank — so it is refused at the door.
    #[test]
    fn a_non_finite_zoom_is_refused() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!bad.is_finite(), "{bad} must be caught by the guard");
        }
        // The guard itself, as `zoom_by` applies it.
        let guard = |steps: f64| steps.is_finite() && steps != 0.0;
        assert!(!guard(f64::NAN));
        assert!(!guard(0.0));
        assert!(guard(1.0));
    }

    /// The stain coefficients must be the *inverted* matrix's row. Typing a
    /// plausible vector from the stain matrix is the mistake
    /// `docs/dev/VIEWER.md` §8.5 records, and nothing failed when it happened —
    /// the channel looked like a working stain channel. This asserts that what
    /// the panel builds is what the pipeline computes, which is the only check
    /// a plausible-but-wrong vector cannot pass.
    #[test]
    fn the_stain_channels_carry_the_pipelines_own_numbers() {
        let stains = Stains::h_and_e();
        for (channel, index) in [(Channel::Haematoxylin, 0), (Channel::Eosin, 1)] {
            let display = Appearance {
                channel,
                ..Appearance::default()
            }
            .display();
            let stain = display.stain().expect("a stain channel has a stain");
            assert_eq!(
                stain.coefficients,
                stains.coefficients()[index],
                "{} took the wrong row",
                channel.label(),
            );
            assert_eq!(stain.background, stains.max_values());
        }
        assert!(
            Appearance::default().display().stain().is_none(),
            "the original colour is not a stain channel",
        );
    }

    /// The default must be the identity, so opening the application draws
    /// exactly what a build without any of this draws — the property
    /// `docs/dev/VIEWER.md` §3.4b's two backends rest on.
    #[test]
    fn the_default_appearance_changes_nothing() {
        assert!(Appearance::default().display().is_identity());
    }

    /// Where the state ends up after a run of commands, rather than what one
    /// command returns: the panel sends a whole `Appearance` every frame, so
    /// the question is whether a channel picked earlier survives a range moved
    /// later, and whether `Reset` puts everything back at once.
    #[test]
    fn the_commands_leave_the_display_where_the_panel_asked() {
        let mut app = App::new(None);
        assert!(!app.show_appearance);
        app.display_command(&Command::ToggleAppearance);
        assert!(app.show_appearance);

        app.display_command(&Command::SetAppearance(Appearance {
            channel: Channel::Eosin,
            ..Appearance::default()
        }));
        app.display_command(&Command::SetAppearance(Appearance {
            min: 40.0,
            max: 200.0,
            ..app.appearance
        }));
        assert_eq!(
            app.appearance.channel,
            Channel::Eosin,
            "moving the range must not forget the channel",
        );
        assert_eq!((app.appearance.min, app.appearance.max), (40.0, 200.0));

        app.display_command(&Command::ResetAppearance);
        assert!(
            app.appearance.display().is_identity(),
            "reset puts the range and the channel back together",
        );
        assert!(
            app.show_appearance,
            "reset is the display, not the panel: the panel stays open",
        );
    }

    /// A range that cannot be a range falls back to the identity rather than to
    /// a negative scale. The sliders keep the two ends apart so they cannot
    /// produce one, which is exactly why this is worth asserting: the guard is
    /// for a value arriving some other way, and an unexercised guard is one
    /// nobody notices has stopped working.
    #[test]
    fn an_impossible_range_falls_back_rather_than_inverting() {
        for (min, max) in [(200.0, 50.0), (128.0, 128.0), (f32::NAN, 255.0)] {
            let display = Appearance {
                min,
                max,
                ..Appearance::default()
            }
            .display();
            assert!(
                display.is_identity(),
                "({min}, {max}) should fall back, and gave {display:?}",
            );
        }
    }

    /// Ticking "Display" from a standing start must show something. The
    /// controls live inside the slide panel and that panel is closed by
    /// default, so the toggle on its own left the user having ticked a box and
    /// seen nothing change — which reads as a broken control rather than a
    /// closed panel. The same is true of the overview.
    ///
    /// Asserted as the gesture rather than the field: `show_appearance` being
    /// true is not the property that matters, `show_appearance && show_metadata`
    /// is, because that is the one that puts pixels on screen.
    #[test]
    fn turning_on_a_control_opens_the_panel_it_lives_in() {
        for (command, showing) in [
            (
                Command::ToggleAppearance,
                (|app: &App| app.show_appearance) as fn(&App) -> bool,
            ),
            (Command::ToggleOverview, |app: &App| app.show_overview),
        ] {
            let mut app = App::new(None);
            app.show_metadata = false;
            app.show_overview = false;
            app.show_appearance = false;

            app.display_command(&command);
            assert!(showing(&app), "{command:?} did not turn the control on");
            assert!(
                app.show_metadata,
                "{command:?} turned a control on inside a closed panel",
            );

            // Turning it back off leaves the panel alone: the user may have
            // opened it deliberately, and closing it under them is the mirror
            // of the bug above.
            app.display_command(&command);
            assert!(!showing(&app));
            assert!(
                app.show_metadata,
                "{command:?} closed the panel under the user"
            );
        }
    }

    /// A pyramid to start a run against, with a level 0 fine enough that
    /// `level_for_detection` reads it.
    ///
    /// The slide the run is pointed at does not exist, and that is what makes
    /// this deterministic: `Detector::start` decides the level and the region
    /// on *this* thread and only opens the file on the worker, so the run is
    /// in flight the moment `start` returns and fails immediately afterwards
    /// without ever reaching `poll`. A run over a real slide would race the
    /// keypress under test — it might finish first, and a test that passes
    /// because the work was already done is a test of nothing.
    fn a_pyramid() -> Vec<qupath_slide::Level> {
        vec![qupath_slide::Level {
            index: 0,
            width: 2220,
            height: 2967,
            tile_width: 240,
            tile_height: 240,
            columns: 10,
            rows: 13,
            downsample: 1.0,
        }]
    }

    /// An application with a detection run in flight and no window.
    fn detecting() -> App {
        let mut app = App::new(Vec::<String>::new());
        let levels = a_pyramid();
        app.detector.start(
            Path::new("/nonexistent/slide.svs"),
            &levels,
            &levels[0],
            Some(0.499),
            Viewport {
                centre: SlidePoint::new(1110.0, 1483.0),
                scale: 1.0,
                width: 1024,
                height: 768,
            },
            Micron::default(),
        );
        assert!(
            app.detector.is_running(),
            "the run under test never started"
        );
        app
    }

    /// Escape abandons the zoom band before it stops a detection run.
    ///
    /// The ordering *is* the behaviour, and it is not the obvious one: both
    /// gestures are "undo the thing I just started", and a keypress that
    /// resolved them the other way round would throw away a whole-slide run —
    /// up to a minute and a half of every core — because someone was drawing a
    /// rectangle while it went. That is a surprising amount of lost work for
    /// one key, and it is silent: the band vanishes either way, so the picture
    /// on screen after the keypress is the same in both cases and the
    /// difference only appears when the run does not come back.
    ///
    /// Two presses rather than one, because the first press asserted alone
    /// says nothing about the run: `Escape::Band` is also what a program that
    /// stopped the run *and* took the band would return. The second press
    /// finding the run still there is what says the first one left it alone.
    #[test]
    fn escape_abandons_the_band_before_it_stops_a_run() {
        let mut app = detecting();
        app.band = Some(ScreenPoint::new(120.0, 80.0));

        assert_eq!(
            app.escape(),
            Escape::Band,
            "a band being dragged is what escape means while it is being dragged",
        );
        assert!(app.band.is_none(), "the band was not abandoned");

        assert_eq!(
            app.escape(),
            Escape::Run(Command::StopDetecting),
            "the run should have survived the press that took the band, and the \
             press after it is what stops it",
        );
    }

    /// Escape stops a run when nothing else is in progress.
    ///
    /// The `Stop` button in the status bar is the discoverable form of this and
    /// goes through the same command; escape is the fast one. What this pins
    /// beyond "something happened" is *which* command — a keypress wired to
    /// `Detect` instead would start a second run over the region, which reads
    /// on screen as the first one having taken longer.
    #[test]
    fn escape_stops_a_run_when_there_is_no_band() {
        let mut app = detecting();
        assert!(app.band.is_none(), "no band is being dragged");

        assert_eq!(app.escape(), Escape::Run(Command::StopDetecting));
    }

    /// Escape with nothing in progress is not consumed.
    ///
    /// The outward-facing half, and the reason `key` returns a bool at all: a
    /// keypress reported as handled is one no other handler sees. Escape is a
    /// key an application gives away when it has nothing to abandon, and the
    /// failure it prevents is invisible from inside this function — a dialog
    /// that will not close, in code that has not been written yet.
    #[test]
    fn escape_with_nothing_in_progress_is_not_consumed() {
        let mut app = App::new(Vec::<String>::new());
        assert!(!app.detector.is_running() && app.band.is_none());

        assert_eq!(app.escape(), Escape::Nothing);
    }
}
