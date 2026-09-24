//! Milestones 3.1 to 3.4 of `docs/dev/VIEWER.md`: a window showing a slide, with
//! pan, zoom, pyramid-level selection, and tiles loaded off the main thread
//! into a cache with a hard byte budget.
//!
//! ```text
//! cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs
//! cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs 64
//! cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs 64 --cpu
//! ```
//!
//! Drag to pan, scroll to zoom, `f` to fit the whole slide, escape to quit. The
//! optional second argument is the texture budget in MB, which is the one knob
//! worth sweeping: set it low enough and the title bar's eviction count climbs
//! while the frame still draws, because the underlay covers what was thrown
//! away.
//!
//! The title bar carries the backend, the level, the tile count, resident
//! megabytes, what the loader is still working on, and how long the last thirty
//! frames took — the level selection and the cache made visible. Zooming out
//! should *drop* the tile count as it crosses to a coarser level, not raise it,
//! and a long pan should leave resident megabytes flat.
//!
//! The milliseconds are time spent in [`App::render`], which is the frame's
//! cost on the main thread and not the whole story: on the GPU path handing the
//! frame over is asynchronous, so it is submission time rather than how long
//! the GPU took, and neither path counts the decode, which happens on the
//! loader threads and is what a pan actually waits for. It is in the title
//! because "does it stutter" is otherwise answered by looking, and this is the
//! part of that question a number can answer.
//!
//! # Two backends
//!
//! The GPU path is the default and is what §3.1 to §3.4 were built on. `--cpu`
//! forces the software one, which draws the same rectangles through
//! [`qupath_viewer::compose`] and presents them with `softbuffer` — no adapter,
//! no driver, no shader. It is also what happens automatically when there is no
//! adapter to be had, which used to be a panic at startup.
//!
//! Both take the same route as far as the draw list: same viewport, same level
//! selection, same loader, same cache, same rectangles from
//! [`Viewport::tile_rect`]. Only the last step differs, which is what makes
//! `--cpu` worth having on a machine that *does* have a GPU — the two frames
//! should differ in sampling and in nothing else.
//!
//! # What this is checking
//!
//! That the pixels `qupath-slide` decodes reach the screen unchanged, that a
//! level change is invisible, and that a sustained pan neither stalls on decode
//! nor grows without bound. `docs/dev/VIEWER.md` §6 is explicit that appearance is
//! checked by looking rather than by a test, so the parts that *can* be
//! asserted live in the library and are tested there — `TileCache`'s budget,
//! `TileLoader`'s cancellation, the viewport geometry, and now where a tile
//! lands in a frame.
//!
//! # What is not here
//!
//! Milestone 3.5, overlays. Nothing from `qupath-objects` is drawn.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qupath_slide::Slide;
use qupath_viewer::compose::{self, Framebuffer};
use qupath_viewer::{Loaded, ScreenPoint, SlideTiles, TileCache, TileIndex, TileLoader, Viewport};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Screen-space rectangle and texture, packed for the shader.
///
/// One instance per tile drawn. `rect` is `(x, y, width, height)` in window
/// pixels, exactly as [`Viewport::tile_rect`] returns it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    rect: [f32; 4],
    /// Window size, so the shader can map pixels to clip space without a
    /// separate uniform buffer.
    window: [f32; 2],
    _padding: [f32; 2],
}

/// How many tiles one frame can draw, underlay included. A 4K window at the
/// finest level needs a few hundred; past this the frame is truncated rather
/// than misdrawn.
const MAX_TILES: usize = 1024;

/// Texture budget when none is given, in bytes. Roughly two thousand tiles.
const DEFAULT_BUDGET: usize = 512 << 20;

/// The colour an undrawn pixel keeps.
///
/// `wgpu` takes this as linear and stores it into an sRGB surface, so what ends
/// up in memory is the encoded form — `(80, 80, 84)` on the driver here, where
/// [`compose::CLEAR`] holds the correctly rounded `(80, 80, 85)`. One unit in
/// blue, decided inside the driver; the two backends clear to the same colour
/// rather than to two colours that happen to look similar.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.08,
    g: 0.08,
    b: 0.09,
    a: 1.0,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let force_cpu = args.iter().any(|argument| argument == "--cpu");
    let mut positional = args.iter().filter(|argument| !argument.starts_with("--"));
    let path = positional
        .next()
        .cloned()
        .unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let budget = positional
        .next()
        .and_then(|megabytes| megabytes.parse::<usize>().ok())
        .map_or(DEFAULT_BUDGET, |megabytes| megabytes << 20);

    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    // One thread short of the machine, so the event loop still gets scheduled
    // while every worker is decoding. `ROADMAP.md` §5's rule against nesting
    // `par_iter` is why this is a pool of its own rather than rayon's.
    let workers = std::thread::available_parallelism()
        .map_or(3, |threads| threads.get().saturating_sub(1).clamp(1, 8));

    println!("{path}");
    println!(
        "  {workers} loader threads, {} MB texture budget",
        budget >> 20
    );
    println!("  drag to pan, scroll to zoom, f to fit, escape to quit");

    let view = Viewport::fit(&slide, 1024, 768);
    let source = Arc::new(SlideTiles::new(slide));
    let loader = TileLoader::new(&source, workers);
    let mut app = App {
        source,
        loader,
        budget,
        force_cpu,
        view,
        dragging: false,
        cursor: ScreenPoint::new(0.0, 0.0),
        requested: Vec::new(),
        incomplete: false,
        frame_times: VecDeque::new(),
        window: None,
        backend: None,
    };

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut app).expect("run");
}

/// Everything that needs a window to exist before it can be built.
struct Graphics {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Decoded tiles resident on the GPU, under a hard byte budget. Dropping a
    /// `BindGroup` releases the texture behind it, so eviction is what actually
    /// frees GPU memory.
    resident: TileCache<wgpu::BindGroup>,
    instances: wgpu::Buffer,
}

/// The same thing with no adapter: tiles kept as bytes in main memory and
/// composed by [`qupath_viewer::compose`].
struct Software {
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    frame: Framebuffer,
    /// Decoded tiles as the loader delivered them. The budget means the same
    /// thing it does on the GPU path — these are the bytes a texture would have
    /// held — so the two backends evict at the same points and the title bar's
    /// resident figure is comparable between them.
    resident: TileCache<Tile>,
    /// Whether a failure to present has already been reported.
    ///
    /// A frame that cannot reach the window server is indistinguishable from a
    /// frame that was never drawn, and dropping the error silently would make a
    /// broken presentation look exactly like a blank slide. Reported once
    /// rather than every frame, because the failure is a standing condition and
    /// the window keeps trying.
    reported: bool,
}

/// A decoded tile held in main memory, ready to blit.
struct Tile {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// One tile and where it goes, in window pixels.
///
/// The last thing the two backends agree on: one turns it into an [`Instance`]
/// for the shader, the other blits into it, and everything upstream — viewport,
/// level, loader, cache — produced the same list either way.
struct Draw {
    tile: TileIndex,
    /// Exactly what [`Viewport::tile_rect`] returns: `(x, y, width, height)`.
    rect: (f32, f32, f32, f32),
}

/// Where a frame ends up.
///
/// The split is deliberately as late as possible: everything up to the draw
/// list — the viewport, the level, the loader, the cache, the rectangles — is
/// shared, and only storing a tile and presenting a frame differ. A backend
/// that diverged earlier would make `--cpu` a different viewer rather than a
/// different last step.
enum Backend {
    Gpu(Box<Graphics>),
    Cpu(Box<Software>),
}

struct App {
    source: Arc<SlideTiles>,
    loader: TileLoader,
    budget: usize,
    /// Whether `--cpu` was given. Skips adapter selection outright rather than
    /// finding a GPU and declining to use it, so the software path can be
    /// looked at on a machine that has one.
    force_cpu: bool,
    view: Viewport,
    dragging: bool,
    cursor: ScreenPoint,
    /// What was last handed to the loader, so a redraw that wants the same
    /// tiles does not cancel and re-queue the request it is waiting on.
    requested: Vec<TileIndex>,
    /// Whether the last frame was missing a tile that is still coming. Drives
    /// the wake-up in [`ApplicationHandler::about_to_wait`].
    incomplete: bool,
    /// How long the last few frames took on the main thread, in milliseconds.
    ///
    /// In the title bar because "does it stutter" is otherwise checked by
    /// looking, and this is the one part of that question a number can answer.
    /// Read it for what it is: time spent in `render`, which is uploading or
    /// blitting whatever arrived, building the draw list, and handing the frame
    /// over. On the GPU path handing it over is asynchronous, so this is
    /// submission time and not how long the GPU took; on the CPU path the
    /// composition is right there in it. Neither includes the decode, which
    /// happens on the loader threads and is what a pan actually waits for.
    frame_times: VecDeque<f64>,
    window: Option<Arc<Window>>,
    backend: Option<Backend>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.backend.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("qupath-viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.view.width,
                self.view.height,
            ));
        let window = Arc::new(event_loop.create_window(attributes).expect("window"));

        let backend = Backend::new(&window, self.budget, self.force_cpu);
        // The window we asked for is not the window we got. winit takes a
        // *logical* size and reports cursor positions in *physical* pixels, so
        // under display scaling — and on Wayland, where the compositor picks
        // the surface size regardless — the viewport would be sized in one
        // unit and driven by events in another.
        //
        // Everything downstream is physical: the surface, `tile_rect`, and the
        // shader's window vector. So the viewport is re-fitted to the real
        // surface here, and `Resized` keeps it honest from then on.
        let (width, height) = backend.size();
        self.view = Viewport::fit(self.slide(), width, height);
        self.window = Some(window);
        self.backend = Some(backend);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(ref c) if c == "f" => {
                        self.view = Viewport::fit(self.slide(), self.view.width, self.view.height);
                        self.redraw();
                    }
                    _ => {}
                }
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging = state == ElementState::Pressed;
            }

            WindowEvent::CursorMoved { position, .. } => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "window coordinates; screen space is f32 by definition"
                )]
                let next = ScreenPoint::new(position.x as f32, position.y as f32);
                if self.dragging {
                    self.view = self.view.pan(
                        f64::from(next.x - self.cursor.x),
                        f64::from(next.y - self.cursor.y),
                    );
                    self.redraw();
                }
                self.cursor = next;
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // A wheel notch is 1.0 in `LineDelta`; trackpads report pixels.
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(position) => position.y / 40.0,
                };
                // Zoom about the cursor, not the window centre — the invariant
                // `zoom_holds_the_anchor_still` pins.
                self.view = self.view.zoom_about(1.2_f64.powf(steps), self.cursor);
                self.redraw();
            }

            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                self.view = self.view.resize(size.width, size.height);
                if let Some(backend) = self.backend.as_mut() {
                    backend.resize(size.width, size.height);
                }
                self.redraw();
            }

            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }

    /// Sleeps when the frame is complete, and ticks while it is not.
    ///
    /// Without this the window would draw once per input event and never again,
    /// so tiles arriving from the loader would land in the cache and stay
    /// invisible until the next mouse move. The wait is a deadline rather than
    /// `Poll` so an incomplete frame costs a wake-up every 8 ms rather than a
    /// spinning core.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.incomplete {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(8),
            ));
            self.redraw();
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

impl App {
    fn slide(&self) -> &Slide {
        self.source.slide()
    }

    fn redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn render(&mut self) {
        let started = Instant::now();
        let slide = self.slide();
        let level = self.view.level(slide).clone();
        let underlay = self.view.underlay_level(slide).cloned();
        let tiles = self.view.visible_tiles(&level);
        let underlay_tiles = underlay
            .as_ref()
            .map_or_else(Vec::new, |level| self.view.visible_tiles(level));

        let view = self.view;
        let Some(backend) = self.backend.as_mut() else {
            return;
        };

        // Take whatever arrived since the last frame. This is the only
        // per-tile work left on the main thread: the decode, the crop and the
        // RGBA expansion all happened on a worker.
        for loaded in self.loader.drain() {
            backend.store(loaded);
        }

        // Underlay first in both lists: it is drawn first so the fine tiles
        // paint over it, and it is *requested* first so the fallback is there
        // before the tiles it stands in for.
        //
        // The draw list is rectangles, not instances or blits, and that is what
        // keeps the two backends the same viewer: everything above this point
        // is shared, and the rectangle is the last thing both of them agree on.
        let mut draws: Vec<Draw> = Vec::new();
        let mut missing: Vec<TileIndex> = Vec::new();
        for (tiles, level) in [
            (&underlay_tiles, underlay.as_ref().unwrap_or(&level)),
            (&tiles, &level),
        ] {
            for &tile in tiles {
                if backend.touch(tile) {
                    draws.push(Draw {
                        tile,
                        rect: view.tile_rect(tile, level),
                    });
                } else {
                    missing.push(tile);
                }
            }
        }
        draws.truncate(MAX_TILES);

        // Re-requesting an unchanged set would cancel the queue the loader is
        // already working through, once per frame, and it never would finish.
        if missing != self.requested {
            self.requested.clone_from(&missing);
            self.loader.request(missing);
        }
        // One question, asked after draining. This used to be `busy before ||
        // busy after`, to cover a tile that landed between the two: a worker
        // released its claim after handing the tile over, so between the drain
        // and this line a finished tile was in no set `is_idle` looked at and
        // the window went back to sleep holding an incomplete frame.
        // `is_idle` now counts what has been sent and not drained, so the
        // window either has the tile or is told to come back for it.
        self.incomplete = !self.loader.is_idle();

        backend.present(&draws);

        // Timed after the present, so the number covers the whole frame. Kept
        // as a short window and averaged: a single frame is dominated by
        // whatever the scheduler did to it, and a mean over half a second is
        // what a hand on the mouse actually perceives.
        self.frame_times
            .push_back(started.elapsed().as_secs_f64() * 1000.0);
        while self.frame_times.len() > 30 {
            self.frame_times.pop_front();
        }
        #[expect(clippy::cast_precision_loss, reason = "at most 30 samples")]
        let mean = self.frame_times.iter().sum::<f64>() / self.frame_times.len() as f64;

        let title = format!(
            "qupath-viewer — {} — level {} of {}, {:.3} px/px, {} tiles +{} under, {} MB, {} loading, {mean:.1} ms",
            backend.name(),
            level.index,
            self.source.slide().levels().len(),
            view.scale,
            tiles.len(),
            underlay_tiles.len(),
            backend.resident_bytes() >> 20,
            self.loader.pending() + self.loader.in_flight(),
        );
        if let Some(window) = self.window.as_ref() {
            window.set_title(&title);
        }
    }
}

impl Backend {
    /// The GPU path if an adapter can be had and `--cpu` was not given, the
    /// software one otherwise.
    ///
    /// Three attempts, in order, and the last one always succeeds. A real
    /// adapter; then `force_fallback_adapter`, which is Mesa's lavapipe where it
    /// is installed and is worth trying because it keeps the shader path
    /// exercised; then [`compose`], which needs no driver at all. This used to
    /// be `expect("adapter")`, and a machine with no Vulkan or GL driver — a
    /// headless server, a VM, an ssh session — got a panic at startup.
    fn new(window: &Arc<Window>, budget: usize, force_cpu: bool) -> Self {
        if !force_cpu {
            if let Some(graphics) = pollster::block_on(gpu(window, budget)) {
                println!("  drawing on the GPU");
                return Self::Gpu(Box::new(graphics));
            }
            println!("  no GPU adapter; drawing on the CPU");
        }
        Self::Cpu(Box::new(software(window, budget)))
    }

    /// What to call this in the title bar.
    fn name(&self) -> &'static str {
        match self {
            Self::Gpu(_) => "gpu",
            Self::Cpu(_) => "cpu",
        }
    }

    /// The surface size in physical pixels, which is what the viewport is sized
    /// in.
    fn size(&self) -> (u32, u32) {
        match self {
            Self::Gpu(state) => (state.config.width, state.config.height),
            Self::Cpu(state) => (state.frame.width(), state.frame.height()),
        }
    }

    /// Bytes of decoded tile resident, GPU textures or main memory.
    fn resident_bytes(&self) -> usize {
        match self {
            Self::Gpu(state) => state.resident.bytes(),
            Self::Cpu(state) => state.resident.bytes(),
        }
    }

    /// Takes a decoded tile into whichever cache this backend draws from.
    fn store(&mut self, loaded: Loaded) {
        let bytes = loaded.pixels.len();
        match self {
            Self::Gpu(state) => {
                let bind_group = upload(state, &loaded.pixels, loaded.width, loaded.height);
                state.resident.insert(loaded.tile, bind_group, bytes);
            }
            Self::Cpu(state) => {
                let tile = Tile {
                    pixels: loaded.pixels,
                    width: loaded.width,
                    height: loaded.height,
                };
                state.resident.insert(loaded.tile, tile, bytes);
            }
        };
    }

    /// Marks a tile as used now, returning whether it is resident.
    fn touch(&mut self, tile: TileIndex) -> bool {
        match self {
            Self::Gpu(state) => state.resident.touch(tile),
            Self::Cpu(state) => state.resident.touch(tile),
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        match self {
            Self::Gpu(state) => {
                state.config.width = width;
                state.config.height = height;
                state.surface.configure(&state.device, &state.config);
            }
            Self::Cpu(state) => state.frame.resize(width, height),
        }
    }

    /// Draws the list back to front and puts it on the screen.
    fn present(&mut self, draws: &[Draw]) {
        match self {
            Self::Gpu(state) => present_gpu(state, draws),
            Self::Cpu(state) => present_cpu(state, draws),
        }
    }
}

/// The GPU path: one instance per tile, then one draw call each.
fn present_gpu(state: &mut Graphics, draws: &[Draw]) {
    let Ok(frame) = state.surface.get_current_texture() else {
        return;
    };

    #[expect(
        clippy::cast_precision_loss,
        reason = "window pixels; screen space is f32 by definition"
    )]
    let window = [state.config.width as f32, state.config.height as f32];

    // Every instance is written in one copy *before* the pass opens.
    // `write_buffer` queues its copy ahead of the whole submission rather
    // than at the point it appears in the recording, so interleaving writes
    // with draws would leave every tile reading the last rectangle written
    // — one tile drawn, the rest stretched over it.
    let instances: Vec<Instance> = draws
        .iter()
        .map(|draw| Instance {
            rect: [draw.rect.0, draw.rect.1, draw.rect.2, draw.rect.3],
            window,
            _padding: [0.0; 2],
        })
        .collect();
    state
        .queue
        .write_buffer(&state.instances, 0, bytemuck::cast_slice(&instances));

    let target = frame.texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(state.config.format),
        ..Default::default()
    });
    let mut encoder = state
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tiles"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&state.pipeline);
        // The pipeline declares the instance data as vertex buffer 0;
        // leaving it unbound is a validation error, not a blank frame.
        pass.set_vertex_buffer(0, state.instances.slice(..));

        // In draw-list order, which is underlay before fine: back to front,
        // so a fine tile that has arrived covers the coarse one standing in
        // for it, and one that has not lets the coarse one show through.
        // `peek` rather than `get` because the pass holds the cache
        // immutably; the uses were all counted when the list was built.
        for (slot, draw) in draws.iter().enumerate() {
            let Some(bind_group) = state.resident.peek(draw.tile) else {
                continue;
            };
            pass.set_bind_group(0, bind_group, &[]);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "bounded by MAX_TILES, which is far below u32::MAX"
            )]
            let slot = slot as u32;
            pass.draw(0..4, slot..slot + 1);
        }
    }
    state.queue.submit([encoder.finish()]);
    frame.present();
}

/// The software path: the same list, blitted in the same order.
///
/// Back to front for the same reason and by the same mechanism — a later tile
/// overwrites an earlier one, which on this path is literally what the loop
/// does.
fn present_cpu(state: &mut Software, draws: &[Draw]) {
    state.frame.clear(compose::CLEAR);
    for draw in draws {
        let Some(tile) = state.resident.peek(draw.tile) else {
            continue;
        };
        state
            .frame
            .draw_tile(draw.rect, &tile.pixels, tile.width, tile.height);
    }

    // One copy of the composed frame into the buffer the window server reads.
    // Composing straight into it would save the copy and would mean holding the
    // surface borrowed across the whole draw; at 3 MB for a 1024x768 frame the
    // copy is well under a millisecond and not what a pan would stutter on.
    //
    // Both backends resize their surface every frame; softbuffer's is a
    // no-op when the size has not changed, and doing it here rather than in
    // `resize` means the buffer's length always matches the framebuffer's.
    if let Err(error) = present_cpu_inner(state)
        && !state.reported
    {
        state.reported = true;
        eprintln!("could not present a frame: {error}");
    }
}

fn present_cpu_inner(state: &mut Software) -> Result<(), softbuffer::SoftBufferError> {
    let (Some(width), Some(height)) = (
        NonZeroU32::new(state.frame.width()),
        NonZeroU32::new(state.frame.height()),
    ) else {
        return Ok(());
    };
    state.surface.resize(width, height)?;
    let mut buffer = state.surface.buffer_mut()?;
    buffer.copy_from_slice(state.frame.pixels());
    buffer.present()
}

fn upload(state: &Graphics, rgba: &[u8], width: u32, height: u32) -> wgpu::BindGroup {
    let texture = state.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tile"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    state.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    state.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("tile"),
        layout: &state.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&state.sampler),
            },
        ],
    })
}

/// Builds the software backend, which cannot fail for want of a driver.
fn software(window: &Arc<Window>, budget: usize) -> Software {
    let context = softbuffer::Context::new(window.clone()).expect("display");
    let surface = softbuffer::Surface::new(&context, window.clone()).expect("surface");
    let size = window.inner_size();
    Software {
        surface,
        frame: Framebuffer::new(size.width.max(1), size.height.max(1)),
        resident: TileCache::new(budget),
        reported: false,
    }
}

/// Builds the GPU backend, or `None` when there is no adapter to build it on.
///
/// Every failure here is a `None` rather than a panic, because the caller has
/// somewhere to go: a machine with no Vulkan or GL driver still gets a viewer.
/// The one thing that is *not* silently tolerated is a driver that produces a
/// device and then cannot make a pipeline — that would be a broken GPU path
/// rather than an absent one, and it should be seen.
async fn gpu(window: &Arc<Window>, budget: usize) -> Option<Graphics> {
    let instance = wgpu::Instance::default();
    let surface = instance.create_surface(window.clone()).ok()?;

    let options = wgpu::RequestAdapterOptions {
        compatible_surface: Some(&surface),
        ..Default::default()
    };
    let adapter = match instance.request_adapter(&options).await {
        Ok(adapter) => adapter,
        // Mesa's lavapipe where it is installed. Slower than the blit in
        // `compose` for what this viewer draws, but it keeps the real shader
        // path exercised, which is worth one attempt before giving it up.
        Err(_) => instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                force_fallback_adapter: true,
                ..options
            })
            .await
            .ok()?,
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .ok()?;

    let size = window.inner_size();
    let mut config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1))?;
    // The shader writes plain sRGB values, so the surface must interpret them
    // as sRGB or every tile comes out washed out.
    config.format = config.format.add_srgb_suffix();
    surface.configure(&device, &config);

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("tile"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });

    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("tile"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let instances = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: (MAX_TILES * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("tile"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("tile"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Instance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x2],
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment"),
            targets: &[Some(config.format.into())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    Some(Graphics {
        surface,
        device,
        queue,
        config,
        pipeline,
        layout,
        sampler,
        resident: TileCache::new(budget),
        instances,
    })
}

/// A textured quad per instance, positioned in window pixels.
///
/// The quad's four corners come from the vertex id, and the instance supplies
/// the screen rectangle — so the CPU sends geometry as `(x, y, w, h)` in the
/// same units [`Viewport::tile_rect`] produces, with no matrix in between.
/// `v` runs top-down so tiles are drawn the way they are stored.
const SHADER: &str = r"
struct Instance {
    @location(0) rect: vec4<f32>,
    @location(1) window: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(@builtin(vertex_index) index: u32, instance: Instance) -> VertexOutput {
    var out: VertexOutput;
    let u = f32(index & 1u);
    let v = f32((index >> 1u) & 1u);

    // Window pixels, then to clip space: x right, y down.
    let pixel = instance.rect.xy + vec2<f32>(u, v) * instance.rect.zw;
    let clip = pixel / instance.window * 2.0 - vec2<f32>(1.0, 1.0);
    out.position = vec4<f32>(clip.x, -clip.y, 0.0, 1.0);
    out.uv = vec2<f32>(u, v);
    return out;
}

@group(0) @binding(0) var tile: texture_2d<f32>;
@group(0) @binding(1) var tile_sampler: sampler;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(tile, tile_sampler, in.uv);
}
";
