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

Drag to pan, scroll to zoom, `f` to fit the slide, `d` to detect, `s` to save
what was found, `o` to open.
The **View** menu opens the side panels — the slide panel holds the overview
map and the brightness and contrast controls.
The project panel lists a study's images and searches them by name or by
metadata (`tif|stain=H&E`), orders them by any key the project carries, masks
the names for a blinded read, and right-clicks a row to edit its metadata back
into the file — §8.3, §8.3b and §8.3c. Projects can be created and slides added
to them, thumbnail and all; QuPath opens the result, reads the entries and the
edited metadata back, and opens the images.

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
- **Not on the parity path — the *viewer* is not, and that is now a narrower
  claim than this bullet used to make.** No fixture governs the drawing, the
  viewport math or the level selection, because QuPath's viewer is not what is
  being ported. But the **formats and the reader** this crate carries are
  governed by six recorded fixtures — `parity_geojson_import`,
  `parity_project_import`, `parity_project_export`, `parity_project_open`,
  `parity_openslide_levels` and `parity_smart_string_sort` — and that arrived
  the same way §8.2's export bullet did: by the file saying "no fixture governs
  any of this" for several sessions after it had stopped being true. Following
  a specification is exactly the thing a fixture can check, so every item §0
  revises above brings one with it.

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
# detects cells in what is on screen, `h` hides the outlines again and `s`
# saves them as GeoJSON; the run happens on another thread, so the view keeps
# moving while it works. The View menu opens the analysis pane, the slide panel
# — which carries the overview map and the brightness controls (§8.4, §8.7) —
# and the mouse/keyboard help.
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

#### A fifth, and it is the interface reporting on itself

The status bar says how many tiles the frame drew and how many outlines it
drew, and both are computed in `draw` — which runs **after** `chrome::draw` has
been laid out, because the commands the layout produces change what that frame
shows. So the numbers on screen are always the previous frame's.

While anything is moving that is invisible. At rest it is not, because the last
frame's numbers are never shown at all: nothing asks for another frame, so the
loop sleeps holding them. What that looks like is pressing `d`, waiting for the
run, and reading **"0 of 4027 outlined" over a screen covered in outlines** —
and it stays there until the mouse is moved, which is exactly the shape of the
three bugs above.

The fix is one flag: `draw` compares the counts it computed against the ones it
is replacing, and `wants_another_frame` reports one more frame if they differ.
It costs a frame after a change and nothing at rest, and it terminates because
the frame that reports the new numbers computes the same ones again.

Worth noting how it was found, because no test would have: the counts were
correct, the outlines were on screen, and every assertion about either passes.
It took pressing `d` in a real window and reading the status bar — and then a
probe through the real `segments_into` to rule out the culling, which is where
the number would have come from if it had been a real zero.

#### A fourth one, found later, and the example had a workaround for it

The three above were found by looking. This one was found by reading
`is_idle`, and it is the same shape: *a tile in the channel is not in any set
`is_idle` looked at.*

`delivered` exists precisely for the gap between a worker sending a tile and
the caller draining it — the comment on the field says a tile in that window is
"invisible to everything else". `is_idle` then answered `pending.is_empty() &&
claimed.is_empty()`, so the instant the last worker released its claim it said
there was nothing left. `about_to_wait` asks that question to decide whether to
request another frame, so the loop went to sleep with the tile still in the
channel and no frame coming to draw it. The underlay covers the hole, so what
it looks like is a pan that stops one tile short of sharp until the next mouse
movement — which reads as a slow decode, not as a bug.

The part worth keeping is that **`examples/window.rs` already worked around
it**. It read `is_idle` *before* draining as well as after, and the comment
explaining why describes exactly this window. So the symptom had been seen,
diagnosed correctly at the call site, and patched there — while the predicate
it was patching stayed wrong for every other caller, and `state.rs` (the
application, which asks only once and only after draining) had the bug in full.
A workaround at one call site is a bug report the code is making about itself;
this one went unread for as long as the example kept working.

`is_idle` now counts `delivered`, `window.rs` is back to asking once, and
`TileLoader::undrained` makes the set observable so the status bar's "N
loading" does not drop to zero while a tile is still owed. The test puts a tile
in the channel on purpose — wait for the workers to settle, do not drain,
assert `!is_idle()` — which is the state the old predicate called idle.

The rename it forces is worth noticing too: two tests and `tests/loading.rs`
used `is_idle` to mean "the *workers* are done", which is a different question
from "there is nothing more to draw". They ask the first one explicitly now.

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
vertices did not exist. They exist now, and the smoothing chain that runs over
them is ported and pinned stage by stage. Cell expansion has since landed too,
so **both of D-008's halves are closed**: the rule compares two smoothed cell
polygons, and both the cells and the smoothing are now computable here. What
remains is routing the pipeline through them, not building them.

### 3.5b Selecting an object, and colouring by a measurement — **done**

Click an outline to select it and read its values; colour every outline by
one measurement column with a legend — QuPath's *measurement map*. Four
pieces, and each lives where the rule in §3.5 says it should: the overlay
stays ignorant of measurements, the library knows nothing about the
application, and the application's shared files change only where wiring
requires.

| Piece | Where | What it is |
| --- | --- | --- |
| hit test | `qupath-viewer/src/pick.rs` | `object_at`, even-odd and half-open, over level-0 outlines; the centroid; the highlight's segments |
| per-object style | `Overlay::owned_segments_into`, `Framebuffer::draw_owned_segments` | the row index of each segment, from the same cull-and-project pass the plain path uses |
| colour map | `qupath-viewer/src/colour_map.rs` | QuPath's Inferno and its index rule, pinned to a recording; a percentile range |
| the pane and the click | `qupath-app/src/inspect.rs` | selection state, colour choice, the **Objects** pane |

#### The click goes through the status bar's mapping

A left press records the slide point under the pointer through
`cursor_slide_position` — the function the status bar's coordinates come
from — and a release that has not moved four physical pixels at any point is a
click. Panning is not delayed: it still starts on the first pixel, so a click
with a shaky hand pans by a pixel and still selects, and a drag that goes out
and comes back is a drag. Clicking empty slide clears; nothing is selected
while the outlines are hidden.

`a_click_selects_the_outline_under_it_at_level_two_and_a_scale_factor_of_two`
is the test, and every factor that is 1 in the development setup is not 1 in
it: CMU-1 viewed at **pyramid level 2**, a slide rectangle converted at a
**scale factor of 2**, and outlines traced at level 2's **downsample of 16**.
The two objects are 40 and 80 screen pixels apart on the two axes, the clicks
are witnessed over the slide and under the status bar's mapping, and a click
read as points instead of pixels is shown to miss. Halving the cursor, or
swapping the axes, fails all four selection tests.

#### A row index that outlives its overlay

A selection is a row number and a colour table is a colour per row, and after
a new run row 312 is another nucleus. Showing the old values beside the new
outline looks exactly like a result. `Detector::show` is now the one place an
overlay arrives, and it and `Detector::clear` bump a revision that both the
selection and the table carry; either is dropped when it moves. The selection
also carries its row's bounding box, which catches a caller that assigns
`detector.overlay` around `show` — the existing tests do exactly that.
`a_selection_does_not_survive_the_objects_it_named` repeats select → new run →
select three times, then the bypass, then a close, and checks what the pane
would read after each; three mutations (never invalidating, no bounds check,
`show` not bumping) each fail it.

#### Hit-testing a whole slide is a linear scan, and the number says so

`examples/pick_cost.rs`: 150,000 synthetic 36-vertex nuclei over a CMU-1-sized
slide — the size of a whole-slide run, with ring lengths matched to a real
one — cost **0.34 ms per click** (332–345 µs over three runs on a loaded
machine), and 4 ms at 1.5 million. A click is one event, not one per frame, so
that is a small fraction of the one frame it happens in and no spatial index
was built. An `f32` box test instead of `f64` measured 318 µs, which is inside
the noise of this machine and was not kept. Choosing a column to colour by
costs 0.55 ms for the percentile range and 0.75 ms for the per-row colours,
once per choice or run.

#### The colours are QuPath's, and that is a recording rather than a reading

The table is Inferno, QuPath's default (`ColorMaps.preferredDefaultColorMaps`
lists it first), built from QuPath's own `colormaps/Inferno.tsv` the way
`DefaultColorMap` builds it, and `index` is `ColorMaps.getInd`: ×256, Java's
half-up rounding, clamped, a reversed range read backwards.
`parity/java/ColourMap.java` records `getColor` over a 1,804-value sweep past
both ends of a range, all 256 rounding ties, a reversed range and a zero-width
one, and `tests/parity_colour_map.rs` holds every one of them exactly — no
libm is involved, so exact is the right bar. The recording found a bug on its
first run: a zero-width range sent values above it to the last entry here and
to the first in QuPath. One difference stays and is asserted by name: QuPath
gives `+∞` the *first* colour through a `long`-to-`int` wrap, this the last.

**The range is not QuPath's, deliberately.** QuPath starts at the data's
minimum and maximum, so one segmentation artefact at ten times the usual area
turns the whole field one colour. This takes the 2nd and 98th percentiles; the
legend states the range, the unit, how many objects fall beyond each end
(drawn in the end colours, as QuPath's own clamp draws them) and how many have
no value (grey, which is off the map; QuPath draws nothing for `NaN`).

The selection is **cyan and three pixels wide**, not QuPath's yellow: the
plain outlines here are yellow-green and the top of Inferno is pale yellow,
so yellow would vanish into either. It is drawn as five one-pixel copies of
the ring one pixel apart, because one-pixel lines are the only primitive both
backends share.

#### Which values, and whose

The per-cell measurement tail is not ported, so the pane lists whatever
columns the run carries — a column added later appears without the pane
changing — and tags each value *ours · not checked against QuPath*, with its
provenance on hover:

- `Nucleus: Area (px)` — the nucleus mask's pixel count, in level-0 pixels.
  QuPath's nucleus area is calibrated and is not ported. The masks behind the
  count are compared with QuPath's (D-009); this column is not.
- `Nucleus: Haematoxylin OD mean` — the background-subtracted haematoxylin
  OD the intensity threshold tests. Not QuPath's `Hematoxylin OD mean`, which
  is part of the unported tail.
- The **centroid** — the traced outline's centre of area, in px and in µm
  when the slide has a pixel size. QuPath reports its smoothed ROI's centroid;
  not compared.

A perimeter is *not* offered, and that is a decision: the traced outline
follows pixel corners, so its length is a staircase — about 4/π of a round
nucleus's true perimeter, and √2 of a diagonal edge's. That is a number that would look like
QuPath's `Perimeter` and would not be it.

#### Where the two backends agree, measured

`examples/render.rs` hands both backends one colour per segment: the CPU path
writes it, the GPU path carries the sRGB bytes per vertex and inverts the sRGB
transfer in the shader so the target encodes them back. On the detail view of
CMU-1 (1:1, level 0):

| | Result |
| --- | --- |
| frames with no outlines | byte-identical, all 786,432 pixels |
| plain outlines, pixels both light | 18,831 of 18,831 the same colour |
| coloured outlines, GPU colours the CPU never drew | 0 of 50,177 |
| coloured outlines, pixels both light in different colours | 186 of 18,888 |
| pixels one backend lights and the other does not | 72,040, of which 72,005 within a pixel of the other's outline |

The last row is §3.5's rasterisation tie, unchanged. The 186 are the same tie
met from the other side, and all 186 were checked: each is a pixel where two
touching outlines meet, and the other backend's colour sits one pixel away in
each frame. Which object's line lands on a shared pixel decides which colour
is on top, and that is the tie deciding overdraw order, not a colour being
computed differently. The colour computation itself — byte to linear to sRGB
— is exact on this driver: not one outline pixel off the CPU's palette.

#### What is not verified

- **A real click in the real window.** The application was launched on
  CMU-1-Small-Region at `WINIT_X11_SCALE_FACTOR=2` and photographed, but
  there is no `xdotool` here to deliver a press, and moving the user's own
  pointer on their desktop is not something to do unasked. What the tests
  reach is everything from a physical pixel position to the selection —
  `cursor_moved` and `left_button` are out of `input` for that — and what they
  do not reach is the two-line match arm in `input` that calls them, the same
  residue §8.8 names for escape.
- **The legend and the pane were looked at, not asserted.** `examples/chrome`
  writes `chrome-objects-{plain,mean,area,selected}.png` from a real run over
  CMU-1-Small-Region; the colours of the pixels are asserted in
  `colouring_changes_colours_and_selecting_changes_one_object`, the words are
  not.
- **The dark end of Inferno is hard to see on a haematoxylin-dark nucleus.**
  That is what a perceptually uniform sequential map costs, and QuPath's
  default pays it too; it is said rather than fixed.

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

#### A hole that stays a hole, now said out loud

`TileLoader::failed` counts tiles that could not be decoded, and its own
documentation says the number is "worth seeing rather than watching redraw
silently". Nothing read it.

The failure it counts is permanent by design: a tile that fails is never asked
for again, because a caller drives requests from what is *not* resident and
would otherwise retry it on every redraw. So the square stays as it is, the
underlay is drawn under it, and — because a failed tile is not outstanding —
the spinner beside the status bar's tile counts is gone. What a person sees is
a frame that looks finished with one region softer than the rest, which reads
as a slow load that stalled rather than as a file that cannot be read there.

The status bar now says "N unreadable" in the warning colour when there are
any, with a tooltip saying they are not retried. Zero on a slide `qupath-slide`
can open at all, since the codec check is per slide rather than per tile, so
the label is absent in the ordinary case.

The loader half is pinned — `a_failed_tile_is_not_held_as_delivered` and
`a_tile_that_cannot_decode_is_not_asked_for_again` — and the label itself is
§6's "checked by looking", which is the honest description of a string in a
status bar.

#### The File menu took the window down, for forty-five commits

`rfd` was added as

```toml
rfd = { version = "0.15", default-features = false, features = ["xdg-portal", "tokio"] }
```

and every file dialog in the application **panicked the process** the instant
it was opened:

```text
thread 'main' panicked at zbus-5.19.0/src/abstractions/executor.rs:217:9:
there is no reactor running, must be called from the context of a Tokio 1.x runtime
```

`pick_file` and `save_file` are `rfd`'s *blocking* API — which is what a modal
dialog on the event-loop thread wants — and it runs the portal call through
`pollster::block_on`. The `tokio` feature builds `ashpd` and `zbus` against
tokio, whose primitives require an ambient runtime, and nothing here is inside
one. `async-std` is `rfd`'s own default for exactly this reason; the
`default-features = false` line turned the default off and picked the
combination that cannot work.

So **File → Open slide… and `o` crashed the viewer**, from the commit that
introduced the application until now. Opening a slide by any means other than
the command line was impossible.

**Why nobody noticed, which is the whole lesson.** Every way this project
exercises the application avoids the dialog. `examples/chrome` renders the
panels with no event loop. The unit tests build an `App` and never press
anything. The window was launched — when it was launched at all — with a path
on the command line, because that is how a developer with a slide in `data/`
starts it. The one path that reaches `rfd` is a person clicking a menu item,
and §8.8's "the running application is still unlooked-at" is exactly that gap,
still being paid for.

It also cannot be caught by a test in the ordinary way: the call blocks on a
real portal and puts a window on a real desktop. What found it was running the
binary and reading `stderr` — and the fix is verified the same way, with a
probe that calls `pick_file` and is killed after thirty seconds instead of
panicking in under one.

The lesson generalises past this crate: **a dependency's feature flags are
code, and `default-features = false` is a decision to re-derive them.** Turning
the defaults off and naming two features looks conservative and is the opposite
— it is opting out of the combination the crate's author tested, which here was
the only one that works.

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
the case that cap was written for, so it became load-bearing rather than
theoretical.

At 64 Mpx it was also too low, and §8.9 is what measuring it properly found: the
rate it was set from was taken on glass and on too few tiles to fill the thread
pool, and the memory it was guarding was never a function of the region in the
first place. It is now 2048 Mpx. A fitted view of CMU-1 is 1514 Mpx at level 0
and runs whole — all **382,432** nuclei in **33.5 s** — rather than the middle
21% of the slide's width it used to return. The cap still fires on anything
larger, shrinking about the centre with the status bar saying "centre of the
view only" when it does.

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

**Cancellation and a progress fraction, since §8.10.** For most of this
milestone neither was offered, and the reason was good: `run_collecting` was one
synchronous call with nothing to poll, so the status bar reported the one thing
it knew — a run is going, and how long it has been going — because a bar
advancing on a timer would have been an invention.

What changed is what is knowable, not the rule. `run_collecting_watched` takes a
`Progress` handle the pipeline writes two atomics into, and the denominator is
the run's own tile count, fixed before the first tile starts. So the bar is
tiles finished over tiles total: a measurement, in jumps, with no finish
estimate beside it because tiles over glass and tiles over tissue cost quite
different amounts. `Stop` in the status bar and escape both end a run, within
about one tile — measured at 1.45 s on a whole-slide level-0 run. Starting a
second run still supersedes the first; stopping is the different gesture, and it
leaves no successor.

**What is not done**, stated rather than left to be discovered:

- **No GPU backend.** The app composes on the CPU only. `egui-wgpu` 0.33 wants
  `wgpu` 27 where the examples are on 26, so the two cannot share a device
  until they are moved together; the software path needs no wgpu at all, so it
  went first. Composition is 1.27 ms against a 16.7 ms frame (§3.4b), so this
  is a missing optimisation and not a missing feature.
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

  **One narrower thing did cross it, and the distinction is worth keeping.**
  Whether a colour is the right one is appearance. Whether *two* rows that
  behave differently are drawn the same is not — it is a claim about the
  interface saying what it does, and it is decidable.
  `only_a_row_that_opens_fills_under_the_pointer` (§8.8) asserts on the
  `Shape::Rect` the browser emits rather than on a rasterised picture, and it
  exists because looking at two pictures found the fill was identical on a row
  that opens and one that cannot. Looking is what *found* it; the test is what
  keeps it found. That is the intended relationship between the two halves of
  this section, and this is the first time in the viewer they have run in that
  order.

  **What that opened is larger than the one fill.** Laying the pane out and
  reading the result back is not only a way to check a colour — `chrome::draw`
  returns the `Command` a frame produced, so a synthesised press turns "what
  happens when someone clicks here" into an assertion. §8.8's *hand on the
  machine* hole had the row click in it on the grounds that a click needs a
  window; it does not. What needs a window is the window manager, the trackpad
  and resizing. Everything between a pixel position and a `Command` is
  reachable from a test.

  That reaches a **disabled** control as well, which is the half worth naming
  separately: a button `egui` refuses the press on returns `Command::None`, and
  so does a press that lands on nothing. The two are indistinguishable in the
  assertion, so a test that a disabled button stays quiet is only a test while
  something else pins the coordinate — `the_add_button_is_where_these_tests
  _think_it_is` (§8.8) exists for that and nothing else.

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
| Cell expansion | — | ported in `qupath-watershed`, **not called by the pipeline** |
| Smooth boundaries | — | ported in `qupath-watershed`, **not called by the pipeline** |
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

#### The same mistake one level down: which pixel size

§8.0 fixed the parameters being converted with an *assumed* pixel size. What it
did not ask is *whose* pixel size — and once §8.9 started moving runs to a level
the viewer is not drawing, the answer stopped being "the slide's".

The pipeline works in the pixels of the level it reads. `detect` converted the
micron parameters with the slide's own `MPP` whatever level that was, which is
right at level 0 and wrong by the downsample everywhere else — and by its
**square** for the two areas. At a downsample of 4, `min_area`'s 10 µm² asked
for a nucleus of 160 level-1 pixels where a real one is 10, so the area filter
would have rejected essentially everything and the run would have reported
almost no objects on a slide full of them.

**Why it survived, and why that is the interesting part.** It is unreachable on
CMU-1. `level_for_detection` takes the coarsest level at or under
`MAX_MICRONS_PER_PIXEL`, and CMU-1's levels are 0.499, 2.0 and 8.0 µm/px — only
level 0 clears 1.0. Every run on the slide this project measures everything
against reads level 0, where the slide's pixel size and the level's are the same
number. A slide scanned at 40x has 0.25 µm/px and a level 1 at exactly 1.0,
which does clear it; that slide is not in `data/`.

That is the same shape as the two unit bugs above — a factor that is 1 on the
only configuration anything is ever run at. It is worth stating as a rule: **a
conversion whose factor is 1 in the development setup is untested, not correct.**
Scale factor 1.0, downsample 1.0, level 0. All three hid a bug here.

The comment on `MAX_MICRONS_PER_PIXEL` had already *asserted* the right
behaviour — "the pipeline converts them with the level's own pixel size, so a
run at any level is arithmetically consistent" — which is what the code was
meant to do and not what it did. The conversion now lives in `params_for`, and
the test asserts the physical round trip across every level of a 0.25 µm/px
pyramid: the pixel value times that level's pixel size is the micron value that
was asked for.

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

#### It came back, and the cause was not the gesture code at all

Reported from use a second time: *pad zoom stopped working.* None of the above
was wrong and none of it was the problem, which is the reason this is written
down rather than folded into the section above.

`cursor_over_slide` compares `self.cursor` against `self.slide_area`. The cursor
comes from winit's `CursorMoved` in **physical pixels**; the area came from
`chrome::draw`, which reads `egui`'s `available_rect` in **logical points**.
Those are the same numbers at a scale factor of 1.0 and at no other, so on a
scaled display the cursor's coordinates were being tested against a rectangle
smaller than the one drawn — at 2.0, the middle of the slide reads as past its
right edge. Every gesture gated on that predicate stopped: not just the
trackpad, but scroll-to-zoom, drag-to-pan and the zoom band, all silently and
all at once. The rectangle is now converted where it is stored, so there is one
unit below that line rather than a conversion at each of three call sites.

Two things worth keeping. The first is that the *symptom* named one gesture and
the cause was under all of them — the trackpad was simply the one being used at
the time, and hunting in the trackpad code would have found nothing, because
§8.1b had already made that code correct. The second is why no test caught it:
a development machine and a headless run both have a scale factor of 1.0, so
the bug is unreachable in exactly the two places code gets exercised. The test
sets `slide_area` the way `frame` sets it and asserts at 2.0, and its last
assertion is that leaving the rectangle unconverted puts the centre outside —
the value that would have made it fail, which §6's rule says to check is
reachable.

#### And it was only half fixed, because the fix was on the wrong side

The paragraph above is correct and the bug it describes is fixed. What it did
not ask is the other half of the same question: the rectangle the interface
*reports* was converted, and the units the interface is **drawn** in were never
looked at.

`egui` tessellates in logical points. `Framebuffer::draw_mesh` takes the
framebuffer's own **physical** pixels — the frame is `winit`'s `Resized`, which
is physical, and the slide's tiles are already drawn in it. `Software::mesh`
handed one straight to the other. At a scale factor of 1.0 that is right; at
2.0 the entire interface was drawn at **half size into the top-left corner** of
the window — a toolbar across half the width, a status bar through the middle
of the tissue, and everything below and right of them bare, over a slide drawn
at full size.

So the two halves were wrong in *opposite directions*, which is why fixing one
did not surface the other: `slide_area` scaled the reported rectangle **up**
while the drawing stayed **down**, and `cursor_over_slide` then answered
correctly for a layout that was nowhere on the screen. The gestures worked; the
picture did not match them.

Both conversions now happen in one place each, and `draw_interface` is a free
function rather than a `Software` method for a reason worth stating: `Software`
owns a `softbuffer::Surface`, which needs a window, so nothing that went
through it could be asserted at all. Taking a `&mut Framebuffer` instead makes
the whole software path — texture deltas, meshes, clip rectangles — run
headless, which is what
`the_interface_is_drawn_across_the_whole_window_at_a_scale_factor_of_two`
does. It rasterises the real interface at 2.0 into a 1280x860 frame and counts
the painted pixels in the **last row**, which the status bar's background
covers and nothing else reaches. The check §6 asks for is the second
assertion: rasterised unscaled, that row is `0` of `1280` pixels.

The general lesson, and it is the one §5 states about coordinate spaces: a unit
conversion applied at the boundary that *reports* a value is not the same fix
as applying it at the boundary that *consumes* one, and a test of the
conversion function passes happily while its caller ignores the argument. This
test fails on a `pixels_per_point` that is accepted and dropped, which is what
the bug actually was.

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

#### The cap bounded the width and nothing bounded the height

§8.1c capped how wide the panels may be so the slide keeps its share, and said
in as many words that "the cap cannot bind a panel whose contents refuse to be
narrower" — which is why the help pane's cells wrap. Nothing bounded a panel's
**height**. A side panel is as tall as the window between the toolbar and the
status bar, and its contents were simply cut off at the bottom.

For the metadata pane that costs a row of the level table. For the analysis
pane it costs the `Detect cells` and `Reset` buttons, which live under six
sliders: on a 760x520 window they are past the bottom edge and **no gesture
reaches them**. A clipped label is visibly clipped; a clipped control is
indistinguishable from a control that was never there.

Each panel's contents now sit in a vertical `ScrollArea`. `auto_shrink` is off
on both axes so the area claims the panel's full height rather than collapsing
to its contents, which is what makes a scrollbar appear only when there is
something below the fold.

Two things it was worth checking in a real window rather than reasoning about.
The buttons are reachable — eight wheel notches over the panel at 760x520
brings them up. And scrolling over a panel does **not** zoom the slide
underneath it, because that gesture is gated on `cursor_over_slide`, which is
geometric; §8.1b's second half is what makes that predicate trustworthy at any
scale factor.

It cost the three-panel width test a pixel per panel of slack. That is not a
loosened bound: every container between the cap and the slide's rectangle
rounds once, and there is now one more of them per panel. 638.9 of 1280 is
49.9%, which is the bound being met.

#### The rectangle was recorded and then not used for the one control named after it

§8.1c gave the slide a rectangle and §8.1b's second half converted it into the
right units. Neither asked what *else* should be reading it, and the answer was
`Fit`.

The viewport spans the whole framebuffer, because that is where the tiles are
drawn; the chrome is painted over it. `Viewport::fit` fitted the slide to the
**window**, so the toolbar and the status bar each ended up with a strip of
slide behind them — on the 1885x1043 window this was looked at in, a tenth of
the slide — and with a side panel open, a quarter of the width, all of it on
one side. The one control whose entire promise is "you are now seeing all of
it" was the control that did not deliver it.

`Viewport::fit_into` takes the rectangle and centres the slide in *it*, so the
overshoot on the unconstrained axis is split between the two visible sides
instead of being pushed under a panel. `fit` is now that function over the
whole window, so there is one implementation rather than two that agree at
first. The test asserts on the projected corners — wherever the slide's four
corners land, they land inside the rectangle a person can see — and its last
assertion is that fitting to the window puts the bottom edge behind the status
bar, which is the value that was reachable.

Two smaller things came with it. A slide *opens* fitted, and `Open::new` runs
before the frame is laid out, so there is no rectangle yet — on the first slide
there has never been one. It fits to the window and the next `render` re-fits
into the rectangle it has just recorded, before that frame is drawn, so the
correction is never visible. And the toolbar's zoom buttons and `1:1` anchored
on the middle of the *window*: with a side panel open that is a hundred pixels
from the middle of what is being looked at, so zooming walked the tissue
sideways. They anchor on the rectangle's middle now.

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

**And it was measuring in the wrong unit, which the ten tests could not see.**
`Viewport::scale` is physical screen pixels per level-0 pixel — the
framebuffer's unit, and the one the tiles are drawn in. `egui` paints in
logical points. The status bar handed the first straight to the second, so on a
display at 2.0 the bar labelled `50 µm` was drawn **twice as long as 50 µm**,
and anything sized against it read half its real size.

This is the same fault as the two above it, and the third instance of it in one
file. Worth stating plainly because of what it does to the argument for a
scalebar at all: the library function is pure, ported exactly, and pinned by
ten tests, *and all of that was true while the bar on screen lied*. Purity of
the rule says nothing about the unit the answer is spent in — a label that is a
round number is precisely what makes a wrong bar look trustworthy, and it is
the one kind of wrong here that ends up quoted in a figure caption.

The call site now converts the scale into points before asking, which also puts
QuPath's preferred bar length of 100 back in the unit QuPath means it in:
JavaFX lays out in logical units, so preferring 100 *physical* pixels would
snap to a different round number than QuPath picks for the same view. The test
is a round trip — what the drawn bar measures against what the label claims,
across three scale factors — and its last assertion is the unconverted value,
which overstates by exactly the scale factor.

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
- **Brightness/contrast and stain vectors — done, §8.5 and §8.7.** `display.rs`
  is the per-pixel operation and `Framebuffer::draw_tile_with` applies it; the
  controls are in the slide panel, which is where §8.7 records what building
  them found. This bullet said for several sessions that the dialog was
  missing and the transform reachable from `examples/panes` only, which had
  stopped being true — the same class of error as the GeoJSON writer
  documented as done with no caller, in the other direction: done, and
  documented as missing.
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
- **Saving detections — done, §8.6, and for several sessions only half.**
  `geojson.rs` writes the dialect QuPath reads, and §0's "nothing found is
  saved" is revised there rather than quietly. What "done" did not mean, until
  it was checked, is that anyone could *reach* it: the writer was a library
  function with no caller, while the top of this file said "what you find can
  be saved as GeoJSON that QuPath opens". It is File → **Save detections…**,
  and `s`. The *parameters* are still not saved, and no detection is
  editable.
- **Selecting a detection and a measurement map — done, §3.5b.** QuPath's
  click-to-select and its *Measurement map* pane, as a click on the slide and
  the **Objects** pane: the selected object's values, and every outline
  coloured by a chosen column with QuPath's own Inferno and a legend. What it
  cannot show is QuPath's per-cell measurements, which are not ported; it
  shows the columns a run carries and says whose they are.

### 8.3 Projects — the format is portable, the browser is not

The tutorial that prompted this is QuPath's *Projects* page, and its figures are
`ProjectBrowser.java`: a pane of image thumbnails, right-click to add metadata,
sort by name or URI, mask names for blinded studies. All four are here now —
the thumbnails and the sort below, masking in §8.3b, the right-click in §8.3c.

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
`docs/dev/VIEWER.md` §0 says *no project format* as a non-goal. Doing this revises
that, and the revision should be deliberate: the argument for it is that a
project is how a pathologist actually works — a study is fifty slides, not one —
and the argument against is that it is the point where "a viewer" becomes "a
QuPath replacement", which §0 rules out.

**Settled, and the JSON parts are written — `project.rs`.** What carried it is
narrower than the argument above: the format is a *specification to follow*, so
following it exactly is interoperability rather than a second application. The
browser pane is **not** written and is the part that would be, and there is
still no `data.qpdata` — see §8.8.

**The URI trap has a second half, on the other side of the same line.** §8.3's
header says a project whose `uri` fields are bare paths opens with every image
missing, and `file_uri` was written to prevent it. What it did not handle is a
path that is *relative*: `file://data/CMU-1.svs` is not a bare path, it is a
well-formed URI naming the **host** `data` and the path `/CMU-1.svs`, so QuPath
looks for the slide on another machine and the entry fails to open — the same
failure, reached from the other side, and by a URI that looks right.

It is the common case rather than an odd one: this file documents starting the
application as `cargo run -p qupath-app -- data/CMU-1.svs`, so the path an
entry would be written from is relative in the commonest invocation there is.
The module has no caller yet, so this was latent — and it would have surfaced
as "the project browser writes projects QuPath cannot open", which is a much
harder thing to trace back to one missing slash.

`file_uri` resolves against the working directory now, which is exactly what
`File.toURI()` does in the Java this module follows. `std::path::absolute` is
the lexical form of that — it joins the working directory without resolving
symlinks or touching the file.

**QuPath could not open it, and the reasons came one at a time.**
`parity/java/ProjectImport.java` calls `ProjectIO.loadProject`, which is what
*Open project* reaches. The first four runs failed, each hiding the next:

1. **No `createTimestamp` or `modifyTimestamp`.** `DefaultProject.loadProject`
   reads both with `getAsLong()` and **no `has` guard** — the first two lines of
   the method, before the URI, the version, the entries or `lastID`, every one
   of which *is* guarded. A project without them throws a
   `NullPointerException` wrapped in an `IOException`, and the message names
   Gson rather than the field. Every project this module has ever written was
   unopenable, and nothing here could have seen it: the file parses, and
   `DefaultProject.java` had been read for what it *writes* rather than for what
   it *demands*. `Timestamps` is a parameter and not a clock, so the format
   stays comparable against itself.
2. **`"channels": ["Red", "Green", "Blue"]`.** An `ImageChannel` is an object
   with a name and a packed colour, so Gson threw `Expected BEGIN_OBJECT but was
   STRING at path $.metadata.channels[0]` — and it took the **whole project**
   down, not the one entry.
3. **`"rgb"` and `"pixelWidthMicrons"` are not fields.** Gson uses the declared
   field name and QuPath registers no naming policy: the flag is `isRGB`, and
   the pixel size lives in a `pixelCalibration` object of `{value, unit}`
   quantities whose unit is compared by string equality against `"µm"` — U+00B5
   MICRO SIGN, not U+03BC GREEK SMALL LETTER MU, which is identical in most
   fonts. Both were being written, dropped, and reported by nothing: QuPath read
   `rgb: false` off a project that said `true`.
4. And with all of those corrected, `metadata.getLevels()` threw
   `NullPointerException`. That is not an idle call — it is what
   `AbstractImageServer.setMetadata` reaches, and `setMetadata` is what
   `AbstractServerBuilder.build` calls **when the image is opened**. The project
   would have opened and every image in it failed.

**So the metadata block is not written at all, and that is the finding rather
than a retreat.** `getLevels()` is `Arrays.asList(levels)` over a field Gson
leaves null, and the pyramid is precisely what this crate does not know: `Entry`
carries a width, a height and — until this — a pixel size, while the level
table and the preferred tile sizes come from `openslide` reading the file.
Writing one level would be worse than writing none, since it says the image has
no pyramid and QuPath would then read full resolution for every view of a
46,000-pixel slide. QuPath's own API has the shape for this already:
`DefaultImageServerBuilder.createInstance(providerClass, uri, args)` passes a
null metadata, and `build` then leaves the server with what OpenSlide read.
That is strictly better information than anything written here.

One half of that has since stopped being true and the conclusion did not move,
which is worth recording. The pyramid *is* now knowable at the point the entry
is written — `add_to_project` holds an open `Slide`, and §8.8 has the parity
recording showing this crate's level table and OpenSlide's agree in every
field. So "this crate does not know" was a reason of its time, and the block is
still not written for the reason that outlived it: `setMetadata` **replaces**
what OpenSlide read, so the cost of the copy going stale is silent wrong pixels
against a benefit that is one file open. §8.8 has that argument in full, and
records it as decided rather than pending.

`Entry::microns_per_pixel` went with it, and its going is the point. With no
metadata block there is nowhere in the format QuPath reads a pixel size from —
`ImageDataSummary` has no such field either — so it was a struct field that
reached a file, was discarded by Gson, and was reported by nothing. That is the
same shape as §8.6's export with no caller, one level further in.

Two more names were wrong in `summary.json` and could not have failed:
`ImageDataSummary`'s fields are **`server`** and **`hierarchy`**, not
`serverSummary` and `hierarchySummary`. Nothing in QuPath reads that file — it
is written and never loaded — so no amount of running QuPath would have
reported it. It is corrected because a file that claims to be in QuPath's format
and is not is exactly what this module exists not to produce.

What opens now: both entries, in order, under the names they were given, with
`entryID` naming the directory, the URI resolved to a slide that is there, the
builder dispatched to `DefaultImageServerBuilder`, and QuPath's own summary
reporting "No data file" — which is true, and is what §8.3 said would happen.
`parity/record-project.sh` writes the project, records QuPath's reading and
deletes it; `crates/qupath-viewer/tests/parity_project_import.rs` asserts the
reading against the same entries the project was written from. The directory
cannot be committed the way the GeoJSON sample is, because every path in it is
an absolute URI naming the machine that wrote it.

#### Reading one back, and the spelling QuPath writes

A browser opens projects it did not write, so `project.rs` now reads as well as
writes — `read`, into a `Stored` of `Image`s. Two things came out of it that
writing could not have shown.

**`File.toURI()` writes `file:/data/x.svs`, with one slash.** `file_uri` writes
`file:///data/x.svs`. Java parses both to `authority == null`, so both open —
which is why writing the second has always worked, and why `movedFromPreviousUri`
is `false` in `ProjectImport.json` even though the project's own `uri` is
written in the other spelling. But a *reader* that accepted only the spelling
this module writes would fail on **every project QuPath wrote**, which is the
entire population a browser exists for. `uri_path` takes both, and refuses a
host — including `localhost`, which RFC 8089 calls equivalent to none and
`new File(URI)` still throws `URI has an authority component` for. Refusing
exactly what QuPath refuses is what keeps "this entry will not open" the same
statement on both sides.

**Reading is not the inverse of writing, and that is the useful part.** A
`Stored` image has an id, a name, a `builderType` and a path; it has no width or
height, because the only place a project keeps those is the metadata block §8.3
removed. The browser gets the location from the project and the pixels from
opening the slide, which is the division that block's removal already implied
and this makes concrete.

An entry the reader cannot open is **listed** rather than dropped:
`Image::slide` is a `Result`, and the sixteen builder types QuPath registers
besides `uri` — `rotated`, `cropped`, `sparse`, `affine`, `pyramidize` and the
rest — wrap another builder instead of naming a file. A browser that omitted
those rows would show a project with fewer images than it has, and nothing about
the picture would say so.

`json.rs` is the parser this needed, and it is a parser rather than a search for
`"uri": ` for one reason: a `serverBuilder` QuPath wrote carries a metadata
block, and the first `uri` in an entry's text is not the entry's. Both are
strings, both look like paths, and taking the wrong one opens the wrong slide in
silence.

#### And then QuPath was asked to write one

The reader above was checked against the writer beside it, which is the test
this repository keeps finding is not a test. `parity/java/ProjectExport.java`
loads the sample project, adds an entry carrying the metadata block an opened
image would have, and writes the whole thing back with `syncChanges()`;
`ProjectExport.json` is that file, and `parity_project_export.rs` reads it.

**Both URI spellings are in the same project.** The two entries QuPath loaded
from this project's file kept `file:///SLIDES/...`; the entry QuPath built
itself from `File.toURI()` is `file:/SLIDES/...`. So the paragraph above is not
a distinction between projects, it is a distinction *within* one — a reader
taking either spelling alone reads two thirds of a project and drops the rest
with nothing in the picture to say so.

**An unnamed entry is `"Image null"`**, not `"Image 3"`.
`DefaultProjectImageEntry`'s constructor builds the fallback from the `Long`
parameter — null on the path that creates an entry from a builder — rather than
from the field it assigned from the counter one line above, so the name is
written three lines from the `"entryID": 3` it was meant to contain. Its
*reader* applies no fallback at all, so an entry whose JSON has no `imageName`
is a blank row in QuPath's own browser. `Image::name` copies neither: a blank
row cannot be pointed at and `Image null` names the bug rather than the entry.

**The metadata block is now written down, which closes half of §8.8's entry on
it.** The fixture holds QuPath's own: a `levels` array of downsample, width and
height; `preferredTileWidth` and `preferredTileHeight`; `isRGB`; `channels` as
objects with a name and a packed colour; and a `pixelCalibration` of
`{value, unit}` quantities whose unit is U+00B5. §8.3 removed that block for not
knowing the pyramid and §8.8 lists carrying one from an open `Slide` as what it
would take. What was missing was never only the pyramid — it was also what the
block has to look like, and that part is no longer a matter of reading
`ImageServerMetadata` and hoping.

#### The browser pane, which is what §8.8 asked for

`project.rs` had no caller. It has one: a left-hand pane listing the images in
the project that is open, a `File` menu that opens one, `p` on the keyboard, and
a `.qpproj` on the command line —
`cargo run -p qupath-app --release -- study/project.qpproj`.

**Opening a project does not open an image.** A study is fifty slides and a
project says nothing about which of them someone wants; opening the first would
be a wait nobody asked for. A row is a click, and it sends the same command the
recent menu and the welcome screen send — `OpenPath`, renamed from `OpenRecent`
for exactly that reason: three places mean the same act and only one of them is
about recency.

**A row was a name and not a thumbnail**, and the reason given was
`thumbnail.jpg` — the one file of an entry's four this workspace cannot produce,
so a grid would be placeholders for every project this application wrote and
pictures for every project it did not. **That reasoning was wrong, and the
section below is what replaced it.**

**Every row is shown, including the ones that will not open.** A slide that is
not on disk is marked in the warning colour with the path on hover; an entry
built by one of the sixteen transforms QuPath supports is greyed with the reason
under it. Dropping either would show a project with fewer images than it has.
The disk check runs **once**, when the project is opened — a `stat` per row per
frame is a syscall storm for an answer that changes when a drive is plugged in,
and the staleness is bounded because clicking the row still tries the file and
reports what happened. QuPath checks on open for the same reason.

A moved project is said once at the top rather than fifty times below: the `uri`
field against the file that was actually opened, compared as paths because the
two spellings of a `file:` URI are different strings for the same place.

The pane is headed by QuPath's own `getName()` — `study/project.qpproj`, the
directory *and* the file — because every project file there has ever been is
called `project.qpproj`, and a browser headed with the file name heads every
project identically.

What is **not** here is writing: no way to create a project and no way to add a
slide to one. That is §8.8's next entry, and the reason it is a separate one is
that rewriting a project's index means writing back entries this reader
summarises rather than preserves — an entry's `description`, its metadata map,
its tags, and any server metadata block. Losing those is data loss, and it is
silent.

#### Rewriting one without emptying it

Adding a slide to a project means writing the index back, and writing it back
from what `read` returns would have been **silent data loss**. `Stored` is a
summary — an id, a name, a path per image — and an entry carries more than
that: a `description`, a `metadata` map QuPath's browser lets a user edit, a set
of `tags`, and a `serverBuilder` that may hold the whole stored metadata block
recorded two sections above. A rebuilt index deletes every one of those from
every entry it did not touch, and the result still opens, still lists the same
images, and has lost what someone wrote against them.

So `read` keeps the document, and `added` appends to it: `images` gains the new
entries, `lastID` becomes the last id issued, `modifyTimestamp` becomes now, and
every other byte at every depth is what was read.

The check that this is complete is an equality rather than an argument —
`a_project_qupath_wrote_is_written_back_unchanged` reads `ProjectExport.json`
and writes it back **byte for byte**. That is only available if `json`'s writer
spells things the way Gson does, so it does, and every part of that had to be
measured against a real `GsonTools.getInstance(true)`: two-space indent, `": "`
between a key and its value, empty containers inline, `<`, `>`, `&`, `=` and `'`
as lowercase `\u00xx` escapes because `disableHtmlEscaping` is never called,
`/` left alone, U+2028 and U+2029 escaped, everything else above ASCII raw, and
no trailing newline.

`lastID` is where the one bug in this was, and it was found by the degenerate
case: an append of *nothing* still advanced the counter, because `next_id` is
the id the next entry takes and writing it back claims that entry exists. The
next append would then skip a number and leave a `data/` directory nothing
points at. No test that added something could have seen it.

#### Creating one, and adding a slide to it

`File > New project…` picks a directory and writes an **empty** project into it;
`Add slide to project`, in the menu and as a button at the top of the pane, puts
the slide showing into the project that is open. QuPath separates *Create
project* from *Add images* and so does this — one click that both made a project
and decided what went into it would be two decisions taken by one.

A directory that already holds a `project.qpproj` is **not** overwritten. Someone
pointing a create dialog at a study directory is far likelier to have meant to
open it, and the two mistakes do not cost the same.

The index is written **last**, after the entry directories. A run interrupted
between the two then leaves a directory with no project in it, which reads as
nothing having happened — rather than a project naming entry directories that
are not there, which reads as a corrupt study.

`App::addable` is one predicate, read by the menu item, by the button and by the
command, for the reason `App::found` is one: two would eventually disagree, and
the way they disagree is a live control that does nothing when pressed. Its
error *is* the disabled hover text, so the answer to "why is this greyed out" is
where the greyed-out thing is.

`examples/project_sample.rs` now builds its project the way the application does
— an empty one, then `added` — so `ProjectImport` records QuPath reading the
bytes the application actually produces rather than a second writer's. Both
fixtures came back identical, which says the Gson-shaped index and the
hand-rolled one are the same project to QuPath.

#### The bug the running window found

The photograph showed the pane listing two images with **neither marked**, while
one of them was the slide on screen.

The application had been started as `-- data/CMU-1-Small-Region.svs`, and every
path a project stores is absolute — `File.toURI()` resolves one, and so does
`file_uri`. So `App::open` kept `data/CMU-1-Small-Region.svs` and the entry said
`/home/…/data/CMU-1-Small-Region.svs`, and the comparison between them was
always false. This is §8.3's relative-path trap met from the other side, and the
mark was the harmless half: `App::addable` compares the same two paths, so it
would have offered to add a slide the project already held — two entries and two
`data/` directories for one image.

`App::open` now resolves with `std::path::absolute` before anything keeps the
path, which is the same lexical resolution `file_uri` does, so the two agree by
construction rather than by coincidence. It is the fourth time in this file that
looking at the running application found something no test was going to.

#### The pictures, and the blocker that was not one

**§8.8 had the thumbnail filed under "the same missing JPEG encoder as ever",
and that was a fact about a file rather than about a picture.** The row's
picture was assumed to be `data/<id>/thumbnail.jpg`, which this workspace can
neither write nor read — so the entry sat under the encoder, where it would have
waited for one.

**§8.3's own first paragraph had it right**, which is the part worth noticing:
"a list, thumbnails from a `TileCache`, and `Viewport::fit` at a small size to
generate them ... the thumbnail generator is already written". A later session
looked at the four files an entry has on disk, found the one this cannot write,
and talked the plan out of a picture it already knew how to make.

`ProjectBrowser.ProjectTreeRowCell.updateItem` says the same thing from the
other side. It asks
`entry.getThumbnail()`, and **when the answer is null it opens the image**:
`executor.submit(...)` builds the server from the entry's builder, calls
`ProjectCommands.getThumbnailRGB(server)`, and stores the result back on the
entry. The stored file is a *cache* of a picture QuPath is prepared to make from
the pixels, not the source of it. Reading a project QuPath wrote and reading a
project this wrote are the same problem, and neither of them needs a codec this
workspace does not have — `qupath-slide` opens the file and
`crate::thumbnails::make` is the picture the overview pane has been drawing
since §8.4.

So the browser is QuPath's: a picture beside each name, at
`ProjectThumbnailSize.SMALL`'s 50 by 40 points, made from the slide. What is
copied is the shape of it — one background thread (`newSingleThreadExecutor`
with a `thumbnail-loader` factory) walking the entries in the order the list
shows them, and a slide that failed never asked for again (`serversFailed`
there; here the queue is a list walked once, so a retry has nowhere to come
from). What is not copied is the four sizes and the hidden setting, which are a
preference this has nowhere to keep, and the tooltip's larger picture.

**The picture belongs to the project, not to the application**, and that is the
whole of how a stale one is prevented. The worker delivers by row index, and row
0 of one study is a different slide from row 0 of the next; a browser with one
queue would paint a picture from the study that was closed onto the list of the
study that replaced it, and both rows would look perfectly ordinary. `Pictures`
is a field of `Project`, so opening another study drops the channel the first
one's pictures would have arrived on. There is no generation counter because
there is nowhere for the answer to arrive.

What crosses that boundary deliberately is the pictures already made: a re-read
carries them over **by path**, because `add_to_project` writes the index and
opens it again, and a browser that started from nothing there would blank a
fifty-slide study and read all fifty again for one added row.

**Uploaded before the interface is laid out, and that ordering is load-bearing.**
A row names a texture id derived from its index, and those ids are reused by the
next project opened. A picture uploaded after the layout that names it is not
merely late — the frame resolves the id to whatever the previous project left
there. `Pictures::ready` is false until the renderer holds *this* project's copy,
and `App::take_pictures` runs beside `request_tiles` rather than inside `draw`.

#### Searching it, and the separator that reads backwards

A study is fifty slides, and a list of fifty names is scrolled rather than read.
QuPath's browser has `tfFilter` over its tree for that, and this has the same box
under the add button — the button is about the slide showing, the search is about
the list, and a control belongs beside the thing it acts on.

`matches` is `ProjectEntryPredicate.createIgnoreCase`, which is what QuPath
builds unless the regular-expression toggle is on: trim, accept everything for an
empty text *or one that is nothing but separators*, lowercase both sides, look
for the whole text in the name — and only then, if the text contains `|`, split
it into tokens of which **every one** must be found.

**`|` reads as "or" and means "and".** `tif|block` shows the images whose name
has both. That is the one thing about the control a user can get wrong, and a
port that read it the obvious way would show *more* rows than QuPath for the same
text with every one of them looking like a match — which is why it has a test
rather than a comment.

What is not ported is the other half of a token: QuPath tests each against the
entry's metadata as `key=value`, plus a synthesised `has_data=<bool>`.
`project::read` summarises an entry to an id, a name, a builder and a path, so
the metadata map is in the document and unread, and `has_data=true` matches
nothing here. Reading it is a different change from a search box, and §8.8 has
it now.

**A row is skipped and its index is not.** A picture belongs to the entry it was
read for, and renumbering the rows a search leaves showing would hand row 3's
picture to row 1 — the same index-identity trap as the one `Pictures` is owned by
a project to avoid, one layer up. The search clears when a project is opened, as
QuPath's does: a search typed against one study hides rows of the next for a
reason that is one scroll off the top of the pane.

#### Three more things the running window found

**A row with no picture and no cross says "wait" for ever.** The study opened for
the photograph has a JPEG 2000 slide in it — the file is there, `Slide::open`
succeeds, and the coarsest level decodes nothing, which is `SlideError`'s
`UndecodableCodec` and exactly what §3 says this reader does. Its row held the
same empty frame as a row whose slide was still being read, permanently. The
worker now sends the *reason* rather than skipping the entry, the frame is
crossed out once it is settled that no picture is coming, and the hover carries
what the reader said. QuPath draws an `INACTIVE_SERVER` icon in the same place
for the same reason. An entry naming no file is crossed too — nothing was ever
queued for it, so an uncrossed frame there would be the same lie.

**`ok() == showing` marked every unopenable row as the image on screen.** An
entry built by one of the sixteen transforms has `None` for its path, and an
application with no slide open has `None` for what it is showing, so the two
compared equal and the row wore the current-image fill. It was written while
moving the row's body into one branch, it reads correctly, and it is only
visible in a picture: `is_showing` is a named function with a test now, because
the shape of it — two absences comparing equal — is not one a reader catches.

**§8.1c's width trap, a third time.** A `Label` inside a horizontal layout
defaults to `TextWrapMode::Extend`, so its desired width is its whole text; a
side panel sized from its contents then grows to the cap that binds it and clips
there. The pane's heading stopped wrapping and the reason under a row ran off
the edge — and the *cause* was three widgets away, in a row that had been given
a picture. Same family as the clipped slider labels and the same fix: the
content has to be able to be narrower than the panel. This is the third
occurrence, which is what `docs/dev/EVOLUTION.md` asks for an entry at.

#### Ordering it, and the sort that is not a string sort

QuPath's browser has a **Sort by...** menu above its tree; this has the same
four choices in a combo under the search box, because §8.8 named sorting as the
cheapest of the three things the pane was still missing — the other two want
the entry's metadata map, which `project::read` summarises away.

**The ordering is `GeneralTools.smartStringSort` and it is not a string sort.**
A name is split on `(\d+)` into digit runs and the text between them, the runs
are parsed to `BigDecimal` and compared by *value*, and the last tiebreak is
the number of parts. `["a1", "a2", "a10"]` stays as it is where a plain sort
puts `a10` second. It is ported as `project::smart_string_compare` and pinned
by `parity/fixtures/SmartStringSort.json`, which is QuPath's own function
handed thirty-three names chosen for the parts a reader gets wrong.

**The recording earned its keep on the first run, on the one thing no reading
would have caught.** Java compares UTF-16 code units and Rust compares UTF-8
bytes. A character from U+10000 up is a *surrogate pair* in Java, both halves
in U+D800..U+E000 — below every character from U+E000 to U+FFFF — so QuPath
puts an emoji **before** `\u{e000}` and `str::cmp` puts it after. It was the
only line of the fixture the byte comparison got wrong, and the fix is
`left.encode_utf16().cmp(right.encode_utf16())`, which allocates nothing. A
slide named with an emoji is not the reason to carry it; the reason is that a
browser whose rows are in a different order from QuPath's for *any* name is one
that has to be checked name by name before it is believed.

Three more the fixture pinned and a reading would have got wrong:

- **`007` and `7` compare equal.** `BigDecimal.compareTo` ignores scale, so the
  tie falls through to the part count, which is also equal. Their order in the
  fixture is nothing but the input order Java's stable sort left them in —
  which is why the equality is asserted *directly* as well as through the list:
  a comparator that ranked them would reproduce the recorded order here and
  diverge on the next project. Same reason the sort in `row_order` is
  `sort_by` and not `sort_unstable_by`.
- **A digit run meets a text part as the digits it prints as.** Java compares
  the two `toString`s and `new BigDecimal("007").toString()` is `7`, so the
  leading zeros come off when the run is *built*, not when it is compared.
- **The tiebreak is a part count, not a length.** `a1` beats `a1b1` on it, and
  beats `a1bbbbbbbb` on text without ever reaching it.

**The sort is case-sensitive and the search box three lines above it is not.**
`Block` sorts before `abc`; `block` matches `BLOCK`. Both are QuPath's
behaviour and they really do disagree, in the same pane about the same names.

##### The order is over indices, which is the whole design

A row's picture is `PROJECT_TEXTURE + index` and `Pictures` is indexed the same
way, so `row_order` returns `Vec<usize>` and never a sorted copy of the
entries. Sorting the entries would hand row 3's picture to whatever ended up
first — the same index-identity trap the search met one heading up, where a row
is skipped and its index is not, with the rows *moved* instead of removed. And
it would have been invisible: every row would still look like a row, with a
real name over a real thumbnail of the wrong slide.

Which is what `chrome-project-sorted.png` is for. It shows the rows in a
different order with the one tissue thumbnail still on the slide it is of, and
that is an assertion no count could make.

##### Three of the four choices are a divergence

QuPath's sort is a **grouping**: a `METADATA` tree node per distinct value with
the entries hung off it. Except when the key is the name, which it
special-cases straight back to a flat list, because "the name effectively being
repeated twice ... looks weird". So *Name* is the same thing on both sides and
is the case this reproduces exactly.

*Entry ID* and *File name* would be groups there and are flat lists here,
ordered by the value the groups would have been ordered by. A pane of flat rows
has nowhere to hang a group; the alternative is a tree, and a tree is a
different pane. **Project order** is QuPath's *None*, and the default, because
it is the order a project has when nobody has asked for another — entries are
appended as images are added, so it is also the order they were added in.

*File name* follows `getDefaultValue`: `uri.getPath()` with everything up to
the last `/` dropped. So it sorts by **file** name where *Name* sorts by the
name the project gave the entry, and the two differ exactly when somebody has
renamed one. An entry naming no file sorts first, rather than under QuPath's
`UNDEFINED_VALUE` — a word in a resource bundle is not something to match on,
and those are the rows that will not open. The bottom of a list is where a row
goes to be scrolled past.

**The order survives a project being opened; the search does not**, and the
asymmetry is the point. A filter left over from another study hides rows and
shows nothing that says why. An order left over hides nothing — every row is
there, and somebody who wants names alphabetical wants that about *lists*, not
about one project. QuPath keeps it on the project, writing `_SORT_KEY` into the
project metadata so a study reopens as it was left; this cannot, because
writing a key `project::read` does not read back is how a rewrite quietly drops
it (§8.3's rewrite is byte-exact precisely so that it does not). Per-session is
the half of QuPath's behaviour that can be honoured without the other half
being a lie.

#### Two more things the picture found

**The sample project's names were already in alphabetical order.** So the first
render of the browser sorted by name was byte-identical to the render of it
unsorted, and neither picture said anything about either. A picture that cannot
distinguish the thing it is of from its absence is §6's "check appearance by
looking" failing in the way §6 warns about — the instrument was pointed at the
right pane and had nothing to show. A fourth entry fixes it: written third,
sorts first.

**And adding that entry moved the greyed transform row onto the wrong slide.**
`examples/chrome` writes a project with `project::files` and then rewrites the
*last* `"builderType": "uri"` in the file, because writing a transform builder
is exactly what `project.rs` refuses to do. The rewrite is positional, so the
new fourth entry inherited it and the row labelled "built by a `rotated`
transform" was no longer the one named `CMU-1, rotated 90°`. Two rows swapped
their states, both went on looking like rows, and nothing in the browser could
have said so. Fixed by writing the new entry *before* the rotated one, with the
reason in a comment at the site rather than in this file, because the next
person to add a row will be reading the array and not §8.3.

**§8.1c's width trap was expected for a fourth time and did not fire.** A
`ComboBox` sizes itself from its widest choice, so inside a content-sized
`SidePanel` it is exactly the widget that cannot be narrow — the shared
resource `EVOLUTION.md`'s entry is about. It is given the width the row has
left instead, floored so a very narrow panel truncates the choice rather than
pushing the panel out, and the list inside it is set to `Truncate` rather than
the `Extend` `ComboBox` uses, so that a fifth choice added later cannot widen
the menu either. Predicted, avoided, and the picture is what says so rather
than the reasoning.

#### Pressing the combo, and two bugs in the instrument rather than in the pane

A closed combo says nothing about its list, so `examples/chrome` now delivers a
**synthesised click** — a press and a release at a position read off the
picture — and photographs what opens. The popup came out with `Project order`
reading `roject order` and `Entry ID` reading `ntry ID`, every other glyph
present, the rows behind it showing through, and an hour went into the widget's
width before either had anything to do with the widget.

**Neither was a bug in the pane. Both were bugs in the instrument, and that is
worse**, because §6's whole argument is that these pictures are what assertions
cannot be. A picture that lies is worth less than no picture.

- **A partial texture delta, dropped in silence.** `egui` sends the font atlas
  whole the first time and *patches* it afterwards, appending the glyphs a
  later frame first asks for. The example skipped every patch, with a comment
  explaining that a render from scratch has nothing for one to apply to — and
  the whole-image set from the first frame is exactly what it applies to. `P`
  and `E` are the only capitals on that pane no earlier frame had asked for, so
  those two words lost their first letter and nothing else did. One 35x11
  patch. It is applied now, and `paste` clips at the edges rather than wrapping,
  because a wrapped row puts one glyph's right-hand column inside a different
  letter.
- **A popup photographed a fifth of the way through its fade-in.** The rows
  were not showing *through* the menu; the menu was at 44% opacity, one frame
  after the click, and `egui` was compositing it correctly. The frame count and
  a `predicted_dt` fix it — an animation needs time to pass, not just frames to
  be drawn.

Both were found the same way, and it is the way `MEMO.md` §4 keeps recommending:
stop reading the widget and read what the *pixels* say. A dump of the vertex
alphas said `{0, 112}` and the transparency stopped being a mystery in one line;
a dump of the texture deltas said `PARTIAL at [1499, 0] size [35, 11]` and so
did the missing glyphs. Both diagnostics took a minute and neither was reached
for until the reasoning had run out.

### 8.3b The entry metadata — one reader change, and the leaks it opened

§8.8 predicted that four things were behind one change: the `key=value` half of
the search box, the synthesised `has_data`, QuPath's metadata sort keys, and
name masking. They were, and the change is small — `project::read` reads the
entry's `metadata`, `tags`, `description` and `randomizedName` instead of
leaving them in the document. What was not small is what the last of the four
turned out to require.

**Two things called `metadata`, at two depths.** An entry has a `metadata` map
a user types into, and its `serverBuilder` has a `metadata` block describing the
pixels. Reading the wrong one offers `width=46000` as a key a study could be
sorted by, and it looks entirely plausible in a menu. This is the `"uri"` trap
one level along — §8.3 already had a test for a `uri` *inside* a metadata block
not being the entry's — and it has its own test now for the same reason: the
two are told apart by where they are and by nothing else.

The map is a `Vec` of pairs rather than a map. Gson serialises a
`LinkedHashMap`, so the file's order is the order somebody entered the keys in,
and `getMetadata` hands the browser that map directly. A `BTreeMap` is the
obvious choice and would re-alphabetise every study silently, so the test uses
keys that are not in alphabetical order.

`ProjectEntryPredicate` flattens the map to `key=value` and matches each token
with **`contains`**, not with an equality. So `stain=h` matches `stain=h&e` and
a bare `stain` matches every entry that has the key. Splitting the token on `=`
and comparing the halves is the natural way to write it, is stricter than
QuPath, and the rows it drops are the ones somebody was searching for.

#### Masking, and why it is not just showing a different string

`getImageName()` returns `randomizedName` under `maskImageNames`, and
`getAllImageRows` shuffles the list "for less biased analyses". Copying those
two lines gets a pane that is not masked, because a browser has more than one
place a name comes out:

- the **search box** still matched the real names, so typing `tumour` answers
  "which row is that?" with a list of one — the row still wearing its masked
  name while being the only one on screen;
- the **hover** showed the slide's full path, and a study's file names are its
  names;
- **sorting by file name** or by a metadata key puts the rows in a ranking of
  the thing being hidden. The rows stay anonymous and their *sequence* does
  not, which is exactly what shuffling them was for.

The first two are closed. The third is a **divergence and the one place this
pane is stricter than QuPath**: its menu offers every key whether or not names
are masked, and `getDefaultValue` hands back the file name and the metadata
value unmasked. Copied faithfully, the checkbox would be a mask that a single
menu choice lifts. Masking is offered as a blinding control, so either it is
one or it should not be there — `Order::offered` drops those choices while the
tick is on, and `row_order` drops them *again* because the order outlives the
checkbox and `File name` picked a moment before ticking would go on quietly
ordering the rows by the names being hidden.

What is deliberately **not** masked is the **thumbnail**, which is the strongest
cue on the row. QuPath does not mask it either, and a pane of grey squares is
one nothing can be done with — so the mask is against recognising a *name*, not
against recognising a slide, and the hover says so rather than letting a tick
imply more than it does.

#### The instrument was wrong again, and this time it was ours

`project::files` writes `entry-<id>` into `randomizedName`, so that the same
project written twice is the same bytes — see `placeholder_name`, and it is the
right trade for a field nothing was reading. It is the wrong one the moment
something reads it: masking a project **this** application wrote replaces every
name with a spelling of its entry id, which is the entry order in longhand.
A blinded study is a claim somebody makes in a paper, and the pane would be the
evidence for it.

So the checkbox is *disabled* for a project whose randomized names are its ids,
with a hover saying which of the two kinds of project this is — disabled rather
than hidden, because a missing control cannot be told apart from a feature that
does not exist. `maskable` is the check, `row_order` re-checks it so that a
project opened while the flag is already set is refused too, and
`the_randomized_name_is_read_and_this_modules_own_is_not_random` in `project.rs`
asserts the placeholder still names the id — so that anything offering to mask
names has to meet this first rather than discovering it later.

The same shape as the memory `run-the-binary-not-only-the-tests` keeps
recording, with the twist that the thing that was wrong was written here. It was
found by asking what the mask would actually *show*, which is a question a test
of `shown_name` does not ask.

And then the *control* for it was wrong, which the same memory found the same
way. Launching the application on both kinds of project at
`WINIT_X11_SCALE_FACTOR=2` and photographing them says that `add_enabled(false)`
dims the label from 179 to 103 on a 27 background and the box from 60 to 43 — a
real difference, and one that reads as **an ordinary unticked checkbox** in a
dark theme. A user meeting a project that cannot be blinded had no way to tell
that from blinding simply being off, and the hover that explains it only appears
to somebody who already suspects there is something to explain. So the label
carries the reason as well: *Mask names — not for this project*. The hover has
the whole explanation; the label is what makes anyone hover.

Worth stating plainly, because it is the third time in this file: **a control
that refuses has to look like it is refusing, and greying it is not that.**
Nothing but a photograph was ever going to say so — the widget was constructed
correctly, the test asserted the right function, and the pane was wrong.

#### The shuffle, and the one thing its seed must not be

QuPath reshuffles inside `getAllImageRows` on every `refreshTree` — which
includes every keystroke in its search box. That cannot be copied directly:
`row_order` runs once per **frame** here, so a shuffle computed there would
reorder the list sixty times a second. It is drawn once, when masking is
switched on, and held by entry id so it survives the project being re-read —
adding a slide rewrites the index and reopens it, and a shuffle stored as
positions would silently become a different shuffle.

The seed is the wall clock, and the one thing it must not be is derivable from
the list. A seed taken from the entries — their count, their ids, a hash of
their names — makes the permutation a function of the project, so the same
study masked twice is masked the same way and two readers comparing notes
recover the order. Every other assertion about a shuffle still passes. The test
therefore checks that two shuffles of the same list *differ*, which is the only
one of the three that would catch it.

### 8.3c Editing that metadata — the first time a user's typing reaches someone else's file

§8.8 said what was left of `ProjectBrowser` was **right-click to edit an
entry's metadata**, and that it was no longer a reader problem: `added`
rewrites the document it read, so the mechanism was there. What it also said
was that nothing answered **what happens when the file has changed underneath
since it was opened**, and that question turned out to be the whole design.

`project::edited` is the third writer. `files` writes a project from nothing
and `added` appends to one; this reaches into an entry another program wrote
and changes one field of it. It takes `added`'s shape — the document `Stored`
kept, cloned and edited at one key — for the reason `added`'s own
documentation gives: a rewrite built from `Image` would drop every entry's
`serverBuilder`, which is how the image opens at all, and drop it silently.

Three decisions in it are worth the lines they cost.

- **The field keeps its place.** QuPath writes `metadata` between `imageName`
  and `tags`. Removing the key and pushing a new one is the obvious spelling
  and moves it to the end of the entry, so the file reads identically and
  *diffs* as the whole entry rewritten. Someone reading that diff to see what
  changed would be reading the wrong thing.
- **Clearing the last key returns the entry to the shape it had**, rather than
  leaving `"metadata": {}` behind. Both read back the same, so this is not
  about correctness — it is that opening an editor and closing it again should
  not change the file.
- **`lastID` does not move**, which is the difference from `added` worth
  naming: no entry was created, so no id was issued. Advancing it leaves a
  `data/` directory nothing points at.

Entries are found by `entryID` and not by position, because the caller's list
is the *browser's* order — sorted, and filtered — and an index into that is an
index into a different list. The same reasoning the pictures already used.

#### The stale check, and the bug in it that only a real write could find

The answer to §8.8's question is **refuse, and say so**. Before writing,
`App::set_metadata` re-reads the index and compares it against what was read
when the project was opened; if the two differ, somebody else has written the
file since and the document held here is no longer the one their change is in.
Rewriting it would silently undo their work — `added`'s "worst shape a bug of
this kind can take", where the file still opens, still lists the same images,
and has lost what was recorded against them.

Merging is the friendlier answer and was rejected for one reason: it never
tells anybody the file moved under them, so an edit made against a stale list
lands looking as though it had been made against the current one. What
refusing costs is one reopen, and the typing is not discarded — the editor
stays open with it in, which is what makes the refusal a delay rather than a
loss.

The comparison is the *text*, not an mtime and not a length, because
`Stored::rewritten` reproduces a Gson-written file byte for byte. That is
exact in both directions: it cannot miss a change made inside a timestamp's
resolution, and it cannot report one for a file rewritten identically.

**And it was wrong, in the direction that makes a feature useless rather than
dangerous.** `project_file` ends the index with a newline and `json::write`
does not, so a project *this application created* differs from its own rewrite
by one trailing byte — and the check refused every edit to one, having found a
change nobody had made. Every unit test passed while this was true, because
they all go through `project::edited` directly. What found it was writing a
project to a real directory and editing it, which is §8.8's *Run the binary*
rule applied to a file format. The comparison now trims the ends and nothing
else: whitespace outside the document is not a change anybody made, and
anything inside it still is.

#### And then QuPath was asked to read it

The unit tests go through `project::edited` and back through `project::read`,
which is one reader agreeing with one writer — §8.6's *a format checked against
the code that writes it is untested*, applied to the one field a user types
into. So `project_sample` now edits an entry before the recorder runs, and
`ProjectImport` records `DefaultProjectImageEntry.getMetadata()` — the map the
right-click edits, and deliberately not the server metadata block beside it,
those being the two fields §8.3b records mistaking for each other.

QuPath reads all three keys back, in the order they were written, with their
values intact. The keys are awkward on purpose: `H&E` and `A=3` carry the
characters the browser's own `key=value` search splits on, and `noté` /
`réséqué` are non-ASCII, because what QuPath has to agree about is the bytes
and a fixture of `a=1` would agree about nothing. The second entry comes back
with an empty map, which is what says the rewrite touched one entry rather than
all of them.

Two things in the recording beyond the assertion are worth having noticed. The
`ProjectExport.json` half is a project **QuPath wrote** after reading ours, and
it re-serialised the map through Gson as `H\u0026E` and `A\u003d3` — so QuPath
did not merely parse the values, it kept them. And its own entry summary, the
line its browser puts under a name, comes back as
`stain:	H&E | block:	A=3 | noté:	réséqué`. That is asserted too: the map is
what QuPath *read*, and the summary is what a QuPath user would *see*, which is
the distinction §8.6 draws between a file that parses and a file that shows
something.

#### Three things the picture found, and one of them was in this file already

The editor is a modal over the browser rather than a fifth side panel. It is a
task with an end — type, then Save or Cancel — which is what QuPath does with
it, and `panel_cap` divides one budget between the panels that are open, so a
fifth would narrow the four beside it every time somebody edited a key. §8.1c
records what that arithmetic costs when it is got wrong.

`examples/chrome` renders it, and the first picture was wrong three times over:

- **It read as transparent**, the browser's rows showing through the dialog's
  own text. Not a frame colour — `egui` *fades a modal in* exactly as it fades
  a popup in, so the second frame is the dialog at 44% opacity. This is the
  same finding §8.3 records for the sort combo's dropdown, written down in the
  same example file, and walked into anyway. The settling loop that was built
  for the popup now runs for a modal too.
- **The remove button drew as `□`.** `✕` is not in the default font. This is
  the second time — `row_picture` chose two line segments over a glyph for the
  same reason and says so in a comment three hundred lines above. A button's
  label cannot be segments, so it is an ASCII `x`.
- **The key box was narrower than the word `value` beside it.** A `TextEdit`'s
  `desired_width` inside a `Grid` is a desire; the grid sizes columns to their
  content. `min_col_width` is what binds.

None of the three is visible to a test that asserts on values, and all three
are obvious in a 1100×760 PNG. The one that matters most is the second
picture, of the **refusal** — a state reached only by two programs writing one
study, which nobody sees while building the feature and which is the entire
reason the write path is shaped the way it is.

What is still not looked at is the right-click itself: the menu is attached to
every row — metadata belongs to the entry and not to the pixels, so an entry
whose slide has been moved away still takes a note — but nothing has pressed a
mouse button on one. It was in §8.8's *hand on the machine* hole with the row
click and the add button; both have since come out of it, by the hole
turning out to be drawn around the wrong thing — `chrome::draw` returns the
command a press produced, so a synthesised press closes the chain. A
right-click is `PointerButton::Secondary`, and the reason it has not followed
is that the menu it opens is `egui`'s rather than this project's.

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

**Nothing called it.** The format was written, tested against a JSON parser,
pinned for level-0 coordinates — and no menu item, key or button anywhere in the
application invoked it, for as long as this section has said "done". The top of
this file has said "what you find can be saved as GeoJSON that QuPath opens"
throughout.

That is worth recording next to the unit bugs above, because it is the same
failure in a different medium: **a claim asserted in prose that the code does
not deliver**, and the reason it survived is that everything the claim names
does exist and is correct. The writer works. The tests pass. The only missing
piece was the one nobody writes a test for, which is that a person can get to
it. Checking took one `grep` for callers, which is now worth doing for every
item in a list of things that are "done".

It is File → **Save detections…**, or `s`. The menu item and the action read
one predicate — `App::found` — because two would drift into a live menu item
that does nothing when pressed, which reads as a broken save rather than as an
empty one.

**Two measurement columns go with the outlines**, under `qupath_pipeline`'s own
`AREA` and `MEAN` constants rather than strings written at the export: a column
header typed beside a column of numbers stays plausible while naming the wrong
measurement, which is §8.5's stain-vector mistake in a different place. The
detector used to keep the geometry and drop the table, which would have made
the export a picture — QuPath's own export carries exactly these two.

The **frame** is the part worth stating, because it is this section's own
warning applied one level in. `AREA` comes out of the pipeline in the pixels of
the level the run read, and the outlines beside it are pushed to level 0. Left
unscaled, a run at level 1 writes an area sixteen times too small next to a
polygon of the right size — a number of the right kind, in the wrong units,
against geometry that agrees with nothing. So `columns_of` scales the area by
the square of the downsample and leaves the mean, which is an optical density,
alone: one frame per file, and "px" in an exported file means level-0 pixels
throughout. µm² is deliberately not written — QuPath's calibrated column has a
name of its own, and guessing it is how a file looks right and reads wrong.

The end-to-end test runs a real 384x384 square of `CMU-1-Small-Region.svs`
through the pipeline and asserts the two column names reach the file and that
the outlines are in slide coordinates rather than the region's. The unit tests
either side of it cover the halves; what nothing else covers is that the halves
are joined, which is exactly what was missing for several sessions.

**One number formatter served two kinds of number, and the coordinate rule
won.** Measurements went through `write_number` — two decimal places, which is
correct for a coordinate and is QuPath's own figure rather than a tidy one:
`GeometryTools.DEFAULT_FACTORY` is built on `new PrecisionModel(-0.01)`, a grid
size of a hundredth of a pixel, so a file carrying more digits than that stores
precision QuPath discards on the way in.

Applied to a measurement it is simply wrong. `Nucleus: Haematoxylin OD mean`
runs from about 0 to 2, so two decimals leave it roughly two significant figures
and about two hundred distinct values — for the number
`qupath_pipeline::ObjectFilter` thresholds on, which means an exported file
could not reproduce its own run's filtering decisions. QuPath does not round:
`MeasurementListTypeAdapter.write` hands the value to `JsonWriter.value(Number)`.

It is the §8.1d family again, and it hid the same way. The *other* column,
`Nucleus: Area (px)`, is a count of labelled pixels: at level 0 — the one
configuration anything is ever run at — it is an integer, and rounding an
integer to two decimals changes nothing. The column that could show the bug is
the one whose values only become fractional at a level nobody runs.

`write_measurement` is the split, and what it writes is exactly right rather
than merely more: a detection's measurements are stored by QuPath as **`float`**
(`PathDetectionObject` asks `MeasurementListFactory` for
`MeasurementListType.FLOAT`), `Measurement::values` is `f32`, and `{}` on an
`f32` is the shortest decimal that reads back as the same float. So the 32 bits
that go in are the 32 bits QuPath ends up holding, and
`parity_geojson_import.rs` asserts that bit for bit rather than within a
tolerance.

**It has now been opened by QuPath, which is what §8.8 said had never happened.**
`parity/java/GeoJsonImport.java` reads `parity/samples/detections.geojson`
through `PathIO.readObjects` — the entry point behind *Open objects* in the GUI,
not a hand-driven Gson call — and records what came back;
`crates/qupath-viewer/tests/parity_geojson_import.rs` asserts that reading is
the overlay the sample was built from, with no JVM in the loop. Geometry agrees
to a worst relative 4.8e-8, which is `f32` rounding in QuPath's own ROIs
(`PackedCoordinateSequenceFactory.FLOAT_FACTORY`) and not a disagreement about
anything; areas agree exactly.

The sample is seven objects and every one of them is a case: downsample 1.0, 4.0
and 2.5, an SVS pyramid's 4.000123, an eight-sided traced outline, and an empty
ring and a two-vertex ring that the writer skips. **The skipped pair is what
puts the measurement columns under test** — rows are indexed by object, so a
writer that advanced them by written feature instead would put object 5's area
on object 3, a number of the right kind on the wrong outline that nothing
downstream can detect. QuPath is what reads those columns back.

Two guesses about the reader were wrong and only running it said so: QuPath
**keeps** the closing position GeoJSON requires, so an eight-vertex ring comes
back with nine points, and an axis-aligned four-corner ring does not come back a
polygon at all — the reader recognises it and builds a **Rectangle** ROI. Neither
moves a coordinate, and both would have failed an assertion written from the
specification instead of from the run.

The chain has one weak link and it is guarded:
`the_committed_sample_is_what_the_writer_produces` asserts the committed file is
what `geojson::write` produces today. Without it a change to the writer would
leave the fixture describing QuPath's reading of a file nothing produces any
more, and every other assertion would keep passing about the wrong thing.

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

### 8.9 Whole-slide detection — **done**, and the entry that asked for it was wrong

§8.8 listed "tiled whole-slide detection" at the top of what was missing, on
the reasoning that "Detect cells" makes one `run_collecting` call, that
`MAX_PIXELS` therefore bounds it to the middle 21% of a fitted CMU-1, and that
fixing it meant a tile loop with overlap and seam deduplication — "a real piece
of work rather than a parameter change".

Every step of that was wrong, and it is kept here rather than quietly replaced
because the way it was wrong is the useful part: **it was reasoning about the
application without reading the layer underneath it.** `qupath_pipeline`
already tiles, already runs those tiles in parallel, and already removes the
duplicates that land on seams — `run_collecting`'s own documentation says
"duplicates across tile seams already removed", and the whole-slide run reports
the stage doing it (428,496 → 382,432, a 10.8% cut). The tile loop I was
proposing to write exists, is tested, and is what ROADMAP §2 was about.

What actually stood in the way was one constant, set from a bad measurement.
`MAX_PIXELS` was 64 Mpx, chosen against a rate of 4.5 Mpx/s to hold a run near
fifteen seconds. That rate came from two views that were mostly **glass** and,
worse, too small to occupy the thread pool — 4 tiles across 20 threads. Measured
on a region that saturates them, the rate is **40.8 Mpx/s**, nine times higher.

The memory half was a guess that turned out to be answerable by measuring:

| region | tiles | peak resident |
|---|---|---|
| 17 Mpx | 4 | 1092 MB |
| 212 Mpx | 48 | 5657 MB |
| 1514 Mpx (whole slide) | 5776 | 5609 MB |

Peak is set by **how many tiles are in flight**, which is the thread count, and
not by the region — the whole slide costs no more than any region large enough
to fill the threads. The objects themselves are 340 bytes each, so all 382,432
of CMU-1's nuclei add about 124 MB on top of that ceiling.

So the fix was the parameter change I had explicitly ruled out. `MAX_PIXELS` is
2048 Mpx, a fitted CMU-1 runs the whole slide at level 0 in **33.5 s**, and
`a_fitted_view_of_cmu1_reads_the_whole_slide` pins it with the slide's real
extent so the cap cannot drift back under a whole slide unnoticed. The cap is
still a cap: a slide larger than CMU-1 is shrunk about its centre and labelled,
as before.

The rule this yields belongs beside §6's: **before writing a component, read
what the layer below already does.** §6's rule is about not trusting a
measurement you have not tried to falsify; this one is about not trusting a
*gap* you have not tried to find already filled. The cost of getting it wrong
here would have been a duplicate tile loop wrapped around a working one, which
is the kind of thing that looks like progress.

What remains genuinely absent is **cancellation and a progress fraction**. §3.6
lists them as not offered, and at 33.5 s they matter more than they did at 1.24
s: the run is on a worker thread with the window live and `Detector::start`
free to supersede it, so the application stays usable, but a person who starts
a whole-slide run has no way to stop it and no idea how far along it is. That
needs a callback through `run_collecting`, which is a pipeline change rather
than an application one.

### 8.10 Cancellation and a progress fraction — **done**, in the pipeline

§8.9 left these as the top gap: a whole-slide run is 33.5 s, and a run that long
with no way to stop it and no sense of how far along it is is what a person
meets first. §3.6 had listed both as "not offered" since the beginning, and the
reason given was sound — `run_collecting` was one synchronous call with nothing
to poll, so a bar beside it would have been advancing on a timer, which §6 calls
an invention rather than a measurement.

What changed is not the honesty rule but what is knowable. The pipeline already
tiles, so there is a **denominator that exists before the first tile starts**:
the run's own tile count. Tiles finished over tiles total is a measurement.

**A handle, not a callback.** The obvious shape is a closure the loop calls, and
it is the wrong one here: the loop is a rayon `par_iter`, so a callback fires
from whichever of twenty workers finished a tile, and every caller would need it
`Sync` and would be drawing its progress from inside the pipeline's threads.
`Progress` inverts that — the pipeline only ever writes two atomics, wait-free,
and the reader polls when it suits. The viewer reads it once a frame. A reader
that never polls costs the run two relaxed increments per tile.

**What the fraction is honest about.** Tiles, not time. Tiles over glass are far
cheaper than tiles over tissue, so it advances in jumps, and no finish estimate
is offered beside it because a linear extrapolation would be exactly the fiction
the old elapsed-seconds display was written to avoid. Before the grid is built
there is no denominator, and `Detector::fraction` returns `None` for that gap
rather than `Some(0.0)` — a bar at zero and a bar that is stuck look identical.

**The bug this found, which is the part worth keeping.** `Progress::start`
originally cleared the cancelled flag, so a handle could be reused. That is the
tidy-looking version and it is a silent failure: `cancel` and the run's start
happen on *different threads* — in the viewer, the UI thread and a worker — so a
stop requested in the window between them is wiped by the run it was meant to
stop. Clearing is now the caller's job, in `Progress::reset`, done before the
worker is spawned.

It was caught because `cancelling_stops_the_run_before_it_finishes_its_tiles`
asserts the run stopped **early**, not merely that it returned an error. A test
that checked only the error would have passed against an implementation that ran
all 16 tiles and reported cancellation at the end — which is the exact failure
this feature exists to prevent, and is indistinguishable from the outside. That
is §6's rule in its narrow form: know what value would have made the check fail.

**What stopping actually costs, measured.** The flag is read at the top of each
tile, so a run ends within about one tile — and one tile is not negligible. A
2052 px tile at level 0 is around 1.7 s on one thread. `examples/stop_a_run.rs`
runs the whole slide, asks it to stop at three seconds, and reports: stopped
**1.45 s later**, after 36 of 391 tiles, with the fraction reading 8.95% when
asked. The first version of this documentation claimed "well under a second at
any level", which that measurement contradicted; the interface now says "a
second or two" because that is what it does.

The integration tests cancel *before* the run starts rather than racing a
sleeping thread against a fixture that finishes in a second — deliberately, so
they test the same branch without being flaky. `stop_a_run` is the other half,
and it is an example rather than a test for the reason `gestures.rs` is: it
measures wall-clock behaviour on a real slide, which is a thing to look at
rather than a thing to assert.

**In the window.** The status bar shows a real bar with the percentage written
into it, and a `Stop` button beside it. Escape stops a run too — it already
meant "abandon what is in progress" for a zoom rectangle, and a whole-slide run
started by mistake is the same gesture. The band wins when both are live, since
a person drawing a rectangle during a background run means the rectangle. A
stopped run keeps the previous overlay and says "Detection stopped after 36 of
391 tiles", which tells the person how much of the slide the outlines they are
still looking at do *not* cover. It never says "failed": stopping was the
instruction, and `Outcome::Cancelled` is a separate variant from
`Outcome::Failed` so a reworded error string cannot turn one into the other.

**And "replacing a run" was not stopping one.** `Detector::start` said it
replaced any run already going, and what it replaced was the *result*: the
generation counter meant a superseded run's answer was dropped on arrival. The
worker itself went on to the end.

Two things followed. The pipeline saturates the machine, so pressing `d` twice
— an ordinary thing to do after moving the view — ran two whole-slide
detections at once, each taking every core it could get, with one of them
already known to be unwanted. And the two shared one `Progress`: the detector
held a single handle for its life and `reset` it before each run, so the old
worker kept counting finished tiles into counters that had just been zeroed
underneath it and were now being reported as the *new* run's progress. The bar
was two runs added together over one run's total, and `Stop` reached whichever
worker read the flag first.

Each run now takes a fresh handle, and the old one is cancelled before it is
let go of. That also removes the reason `Progress::reset` was being called at
all: a new handle is already clear, so the ordering rule its documentation
states — reset before you hand the handle over, never inside the run — has
nothing to catch here. The rule is still right for a handle that *is* reused;
the point is that the viewer no longer reuses one.

`clear` had the same shape: it discarded the overlay and the result, and left
the work running over a slide that had just been closed. It cancels now.

The tests hand `start` a path that does not exist, which works because the
level and the region are decided on the calling thread and only the *slide* is
opened on the worker: the run fails immediately and the handles are still
exactly what the supersede did to them.

### 8.8 What is still missing

In the order it is worth having. Two entries here are neither missing nor done:
the first is **decided against**, and `data.qpdata` below is **refused**. Both
keep their place rather than being deleted, because a reader who wonders why the
block is absent needs to find the reasoning where they look for the gap.

- **Stored image metadata — decided: not written, and this is the reasoning.**
  The comparison this entry waited on is made and the two agree, which is what
  turned it from an open question into a settled one. It is settled the way it
  was leaning: the block stays unwritten. Not for want of the information —
  every field of it is available and checked — but because of what the two
  sides of the trade actually are, which is below.

  `ProjectExport.json` holds QuPath's own block, so "nobody knows what to write"
  was already answered — `levels`, `preferredTileWidth` and
  `preferredTileHeight`, `isRGB`, `channels` as objects, `pixelCalibration` in
  microns. And carrying it from an open `Slide` was never an obstacle either:
  `App::add_to_project` holds one, `Slide::levels` is the table, and `Level` has
  the tile sizes.

  What stopped it was that the block **replaces** what QuPath would otherwise
  have read. `AbstractServerBuilder.build` calls `setMetadata` with it when the
  image is opened, so a level table that disagrees with OpenSlide's is not a
  worse description of the slide — it is QuPath reading the wrong pixels,
  silently, for as long as the project exists. `Slide::open` keeps the tiled
  directories and drops the label, macro and thumbnail; OpenSlide's Aperio driver
  makes its own choice, and the two had never been compared.

  **They have now, and they agree in every field.** `parity/java/OpenSlideLevels
  .java` opens the slide through `qupath-extension-openslide` and records what
  QuPath ends up with; `parity_openslide_levels.rs` checks it against
  `Slide::levels` on the same file. Level count, every width and height, every
  downsample, the preferred tile size and the pixel size all match, on a
  three-level slide and a single-level one. So the block is now *safe* to write
  rather than merely tempting.

  **What that changed is the standing of §8.3's judgement, not the answer.**
  Before it, "let QuPath read the slide itself" could have been
  laziness dressed as principle — there was no measurement saying the two would
  agree, so declining to write the block was as much precaution as preference.
  Now there is one, and the judgement stands as a choice made *with* the
  information rather than for lack of it. That is the whole value of the
  recording, and it is worth having on its own: an item moves from *blocked* to
  *decided*, which is a different state from *pending*.

  **And the decision is not to write it, because the two sides of the trade are
  not the same kind of thing.** What writing it buys is a performance
  optimisation: QuPath does not open the slide to learn a level table it could
  have been told. That is one OpenSlide open per entry, on the first open of a
  project — real, but small, and a cost QuPath's own users already pay on every
  project QuPath itself writes without a block. What it risks is silent wrong
  pixels. `setMetadata` **replaces** what OpenSlide read, so a block that goes
  stale — the file at that path replaced, re-scanned at a different resolution,
  swapped for a re-export — is the wrong level table applied to the right path,
  with no reader anywhere to flag it. The optimisation saves milliseconds; the
  failure corrupts an analysis in a way that surfaces months later, if ever.
  Those do not trade at par.

  This is §8.3b's rule pointed outward, and it is worth seeing as the same rule.
  `read` keeps every byte it did not touch precisely because a rebuilt index
  silently loses what it did not understand. Writing a block whose truth this
  application cannot later re-verify is that mistake made in the other
  direction: claiming authority over a fact it will not be around to maintain.

  **Three things would change the answer, and none of them hold here.** *One*, a
  vendor whose level table QuPath gets wrong — a block would then be a
  correction rather than a duplicate, buying accuracy instead of speed. Both
  slides on this machine are Aperio, and the agreement above is evidence about
  the pair `Slide::open` and OpenSlide's Aperio driver, not about every driver
  OpenSlide has; whoever revisits this should record a fixture for the vendor
  they are revisiting it for, before writing anything. *Two*, a path that cannot
  go stale — content-addressed or immutable storage removes the staleness
  argument entirely and flips the calculus. *Three*, slides this project can read
  and QuPath cannot, where a block is the only way QuPath learns anything at all.
  The JP2K entry is nearly that and backwards: QuPath reads it and this project
  does not.

  **If the open cost ever actually bites**, on a study with hundreds of entries
  and someone complaining about first-open latency, the answer is not this block.
  It is `thumbnail.jpg`, which is already written, and which buys the same "do
  not re-read every slide" with none of the authority to override. That is
  strictly the safer half of one optimisation, and the asymmetry is the whole
  argument: a wrong thumbnail is a wrong *picture*, which a person notices at
  once. A wrong level table is wrong *pixels*, which nobody does.

  **The blocker this entry named for a long time was not real, and the mistake
  is worth keeping.** It read as blocked on "a native library, and there is none
  on this machine", which was checked with `ldconfig` and `apt` — and QuPath does
  not use the system library at all. The extension depends on
  `io.github.qupath:openslide:<version>:<platform>`, a jar carrying its own
  `libopenslide.so` that JNA extracts at load time
  (`OpenSlideLoader.tryToLoadJnaInstance` → `Native.extractFromResourcePath`).
  Nothing needed installing, and no password was needed; the evidence for the
  blocker was gathered about the wrong artefact. *An image opened from a project*
  below was blocked on the same non-blocker.

  Two things about the recorder are deliberate, and both would be easy to get
  wrong. It records the **server**, not `openslide_get_level_downsample`:
  `OpenslideImageServer` explicitly declines that value — OpenSlide averages the
  width and height ratios, which is inexact whenever the true downsample is an
  integer — and rebuilds the table from the level dimensions, so the native call
  answers a question QuPath never asks. And it records **two** slides, because
  OpenSlide's fallback tile size is 256: on CMU-1 alone "the tile sizes agree"
  would be two defaults coinciding, and CMU-1-Small-Region's **240** is the only
  reason that comparison is a measurement. A test asserts the 240 is still there,
  so a re-recording cannot quietly turn it back into a default.
- ~~**A thumbnail.**~~ **Done, and the entry was wrong about what it needed.**
  It read as blocked on a JPEG encoder because it assumed a row's picture is
  `data/<id>/thumbnail.jpg`. QuPath's own browser opens the image when that file
  is absent, so the picture comes from the pixels on both sides; §8.3 has the
  call site and what it cost to find out. The encoder is still owed — see below
  — but nothing about the *browser* was ever waiting on it.
- ~~**The rest of `ProjectBrowser`.**~~ **The reader change landed and took
  four things with it — see §8.3b.** The entry below this one predicted that:
  the metadata map, the `key=value` search and `has_data`, the metadata sort
  keys and name masking were one reader change underneath, and they were.
  ~~What is left of QuPath's pane is **right-click to edit an entry's
  metadata**.~~ **Done, §8.3c, and the unanswered question in it was the
  design.** It was right that the mechanism was there — `edited` is `added`'s
  shape — and right that what was missing was an answer to the file having
  changed underneath. That answer is *refuse and say so*: a merge never tells
  anybody the file moved under them. The check was then wrong in the direction
  that makes a feature useless rather than dangerous, refusing every edit to a
  project this application had itself written, and only writing one to a real
  directory found it.
- **`data.qpdata`, which should stay missing.** Java serialisation tied to
  Java's object graph. A guessed version is how a project becomes corrupt in a
  way someone discovers much later, having trusted it. The detections belong
  beside it as GeoJSON, which both sides can read.
- ~~**A JPEG encoder for `thumbnail.jpg`.**~~ **Done, and QuPath reads what it
  writes.** `thumbnails::encode_jpeg` encodes the picture the browser already
  makes, and `add_to_project` writes it beside the entry's JSON. What the file
  buys is what this entry always said: QuPath not re-reading every slide in a
  study the first time it opens it. It was never in front of anything.

  It lives in `qupath-app`, not in `qupath-viewer`, and that is the §7 rule
  holding rather than an accident — the crate that defines the format depends on
  `qupath-slide` alone, and `project::files` already said it returns the JSON and
  leaves this file to its caller. `Output::contents` is a `String`; these are
  bytes. The dependency is `image` with `default-features = false` and only
  `jpeg`, which adds **no new crate to the tree**: `image` was already there
  through `arboard` under `egui-winit`. `zune-jpeg`, which reads the tiles, is
  decode-only.

  **The alpha channel is the whole trap, and it is a §6 failure exactly.**
  `Thumbnail::pixels` is RGBA because a texture upload wants it; JPEG has no
  alpha. Handing four-channel data to an encoder expecting three does not fail —
  it reads each pixel's fourth byte as the next one's first, so the picture
  shears by a channel and slides further out of register down the image. It
  produces a valid JPEG of the right size in false colour. The size-only test
  passes on it; only decoding and comparing against the source pixels catches
  it, which is what `a_thumbnail_round_trips_through_a_decoder` does — and it
  was confirmed by reintroducing the bug and watching that test go red while the
  other stayed green. It decodes with `zune-jpeg` rather than with `image`, so
  the check is not one implementation agreeing with itself.

  **And then QuPath was asked to read one.** `ProjectImport`'s
  `thumbnailReadable` had recorded `false` for as long as it existed, which
  measured only that QuPath tolerates the file's absence — it could never say
  whether a JPEG written here is one `ImageIO.read` accepts. `record-project.sh`
  now writes thumbnails before recording, and the field reads **`true`**. The
  JP2K entry still reads `false`, because this project cannot decode its pixels
  (§3) and so writes it no thumbnail: one recording now carries both the format
  being accepted and the absence still being tolerated, which is a better
  fixture than either alone.
- **Driving the real window.** The application has been launched on a project
  and a slide together and photographed — `WINIT_X11_SCALE_FACTOR=2`, the list
  drawn, the add button live, no stderr — and those photographs are what found
  the relative-path bug above, the row that could not say its picture was never
  coming, and the current-image fill on a row that names no image. ~~But nothing
  has *clicked* a row, and nothing has pressed the add button; both are checked
  through `App` instead. The same hole as the overview's click-to-jump and
  escape during a run: reachable only with a hand on the machine.~~ **All four
  are out of it now, and none of them needed a hand** — the hole was drawn
  around the wrong thing every time.

  **A row has now been clicked, and it needed no hand either.** The hole was
  drawn around the wrong thing: what `App` cannot reach is not the click but the
  *pixel position*, and `chrome::draw` returns the `Command` a press produced,
  so the whole chain from a coordinate to what the application is asked to do
  closes without a window. `pressing_a_row_asks_for_that_row_and_only_that_row`
  presses each of the three row states and asserts on the command; the same
  `lay_out` the fill test uses, with a press and a release added.

  Three things it pins that `App` could not. A row asks for **its own** slide —
  a mutation routing every press to the first entry fails it, and that is the
  bug that opens the wrong image silently. A row whose slide is **missing still
  asks**, because the browser's mark is `Path::is_file` from when the project
  was opened and the row's own hover text promises "Clicking still tries it";
  making it refuse fails the test with that sentence in the message. And
  pressing the empty space **below** the list asks for nothing, which is the
  outward-facing half of the rule that a row takes the click across its whole
  width.

  **One assertion in it is weaker than it looks, and it is labelled as such.**
  Pressing the un-openable row yields no command — but `openable` *is*
  `slide.is_ok()` and the command is emitted inside `if let Ok(path)`, so that
  row cannot ask for anything whatever its `Sense` is. Giving it
  `Sense::click()` leaves the test passing, which was checked rather than
  assumed. The sense controls the fill and the cursor, and the fill test above
  is what guards that half.

  A fourth test, `a_row_is_where_these_tests_think_it_is`, exists because a
  click that misses produces no command and a hover that misses produces no
  fill — so a coordinate that stopped being right *satisfies* both of the
  assertions above. It asserts the three positions land on three different rows,
  in the order the project lists them. `examples/chrome` records the same
  failure twice already, once for a click landing on no popup and once for a
  builder rewrite landing on the wrong entry.

  **And one flake worth keeping.** All four tests shared a fixture directory, so
  one test's `remove_dir_all` ran while another was reading the project out of
  it — the fill test failed only when run alongside the others and passed alone.
  A fixture torn down under a reader is a flake that looks exactly like a bug in
  the thing under test. Each test now names its own directory.

  **The add button has now been pressed too, and by the same route.** It is a
  coordinate like any other — `ADD_BUTTON` beside `ROWS`, read off the layout
  rather than computed — and pressing it in the pane produces `AddToProject`.
  What is worth the test is the two states in which it must produce *nothing*:
  a project open with no slide showing, and a slide the project already holds.

  **That pair is not the weak assertion the row's `Sense` was.** The button is
  `add_enabled(app.addable().is_ok(), …)`, so what refuses the press is `egui`
  and not a branch here, which is exactly the shape of the assertion labelled
  weak above. The difference is that the mutation is reachable: replacing the
  predicate with `true` leaves *both* silent states asking for `AddToProject`,
  and that was run — it fails with the disabled hover text in the message
  ("Open a slide first (o); it is the one that gets added."). The two
  consequences are the reason to pin it. With nothing to add, the command is
  `add_to_project` rewriting the index with nothing to write; with the slide
  already in the project, it is a second entry for one image, which is a second
  `data/` directory and a second row in QuPath's own browser. Neither says
  anything on screen when it happens.

  Three states of one button rather than one, for the fill test's reason: that
  it stays quiet with nothing to add means nothing unless it speaks when there
  is something. And `the_add_button_is_where_these_tests_think_it_is` guards the
  coordinate, more sharply than the rows needed it — two of the three assertions
  are that the press produces **no** command, which is what pressing empty space
  produces, so a coordinate that drifted off the button would leave two thirds
  of the test passing while checking nothing.

  Unlike the hover and like the row press, this found no bug. The behaviour was
  right; the check was missing.

  ~~What is still in the hole is the overview's click-to-jump and escape during
  a run.~~ **The hole is empty.** Both came out of it, and neither needed a
  hand — which is now true of everything that was ever listed in it: the row
  press, the add button, the hover fills, the sort dropdown, and these two.
  What still needs a hand on the machine is in the bullet below this one, and
  it is a different list: the trackpad, resizing, and the window manager. Those
  are not gestures a synthesised pointer can stand in for.

  The overview's click-to-jump came out by the route the row press came out by:
  `tests/overview.rs` presses the picture, drags across it, and presses beside
  it, all through the `Command` `chrome::draw` returns. The harness the two
  files share now lives in `tests/gestures/mod.rs` rather than being copied
  into the second of them — the six frames, the gesture withheld on the first
  and the repeat on every frame after are three decisions whose reasons live in
  comments, and a copied reason is one that can stop being true in one place.

  **Escape came out by the same correction applied a second time, and that is
  the part worth keeping.** It read as blocked because `App::key` takes a
  `&Window` and no test can build one — which is true, and is not what the
  gesture is. What needs the window is the *redraw* a command may ask for. What
  escape actually does is decide, in order, which of two things in progress it
  abandons, and a decision needs nothing. `App::escape` now returns that
  decision — `Escape::Band`, `Escape::Run(Command)` or `Escape::Nothing` — and
  `key` is the two lines that carry it out. Three tests in `state.rs` assert the
  ordering.

  The ordering is the whole behaviour and it is not the obvious one. Both
  gestures mean *undo the thing I just started*, and resolving them the other
  way round throws away a whole-slide run — up to a minute and a half of every
  core — because someone was drawing a zoom rectangle while it went. It is also
  silent: the band vanishes either way, so the frame after the keypress looks
  identical in both cases, and the difference only appears when the run never
  comes back. That is §6's shape again — two behaviours that draw the same
  frame — and a picture is no use against it whichever way it is looked at.

  Four mutations, each caught by the test that claims it. Stopping the run
  before taking the band fails the ordering test; reading the band without
  taking it fails the same one, because the second press then finds the band
  again instead of the run; returning `Command::Detect` where it should return
  `Command::StopDetecting` fails the two tests that name the command — a
  keypress wired that way starts a *second* run over the region, which on
  screen is the first one appearing to take longer; and claiming the key when
  nothing is in progress fails the third.

  What the run is pointed at is `/nonexistent/slide.svs`, deliberately.
  `Detector::start` picks the level and the region on the calling thread and
  only opens the file on the worker, so the run is in flight the moment `start`
  returns and fails immediately afterwards without reaching `poll` — where a
  run over a real slide would race the keypress under test, and a test that
  passes because the work had already finished is a test of nothing. The trick
  is borrowed from `detect.rs`, where `starting_a_run_stops_the_one_it_replaces`
  uses it for the same reason.

  What is *not* covered is the two-line match arm in `key` that runs the
  returned command, because that is the line the `&Window` is actually on. It
  is as small as that seam gets, and it is the honest residue rather than a
  claim of completeness.

  **The sort combo's dropdown was in the hole for about an hour and is not any
  more**: a popup only exists once its box has been pressed, so
  `examples/chrome` now presses it — a synthesised click, and then frames until
  the fade-in finishes. §8.3 has what that cost and what it found.

  **What the click tests add to §8.4's unit test is a factor of one.** The
  arithmetic there is checked and stays checked; what it cannot see is what the
  *pane* puts either side of it — the picture's offset inside a panel, and the
  `scale` the picture is fitted to that panel by. `overview_pane` divides the
  click position back by that scale, and in every window anyone runs this in
  the scale is exactly **1.0**: the panel asks for 240 points and the picture of
  `CMU-1-Small-Region.svs` is 112 wide, so dividing by the factor, multiplying
  by it and forgetting it are three programs that agree. Six of the seven tests here
  run at 1.0 and are worth having for the offset and the mappings; the seventh
  lays the interface out in a 200-point window, where the panel is capped to
  half of it and the picture really is fitted down.

  That split is not a guess about which test carries which claim — it was run.
  Dropping the `/ scale` from the pane leaves **six of the seven passing** and
  fails only `a_pane_narrower_than_the_picture_still_lands_where_it_was_aimed`,
  by a third of the slide: a click on tissue landing on background. Three more
  mutations, each caught by the test that claims it — swapping the axes fails
  all four that check where a jump lands, dropping the `- rect.min` offset
  fails four, and narrowing `clicked() || dragged()` to `clicked()` fails the
  drag alone.

  **The axis swap is only caught by four of them because running it changed
  one.** The drag test first dragged to (0.8, 0.6) of the picture, and the
  picture is 112 by 150: that is 89.6 points across and 90.0 down, so swapping
  the two moved the jump by eight slide pixels and the test passed through the
  mutation. Fractions that differ are not coordinates that differ, and a test
  whose two axes come out nearly equal in pixels is blind to every mistake that
  exchanges them. It drags to (0.85, 0.25) now. The other three tests had
  caught the swap regardless, which is exactly why this one had to be run
  rather than reasoned about — a mutation caught by the suite is not a mutation
  caught by the test that claims it. This is §8.0's rule met in the interface rather than in the
  pipeline — *a conversion whose factor is 1 in the development setup is
  untested, not correct* — and it is the fourth factor of one to turn up here.
  The difference is the outcome: scale factor 1.0, downsample 1.0 and level 0
  each hid a live bug, and the fit-to-panel factor hid nothing. None of the
  four mutations found one. The pane was right; what was missing was any way
  for it to be wrong and be noticed, which is the same thing the add button's
  three states were worth and is a weaker claim than the hover's.

  ~~The hover states of the browser's rows are in that hole now too — the fill
  under a hovered row has been reasoned about and drawn, and never seen.~~
  **Out of it, and looking cost a bug immediately.** A hover is a
  `PointerMoved` and nothing else, so the machinery the combo needed already
  covered it: `render_hovering` moves the pointer and then settles, because
  `egui` *interpolates* a widget towards its hovered colour rather than
  switching to it and the frame the pointer arrives on is a picture of that
  animation part-way through.

  What the two pictures showed is that **the fill said nothing**.
  `chrome-project-hover.png` and `chrome-project-hover-unopenable.png` were
  indistinguishable at the fill — an entry built by a transform this cannot
  open took byte-identically the same `(70, 70, 70)` as a slide that opens. The
  rows carry different senses (`Sense::click()` against `Sense::hover()`, the
  second so the context menu still reaches them) and the fill was drawn for
  both, so the only thing it communicated was that the pointer was there, which
  the pointer already communicates. The greyed text was carrying the whole
  difference, and it is the quieter of the two: a row that lights up and then
  ignores the press is the interface promising something.

  The fill is now `openable && hovered`, and
  `only_a_row_that_opens_fills_under_the_pointer` pins it — three layouts of
  one project, because the claim is a *difference*: the un-openable row not
  filling means nothing unless the openable one does, on the same list, and the
  neutral layout is what says a fill seen at all is the hover's. It reads the
  `Shape::Rect` rather than the rasterised pixels, since a rounded rectangle
  tessellates to a fan carrying the same colour repeated and reading it back
  out of one is reading it out of a rasteriser. Confirmed by restoring the old
  condition and watching it fail with `#46_46_46_FF` — the colour the pictures
  had shown.

  **This is §6 again, and the fourth time in this file the picture is what
  found it.** Nothing about the old code was wrong to read: the comment above
  it said hovering fills a row while the pointer is over it, and it did. What
  no reading produces is the two rows side by side.
- **A look at the interface in a real window — started, and it paid at once.**
  The application has now been run and photographed on X11 at
  `WINIT_X11_SCALE_FACTOR=2`, which is what an offscreen render cannot be: a
  headless run and a development machine both have a scale factor of 1.0, and
  the two unit bugs above are invisible at 1.0 by construction. The first
  photograph is what showed `Fit` leaving slide behind the status bar.
  What is *still* unlooked-at is everything that needs a hand on the machine —
  the trackpad, resizing, and the window manager. Hover came off this list by
  being synthesised rather than by being touched, which is the entry above.
- ~~**An image opened from a project.**~~ **Done, and the blocker was not
  real.** Both formats had been through QuPath itself — §8.6 and §8.3 — but
  `ProjectImport` stops before the pixels, and this entry recorded that as
  needing `qupath-extension-openslide` and "a working native library, and there
  is none on this machine". That was checked with `ldconfig` and `apt`, and
  QuPath uses neither: the extension depends on `io.github.qupath:openslide`,
  a jar carrying its own `libopenslide.so` that JNA extracts at load time. The
  evidence for the blocker had been gathered about the wrong artefact, and the
  same mistake was holding *Stored image metadata* above.

  `parity/java/ProjectOpen.java` now opens every entry through
  `entry.readImageData()` — what double-clicking a row reaches, and the path
  that goes through `getServerBuilder()`, which is where a stored block would be
  applied. `parity_project_open.rs` checks the result against this project's own
  reader. Both entries open as `OpenslideImageServer`, the dimensions, level
  counts and tile sizes agree, and — the part that is worth the JVM — **an 8×8
  block of tissue decodes byte-for-byte identically** on the JPEG slide.

  It was also not "testing OpenSlide rather than the format", which is what this
  entry argued. Every field it checks is one a wrong project file would corrupt:
  a `ServerBuilder` that will not build, a URI resolving to the wrong slide, or a
  metadata block `setMetadata` lays over what OpenSlide read. The pixels are what
  separate those from a project that merely *lists* correctly.

  **The placement of that block is the whole value, and the first version got it
  wrong** — a fourth instance of §6's recurring mistake. It clamped the large
  slide's tissue coordinates into the 2220×2967 small region, landing on its
  bottom-right corner: background, mean 243 against tissue's 99, where two
  readers agree by both returning white. The uniformity check missed it because
  background is not perfectly uniform. Both sides now name tissue per slide, the
  recorder refuses a block spanning fewer than 24 levels, and a test asserts the
  committed fixture still spans more.

  One entry is deliberately not pixel-compared: `CMU-1-JP2K-33005.svs` is Aperio
  JPEG 2000 and this project decodes baseline JPEG only (§3). Its metadata is
  still checked. The fixture carries `pixelsComparable` per entry rather than
  letting the test infer it from a file name, and a separate test asserts at
  least one entry *is* comparable — a recording in which every entry were JP2K
  would otherwise make the pixel check vacuous while still passing.
