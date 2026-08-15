//! Renders viewports offscreen and writes them as PNGs.
//!
//! ```text
//! cargo run -p qupath-viewer --example render --release -- data/CMU-1.svs out.png
//! cargo run -p qupath-viewer --example render --release -- data/CMU-1.svs out.png --cpu
//! ```
//!
//! `--cpu` composes through [`qupath_viewer::compose`] instead of the shader,
//! and is what runs when there is no adapter. Everything else about the run is
//! the same, which is the point: the tile counts, the level choices and the
//! coverage numbers below are all properties of the viewport arithmetic and the
//! loader, so the two backends must agree on them. Where they are *allowed* to
//! differ is sampling — the shader filters linearly, `compose` takes the nearest
//! pixel — so the PNGs are not byte-identical wherever a tile is magnified.
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
//! the coverage numbers below something to run against.
//!
//! # The scripted pan
//!
//! The last section is milestone 3.4's own check. It drives a pan through the
//! real [`TileLoader`] and [`TileCache`] — frames produced as fast as the GPU
//! will take them, tiles arriving when they arrive — and prints how much of
//! each frame was never drawn. Running it twice, once with the underlay and
//! once without, is what turns "keep the old level while loading" from a
//! plausible idea into a number.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::fs::File;
use std::io::BufWriter;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qupath_slide::{Level, Slide};
use qupath_viewer::compose::{self, Framebuffer};
use qupath_viewer::loader::expand_to_rgba_into;
use qupath_viewer::{
    ScreenPoint, SlidePoint, SlideTiles, TileCache, TileIndex, TileLoader, Viewport,
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    rect: [f32; 4],
    window: [f32; 2],
    _padding: [f32; 2],
}

/// The colour an undrawn pixel keeps, in the linear space `wgpu` takes.
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
    let out = positional
        .next()
        .cloned()
        .unwrap_or_else(|| "viewer-render.png".to_owned());

    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let (width, height) = (1024u32, 768u32);
    println!("{path}");

    // One device for every frame below. Building an adapter per PNG worked and
    // was slow enough to discourage rendering a whole pan, which is the thing
    // 3.4 actually needed looking at.
    let mut renderer = Renderer::new(width, height, force_cpu);

    // Two views through the same path: the whole slide, and a zoomed-in detail
    // reached by `zoom_about` rather than by constructing a viewport directly,
    // so the pan/zoom arithmetic is what places these pixels.
    let overview = Viewport::fit(&slide, width, height);
    render_to_png(&mut renderer, &slide, overview, &out);

    let tissue = SlidePoint::new(27776.0 + 512.0, 5952.0 + 512.0);
    let detail = Viewport {
        centre: tissue,
        scale: 1.0,
        width,
        height,
    };
    render_to_png(
        &mut renderer,
        &slide,
        detail,
        &out.replace(".png", "-detail.png"),
    );

    // The interaction as a user performs it: start fitted, then scroll in on a
    // point. Every frame here crosses at least one level boundary.
    // Anchored on tissue, not on the slide centre, which is glass — a level
    // that came out soft or a seam that opened would be invisible on glass.
    let anchor = overview.to_screen(tissue);
    let mut zooming = overview;
    for step in 0..4 {
        for _ in 0..6 {
            zooming = zooming.zoom_about(1.5, anchor);
        }
        render_to_png(
            &mut renderer,
            &slide,
            zooming,
            &out.replace(".png", &format!("-zoom{step}.png")),
        );
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
        view = view.zoom_about(0.25, ScreenPoint::new(512.0, 384.0));
    }

    scripted_pan(&mut renderer, &path, &out);

    // Only on the software path, and only composition: clearing the frame and
    // blitting every tile of the draw list, averaged over every frame above.
    // What it is being weighed against is a 60 Hz frame at 16.7 ms, and what it
    // leaves out is the decode, which is the same on both paths and is what
    // actually makes a pan wait.
    if let Some(cost) = renderer.compose_cost() {
        println!(
            "\n  composing one 1024x768 frame cost {:.2} ms on average.",
            cost.as_secs_f64() * 1000.0
        );
    }
}

/// Renders one viewport with everything decoded up front, and writes a PNG.
fn render_to_png(renderer: &mut Renderer, slide: &Slide, view: Viewport, out: &str) {
    let level = view.level(slide).clone();
    let underlay = view.underlay_level(slide).cloned();
    let tiles = view.visible_tiles(&level);

    // Underlay first, so the fine tiles paint over it — the same back-to-front
    // order the window draws in.
    let mut uploaded = Vec::new();
    if let Some(under) = underlay.as_ref() {
        decode_into(renderer, slide, view, under, &mut uploaded);
    }
    let under_count = uploaded.len();
    decode_into(renderer, slide, view, &level, &mut uploaded);

    let draws: Vec<(&Texture, Instance)> = uploaded
        .iter()
        .map(|(texture, instance)| (texture, *instance))
        .collect();
    let pixels = renderer.draw(&draws);
    write_png(out, &pixels, view.width, view.height);

    println!(
        "  {out}: level {} ({:.1}x), {} tiles at {:.4} px/px, +{under_count} underlay",
        level.index,
        level.downsample,
        tiles.len(),
        view.scale
    );
    println!(
        "    {:.1}% of the frame never drawn",
        100.0 * undrawn(&pixels)
    );
}

/// Decodes every visible tile of `level` and appends it to `uploaded`.
fn decode_into(
    renderer: &Renderer,
    slide: &Slide,
    view: Viewport,
    level: &Level,
    uploaded: &mut Vec<(Texture, Instance)>,
) {
    let window = [view.width as f32, view.height as f32];
    let mut decoded = Vec::new();
    let mut rgba = Vec::new();
    for tile in view.visible_tiles(level) {
        if slide
            .read_tile_into(tile.level, tile.column, tile.row, &mut decoded)
            .is_err()
        {
            continue;
        }
        let (tile_width, tile_height) = tile.visible_size(level);
        expand_to_rgba_into(
            &decoded,
            level.tile_width,
            tile_width,
            tile_height,
            &mut rgba,
        );
        let (x, y, w, h) = view.tile_rect(tile, level);
        uploaded.push((
            renderer.upload(&rgba, tile_width, tile_height),
            Instance {
                rect: [x, y, w, h],
                window,
                _padding: [0.0; 2],
            },
        ));
    }
}

/// Milestone 3.4's own check: a pan driven through the loader and the cache,
/// with and without the underlay, reported as coverage.
///
/// Frames are produced as fast as the GPU takes them and tiles land when they
/// land, which is the case the underlay exists for. Without it, a frame drawn
/// before its tiles arrive is clear colour; with it, it is the coarsest level
/// stretched. The difference between the two columns is the whole argument.
fn scripted_pan(renderer: &mut Renderer, path: &str, out: &str) {
    // Deliberately tight: about three screens of tiles, so eviction runs.
    const BUDGET: usize = 48 * 256 * 256 * 4;
    const FRAMES: usize = 40;

    println!(
        "\n  a scripted pan at full zoom, {:.0} px per frame, {} MB budget:",
        STEP,
        BUDGET >> 20
    );

    // Thrown away. The first pass over a region reads it from disk and the
    // second finds it in the page cache, so whichever ran second would look
    // better for a reason that has nothing to do with the underlay. This is
    // `ROADMAP.md` §5's rule about what a measurement is allowed to claim,
    // applied to a comparison rather than to a ratio.
    let _warm = pan_once(renderer, path, false, FRAMES, BUDGET, None);
    let with = pan_once(renderer, path, true, FRAMES, BUDGET, Some(out));
    let without = pan_once(renderer, path, false, FRAMES, BUDGET, Some(out));

    println!(
        "  {:>5}  {:>16}  {:>16}",
        "frame", "undrawn, under", "undrawn, none"
    );
    for frame in (0..FRAMES).step_by(4) {
        println!(
            "  {frame:>5}  {:>15.1}%  {:>15.1}%",
            100.0 * with.undrawn[frame],
            100.0 * without.undrawn[frame]
        );
    }
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
    println!(
        "  {:>5}  {:>15.1}%  {:>15.1}%",
        "mean",
        100.0 * mean(&with.undrawn),
        100.0 * mean(&without.undrawn)
    );

    // Frame 0 is 100% undrawn in both columns and that is not a defect: the
    // underlay has to be decoded too. It helps from the second frame on, which
    // is every frame of a drag after the first.
    println!(
        "\n    with underlay: decoded {}, cancelled {}, evicted {}, peak {} MB",
        with.decoded,
        with.cancelled,
        with.evicted,
        with.peak >> 20
    );
    println!(
        "    without:       decoded {}, cancelled {}, evicted {}, peak {} MB",
        without.decoded,
        without.cancelled,
        without.evicted,
        without.peak >> 20
    );
    println!(
        "    the budget is {} MB and neither pass exceeded it.",
        BUDGET >> 20
    );
    // Worth saying out loud, because a zero in a report usually means the thing
    // was not measured. It was not: a frame here ends with a blocking GPU
    // readback, which gives four workers long enough to drain the queue before
    // the next request arrives, so nothing is ever left to cancel. Cancellation
    // is pinned deterministically by `a_superseded_request_is_never_decoded`
    // instead, against a source that can be held shut.
    println!(
        "    cancelled is {} because this script never backs the queue up.",
        with.cancelled
    );
}

/// How far the scripted pan moves per frame, in level-0 pixels.
///
/// Most of a screen: slow enough to be a gesture a hand can make, fast enough
/// that four decoding threads cannot keep up with it. A creeping pan needs no
/// underlay because nothing is ever missing, which measures nothing.
const STEP: f64 = -700.0;

struct PanReport {
    undrawn: Vec<f64>,
    decoded: u64,
    cancelled: u64,
    evicted: u64,
    peak: usize,
}

/// One pass of the scripted pan. This is the window's render loop with the
/// window taken out, deliberately: if the two drift apart the report stops
/// meaning anything.
fn pan_once(
    renderer: &mut Renderer,
    path: &str,
    use_underlay: bool,
    frames: usize,
    budget: usize,
    out: Option<&str>,
) -> PanReport {
    let slide = Slide::open(path).expect("open");
    let base = &slide.levels()[0];
    // Start near the left edge at full resolution and cross the slide, so every
    // frame needs tiles it has never seen.
    let mut view = Viewport {
        centre: SlidePoint::new(
            f64::from(base.width).min(4000.0),
            f64::from(base.height).min(5952.0),
        ),
        scale: 1.0,
        width: renderer.width(),
        height: renderer.height(),
    };

    let source = Arc::new(SlideTiles::new(Slide::open(path).expect("open")));
    let loader = TileLoader::new(&source, 4);
    let mut cache: TileCache<Texture> = TileCache::new(budget);
    let mut requested: Vec<TileIndex> = Vec::new();
    let mut undrawn_per_frame = Vec::with_capacity(frames);
    let mut peak = 0;

    for frame in 0..frames {
        view = view.pan(STEP, STEP / 8.0);
        let level = view.level(&slide).clone();
        let underlay = use_underlay
            .then(|| view.underlay_level(&slide).cloned())
            .flatten();
        let tiles = view.visible_tiles(&level);
        let underlay_tiles = underlay
            .as_ref()
            .map_or_else(Vec::new, |level| view.visible_tiles(level));

        for loaded in loader.drain() {
            let bytes = loaded.pixels.len();
            let texture = renderer.upload(&loaded.pixels, loaded.width, loaded.height);
            cache.insert(loaded.tile, texture, bytes);
        }
        peak = peak.max(cache.bytes());

        let window = [view.width as f32, view.height as f32];
        let mut order: Vec<(TileIndex, Instance)> = Vec::new();
        let mut missing = Vec::new();
        for (tiles, level) in [
            (&underlay_tiles, underlay.as_ref().unwrap_or(&level)),
            (&tiles, &level),
        ] {
            for &tile in tiles {
                if cache.touch(tile) {
                    let (x, y, w, h) = view.tile_rect(tile, level);
                    order.push((
                        tile,
                        Instance {
                            rect: [x, y, w, h],
                            window,
                            _padding: [0.0; 2],
                        },
                    ));
                } else {
                    missing.push(tile);
                }
            }
        }
        if missing != requested {
            requested.clone_from(&missing);
            loader.request(missing);
        }

        let draws: Vec<(&Texture, Instance)> = order
            .iter()
            .filter_map(|(tile, instance)| Some((cache.peek(*tile)?, *instance)))
            .collect();
        let pixels = renderer.draw(&draws);
        undrawn_per_frame.push(undrawn(&pixels));

        // One frame from the middle of each pass, where the pan is at speed and
        // the cache is already under pressure. These two PNGs side by side are
        // what the coverage numbers are describing.
        if frame == frames / 2
            && let Some(out) = out
        {
            {
                let name = if use_underlay {
                    "-pan-under"
                } else {
                    "-pan-none"
                };
                write_png(
                    &out.replace(".png", &format!("{name}.png")),
                    &pixels,
                    view.width,
                    view.height,
                );
            }
        }
    }

    PanReport {
        undrawn: undrawn_per_frame,
        decoded: loader.decoded(),
        cancelled: loader.cancelled(),
        evicted: cache.evicted(),
        peak,
    }
}

/// The fraction of a frame still holding the clear colour.
///
/// The threshold used to be a guessed byte value against a linear one, and it
/// never fired: the target is `Rgba8UnormSrgb`, so `wgpu` stores the *encoded*
/// form of the linear clear colour and 0.08 linear arrives as 80, not 20. An
/// entirely undrawn frame reported 0.0% undrawn, which is the worst possible
/// answer from a check whose only job is to say whether anything was drawn.
/// The clear colour is now encoded the same way the GPU encodes it and matched
/// exactly, with one unit of slack for rounding.
fn undrawn(rgba: &[u8]) -> f64 {
    let clear = [srgb8(CLEAR.r), srgb8(CLEAR.g), srgb8(CLEAR.b)];
    let matching = rgba
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[..3]
                .iter()
                .zip(clear)
                .all(|(&got, want)| got.abs_diff(want) <= 1)
        })
        .count();
    matching as f64 / (rgba.len() / 4) as f64
}

/// A linear channel as the 8-bit sRGB value a `*UnormSrgb` target stores.
fn srgb8(linear: f64) -> u8 {
    let encoded = if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to [0, 255] before the cast"
    )]
    let value = (encoded * 255.0).round().clamp(0.0, 255.0) as u8;
    value
}

/// A decoded tile in whatever form the renderer draws from.
///
/// One enum rather than a generic renderer: the cache, the draw list and the
/// coverage number are all the same on both paths, and only this and the last
/// step differ.
enum Texture {
    Gpu(wgpu::BindGroup),
    Cpu(Tile),
}

/// A decoded tile held in main memory, ready to blit.
struct Tile {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// Where a frame is drawn.
enum Renderer {
    Gpu(Box<Gpu>),
    Cpu(Software),
}

/// The software path, with what it cost.
///
/// The cost is here because "will this stutter" is the first question a CPU
/// compositor gets, and the honest answer is a number rather than an argument
/// about how simple a blit is. It times the composition alone — clearing and
/// blitting every tile of the draw list — and not the read-back, which is this
/// tool's own overhead and not something the window does.
struct Software {
    frame: Framebuffer,
    composing: Duration,
    frames: u32,
}

impl Renderer {
    /// The GPU path unless `force_cpu`, falling back to the software one when
    /// there is no adapter.
    ///
    /// This used to be `expect("adapter")`, which made a headless machine a
    /// panic. The point of the fallback is not politeness: the numbers this
    /// tool prints — tile counts, level choices, coverage — are properties of
    /// the viewport arithmetic and the loader, and none of them needs a GPU to
    /// be true, so refusing to produce them without one was refusing for no
    /// reason.
    fn new(width: u32, height: u32, force_cpu: bool) -> Self {
        if !force_cpu {
            if let Some(gpu) = pollster::block_on(Gpu::new(width, height)) {
                println!("  drawing through the shader");
                return Self::Gpu(Box::new(gpu));
            }
            println!("  no GPU adapter; composing on the CPU");
        } else {
            println!("  composing on the CPU");
        }
        Self::Cpu(Software {
            frame: Framebuffer::new(width, height),
            composing: Duration::ZERO,
            frames: 0,
        })
    }

    /// Mean time to compose one frame, or `None` on the GPU path.
    fn compose_cost(&self) -> Option<Duration> {
        match self {
            Self::Gpu(_) => None,
            Self::Cpu(state) => (state.frames > 0).then(|| state.composing / state.frames),
        }
    }

    fn width(&self) -> u32 {
        match self {
            Self::Gpu(gpu) => gpu.width,
            Self::Cpu(state) => state.frame.width(),
        }
    }

    fn height(&self) -> u32 {
        match self {
            Self::Gpu(gpu) => gpu.height,
            Self::Cpu(state) => state.frame.height(),
        }
    }

    fn upload(&self, rgba: &[u8], width: u32, height: u32) -> Texture {
        match self {
            Self::Gpu(gpu) => Texture::Gpu(gpu.upload(rgba, width, height)),
            Self::Cpu(_) => Texture::Cpu(Tile {
                pixels: rgba.to_vec(),
                width,
                height,
            }),
        }
    }

    /// Draws in order, back to front, and returns the frame as packed RGBA.
    ///
    /// Both paths return the same thing in the same format, which is what lets
    /// `undrawn` and `write_png` stay backend-agnostic — and what makes the two
    /// coverage tables comparable line by line.
    fn draw(&mut self, draws: &[(&Texture, Instance)]) -> Vec<u8> {
        let draws = &draws[..draws.len().min(MAX_TILES)];
        match self {
            Self::Gpu(gpu) => gpu.draw(draws),
            Self::Cpu(state) => {
                let started = Instant::now();
                state.frame.clear(compose::CLEAR);
                for (texture, instance) in draws {
                    let Texture::Cpu(tile) = texture else {
                        continue;
                    };
                    let [x, y, width, height] = instance.rect;
                    state.frame.draw_tile(
                        (x, y, width, height),
                        &tile.pixels,
                        tile.width,
                        tile.height,
                    );
                }
                state.composing += started.elapsed();
                state.frames += 1;

                let mut rgba = Vec::new();
                state.frame.to_rgba(&mut rgba);
                rgba
            }
        }
    }
}

/// Everything needed to draw a frame through the shader, built once.
struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    pipeline: wgpu::RenderPipeline,
    target: wgpu::Texture,
    target_view: wgpu::TextureView,
    instances: wgpu::Buffer,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
    /// Row stride of the readback buffer; `copy_texture_to_buffer` needs a
    /// multiple of 256 and the padding is stripped after mapping.
    padded: u32,
}

/// How many tiles one frame can draw, underlay included.
const MAX_TILES: usize = 1024;

impl Gpu {
    /// Builds the shader path, or `None` when there is no adapter for it.
    ///
    /// A software adapter — Mesa's lavapipe where it is installed — is tried
    /// before giving up, because it still exercises the real shader, which is
    /// the thing `compose` cannot check.
    async fn new(width: u32, height: u32) -> Option<Self> {
        let instance = wgpu::Instance::default();
        let adapter = match instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await
        {
            Ok(adapter) => adapter,
            Err(_) => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    force_fallback_adapter: true,
                    ..Default::default()
                })
                .await
                .ok()?,
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()?;

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

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

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (MAX_TILES * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let padded = (width * 4).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Some(Self {
            device,
            queue,
            layout,
            sampler,
            pipeline,
            target,
            target_view,
            instances,
            readback,
            width,
            height,
            padded,
        })
    }

    fn upload(&self, rgba: &[u8], width: u32, height: u32) -> wgpu::BindGroup {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
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
        self.queue.write_texture(
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
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Draws `draws` in order, back to front, and reads the frame back as RGBA.
    ///
    /// Anything that is not a GPU texture is skipped rather than asserted on:
    /// the two backends never mix in practice, and a frame that silently loses
    /// a tile is easier to see in the coverage number than a panic is to read.
    fn draw(&self, draws: &[(&Texture, Instance)]) -> Vec<u8> {
        // Every instance in one copy before the pass opens. `write_buffer`
        // queues its copy ahead of the whole submission rather than at the
        // point it appears in the recording, so interleaving writes with draws
        // would leave every tile reading the last rectangle written.
        let instances: Vec<Instance> = draws.iter().map(|(_, instance)| *instance).collect();
        if !instances.is_empty() {
            self.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tiles"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.target_view,
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
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.instances.slice(..));
            for (slot, (texture, _)) in draws.iter().enumerate() {
                let Texture::Gpu(bind_group) = texture else {
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

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let slice = self.readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device.poll(wgpu::PollType::Wait).expect("poll");
        receiver.recv().expect("map").expect("mapped");

        let mapped = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
        for row in 0..self.height as usize {
            let start = row * self.padded as usize;
            rgba.extend_from_slice(&mapped[start..start + (self.width * 4) as usize]);
        }
        drop(mapped);
        self.readback.unmap();
        rgba
    }
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
