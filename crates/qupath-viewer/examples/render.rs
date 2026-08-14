//! Renders a viewport offscreen through the GPU and writes it as a PNG.
//!
//! ```text
//! cargo run -p qupath-viewer --example render --release -- data/CMU-1.svs out.png
//! ```
//!
//! # Why this exists
//!
//! `dump` reaches its pixels with no GPU, so it cannot catch a bug in the
//! geometry or the shader. `window` exercises exactly that path but needs
//! somebody watching it. This runs the *same* tile placement through the *same*
//! shader into an offscreen texture and reads it back, so the composite can be
//! looked at on a machine with no display — and, unlike the window, checked by
//! a program.
//!
//! `docs/VIEWER.md` §6 stands: this does not turn appearance into an assertion.
//! What it does is make the appearance inspectable without a display, and give
//! the two coverage-free checks below something to run against.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::fs::File;
use std::io::BufWriter;

use qupath_slide::{Level, Slide};
use qupath_viewer::{SlidePoint, TileIndex, Viewport};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    rect: [f32; 4],
    window: [f32; 2],
    _padding: [f32; 2],
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let out = args.next().unwrap_or_else(|| "viewer-render.png".to_owned());

    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let (width, height) = (1024u32, 768u32);
    println!("{path}");

    // Two views through the same path: the whole slide, and a zoomed-in detail
    // reached by `zoom_about` rather than by constructing a viewport directly,
    // so the pan/zoom arithmetic is what places these pixels.
    let overview = Viewport::fit(&slide, width, height);
    render_to_png(&slide, overview, &out);

    let detail = Viewport {
        centre: SlidePoint::new(27776.0 + 512.0, 5952.0 + 512.0),
        scale: 1.0,
        width,
        height,
    };
    render_to_png(&slide, detail, &out.replace(".png", "-detail.png"));

    // The interaction as a user performs it: start fitted, then scroll in on a
    // point. This is the path that was reported as overlapping and unzoomable,
    // so it is rendered through the GPU rather than reasoned about — every
    // frame here crosses at least one level boundary.
    // Anchored on tissue, not on the slide centre, which is glass — a level
    // that came out soft or a seam that opened would be invisible on glass.
    let anchor = overview.to_screen(SlidePoint::new(27776.0 + 512.0, 5952.0 + 512.0));
    let mut zooming = overview;
    for step in 0..4 {
        for _ in 0..6 {
            zooming = zooming.zoom_about(1.5, anchor);
        }
        render_to_png(&slide, zooming, &out.replace(".png", &format!("-zoom{step}.png")));
    }

    // Zooming out from the detail must cross a level boundary. This prints the
    // level and the tile count at each step, which is the sequence a level bug
    // shows up in as a count that rises when it should fall.
    println!("\n  zooming out from the detail view:");
    println!("  {:>8}  {:>5}  {:>6}", "scale", "level", "tiles");
    let mut view = detail;
    for _ in 0..8 {
        let level = view.level(&slide);
        println!(
            "  {:>8.4}  {:>5}  {:>6}",
            view.scale,
            level.index,
            view.visible_tiles(level).len()
        );
        view = view.zoom_about(0.25, qupath_viewer::ScreenPoint::new(512.0, 384.0));
    }
}

fn render_to_png(slide: &Slide, view: Viewport, out: &str) {
    let level = view.level(slide).clone();
    let tiles = view.visible_tiles(&level);
    println!(
        "  {out}: level {} ({:.1}x), {} tiles at {:.4} px/px",
        level.index,
        level.downsample,
        tiles.len(),
        view.scale
    );

    let pixels = pollster::block_on(render(slide, view, &level, &tiles));
    write_png(out, &pixels, view.width, view.height);

    // Two checks that need no eyes. Neither is an assertion about how the
    // image *looks*; both are about whether it was drawn at all.
    let opaque = pixels.chunks_exact(4).filter(|p| p[3] == 255).count();
    let background = pixels
        .chunks_exact(4)
        .filter(|p| p[0] < 40 && p[1] < 40 && p[2] < 40)
        .count();
    let total = (view.width * view.height) as usize;
    println!(
        "    {:.1}% opaque, {:.1}% still clear colour",
        100.0 * opaque as f64 / total as f64,
        100.0 * background as f64 / total as f64
    );
}

async fn render(slide: &Slide, view: Viewport, level: &Level, tiles: &[TileIndex]) -> Vec<u8> {
    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .expect("adapter");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .expect("device");

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: view.width,
            height: view.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });

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
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
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
            targets: &[Some(format.into())],
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

    // Decode every visible tile and upload it, then write all the instance
    // rectangles before the pass opens — the same ordering `window` needs, and
    // for the same reason.
    let mut bind_groups = Vec::new();
    let mut instances = Vec::new();
    let mut pixels = Vec::new();
    let window = [view.width as f32, view.height as f32];
    for &tile in tiles {
        if slide
            .read_tile_into(tile.level, tile.column, tile.row, &mut pixels)
            .is_err()
        {
            continue;
        }
        let (visible_width, visible_height) = tile.visible_size(level);
        let rgba = to_rgba(&pixels, level.tile_width, visible_width, visible_height);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: visible_width,
                height: visible_height,
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
            &rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(visible_width * 4),
                rows_per_image: Some(visible_height),
            },
            wgpu::Extent3d {
                width: visible_width,
                height: visible_height,
                depth_or_array_layers: 1,
            },
        );
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        bind_groups.push(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        }));
        let (x, y, w, h) = view.tile_rect(tile, level);
        instances.push(Instance {
            rect: [x, y, w, h],
            window,
            _padding: [0.0; 2],
        });
    }

    let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: (instances.len().max(1) * std::mem::size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&instance_buffer, 0, bytemuck::cast_slice(&instances));

    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tiles"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
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
        pass.set_pipeline(&pipeline);
        pass.set_vertex_buffer(0, instance_buffer.slice(..));
        for (slot, bind_group) in bind_groups.iter().enumerate() {
            pass.set_bind_group(0, bind_group, &[]);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "one slot per visible tile; far below u32::MAX"
            )]
            let slot = slot as u32;
            pass.draw(0..4, slot..slot + 1);
        }
    }

    // Read back. `bytes_per_row` must be a multiple of 256, so the copy is
    // padded and the padding is stripped after mapping.
    let padded = (view.width * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded) * u64::from(view.height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(view.height),
            },
        },
        wgpu::Extent3d {
            width: view.width,
            height: view.height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::PollType::Wait).expect("poll");
    receiver.recv().expect("map").expect("mapped");

    let mapped = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity((view.width * view.height * 4) as usize);
    for row in 0..view.height as usize {
        let start = row * padded as usize;
        rgba.extend_from_slice(&mapped[start..start + (view.width * 4) as usize]);
    }
    drop(mapped);
    readback.unmap();
    rgba
}

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

fn write_png(path: &str, rgba: &[u8], width: u32, height: u32) {
    let file = File::create(path).expect("create png");
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("png header")
        .write_image_data(rgba)
        .expect("png data");
}

/// The same shader `window.rs` uses, so this renders what the window renders.
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
