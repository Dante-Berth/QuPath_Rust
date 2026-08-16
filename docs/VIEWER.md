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

## 0. What this is not

- **Not a QuPath replacement.** No project format, no annotation editing, no
  scripting. If those are ever wanted they are a separate decision. There is an
  application now — `crates/qupath-app`, §3.6 — and it does not change this:
  it opens a slide, looks at it, and runs detection over what is on screen.
  Nothing it finds is saved, exported or editable, which is the line.
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
# Drag to pan, scroll or +/- to zoom about the cursor, right-drag a box to zoom
# to that region, f to fit. `d` detects cells in what is on screen and `h` hides
# the outlines again; the run happens on another thread, so the view keeps
# moving while it works.
cargo run -p qupath-app --release
cargo run -p qupath-app --release -- data/CMU-1.svs

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
Tests need neither, by construction (§6) — 57 of them now, and none links
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

**The region is the view, at the level the viewer is showing it at** — not at
level 0. That is what makes the button's cost independent of the zoom, and it
is the whole reason the feature is usable: on `CMU-1.svs` a fitted view reads
level 2, where the entire slide is 5.6 Mpx and the run takes **1.24 s**, while
the same view at level 0 is 1.5 Gpx. A 1:1 view of tissue is 1.05 Mpx and
**0.38 s** for 1357 nuclei. `region_for` is a pure function of a viewport and a
level — the conversion into level pixels, the clip to the level, the cap, and a
view panned off the slide are all unit tests with no slide and no window.

The cap exists but does not fire in normal use, and the test says so rather
than the comment: the viewer reads the level that matches the zoom, so the
pixels behind a window stay near the window's own size at every scale.

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
  had to shrink is the case that would want it, and that case is currently
  unreachable from the interface.
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
