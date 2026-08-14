//! Milestones 3.1 to 3.3 of `docs/VIEWER.md`: a window showing a slide, with
//! pan, zoom and pyramid-level selection.
//!
//! ```text
//! cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs
//! ```
//!
//! Drag to pan, scroll to zoom, `f` to fit the whole slide, escape to quit.
//! The title bar shows the level and the tile count, which is the level
//! selection made visible: zooming out should *drop* the tile count as it
//! crosses to a coarser level, not raise it.
//!
//! # What this is checking
//!
//! That the pixels `qupath-slide` decodes reach the screen unchanged, and that
//! a level change is invisible — the image must not jump or resize when the
//! viewport crosses a pyramid boundary. `docs/VIEWER.md` §6 is explicit that
//! appearance is checked by looking rather than by a test, so the geometry that
//! *can* be asserted lives in [`Viewport`] and is tested there;
//! `adjacent_tiles_are_placed_edge_to_edge` and
//! `a_coarse_tile_is_drawn_larger_than_a_fine_one` are the two that would fail
//! before a seam or a jump became visible here.
//!
//! # What this is not
//!
//! Milestone 3.4. Tiles are decoded **synchronously on the main thread** and
//! cached without a byte budget, so a fast pan stutters and memory grows with
//! the number of distinct tiles visited. Both are 3.4's job — the async loader
//! with cancellation, and the LRU with a hard budget. This example exists to
//! make the viewport math visible, not to be smooth.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::collections::HashMap;
use std::sync::Arc;

use qupath_slide::{Level, Slide};
use qupath_viewer::{ScreenPoint, TileIndex, Viewport};
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

    println!("{path}");
    println!("  drag to pan, scroll to zoom, f to fit, escape to quit");

    let view = Viewport::fit(&slide, 1024, 768);
    let mut app = App {
        slide,
        view,
        dragging: false,
        cursor: ScreenPoint::new(0.0, 0.0),
        state: None,
    };

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.run_app(&mut app).expect("run");
}

/// One decoded tile, resident on the GPU.
struct Resident {
    bind_group: wgpu::BindGroup,
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
    /// Decoded tiles, keyed by their index. **No byte budget** — that is
    /// milestone 3.4, and this grows without bound on a long pan.
    resident: HashMap<TileIndex, Resident>,
    instances: wgpu::Buffer,
}

struct App {
    slide: Slide,
    view: Viewport,
    dragging: bool,
    cursor: ScreenPoint,
    state: Option<Graphics>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let state = pollster::block_on(init(event_loop, &self.view));
        // The window we asked for is not the window we got. winit takes a
        // *logical* size and reports cursor positions in *physical* pixels, so
        // under display scaling — and on Wayland, where the compositor picks
        // the surface size regardless — the viewport would be sized in one
        // unit and driven by events in another. That mismatch puts the zoom
        // anchor off-screen and doubles every tile rectangle, which is exactly
        // the "cannot zoom, and the tiles overlap" pair it was reported as.
        //
        // Everything downstream is physical: the surface, `tile_rect`, and the
        // shader's window vector. So the viewport is re-fitted to the real
        // surface here, and `Resized` keeps it honest from then on.
        self.view = Viewport::fit(&self.slide, state.config.width, state.config.height);
        self.state = Some(state);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Character(ref c) if c == "f" => {
                        self.view = Viewport::fit(&self.slide, self.view.width, self.view.height);
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
}

impl App {
    fn redraw(&self) {
        if let Some(state) = self.state.as_ref() {
            state.window.request_redraw();
        }
    }

    /// Decodes and uploads any visible tile not already resident.
    ///
    /// Synchronous, on the main thread: this is what milestone 3.4 replaces
    /// with a loader pool and a cancellation generation.
    fn ensure_resident(&mut self, level: &Level, tiles: &[TileIndex]) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        let mut pixels = Vec::new();
        for &tile in tiles {
            if state.resident.contains_key(&tile) {
                continue;
            }
            if self
                .slide
                .read_tile_into(tile.level, tile.column, tile.row, &mut pixels)
                .is_err()
            {
                continue;
            }
            let (visible_width, visible_height) = tile.visible_size(level);
            let rgba = to_rgba(&pixels, level.tile_width, visible_width, visible_height);
            let bind_group = upload(state, &rgba, visible_width, visible_height);
            state.resident.insert(tile, Resident { bind_group });
        }
    }

    fn render(&mut self) {
        let level = self.view.level(&self.slide).clone();
        let tiles = self.view.visible_tiles(&level);
        self.ensure_resident(&level, &tiles);

        let Some(state) = self.state.as_ref() else {
            return;
        };

        state.window.set_title(&format!(
            "qupath-viewer — level {} of {}, {:.3} px/px, {} tiles",
            level.index,
            self.slide.levels().len(),
            self.view.scale,
            tiles.len()
        ));

        let Ok(frame) = state.surface.get_current_texture() else {
            return;
        };

        // Every instance is written *before* the pass opens. `write_buffer`
        // queues its copy ahead of the whole submission rather than at the
        // point it appears in the recording, so interleaving writes with draws
        // would leave every tile reading the last rectangle written — one
        // tile drawn, the rest stretched over it.
        let window = [state.config.width as f32, state.config.height as f32];
        let drawn: Vec<(usize, &TileIndex)> = tiles
            .iter()
            .enumerate()
            .filter(|(slot, tile)| {
                (*slot as u64) < MAX_TILES && state.resident.contains_key(tile)
            })
            .collect();
        for (slot, tile) in &drawn {
            let (x, y, width, height) = self.view.tile_rect(**tile, &level);
            let instance = Instance {
                rect: [x, y, width, height],
                window,
                _padding: [0.0; 2],
            };
            let offset = (slot * std::mem::size_of::<Instance>()) as u64;
            state
                .queue
                .write_buffer(&state.instances, offset, bytemuck::bytes_of(&instance));
        }

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

            // One draw per tile, each reading its own slot of the instance
            // buffer. A real viewer batches this; at a hundred tiles it does
            // not matter, and 3.4 changes the shape here anyway.
            for (slot, tile) in &drawn {
                let Some(resident) = state.resident.get(tile) else {
                    continue;
                };
                pass.set_bind_group(0, &resident.bind_group, &[]);
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "bounded by MAX_TILES, which is far below u32::MAX"
                )]
                let slot = *slot as u32;
                pass.draw(0..4, slot..slot + 1);
            }
        }
        state.queue.submit([encoder.finish()]);
        frame.present();
    }
}

/// RGB8 rows of `stride` pixels to tightly packed RGBA8 of `width` by `height`.
///
/// Drops the stored padding and adds the alpha channel the GPU requires; see
/// `docs/VIEWER.md` §3.2 for why the decoder is not changed instead.
fn to_rgba(rgb: &[u8], stride: u32, width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let start = row * stride as usize * 3;
        for column in 0..width as usize {
            let pixel = start + column * 3;
            rgba.extend_from_slice(&[rgb[pixel], rgb[pixel + 1], rgb[pixel + 2], 255]);
        }
    }
    rgba
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

/// How many tiles one frame can draw. A 4K window at the finest level needs a
/// few hundred; past this the frame is truncated rather than misdrawn.
const MAX_TILES: u64 = 1024;

async fn init(event_loop: &ActiveEventLoop, view: &Viewport) -> Graphics {
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
        size: MAX_TILES * std::mem::size_of::<Instance>() as u64,
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
        resident: HashMap::new(),
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
