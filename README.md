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
| `qupath-watershed` | Marker-controlled flood, bucket queue | recorded |
| `qupath-filters` | MIN/MAX rank filter, ImageJ Gaussian blur and convolution | recorded, 42 + 22 + 22 cases |
| `qupath-slide` | Aperio SVS / tiled TIFF reader, pure Rust | recorded, 5 regions |
| `qupath-pipeline` | Streams slide tiles through the above | colour deconvolution |

`qupath-pipeline` reads real slide pixels and runs the ported stages over them
with bounded memory, but it is **not** `WatershedCellDetection`: the maximum
finding, ROI tracing and measurement are not ported. Its output is flooded
basins, not cells, and its runtime is therefore not comparable with QuPath's.

The Laplacian of Gaussian is now complete — blur and 3x3 convolution, both
bit-identical to ImageJ — and **the basin count went up when it landed**, from
42,966 to 91,957 against QuPath's 25,757 cells. That is the honest direction: a
Laplacian is a second derivative, so it creates local maxima wherever the
blurred image has structure, and the stage that discards them is the one still
missing. QuPath uses `MorphologicalReconstruction.findRegionalMaxima`, an
h-maxima transform whose whole job is to merge maxima not separated by a real
descent. The count is not a score until that exists.

The deconvolution fixture is worth singling out, because it is the argument for
this whole approach in miniature. It found that the port had been using the
**DAB** stain vector under the name `EOSIN` — H-DAB deconvolution presented as
H&E, a 52% error on the leading coefficient — and it had been producing a
smooth, entirely plausible image the whole time. No test that did not compare
against QuPath's own numbers could have found it.

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
