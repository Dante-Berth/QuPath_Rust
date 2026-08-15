//! Milestones 3.1 to 3.4 of `docs/VIEWER.md`: a window showing a slide, with
//! pan, zoom, pyramid-level selection, and tiles loaded off the main thread
//! into a cache with a hard byte budget.
//!
//! ```text
//! cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs
//! cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs 64
//! ```
//!
//! Drag to pan, scroll to zoom, `f` to fit the whole slide, escape to quit. The
//! optional second argument is the texture budget in MB, which is the one knob
//! worth sweeping: set it low enough and the title bar's eviction count climbs
//! while the frame still draws, because the underlay covers what was thrown
//! away.
//!
//! The title bar carries the level, the tile count, resident megabytes and
//! what the loader is still working on — the level selection and the cache made
//! visible. Zooming out should *drop* the tile count as it crosses to a coarser
//! level, not raise it, and a long pan should leave resident megabytes flat.
//!
//! # What this is checking
//!
//! That the pixels `qupath-slide` decodes reach the screen unchanged, that a
//! level change is invisible, and that a sustained pan neither stalls on decode
//! nor grows without bound. `docs/VIEWER.md` §6 is explicit that appearance is
//! checked by looking rather than by a test, so the parts that *can* be
//! asserted live in the library and are tested there — `TileCache`'s budget,
//! `TileLoader`'s cancellation, and the viewport geometry.
//!
//! # What is not here
//!
//! Milestone 3.5, overlays. Nothing from `qupath-objects` is drawn.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use qupath_slide::Slide;
use qupath_viewer::{ScreenPoint, SlideTiles, TileCache, TileIndex, TileLoader, Viewport};
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

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let budget = args
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
        view,
        dragging: false,
        cursor: ScreenPoint::new(0.0, 0.0),
        requested: Vec::new(),
        incomplete: false,
        state: None,
    };

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut app).expect("run");
}

/// Everything that needs a window to exist before it can be built.
struct Graphics {
    window: Arc<Window>,
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

struct App {
    source: Arc<SlideTiles>,
    loader: TileLoader,
    budget: usize,
    view: Viewport,
    dragging: bool,
    cursor: ScreenPoint,
    /// What was last handed to the loader, so a redraw that wants the same
    /// tiles does not cancel and re-queue the request it is waiting on.
    requested: Vec<TileIndex>,
    /// Whether the last frame was missing a tile that is still coming. Drives
    /// the wake-up in [`ApplicationHandler::about_to_wait`].
    incomplete: bool,
    state: Option<Graphics>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let state = pollster::block_on(init(event_loop, &self.view, self.budget));
        // The window we asked for is not the window we got. winit takes a
        // *logical* size and reports cursor positions in *physical* pixels, so
        // under display scaling — and on Wayland, where the compositor picks
        // the surface size regardless — the viewport would be sized in one
        // unit and driven by events in another.
        //
        // Everything downstream is physical: the surface, `tile_rect`, and the
        // shader's window vector. So the viewport is re-fitted to the real
        // surface here, and `Resized` keeps it honest from then on.
        self.view = Viewport::fit(self.slide(), state.config.width, state.config.height);
        self.state = Some(state);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(ref c) if c == "f" => {
                        self.view =
                            Viewport::fit(self.slide(), self.view.width, self.view.height);
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
                if let Some(state) = self.state.as_mut() {
                    state.config.width = size.width;
                    state.config.height = size.height;
                    state.surface.configure(&state.device, &state.config);
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
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }

    fn render(&mut self) {
        let slide = self.slide();
        let level = self.view.level(slide).clone();
        let underlay = self.view.underlay_level(slide).cloned();
        let tiles = self.view.visible_tiles(&level);
        let underlay_tiles = underlay
            .as_ref()
            .map_or_else(Vec::new, |level| self.view.visible_tiles(level));

        // Asked *before* draining. A worker releases its claim after handing
        // the tile over, so "busy now" means "there is more coming"; asking
        // afterwards would miss a tile that landed in between and let the
        // window go back to sleep holding an incomplete frame.
        let was_busy = !self.loader.is_idle();

        let view = self.view;
        let Some(state) = self.state.as_mut() else {
            return;
        };

        // Upload whatever arrived since the last frame. This is the only
        // per-tile work left on the main thread: the decode, the crop and the
        // RGBA expansion all happened on a worker.
        for loaded in self.loader.drain() {
            let bytes = loaded.pixels.len();
            let bind_group = upload(state, &loaded.pixels, loaded.width, loaded.height);
            state.resident.insert(loaded.tile, bind_group, bytes);
        }

        // Underlay first in both lists: it is drawn first so the fine tiles
        // paint over it, and it is *requested* first so the fallback is there
        // before the tiles it stands in for.
        let mut draws: Vec<(TileIndex, Instance)> = Vec::new();
        let mut missing: Vec<TileIndex> = Vec::new();
        let window = [state.config.width as f32, state.config.height as f32];
        for (tiles, level) in [
            (&underlay_tiles, underlay.as_ref().unwrap_or(&level)),
            (&tiles, &level),
        ] {
            for &tile in tiles {
                if state.resident.touch(tile) {
                    let (x, y, width, height) = view.tile_rect(tile, level);
                    draws.push((
                        tile,
                        Instance {
                            rect: [x, y, width, height],
                            window,
                            _padding: [0.0; 2],
                        },
                    ));
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
        self.incomplete = was_busy || !self.loader.is_idle();

        state.window.set_title(&format!(
            "qupath-viewer — level {} of {}, {:.3} px/px, {} tiles +{} under, {} MB, {} loading",
            level.index,
            self.source.slide().levels().len(),
            view.scale,
            tiles.len(),
            underlay_tiles.len(),
            state.resident.bytes() >> 20,
            self.loader.pending() + self.loader.in_flight(),
        ));

        let Ok(frame) = state.surface.get_current_texture() else {
            return;
        };

        // Every instance is written in one copy *before* the pass opens.
        // `write_buffer` queues its copy ahead of the whole submission rather
        // than at the point it appears in the recording, so interleaving writes
        // with draws would leave every tile reading the last rectangle written
        // — one tile drawn, the rest stretched over it.
        let instances: Vec<Instance> = draws.iter().map(|(_, instance)| *instance).collect();
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
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.08,
                            g: 0.08,
                            b: 0.09,
                            a: 1.0,
                        }),
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
            // immutably; the uses were all counted in the loop above.
            for (slot, (tile, _)) in draws.iter().enumerate() {
                let Some(bind_group) = state.resident.peek(*tile) else {
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

async fn init(event_loop: &ActiveEventLoop, view: &Viewport, budget: usize) -> Graphics {
    let attributes = Window::default_attributes()
        .with_title("qupath-viewer")
        .with_inner_size(winit::dpi::LogicalSize::new(view.width, view.height));
    let window = Arc::new(event_loop.create_window(attributes).expect("window"));

    let instance = wgpu::Instance::default();
    let surface = instance.create_surface(window.clone()).expect("surface");
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        })
        .await
        .expect("adapter");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .expect("device");

    let size = window.inner_size();
    let mut config = surface
        .get_default_config(&adapter, size.width.max(1), size.height.max(1))
        .expect("surface config");
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

    Graphics {
        window,
        surface,
        device,
        queue,
        config,
        pipeline,
        layout,
        sampler,
        resident: TileCache::new(budget),
        instances,
    }
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
