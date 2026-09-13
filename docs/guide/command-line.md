# Command-line tools

Everything here runs **without a window and without a GPU**, which makes it
what to use for measurement, for scripting, and for working out what a slide
contains before opening it.

They are `cargo` examples rather than installed binaries, so each is
`cargo run --release -p <crate> --example <name>`.

## Reading a slide

### `info` — the pyramid, and a timed read

```bash
cargo run --release -p qupath-slide --example info -- data/CMU-1.svs
```

```text
data/CMU-1.svs
level           size        tiles       tile  downsample
    0    46000x32914      180x129    256x256        1.0x
    1     11500x8228        45x33    256x256        4.0x
    2      2875x2057         12x9    256x256       16.0x

level 0: Aperio Image Library v10.0.51
46920x33014 [0,100 46000x32914] (256x256) JPEG/RGB Q=30

region 27776,5952 4000x4000 at level 0: 0.118 s (405.1 MB/s decoded)
  mean channel value 147.0, 32.8% of samples below 128
```

The first thing to run against any slide. If this prints, the reader can open
it — and the level table tells you which levels detection can honestly use (see
[Detecting cells](detecting.md#which-pyramid-level-a-run-reads)).

### `tissue_scan` — find the tissue

```bash
cargo run --release -p qupath-slide --example tissue_scan
```

```text
# data/CMU-1.svs, 4000x4000 candidates ranked by mean optical density
# density is 255 - mean channel value on level 2
28000,6000,4000,4000  # density 108.6
8000,16000,4000,4000  # density 108.3
36000,4000,4000,4000  # density 103.2
```

Ranks candidate regions by how much tissue is in them. **Use this rather than
picking coordinates by eye** — doing it by eye put four of the first six
regions on glass, where there is nothing to measure. Output is
`x,y,width,height`, ready to paste into the tools below.

## Running detection

### `region` — the pipeline over one region, with timings

```bash
cargo run --release -p qupath-pipeline --example region
```

Prints per-stage timing, the object count after every stage, and peak memory:

```text
object counts, stage by stage:
  intensity filter     80762 ->   51152  (-36.7%)
  merge                51152 ->   26397  (-48.4%)
  split and refine     26397 ->   45990  (+74.2%)
  area filter          45990 ->   28359  (-38.3%)
  tile seams           28359 ->   26647  (-6.0%)
```

This is the tool for "why did my count change" — it shows which stage moved.

## Rendering without a window

### `dump` — write the slide to a PNG

```bash
cargo run --release -p qupath-viewer --example dump -- data/CMU-1.svs
```

Writes the whole slide at the level the viewport picks, plus a 1024×1024 crop
at level 0, then prints the level table across scales.

### `render` — the viewer's geometry, offscreen

```bash
cargo run --release -p qupath-viewer --example render -- data/CMU-1.svs
```

Renders what the window would render, including a zoom sequence crossing every
level boundary and a scripted pan driven through the real loader and cache.
**This is what to run when the window looks wrong and you want numbers instead
of an impression.** Add `--cpu` to force the software path; running it both ways
is the cross-check the no-GPU support rests on.

### `chrome` and `panes` — the interface, offscreen

```bash
cargo run --release -p qupath-app --example chrome
```

Renders the application's own panels to PNGs with no window and no GPU. Without
a slide path it renders the **empty states**, which are what a first-time user
meets.

### `window` — the viewer with instrumentation

```bash
cargo run --release -p qupath-viewer --example window -- data/CMU-1.svs [budget_mb]
```

A window whose title bar carries the backend, level, tile count, resident
texture memory, what the loader is still working on, and the mean cost of the
last thirty frames. The optional second argument is the texture budget in MB.

Two invariants worth watching: zooming out should **drop** the tile count, never
raise it, and a long pan should leave resident megabytes **flat**.

## Parity and measurement

| Command | Prints |
| --- | --- |
| `--example cell_counts -p qupath-pipeline` | The ten-region count comparison. |
| `--example object_masks -p qupath-pipeline` | The Dice figures against QuPath's masks. |
| `--example stage_ladder -p qupath-pipeline` | Both sides' counts at four stage boundaries. |
| `--example tile_seams -p qupath-pipeline` | What seam deduplication removes. |

These compare against committed fixtures. **Recording new fixtures needs a
QuPath build**; running the comparisons does not.

### Benchmarks

```bash
cargo bench
```

`[profile.bench]` sets thin LTO and one codegen unit — benchmarks are
meaningless without optimisation. Committed output, with machine, thread count
and fixture stated, is in [`docs/benchmarks/`](../benchmarks/).

Measure on a **saturated grid**: a 4-tile run uses only performance cores, so it
does not represent a real workload.

## Tests

```bash
cargo test --workspace --no-fail-fast
QUPATH_RUST_REQUIRE_DATA=1 cargo test --workspace --no-fail-fast
```

Tests needing the 177 MB slide skip when it is absent, and a skipped test
reports `ok`. The environment variable turns those skips into failures so they
are visible. `--no-fail-fast` is required — without it the run stops early and
the summary names only a prefix of what was skipped.
