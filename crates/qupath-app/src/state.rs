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

use qupath_slide::Slide;
use qupath_viewer::compose::{self, Framebuffer, MeshVertex, Texture};
use qupath_viewer::{ScreenPoint, SlidePoint, SlideTiles, TileCache, TileLoader, Viewport};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use winit::window::Window;

use crate::chrome::{self, Command};

/// Texture budget, in bytes. Roughly two thousand tiles.
const BUDGET: usize = 512 << 20;

/// Decoding threads. Four is what `docs/VIEWER.md` §3.4 measured with.
const WORKERS: usize = 4;

/// How many frame costs the status bar averages over.
const WINDOW: usize = 30;

/// How many slides the recent list keeps.
const RECENT: usize = 8;

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
        let source = Arc::new(SlideTiles::new(slide));
        let loader = TileLoader::new(&source, WORKERS);
        Ok(Self {
            path,
            source,
            loader,
            cache: TileCache::new(BUDGET),
            view,
            drawn: 0,
        })
    }

    pub fn slide(&self) -> &Slide {
        self.source.slide()
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
    cursor: ScreenPoint,
    costs: Vec<Duration>,
    pub show_overlay: bool,
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
            cursor: ScreenPoint::new(0.0, 0.0),
            costs: Vec::new(),
            show_overlay: true,
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
                let name = display_name(&open.path);
                self.message = format!("{name} — {}", describe(open.slide()));
                self.recent.retain(|recent| recent != &path);
                self.recent.insert(0, path);
                self.recent.truncate(RECENT);
                self.open = Some(open);
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
            open.view = open.view.resize(width, height);
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
            || self
                .open
                .as_ref()
                .is_some_and(|open| !open.loader.is_idle())
    }

    /// Handles one window event that the interface did not take, and says
    /// whether a redraw is needed.
    ///
    /// `consumed` is `egui`'s answer to "was this mine". Without honouring it a
    /// drag that starts on a slider also pans the slide, and a scroll over a
    /// panel zooms the view underneath it.
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
                    open.view = open.view.pan(dx, dy);
                    return true;
                }
                // A moved cursor still repaints, because the status bar carries
                // the position under it.
                self.open.is_some()
            }
            WindowEvent::MouseInput { state, button, .. } if *button == MouseButton::Left => {
                // A drag is only the slide's if it *began* outside the
                // interface. Testing `consumed` on every motion instead would
                // drop the drag the moment the cursor crossed a panel.
                self.dragging = *state == ElementState::Pressed && !consumed;
                false
            }
            WindowEvent::MouseWheel { delta, .. } if !consumed => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    MouseScrollDelta::PixelDelta(position) => position.y / 50.0,
                };
                if let Some(open) = &mut self.open {
                    open.view = open.view.zoom_about(1.2f64.powf(steps), self.cursor);
                    return true;
                }
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

    fn key(&mut self, window: &Window, key: &Key) -> bool {
        match key.as_ref() {
            Key::Character("f") => {
                if let Some(open) = &mut self.open {
                    let (width, height) = (open.view.width, open.view.height);
                    open.view = Viewport::fit(open.slide(), width, height);
                    return true;
                }
                false
            }
            Key::Character("o") => {
                self.run(Command::Open, window);
                true
            }
            Key::Character("d") => {
                self.show_overlay = !self.show_overlay;
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Not an exit. In an application escape closes a dialog, and
                // taking the window down on a stray keypress is what makes an
                // example an example.
                false
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
                self.message = String::from("Open a slide to begin.");
            }
            Command::Fit => {
                if let Some(open) = &mut self.open {
                    let (width, height) = (open.view.width, open.view.height);
                    open.view = Viewport::fit(open.slide(), width, height);
                }
            }
            Command::Zoom(steps) => {
                if let Some(open) = &mut self.open {
                    // The same 1.5 a scroll step uses, so the buttons and the
                    // wheel move the view by the same amount.
                    open.view = open.view.zoom_about(1.5f64.powi(steps), centre(&open.view));
                }
            }
            Command::Actual => {
                if let Some(open) = &mut self.open {
                    let factor = 1.0 / open.view.scale;
                    open.view = open.view.zoom_about(factor, centre(&open.view));
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

        // The interface is laid out *before* the frame is drawn, because the
        // commands it produces change what the frame shows — a fit pressed this
        // frame must be the view this frame draws, not the next one.
        let Some(egui) = self.egui.as_mut() else {
            return;
        };
        let input = egui.take_egui_input(window);
        let mut command = Command::None;
        let output = self.context.clone().run(input, |context| {
            command = chrome::draw(context, self);
        });
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

        let Some(state) = &mut self.surface else {
            return;
        };
        state.frame.clear(compose::CLEAR);
        for (rect, tile) in &draws {
            state
                .frame
                .draw_tile(*rect, &tile.pixels, tile.width, tile.height);
        }
        drop(draws);
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

/// Where the cursor is on the slide, in level-0 pixels.
pub fn cursor_slide_position(open: &Open, cursor: ScreenPoint) -> SlidePoint {
    open.view.to_slide(cursor)
}

impl App {
    /// The cursor's position, for the status bar.
    pub fn cursor(&self) -> ScreenPoint {
        self.cursor
    }
}
