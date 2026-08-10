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
| `qupath-watershed` | Marker-controlled flood, bucket queue, 4/8-connected labelling | recorded |
| `qupath-filters` | MIN/MAX rank filter, ImageJ Gaussian blur, convolution, morphological reconstruction | recorded, 42 + 22 + 22 + 24 cases |
| `qupath-slide` | Aperio SVS / tiled TIFF reader, pure Rust | recorded, 5 regions |
| `qupath-pipeline` | Streams slide tiles through the above | colour deconvolution |

Every image-processing stage `WatershedCellDetection` runs **before** its
watershed is now ported and pinned against output recorded from QuPath itself —
deconvolution, background subtraction by reconstruction, the Laplacian of
Gaussian, the regional maxima, the labelling and the flood. Nothing it runs
**after** the watershed is: ROI tracing, the mean-intensity and area filters,
the merge-and-split, and measurement are all absent. So the output is still
flooded basins rather than cells, and the runtime is still not comparable with
QuPath's.

The basin count is 88,424 against QuPath's 25,757 on the same region, and
**the stage that was supposed to close that gap turned out not to**. The
roadmap had said for two revisions that `findRegionalMaxima`'s prominence test
was what would bring the count down. It is ported now, bit-identically, and it
removes two markers — its threshold gates a maximum's *value*, not its
prominence, and the prominence test itself only ever discards maxima sitting on
an exact plateau, which a Laplacian of Gaussian of real pixels does not have.
The gap is in the object-level filters that run after the watershed; the measured
breakdown is in [docs/ROADMAP.md](docs/ROADMAP.md) §1.5.

Two bugs the fixtures caught this round, both of which had been producing
entirely plausible images:

- The pipeline ran a **morphological opening in place of a top hat**. QuPath
  estimates its background by opening by reconstruction and then *subtracts* it;
  replacing the image with its opening keeps the background and discards the
  nuclei, which is the opposite. Wrong operation, wrong radius (2 px against
  16), wrong direction.
- Earlier, the port had been using the **DAB** stain vector under the name
  `EOSIN` — H-DAB deconvolution presented as H&E, a 52% error on the leading
  coefficient. This is the argument for the whole approach in miniature: no test
  that did not compare against QuPath's own numbers could have found either.

**End-to-end cell detection is unchanged at QuPath's 15.87 s**, and will stay
that way until the stages above exist. Component speedups and what they are
worth are in [docs/benchmarks/README.md](docs/benchmarks/README.md); what comes
next is in [docs/ROADMAP.md](docs/ROADMAP.md).

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
