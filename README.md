# QuPath_Rust

An accelerated Rust implementation of parts of [QuPath](https://qupath.github.io/),
reproducing its logic with a data layout suited to bulk analysis.

## Licence

GPLv3, matching QuPath. This code is written by reading QuPath's source in order
to reproduce its behaviour, which makes it a derivative work.

## Design principles

- **Objects are columnar.** Detections and their measurements are a struct of
  arrays — one contiguous column per feature, one row per object — rather than
  QuPath's per-object string-keyed measurement map. This is the main source of
  the speedup, not the choice of language.
- **Parity is the test suite.** Behaviour is checked against output recorded
  from QuPath itself, at documented tolerances. Intentional differences are
  recorded in [DIVERGENCES.md](DIVERGENCES.md).
- **Performance claims carry evidence.** Every claim is backed by committed
  benchmark output stating machine, thread count, and fixture.

## Status

Five crates on the parity path, each checked against fixtures recorded from a
real QuPath build:

| Crate | Does | Parity |
| --- | --- | --- |
| `qupath-objects` | Columnar detection/measurement table | recorded |
| `qupath-watershed` | Marker-controlled flood, bucket queue, 4/8-connected labelling, filled-region tracing | recorded, 3 + 13 + 17 cases |
| `qupath-filters` | MIN/MAX rank filter, ImageJ Gaussian blur, convolution, morphological reconstruction | recorded, 42 + 22 + 22 + 24 cases |
| `qupath-slide` | Aperio SVS / tiled TIFF reader, pure Rust | recorded, 5 regions + the level table and a tissue block against OpenSlide |
| `qupath-pipeline` | Tiles a region QuPath's way and runs every detection stage over it | colour deconvolution; object filters, 17 images; merge and split; boundary refinement |

And one that is not on it: `qupath-viewer`, a slide viewer with pan, zoom and
pyramid-level selection, tracked in [docs/VIEWER.md](docs/VIEWER.md). **It
claims no parity and no speedup** — QuPath's viewer is not what is being
ported, and nothing in it makes the detection pipeline faster. It is built
because the read side had nothing looking at it. See
[Viewing a slide](#viewing-a-slide).

**The detection chain is closed.** Every stage `WatershedCellDetection` runs
from the colour deconvolution to the objects it goes on to measure is ported and
pinned against output recorded from QuPath itself, and running on QuPath's own
tile grid: background subtraction by reconstruction, the Laplacian of Gaussian,
the regional maxima, the labelling, the flood, the region tracing, the
mean-intensity filter, the merge, the shape split, `refineBoundary` and the area
filter. What is missing is the tail — cell expansion and the measurements —
neither of which changes how many objects there are. The ROI **vertices** and
the **boundary smoothing** over them were part of that tail and are now ported:
the contour against ImageJ's own Wand over nine cases, and `smoothBoundaries`'
four stages against QuPath over twelve more, every intermediate pinned.

**Measured over ten regions of CMU-1 the pooled count is 156,979 objects against
QuPath's 147,351 cells — 1.065x, with a range of 1.012–1.129 across regions** and
always in the same direction: this over-counts. On the profiled 4000x4000 region
alone it is 26,647 against 25,757, 1.035x, from 1.86x — read that as one region
and the best of the ten, not as the result. Reproduce with
`./parity/record-cell-counts.sh` then
`cargo run --release --example cell_counts -p qupath-pipeline`.

What the gap tracks is cell **density**, inversely: sparse regions diverge by
6–13% where dense ones agree to within 1–3.5%, and the seam count does not sort
the table. So it is **not** primarily the tile-seam resolution D-008 predicted —
three of the ten regions are a single tile with no seams at all, and they still
disagree. Three stages have since been ruled out against QuPath on real tissue,
pixel for pixel — the merge, the shape split, and the background estimate — which
puts the cause between the deconvolution and the flood. See D-009 in
[DIVERGENCES.md](DIVERGENCES.md) and §2 of [docs/MEMO.md](docs/MEMO.md).

A matching count is also the weakest evidence of a match there is — two runs can
agree on how many objects they found and disagree about where every one of them
is. The strong test, Dice or IoU over the object masks, needs QuPath's
per-object masks recorded, and no fixture holds them yet.

**None of the last four stages was the fix the roadmap expected**, and the
corrections are worth more than the code:

- The roadmap said for two revisions that `findRegionalMaxima`'s prominence test
  was what would bring the count down. It is ported now, bit-identically, and it
  removes two markers — its threshold gates a maximum's *value*, not its
  prominence, and the prominence test itself only ever discards maxima on an
  exact plateau, which a Laplacian of Gaussian of real pixels does not have.
- The roadmap then said the object filters "need only per-label sums and counts
  — no tracing", and that the area filter could be estimated on raw basins. Both
  are wrong at the call site: ImageJ's Wand traces a *threshold range*, so two
  basins meeting with no watershed line between them are one object, and the
  area filter runs after a merge-and-split on objects that do not exist yet.
- Its list of remaining stages then turned out to be missing one. Between the
  shape split and the area filter stands `refineBoundary` — a second Gaussian
  and a second Laplacian at sigma 1, on the raw deconvolved channel, guarded by
  a hard-coded `true`. A list of stages is not a reading of the source.

Three bugs the fixtures caught, all of which had been producing entirely
plausible images:

- The pipeline ran a **morphological opening in place of a top hat**. QuPath
  estimates its background by opening by reconstruction and then *subtracts* it;
  replacing the image with its opening keeps the background and discards the
  nuclei, which is the opposite. Wrong operation, wrong radius (2 px against
  16), wrong direction.
- Earlier, the port had been using the **DAB** stain vector under the name
  `EOSIN` — H-DAB deconvolution presented as H&E, a 52% error on the leading
  coefficient.
- Aperio's JPEG tiles carry no Adobe APP14 marker, so a decoder following the
  JFIF convention converts RGB as though it were YCbCr. The slide still looks
  like plausible H&E.

That is the argument for the whole approach in miniature: no test that did not
compare against QuPath's own numbers could have found any of them.

## Speed

On the region QuPath itself was profiled on — 4000x4000 at (27776, 5952) of
`CMU-1.svs`, same slide, same default parameters:

![End-to-end cell detection against QuPath](docs/benchmarks/compare-endtoend.svg)

**4.7x single-threaded, 15.6x on twenty threads.** Read the single-threaded bar
when the question is whether the port is faster, because QuPath's 15.87 s was
measured single-threaded and QuPath parallelises across tiles in real use. And
read both as an *upper bound*: QuPath's time includes the tail this pipeline has
not written — cell expansion and the measurements — so some of that
gap is work not yet done rather than work done faster. The gap is decisive
either way; its exact size is not yet measurable.

The two profiles do not have the same shape, which is the point — a stage that
dominates QuPath is not the stage that dominates here:

![QuPath's hot frames](docs/benchmarks/compare-hotframes.svg)

![QuPath_Rust's stage breakdown](docs/benchmarks/compare-stages.svg)

QuPath's single largest cost is a `PriorityQueue<PixelWithValue>` of boxed
objects compared through an interface call — 29.4% of self time in one data
structure. Here the flood is 12.1% and the queue itself is a flat bucket array.
The port's own largest stage is the background reconstruction, which barely
registers in QuPath's profile at 5.1%. **Neither profile predicts the other**,
and this is why every optimisation in this repository is chosen from a
measurement of the port rather than from QuPath's flame graph.

Component-level, against ImageJ's own `RankFilters` on the same machine:

![Rank filter against ImageJ](docs/benchmarks/compare-rank.svg)

3.1–9.7x depending on radius, and the *shape* matters more than the peak: the
win is largest at small radii and settles around 3.4x at the radii detection
actually uses. Naively scanning the kernel per pixel was **7.6–35.6x slower**
than ImageJ before the rectangle decomposition replaced it — the algorithm was
the variable, not the language.

Scaling is set by how many tiles a region has, not by how many cores the machine
has:

![Wall clock against thread count](docs/benchmarks/compare-threads.svg)

The 4000x4000 region is four tiles, so it saturates at four threads and
everything past six is idle. The 9000x9000 region is twenty-five tiles and
keeps improving to twenty. A stage share quoted without its tile count and
thread count is not a number anyone can use.

Charts regenerate with `python3 docs/benchmarks/plot.py`; every figure in them
is copied from a recording in [docs/benchmarks/](docs/benchmarks/) or
[docs/profiles/](docs/profiles/). Component speedups and what they are worth in
place are in [docs/benchmarks/README.md](docs/benchmarks/README.md); what comes
next is in [docs/ROADMAP.md](docs/ROADMAP.md). Where the remaining count gap
enters, stage by stage, is `cargo run --release -p qupath-pipeline --example
stage_ladder`. How the fixtures every parity
claim rests on are recorded — and which recorders need a slide, which is not a
detail — is in [parity/README.md](parity/README.md). **New here? Start with
[docs/MEMO.md](docs/MEMO.md)** — what is finished, what is genuinely faster, and
what to distrust, in one page.

## Viewing a slide

`CMU-1.svs` — 46000x32914, four tissue sections — fitted to a 1024x768 view.
The viewer picks **level 2** for this and reads 108 tiles; level 0 would be
23,220 for a worse image.

![The whole slide, fitted](docs/images/viewer.jpg)

Zoomed to the region `docs/ROADMAP.md` profiles, with the detections from a
`d` keypress drawn over it — **1272 objects**, the outline of each traced
nucleus in orange:

![Cell detection over tissue](docs/images/viewer-detail.jpg)

Further in, at level 0 and past 1:1, where the outlines can be checked against
individual nuclei. The boundaries are pixel-stepped because nothing smooths
them yet — QuPath's *Smooth boundaries* is not implemented
([docs/VIEWER.md](docs/VIEWER.md) §8.1), and a control for it is deliberately
absent rather than present and inert:

![Nuclear outlines at level 0](docs/images/viewer-zoom1.jpg)

Every image above is written by `examples/render`, which composes offscreen
through the same shader the window uses — so they are the viewer's own output
rather than screenshots, and they can be regenerated on a machine with no
display. `docs/VIEWER.md` §6 is why that distinction is kept.

```sh
# Drag to pan, scroll to zoom, f to fit, escape to quit. The title bar shows
# the backend, the pyramid level, the tile count and resident texture memory.
# The optional second argument is the texture budget in MB.
cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs 256

# The same window with no GPU involved at all: no adapter, no driver, no
# shader. This is also what runs by itself when there is no adapter to be had.
cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs 256 --cpu

# No window and no GPU: writes the slide as a PNG at the level the viewport
# picks, and prints the level table across scales.
cargo run -p qupath-viewer --example dump --release -- data/CMU-1.svs

# No window: renders the same geometry offscreen, writes it as PNGs, and
# reports a scripted pan through the loader and the cache. Takes `--cpu` too.
cargo run -p qupath-viewer --example render --release -- data/CMU-1.svs
```

### The application

`crates/qupath-app` is the viewer with an interface around it: a toolbar, a file
dialog, a status bar with a scalebar, an overview thumbnail showing where on the
slide the view is, and three panels off the View menu — QuPath's detection
parameters in microns, the slide's metadata and level table, and a list of every
gesture and key.

```sh
# `d` detects cells in what is on screen, `h` hides the outlines. Detection runs
# on another thread, so the view keeps moving while it works.
cargo run -p qupath-app --release -- data/CMU-1.svs

# What your trackpad actually sends, which is platform-specific enough to be
# worth printing rather than guessing at.
cargo run -p qupath-app --example gestures --release
```

Following QuPath's interface is a **port of the design, not of the code**, and
the measurement is why: `qupath-gui-fx` is 101,712 lines against this
application's 1,600, `ProjectBrowser.java` alone is larger than all of it, and
JavaFX is retained-mode where `egui` rebuilds from state every frame. What is
portable is which panes exist and what each one holds — and, where a rule is
genuinely a specification, the rule itself. The scalebar is the clearest case:
`Scalebar.java`'s table of permitted lengths and its snap-to-nearest is
arithmetic with no JavaFX in it, so it is ported exactly, down to the irregular
250/400/800 entries a generated 1-2-5 sequence would get wrong. The pattern that
emerged is that **arithmetic and file formats port cleanly and drawing does
not** — the overview's two mappings, the brightness/contrast transform, GeoJSON
and `.qpproj` all came across the same way.

Since then: the **overview thumbnail** is in, the **brightness/contrast
controls** drive the display transform over the deconvolved stain channels,
**GeoJSON export** writes the dialect QuPath reads, and the **project format** is
written — JSON files, a browser that searches and sorts and edits entry metadata,
and `thumbnail.jpg`. The last two revise [docs/VIEWER.md](docs/VIEWER.md) §0's
stated non-goals, deliberately and on the record rather than by drift.
`data.qpdata` is Java serialisation and is deliberately still not attempted: a
guessed version is how a project becomes corrupt in a way someone finds out much
later.

**Both formats have since been opened by QuPath itself**, which is a different
claim from the one this paragraph used to make. A project written here is loaded
by `ProjectIO.loadProject`, its entries and their edited metadata read back, its
images opened through OpenSlide, and its thumbnails read by `ImageIO`; the
GeoJSON goes through `PathIO.readObjects`. A format checked only against the code
that writes it is untested, and that gap is now closed in both directions —
QuPath also writes a project here for this reader to be checked against.

Level selection is the whole point, and it is worth one number: a 1024x768 view
of the whole slide picks level 2 and reads **108 tiles against level 0's
23,220**. That is doing less work rather than doing the same work faster, which
is the first item in the guidelines' own ordering — though it is a property of
the viewer, not a speedup of the detection pipeline.

Tiles decode on a thread pool whose queue a pan *cancels* rather than appends
to, into a cache with a hard byte budget, with the coarsest pyramid level drawn
underneath while the finer tiles arrive. That last part is worth its own
number: over a scripted pan at 700 px per frame, **7.3% of the average frame is
never drawn with the underlay against 57.6% without**, for the same decode work.

**No GPU is required for any of it.** The viewer's whole draw call is
axis-aligned textured quads — no rotation, no blending, no depth — which is a
scaled blit, so it is written out on the CPU in the library and used whenever
there is no adapter. That costs **1.27 ms** to compose a 1024x768 frame, against
16.7 ms for a 60 Hz one. The window needs a display; nothing else needs
anything.

Having both paths is also a check rather than a courtesy. Run the same scripted
pan through each and two rasterisers that share no code agree on every number in
the report, and on the detail view — level 0, 1:1, integer offsets — the two
frames are **byte-identical across all 786,432 pixels**.

[docs/VIEWER.md](docs/VIEWER.md) is the plan and the running account of what has
gone wrong, including a level-selection bug that cost an 11x over-decode while
every frame still looked correct.

## Building

Rust: `cargo test` — no system dependencies. Reading JPEG 2000 slides would
need OpenSlide through FFI and is not implemented; those files are refused by
name rather than mis-decoded.

The parity recorders that open a slide *through QuPath* do use OpenSlide, and it
needs no system package either: `qupath-extension-openslide` depends on a jar
carrying its own `libopenslide.so`, which JNA extracts at load time. `ldconfig`
having no `libopenslide` says nothing about whether those recorders run — an
error this repository made, and kept two items marked blocked on for several
sessions.

Tests that need a slide skip themselves when `data/` is empty — see
[docs/test-data.md](docs/test-data.md).

QuPath (for parity fixtures and benchmarks) needs **JDK 25**. The Gradle wrapper
cannot auto-provision it from a JDK below 17, so point it at a JDK 25 explicitly:

```bash
JAVA_HOME=/path/to/jdk-25 ./qupath/gradlew -p qupath :qupath-core:jar
```

The `qupath/` directory is an upstream QuPath checkout and is not tracked here.
