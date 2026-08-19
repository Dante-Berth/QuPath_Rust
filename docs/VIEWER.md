# Viewer

A whole-slide viewer in Rust, on top of `qupath-slide`. Separate from
`ROADMAP.md` on purpose: that file tracks parity against QuPath and throughput
on a profiled region, and **this project claims neither**. Nothing here makes
the detection pipeline faster. It is built because a viewer is the thing the
read side has been missing, and because writing one is the stated goal.

Read `MEMO.md` first if you have not. The standing constraints in `ROADMAP.md`
§5 still apply to any speed claim made here, and one of them — "assert on counts
and on recorded output, never on appearance" — is *harder* here, because the
output of this project genuinely is an appearance. §6 below says what to do
about that.

## What it does, in a paragraph

Open a whole-slide image, look at it, and count cells in what is on screen.
Panning and zooming pick the pyramid level for you, so a whole 46,000-pixel
slide costs about a hundred tiles rather than twenty thousand. Pressing `d`
runs QuPath's cell detection over the visible region on a background thread
and draws the outlines over the image. What you find can be saved as GeoJSON
that QuPath opens.

Two things it deliberately will not do: edit annotations, and write
`data.qpdata`. Both are explained in §0 and §8.8.

```sh
cargo run -p qupath-app --release -- data/CMU-1.svs
```

Drag to pan, scroll to zoom, `f` to fit the slide, `d` to detect, `o` to open.
The **View** menu opens the side panels — the slide panel holds the overview
map and the brightness and contrast controls.

## How to read this file

It is a record of decisions, not a tutorial, and it is written for whoever has
to change this code next. Each numbered section says what was built, what it
was ported from, and — most usefully — what went wrong on the way, because
that is the part that is not recoverable from the source. The §-references are
cross-links between those decisions; you can follow them or ignore them, but
nothing later depends on having read them in order.

If you are looking for something specific: §3 is the viewport and level
selection, §6 is why appearance is checked by looking rather than asserting,
§8 is the interface, and §8.8 is what is still missing.

## 0. What this is not

- **Not a QuPath replacement**, with two clauses now deliberately revised.
  What stands: no annotation editing, no scripting, and nothing here is a
  second application. What changed, both as decisions rather than as drift —
  **export** (§8.2, `geojson.rs`) and the **project format** (§8.3,
  `project.rs`). §8.3 said its item was "not to be started without settling"
  this, and it was settled: the argument that carries both is that each is a
  *specification to follow* rather than a design to invent, so a file this
  writes and QuPath opens is interoperability and not a step towards
  replacing it. "No annotation editing" is what now holds the line: nothing
  found here is editable, and a project written here carries no `data.qpdata`
  because that would mean guessing at Java serialisation.
- **Not a performance lever.** The two remaining levers are in `ROADMAP.md` §4:
  pyramid-level selection on whole slides, and touching less memory per tile.
  This project *uses* the first one but does not deliver it.
- **Not on the parity path.** No fixture recorded from QuPath governs any of
  this, because QuPath's viewer is not what is being ported.

The one real connection is §3: viewport-driven level selection is the same
`Slide::level_for_width` the whole-slide speedup needs, and a viewer is a
visual check that it is correct.

**`Viewport::level` is the level selection this project delivers**, and the
numbers are worth recording. On `CMU-1.svs` a 1024x768 view of the whole slide
picks level 2 and needs **108 tiles against level 0's 23,220** — 215x fewer,
for an image that is *better* than reading level 0 and throwing 99.5% of it
away. That is `ROADMAP.md` §0's "do less work" in its purest form. It is not a
pipeline speedup and is not claimed as one: it says the level table is wired up
correctly, which is the prerequisite for the whole-slide work.

It does **not** call `Slide::level_for_width` — §3.3 explains why, and it is
the most transferable thing this project has produced so far. That function
asks an *extent* question ("which level is at least this wide") where a viewer
needs a *resolution* one, and using it cost an 11x over-decode that no frame
looked wrong for. Its doc comment now carries the warning. Whoever picks up the
whole-slide work in `ROADMAP.md` §4 will be choosing levels too, and should
read that comment before assuming the obvious function fits.

`examples/dump.rs` prints the whole table across scales, and the overview PNG
it writes makes the other half of the argument visible — most of a real slide
is glass.

## 1. What the read side already gives us

Better than expected, and none of it needs changing to start:

- `Slide` holds a `File` and reads tiles through positioned reads, so it is
  `Sync` and a loader pool needs no lock around it. Step 3.4 depends on this
  outright: four workers share one open file and never contend.
- `Slide::read_tile_into` fills a caller-owned `Vec<u8>`, so a loader thread
  allocates nothing per tile once warm. That survived contact: `TileSource::load`
  keeps a per-worker decode scratch for exactly this reason. The one allocation
  left per tile is the packed RGBA handed to the main thread, which has to be
  given away and so cannot be reused. A recycling channel would close it and has
  not been written, because the cost has not been measured — `ROADMAP.md` §5.
- `Level` carries `columns`, `rows`, `tile_width`, `tile_height` and a computed
  `downsample` — every number the viewport math needs.
- `Slide::level_for_width` exists and nothing calls it. Step 3 was meant to be
  its first caller and turned out to be the wrong caller; §3.3 is what happened.

Known limits inherited from the crate, both fine for now:

- Baseline JPEG only. `data/CMU-1-JP2K-33005.svs` will not open, by design
  (`SlideError::UndecodableCodec`). See `ROADMAP.md` §3.
- Edge tiles come back full-sized with padding. `read_region_into` is what
  crops. A viewer drawing tile quads must either crop or clamp its texture
  coordinates — decide once, in step 2, and write down which.

## 2. Stack

- **`winit` + `wgpu`** — window and rendering. The draw call is textured quads
  in a grid; this does not need a game engine. It turned out not to need a GPU
  either — see §3.4b, which is what "textured quads in a grid" means once it is
  taken literally.
- **`egui`** for chrome. Immediate-mode suits a UI whose state is almost
  entirely derived from viewport state. `iced` is the retained-mode alternative
  and would fight harder for a custom GPU surface underneath.

  The plan said `egui-wgpu`, and that turned out to **contradict §3.4b**: it
  needs an adapter, and §7 records that a machine without one still gets the
  viewer. Both are kept instead. `egui` tessellates its whole interface to
  triangles carrying a position, a texture coordinate and a premultiplied
  colour, so `compose::Framebuffer::draw_mesh` rasterises the same interface
  with no driver installed. That is §3.4b's argument applied a second time, and
  it came out the same way: the draw call was simple enough that writing it out
  was cheaper than requiring a driver for it.

  The one thing that needed care is the **fill rule**. A quad is two triangles
  sharing a diagonal, and on an axis-aligned quad at integer coordinates the
  pixel centres along that diagonal land *exactly* on it. Three inclusive edge
  tests draw those pixels twice, which puts a bright diagonal through every
  translucent panel; three exclusive ones draw them zero times and leave a
  hairline. The top-left rule decides it, and both cases are pinned — the
  adjacent-panel test puts a shared border at `x = 4.5` on purpose, because at
  an integer boundary no pixel centre ever lands on the edge and a rasteriser
  can be wrong while looking correct on every rectangle it is shown.
- **A thread pool + channels** for tile loading, not async. Decode is
  CPU-bound; what is actually needed is a request queue with **cancellation**,
  because panning makes half the in-flight requests garbage before they land.
  Built in §3.4 as `loader::TileLoader`, and the shape held: cancellation is
  "replace the queue", which needs no generation check on the worker side
  because a dropped request never reaches one.

Note the workspace already depends on rayon, and the loader must **not** be a
`par_iter` nested inside anything (`ROADMAP.md` §5: "Do not nest `par_iter`").
A dedicated pool for tile loading, separate from the detection pool, is the
safe shape.

## 2b. Running it

`qupath-app` is the application; the rest are the library's examples, which
take a slide path defaulting to `data/CMU-1.svs`.

```sh
# The application. A toolbar, a file dialog, a status bar, and a message when a
# slide will not open rather than an exit. A path on the command line is opened
# at startup; without one it comes up asking for a slide.
#
# Drag to pan -- the view is held on the slide, §3.8 -- scroll or +/- to zoom
# about the cursor, right-drag a box to zoom to that region, f to fit. `d`
# detects cells in what is on screen and `h` hides the outlines again; the run
# happens on another thread, so the view keeps moving while it works. The View
# menu opens the analysis pane, the slide panel — which carries the overview
# map and the brightness controls (§8.4, §8.7) — and the mouse/keyboard help.
cargo run -p qupath-app --release
cargo run -p qupath-app --release -- data/CMU-1.svs

# The application's own panels, rendered to PNGs with no window and no GPU.
# This is §6's "check appearance by looking" applied to the interface rather
# than to the image; §8.7 lists what it caught. Without a slide path it renders
# the empty states, which are what a first-time user meets.
cargo run -p qupath-app --example chrome --release
cargo run -p qupath-app --example chrome --release -- data/CMU-1.svs

# The viewer. Drag to pan, scroll to zoom, f to fit, escape to quit. The title
# bar carries the backend, the level, the tile count, resident texture memory,
# what the loader is still working on, and the mean cost of the last thirty
# frames on the main thread -- the level selection and the cache made visible.
# Zooming out should drop the tile count, never raise it, and a long pan should
# leave resident megabytes flat.
#
# The optional second argument is the texture budget in MB. It is the one knob
# worth sweeping: set it low and the frame still draws, because the underlay
# covers what was evicted.
cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs 256

# The same window with no GPU: no adapter, no driver, no shader. This is also
# what runs by itself when there is no adapter to be had (§3.4b).
cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs 256 --cpu

# No window, no GPU: writes the whole slide at the level the viewport picks,
# plus 1024x1024 at level 0 from the profiled region, then prints the level
# table across scales.
cargo run -p qupath-viewer --example dump --release -- data/CMU-1.svs

# No window: renders the same geometry the window renders into an offscreen
# image and writes it as a PNG, including a zoom sequence anchored on tissue
# that crosses every level boundary, and a scripted pan driven through the real
# loader and cache. This is what to run when the window looks wrong and you
# need numbers instead of an impression. `--cpu` here too, and running it both
# ways is the cross-check §3.4b rests on.
cargo run -p qupath-viewer --example render --release -- data/CMU-1.svs

cargo test -p qupath-viewer
```

`--release` matters for both: a debug build decodes JPEG tiles slowly enough to
be annoying.

**The window needs a display. Nothing here needs a GPU.** It runs on Vulkan
here; where there is no adapter at all the window composes on the CPU instead
and says so, which used to be a panic at startup. `render` needs neither a
display nor an adapter, which is the combination a headless machine usually has.
Tests need neither, by construction (§6) — 96 of them now, and none links
`wgpu`. The integration tests need `data/CMU-1-Small-Region.svs` and return
early without it.

## 3. Milestones

Each one is something that can be looked at. No step is finished until the
thing on screen is correct, not merely present.

### 3.1 Window, wgpu, clear to a colour — **done**

`examples/window.rs`. winit 0.30 and wgpu 26, which is the pairing that
resolves cleanly; winit's 0.31 is a beta. Vulkan on the Intel RPL-P here.

### 3.2 One hardcoded tile on screen — **done**

`examples/window.rs` draws the centre tile of the level `Viewport::fit` picks,
as a textured quad from a four-vertex strip with no vertex buffer.

Two things this settled that the plan had left open:

- **Edge-tile padding.** Decided as §1 said to: `TileIndex::visible_size`
  returns the cropped extent and the upload drops the padding, rather than the
  reader changing. `an_edge_tile_reports_only_its_real_pixels` pins it.
- **RGB8 reaching the GPU.** No GPU takes a three-byte texture — `wgpu` has no
  `Rgb8Unorm` — so the upload expands to RGBA once. The decoder is untouched;
  the detection pipeline reads the same tiles and wants no alpha channel. The
  surface format also needs `add_srgb_suffix`, without which the tile is
  visibly washed out, and that is the colour bug §3.2 was written to catch.

`examples/dump.rs` is the reference to compare against: same pixels, no GPU.

### 3.3 Pan, zoom, and level selection — **done**

Drag pans, scroll zooms about the cursor, `f` refits, and the level follows the
scale. `Viewport` gained `pan`, `zoom_about`, `resize`, `to_screen`,
`to_slide` and `tile_rect`; the window example draws every visible tile rather
than one. Still synchronous and still stuttery, which is 3.4's problem.

Three bugs, and the shape of them is the point — **none was visible in the
window, and two were found by rendering the same geometry offscreen and reading
back the numbers.**

- **The vertex buffer was declared and never bound.** A validation error, so it
  crashed rather than drawing something plausible. The cheapest kind.
- **Instance writes were interleaved with draws.** `write_buffer` queues its
  copy ahead of the whole submission rather than at the point it appears in the
  recording, so every tile would have read the *last* rectangle written — one
  tile drawn and the rest stretched over it. All instances are now written
  before the pass opens.
- **The level criterion was extent, not resolution.** See below.

#### The level bug, which is the one worth keeping

`Viewport::level` asked `Slide::level_for_width` for `base.width * scale`,
demanding that a level's *whole extent* cover the slide at screen scale. That
is the same condition algebraically and it fails on the pyramid's own rounding:
at scale 0.2536 a 46000 px slide wants 11666 px, level 1 offers 11500, and the
level is rejected over a **1.4% shortfall**. So the viewer jumped from level 2
straight to level 0 and decoded **221 tiles where 20 would do** — an order of
magnitude of pointless work, to show a window of glass.

It also made the level a function of *how the scale was reached*: zooming out
took scale 0.2536 to level 1, zooming in took the same scale to level 0.

The fix compares `Level::downsample` against the resolution the screen wants,
with a 5% tolerance — below what is visible, and far under the 4x gap between
adjacent levels, so it can never promote a level two steps too coarse. A first
attempt at a 1% tolerance on the *width* comparison did not fix it, because the
shortfall was 1.4%; the number was guessed instead of measured, which is the
`ROADMAP.md` §5 failure mode about ratios, one level down.

`level_for_width` is not wrong and was not changed. It answers "which level is
at least this wide", which is right for reading a region of known size and
wrong for "what does a screen need". Its doc comment now says so.

Two tests pin it, and neither is about appearance:
`the_level_is_the_same_whichever_way_the_scale_is_reached` walks a zoom ladder
in both directions and demands the same answer at every rung, and
`the_level_never_costs_more_tiles_than_the_one_below_it` forbids a coarser
level from costing more tiles than a finer one.

### 3.4 Async loading, LRU cache, keep-old-level-while-loading — **done**

Where it starts feeling like a viewer rather than a demo. All three pieces are
in the library, none of them needs a GPU, and all three carry tests.

- **`loader::TileLoader`** — a pool of decoding threads fed by a queue that
  `request` **replaces** rather than appends to. That is the cancellation: a
  fast pan decodes the frame it stopped on, not every frame it passed through,
  and the only work that survives is the one tile per worker already claimed.
- **`cache::TileCache<T>`** — an LRU with a hard byte budget. Resident bytes
  never exceed the budget after any operation, which is the assertion §6 asks
  for. Generic over the value, so the window stores a `wgpu::BindGroup` and the
  tests store nothing at all through the same eviction arithmetic.
- **`Viewport::underlay_level`** — the coarsest level, drawn beneath the chosen
  one, back to front. Not a texture-coordinate trick: each tile is drawn at its
  own level's `tile_rect` and the finer ones cover the coarser ones because
  they come later.

The RGBA expansion moved onto the worker along the way. It is per-pixel work on
a tile that is still warm in that thread's cache, and doing it between frames is
exactly what makes a pan stutter.

#### Why the coarsest level and not the next one down

Both were plausible; the coarsest wins on two counts, and both come from it
being the cheapest thing in the pyramid. A whole-screen view of it is a handful
of tiles at any zoom, so it survives the pan that evicts everything else. And
having exactly *one* fallback level means the underlay is already loaded by the
time it is needed — the next-level-down policy cannot promise that, because
that level is the one the previous frame was still loading.

#### What it is worth, as a number

`render`'s scripted pan is the check, and it is the reason that example exists.
It drives a pan through the real loader and cache, once with the underlay and
once without, and reports how much of each frame was never drawn. On `CMU-1.svs`
at 700 px per frame with a 12 MB budget:

| Pass | Mean frame never drawn | Tiles decoded | Peak resident |
| --- | --- | --- | --- |
| with underlay | **7.3%** | 489 | 12 MB |
| without | **57.6%** | 479 | 12 MB |

The 57.6% is one frame of latency made visible: a 700 px step across a 1024 px
window leaves most of the screen new, and the tiles requested this frame arrive
after it is drawn. Frame 0 is 100% in both columns and that is not a defect —
the underlay has to be decoded too. It helps from the second frame on, which is
every frame of a drag after the first.

#### Three bugs, all about *when* state becomes visible

None was arithmetic, and the second two would have been invisible to a reader.

- **A worker released its claim on a tile before sending the result.** Between
  the two the tile was neither in flight nor delivered, so a redraw landing in
  that window re-queued a tile that was already decoded and on its way.
  `a_stationary_view_decodes_each_tile_once` caught it decoding ten tiles for a
  nine-tile view. The claim is now released after the hand-off, and the render
  loop asks "is the loader busy" *before* draining for the same reason.
- **A tile that failed to decode was never remembered**, so a caller driving
  requests from what is not resident would ask for it again every redraw,
  forever — a busy core and a frame rate that quietly halves, with nothing on
  screen to say why. Failures are recorded and refused.
- **The comparison above had no warm-up pass**, and reported the underlay as
  *worse*. The first pass over a region reads it from disk and the second finds
  it in the page cache, so whichever ran second looked better for a reason that
  had nothing to do with the feature. That is `ROADMAP.md` §5's rule about what
  a measurement may claim, applied to a comparison instead of a ratio: the
  confound was the measurement, not the thing measured.

#### What is honestly not shown

`cancelled` comes out **0** in that report, and the report says so rather than
letting a zero read as a working feature. A frame there ends with a blocking GPU
readback, which gives four workers long enough to drain the queue before the
next request arrives, so nothing is ever left to cancel. Cancellation is pinned
deterministically by `a_superseded_request_is_never_decoded` instead — one
worker, a source held shut on command, and 500 queued tiles that must come out
as 4 decodes rather than 503.

The "neither stutters visibly" half of the original done-condition is checked by
looking, and is stated that way rather than dressed up. What *is* measured: the
window loads and draws the fit view of `CMU-1.svs` in 0.24 core-seconds and then
consumes **zero** CPU until the next input, so the redraw-while-loading tick
sleeps rather than spins.

### 3.4b Composing without a GPU — **done**

Not on the original list. It came from a question — *what does a user with no
GPU do?* — and the answer turned out to be worth more than the question.

`request_adapter(...).expect("adapter")` made a machine with no Vulkan or GL
driver a panic at startup, and that is not a rare machine: a headless server, a
VM with no passthrough, an ssh session, anything locked down. There are now
three attempts and the last one always succeeds — a real adapter, then
`force_fallback_adapter` for Mesa's lavapipe where it is installed, then
`compose::Framebuffer`, which needs no driver at all. `--cpu` skips to the third
on purpose, so the software path can be looked at on a machine that has a GPU.

**The reason it is written out rather than delegated to lavapipe** is that this
viewer's entire draw call is axis-aligned textured quads: no rotation, no
blending, no depth. That is a scaled blit. Pushing it through a software Vulkan
stack costs the user a driver install and buys nothing, where the blit is one
source read per destination pixel.

Two decisions, both recorded because either could have gone the other way:

- **Coverage of a destination pixel is decided by its centre.** That makes
  tiling exact by construction rather than by a rounding rule that has to be got
  right twice: one tile's `span_end` is the next tile's `span_start`, the same
  expression. It is also, it turns out, exactly the rule a GPU rasterises a quad
  with — which is why the cross-check below comes out as cleanly as it does.
- **Sampling is nearest**, where the shader filters linearly. It makes the 1:1
  case exact rather than approximately exact, and at the scales level selection
  produces — a tile drawn between 1x and about 1.05x its own size — there is
  nothing for a filter to do. Where it shows is the underlay, magnified 16x or
  more, which reads as blocky rather than soft while the real tiles load. That
  is a placeholder either way.

#### What the two backends agree on, which is the point

`render` takes `--cpu`, so the same run can be made twice. Two rasterisers that
share no code agree on the whole report: the scripted pan's undrawn fraction
frame by frame and in the mean (7.3% against 57.6%), decoded, evicted, peak
resident, every level choice and tile count in the zoom ladder.

They agree at the pixel too. On the detail view — level 0, scale 1.0, integer
offsets — the two frames are **byte-identical across all 786,432 pixels**.

| Frame | Identical pixels | Mean difference |
| --- | --- | --- |
| detail, 1:1 | **100.0%** | 0 |
| pan, 1:1 fractional offset | 87.8% of drawn | 0.32 / 255 |
| overview, minified 2.8x | 72.3% | 1.48 / 255 |
| zoom, magnified 32x | 1.0% | 5.20 / 255 |

The gradient down that table is the sampling difference and nothing else: the
more a tile is scaled, the more linear filtering has to do. Nothing is
misplaced at any scale, which is the claim that matters — coverage is identical
on every frame.

**One unit, measured rather than assumed.** Undrawn pixels differ by exactly one
in blue. The correct sRGB encoding of 0.09 linear is 84.62, `compose::CLEAR`
holds the correctly rounded 85, and the Intel Vulkan driver here stores 84.
Pinning the constant to 84 would pin it to one driver's rounding, so it stays
correct and `render`'s GPU-side check keeps its one unit of slack. Worth writing
down because it is the same family as §6's sRGB bug — a colour that was guessed
rather than encoded — one rounding step further in.

#### What it costs

Composing one 1024x768 frame — clearing it and blitting every tile of the draw
list — costs **1.27 ms** averaged over 86 frames, against 16.7 ms for a 60 Hz
frame. That excludes the read-back, which is `render`'s overhead and not the
window's, and it excludes the decode, which is identical on both paths and is
what actually makes a pan wait. Presenting adds one 3 MB copy into `softbuffer`'s
buffer; composing straight into it would save that and would mean holding the
surface borrowed across the whole draw, which is not a trade worth making at
this size.

Nothing here is parallelised, and it does not need to be yet. If it ever does,
the rows are independent.

### 3.5 Overlays — **done**

Detected objects drawn over the slide, in slide coordinates, culled to the
viewport. The half of it nobody had costed was upstream: **nothing in the
repository could hand the viewer object geometry.** `qupath_pipeline::run`
returned counts, and `Candidate`'s mask was private and built only for the few
percent of objects that reach a tile seam. So this milestone begins in the
pipeline and ends in the compositor.

#### Getting objects out of a run

Three pieces, in the pipeline crate and none of them the viewer's business.

- **`contour`** traces a filled region to the polygon that bounds it. It follows
  pixel **corners**, not centres, so one pixel comes back as its unit square
  rather than as a degenerate point — and that is what makes the shoelace area
  *exactly* the filled pixel count instead of approximately it. That identity is
  the whole test suite: an outline wrong anywhere encloses the wrong number of
  pixels, with no tolerance involved. The walk prefers a right turn to going
  straight to a left turn, which walks *around* a diagonal pinch rather than
  through it — `Wand.FOUR_CONNECTED`, the connectivity every trace here uses.
- **`Detections`** is the columnar result, per `ROADMAP.md` §4. It gives
  `qupath-objects::Table` its first producer anywhere in the workspace; that
  crate was written for exactly this and then not reached for four milestones.
  Geometry is kept apart from measurements because almost nothing touches both.
- **`run_collecting`** is a second entry point rather than a flag on `run`,
  because the two have different memory contracts: `run` is bounded by the
  thread count and this one grows with the region. **322 bytes per object**,
  measured on the fixture, five sixths of it outline.

The assertion that matters is that `run` and `run_collecting` agree on every
count, field by field — so collecting is a *read* of what the pipeline already
did, not a second, differently-parameterised pass. And `Detections::len` equals
`Outcome::objects`, which is the count *after* seam duplicates are removed, so
an object whose candidate lost is gone from the geometry rather than merely
uncounted. That test asserts its own premise: the region must resolve
duplicates at all, or the check passes vacuously.

#### Drawing them

`overlay::Overlay` holds outlines in **slide** coordinates — level 0, §5's
canonical frame — because that is the only frame that survives the viewer
changing level underneath them. It knows nothing about the pipeline:
`push_level_ring` takes lattice corners and a downsample, so §7's "the library
depends on `qupath-slide` alone" is still true and `qupath-pipeline` is a
dev-dependency of the example that needs it.

`segments_into` is the same shape the tiles have — one draw list, and only the
last step differs: a `LineList` vertex buffer on the GPU, `chunks_exact(2)` and
a DDA on the CPU. Two culls, both from the bounding box alone: off screen, and
under a floor in screen pixels. The second is not an optimisation in disguise.
Zoomed out, a nucleus is a third of a pixel, and forty thousand sub-pixel rings
are a grey smear that costs more than the tiles underneath and says less.

The scan is **linear, with no spatial index**, and that is now a measurement
rather than a shrug. On the profiled region — 1272 objects, 47,358 vertices,
394 kB — culling and projecting a frame takes **183 µs**, and rasterising the
result adds **0.03 ms** to the 1.27 ms a frame already costs. An index would be
optimising 2% of composition. `M-HOTPATH` says profile first; this is what that
looks like when the answer is "don't".

| Frame | Objects drawn | Segments | Cull |
| --- | --- | --- | --- |
| detail, 1:1 | 1016 of 1272 | 37,994 | 183 µs |
| zoom 0, level 1 | 1269 | 47,318 | 235 µs |
| zoom 1, 2.9x | 156 | 5,616 | 30 µs |
| zoom 2, 32x | 6 | 208 | 7 µs |

Both backends produce that table identically, as they do the tile counts, the
level choices and the coverage.

#### Where the two backends stop agreeing, and why it is not a defect

Tiles are byte-identical across the GPU and CPU paths (§3.4b). **Outlines are
not**, and the reason is worth writing down because it looks like a bug and is
not.

On the detail frame the two agree on 90.8% of *all* pixels but on only **31.6%
of the overlay pixels**. That number reads like something is misplaced. It is
not: **99.9% of the pixels one backend lights and the other does not are one
pixel away from the other's outline**, and the offsets are dominated by
`(0, +1)` and `(+1, 0)`.

That is the whole explanation. These contours follow pixel corners, so at 1:1
every edge lies *exactly on a pixel boundary* — a horizontal edge at `y = 7.0`
runs between row 6 and row 7, equidistant from both centres. Every edge is a
rasterisation tie, and the two rasterisers break it in opposite directions:
`compose` floors, and GPU line rasterisation is implementation-defined — the
specification permits a range of rules, so two drivers may disagree here and
neither is wrong.

So the claim §3.4b makes for tiles does not extend to lines, and it is not
extended. What stays assertable is *where an outline lands*, with no GPU:
`an_outline_lands_on_the_tile_pixels_it_covers` draws a tile at 1:1, draws a
segment along the slide coordinate of one of its rows, and asserts the lit
pixels are the ones the tile put that row in — and that the row above is
untouched, so the line is one pixel wide and on the row it was asked for.

#### What this earns back for the port

`DIVERGENCES.md` D-008 is a claim about *geometry* — nucleus masks against
smoothed cell polygons — and it had never been looked at directly because the
vertices did not exist. They exist now. D-008's two halves are still both open,
and are now both **reachable**: the outlines here are the raw pixel boundary,
where QuPath interpolates, smooths, interpolates again and simplifies before
anything sees them.

### 3.6 An application — **partly done**

`crates/qupath-app`. The viewer with an interface rather than a title bar: a
File menu with a dialog and a recent list, Fit / 1:1 / zoom buttons, a
`Detect cells` button, and a status bar carrying the level, the downsample, the
cursor in slide pixels, the tiles drawn, resident megabytes, what the loader
still owes, how many outlines the frame drew of how many were found, and the
mean frame cost.

Nothing about the viewer is reimplemented — `Viewport`, `TileLoader`,
`TileCache` and `compose` come straight from the library, and what this crate
adds is chrome and an event loop. The split inside it is the one an
immediate-mode interface wants: `chrome` is a pure function of the state that
returns a `Command`, and `state` is the only thing that acts on one. "Where a
button lives" and "what a button does" do not grow into each other.

Two things an application owes that an example does not, and both are the
reason this is a separate crate rather than a flag on `window.rs`:

- **A slide that will not open is a message, not an exit.** `data/CMU-1-JP2K-33005.svs`
  puts its `UndecodableCodec` in the status bar and leaves the window up, and a
  failed open leaves the previous slide open rather than closing it.
- **No slide is a state, not a blank window.** An empty grey rectangle is the
  commonest way an idle application looks broken.

#### The idle claim, which was wrong twice

This section first recorded **0% of one core** at rest, and that was measured
with `ps`. It was wrong: the window was running at **90 frames a second and
100% of one core** on a still picture, and had been since the chrome went in.

The bug is worth stating exactly, because it is a shape that recurs.
`egui_winit::State::on_window_event` was being handed *every* window event,
`RedrawRequested` among them. `egui` answers that one with `repaint: true` — it
is the answer to a redraw request, not a reason for one — so rendering a frame
asked for the next: render, receive `RedrawRequested`, request a redraw. A loop
that needs no input to keep itself running, which is exactly what
`ControlFlow::Wait` is set to prevent.

What let it stand was the instrument. `ps` reports `%cpu` as a *lifetime
average*, and this section had already been caught by that once — the 98% a
first measurement showed really was startup decode dominating the average. The
correction drawn from it was "the number is inflated by startup", when the
lesson available was "this number cannot answer this question at all". Read
instead as a delta of `utime + stime` from `/proc/<pid>/stat` over a fixed
interval, the spin is unmissable and the fix is verifiable: 0%, sustained,
across four 3-second samples.

A measurement that agrees with what you expected is not evidence that the
instrument works. This one printed a passing grade for a milestone, and §6's
`fraction_matching` did the same thing two milestones earlier.

#### Detection, wired to a button

`Detect cells` runs `run_collecting` over what the window is showing and draws
the outlines it finds; `h` hides them without re-running, and `d` starts a run.
The pipeline call and the `Overlay` are unchanged from what §3.5 built, so
`crates/qupath-app/src/detect.rs` is two decisions and no new geometry.

**The region is the view; the level is the coarsest one that can still resolve
a nucleus** — capped at **1.0 µm/px**, which is not always the level the viewer
is drawing. This was originally the viewer's own level, and that is recorded
here rather than quietly replaced, because the reasoning for it was sound and
the thing it missed is the interesting part. Reading the viewer's level made
the button's cost independent of the zoom: on `CMU-1.svs` a fitted view reads
level 2, where the entire slide is 5.6 Mpx and the run takes **1.24 s**, while
the same view at level 0 is 1.5 Gpx.

What that got wrong is that a level cheap enough to *draw* is not always fine
enough to *measure*. Detection parameters are in microns and the pipeline
converts them with the level's pixel size, so a coarse run is arithmetically
consistent — but no conversion puts back evidence the downsampling removed. At
CMU-1's level 2 a pixel is 2.0 µm and `Micron::min_area`'s 10 µm² is under four
pixels of area, so what cleared the filters was clumps of stained tissue whose
size and contrast at that downsample resemble a nucleus at level 0. The counts
say it: a fitted whole-slide view found **1638** objects where one screenful of
tissue found **1357**, when the whole slide holds far more nuclei than one
screenful. That is a result that looks like a result, which is the failure this
file is written against.

So the rule is a *cap* rather than a fixed level 0 — a run at a moderate zoom
still reads the cheap level it always did, and 1.0 µm/px keeps the smallest
surviving object at ten pixels of area, enough for a shape rather than a blob.
On CMU-1, whose levels are 0.499, 2.0 and 8.0 µm/px, only level 0 clears it, so
a zoomed-out run there reads level 0. A slide recording no pixel size gets the
viewer's level unchanged: without one there is no way to know what any level
resolves, and `Params::default`'s assumed pixel size is deliberately not reused,
because guessing the physical scale is exactly what produces the confident wrong
answer this is meant to prevent. A 1:1 view of tissue is 1.05 Mpx and **0.38 s**
for 1357 nuclei, unchanged — at that zoom the two rules pick the same level.
`region_for` is a pure function of a viewport and a level — the conversion into
level pixels, the clip to the level, the cap, and a view panned off the slide
are all unit tests with no slide and no window.

**The tradeoff moved onto `MAX_PIXELS`.** It used to be true that the cap did
not fire, because the run read the level the viewer drew and the pixels behind a
window stayed near the window's own size at any scale —
`the_level_the_viewer_shows_keeps_a_run_under_the_cap_at_any_zoom` still asserts
that of `region_for` alone. Asking for a finer level than the view is exactly
the case that cap was written for, so it is now load-bearing rather than
theoretical. A fitted view of CMU-1 is 1444 Mpx at level 0; capped at 64 Mpx, a
run reads the middle **21%** of the slide's width, and the status bar says
"centre of the view only" when it does. That beats both detecting nothing and a
fifteen-second freeze, but it is a partial answer — and it is why detecting a
*whole* slide needs tiling rather than a bigger cap (§8.8).

**A result must never outlive the slide it was found on.** Outlines are in that
slide's coordinates, so drawing them over a different slide puts plausible
nuclei in arbitrary places — wrong in the way that looks right, which is the
failure this whole file is written against. `Detector::generation` closes it,
the same device `loader` uses for tiles: opening or closing a slide bumps the
generation, and a run that finishes afterwards is dropped unread.

Verified against a real slide at three zooms: the overlay's extent lands inside
the requested region in slide coordinates every time. That is the assertion
that catches a downsample applied wrongly, which is otherwise invisible — every
nucleus is drawn at a plausible-looking place that is not the right one.

**What is not offered:** no cancellation and no progress fraction.
`run_collecting` is one synchronous call with nothing to poll and nothing to
interrupt, so the status bar reports what is actually known — a run is going,
and how long it has been going. A bar advancing on a timer would be an
invention, and starting a second run supersedes the first rather than stopping
it.

**What is not done**, stated rather than left to be discovered:

- **No GPU backend.** The app composes on the CPU only. `egui-wgpu` 0.33 wants
  `wgpu` 27 where the examples are on 26, so the two cannot share a device
  until they are moved together; the software path needs no wgpu at all, so it
  went first. Composition is 1.27 ms against a 16.7 ms frame (§3.4b), so this
  is a missing optimisation and not a missing feature.
- **Detection cannot be interrupted**, per above. A run over a region the cap
  had to shrink is the case that would want it, and since the level cap that
  case is now reachable from the interface: any sufficiently zoomed-out run on
  CMU-1 hits it.
- **A coarse run gives coarse outlines.** A nucleus detected at level 2 is
  traced in level-2 pixels and scaled up, so it is blocky when zoomed back in.
  This is honest — the run never read the detail — but it means the outlines
  and the tiles under them can disagree in resolution, and nothing says so in
  the window beyond the level in the status line.
- **Appearance is unchecked**, as everything in §6 says it is. The window opens,
  holds its tiles, reports its numbers, draws its outlines and idles; how it
  *looks* has not been seen.

### 3.7 Zooming to a rectangle — **done**

Scroll-to-zoom and the toolbar buttons existed since §3.3. What did not was a
way to say "that region, fill the window with it", which is the gesture a
person reaches for when they can see where they want to go.

**Right-drag draws a box and releasing zooms to it.** Left-drag still pans, so
neither gesture is modal and nothing has to be unlearned; escape abandons a box
mid-drag, and a right-drag under eight window pixels is treated as a click
rather than a zoom by a factor of several hundred.

`Viewport::zoom_to_box` is the whole of the arithmetic and lives in the library
beside `fit` and `zoom_about`, so it is unit-tested with no window: the box's
own corners land back inside the window afterwards, all four drag directions
give the same view, a box the wrong shape **overshoots rather than crops**, a
degenerate box is refused instead of dividing by zero, and the clamp is the one
every other zoom obeys. Fitting rather than stretching is forced by `Viewport`
having a single `scale`; the alternative is a scale per axis and a second factor
in every conversion in that file.

#### The bug this uncovered, which was the more valuable half

Reported from use: *"when I zoom the center remains the same"*, and separately
that the toolbar buttons were the only way to zoom that worked at all.

**Scroll-to-zoom had never worked in the application.** `state::input` gated the
wheel on `egui`'s `consumed`, and `egui_winit` derives that from
`wants_pointer_input()` — which is true for a pointer anywhere over a *panel*.
The slide is a `CentralPanel`, so the answer is true over bare glass with no
widget within four hundred pixels; an empty central panel with nothing allocated
in it at all is enough. Every scroll over the slide was discarded, leaving the
toolbar's buttons as the only working control, and those anchor on the view
centre. Hence the symptom: zoom appeared permanently centre-locked.

Panning was unaffected only because `CursorMoved` is reported against
`is_using_pointer()`, which is true only while a button is already held. Two
gestures gated on two different questions is what let one work and the other
vanish silently.

The first attempted fix — changing the panel's `Sense::drag()` to
`Sense::empty()` — was wrong, and a probe said so: `wants_pointer_input` is true
over a central panel *regardless of sense*, including with nothing allocated.
The sense was never the discriminator. What separates the slide from the chrome
is geometry, so `chrome::draw` now returns `available_rect` — the strip left
after the toolbar and status bar have taken theirs — and all three mouse
gestures ask `cursor_over_slide()` instead of `consumed`. `consumed` is still
honoured for the keyboard, where "does something have focus" is a question only
`egui` can answer.

Two tests hold it: one that the reported rectangle contains the slide's middle
and excludes both strips, and one asserting that `egui` *does* claim the pointer
over a bare central panel — so that if that ever changes, the workaround is
retired deliberately rather than left as cargo.

#### Discoverability, which was the actual complaint

The gesture was invisible, which is how the bug above survived: with the wheel
dead and nothing advertising right-drag, the buttons were all there was. So the
status bar now carries `drag to pan · right-drag a box to zoom · +/−` at rest
and, while a box is being dragged, **what releasing it would do** — `zoom to
4.2x`, read from `zoom_to_box` itself rather than recomputed, because a label
predicting a different factor from the one applied is worse than no label. The
`+`/`−` keys zoom about the cursor, and the toolbar tooltips name the gesture.

**What is not done:** the box is drawn as four one-pixel segments through
`draw_segments`, so it has no fill and no dashes; and as everywhere else in §6,
how it *looks* has not been checked, only that it appears, tracks the cursor and
zooms to the region it enclosed.

### 3.8 Staying on the slide — **done**

Not on the original list, and not in §3.6's "what is not done" either, because
nobody had looked. **`pan` moved the centre with no bound**, so a drag could
take the view off the slide entirely and leave a window of clear colour with
nothing on it to say which way the slide had gone. `f` was the only way back.

The reason this is a milestone rather than a footnote is the scale of it. At the
scale `fit` picks for `CMU-1.svs`, **one second of ordinary dragging** — sixty
frames of forty pixels — takes the centre to `(-84812, -91355)` and the visible
tile count from **108 to zero**. Not a stress case: that is a normal gesture at
the zoom the application opens at.

`Viewport::clamp_to_extent` holds the centre inside the slide's extent. The
stronger rule — *the slide covers the window* — was considered and cannot hold:
zoomed out past fit the slide is smaller than the window, so no centre makes it
cover, and `Viewport::fit` itself would violate it on any slide whose aspect
ratio differs from the window's, which is every slide. The centre rule holds at
every scale, contradicts nothing, and still guarantees the slide reaches the
middle of the window from whichever side it is on.

It clamps **per axis rather than rejecting the move**, which is the difference
between an edge that guides and one that catches: a diagonal drag that has run
out of room on one axis still slides along the other, where a rejection would
freeze both and read as the viewer having hung.

Every write to `Open::view` goes through `Open::look`, which applies it. Six
call sites move the view, and applying the clamp at each of them is applying it
at five — the sixth being this bug again.

Three unit tests, all on tile counts and none on appearance, and **the two
load-bearing ones were confirmed to fail with the clamp neutralised** before
being kept. That is §6's rule about a check that has never failed, applied at
the time of writing rather than two milestones later.

#### What this says about the ones before it

The bug was found by asking what a gesture does when it is repeated, rather than
whether it works once. Every test in §3.3 and §3.7 drives its gesture a bounded
number of times from a sensible starting point, and every one of them passes on
a viewport that has no bound at all. The gap was not in the arithmetic — `pan`
is two correct subtractions — but in nobody having asked where it *ends up*.

Worth stating because it generalises: this file's tests are strong on "is the
answer right" and were, until here, silent on "can the state get somewhere it
should never be". The three tests added are the second kind.

## 4. The two things that will bite

- **Texture memory — handled, in `cache::TileCache`.** A naive "upload every
  tile you see" viewer dies within about ten seconds of panning. The budget went
  in with step 3.4 rather than after the first time it fell over, which was the
  point of writing this down in advance: it is the same failure mode as
  `map_init` in `ROADMAP.md` §5, invisible at small scale and unbounded at real
  scale. It is a **hard** budget, not a target — resident bytes never exceed it
  after any operation, and a tile larger than the whole budget is refused rather
  than allowed to evict everything and still not fit.
- **Coordinate spaces.** There are four — slide (level-0) coordinates, level
  coordinates, tile indices, and screen pixels — and mixing them is the source
  of essentially every viewer bug. Newtype them rather than passing `(f64, f64)`
  around. This is the one place in this project where the type system pays
  immediately, and §5 is the whole reason to bother.

## 5. Coordinate types, written once

Before step 3.3 introduces the second space:

```rust
struct SlidePoint { x: f64, y: f64 }   // level 0, the canonical frame
struct LevelPoint { x: f64, y: f64, level: usize }
struct TileIndex  { column: u32, row: u32, level: usize }
struct ScreenPoint { x: f32, y: f32 }
```

Conversions go through explicit methods carrying the `Level`, never through
bare arithmetic at a call site. `level.downsample` is the only scale factor;
do not re-derive it from widths.

## 6. How this gets tested, given the output is a picture

`ROADMAP.md` §5 says to assert on counts and recorded output, never on
appearance — and three real bugs in this repository produced output that looked
entirely reasonable. That constraint does not get suspended because this
project draws pictures. What it means here:

- **Viewport math is pure and gets unit tests.** Which tiles are visible for a
  given (centre, zoom, window size), which level is chosen, and every
  coordinate conversion in §5 — all testable with no GPU and no window.
- **Tile addressing gets a golden check.** The tile the viewer decides to draw
  at a screen position must be the tile `read_region_rgb` returns for the
  corresponding region. That is an assertion on bytes, and it is the one that
  catches an off-by-one in tile indexing.
- **The cache gets a budget test.** Pan a scripted path, assert resident bytes
  never exceed the budget. Written as stated, in
  `a_scripted_pan_stays_inside_the_budget`, with the budget set deliberately
  below the path's footprint so eviction actually runs — a budget test that
  passes by never filling up proves nothing, and that assertion is itself
  asserted.
- **The loader gets a fake source.** `TileSource` is a trait for one reason: a
  real JPEG decodes far too fast to still be running when the next request
  lands, so cancellation cannot be observed against a real slide. The fake can
  be held shut on command, which turns "the queue is replaced" into 4 decodes
  where 503 were queued.
- **Placement gets byte assertions, since §3.4b.** This is the concession above
  taken back. The golden check no longer stops at the tile: at 1:1 on an integer
  offset the compositor must be a *copy*, and a half-pixel anywhere in the
  sampling shows up as a shifted image. Two tiles drawn edge to edge must
  compose to the same pixels as one tile covering both, which is strictly
  stronger than `adjacent_tiles_are_placed_edge_to_edge` — that asserts the
  rectangles touch, this asserts the pixels do. And a fractional seam is swept
  over sixteen subpixel offsets, because a pan leaves tile edges on fractional
  positions essentially always and a rounding rule that works on integers is how
  a grid of clear-colour lines appears. None of it links `wgpu`.
- **What is *still* not claimed by a test is appearance.** Colours, orientation
  and smoothness are checked by looking, and that is stated rather than dressed
  up as a test. What §3.4b moved across the line is *where a tile landed*, not
  what it looks like.

### `examples/render.rs`, and what it changed about the above

The window needs somebody watching it, and `dump` reaches its pixels with no
GPU so it cannot catch a bug in the geometry or the shader. `render` runs the
*same* tile placement through the *same* shader into an offscreen texture and
reads it back as a PNG.

That does not turn appearance into an assertion, and it is not offered as one.
What it does is make the appearance **inspectable without a display**, which
turned out to matter more than expected: of the three bugs in §3.3, the
221-tile level bug was found in `render`'s printed tile counts, not by looking
at anything. The frames it writes are also how a seam or a stretched tile would
be caught — both were checked over tissue rather than glass, because a defect
on glass is invisible.

It prints one number per frame that needs no eyes: the fraction of the output
still holding the clear colour. That says whether the frame was drawn at all,
which is a weaker claim than "it looks right" and an honest one. It is also what
makes §3.4's underlay measurable, since "keep the old level while loading" is
precisely a claim about how much of a frame is undrawn.

#### The check that could not see a blank frame

Worth keeping, because it is the failure mode this whole section is about. That
coverage number was **wrong from the day it was written**, and wrong in the
reassuring direction: it compared a guessed byte threshold against the *linear*
clear colour, but the render target is `Rgba8UnormSrgb`, so `wgpu` stores the
encoded form and 0.08 linear arrives as **80**, not 20. Nothing ever matched. An
entirely undrawn frame reported `0.0% still clear colour` — the worst possible
answer from a check whose only job is to say whether anything was drawn.

It was found by running the tool on a viewport that was off the slide, noticing
it claimed a fully-drawn frame from zero tiles, and following the number rather
than the intent. The clear colour is now encoded exactly the way the GPU encodes
it and matched with one unit of slack. The lesson is not about sRGB: a check
that has never failed has never been shown to work, and this one had been
printing a passing grade for two milestones.

#### The same failure, with a different instrument

§3.6's idle-CPU number was the second instance, and it is here because two
instances make a pattern rather than an anecdote. The window was spinning at
100% of one core; `ps` reported 0%, and 0% was believed because it was the
answer expected.

The two share a shape. Neither check could *fail* — `fraction_matching`
compared against a value nothing could ever equal, and `ps` averages over a
process's lifetime so a spin that starts after a slow launch is diluted away.
Both were read as confirmations for as long as they agreed with the intent, and
in both cases the disagreement, once looked at, took minutes to find.

The rule this yields is narrower than "test more" and is worth stating: **before
trusting a measurement, know what value would have made it fail, and check that
that value is reachable.** For the coverage check, no byte value was. For `ps`,
no idle process is — its `%cpu` cannot distinguish "asleep now" from "was busy
once". A number that cannot come out wrong is not evidence.

#### A third instance, and the part the rule missed

Adding the zoom rectangle (§3.7) re-measured the idle number by the `/proc`
delta method above, and got **55%, 65%, 94%** — a spin, apparently, and one the
same method had shown as 0% a milestone earlier. The instrument was the good
one. It was still the wrong reading twice over.

The first error was sampling before the window had settled: CMU-1 takes about
thirteen seconds to finish its startup decode, and a three-second sample taken
at nine seconds is measuring the decode. The fix is not a longer sleep but a
*termination check* — poll a frame counter until it stops moving, then measure,
so the wait is derived from the program rather than guessed. The second error
was worse and is why the numbers were erratic rather than merely high: earlier
test binaries were still running. `/proc/<pid>/stat` was being read for a pid
that was contending with three other copies of the same viewer.

What the rule as stated did not cover: it asks whether the measurement can come
out *wrong*, and this one could. It does not ask whether the thing being
measured is the thing you think you are pointing at. Both errors here were of
that second kind — right instrument, right units, wrong subject. So the rule
gains a clause: **check that the number moves when the thing you are measuring
moves, and that nothing else is in the frame.** A per-thread breakdown answers
both at once, which is why it is what finally settled this: it showed every
thread at 0% and the main thread in `ep_poll`, which no amount of aggregate
percentage could have distinguished from a busy process.

The idle claim, measured after all of that on a quiet machine with the frame
counter confirmed stopped: **0% across four three-second samples**, main thread
in state `S`.

## 7. Open questions

- **Where the crate lives — settled.** `crates/qupath-viewer`, and the
  dependency worry was answered rather than accepted: winit, wgpu, softbuffer
  and png are **dev-dependencies**, needed only by the examples that open a
  window. The library itself depends on `qupath-slide` alone, so `cargo test -p
  qupath-viewer` builds none of the graphics tree. That falls out of §6 — if
  the tests need no GPU, the library does not need to link one. §3.4b did not
  change it: `compose` draws pixels and still depends on nothing.
- **What a user with no GPU does — settled, §3.4b.** They get the viewer. The
  question was asked expecting an answer about fallbacks and produced a piece of
  the library instead, because the draw call was simple enough that writing it
  out was cheaper than delegating it. The unexpected part is that it paid §6
  back: two independent rasterisers over the same draw list is a check neither
  path could perform alone.
- **Whether `qupath-slide` needs a tile cache of its own — settled, no.**
  `cache::TileCache` sits above it and the detection pipeline streams. §3.4
  built the viewer's cache and wanted nothing from the reader, which is the
  answer this was waiting for.
- **`missing_docs` and `unsafe_code = "deny"` — settled, both stay on.** Four
  milestones in, wgpu has needed no unsafe from us and every public item carries
  a doc comment. The thread pool in §3.4 was the one place it looked like it
  might bite, and it did not: `Slide` is already `Sync`, and `Mutex` plus
  `Condvar` needed nothing beyond safe code.
- **Whether the loader should recycle its output buffers.** Open. Each delivered
  tile is one fresh allocation of packed RGBA, because the buffer is given to
  the main thread and cannot be reused. A return channel would close it. It has
  not been measured, so it has not been written.

## 8. Following QuPath's interface

Asked directly: *take the Java that gives QuPath its interface and do the same
in Rust.* This section is the answer, and it starts with the measurement,
because the measurement changes the plan.

### What the Java actually is

| | Lines |
| --- | --- |
| `qupath-gui-fx` | **101,712** |
| `ProjectBrowser.java` alone | 1,676 |
| `ImageDetailsPane.java` | 1,200 |
| `PathClassPane.java` | 1,065 |
| all of `crates/qupath-app` | **1,600** |

**So it is not a translation, and this file will not pretend otherwise.** One
pane of QuPath's interface is the size of our whole application. Beyond the
size, the two are built on opposite models: JavaFX is retained-mode with
observable properties, FXML layouts, a scene graph and CSS, where `egui` is
immediate-mode and rebuilds the interface every frame from state. There is no
line-by-line correspondence to find — §2 chose immediate mode deliberately,
because this viewer's interface is almost entirely derived from viewport state.

What *is* portable, and what this section takes, is **the design**: which panes
exist, what each one holds, and — the one piece that really is a specification —
the project file format, which is JSON and has nothing to do with JavaFX.

### 8.1 Detection parameters — **done**

The finding that started this: **the pipeline already implements most of
QuPath's `WatershedCellDetection` parameters, and the application throws them
away.** `detect.rs` sets `level` and takes `Params::default()` for everything
else, so six numbers QuPath puts in a dialog are unreachable here.

| QuPath (`WatershedCellDetection`) | `Params` | Status |
| --- | --- | --- |
| Background radius | `background_radius` | implemented, not exposed |
| Sigma | `sigma` | implemented, not exposed |
| Threshold | `intensity_threshold` | implemented, not exposed |
| Max background intensity | `max_background` | implemented, not exposed |
| Minimum area | `min_area` | implemented, not exposed |
| Maximum area | `max_area` | implemented, not exposed |
| Median filter radius | — | **not implemented** |
| Cell expansion | — | **not implemented** (lib.rs §31) |
| Smooth boundaries | — | **not implemented** (lib.rs §32) |
| Split by shape | always on | not a parameter here |

The right-hand column is why this is a table rather than a list. **A control for
a parameter the pipeline ignores is worse than no control**: it reads as a knob
that does nothing, and the user's reasonable conclusion is that detection is
broken. Only the first six get controls, and the rest stay off the panel until
something implements them.

Units are **microns**, as QuPath's are, which is what made §8.0 below a
prerequisite rather than a nicety. Each slider shows the **pixel value it
converts to on this slide** beside it, because that is what actually runs — and
because an area divides by the *square* of the pixel size where a length divides
by it once, which is worth showing rather than trusting. Without a pixel size
the controls are disabled and the panel says why, rather than offering sliders
whose µm labels the run would not honour.

`chrome::draw` still takes `&App` and still changes nothing: a slider's edit
comes back as `Command::SetTuning`, so the layout code keeps the one property
this crate is built on rather than gaining a `&mut` to everything.

Changing a parameter **does not re-run detection**. A run is about a second and
a slider produces a value every frame, so re-running on change would start dozens
of runs across one drag and show the answer to a parameter the user had already
moved past. `Detect` stays the deliberate act, as it is in QuPath's dialog.

Verified end-to-end on the profiled region, which is how a control is known to
be connected to anything: **1272** objects at the defaults — §3.5's own number —
against 23 at a minimum area of 60 µm², 851 at a threshold of 0.30, and 852 at a
sigma of 3 µm.

`a_side_panel_is_not_part_of_the_slide_rectangle` pins the layout ordering the
panels depend on: they are laid out *before* `available_rect` is read, so the
slide's rectangle excludes them. Confirmed to fail when the read is moved ahead
of the panel — which is the mistake that would compile, look right, and make
dragging a slider pan the slide underneath it. That is §3.7's bug class exactly.

### 8.0 Reading the pixel size — **done**

A correctness bug found on the way in, and the reason it is numbered before the
panel that needed it. `Params::default` hardcoded **0.4990 µm/px** — CMU-1's —
and every micron-derived default divides by it: 8 µm background radius, 1.5 µm
sigma, 10 and 400 µm² areas. On a slide scanned at 0.25 µm/px every one of those
was **out by a factor of two**, silently, with detection still returning
plausible objects. That is this file's own failure mode: wrong in the way that
looks right.

`Slide::microns_per_pixel` and `Slide::magnification` read Aperio's `MPP` and
`AppMag` from the level-0 description, which this crate already parsed and
handed out as a string. Both return `Option` rather than a default, because a
caller that needs the number has to decide what to do without one rather than be
given a guess.

Six tests, four of them on the parser as a pure function: it trims around `=`
because Aperio writes the spaces, and matches the whole key so `MPP` is not
found inside `StripeWidth`.

### 8.1b Trackpad zoom — **done**, and one half of it cannot be fixed here

Reported from use: *pinch-to-zoom on a trackpad does nothing.* Two causes, and
they are worth separating because only one was ours.

**The bug.** A wheel and a trackpad arrive through the *same* `MouseWheel`
event and are different gestures. A wheel notch is one `LineDelta` of 1.0; a
two-finger swipe is a **stream** of `PixelDelta` events carrying a few pixels
each. The pixel branch divided by 50, so an ordinary 150 px swipe came to
1.2³ spread over fifteen events — **1.2^0.06 per event**, which reads as
nothing happening at all. This had been there since §3.3 and was invisible on a
mouse, which is what it was developed with.

**The half that is not ours.** `winit` 0.30 emits `PinchGesture` on **macOS and
iOS only**, and its Wayland backend does not implement `zwp_pointer_gestures`,
so on Linux a true two-finger pinch produces **no winit event to catch**. No
code in this application can fix that; it needs winit. The arm is written
anyway, because it costs three lines and the alternative is that this works on a
Mac by accident later and nobody knows why.

What a GNOME trackpad pinch *does* send is **ctrl with a scroll**, so ctrl now
multiplies the zoom rather than gating it — a pinch zooms faster than the same
finger movement without it, and plain scroll still zooms as before. Ctrl-scroll
is the conventional zoom in browsers and editors besides, so it earns its place
independently of the pinch.

`zoom_by` is now shared by the wheel, the trackpad and the pinch so all three
agree what a step is, and it **refuses a non-finite value**: `PinchGesture`
documents that its delta may be NaN, and a NaN reaching `scale` makes every
later frame compute an empty tile list — blank, and staying blank, with `f` the
only way back.

Four tests on the step arithmetic, extracted so it asserts with no window.
`a_trackpad_scroll_zooms_visibly` asserts on the **swipe** rather than on one
event, because that is the unit a person performs, and was confirmed to fail at
the old divisor.

`examples/gestures.rs` prints what the hardware actually sends. It exists
because **what a pinch produces is platform-specific and cannot be read off the
code** — the question that settled this was answered by looking at winit's
Linux backend, and the next person should be able to look at their own device
instead.

### 8.1c Three panels at once — **done**, and it was not a cosmetic problem

§6 has said throughout that appearance is unverified, and §8.1 predicted the
rough edge as *"three panels open at once on a small window leaving little
slide"*. That was written as a matter of taste. It was a **bug**, and a silent
one.

The measurement, once a test actually ran `chrome::draw` rather than a replica of
it: with all three panels open on an **800x600** window the slide rectangle came
out **−130 pixels wide**. Not cramped — *inverted*. `cursor_over_slide` tests
`x >= left && x < right`, which no point satisfies when `right < left`, so **every
gesture over the slide stopped working**: no pan, no scroll-zoom, no zoom
rectangle. On a 1280px window it was not negative but still wrong, leaving the
slide 471px of 1280.

Two independent causes, which is why the first fix moved nothing:

- **The panels did not know about each other.** `default_width` of 260, 240 and
  280 is 780 of an 800px window. The cap is now a *share* of the window divided
  by however many panels are open, so they shrink together on a small window and
  are untouched on a large one.
- **The contents refused to be narrower.** This is the one worth keeping. An
  `egui::Grid` sizes itself to its widest **unwrapped** cell, and that intrinsic
  width **beats the panel's `max_width`**. The help pane's `"Zoom about the
  cursor. Two fingers on a trackpad."` set a floor no cap could get under, and
  capping the panels changed the numbers **not at all** until the labels wrapped.

Worth stating because it generalises past `egui`: a bound applied to a container
is not a bound on what the container is asked to hold. The cap read as correct,
the arithmetic *was* correct — `panel_cap` returned 213 for a 1280px window
throughout — and the layout ignored it. What found it was printing the resulting
rectangle rather than the intended width, which is §6's rule about knowing what
value would make a check fail.

Both halves were confirmed load-bearing separately, and the two failures are
different, which is what says neither is redundant. Remove **the cap** and the
800px window goes to a 0px slide — that half is what keeps the rectangle from
collapsing. Remove **the wrap** and the small window is fine while the 1280px one
falls back to 471px — that half is what makes the cap bind at all. Each fix
alone leaves a real failure standing.

Three tests, all on widths and none on appearance. The third —
`every_panel_reserves_width_of_its_own` — exists because the other two pass
trivially if a panel is never laid out at all, and a test satisfied by a missing
panel is not measuring the panel.

`draw_headless` runs the **real** `draw`, which is the change that made any of
this visible. `a_side_panel_is_not_part_of_the_slide_rectangle` and its
neighbours rebuild a *replica* of the layout, and a replica cannot contain a
panel's contents — so the entire class of bug above was outside what they could
see. It is kept, because it pins the layout *ordering* the replica is good for.

**Appearance is still not asserted**, and this does not claim otherwise. What
moved across the line is *how much window the slide gets*, which is a number.
Colours and legibility are still unlooked-at.

### 8.1d A scalebar — **done**, and the first piece that really is a translation

§8 said the portable part of `qupath-gui-fx` is *the design, not the code*, with
the project format as the exception. `Scalebar.java` is a second exception, and a
cleaner one: the rule for how long a scalebar should be is arithmetic over a
fixed table, with no JavaFX anywhere in it. `crates/qupath-viewer/src/scalebar.rs`
is that rule ported line for line — the first thing in this section that is a
translation rather than a re-design.

The rule, from `Scalebar.updateScalebar`: take a preferred length of **100 screen
pixels**, convert it to microns at the current downsample, snap to the nearest
entry in `permittedScales`, and convert back to find the bar's length on screen.
Snapping in *microns* rather than in pixels is the whole point — the label is a
round number and the bar is however long that number happens to measure.

**The table is copied, not generated.** It looks like a 1-2-5 progression and is
not: it also carries **250, 400 and 800**. A generated sequence would agree with
QuPath at most scales and disagree at those, which would show up as a
differently-labelled bar on the same slide — the kind of divergence that is
invisible until someone compares two screenshots. `permitted` is tested at 300 →
250 and 420 → 400 for exactly that reason.

Verified against CMU-1's real scales, which is what says the arithmetic is the
right way up rather than merely plausible:

| `scale` | Label | Bar |
| --- | --- | --- |
| 1.0 | 50 µm | 100 px |
| 0.25 | 200 µm | 100 px |
| 0.05 | 1 mm | 100 px |
| 0.0223 (fit) | **2 mm** | 89 px |
| 0.01 | 5 mm | 100 px |

The fit row is the one worth checking by hand: 46,000 px at 0.499 µm/px is about
**23 mm** of slide across the window, so a 2 mm bar at 89 of 1024 pixels is
right. A number that is merely round proves nothing here — an inverted downsample
would produce 50 µm, 20 µm, 5 µm at successive scales and every one of them would
look like a working scalebar. `zooming_out_never_claims_a_shorter_length` is the
test that catches it, and it has to compare *across* two scales to do so: this is
§3.3's level bug in miniature, where each frame was individually defensible.

**Where it goes, and why not where QuPath puts it.** QuPath anchors its scalebar
over the bottom-left of the image itself (`QuPathViewerPlus`, an `AnchorPane`).
Here it is in the status bar instead — a bar floating over the slide would sit
inside the slide's rectangle, and §8.1c is the record of what lands there. It
also gives the `px/px` already in the status bar a physical meaning, which was
the gap.

**What the table does past a real slide.** `permittedScales` stops at 100,000 µm,
so zoomed out far enough the bar cannot grow and starts under-reporting — 23 px
at a scale of 0.0001. That is QuPath's behaviour, not a defect in the port, and
it is **pinned rather than fixed**: extending the table would be diverging from
QuPath to serve a scale no slide reaches. It was found by sweeping past the
reachable range, which is why the sweep test's lower bound is now a stated
quarter of `Viewport::fit`'s scale rather than an arbitrary small number — a
bound chosen by taste would have hidden it.

Ten tests, none needing a window: the library function is pure, so `scalebar` is
`Option<f64>` and `f64` in, a length and a string out. It refuses a non-finite or
non-positive scale for §8.1b's reason — a NaN reaching a label is the same class
of bug as a NaN reaching the viewport.

### 8.2 The panes QuPath has and this does not

Taken from the interface rather than from the code, in the order they are worth
having.

- **Slide metadata — done.** `ImageDetailsPane` is 1,200 lines there and about
  seventy here: pixel size, magnification, and the level table with the level
  currently being shown marked, which is where the status bar's level number
  gets its meaning. Everything it shows is parsed by §8.0 or by `Slide::levels`.
  A missing value is a dash rather than a plausible default, for
  `microns_per_pixel`'s reason.
- **The overview thumbnail — done, §8.4.** `ImageOverview.java`'s two mappings,
  in `overview.rs`, and the pane in the application. It answered what §8.2 said
  it would: zoomed in past a few hundred microns there was **nothing on screen
  that said where on the slide you are**, the status bar's level-0 coordinates
  being a number rather than a position.
- **Brightness/contrast and stain vectors — the transform is done, §8.5; the
  controls are not.** `display.rs` is the per-pixel operation and
  `Framebuffer::draw_tile_with` applies it. What remains is the *dialog*: there
  is no way to move the range or pick a channel from the interface yet, so the
  transform is reachable from the library and from `examples/panes` only.
- **A scalebar — done, §8.1d.** Listed here because it is part of QuPath's viewer
  rather than a pane, and because it was the first item that was a genuine
  translation of the Java rather than a port of the design. It is no longer the
  only one: §8.4's overview and §8.5's display transform are translations too,
  and §8.6 and §8.3 follow file formats exactly. The pattern is that what
  ports cleanly is *arithmetic and formats*, and what does not is drawing.
- **A gesture and key list — done.** The complaint that started §3.7 was
  discoverability, and the status bar fixed one gesture's worth of it. QuPath has
  no equivalent pane; this one is ours. It is also where the trackpad and
  ctrl-scroll gestures of §8.1b are stated, which is the only place a user would
  find them.
- **Saving detections — done, §8.6.** `geojson.rs`, in the dialect QuPath
  reads. §0's "nothing found is saved" is revised there rather than quietly:
  the *parameters* are still not saved, and no detection is editable.

### 8.3 Projects — the format is portable, the browser is not

The tutorial that prompted this is QuPath's *Projects* page, and its figures are
`ProjectBrowser.java`: a pane of image thumbnails, right-click to add metadata,
sort by name or URI, mask names for blinded studies.

The **format is a real specification and worth following exactly**, because a
project written here that QuPath can open is worth far more than one that is
merely similar. From `DefaultProject.java` it is a directory:

```text
project.qpproj          JSON: {version, uri, images: [...]}
data/<entry-id>/
    thumbnail.jpg       a JPEG thumbnail
    server.json         how to reopen the image
    data.qpdata         the objects, QuPath's own binary
    summary.json
```

Three of those four we can write today. **`data.qpdata` is QuPath's Java
serialisation and we should not attempt it** — that is a binary format tied to
Java's object graph, and guessing at it is how a project file becomes corrupt in
a way the user discovers later. Writing the JSON parts and leaving the objects
to GeoJSON alongside is honest and useful; writing a broken `.qpdata` is neither.

The browser pane itself is ordinary: a list, thumbnails from a `TileCache`, and
`Viewport::fit` at a small size to generate them. The thumbnail generator is
already written — `examples/dump.rs` produces exactly that overview PNG.

**This is the largest item in this section and the least certain**, because
`docs/VIEWER.md` §0 says *no project format* as a non-goal. Doing this revises
that, and the revision should be deliberate: the argument for it is that a
project is how a pathologist actually works — a study is fifty slides, not one —
and the argument against is that it is the point where "a viewer" becomes "a
QuPath replacement", which §0 rules out.

**Settled, and the JSON parts are written — `project.rs`.** What carried it is
narrower than the argument above: the format is a *specification to follow*, so
following it exactly is interoperability rather than a second application. The
browser pane is **not** written and is the part that would be, and there is
still no `data.qpdata` — see §8.8.

### 8.4 The overview — the invariant is that the two mappings agree

`overview.rs` is `ImageOverview.java`'s arithmetic: slide to overview, which
places the rectangle, and overview to slide, which turns a click into a centre.
The drawing is JavaFX and stayed there.

The property worth stating is that **the two must agree** — clicking the centre
of the drawn rectangle must not move the view. Neither function alone can catch
an inverted or transposed mapping, because each is individually defensible at
any single scale; this is §3.3's level bug in the same shape as §8.1d's
scalebar, and `zooming_in_never_grows_the_rectangle` compares across scales for
exactly that reason.

Constants copied rather than chosen: `maxLength` 150, the rectangle at
`rgba(200,0,0,0.8)`, the border `rgb(64,64,64)` at width 2. At `Viewport::fit`
QuPath draws **no rectangle at all** — "in which case everything is visible" —
and the test for that is against the slide's extent rather than against a
remembered scale, because a resize or a pan reaches the same condition without
going through `fit`.

**Where it goes.** The top of the slide-metadata panel, not floating over the
image where `QuPathViewerPlus` anchors it: a picture over the slide would sit
inside the slide's rectangle, and §8.1c is the record of what lands there. The
same call the scalebar made in §8.1d.

The thumbnail is read once per slide from the **coarsest** level, which is what
makes it cheap — CMU-1's top level is 1,024 px on its long edge, so it is one
small region read rather than a decimation of level 0 — and uploaded to the
renderer when the slide changes rather than per frame, because the picture is of
the slide and not of the view.

### 8.5 The display transform — and why it cannot reach a count

`display.rs` is QuPath's `AbstractChannelInfo` arithmetic: `offset =
minDisplay`, `scale = 255 / (max - min)`, applied per channel and clamped to a
byte. Ported rather than re-derived, because "brightness and contrast" has a
dozen reasonable definitions and the one worth having is the one that agrees
with QuPath at the same settings.

**Display only, structurally rather than by intention.** A brightness control
that reached the pipeline would change every count this project has recorded,
and would do it in a way no count-to-count test would notice — the parity
fixtures would simply start disagreeing with a slider position. The separation
is that this module is in `qupath-viewer`, the detection path is in
`qupath-pipeline`, and the dependency only runs one way. `ROADMAP.md` §5's rule
about not asserting on appearance has a mirror image, which is that appearance
must not be able to move an assertion.

The stain arithmetic is **duplicated** rather than shared, because §7 keeps this
crate on `qupath-slide` alone and taking a `Stains` would pull the pipeline into
every build of the viewer for nine numbers and a logarithm.
`matches_the_pipelines_own_arithmetic` records the pipeline's own values for the
same pixels so the copy cannot drift. The coefficients are the *inverted*
matrix's row, taken from the pipeline rather than typed from the stain vectors —
the first attempt at this typed a plausible vector that was wrong, and only
comparing against the real numbers caught it.

`draw_tile_with` hoists the identity branch **out of the loop**, so the default
display is byte-identical to no display at all and §3.4b's agreement between the
two backends does not become conditional on a display being set.

What was missing was the **dialog**, and §8.7 is it.

### 8.6 GeoJSON — the part that is wrong in a way that looks right

`geojson.rs` writes what `QuPathTypeAdapters` writes, and the shape is taken
from there rather than from the GeoJSON specification. That distinction is the
whole point: GeoJSON is published, so writing something a validator accepts
takes no effort and still lands in QuPath showing nothing. `objectType` is the
camelCase spelling its reader wants — it warns on the older `object_type` — and
`measurements` is the post-0.4.0 name-to-number object rather than the array of
`{name, value}` that came before.

The correctness question is **coordinates**, and it has the §3.3 shape again: a
file written in level coordinates opens without complaint and puts every object
in the top-left corner at the wrong size.
`coordinates_are_level_zero_pixels` compares the same ring pushed at downsample
1 against downsample 4, because a single file is individually plausible whatever
frame it is in.

No serde: the workspace has no serialisation dependency, and one nesting of
arrays and a flat map of numbers is not enough reason to acquire one. Verified
by round-tripping real output through a JSON parser rather than only by the
brace-balance test, because balanced braces are exactly what a hand-rolled
writer gets right while getting the field names wrong.

### 8.7 The brightness/contrast panel — the controls, and where the range lives

**What it is, plainly.** Open the slide panel and there are three buttons —
original, haematoxylin, eosin — and a couple of sliders. The buttons switch
between the slide's own colours and the two stains pulled apart from them:
haematoxylin shows the nuclei and drops nearly everything else, eosin shows the
opposite. The sliders set which pixel values count as black and white, or for
a stain channel, how much of it counts as fully dark. None of it touches the
numbers a detection produces.

The rest of this section is why it is built the way it is.

§8.5's transform, with a hand on it.

**In the slide panel, not floating over the image.** QuPath opens this as a
dialog anchored over the viewer. Here it goes under the overview in the metadata
pane, for §8.1c's reason and the overview's and the scalebar's: anything drawn
over the slide sits inside the slide's rectangle, and what lands there is a
hit-test that stops agreeing with what is drawn. Three items have now taken this
decision the same way, which makes it the rule rather than three coincidences.

**The panel holds an `Appearance`, not a `Display`.** A `Display` with no stain
has forgotten which stain it was, and one rebuilt whenever the channel changes
has forgotten the range — so a user who dials in a range, looks at haematoxylin
and comes back finds their range gone. `Appearance` keeps the range, the channel
and the stain contrast as three independent fields and computes the `Display`,
which is what makes the controls independent of each other.
`the_commands_leave_the_display_where_the_panel_asked` asserts on where a *run*
of commands leaves the state rather than on what one returns, because a single
`SetAppearance` is individually correct in every version of this that loses the
range.

**The stain numbers are read, not typed.** `Stains::coefficients` and the new
`Stains::max_values` are what the channel buttons build from.
§8.5 records typing a plausible haematoxylin vector from the *stain* matrix
where the inverted matrix's row was wanted, and that nothing failed — the
channel looked like a working stain channel. `max_values` was exposed for the
same reason it matters: a background typed as `[255, 255, 255]` because that is
what it usually is would be right for the default stains and silently wrong for
a slide carrying its own.
`the_stain_channels_carry_the_pipelines_own_numbers` compares what the panel
builds against what the pipeline computes, which is the only check a
plausible-but-wrong vector cannot pass.

**Display only, still structurally.** The transform is applied in `render`, on
the way from the tile cache to the frame. The cache holds the slide's own
pixels, so moving a slider costs a redraw rather than a reload — and nothing
`Detector::start` is handed has been through it. The crate boundary of §8.5
stops a `Display` reaching `qupath-pipeline`; `App::display_command` is the
interface's half, and it touches two fields and no detector.

**The range sliders cannot make an impossible range.** Dragging black past white
pushes white along, as QuPath's dialog does. `Appearance::display` still falls
back to the identity for an inverted or empty range, and
`an_impossible_range_falls_back_rather_than_inverting` exercises the fallback
precisely because the sliders no longer can: a guard nothing reaches is one
nobody notices has stopped working.

**Looked at, per §6.** `examples/panes` now renders the "full black" slider at
both ends, which is the one control §8.5's pictures did not already cover. At
0.3 the nuclei saturate to solid black against near-white stroma; at 3.0 the
same field is uniformly pale with the nuclei as faint grey. Both are what the
control is for, and a slider whose two ends looked alike would be the bug.

**And the panel itself has now been looked at**, which §6 asks for and which
nothing in this project had done for any pane. `examples/chrome` lays the
interface out with `egui` and rasterises it through `Framebuffer::draw_mesh` —
no window, no GPU, the same call the application makes on a machine with no
adapter (§3.4b), so what it writes is what that machine draws.

Four things it found, none of which a test here would have:

- **The analysis panel's slider labels were clipped.** `Slider::text` lays the
  name out *inline*, so the widget plus its text is what has to fit a panel
  that `panel_cap` binds — "Background radius" showed as "Bac". Every slider
  now puts its name on the line above. This is §8.1c's shape exactly: contents
  that refuse to be narrower than the cap that binds them, and the layout tests
  asserted on rectangles that were all correct.
- **A `▸` in the empty-state text rendered as `□`.** No glyph in the default
  atlas. Written and read back within the hour.
- **Turning a control on inside a closed panel did nothing visible.** The
  overview and the brightness controls both live in the slide panel, which is
  closed by default, so ticking either left the user having ticked a box and
  seen nothing — which reads as a broken toggle.
  `turning_on_a_control_opens_the_panel_it_lives_in` asserts the gesture rather
  than the field, because `show_appearance` being true is not the property that
  matters; `show_appearance && show_metadata` is.
- **Reset butted straight against the sliders**, reading as another row of the
  same stack rather than the thing that undoes them.

What is still unseen is the interface *in a real window* — these are offscreen
renders, and the trackpad and the window manager are not in them.

### 8.8 What is still missing

In the order it is worth having.

- **Tiled whole-slide detection.** The one gap a person meets by using the
  application rather than by reading it. "Detect cells" reads one region in one
  `run_collecting` call, so `MAX_PIXELS` bounds it, and since §3.6's level cap a
  fitted view of CMU-1 gets the middle 21% of the slide's width — labelled, but
  still a partial answer to a question that reads like a whole-slide one. The
  fix is not a bigger cap: 1444 Mpx at level 0 is a quarter-hour of pipeline at
  the measured 4.5 Mpx/s, in one uninterruptible call. It is a loop over tiles
  with overlap, deduplicating objects that straddle a seam, which is what QuPath
  does and is a real piece of work rather than a parameter change. It also wants
  the two things §3.6 lists as not offered — cancellation and a progress
  fraction — which become honest the moment there is a tile count to report, and
  are the reason this is worth doing as one piece.
- **The project browser pane.** §8.3's format is written and nothing in the
  application creates or opens a project, so `project.rs` is a library with no
  caller. Writing the pane is what would make it real; it is also the point at
  which the §0 argument deserves revisiting rather than being treated as
  settled once.
- **`data.qpdata`, which should stay missing.** Java serialisation tied to
  Java's object graph. A guessed version is how a project becomes corrupt in a
  way someone discovers much later, having trusted it. The detections belong
  beside it as GeoJSON, which both sides can read.
- **A JPEG encoder for `thumbnail.jpg`.** `project.rs` returns the JSON files
  and leaves the thumbnail to the caller, because nothing in this workspace
  encodes JPEG. QuPath tolerates its absence.
- **A look at the interface in a real window.** `examples/chrome` renders the
  panels offscreen and §8.7 records the four bugs that found, but an offscreen
  render is not a window: it has no trackpad, no window manager, no resizing
  and no hover states. The panels are no longer unlooked-at; the *running
  application* still is.
- **Parity for the two file formats.** Nothing here has been opened *by QuPath*.
  Both formats were checked against a JSON parser and against the Java that
  writes them, which is a stronger claim than "it validates" and a weaker one
  than "QuPath opened it". Until a file has been through QuPath, that is the
  honest description.
