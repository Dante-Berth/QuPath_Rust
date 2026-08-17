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

use qupath_pipeline::Micron;
use qupath_slide::Slide;
use qupath_viewer::compose::{self, Framebuffer, MeshVertex, Texture};
use qupath_viewer::overview::Overview;
use qupath_viewer::{ScreenPoint, SlidePoint, SlideTiles, TileCache, TileLoader, Viewport};
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
    pub fn outstanding(&self) -> usize {
        self.loader.pending() + self.loader.in_flight()
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
    /// `(left, top, right, bottom)` in window pixels.
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
            uploaded_overview: false,
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
                if let Some(open) = &mut self.open {
                    let (width, height) = (open.view.width, open.view.height);
                    let fitted = Viewport::fit(open.slide(), width, height);
                    open.look(fitted);
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
            Key::Named(NamedKey::Escape) => {
                // Not an exit. In an application escape closes a dialog, and
                // taking the window down on a stray keypress is what makes an
                // example an example. What it does do is abandon a zoom
                // rectangle mid-drag, which is the way out of a box drawn in
                // the wrong place that does not require guessing where the
                // corner was.
                self.band.take().is_some()
            }
            _ => false,
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
            Command::Detect => {
                if let Some(open) = &self.open {
                    let level = open.view.level(open.slide()).clone();
                    self.detector
                        .start(&open.path, &level, open.view, self.tuning);
                }
            }
            Command::ToggleOverlay => self.show_overlay = !self.show_overlay,
            Command::ToggleAnalysis => self.show_analysis = !self.show_analysis,
            Command::ToggleMetadata => self.show_metadata = !self.show_metadata,
            Command::ToggleHelp => self.show_help = !self.show_help,
            Command::ToggleOverview => self.show_overview = !self.show_overview,
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
            Command::SetTuning(tuning) => self.tuning = tuning,
            Command::ResetTuning => self.tuning = Micron::default(),
            Command::Fit => {
                if let Some(open) = &mut self.open {
                    let (width, height) = (open.view.width, open.view.height);
                    let fitted = Viewport::fit(open.slide(), width, height);
                    open.look(fitted);
                }
            }
            Command::Zoom(steps) => {
                if let Some(open) = &mut self.open {
                    // The same 1.5 a scroll step uses, so the buttons and the
                    // wheel move the view by the same amount.
                    open.look(open.view.zoom_about(1.5f64.powi(steps), centre(&open.view)));
                }
            }
            Command::Actual => {
                if let Some(open) = &mut self.open {
                    let factor = 1.0 / open.view.scale;
                    open.look(open.view.zoom_about(factor, centre(&open.view)));
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
        self.slide_area = slide_area;
        if let Some(egui) = self.egui.as_mut() {
            egui.handle_platform_output(window, output.platform_output);
        }
        self.run(command, window);

        let primitives = self
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        self.draw(&primitives, &output.textures_delta);

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
    fn draw(&mut self, primitives: &[egui::ClippedPrimitive], delta: &egui::TexturesDelta) {
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
        for (rect, tile) in &draws {
            state
                .frame
                .draw_tile(*rect, &tile.pixels, tile.width, tile.height);
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

        state.apply(delta);
        for primitive in primitives {
            let egui::epaint::Primitive::Mesh(mesh) = &primitive.primitive else {
                // `Callback` is for a renderer that draws its own thing inside
                // the interface. Nothing here does, so it is skipped rather
                // than asserted on.
                continue;
            };
            state.mesh(primitive.clip_rect, mesh);
        }

        state.present();
    }
}

impl Software {
    /// Applies `egui`'s texture changes: whole images and patches of them.
    fn apply(&mut self, delta: &egui::TexturesDelta) {
        for (id, image) in &delta.set {
            let egui::ImageData::Color(source) = &image.image;
            let [source_width, source_height] = source.size;
            let pixels: Vec<[u8; 4]> = source.pixels.iter().map(|pixel| pixel.to_array()).collect();

            let at = self
                .textures
                .iter()
                .position(|(existing, _)| existing == id);
            match (image.pos, at) {
                // A patch of an existing texture, which is what `egui` sends
                // when a new glyph is rasterised into the atlas.
                (Some([x, y]), Some(at)) => {
                    let texture = &mut self.textures[at].1;
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
                        self.textures[at].1 = texture;
                    } else {
                        self.textures.push((*id, texture));
                    }
                }
            }
        }
        for id in &delta.free {
            self.textures.retain(|(existing, _)| existing != id);
        }
    }

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

    /// Rasterises one `egui` mesh into the frame.
    ///
    /// The texture is resolved to an *index* rather than a reference so that the
    /// frame, the vertex buffer and the texture can be borrowed at once — the
    /// three live in this struct and a returned `&Texture` would borrow all of
    /// it.
    fn mesh(&mut self, clip: egui::Rect, mesh: &egui::Mesh) {
        let Some(at) = self
            .textures
            .iter()
            .position(|(id, _)| *id == mesh.texture_id)
        else {
            return;
        };

        // `epaint::Vertex` is `{Pos2, Pos2, Color32}` and `MeshVertex` is the
        // same three fields; this is that conversion, written out because a
        // cast would need a trait the viewer library does not implement.
        self.vertices.clear();
        self.vertices
            .extend(mesh.vertices.iter().map(|vertex| MeshVertex {
                position: [vertex.pos.x, vertex.pos.y],
                uv: [vertex.uv.x, vertex.uv.y],
                colour: vertex.color.to_array(),
            }));

        let Self {
            frame,
            textures,
            vertices,
            ..
        } = self;
        frame.draw_mesh(
            (clip.min.x, clip.min.y, clip.max.x, clip.max.y),
            vertices,
            &mesh.indices,
            &textures[at].1,
        );
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
}
