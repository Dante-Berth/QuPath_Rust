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

Five crates, each checked against fixtures recorded from a real QuPath build:

| Crate | Does | Parity |
| --- | --- | --- |
| `qupath-objects` | Columnar detection/measurement table | recorded |
| `qupath-watershed` | Marker-controlled flood, bucket queue, 4/8-connected labelling, filled-region tracing | recorded, 3 + 13 + 17 cases |
| `qupath-filters` | MIN/MAX rank filter, ImageJ Gaussian blur, convolution, morphological reconstruction | recorded, 42 + 22 + 22 + 24 cases |
| `qupath-slide` | Aperio SVS / tiled TIFF reader, pure Rust | recorded, 5 regions |
| `qupath-pipeline` | Tiles a region QuPath's way and runs every detection stage over it | colour deconvolution; object filters, 17 images; merge and split; boundary refinement |

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

**End-to-end cell detection is still not comparable with QuPath's 15.87 s**,
because QuPath spends part of it on the tail this pipeline does not have.
Component speedups and what they are worth are in [docs/benchmarks/README.md](docs/benchmarks/README.md); what comes
next is in [docs/ROADMAP.md](docs/ROADMAP.md). **New here? Start with
[docs/MEMO.md](docs/MEMO.md)** — what is finished, what is genuinely faster, and
what to distrust, in one page.

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
