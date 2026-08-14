//! Milestones 3.1 and 3.2 of `docs/VIEWER.md`: a window, and one tile from a
//! real slide on screen.
//!
//! ```text
//! cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs
//! ```
//!
//! Escape or closing the window quits. There is no pan and no zoom — that is
//! milestone 3.3, and [`Viewport`] already computes what it needs.
//!
//! # What this is checking
//!
//! That the pixels `qupath-slide` decodes reach the screen unchanged: right
//! orientation, right colours, no padding drawn. `docs/VIEWER.md` §6 is explicit
//! that appearance is checked by looking rather than by a test, so the thing to
//! compare against is the PNG from the `dump` example, which reaches the same
//! pixels through a path with no GPU in it.
//!
//! # Why the tile is uploaded as RGBA
//!
//! `read_tile_into` produces RGB8, and no GPU accepts a three-byte texture
//! format — `wgpu::TextureFormat` has no `Rgb8Unorm`. The expansion to four
//! bytes happens once, on upload, rather than the decoder being changed: the
//! detection pipeline reads the same tiles and wants nothing to do with an
//! alpha channel.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::sync::Arc;

use qupath_slide::Slide;
use qupath_viewer::{TileIndex, Viewport};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

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

    // The tile to show: the middle of the slide at a level that fits a window,
    // so there is tissue on screen rather than the glass at the edges.
    let view = Viewport::fit(&slide, 1024, 768);
    let level = view.level(&slide).clone();
    let tile = TileIndex::new(level.columns / 2, level.rows / 2, level.index);

    let mut pixels = Vec::new();
    if let Err(error) = slide.read_tile_into(tile.level, tile.column, tile.row, &mut pixels) {
        eprintln!("tile read failed: {error}");
        std::process::exit(1);
    }

    // Only the real pixels: TIFF stores edge tiles full-sized with padding, and
    // drawing the padding is the bug `TileIndex::visible_size` exists to avoid.
    let (visible_width, visible_height) = tile.visible_size(&level);
    println!("{path}");
    println!(
        "  level {} tile {},{} — {}x{} visible of {}x{} stored",
        tile.level,
        tile.column,
        tile.row,
        visible_width,
        visible_height,
        level.tile_width,
        level.tile_height
    );
    println!("  escape or close the window to quit");

    let rgba = to_rgba(
        &pixels,
        level.tile_width,
        visible_width,
        visible_height,
    );

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        rgba,
        width: visible_width,
        height: visible_height,
        state: None,
    };
    event_loop.run_app(&mut app).expect("run");
}

/// RGB8 rows of `stride` pixels to tightly packed RGBA8 of `width` by `height`.
///
/// Drops the stored padding and adds the alpha channel the GPU requires.
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

/// Everything that needs a window to exist before it can be built.
struct Graphics {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
}

struct App {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    state: Option<Graphics>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        self.state = Some(pollster::block_on(self.init(event_loop)));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::KeyboardInput { event, .. }
                if event.logical_key == Key::Named(NamedKey::Escape) =>
            {
                event_loop.exit();
            }
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                state.config.width = size.width;
                state.config.height = size.height;
                state.surface.configure(&state.device, &state.config);
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => render(state),
            _ => {}
        }
    }
}

impl App {
    async fn init(&self, event_loop: &ActiveEventLoop) -> Graphics {
        let attributes = Window::default_attributes()
            .with_title("qupath-viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(self.width, self.height));
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
        // The shader writes plain sRGB values, so the surface must interpret
        // them as sRGB or the tile comes out washed out — the first place a
        // colour bug hides, and why `dump` exists to compare against.
        config.format = config.format.add_srgb_suffix();
        surface.configure(&device, &config);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tile"),
            size: wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 4),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
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
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                targets: &[Some(config.format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
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
            bind_group,
        }
    }
}

fn render(state: &Graphics) {
    let Ok(frame) = state.surface.get_current_texture() else {
        return;
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor {
            format: Some(state.config.format),
            ..Default::default()
        });
    let mut encoder = state
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tile"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        pass.set_bind_group(0, &state.bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
    state.queue.submit([encoder.finish()]);
    frame.present();
}

/// A full-window textured quad from a four-vertex triangle strip.
///
/// No vertex buffer: the positions are indexed out of the vertex id, which is
/// the standard trick for a quad and keeps this example to one file. `v` runs
/// top-down so the tile is drawn the way it is stored, rather than flipped —
/// the orientation check milestone 3.2 is for.
const SHADER: &str = r"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOutput {
    var out: VertexOutput;
    let u = f32(index & 1u);
    let v = f32((index >> 1u) & 1u);
    out.position = vec4<f32>(u * 2.0 - 1.0, 1.0 - v * 2.0, 0.0, 1.0);
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
