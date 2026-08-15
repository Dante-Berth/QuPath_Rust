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
| `qupath-slide` | Aperio SVS / tiled TIFF reader, pure Rust | recorded, 5 regions |
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
filter. What is missing is the tail — ROI vertices, cell expansion, the
measurements — none of which changes how many objects there are.

**The count is 26,647 objects against QuPath's 25,757 cells on the same region**,
1.035x, from 1.86x. It is not clean, and where it is not is documented: QuPath
resolves its tile-seam overlaps on *cells* with smoothed polygon geometry, where
this compares nucleus pixel masks, so it removes at least as many duplicates as
this does. See D-008 in [DIVERGENCES.md](DIVERGENCES.md). A matching count is
also the weakest evidence of a match there is — two runs can agree on how many
objects they found and disagree about where every one of them is.

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
not written — ROI vertices, cell expansion, the measurements — so some of that
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
next is in [docs/ROADMAP.md](docs/ROADMAP.md). **New here? Start with
[docs/MEMO.md](docs/MEMO.md)** — what is finished, what is genuinely faster, and
what to distrust, in one page.

## Viewing a slide

```sh
# Drag to pan, scroll to zoom, f to fit, escape to quit. The title bar shows
# the pyramid level, the tile count and resident texture memory. The optional
# second argument is the texture budget in MB.
cargo run -p qupath-viewer --example window --release -- data/CMU-1.svs 256

# No window and no GPU: writes the slide as a PNG at the level the viewport
# picks, and prints the level table across scales.
cargo run -p qupath-viewer --example dump --release -- data/CMU-1.svs

# No window, but through the GPU: renders the same geometry offscreen, writes
# it as PNGs, and reports a scripted pan through the loader and the cache.
cargo run -p qupath-viewer --example render --release -- data/CMU-1.svs
```

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

The window needs a display and a GPU; `dump` needs neither, `render` needs a GPU
but no display, and the tests need neither.
[docs/VIEWER.md](docs/VIEWER.md) is the plan and the running account of what has
gone wrong, including a level-selection bug that cost an 11x over-decode while
every frame still looked correct.

## Building

Rust: `cargo test` — no system dependencies. Reading JPEG 2000 slides would
need OpenSlide through FFI and is not implemented; those files are refused by
name rather than mis-decoded.

Tests that need a slide skip themselves when `data/` is empty — see
[docs/test-data.md](docs/test-data.md).

QuPath (for parity fixtures and benchmarks) needs **JDK 25**. The Gradle wrapper
cannot auto-provision it from a JDK below 17, so point it at a JDK 25 explicitly:

```bash
JAVA_HOME=/path/to/jdk-25 ./qupath/gradlew -p qupath :qupath-core:jar
```

The `qupath/` directory is an upstream QuPath checkout and is not tracked here.
