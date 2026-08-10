# Roadmap

Where the port stands, what comes next, and what each next thing needs before
it can claim anything. Written to be picked up cold.

## State of play

Five crates, all five now with parity fixtures recorded from a real QuPath
build.

| Crate | Does | Parity | Measured |
| --- | --- | --- | --- |
| `qupath-objects` | Columnar detection/measurement table | `MeasurementPrecision.json` | ~100x on scan/filter |
| `qupath-watershed` | Marker-controlled flood, bucket queue, 4/8-connected labelling | `WatershedFlood.json`, 3 cases; `RegionalMaxima.json` labels, 13 surfaces | queue 20-32x vs heap; flood 1.40x vs previous; labelling identical |
| `qupath-filters` | MIN/MAX rank filter, Gaussian blur, convolution, morphological reconstruction | `RankFilterMinMax.json` 42 cases; `GaussianBlur.json` 10 kernels + 12 images; `LaplacianConvolution.json` 5 scales + 17 images; `RegionalMaxima.json` 11 reconstructions + 13 surfaces | rank 3.1-9.7x vs ImageJ; blur, convolution and reconstruction bit-identical |
| `qupath-slide` | Aperio SVS / tiled TIFF reader | `SlideRegion.json`, 5 regions | 4000x4000 region in 0.09 s |
| `qupath-pipeline` | Streams tiles through the above | `ColourDeconvolution*.json` | 17.8 Mpx in 0.21 s, 60 MB peak |

**The end-to-end number is still QuPath's 15.87 s**, because the pipeline is not
`WatershedCellDetection`. Every image-processing stage QuPath runs *before* its
watershed is now ported and pinned; nothing it runs *after* is. Do not compare
the two figures until §1.5 below is done; the pipeline currently produces 88,424
basins where QuPath detects 25,757 cells.

**The pipeline's own figure is not a speedup series and must not be read as
one.** It has moved 0.114 s -> 0.062 s -> 0.091 s -> 0.114 s -> 0.21 s, and no
step was the same work as the one before it: §1.1 corrected a wrong stain
vector, and §1.2, §1.3 and §1.4 each added a stage that had been missing. A
figure that goes up because a stage was added is the honest outcome.

**The counts are not a quality series either**: 397,822 -> 211,728 -> 42,966 ->
91,957 -> 88,424 basins, against QuPath's 25,757 cells.

### The correction this file owes: §1.4 was not the stage that fixes the count

Two revisions of this file said the prominence test in `findRegionalMaxima` was
what would bring the count down — "the stage that removes them", "exactly what
is still missing". **It is ported now, bit-identically, and it removes
essentially nothing.** Measured on the profiled region: 88,422 candidate maxima
before the prominence test, 88,424 markers after it. The +2 is the 4-connected
labelling splitting two diagonal pairs; the prominence test itself discarded
none of them.

The reasoning was wrong in a way worth keeping, because it is the same shape as
the `MaximumFinder` and `EOSIN` errors — a plausible reading of the source that
nobody checked against a run:

- `findRegionalMaxima(fpLoG, 0.001f, false)` looks like an h-maxima transform
  with `h = 0.001`, and the threshold looks like a prominence floor. It is
  neither. `0.001` gates the maximum's **value**, and what survives is
  prominence **strictly greater than zero**.
- A prominence of exactly zero happens only on an exact plateau — a terrace
  whose pixels are bit-identical to a neighbour that is not itself a maximum.
  On synthetic surfaces that is common, which is why the fixture's
  `shoulder-plateau` case is emphatic. On a real Laplacian of Gaussian, values
  are continuous floats and exact ties essentially never occur.
- So on real data the stage is an identity on the marker count. It is still
  required — it is what QuPath computes, and the port is now bit-identical to it
  — but it is a correctness fix, not a count fix.

**Where the count gap actually is**, measured on the same region (see §1.5):
QuPath's object-level filters, all of which run *after* the watershed and all of
which only ever remove objects. Applying its area window (40.2–1606 px²) to the
basins here would drop 27%; its mean-intensity test (`> 0.1` on the
background-subtracted optical density) would drop 25%; together they leave
50,927. The remaining 2x is the merge-and-split, the ROI tracing's hole filling,
and the tile seams of §2 — none of which is ported and each of which is a
plausible several thousand.

The lesson for the next stage on this list: **measure what a stage removes
before claiming it is the one that will.** The count was used as a prediction
here and it was a bad one.

The parts that *are* comparable — component A/B runs, interleaved, asserting
identical output — are in `docs/benchmarks/README.md`.

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

**The marker count fell from 211,728 to 42,966** when this landed, against
QuPath's 25,757 cells for this region — from 8.2x too many to 1.7x. That was the
noise-driven maxima this item predicted, going away. It was never parity, and
§1.3 landing afterwards took the count back up to 91,957: the blur alone
suppresses maxima, and the Laplacian that follows it creates them again. The
sentence that used to end this paragraph — "only §1.4 removes the ones that
should not be there" — was wrong, and §1.4 records why.

The blur was 31% of pipeline stage time when it landed and is now 17%, after
the change in §4.3. See `docs/benchmarks/README.md` for both, including the
transpose that was tried first and was slower.

### 1.3 The Laplacian — **done**

Pinned by `LaplacianConvolution.json`: 17 images and 5 kernel scales through
`FloatProcessor.convolve`, the call on the line after the blur. **Bit-identical**
to ImageJ on every value, and the test asserts that rather than reporting it —
unlike the blur there is nothing transcendental here, so there is no legitimate
source of drift and any difference is a defect.

Four things the call site does not show, each pinned by a case that fails
without it:

- **The Laplacian is the kernel normalisation skips.** `Convolver` divides by
  the kernel sum but guards it with `if (sum != 0.0)`, and this kernel sums to
  exactly zero, so the scale stays 1.0. Always normalising divides by zero;
  never normalising passes this kernel and fails every other.
- **The product rounds to `f32` before it widens.** Java evaluates
  `pixels2[...] * kernel[i]` as `float*float` and only then adds it to the
  `double`. Accumulating in full `f64` is a different answer — not for the
  Laplacian, whose taps are `0`, `±1` and `4`, but for every other kernel.
- **It correlates; the kernel is never flipped.**
- **Edge taps clamp per coordinate**, which is *not* the blur's convention two
  files away.

`FloatProcessor.convolve` snapshots before handing the pixels to a fresh
`Convolver`, so `normalize` is at its field default and the source is a clean
copy. Worth checking rather than assuming: `Convolver` reads
`getSnapshotPixels()` when one exists, and a stale snapshot would silently have
convolved the pre-blur image.

Optimised in the same pass, 3.69x in place — see §4.4.

### 1.4 The maxima — **done**

Pinned by `RegionalMaxima.json`: 11 marker/mask pairs through
`morphologicalReconstruction`, and 13 whole surfaces through
`findRegionalMaxima` and the `labelImage` after it. **Bit-identical** to
QuPath on every value of every case, asserted rather than reported — this is
comparisons and assignments with one absolute difference at the end, so there is
no rounding freedom and any difference is a defect.

What detection calls:

```java
ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false);
ImageProcessor ipNucleusLabels = RoiLabeling.labelImage(ipTemp, 0, false);
Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false);
```

Two levels are recorded so a failure says which half is wrong. `getMaximaLabels`
sits between them and is package-private, so it is pinned only through what the
pair produce.

Five things the call site does not show, each mutation-checked against a case
that fails without it:

- **The maximum test is not strict**, and it skips the one-pixel frame:
  `getMaximaLabels` marks every pixel `>= 0.001` that is `>=` all eight
  neighbours, scanning `y1+1 .. y2-1`. Making it strict, or scanning the border,
  both fail.
- **Reconstruction only ever raises the marker.** It assumes `marker <= mask`
  and never checks: `validateMarkerMask` exists for exactly that and
  `morphologicalReconstruction` does not call it. A marker above its mask is
  left where it is, and the fixture's `marker-above-mask` comes back untouched.
  Both callers here satisfy the precondition structurally.
- **Reconstruction is 8-connected in the queue too.** The raster passes read
  both diagonals of the previous row for free, so a queue that propagates only
  orthogonally passes every ordinary case; `diagonal-serpentine` is a corridor
  whose legs join only at corners, and it is what caught that. A checkerboard
  was tried first and did *not* catch it — the raster passes settle it — which
  is worth remembering before trusting a case that looks discriminating.
- **The output is `|reconstructed - original|`**, an absolute difference.
  Dropping the `abs` fails.
- **The labelling is 4-connected and its threshold is strictly `> 0`**, where
  the maxima that produced the pixels were found 8-connected and gated at
  `0.001`. Three thresholds in two lines and none of them is the same number.

Two properties confirmed rather than assumed, by mutation:

- **The fixed point does not depend on the traversal.** Turning the queue from
  FIFO to LIFO, and deleting the `>10%` repeat loop entirely, both leave every
  case bit-identical. So a future faster traversal is free to differ — which is
  not true of anything else ported in this crate.
- **The queue phase is still load-bearing.** Deleting it fails, on the
  serpentine cases: the raster passes alone do not converge on a path that
  doubles back.

`fpLoG.setRoi(roi)` is called *before* `findRegionalMaxima` and bounds every
loop inside it, where the Laplacian on the line above ran over the whole image.
The tile pipeline uses whole tiles, so this is still moot, and stops being moot
as soon as a real ROI is passed.

**On the count, see the correction at the top of this file.** This stage removed
2 markers net on the profiled region, not the thousands this list predicted, and
the prediction was the error rather than the port.

Cost in place: 12.0% of pipeline stage time, which is the reconstruction — the
labelling that follows it is 0.9%.

### 1.4b Background subtraction — **done, and it was a real divergence**

Not previously on this list, which is why it is numbered here rather than
renumbering everything below it. Found while reading the call site for §1.4.

`WatershedCellDetection` does not run its rank filter the way this pipeline was
running one. Its median filter defaults to radius **0** and is skipped entirely;
what actually sits between the deconvolution and the blur is:

```java
ipBackground = fpLoG.duplicate();
bpBackgroundMask = estimateBackground(fpLoG, ipBackground, backgroundRadius, maxBackground, true);
fpLoG.copyBits(ipBackground, 0, 0, Blitter.SUBTRACT);
```

`estimateBackground` is an **opening by reconstruction** — erode by 8 µm, then
let the result grow back under the original image — and the line after it
**subtracts** that estimate. Together they are a top hat.

The pipeline had a morphological opening at radius 2 px *replacing* the image,
described in its own doc comment as standing in for the median filter. Three
things wrong at once: the wrong operation (opening, not opening by
reconstruction), the wrong radius (2 px against 16.03), and — the one that
matters — **replacing rather than subtracting**, which keeps the background and
discards the nuclei instead of the other way round. It produced a perfectly
plausible image, like the other three bugs this repository has caught.

Both halves reuse pinned code: the MIN rank filter from §1.2's crate and the
reconstruction from §1.4. `max_background` is ported too — the scan for regions
too dark to be background, and the dilated mask that drops them to negative
infinity — because it changes the estimate. QuPath's *other* use of that mask,
discarding nuclei that sit on it, is an object-level test and belongs in §1.5
with the rest.

The flood connectivity was wrong in the same place and is fixed with it:
`doWatershed(fpLoG, ipNucleusLabels, 0, false)` — the `false` is `conn8`, so the
flood is **4**-connected, not 8. It is also not the connectivity of the maxima
test one line earlier, which is 8.

This stage now costs 52% of pipeline stage time, the largest by a wide margin —
59% of it the radius-16 erosion, 40% the reconstruction. Both numbers are new
and neither has been looked at; see §4.7.

### 1.5 ROI tracing, the object filters, measurements — **the count gap**

This is now the whole remaining gap, and §1.4's correction at the top of this
file is the argument that it is. Everything QuPath does after the watershed, in
order, with what each is worth on the profiled region:

| Stage | Measured effect on the 88,424 basins |
| --- | --- |
| `getFilledPolygonROIs`, 4-connected, holes filled | not measured; changes object identity, not only count |
| mean intensity `> 0.1` on the background-subtracted OD | 66,740 survive (25% dropped) |
| `bp.filter(MAX)`, AND with the thresholded LoG | merges adjacent basins; not measured |
| `watershedPostProcess`: EDM watershed, splits by shape | splits some back; not measured |
| area outside 10–400 µm² (40.2–1606 px² here) | 64,826 survive (27% dropped) |
| both filters together | **50,927** |
| QuPath | **25,757** |

The two filters that are cheap to estimate account for a little under half the
gap. The remaining 2x has to be in the merge-and-split, the hole filling, or the
tile seams of §2, and **which of those it is has not been established** — the
estimates above were taken on raw basins, where QuPath measures traced,
merged-and-resplit objects, so they are indicative and not a prediction.

Order to port in, cheapest evidence first:

1. **The mean-intensity and area filters**, which need only per-label sums and
   counts over the label image — no tracing. Do these first: they are a
   histogram pass, and they will say how much of the gap is really elsewhere.
2. **Tile seams (§2)**, which are a correctness bug independent of all this and
   inflate every count above.
3. **ROI tracing.** The fiddly one: QuPath uses ImageJ's `Wand` with a specific
   contour-following convention and fills interior holes, and the vertices must
   match.
4. **Merge and shape split**, which needs a Euclidean distance map and a second
   watershed.
5. **Measurements**, into the columnar table `qupath-objects` already provides.

A fixture for each, recorded the same way as everything above. Note that the
mean-intensity test measures `ipToMeasure`, which is the background-subtracted
optical density **before** the blur and Laplacian — not the LoG the watershed
ran on, and not the raw density either.

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
4. ~~Laplacian convolution~~ — **done**, 3.54x in isolation and 3.69x in place,
   from 20.6% of pipeline stage time to 6.4%, marker count unchanged and output
   still bit-identical against ImageJ.

   **It was the same defect as §4.3, in code written after §4.3 fixed it.**
   ImageJ's `Convolver` accumulates one output pixel into a loop-carried
   `double`, so nine taps ran as nine dependent adds. The tell was in the stage
   table and was ignorable if you were not looking: a nine-tap stencil cost more
   than the separable twenty-nine-tap blur ahead of it. **A stage that costs
   more than a strictly larger stage is a stall, and that ratio is worth
   checking for on every stage this list adds** — it is now the second time the
   same dependency has been shipped.
5. **The watershed is now about 17% of pipeline stage time** — down from 40%
   only because §1.4b added a much larger stage in front of it, not because it
   got faster. In absolute terms it is unchanged.
6. **The background stage is now 52% of pipeline stage time**, which makes it
   the largest single lever and the first thing §4 should look at once §1 and §2
   are done. It splits 59% erosion / 40% reconstruction, and **neither half has
   been examined**:

   - The erosion is the existing rank filter at radius 16.03, where every
     measurement in `docs/benchmarks/README.md` was taken at radius 10 or below.
     Its rectangle decomposition should make cost roughly independent of radius;
     that it is now the single most expensive thing in the pipeline says the
     assumption is worth re-testing at this radius rather than assumed.
   - The reconstruction is fresh code and has had no optimisation pass at all.
     Its raster halves are the same shape as the loop-carried accumulator that
     §4.3 and §4.4 each found, but the dependency here is genuine — each pixel
     reads the neighbour the pass just wrote — so the fix that worked twice does
     not apply, and the §4.4 heuristic below would mislead here. The queue phase
     is a different matter and is order-independent (§1.4), so it is free to be
     restructured.

   Apply the §4.4 heuristic before believing any of this: measure in place, and
   check the ratio against the neighbouring stages.
7. **SIMD**, last, per §0.5 — and only behind a scalar fallback with a
   differential test. Note §4.3 and §4.4 got 2.3x and 3.7x without any explicit
   SIMD, by removing a dependency rather than by widening an instruction.

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
- **Check *which* algorithm QuPath calls before porting one.** §1.4 said
  `MaximumFinder` for two revisions of this file. `WatershedCellDetection` never
  calls it; it calls `MorphologicalReconstruction.findRegionalMaxima`.
  `MaximumFinder` is real and is used by three *other* QuPath detectors, which
  is how it got in here — a plausible name from a neighbouring file, exactly
  like the `EOSIN` constant that was the DAB vector. The port would have been a
  correct implementation of something this pipeline never runs. Read the call
  site, not the class that sounds right.
- **A recorder must exit, not merely finish.** ImageJ's parallel filters leave a
  cached pool of non-daemon workers alive, so `GaussianBlur`'s recorder wrote
  its fixture and then hung in `DestroyJavaVM` forever, taking `record.sh` down
  with it. Same shape as the entry above: one recorder stalling the whole
  script. Every recorder now flushes and calls `System.exit(0)`, and the flush
  is load-bearing — `System.exit` does not drain `System.out`, and a truncated
  fixture is worse than a hang because it still looks like a recording.
- **A stage that costs more than a strictly larger stage is a stall.** The
  Laplacian's nine taps cost more than the separable twenty-nine-tap blur ahead
  of it, because both had the same loop-carried accumulator — the second one
  written after the first was fixed. The stage table shows this for free; read
  it that way when a stage lands. Note the converse is not a clean bill of
  health: §1.4b's background stage is the most expensive in the pipeline and
  also has by far the largest kernel, so the heuristic says nothing about it
  either way.
- **Measure what a stage removes before claiming it is the one that will.** This
  file predicted for two revisions that §1.4's prominence test was what would
  bring the marker count down. It is now ported and bit-identical, and it
  removes 0.0% of the markers on real data — the test only discards maxima on
  exact plateaus, and a Laplacian of Gaussian of real pixels has none. The claim
  was inferred from the source and never checked against a run, which is the
  same failure as the `MaximumFinder` and `EOSIN` entries above. A count is
  evidence about the stage that produced it, never about a stage that does not
  exist yet.
- **Reproducing a stage's inputs is not reproducing the stage.** The pipeline
  ran a morphological opening where QuPath runs an opening by reconstruction and
  then *subtracts* it. Same family of operation, opposite result: one keeps the
  background, the other keeps the nuclei. It had been there through three
  count changes, all of which were attributed to other stages.
