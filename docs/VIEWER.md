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

## 3. Milestones

Each one is something that can be looked at. No step is finished until the
thing on screen is correct, not merely present.

### 3.1 Window, wgpu, clear to a colour

Boring, and it is where all the setup pain lives. Done when the window opens,
resizes without panicking, and closes cleanly.

### 3.2 One hardcoded tile on screen

**The real first milestone.** One `read_tile_into` at a fixed level, column and
row, uploaded as a texture, drawn as a quad.

Done when the tile is recognisably the right part of the slide, the right way
up, and the right colours — RGB8 from the decoder against whatever the surface
format wants is the first place a bug hides here. Compare against the same
region written to a PNG from `read_region_rgb`, which is already tested.

### 3.3 Pan, zoom, and level selection

Viewport math. Mouse drag pans, scroll zooms, and the zoom picks a level
through `Slide::level_for_width`. Still synchronous, still stuttery — that is
expected and is not the milestone.

Done when zooming out crosses a level boundary without the image jumping or
changing scale. A jump here means the level's `downsample` is not being applied
where the viewport thinks it is.

This is the step that produces the coordinate-space types in §5. Do not defer
them past it.

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

## 7. Open questions

- **Where the crate lives.** `crates/qupath-viewer` fits the workspace, but it
  pulls winit/wgpu/egui into a workspace whose other five crates have four
  dependencies between them. A separate workspace member is still the simplest
  answer; revisit if build times get annoying.
- **Whether `qupath-slide` needs a tile cache of its own.** Probably not — the
  viewer's texture cache sits above it and the detection pipeline streams. Do
  not add one speculatively.
- **`missing_docs` and `unsafe_code = "deny"`.** wgpu needs no unsafe from us at
  this level. Keep both lints on and find out early if that is wrong.
