# Roadmap

Where the port stands, what comes next, and what each next thing needs before
it can claim anything. Written to be picked up cold.

## State of play

Five crates, all five now with parity fixtures recorded from a real QuPath
build.

| Crate | Does | Parity | Measured |
| --- | --- | --- | --- |
| `qupath-objects` | Columnar detection/measurement table | `MeasurementPrecision.json` | ~100x on scan/filter |
| `qupath-watershed` | Marker-controlled flood, bucket queue | `WatershedFlood.json`, 3 cases | queue 20-32x vs heap; flood 1.40x vs previous |
| `qupath-filters` | MIN/MAX rank filter, Gaussian blur | `RankFilterMinMax.json` 42 cases; `GaussianBlur.json` 10 kernels + 12 images | rank 3.1-9.7x vs ImageJ; blur bit-identical |
| `qupath-slide` | Aperio SVS / tiled TIFF reader | `SlideRegion.json`, 5 regions | 4000x4000 region in 0.09 s |
| `qupath-pipeline` | Streams tiles through the above | `ColourDeconvolution*.json` | 17.8 Mpx in 0.091 s, 53 MB peak |

**The end-to-end number is still QuPath's 15.87 s**, because the pipeline is not
`WatershedCellDetection`. It runs real slide pixels through the ported stages
and nothing else. Do not compare the two figures until §1 below is done; the
pipeline currently produces 42,966 basins where QuPath detects 25,757 cells.

**The pipeline's own figure is not a speedup series and must not be read as
one.** It has moved 0.114 s -> 0.062 s -> 0.091 s, and no step was the same
work as the one before it: §1.1 corrected a wrong stain vector, and §1.2 added
a stage that had been missing. A figure that goes up because a stage was added
is the honest outcome; the counts moved with it, from 397,822 to 211,728 to
42,966 basins against QuPath's 25,757 cells. The parts that *are* comparable —
component A/B runs, interleaved, asserting identical output — are in
`docs/benchmarks/README.md`.

Reproduce everything:

```bash
cargo test --workspace
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo bench --workspace
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo run --release -p qupath-pipeline --example region
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example previous_vs_current
./parity/record.sh && ./parity/record-slide.sh && ./parity/bench.sh   # needs JDK 25
```

---

## 1. Close the detection pipeline — the only item that moves 15.87 s

Everything else on this list is optimisation of things that already work. This
is the item that makes an end-to-end claim possible at all. In dependency
order:

### 1.1 Colour deconvolution parity — **done**

Pinned by `ColourDeconvolution.json` (1000 RGB values x 2 stain sets, no slide
needed) and `ColourDeconvolutionTile.json` (a real 64x64 tile), recorded from
`ColorTransformer.getTransformedPixels` — the call `IJTools.colorDeconvolve`
makes for `WatershedCellDetection`. Worst relative difference **5.9e-8**,
against a `1e-5` tolerance.

**The fixture found a real bug on its first run.** The `EOSIN` constant was the
**DAB** vector, so the pipeline had been running H-DAB deconvolution and calling
it H&E. QuPath's eosin is `[0.2159, 0.8012, 0.5581]` from AMIDA13, not Ruifrok
and Johnston's `[0.07, 0.99, 0.11]`, and not the DAB vector that sits three
lines away from it in `StainVector.java`. The error was 52% on the leading
haematoxylin coefficient — 1.2078 against QuPath's 1.8383 — and produced a
smooth, plausible single-channel image the entire time. Nothing downstream could
have caught it. The marker count fell from 397,822 to 211,728 when it was fixed.

Three smaller corrections came with it, all confirmed against the fixture:

- Constants are QuPath's **raw** vectors, normalised at run time. They had been
  stored pre-normalised and rounded to three decimals, which is a ~1e-4 error
  and would not have met the tolerance on its own.
- The result is **no longer clamped** at zero. QuPath's `deconvolve` does not
  clamp, and a stain amount is legitimately negative over most of a slide's
  background; clamping flattened that to a plateau of zeros, which is exactly
  the sort of thing the maximum finder in §1.3 will be sensitive to.
- Arithmetic is `f64` accumulated and narrowed to `f32` once, which is what
  QuPath does. It had been `f32` throughout.

`Stains::from_two` now takes stain vectors and a per-channel white point, so a
slide carrying its own estimate can be deconvolved the way QuPath would.
`CMU-1.svs` carries none and falls back to the defaults, as QuPath does.

### 1.2 Gaussian blur — **done**

Pinned by `GaussianBlur.json`: 10 kernels straight from `makeGaussianKernel`,
and 12 whole images through `FloatProcessor.blurGaussian` — the call detection
makes. **Bit-identical** to ImageJ on every value of all twelve images and all
ten kernels, which is stronger than the `1e-5` the tolerance allows; the test
gates on the tolerance and reports any value that stops matching exactly,
because the kernel goes through `exp`, `ln` and `sqrt` where a different libm
is entitled to an ulp.

The warning above was the right one — it is not a textbook kernel, in four ways
that the source shows and a derivation does not:

- **The accuracy is 0.0002.** `blurGaussian` picks between `0.002` and
  `0.0002` by processor type, and detection blurs a `FloatProcessor`, which
  takes the tighter one. The other value sits in the same expression and
  shortens every kernel by about a fifth.
- **The tail is a parabola.** Past a break point the Gaussian values are
  replaced by `(kRadius - r)²·slope²`, which reaches zero exactly at the first
  sample outside the kernel, so truncation leaves no step.
- **The break point is searched for**, by scanning inwards for the smallest
  `sqrt(kernel[r])/(kRadius-r)` and stopping at the first value that fails to
  improve. For sigma 3 that lands on a single tap: the other thirteen are the
  plain Gaussian.
- **Edges extend by replication through the tail sum**, not by clamping each
  tap. `kernSum[i]` carries the mass beyond sample `i` *including what the
  truncation dropped*, so the kernel behaves as though it were infinite.

The large-sigma path is ported too. Above sigma 8.5 ImageJ downscales the line,
convolves, and upscales, correcting sigma for the variance the two resamplings
add. Detection does not reach it on defaults — 1.5 µm needs a pixel size under
0.176 µm to cross 8.5, finer than a 40x scan — but sigma is a user-facing
parameter and raising it is the documented remedy for fragmented nuclei, so the
threshold is one dialog box away and skipping it would have been a silent
divergence at a value nothing marks as special.

§5's determinism requirement is met structurally: lines are independent within
a pass and each output pixel is accumulated by one line in a fixed order.
`blurring_is_independent_of_how_lines_are_split` asserts that against 1, 2, 3,
5 and 8 bands rather than trusting it.

**The marker count fell from 211,728 to 42,966**, against QuPath's 25,757 cells
for this region — from 8.2x too many to 1.7x. That is the noise-driven maxima
this item predicted, going away. It is not parity: §1.3's prominence test and
the 3x3 Laplacian on the line after the blur are both still missing, and the
remaining factor is where they live.

The blur was 31% of pipeline stage time when it landed and is now 17%, after
the change in §4.3. See `docs/benchmarks/README.md` for both, including the
transpose that was tried first and was slower.

### 1.3 The Laplacian, then `MaximumFinder`

Two things, in the order detection does them.

The line after the blur is `fpLoG.convolve(new float[]{0,-1,0,-1,4,-1,0,-1,0},
3, 3)`, which completes the Laplacian of Gaussian. It looks trivial and is not:
ImageJ's `Convolver` normalises the kernel by its sum unless told otherwise —
and this kernel sums to zero, which is its own branch — and has its own edge
convention. Record `Convolver.convolveFloat` before porting it; the blur above
is the second time in this repository that reading the source gave the wrong
constant.

Then `MaximumFinder`, the piece that makes the marker count wrong. ImageJ's
`MaximumFinder.getMaxima` sorts candidate maxima by value, floods each with a
tolerance, and discards maxima reachable from a higher one without descending
below `value - tolerance`.

- The prominence test is the whole algorithm; the plateau merging that
  `qupath-pipeline::seed_maxima` already does is the easy half.
- Fixture: record `getMaxima` output on small synthetic surfaces with plateaus
  and near-equal peaks, plus one real tile.
- The bucket queue in `qupath-watershed` is directly reusable here.

### 1.4 Morphological reconstruction

5.1% of self time (`dilateAndCompare`). QuPath's
`MorphologicalReconstruction` iterates dilate-and-compare to convergence —
the naive form is what the profile caught, and a queue-based reconstruction is
the standard fix, again reusing the bucket queue.

### 1.5 Thresholding, ROI tracing, measurements

The tail of detection: threshold the reconstructed image, trace object
boundaries to ROIs, compute per-object measurements into the columnar table
that `qupath-objects` already provides. ROI tracing is the fiddly one — QuPath
uses a specific contour-following convention and the vertices must match.

### 1.6 The claim

Only once 1.1–1.5 are done: run both implementations on the same region and
compare **cell counts first, then time**. A run that is fast and finds a
different number of cells has not been ported. Success is Dice or IoU >= 0.99
on the object masks per §5, and a cell count within a documented margin.

---

## 2. Tile seams — a correctness bug the current pipeline has

Tiles are processed independently, so an object straddling a tile boundary is
detected twice, once truncated on each side. QuPath handles this with overlap
and object merging.

This is not visible in the timings and does not affect them, but it makes the
object counts wrong in a way that will not show up until 1.6 compares them.

- Read tiles with a halo of at least the largest kernel radius plus the maximum
  expected object radius, process the padded tile, and keep only objects whose
  centroid falls in the unpadded core.
- Add a test that an object placed exactly on a boundary is detected once.

---

## 3. JPEG 2000, and the FFI §9 asks for

`qupath-slide` reads baseline-JPEG SVS in pure Rust and **refuses** the JPEG
2000 variants by name (`SlideError::UndecodableCodec`). `data/CMU-1-JP2K-33005.svs`
is exactly that case and cannot be opened today.

§9 is unambiguous about the answer: FFI to OpenSlide and/or OpenJPEG, in a
`*-sys` crate wrapped by a safe crate, sources vendored with a pinned hash,
built with `cc`, never shelling out to a Makefile. `M-FFI-NAMING`,
`M-FFI-TRANSLATES`, `M-OOBE` and `M-SYS-CRATES` all apply.

Note OpenSlide is not installed on the current machine (no headers, no
pkg-config), so this starts with vendoring rather than linking.

**Licensing, from §9, decide before the first commit:** OpenSlide is LGPL and
QuPath is GPLv3. This code is derived from reading QuPath source and is already
GPL-3.0-or-later in `Cargo.toml`. Confirm that stays correct once native
dependencies are linked.

---

## 4. Optimisation, in §0's order

Only after 1 and 2. Each of these is worth less than it looks — the Amdahl
ceilings in `docs/benchmarks/README.md` multiply to about 1.7x for what is
already ported.

1. **Doing less work.** Untouched so far and the largest lever by far.
   `Slide::level_for_width` exists but nothing uses it: detection at a
   coarser pyramid level where the analysis permits, tissue detection to skip
   background tiles entirely (the profiled region is 99% tissue; a whole slide
   is mostly glass), and early rejection before the expensive stages.
2. ~~Deconvolution~~ — **done**, alongside §1.1, because matching QuPath's
   `f64` accumulation had pushed the stage to 36.5% of pipeline time. The
   lookup table this list predicted is what QuPath itself does
   (`makeODLUT`), and folding each row's coefficient into the table as it is
   built removes the three multiplies as well as the three logarithms. The
   stage went from 0.768 s to 0.031 s summed across threads, 36.5% to under 4%.
   It is now the smallest stage in the pipeline and not worth revisiting.
3. ~~Gaussian blur~~ — **done**, 2.31x in isolation and 2.30x in place, from
   31.2% of pipeline stage time to 16.6%, with the marker count unchanged and
   the output bit-identical. ImageJ's inner loop carries one output pixel, and
   its accumulator is a loop-carried dependency, so the interior ran at the
   latency of a multiply-add rather than its throughput; eight lanes at once
   makes them independent and costs no extra loads, because they read almost
   the same taps.

   **Transposing the tile so the column pass ran contiguously was tried first
   and was slower at every size** — 0.82x to 0.98x. The column pass already
   copies each column into a contiguous buffer, so the taps were never strided;
   only the copy-in and the store were, and two transposes cost more. The
   profile share said "the y pass is expensive" and the reason was wrong. Worth
   remembering before the next one.
4. **The watershed is now 53% of pipeline stage time**, far above the 29.4% the
   JFR profile attributed to it in QuPath, and it grew as a share every time
   something else shrank. This is the largest lever after §0.1.
5. **SIMD**, last, per §0.5 — and only behind a scalar fallback with a
   differential test. Note §4.3 got 2.3x without any explicit SIMD, by removing
   a dependency rather than by widening an instruction.

---

## 5. Standing constraints

Carried forward because they are the observed failure modes in this repository,
not hypothetical ones. Two of them were hit again this session.

- **Never weaken a test to make it pass.** No widened tolerances, no `#[ignore]`,
  no edited fixtures.
- **A component measurement overstates what it is worth in place.** Buffer reuse
  looked like a third of the queue cost and was 3% of the flood. The
  incremental rank filter looked 1.9x ahead of ImageJ and was behind it from
  radius 3. Measure in place before believing it.
- **Benchmarks must own their memory.** `iter_batched` with a megabyte input
  made the allocator a first-order term and reported radius 10 as faster than
  radius 5.
- **This machine is not quiet** (load average ~3). Compare implementations
  interleaved in one process, take the minimum, and assert both produce
  identical output on every timed iteration. See the `previous_vs_current`
  examples.
- **Plausible wrong output is the real risk.** Three bugs now — the transposed
  stain matrix, the strict maximum test, and the DAB vector stored under the
  name `EOSIN` — produced output that looked entirely reasonable. Every one was
  caught by comparing against something QuPath recorded, or by counting what
  was found; none was visible in the image. Assert on counts and on recorded
  output, never on appearance.
- **Reading the source is not recording a run.** §1.1's constants came from
  QuPath's source and were still wrong, because the file holds three similar
  triples and the port took the wrong one. Every remaining item in §1 is
  another set of constants and conventions read the same way. Record the
  fixture before believing the port, not after it looks right.
- **A recorder that needs a slide must not run without one.** `record.sh`
  truncated its target fixture before invoking each recorder, then ran
  `SlideRegion` — which needs Bio-Formats and a slide — with neither, so under
  `set -e` it emptied a committed fixture and aborted before the recorders
  after it. The documented `record.sh && record-slide.sh` never reached the
  second command. `SlideRegion` is now skipped there, where it was always
  meant to be.
