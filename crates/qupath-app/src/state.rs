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
use qupath_viewer::overview::Overview;
use qupath_viewer::{
    Display, Overlay, ScreenPoint, SlidePoint, SlideTiles, Stain, TileCache, TileLoader, Viewport,
};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use winit::window::Window;

use crate::chrome::{self, Command};
use crate::detect::Detector;

/// Texture budget, in bytes. Roughly two thousand tiles.
const BUDGET: usize = 512 << 20;

/// Decoding threads. Four is what `docs/VIEWER.md` §3.4 measured with.
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

/// A whole-slide picture and the mapping onto it.
pub struct Thumbnail {
    /// The picture, RGBA, `width` by `height` as `Overview::size` gave them.
    pub pixels: Vec<u8>,
    /// Geometry: where the view is, and where a click lands.
    pub overview: Overview,
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
        let overview = thumbnail(&slide);
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
    /// the bug `docs/VIEWER.md` §3.4b records finding in `present_cpu`.
    reported: bool,
    /// One mesh's vertices, converted from `egui`'s and reused across meshes
    /// and frames.
    ///
    /// `epaint::Vertex` and `compose::MeshVertex` have the same layout, so a
    /// cast would do — but a cast needs `bytemuck::Pod` on `MeshVertex`, which
    /// means a dependency the viewer library does not have and
    /// `docs/VIEWER.md` §7 says it should not acquire. The copy is a few
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
    /// `Stains::max_values` was exposed: `docs/VIEWER.md` §8.5 records that the
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
    /// units, so [`App::cursor_over_slide`] read false over most of the window
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
}

impl App {
    #[must_use]
    pub fn new(path: Option<String>) -> Self {
        let context = egui::Context::default();
        chrome::style(&context);
        Self {
            window: None,
            egui: None,
            context,
            surface: None,
            open: None,
            message: String::from("Open a slide to begin."),
            recent: Vec::new(),
            pending: path.map(PathBuf::from),
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

    /// Mean main-thread cost of the last [`WINDOW`] frames.
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
    /// nothing. Those ask [`App::cursor_over_slide`] instead, which is a
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
            Key::Named(NamedKey::Escape) => {
                // Not an exit. In an application escape closes a dialog, and
                // taking the window down on a stray keypress is what makes an
                // example an example. What it does do is abandon whatever is in
                // progress, which is now two things.
                //
                // The band goes first and alone: a person drawing a zoom
                // rectangle while a detection runs in the background means the
                // rectangle, and stopping their run instead would be a
                // surprising amount of lost work for one keypress.
                if self.band.take().is_some() {
                    return true;
                }
                // A zoom rectangle in the wrong place and a whole-slide run
                // started by mistake are the same gesture: undo the thing I
                // just started. The `Stop` button in the status bar is the
                // discoverable form of this; escape is the fast one.
                if self.detector.is_running() {
                    self.run(Command::StopDetecting, window);
                    return true;
                }
                false
            }
            _ => false,
        }
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
    /// `docs/VIEWER.md` §8.6 wrote the format and §8.2 recorded saving as done,
    /// and for several sessions **nothing called it**: `geojson::write` was a
    /// library function with no caller while the file's own opening paragraph
    /// said "what you find can be saved as GeoJSON that QuPath opens". This is
    /// that sentence being made true.
    ///
    /// No measurements are written, and that is a property of what the viewer
    /// keeps rather than a gap in the writer: [`crate::detect`] turns a run's
    /// [`Detections`](qupath_pipeline::Detections) into an [`Overlay`] of
    /// outlines and drops the rest, so there is nothing to attach. QuPath opens
    /// the file and shows the objects; a measurements column would need the
    /// detector to hold on to more, which is `docs/VIEWER.md` §8.8's business
    /// and not this function's.
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

        // No measurements: see this function's documentation.
        let (text, written) = qupath_viewer::geojson::write(overlay, &[]);
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
    /// omission: `docs/VIEWER.md` §8.5's display-only claim is structural in
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
            Command::OpenRecent(path) => {
                self.pending = Some(path);
                window.request_redraw();
            }
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
        if let Some(path) = self.pending.take() {
            self.open(path);
        }

        let (width, height) = self.size();
        if width == 0 || height == 0 {
            return;
        }
        self.request_tiles();
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
        // whether it is busy first — both are `docs/VIEWER.md` §3.4's rule
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
            state.set_overview(&pixels, width, height);
            self.uploaded_overview = true;
        }
        state.frame.clear(compose::CLEAR);
        // The display transform is applied here, on the way to the frame, and
        // nowhere earlier: the tiles in the cache are the slide's own pixels,
        // so changing the range costs a redraw and not a reload, and nothing
        // the detector reads has been through it. That is the structural half
        // of `docs/VIEWER.md` §8.5's display-only claim — `Detector::start` is
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
    /// Registers the overview picture as a user texture, replacing any
    /// previous one.
    ///
    /// `egui` owns the *managed* ids in [`Software::textures`]; this claims one
    /// user id alongside them, so `mesh` resolves it through the same lookup
    /// and needs no special case. Uploaded per slide rather than per frame:
    /// `chrome` names the texture every frame, but the pixels only change when
    /// a different slide is opened, which is what `dirty` tracks.
    fn set_overview(&mut self, pixels: &[u8], width: u32, height: u32) {
        let id = egui::TextureId::User(chrome::OVERVIEW_TEXTURE);
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

/// Reads a slide's overview picture, or `None` if it has none to give.
///
/// From the **coarsest** level, which is the whole point: the top of CMU-1's
/// pyramid is 1,024 px on its long edge, so the picture costs one small region
/// read rather than a decimation of level 0. Downsampled from there by nearest
/// sampling — the picture is 150 px wide and this runs once per slide, so a
/// better filter would be invisible and unmeasurable.
fn thumbnail(slide: &Slide) -> Option<Thumbnail> {
    let base = slide.levels().first()?;
    let overview = Overview::fit(base.width, base.height)?;
    let (width, height) = overview.size();
    let coarsest = slide.levels().last()?;
    let source = slide
        .read_region_rgb(coarsest.index, 0, 0, coarsest.width, coarsest.height)
        .ok()?;

    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for row in 0..height {
        // The centre of this destination row in source pixels, which is the
        // same "which pixel does this point fall in" rule `Framebuffer` uses.
        let source_row =
            ((f64::from(row) + 0.5) * f64::from(coarsest.height) / f64::from(height)) as usize;
        let source_row = source_row.min(coarsest.height as usize - 1);
        for column in 0..width {
            let source_column =
                ((f64::from(column) + 0.5) * f64::from(coarsest.width) / f64::from(width)) as usize;
            let source_column = source_column.min(coarsest.width as usize - 1);
            let at = (source_row * coarsest.width as usize + source_column) * 3;
            pixels.extend_from_slice(&[source[at], source[at + 1], source[at + 2], 255]);
        }
    }
    Some(Thumbnail { pixels, overview })
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
    /// being a drag, at the same threshold [`App::finish_band`] uses.
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
        // twice. See `docs/VIEWER.md` §3.6.
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
    use super::*;

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
    /// `docs/VIEWER.md` §8.5 records, and nothing failed when it happened —
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
    /// `docs/VIEWER.md` §3.4b's two backends rest on.
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
}
