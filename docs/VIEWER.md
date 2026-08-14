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
  scripting. If those are ever wanted they are a separate decision.
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
  `Sync` and a loader pool needs no lock around it.
- `Slide::read_tile_into` fills a caller-owned `Vec<u8>`, so a loader thread
  allocates nothing per tile once warm.
- `Level` carries `columns`, `rows`, `tile_width`, `tile_height` and a computed
  `downsample` — every number the viewport math needs.
- `Slide::level_for_width` exists and nothing calls it. Step 3 is its first
  caller.

Known limits inherited from the crate, both fine for now:

- Baseline JPEG only. `data/CMU-1-JP2K-33005.svs` will not open, by design
  (`SlideError::UndecodableCodec`). See `ROADMAP.md` §3.
- Edge tiles come back full-sized with padding. `read_region_into` is what
  crops. A viewer drawing tile quads must either crop or clamp its texture
  coordinates — decide once, in step 2, and write down which.

## 2. Stack

- **`winit` + `wgpu`** — window and rendering. The draw call is textured quads
  in a grid; this does not need a game engine.
- **`egui`** via `egui-wgpu` for chrome. Immediate-mode suits a UI whose state
  is almost entirely derived from viewport state. `iced` is the retained-mode
  alternative and would fight harder for a custom GPU surface underneath.
- **A thread pool + channels** for tile loading, not async. Decode is
  CPU-bound; what is actually needed is a request queue with **cancellation**,
  because panning makes half the in-flight requests garbage before they land.

Note the workspace already depends on rayon, and the loader must **not** be a
`par_iter` nested inside anything (`ROADMAP.md` §5: "Do not nest `par_iter`").
A dedicated pool for tile loading, separate from the detection pool, is the
safe shape.

## 2b. Running it

Both examples take a slide path, defaulting to `data/CMU-1.svs`.

```sh
# The viewer. Drag to pan, scroll to zoom, f to fit, escape to quit. The title
# bar carries the level and the tile count, which is the level selection made
# visible -- zooming out should drop the tile count, never raise it.
cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs

# No window, no GPU: writes the whole slide at the level the viewport picks,
# plus 1024x1024 at level 0 from the profiled region, then prints the level
# table across scales.
cargo run -p qupath-viewer --example dump --release -- data/CMU-1.svs

# No window, but through the GPU: renders the same geometry the window renders
# into an offscreen texture and writes it as a PNG, including a zoom sequence
# anchored on tissue that crosses every level boundary. This is what to run
# when the window looks wrong and you need numbers instead of an impression.
cargo run -p qupath-viewer --example render --release -- data/CMU-1.svs

cargo test -p qupath-viewer
```

`--release` matters for both: a debug build decodes JPEG tiles slowly enough to
be annoying.

The window needs a display and a GPU. It runs on Vulkan here; wgpu will fall
back to other backends but the fallback is untested. If it cannot find an
adapter it panics at startup with `expect("adapter")` rather than opening a
blank window, which is the intended failure — `dump` is the path that needs
neither.

Tests need no display and no GPU, by construction (§6). The integration tests
need `data/CMU-1-Small-Region.svs` and return early without it.

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

### 3.4 Async loading, LRU cache, keep-old-level-while-loading

Where it starts feeling like a viewer rather than a demo.

Three pieces, and the third is the one that is usually skipped and shouldn't
be: requests go to the pool with a **generation counter** so a pan cancels what
it invalidated; decoded tiles land in a texture cache with a **hard byte
budget**; and the previous level's tiles stay drawn underneath until the new
level's have arrived, so panning degrades in resolution instead of flashing
white.

Done when a sustained pan at full zoom neither stutters visibly nor grows RSS
without bound.

### 3.5 Overlays

Only now. Detected objects from `qupath-objects` drawn over the slide, in slide
coordinates, culled to the viewport.

This is also the first point where the viewer earns something back for the
port: `DIVERGENCES.md` D-008 is a claim about *geometry* — nucleus masks
against smoothed cell polygons — and it has never been looked at directly.

## 4. The two things that will bite

- **Texture memory.** A naive "upload every tile you see" viewer dies within
  about ten seconds of panning. The LRU budget belongs in step 3.4, not after
  the first time it falls over. This is the same failure mode as `map_init` in
  `ROADMAP.md` §5 — invisible at small scale, unbounded at real scale.
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
  never exceed the budget.
- **What is *not* claimed by a test is appearance.** Colours, orientation and
  smoothness are checked by looking, and that is stated rather than dressed up
  as a test.

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

It prints two numbers per frame that need no eyes: the fraction of the output
that is opaque, and the fraction still holding the clear colour. Those say
whether the frame was drawn at all, which is a weaker claim than "it looks
right" and an honest one.

## 7. Open questions

- **Where the crate lives — settled.** `crates/qupath-viewer`, and the
  dependency worry was answered rather than accepted: winit, wgpu and png are
  **dev-dependencies**, needed only by the examples that open a window. The
  library itself depends on `qupath-slide` alone, so `cargo test -p
  qupath-viewer` builds none of the graphics tree. That falls out of §6 — if
  the tests need no GPU, the library does not need to link one.
- **Whether `qupath-slide` needs a tile cache of its own.** Probably not — the
  viewer's texture cache sits above it and the detection pipeline streams. Do
  not add one speculatively.
- **`missing_docs` and `unsafe_code = "deny"`.** wgpu needs no unsafe from us at
  this level. Keep both lints on and find out early if that is wrong.
