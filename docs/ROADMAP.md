# Roadmap

Where the port stands, what comes next, and what each next thing needs before
it can claim anything. Written to be picked up cold.

**Start with `docs/MEMO.md`** if you have not read it — twenty minutes of
orientation, including an honest accounting of what is genuinely faster and what
only looks it. This file is the detail underneath that.

`docs/VIEWER.md` is a separate project — a slide viewer on top of
`qupath-slide`. It is not on the parity path and claims no speedup; it is
tracked apart from this file for that reason.

## State of play

Five crates, all five now with parity fixtures recorded from a real QuPath
build.

| Crate | Does | Parity | Measured |
| --- | --- | --- | --- |
| `qupath-objects` | Columnar detection/measurement table | `MeasurementPrecision.json` | ~100x on scan/filter |
| `qupath-watershed` | Marker-controlled flood, bucket queue, 4/8-connected labelling, filled-region tracing, object outlines | `WatershedFlood.json`, 3 cases; `RegionalMaxima.json` labels, 13 surfaces; `ObjectFilters.json` regions, 17 images; `WandOutline.json` vertices, 9 cases | queue 20-32x vs heap; flood 1.40x vs previous; labelling, tracing and outlines identical |
| `qupath-filters` | MIN/MAX rank filter, Gaussian blur, convolution, morphological reconstruction | `RankFilterMinMax.json` 42 cases; `GaussianBlur.json` 10 kernels + 12 images; `LaplacianConvolution.json` 5 scales + 17 images; `RegionalMaxima.json` 11 reconstructions + 13 surfaces | rank 3.1-9.7x vs ImageJ; blur, convolution and reconstruction bit-identical |
| `qupath-slide` | Aperio SVS / tiled TIFF reader | `SlideRegion.json`, 5 regions; `OpenSlideLevels-*.json`, the level table and tile size on 2 slides; `ProjectOpen.json`, a tissue block byte-identical to OpenSlide's decode | 4000x4000 region in 0.09 s |
| `qupath-pipeline` | Tiles a region QuPath's way and streams each tile through every stage from the deconvolution to the area filter | `ColourDeconvolution*.json`; `ObjectFilters.json` statistics and decisions, 17 images; `MergeAndSplit.json`; `RefineBoundary.json` | 70.1 Mpx/s on a saturated grid; 1.065x QuPath's cell count pooled over ten regions (1.012-1.129 by region) |

**The detection chain is closed.** Every stage `WatershedCellDetection` runs from
the colour deconvolution to the objects it goes on to measure is ported, pinned
by a fixture recorded from a real QuPath build, and wired into the pipeline on
QuPath's own tile grid: the background subtraction, the Laplacian of Gaussian,
the regional maxima, the labelling, the flood, the region tracing, the intensity
filter, the merge, the shape split, `refineBoundary` and the area filter.

**On the profiled 4000x4000 region that is 26,647 objects against QuPath's
25,757 cells — 1.035x, from 1.86x.** Read that as one region, not as the
result: measured over ten regions the pooled gap is **1.065x** and the range is
**1.012–1.129**, with this region the best of them. See `docs/MEMO.md` §2 for
the sweep and what the gap actually tracks, which is not the seam resolution
D-008 predicted. Each figure below is the stage measured
against the stage before it, never a prediction:

| Stage | Objects | Change |
| --- | ---: | ---: |
| basins traced | 80,762 | |
| intensity filter | 51,152 | −36.7% |
| merge | 26,397 | −48.4% |
| shape split and `refineBoundary` | 45,990 | +74.2% |
| area filter | 28,359 | −38.3% |
| tile seams | **26,647** | −6.0% |
| QuPath | 25,757 | |

**Do not read 1.035x as clean — and do not start at the seams.**
`DIVERGENCES.md` D-008 is real: QuPath resolves its seam overlaps on **cells** —
`cellExpansion` is 5 µm and `preferNucleus` is `false` — using smoothed polygon
geometry, where this compares nucleus pixel masks. A cell is strictly larger than
its nucleus, so QuPath removes at least as many duplicates as this does, and the
residual 890 objects are the same order of magnitude as the 1,712 resolved away
here.

**That reasoning is sound and it is no longer where to look first.** The
ten-region sweep sorts the gap by cell **density**, not by seam count, and three
of the ten regions are a single tile with no seams at all and still disagree —
one of them by 1.113, the widest of the three. D-009 has since ruled out the
merge, the shape split and the background estimate against QuPath on real tissue,
pixel for pixel. What is left is between the deconvolution and the flood, and
that is the first place to look for the remainder.

**The end-to-end time is still not comparable to QuPath's 15.87 s.** What
separates the two is now the tail — the measurements — which does not change how
many objects there are, and which QuPath spends time on.

### The correction this file owes: §2's plan was wrong in the same way §1.5's was

§2 said to "read tiles with a halo of at least the largest kernel radius plus
the maximum expected object radius ... and keep only objects whose centroid
falls in the unpadded core". **Every clause of that is a different algorithm
from the one QuPath runs**, and five minutes at the call site said so:

- **The halo is not derived from a kernel radius.** `getTileOverlap` is
  `(10 µm + cellExpansion) / pixelSize * 2` — 60 px here — and no filter's
  radius appears in it. The background erosion's radius is 16 px and its mask
  dilation's is 32; neither is what QuPath pads by.
- **Objects are not owned by a core.** `ParallelTileObject.resolveOverlaps`
  keeps the **larger** of two overlapping detections whenever the intersection
  exceeds 10% of the smaller. Centroid ownership discards the copy that saw the
  whole nucleus whenever its centroid happens to land outside the core, which is
  the opposite of what the halo is for.
- **The tiles are not the slide's tiles.** This is the part that mattered most
  and that no plan mentioned at all: detection tiles at
  `PREFERRED_TILE_SIZE = 2048`, and a region under `MAX_TILE_SIZE = 3072` is not
  tiled at all. The pipeline was processing the **TIFF's 256 px storage tiles**,
  so it had a seam every 256 px where QuPath has one every 2000.

That last point is the whole of the §2 bug, and it was never a halo problem.
Measured on the profiled region, changing only the grid: **62,102 candidates on
a storage-sized grid against 46,772 with no seam at all**, a 1.33x inflation.

The lesson is the one the previous session already wrote down and this file
still had to learn again: **this file's plan for a stage is not that stage's
call site.** Both remaining plans in §1.5b were written the same way.

**The pipeline's own figure is not a speedup series and must not be read as
one.** It has moved 0.114 s -> 0.062 s -> 0.091 s -> 0.114 s -> 0.21 s -> 0.215
s, and no step was the same work as the one before it: §1.1 corrected a wrong
stain vector, and §1.2, §1.3, §1.4 and §1.5a each added a stage that had been
missing. A figure that goes up because a stage was added is the honest outcome.

**The counts are not a quality series either**: 397,822 -> 211,728 -> 42,966 ->
91,957 -> 88,424 basins -> 66,739 candidates -> 48,045 after §2 put the tiles on
QuPath's grid -> 48,033 once the seam resolution stopped skipping a copy QuPath
had already deleted -> **26,647** once the merge, the shape split,
`refineBoundary` and the area filter landed, against QuPath's 25,757 cells. Only
the last step was a stage removing what a plan said it would, and its plan was
the one written from the call site rather than from this file.

**Nor is the throughput figure.** It has moved 0.215 s -> 0.95 s -> 1.09 s on
the profiled 4000x4000 region, and neither step is a regression: the first was
§2 putting the region on QuPath's grid, where it is 4 tiles on 20 threads rather
than 272, and the second is four stages that did not exist before being in the
path. On a region large enough to fill the pool the pipeline does **70.1
Mpx/s**, best of three on a 9000x9000 region at 25 tiles on 20 threads. It was
80.4 there before this session's stages landed, and 66.7 before §4.7's raster
pass; the 42.6 this file used to give is a different region with more tissue in
it and the two are not comparable. **A figure that moves because a stage was
added is the honest outcome** — quote it with the stage list attached, or not at
all.

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
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo run --release -p qupath-pipeline --example tile_seams
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example reconstruct_cost
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example reconstruct_previous_vs_current
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example previous_vs_current
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example trace_previous_vs_current
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example split_cost
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example erode_cost
# the erosion's horizontal half: the chain-cut diagnostic and the log-depth
# AVX2 scan that closes §4.9's last lever — see §4
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example scan_chain_cut
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example reconstruct_rows
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example reconstruct_blocked
# the saturated grid the throughput figure is quoted on, and the one whose
# stage shares are the ones to optimise against — see §4
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example region -- \
    data/CMU-1.svs 20000 5000 9000 9000
# the same region at one, four and twenty threads, which is what says how much
# of a stage's share is the stage and how much is the pool
RAYON_NUM_THREADS=4 RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline --example region -- \
    data/CMU-1.svs 20000 5000 9000 9000
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

This stage costs **37.6%** of pipeline stage time, still the largest by a wide
margin — 26.2 points of it the reconstruction and 10.4 the radius-16 erosion.
It was 52.9% before the stages of §1.5b landed around it; in absolute terms
nothing about it moved. See §4.7.

### 1.5a Region tracing and the intensity filter — **done**

Pinned by `ObjectFilters.json`: 17 label images through
`getFilledPolygonROIs(ipNucleusLabels, Wand.FOUR_CONNECTED)` and the filter loop
that follows it, including two built by running the real chain — blur,
Laplacian, regional maxima, labelling, flood — so the stage is checked on the
input it actually meets. Region maps match pixel for pixel, areas and survival
decisions match exactly, and **means match bit for bit**: this is a sum of `f32`
widened to `f64`, so there is nothing transcendental to excuse a difference and
the test asserts equality rather than a tolerance.

**A previous plan for this item said the filters "need only per-label sums and
counts over the label image — no tracing". That was wrong on both halves**, and
reading the call site rather than the summary is what caught it:

- **The Wand does not trace labels.** It is handed the threshold range
  `[0.5, inf)`, so it outlines whatever run of *labelled* pixels it can reach.
  Two basins meeting with no watershed line between them are **one** object.
  Measured on the profiled region: 88,424 markers become 88,423 traced regions,
  so it happens once here — but a port that iterates label values would pass
  every synthetic case and be wrong in a way nothing downstream reports.
- **The area filter cannot be applied here at all.** It runs on
  `getFilledPolygonROIs(bp, ...)` *after* the merge and the shape split, on
  those objects rather than these. Applying it to raw basins, as the earlier
  estimate did, measures something QuPath never computes.

Five conventions the call site does not show, each mutation-checked against a
case that fails without it:

- **Holes are filled**, so a ring-shaped basin is measured over its interior
  too, including pixels carrying no label. `ring-with-hole` is a case where the
  filled hole is exactly what drops the object.
- **A basin enclosed by another is discarded outright**, not merged and not
  kept: the *filled* polygon is marked completed, so its turn in the scan never
  comes.
- **Background is eight-connected**, dual to the four-connected regions, so a
  hole is a run that cannot escape even diagonally. Four-connected background
  fills too much and fails on `random-dense`.
- **The mean test is `<=`**, so a candidate landing exactly on the threshold is
  dropped. `mean-exactly-threshold` uses 0.5, which is exact in `f32` and `f64`,
  because the default 0.1 is exact in neither and would prove nothing. Note the
  *second* filter pass, after the merge, tests `<` on the same quantity.
- **ImageJ's statistics skip non-finite pixels**, in `pixelCount` as well as in
  the sum, so the area a later filter compares against 10–400 µm² is not the
  region's size. This one was **not** pinned by the first recording — no case
  had such a pixel and the mutation passed — so `non-finite-pixels` was added
  and the fixture re-recorded rather than the convention left on trust.

A sixth was tried and is **unobservable**: whether a filled hole overwrites
another region's index or only claims unclaimed pixels. Both spellings pass
every case, because an encloser's outline runs above whatever it encloses and is
therefore always traced first. Recorded here so the next reader does not spend
the same hour on it.

The mean is measured on `ipToMeasure`, which is the background-subtracted
optical density taken **before** the blur and Laplacian — not the LoG the
watershed ran on, and not the raw density either. The tile scratch now carries
that copy, and `subtract_background` clears its mask on every call rather than
only when it rebuilds it: the filter reads that mask, and a previous tile's
would reject candidates at coordinates meaningless in this one.

Cost in place: tracing 7.1% of pipeline stage time, the filter 0.5%. Both
roughly halved as a share when the stages below landed around them; in absolute
terms neither moved. The §4.4 ratio check does not fire — tracing does about 1.1
image-equivalents of work (6.1 M labelled pixels flooded, plus 13.7 M
bounding-box pixels for the hole test against a 17.8 M image), which is
comparable to the watershed beside it at 11.7%.

### 1.5b What is left — **the chain is closed; this is now the tail**

Everything this section listed is ported, pinned and wired in. The table it used
to hold as a prediction is the measured table at the top of this file, and the
gap it was tracking went from 2.6x to **1.035x**.

The one number worth restating here, because it was this section's only piece of
evidence before the pipeline could be asked: on `MergeAndSplit.json`'s realistic
case the merge and split together took **87 candidates to 49 objects, 1.78x**.
In place on the profiled region the same pair is 51,152 to 45,990 — 1.11x — and
the difference is not a defect in either number. The fixture's case is 96x96
random pixels, where nearly every candidate has a neighbour close enough for the
dilation to bridge; a real tile is mostly nuclei that already stand apart. **A
ratio measured on a synthetic input is not that ratio on a real one**, which is
§5's own lesson arriving a second time and from the other direction.

What remains — the old list's steps 4 and 5, with one added — none of which
changes how many objects the pipeline finds:

- **Step 4, ROI vertices — done, contour and smoothing both.**
  `qupath-watershed`'s `outline` traces the vertex list QuPath's `PathObject`
  carries, pinned against ImageJ's own Wand by `WandOutline.json` over nine
  cases, and `smooth_boundary` runs `smoothBoundaries` over it — the
  interpolate/smooth/interpolate/simplify chain at `WatershedCellDetection`'s
  lines 925-933 — pinned stage by stage by `SmoothBoundaries.json` over twelve.

  Every intermediate is compared, not only the polygon that comes out, because
  stage three's interpolation interval is `min(2, n * 0.1)` over the count
  **stage two** produced: a stage two of the right shape and the wrong length
  respaces everything after it and can still land close to the recorded end.

  Four things in that chain are not what they look like, and the last one is
  the reason the fixture exists rather than a careful reading:

  - `getInterpolatedPolygon` **closes the ring** before walking it, so four
    vertices go in and five come out, and it is the five that stage two's
    stride counts over.
  - `smoothPolygonRoi` steps `i += 2` and so **halves** the ring. It is not a
    3-point moving average, though `ShapeSimplifier.smoothPoints` — public, in
    the same codebase, and the one actually named "smooth" — is exactly that.
  - A **coarser downsample simplifies less**. The threshold is
    `sqrt(downsample)/2` while the coordinates scale by `downsample`, so the
    polygon outgrows the tolerance: `ell` keeps 3 points at downsample 1 and 7
    at downsample 4. At downsample 1 the scaling is the identity and the whole
    interaction is invisible, which is why the fixture records two.
  - `getInterpolatedPolygon`'s `allowToAdjust` is **`interval < 0`**, not
    `>= 0`. A negative interval asks ImageJ to converge the spacing until the
    ring closes evenly; a positive one asks for exactly that spacing with a
    short last segment. Reading the bytecode's `dcmpg; ifge` the natural way
    round inverts this, and it fails quietly rather than loudly: the
    convergence loop then runs on every call, a unit square converges near
    0.87, and every vertex comes back slightly off. What settled it was asking
    ImageJ rather than reading it — a 3x2 rectangle interpolated at 1.0, 3.0
    and 0.7 comes back with steps of exactly those, which is only possible if
    the loop never runs. §5's lesson again, applied to a branch.

  Four things the fixture settled, and three would have been guessed wrong:
  vertices sit on pixel **corners**, not centres; collinear runs emit **only
  corners**, so a four-pixel bar is four points; a **hole is filled**, not
  traced; and the walk keeps the **interior on its right**, which winds
  clockwise on screen and gives a *positive* shoelace sum with `y` downwards.

  `FOUR_CONNECTED` turned out not to live where it looks like it should. The
  obvious place is a turn table with the diagonal arm flipped, and that is
  wrong: a concave corner and a corner shared by two regions reach the *same*
  arm, so the turn cannot tell them apart. What can is whether the region
  continues around the corner — filled at a concave one, empty at a shared one.
  Without that test, seeding `diagonal_touch`'s lower pixel traced the **upper**
  one, and the mask-side test could not have caught it: the scan had already
  completed the upper region, so the two outlines came out merely duplicated.

  The listing **offset** is not ported, and that is a named gap rather than a
  widened tolerance. `single_pixel` and `square` are listed from the region's
  bottom-right corner, which looks like a rule until `ell` is listed from
  `(2, 3)` — not a corner of its seed pixel at all. The offset falls out of
  where `Wand`'s own scan begins and is not determined by anything recorded.
  The test compares up to rotation while pinning every vertex, the length and
  the winding; what consumes these lists is smoothing and an area, and neither
  depends on which vertex is written first.
- ~~**Step 4b, cell expansion**~~ — **done**. `qupath-watershed`'s
  `expand_labels` is the EDM/negate/flood chain `WatershedCellDetection` runs
  over the nucleus labels, and `cell_areas` is `labelsToFilledROIs` and the area
  of what it traces. Pinned by `CellExpansion.json` over ten cases at four
  checkpoints each, and the test discriminates: mutating either load-bearing
  constant — the EDM's `(byte)255` background, or the `<=` on the
  `-cellExpansion` threshold — fails it.

  What is left of D-008 is therefore **not** a missing capability. It is routing
  the pipeline through these and resolving overlaps on cells rather than on
  nucleus masks. The shortcut named when this was still open still applies:
  resolving on *smoothed nuclei* would compare the wrong objects more precisely
  and close nothing.
- **Step 5, measurements**, into the columnar table `qupath-objects` already
  provides.

A fixture for each, recorded the same way as everything above.

### The correction this file owed: §1.5b's step list had been missing a stage

The list below said the area filter runs on what the shape split leaves. **It
did not.** Between them stands `refineBoundary`, and reading the call site
rather than this list is what found it:

```java
if (refineBoundary && sigma > 1.5) {
    FloatProcessor fpBoundaryCleanup = (FloatProcessor)fpDetection.duplicate();
    fpBoundaryCleanup.blurGaussian(1);
    fpBoundaryCleanup.convolve(new float[]{0, -1, 0, -1, 4, -1, 0, -1, 0}, 3, 3);
    ByteProcessor bp2 = SimpleThresholding.thresholdAbove(fpBoundaryCleanup, 0.0);
    bp2.copyBits(bp, 0, 0, Blitter.MIN);
    bp.filter(ByteProcessor.MIN);
    bp.copyBits(bp2, 0, 0, Blitter.MAX);
}
```

Three things make this load-bearing rather than a detail:

- **It fires on CMU-1.** `refineBoundary` is a hard-coded `true` with a `TODO`
  beside it, and `sigma` is `1.5 / 0.4990 = 3.006`, comfortably over the 1.5.
- **It needs an image the pipeline does not keep.** `fpDetection` is the *raw*
  deconvolved channel — before the median, before the background subtraction,
  before the blur. That is a third distinct image, and it is neither `log` nor
  `to_measure`. §1.5a had already found that the mean is measured on an image
  nothing else uses; this is a second one.
- **It is a second Gaussian and a second Laplacian** over the whole tile, at
  sigma 1. On a memory-bandwidth-bound pipeline that is not free, and it is not
  in any cost estimate in this file.

The order it was ported in, cheapest evidence first, and what each was worth:

1. ~~**Tile seams (§2)**~~ — **done**, and it was worth 1.33x of the gap. See §2.
2. ~~**Merge and shape split**~~ — **done**, bit-identical, pinned by
   `MergeAndSplit.json` at four checkpoints: the merged mask, the float EDM, the
   `MaximumFinder` segmentation and the mask `toWatershed` leaves. Six mutations
   were tried against it and five fail. The distance map is exact rather than a
   chamfer, and `isEDM = true` is what makes the maxima grouping work on
   plateaus. In place: 51,152 candidates to 26,397 merged blobs.
3. ~~**`refineBoundary`**~~ — **done**, bit-identical, pinned by
   `RefineBoundary.json` at two checkpoints: the sigma-1 Laplacian of Gaussian
   compared by its bits, and the mask the block leaves. Fourteen cases, eight
   mutations tried and all eight fail. The whole block is
   `erode(bp) OR (bp AND cleanup > 0)`, and the merge's `MAX` and this `MIN`
   share `ByteProcessor.filter` — where the merge could not tell a replicated
   border from a zero-padded one, this can, and `all-foreground` is the case
   that does. Together with the split: 26,397 to 45,990.
4. ~~**The area filter**~~ — **done**. 45,990 to 28,359, 38.3% dropped. Its
   three tests are not the first filter's: the mean is `<` where that one is
   `<=`, and both area tests *and* the mean test sit inside
   `if (minArea > 0 || maxArea > 0)`, so clearing the area window removes the
   mean test with it.

**The wiring landed with them**, which is what this section said it would. The
area filter came with the wiring rather than after it, for the same reason the
merge did not land alone: the seam resolution has to run on QuPath's final
object set, and running it a stage early would have measured something QuPath
never computes.

### 1.6 The claim

**Object counts: 26,647 against QuPath's 25,757, 1.035x** — one region, and the
best of the ten later measured; the pooled figure is 1.065x, see `docs/MEMO.md`
§2 — on the profiled
4000x4000 region at QuPath's own tiling. That is the first end-to-end count
comparison this project has been able to make, and D-008 says where the residual
most likely is.

**What is still not claimable is time**, and no honest comparison against
QuPath's 15.87 s exists yet: QuPath spends part of that on the tail this
pipeline does not have. **The stronger success criterion has now been measured, and it is not met.**
§5 asks for Dice or IoU >= 0.99 on the object masks. `ObjectMasks.json` holds
QuPath's per-object masks over three single-tile regions, `examples/object_masks`
compares them, and the pooled Dice comes to **0.935, 0.934 and 0.880** — mean
Dice 0.921 over 16,759 one-to-one pairs, with 97.9% of QuPath's 17,079 nuclei
matched at Dice >= 0.5.

That is a miss against §5 and is recorded as one rather than as a tolerance to
be widened, which `parity/README.md` forbids. Two things are worth separating in
it. The **shape**
agreement on objects that correspond stays high across regions — mean Dice 0.929,
0.928 and 0.894, a spread of 0.035 — while what moves far more is **how many objects have no counterpart at
all**, which tracks density exactly as the count gap does (D-009 carries the
table). So the residual is not a boundary that lands consistently one pixel out
— that would depress Dice everywhere and it does not — it is a population of
extra objects on sparse tissue. Reaching 0.99 means closing D-009, not tightening
a tracer.

A matching count was necessary and was never sufficient: two runs can agree on
how many objects there are and disagree about where. That is no longer the
strongest thing said here.

---

## 2. Tile seams — **done**, and the plan for it was wrong

Ported in `crates/qupath-pipeline/src/tiles.rs` and `overlaps.rs`. See the
correction at the top of this file for what the plan said and what the call site
said instead. What landed:

- **The grid**, from `addRunnableTasks` and `RoiTools.computeTiledROIs`: cores of
  `ceil(width / ceil(width / 2052))`, centred on the region, each grown by 60 px
  on every side and intersected with the region. The profiled 4000x4000 region
  becomes **4 tiles of 2000 with a 60 px halo**, where it used to be 272 storage
  tiles of 256 with none.
- **The resolution**, from `ParallelTileObject.resolveOverlaps`: keep the larger
  of any two detections from different tiles whose intersection exceeds 10% of
  the smaller. Pixel masks stand in for JTS geometry, exactly rather than
  approximately — the Wand's polygon encloses whole pixels, so its area *is* the
  filled pixel count.

Measured on the profiled region, changing only the grid. **The right-hand column
is now objects after the whole chain**, where it used to be candidates after the
intensity filter, so it is not comparable to the figures the paragraphs below
discuss:

| Grid | Tiles | Markers | Regions | Duplicates | Objects |
| --- | ---: | ---: | ---: | ---: | ---: |
| storage-sized, 240 px, no halo | 289 | 79,399 | 79,398 | 0 | 31,982 |
| **QuPath's, 2052/3078, 60 px halo** | 4 | 80,763 | 80,762 | 1,712 | **26,647** |
| one tile, no seam | 1 | 75,736 | 75,735 | 0 | 26,420 |

Reproduce with `cargo run --release -p qupath-pipeline --example tile_seams`.

**The candidate-level figures the paragraphs below were written against**, kept
because the conclusions are theirs: 62,102 / 48,045 / 46,772 on the three grids,
and the middle row became 3,119 duplicates and 48,033 candidates once
`resolve_overlaps` stopped skipping a second-tile copy something had already
removed — QuPath does not, because `listSecond` is a snapshot and a deleted copy
goes on deleting. Twelve objects, 0.02%, changing none of the conclusions.

Three things worth keeping from this:

- **A halo cannot make tiling exact, and is not meant to.** Both reconstructions
  in this pipeline are geodesic dilations, which propagate without bound, so no
  finite halo reproduces a single-tile run. QuPath has the same property: its own
  4-tile run lands at 48,045 against a seamless 46,772, a 2.7% difference that is
  inherent to tiling rather than a defect. **The target is matching QuPath's
  grid, not choosing a better one.** At object level the same two runs are 26,647
  and 26,420, **0.9%** — the stages after the filter absorb most of the seam
  disagreement, which nothing predicted and which is worth knowing before anyone
  proposes a wider halo.
- **The count fell 28%**, 66,739 to 48,045, and the gap to QuPath's 25,757 went
  from 2.6x to **1.87x**. This is the first stage in three that removed anything
  like what a plan predicted — and it is the one whose prediction nobody wrote
  down, because the item was filed as a correctness bug rather than a count fix.
- **The intensity filter now drops 36.7% where it dropped 24.5%.** Not because
  the filter changed but because the background estimate did: an opening by
  reconstruction on a 256 px tile is a different image from one on a 2000 px
  tile, and the smaller one was wrong.

Still open here, and it is now the weakest link in the count claim rather than a
loose end: an object placed exactly on a boundary is covered by
`overlaps::tests`, on hand-built candidates rather than through the pipeline. A
fixture recorded from QuPath's own `resolveOverlaps` would be stronger and does
not exist — and **D-008 says the port is comparing the wrong objects anyway**,
nucleus masks where QuPath compares smoothed cell polygons. Both of those
objects can now be built here (§1.5b step 4b, and the smoothing before it), so
this is the next thing that can actually be fixed rather than only described.

---

## 3. JPEG 2000, and the FFI §9 asks for

`qupath-slide` reads baseline-JPEG SVS in pure Rust and **refuses** the JPEG
2000 variants by name (`SlideError::UndecodableCodec`). `data/CMU-1-JP2K-33005.svs`
is exactly that case and cannot be opened today.

§9 is unambiguous about the answer: FFI to OpenSlide and/or OpenJPEG, in a
`*-sys` crate wrapped by a safe crate, sources vendored with a pinned hash,
built with `cc`, never shelling out to a Makefile. `M-FFI-NAMING`,
`M-FFI-TRANSLATES`, `M-OOBE` and `M-SYS-CRATES` all apply.

Note there is no OpenSlide *development* install on the current machine — no
headers, no pkg-config — so this starts with vendoring rather than linking.

That is a narrower statement than it used to be, and the difference matters.
There **is** a working OpenSlide here, inside a jar:
`qupath-extension-openslide` depends on `io.github.qupath:openslide`, which
carries its own `libopenslide.so` for JNA to extract at load time. It is what
`parity/record-openslide-levels.sh` and `ProjectOpen` run against, and it is
enough to *record* what OpenSlide reports about a slide — which is how
`VIEWER.md` §8.8's level-table comparison was made. What it is not is something
this Rust code can link, so §9's answer for the JP2K reader stands unchanged.
Writing it the old way cost two items in §8.8, marked blocked for several
sessions on a library that was present; `EVOLUTION.md`'s "A blocker is a claim
about a test that was run" has that account.

**Licensing, from §9, decide before the first commit:** OpenSlide is LGPL and
QuPath is GPLv3. This code is derived from reading QuPath source and is already
GPL-3.0-or-later in `Cargo.toml`. Confirm that stays correct once native
dependencies are linked.

---

## 3b. Following QuPath's interface — in `VIEWER.md` §8, not here

Asked for on the roadmap, and it is recorded there instead for the reason
`VIEWER.md` §0 gives: the viewer is **not on the parity path**, and no fixture
recorded from QuPath governs any of it. Putting interface work in this file
would make it look like a parity claim, which it is not.

One finding from it belongs here, because it is about the pipeline rather than
about the interface. **`Params::default` hardcoded CMU-1's 0.4990 µm/px**, and
every micron-derived default divides by it — 8 µm background radius, 1.5 µm
sigma, 10 and 400 µm² areas. On a slide at 0.25 µm/px all four were out by a
factor of two, silently, with detection still returning plausible objects.
`Slide::microns_per_pixel` now reads Aperio's `MPP`; see `VIEWER.md` §8.0. That
is a `D-005`-adjacent divergence — this pipeline runs at the slide's own pixel
size — and it means **any measurement quoted from a slide other than CMU-1 and
taken before that fix used the wrong filter sizes.** Every number in this file is
from CMU-1, so none of them is affected.

The other thing §8 settles is scope: `qupath-gui-fx` is **101,712 lines** of
JavaFX against this application's 1,600, and the two use opposite interface
models, so it is a port of the *design* rather than of the code. The one piece
that is a genuine specification is the `.qpproj` format, which is JSON.

---

## 4. Optimisation, in §0's order

Only after 1 and 2. Each of these is worth less than it looks — the Amdahl
ceilings in `docs/benchmarks/README.md` multiply to about 1.7x for what is
already ported.

**The stage table, on the profiled region, now that the chain is closed.** Every
share below is against a total of 3.78 s summed across threads, and four of the
rows did not exist a session ago:

| Stage | Share |
| --- | ---: |
| background — regrow | 26.2% |
| background — erode | 10.4% |
| **shape split** | **14.4%** |
| watershed flood | 11.7% |
| tracing | 7.1% |
| **merge** | **6.2%** |
| maxima | 5.1% |
| **nuclei: second trace and area filter** | **4.4%** |
| decode | 3.5% |
| **`refineBoundary`** | **3.3%** |
| gaussian blur | 2.5% |
| deconvolve | 1.3% |
| laplacian | 1.1% |
| labelling | 0.9% |
| object filter | 0.5% |

### The stage table above is a configuration, not a property of the pipeline

**Every share in it was taken on the 4000x4000 profiled region, which is four
tiles on a twenty-thread pool.** On a grid large enough to fill the pool the
table is a different table, and the difference is large enough to change which
stage anyone would work on next. Same slide, same code, same run of `region`:

| Stage | profiled 4000², 4 tiles | saturated 9000², 25 tiles |
| --- | ---: | ---: |
| background — **erode** | 10.4% | **28.0%** |
| background — regrow | **26.2%** | 10.8% |
| shape split | 14.4% | 8.7% |
| gaussian blur | 2.5% | 7.9% |
| maxima | 5.1% | 7.9% |
| watershed flood | 11.7% | 7.3% |
| `refineBoundary` | 3.3% | 6.7% |
| tracing | 7.1% | 3.7% |

Two different things move these and they have to be separated before either
column is used:

- **The tissue.** The profiled region is 34.1% labelled pixels and the
  saturated one 5.9%, and the data-dependent stages follow that — the
  reconstruction settles in fewer passes on sparser tissue, and the flood and
  the tracing have less to do. This is why the regrow's share falls.
- **The threads.** The erosion's cost per pixel is fixed by the kernel and does
  not depend on the image at all, so its share can only have moved because
  everything is measured against a total that moved — and because the stage
  itself got more expensive per pixel. It did. Holding the region fixed and
  varying only the pool, stage seconds summed across threads, where perfect
  scaling would be a flat row:

  | Stage | 1 thread | 4 threads | 20 threads | 20 / 1 |
  | --- | ---: | ---: | ---: | ---: |
  | gaussian blur | 0.490 | 0.556 | 1.529 | 3.12x |
  | decode | 0.192 | 0.214 | 0.582 | 3.03x |
  | **erode** | 1.743 | 1.877 | 5.036 | 2.89x |
  | watershed flood | 0.422 | 0.499 | 1.213 | 2.87x |
  | `refineBoundary` | 0.536 | 0.620 | 1.238 | 2.31x |
  | maxima | 0.572 | 0.638 | 1.317 | 2.30x |
  | regrow | 1.001 | 1.072 | 2.013 | 2.01x |
  | shape split | 0.855 | 0.951 | 1.549 | 1.81x |
  | wall clock | 7.013 | 2.069 | 1.240 | **5.65x faster** |

  **Nothing is flat, and almost none of the inflation happens before four
  threads.** One thread to four costs 6–9%; four to twenty costs another
  2.4–2.8x. That shape is what this machine is: an i7-13800H is **6
  performance cores at 5.0–5.2 GHz with two threads each, plus 8 efficiency
  cores at 4.0 GHz**, so a four-thread run is four distinct P-cores and a
  twenty-thread run is every SMT sibling and every E-core as well. A share
  taken at four threads is a share taken on the least contended configuration
  this machine has.

**So §4.8's reading — that the pipeline is memory-bandwidth-bound and that
cache-blocking is one of the two honest answers — is not established by these
numbers.** Bandwidth would degrade gradually as threads were added; this
degrades almost entirely at the step where slower cores and shared execution
resources arrive. And the blocking §4.8 proposes was measured this session and
is 1.7–2.6x **worse** — see below.

**That run has since been done, and it disproves the bandwidth reading.**
Holding the six P-cores fixed and toggling only SMT costs 1.66x the CPU time to
buy 1.11x the wall clock, which bandwidth cannot explain and execution-port
contention can. The erosion's uncontended floor is 1.16x on six P-cores against
2.20x with SMT, so most of its saturated-grid share is contention rather than
work. Both are recorded with their numbers in §4.9's findings list below.

**Where that leaves the next lever: the erosion, which this file wrote off.**
§4.7 recorded it as 12% of the background stage and said "the erosion is not the
problem". On the saturated grid it is 28% of the whole pipeline and the largest
single stage, and it is the largest at one thread too, so this is not a
scheduling artefact. `erode_cost` says what it is made of, on a real tile:

| | ns/px |
| --- | ---: |
| the erosion, radius 16.03 | 19.30 |
| its 33 folded source rows alone | 2.69 |

**86% of it is the horizontal maxima**, not the vertical folds — **eleven**
rectangles (this file said thirteen; `Plan::for_radius(16.03)` produces eleven,
of half-widths 16, 15, 14, 13, 12, 11, 10, 9, 7, 5 and 1), each costing four
passes along the row (the extended copy, the prefix scan, the suffix scan, the
combine) plus the negated whole-image copy that MIN takes on the way in.
Pairing the prefix and suffix chains so four run in flight instead of one was
tried and is worth **1.00x**, so it is throughput and not latency: the passes
themselves are the cost.

**And the ratio in that table was taken on the wrong build.** Re-measured at
`-C target-cpu=x86-64-v3`, which is the flag every benchmark in this file is
quoted at, on the same 2120² real tile: **25.87 ns/px against a vertical floor
of 3.53**, so the filter costs **7.32x its floor**, not 2.82x. The gap between
those two ratios is the finding. AVX2 speeds the vertical fold — a
slice-to-slice maximum — up **10.7x**, and the horizontal half, which carries a
serial running maximum through each block, only **3.1x** (68.74 to 22.34
ns/px). **The horizontal half is not merely most of the stage, it is the half
that does not vectorise.** The 86% share survives intact on the AVX2 build,
22.34 of 25.87 ns/px; it was the ratio beside it that was measuring a different
program.

### "Fewer passes per rectangle" was the wrong place to look, and here is why

This file named that as the change to try. It has now been worked out rather
than tried, and there is no slack in it. Three arguments, and each closes off
one of the ways in:

1. **Eleven rectangles is the minimum for an exact cover.** Grayscale MAX is
   monotone in the neighbourhood, so a cover that reaches any pixel outside the
   disc inflates the answer — an over-approximation is not merely inexact, it
   is wrong in the direction that matters. Every row `dy` must therefore be
   covered to exactly `hw[dy]`, and a rectangle contributes its full half-width
   to every row it touches, so the widest rectangle touching row `dy` has
   half-width exactly `hw[dy]`. Distinct half-widths force distinct rectangles.
   Radius 16.03 has eleven of them, and `Plan` already emits eleven.
2. **The four passes cannot be shared between rectangles.** van Herk requires
   the block length to equal the window length — with blocks of 33 and a window
   of 31, `max(suffix[l], prefix[r])` over-reaches whenever both ends land in
   one block, which a numeric sweep confirms at every narrower span. And the
   over-reach is not harmlessly inside the disc: the `hw = 1, hh = 16`
   rectangle stretched to 33 columns would read `dx = ±16` at `dy = 16`, where
   the disc allows one. So each rectangle needs its own scan structure.
3. **Nor is either scan doing work that gets thrown away.** The combine reads
   `suffix[..width]` and `prefix[2*half_width..]`; against an extended row of
   `width + 2*half_width` that leaves 32 of 2,152 entries unused at the widest
   rectangle and fewer at the rest — about 1.5%, not a lever.

**The transposed ordering costs exactly the same**, which is worth recording
because it looks like it should win. Computing the windowed maximum once per
*source* row at each distinct width and folding vertically afterwards comes to
`2120 x 44` horizontal plus `2120 x 33` vertical row-passes; the current
ordering is `2120 x (33 + 44)`. Identical. The growing accumulator already
amortises the vertical half optimally, and the transpose only moves the same
horizontal work from the output side to the input side while needing eleven
buffers of 33 rows live per thread instead of four rows.

**What was removed instead**, in `1931ba5`: every output row opened with
`out.fill(NEG_INFINITY)` so the first rectangle's fold had something to beat.
The first rectangle now writes `out` and the rest fold into it — one pass of
about 77 gone. Isolated on a real tile, 0.1164 s to 0.1159 s, lower in all six
alternating pairs; ~0.4%, under the 1.3% its pass count predicts because a fill
is a store-only stream that never misses.

**That 0.4% does not meet the bar §5 now sets**, and this file should say so
rather than leave the two paragraphs to contradict each other. It was measured
by alternating two separately built binaries, and two binaries of identical
source were later found to differ by 7.7% here on layout alone. The change
removes a pass that is provably there and the six pairs did agree in sign, so
it is kept; but the number attached to it is not resolved by the method that
produced it, and the same caveat applies to every sub-percent alternating-binary
claim in this section — including `afae767`'s 1.3%.

**So the erosion's remaining cost is the decomposition working as designed**,
and the next attempt on it is not a better rectangle scheme. The target the
measurements above actually name is the van Herk scan itself: a serial running
maximum along each block of 33, which is a loop-carried dependency and so the
one part of this filter SIMD does not help. Two things follow, neither tried:

- ~~**A vectorised prefix-maximum.**~~ — **written and measured, and it works;
  it is still not worth shipping.** The running maximum within a block is a
  scan, and a scan over eight lanes has a standard log-depth SIMD form (shift
  by 1, 2 and 4, taking a maximum each time). It was the only change identified
  here that attacks the 3.1x rather than the pass count, and it is the last
  untried lever this section had.

  `scan_chain_cut` carries all of it — the chain-cut diagnostic first, which
  this repository's history says should always precede the rewrite, then the
  rewrite itself. On a real 1920² tile, single-threaded, minimum of five
  interleaved repetitions:

  | | ns/px |
  | --- | ---: |
  | scan as written | 6.40 |
  | scan, chain cut (wrong answer) | 4.26 |
  | **scan, log-depth AVX2 (correct)** | **5.37** |
  | scan, no arithmetic (wrong answer) | 2.99 |
  | extend + combine only | 1.11 |

  **The chain is real — cutting it is worth 1.50x — and the log-depth scan
  collects 1.19x of it**, stable across five runs (1.16-1.19x), bit-identical to the scalar
  path over every span and length including `NaN`, both zeros and both
  infinities.

  **And 1.19x on the scan is ~1.013x on the pipeline.** The scan saves ~1.0
  ns/px where `erode_cost` measures the whole filter at 19.37, so it is ~1.05x
  on the erosion, and the erosion is 28% of the saturated grid. That is below
  this machine's run-to-run spread and far below §5's several-percent floor for
  a two-binary claim, so **the change could not be shown to work in place even
  though it demonstrably works in isolation** — and the entry three bullets
  down is the case where a gain of exactly this size reversed sign at 25 tiles.
  The prediction in §4.9's findings list was right on both counts: a vectorised
  prefix-maximum inherits the SMT sensitivity, and its component gain does not
  survive in place.

  It also needs `unsafe`, which the workspace denies outright and which
  `.claude/skills/ms-rust/SKILL.md` permits only behind "a committed profile
  showing what it buys". The probe is that profile and it prices the change out.
  **The erosion's horizontal half is now closed**: the pass count was argued
  shut above, the block scheme's crossover was measured in `7270ed6`, and the
  scan itself is this entry. What is left in the stage is the traffic, which is
  §4.8's problem and not a loop's.

  **One thing the probe caught that is worth keeping.** The first version
  propagated `NaN`. `_mm256_max_ps(a, b)` returns `b` whenever either operand
  is `NaN`, while the scalar fold's `value > acc` discards one — so the
  accumulator must be the *first* operand at every step, and a `NaN` arriving
  in the first lane of a block needs blending to `-inf` on load because no seed
  protects it there. Both were found by the differential test on its first run,
  neither by reading the code. A SIMD maximum is not a drop-in for a comparison
  fold, and this repository's `NaN` convention is load-bearing.

  **Carrying several block chains at once does not work, and it is the obvious
  thing to try first.** Blocks are disjoint reductions, so four chains can run
  in flight with no reordering — the argument that was worth 2.4x on the
  reconstruction's raster pass in `edb7d74`. It is bit-identical, pinned over
  every span and length including NaN and both zeros. It is also **20% slower**
  at four chains, and the regression starts before any interleaving at all:

  | chains in flight | 1 | 2 | 3 | 4 | 6 | 8 |
  | --- | ---: | ---: | ---: | ---: | ---: | ---: |
  | seconds | .1295 | .1302 | .1506 | .1559 | .1363 | .1399 |

  Against `.1152` at HEAD. **One chain through the restructured loop is already
  12% slower than the slice-zip loop it replaced**, so the cost is not the
  interleaving, it is having written the scan as indexed accesses into an array
  of accumulators instead of a `zip` over two slices. That shape is what the
  autovectoriser was consuming; a hand-interleaved version takes it away and
  gives back less than it costs. Any future attempt at this scan has to keep
  the slice-zip form, which is a real constraint on the SIMD version above —
  and it is the second time in two sessions that an in-flight-widening that
  worked elsewhere has reversed sign here.
- **Dropping van Herk at the narrow widths — done, in `7270ed6`.** It is O(1)
  per pixel in comparisons but pays four passes and a serial chain for it,
  and `horizontal_max` chose between it and the shift path at half-width 3, on
  a pass count that assumed both sides vectorise equally. They do not. Swept
  inside one binary on a real tile:

  | threshold | 3 | 6 | **7** | 8 | 9 | 11 | 13 | 17 |
  | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
  | seconds | .1159 | .1153 | **.1152** | .1159 | .1178 | .1198 | .1255 | .1436 |

  Seven wins in ten of ten interleaved runs, about **0.6%**, and the curve
  turns up sharply after 9 — so the shift path is ahead further than counting
  said, but van Herk is still right for the ten widest rectangles. This moves
  three rectangles across: half-widths 5 and 1 of the erosion, 2 of the
  radius-32.06 dilation. The saturated grid still reports 19,063 objects. What
  it does *not* do is touch the widest rectangles, which is where the 22.34
  ns/px actually sits — the first bullet is still the one that matters.

**A note for anyone re-measuring:** `erode_cost` reports 25.87 ns/px with
`-C target-cpu=x86-64-v3` and 106.43 without it. Every figure in this section
is the former. The flag is in the invocation list at the top of this file; run
without it and you are timing a different program.

### Four more changes measured, and reverted

All four are in ROADMAP §5's collection now. Each is reproducible from a probe
committed here, and each was measured on a real tile rather than a generated
one:

1. **Skipping settled rows in the reconstruction's raster passes.** The premise
   was that after a few passes the changes live in a few rows and the rest of
   each sweep is work that happens and is not needed. `reconstruct_rows` says
   they do not: on a real tile every one of the 2,120 rows still holds a moved
   pixel through pass nine, and at pass thirteen — the last one — 1,984 rows
   still move and a conservative skip rule would still visit 2,071 of them.
   Nothing was written; the probe was the whole experiment and cost ten minutes.
2. **Cache-blocking the reconstruction**, which is §4.8's own suggestion.
   Settling a strip of rows several times while it is in L2 rather than
   sweeping the whole tile once. It reaches the same fixed point bit for bit —
   the queue after the raster phase is a complete algorithm, so any sequence of
   valid dilation updates ahead of it is legal — and it is slower everywhere:
   **1.53x** slower in isolation (140.7 ms against 215.9 for the best of fifteen
   strip and round settings), and **1.7–2.6x** slower in place on the saturated
   grid, where the theory said the traffic saving should pay most. The strip
   size barely matters, from 32 rows to 512, which is itself the answer: L2
   residency buys about 20% per sweep and the blocking needs 24–36 sweeps where
   the whole-image passes need 14.
3. **Four rows of a raster pass in flight at once**, skewed two columns apart so
   each row's chain is independent — the change that was worth 2.42x on this
   same pass when the chain was shortened, applied again to widen it. It is
   bit-identical, pinned by `rows_in_flight_change_nothing` over every shape
   from 1x1 up including both zeros, and **its sign depends on the thread
   count**:

   | | pass | reconstruct | in place |
   | --- | ---: | ---: | ---: |
   | isolated, one thread | **1.21x** | 1.14x | |
   | profiled region, 4 tiles | | | **1.05x** |
   | saturated grid, 25 tiles | | | **0.83x** |

   Four alternating pairs of binaries agreed on the last row. It turns three
   sequential streams per tile into nine concurrent ones — five marker rows and
   four mask rows — and twenty tiles of that is what the last column is. The
   gain in isolation is real and it is not available where the pipeline runs.
4. **Pairing the rank filter's prefix and suffix scans**, above: 1.00x.

**The one thing that was kept removes work rather than instructions**, which is
now this repository's only reliable pattern: seven tile buffers were cleared and
zero-filled before the stage that overwrites them, ~150 MB per tile of memset on
a pipeline that had no use for any of it. On the saturated grid that is stage
time 18.079 s to 17.839 s, lower in all three alternating pairs — about 1.3%,
which is at the edge of what this machine can resolve and is the honest size of
it.

**The §4.4 ratio check was applied to all four new stages and does not fire.**
The shape split is the largest of them and lands third overall, but it is
genuinely more work than the flood beside it: four sweeps of an exact distance
map, then `MaximumFinder`'s five stages over it. `refineBoundary`'s 3.3% is a
sigma-1 blur plus a Laplacian, against 2.5% and 1.1% for the sigma-3 pair, which
is what a narrower kernel over the same image should cost. The merge's 6.2% is
dominated by the trace and fill inside it, and the main trace is 7.1%.

### The shape split was where this file said to start, and there is no hotspot there

Measured rather than inferred, with `split_cost` on the merged binary of a real
tile. The split is **26% distance map and 74% segmentation**, and the
segmentation is five stages of which the largest is a third of it:

| Stage | ms on a 2120² tile | share of the segmentation |
| --- | ---: | ---: |
| `getSortedMaxPoints` | 27.1 | 33% |
| the dilation | 19.3 | 23% |
| `analyzeAndMarkMaxima` | 13.1 | 16% |
| the dilation's histogram and buckets | 8.5 | 10% |
| `make8bit` | 5.5 | 7% |
| `watershedPostProcess` and the `AND` | 3.3 | 4% |
| `cleanupMaxima` | 1.8 | 2% |

**Three transformations were tried against it and together they are worth
1.02x**, this machine's noise floor, and all three were reverted:
`trueEdmHeight` through one window with the ridge branch replaced by a select;
its four heights reduced as a tree rather than through one accumulator — the
change that was worth 2.42x on the reconstruction's raster pass; and the
dilation's neighbourhood as eight fixed-offset loads instead of eight
bounds-tested branches.

What was kept is the one change that removes *work* rather than instructions:
`levels`, `list` and `coordinates` are written before they are read, so clearing
them was **22 MB of memset per tile** that nothing observed.

The stage is five sequential passes over buffers that do not fit in cache, in an
order set by grey level rather than by position. **Making it faster means
changing what it touches, not how it computes**, and that is a different piece
of work from the four in this list.

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
5. **The watershed is 11.7% of pipeline stage time** — down from 40% only
   because §1.4b, §1.5a and §1.5b added stages around it, not because it got
   faster. In absolute terms it is unchanged, and it has been unchanged through
   three revisions of this number.
6. ~~**Region tracing**~~ — **done, 1.1x, and it is the only thing this session
   found that was worth keeping.** The hole test walked each region's bounding
   box **twice**: once to flood the background inwards from the grown border,
   and once more to claim whatever the flood had not reached. The second pass is
   only ever useful when the first left something behind, and the first already
   knows — count the pixels it reached inside the box, add the region's own
   area, and if that reaches the box's area there is nothing to find. The two
   counts cannot overlap, because a region pixel is never seeded and never
   marked.

   **On a real tile the skip fires for 26,976 of 27,308 regions**: a nucleus
   with a hole is rare. Interleaved, asserting the region map *and* the region
   count identical on both images the pipeline traces: **1.15x on the recorded
   run, 1.08x to 1.16x across five**, so read it as 1.1x. It is worth 1.015x on
   the pipeline at most — the tracer is ~15% of stage time across its three
   stages — and on this machine that is inside the run-to-run spread of the
   end-to-end figure. The object count is unchanged at 26,647.

   **The division was not the cost, and that is the fourth time.** Both floods
   pop an offset and recover `x` and `y` with `at % width` and `at / width`,
   about 4 M hardware divisions per tile. Removing every one of them by carrying
   packed coordinates is worth **1.00x**, measured one variable at a time, and
   was reverted. `trace_previous_vs_current` is the harness.
7. **The background stage is 37.6% of pipeline stage time** — 59% before §2,
   52.9% before §1.5b — and is still the largest single lever. **The 59%
   erosion / 40% reconstruction split this file used to record was measured on
   256 px tiles and does not survive §2.** At the tile size QuPath actually uses
   it is:

   | | 256 px tiles (old) | 2120 px tiles (measured) |
   | --- | ---: | ---: |
   | erosion | 59% | **12%** |
   | reconstruction | 40% | **86%** |

   So the erosion — the half this file pointed the next optimisation pass at —
   is not the problem, and its rectangle decomposition holds up at radius 16
   after all. Reproduce with
   `cargo run --release -p qupath-pipeline --example reconstruct_cost`.

   The reconstruction is 26.2% of pipeline stage time on its own, and the
   regional maxima add another 5.1% of the same code. Two things have now been
   ruled out rather than assumed:

   - **The repeat heuristic is already optimal.** `morphologicalReconstruction`
     alternates raster passes while one still moves a tenth of the image, and
     because the fixed point is traversal-independent (§1.4) that fraction can be
     changed freely. Measured on a real tile, interleaved, asserting bit-identical
     output at every setting: the textbook one-forward-one-backward hybrid is
     **2x slower**, 1/4 is within noise of 1/10, and 1/50 is 16% slower. QuPath's
     tenth is the right number. `reconstruct_previous_vs_current`.
   - **It does not scale superlinearly with tile size.** Cost per megapixel is
     flat from 256 px to 2120 px (0.055 to 0.063 s/Mpx); the pass count rises
     from 6 to 14 and then plateaus. The stage got expensive because tiles got
     70x bigger, not because it degrades.

   **Done — and the diagnosis this file gave was wrong.** It said the raster
   pass was expensive because it transliterated QuPath's index arithmetic:
   `isize` strides, a runtime `Option` for the queue tested per pixel,
   `first_row`/`first_column` branches inside the inner loop, "several times the
   work the arithmetic implies". Specialising the pass on direction removes
   every one of those and is worth **1.03x**. The loop was already compiling to
   compare-and-blend with no bounds checks.

   The cost was the **loop-carried dependency** — one pixel's result is the next
   one's left neighbour, with three dependent compare-and-selects between them.
   Cutting it (wrong answer, diagnostic only) takes the pass from 3.87 ns/px to
   0.62, so six-sevenths of the stage was that chain and everything this file
   listed was inside the other seventh.

   `min` distributes over `max`, which hoists the half that does not involve the
   left neighbour and leaves two comparisons; spelling the clip as the strict
   `<` that `MINSS` implements rather than the negated `>=` makes each of them
   one instruction. **2.42x on the pass, 2.03x on the reconstruction, 1.71x on
   the regrow in place, 1.19x on wall clock** at 25 tiles on 20 threads, with the
   object count unchanged. Bit-identical, with the change count asserted too
   because it drives the repeat loop. `docs/benchmarks/README.md` has the tables
   and `reconstruct_pass_previous_vs_current` is the harness.

   The reassociation is an identity on a total order and NaN is not in one, so
   `reconstruct` checks both buffers once — under 1% of the stage — and runs the
   exact transliteration if either holds one, rather than assuming the pipeline
   never produces a NaN.

   **The lesson is the §4.3 one again, and this is the third time.** A profile
   share says a stage is expensive; it does not say why, and the reason written
   down without measuring was wrong here as it was for the blur's transpose. The
   measurement that settled it — break the dependency, keep everything else,
   accept the wrong answer, read the clock — took ten minutes and should have
   come first.

8. **The pipeline is now memory-bandwidth-bound, and that is new.** Parallel
   efficiency on a saturated grid, 90 Mpx region, measured:

   | Threads | Mpx/s | Peak | Efficiency |
   | ---: | ---: | ---: | ---: |
   | 2 | 13.4 | 358 MB | 96% |
   | 8 | 30.6 | 1371 MB | 74% |
   | 12 | 36.1 | 1737 MB | 69% |
   | 20 | 42.5 | 3297 MB | 61% |

   Going from 12 to 20 threads buys 3% for 30% more memory. The cause is §2's
   tile size: at 256 px a tile's working set was ~2 MB and sat in L2, and at
   2120 px it is ~145 MB and streams from DRAM. **This is the single biggest
   remaining speed lever and it did not exist before §2.** The honest options are
   cache-blocking the separable stages within a tile, or accepting it; what is
   *not* available is going back to small tiles, because that is the §2 bug.

   **That table predates §1.5b and the peak column has moved**, though the
   efficiency argument has not: a tile's working set is now ~240 MB and the
   20-thread peak is **4.46 GB**, from 3.30. The four new stages needed one more
   float image that cannot be reconstructed (`fpDetection`, which
   `refineBoundary` reads), a byte mask, the merger's region map, and the shape
   split's own scratch at ~17 bytes a pixel — which is the largest single item in
   the budget. Everything else they need is a buffer an earlier stage has
   finished with: the distance map lands in the Laplacian's, and the
   refinement's two working images come out of that and the prominence.
   `bounded_memory.rs` carries the budget as 66 bytes per padded tile pixel.

   Two things already came out of this and are done. **Both were measured
   one variable at a time**, best of 5 on a 9000x9000 region, every variant
   producing the same 142,957 objects:

   | Variant | Best | Mpx/s |
   | --- | ---: | ---: |
   | `map_init` scratch, rows nested | 2.053 s | 39.5 |
   | `map_init` scratch, rows sequential | 1.750 s | 46.3 |
   | thread-local scratch, rows sequential (**main**) | 1.901 s | 42.6 |

   - **The nested `par_iter` is gone**, and that is a real **1.17x**. The rank
     filter and the convolver were rayon-parallel over rows *inside* a rayon job
     per tile — a §6 violation that had been there since the tile loop was
     written — and the inner level was only contending for cores the outer level
     already had. It costs the few-tile case, where the region is smaller than one
     grid step and 16 of 20 cores now idle;
     `RankFilter::with_parallel_rows(true)` is still there for that.
   - **Per-worker scratch is a thread-local, not `map_init`**, and that costs
     **8%**. `map_init` builds its state once per rayon *job*, and rayon makes
     more jobs when there are more items, so peak memory grew with the tile count
     rather than the thread count. At 2 MB a tile nobody noticed; at 145 MB it is
     the difference between bounded and unbounded. The 8% is the price of the
     bound and is paid deliberately.

   **Net, the two together are 1.08x** — not the 1.14x an earlier revision of
   this file claimed from a single-shot pair of runs that also happened to change
   two things at once. Caveat on all three rows: separate processes, not
   interleaved in one, so they do not meet the bar
   `docs/benchmarks/README.md` sets for a component claim. Treat 1.17x and 8% as
   approximate and the ordering as sound.
9. **SIMD**, last, per §0.5 — and only behind a scalar fallback with a
   differential test. Note §4.3 and §4.4 got 2.3x and 3.7x without any explicit
   SIMD, by removing a dependency rather than by widening an instruction.

---

## 5. Standing constraints

Carried forward because they are the observed failure modes in this repository,
not hypothetical ones. The plan-versus-call-site one was hit again, on §2, for
the third stage running.

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
- **Two binaries are not a controlled comparison below about 5%.** Building the
  same source twice and timing the two executables — the alternating-binaries
  protocol used for several changes in §4 — measured `erode_cost` at 0.1250 and
  0.1160 s for **identical semantics**, 7.7% apart from code layout alone. Any
  claim smaller than that made by comparing two builds is measuring the linker.
  Where a change can be expressed as a parameter, vary it *inside* one binary
  and interleave the settings; `BLOCK_THRESHOLD` and its environment override
  in `qupath-filters` are the worked example, and that is how the 0.6% there
  was established after a two-binary run had shown 0.3% with a reversed pair.
  Where it cannot, the honest floor for a two-binary claim is several percent,
  which is a reason to prefer changes large enough to clear it.
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
- **Instruction count is not cost, and this repository has now proved it five
  times.** The reconstruction's index arithmetic: 1.03x. The shape split's
  branchless height, its tree-shaped minimum and its branchless neighbourhood:
  1.02x together. The tracer's 4 M hardware divisions per tile, removed
  entirely: 1.00x. The rank filter's prefix and suffix scans, paired so four
  chains run in flight: 1.00x. Every one of them was an expensive-looking
  instruction on a path the out-of-order engine was already covering, and every
  one cost an afternoon. **What has actually worked, every time, is removing
  work**: a stage that ran twice (§4.6), a memset nothing read — twice now, 22 MB
  a tile in the segmenter and ~150 MB a tile in the tile loop — and a dependency
  that serialised a loop (§4.3, §4.4, §4.7). Ask which of those a change is
  before writing it.
- **A profile share carries the thread count and the region it was taken on.**
  Every share in §4's stage table comes from the 4000x4000 profiled region,
  which is four tiles on a twenty-thread pool — four P-cores of a machine that
  has six P-cores and eight slower E-cores. On a grid that fills the pool the
  erosion is 28% where that table says 10.4%, and the reconstruction 10.8%
  where it says 26.2%. **The largest stage is not the same stage in the two
  columns**, and this file has pointed three optimisation passes at the second
  one. Quote a share with its tile count and its thread count, or do not quote
  it.
- **A speedup can change sign with the thread count, and one did.** Four rows of
  the reconstruction's raster pass in flight at once is 1.21x on the pass in
  isolation, 1.05x in place on four tiles, and **0.83x** on a saturated grid,
  measured with alternating binaries four times over. It computes bit-identical
  output and does not do one instruction more; what it changes is that a tile
  reads nine streams at once instead of three, and twenty tiles of that is a
  different machine from one. **An A/B on a component, however carefully
  interleaved and asserted, does not predict the sign in place.** The bar for a
  speed claim is now the saturated grid as well as the component.
- **The pipeline is already 4.7x faster than QuPath on one thread, and the
  stage table has been read as if it were not.** Same slide, same 4000x4000
  region, same default parameters, `region` against the 15.87 s in
  `docs/profiles/README.md`: **3.359 s on one thread, 1.019 s on twenty.** That
  is 4.7x and 15.6x. The comparison is generous to us — QuPath's 15.87 s
  includes the tail this pipeline has not ported (the measurements) — so the
  honest claim is "decisively faster, by a factor
  this measurement puts at most at 4.7x single-threaded". It is not close, and
  no session needs to open by asking whether we are ahead.
- **Wall clock on the profiled region is flat from six threads, and that is a
  property of the region, not the machine.** On the 4000x4000 region: 1 thread
  3.359 s, 6 threads 1.028 s, 12 threads 1.017 s, 20 threads 1.019 s. Four
  tiles is four units of parallelism, so the pool is exhausted at four and
  everything past it is idle. On the saturated 9000x9000 grid the same sweep
  still improves throughout — 6 threads 1.554 s, 12 1.427 s, 16 1.311 s, 20
  1.203 s, a real **1.29x** from 6 to 20. **Pool tuning is therefore not a
  lever**: on the small region extra threads already cost nothing in wall clock,
  and on the large one they are all earning. The only thing left that moves wall
  clock is per-pixel work on the critical path.
- **§4.8's memory-bandwidth reading is disproved by an SMT control.** Six
  P-cores with SMT off against the same six P-cores with SMT on — identical
  silicon, identical clocks, identical memory system, the only variable being
  sibling threads — on the saturated grid: total stage CPU time **8.526 s to
  14.162 s (1.66x)** to buy wall clock **1.552 s to 1.396 s (1.11x)**. Memory
  bandwidth cannot tell a sibling thread from a distinct core, so it cannot
  produce that; contention for shared execution ports can and does. This is the
  run this file asked for above ("somebody should separate the two causes with a
  run pinned to P-cores only"), and it also explains why the cache-blocking §4.8
  proposed measured 1.7–2.6x **worse** rather than merely failing to help.
- **Most of the erosion's saturated-grid share is contention, not work.** Its
  uncontended floor is one thread on one P-core, where it costs **1.789 CPU-s**
  on the 9000x9000 grid. Six P-cores without SMT: 2.073 s, only **1.16x** — it
  scales nearly perfectly on real cores. Six P-cores *with* SMT: **3.928 s,
  2.20x the floor**, and the eight E-cores land at 2.19x independently. So the
  "28% of the pipeline" that sent two sessions at this stage is roughly half an
  artefact of oversubscribing the machine; on six P-cores it is 24.3%. It fits
  the stage's shape — eleven rectangles of four scalar passes each is pure ALU
  throughput with no stalls for a sibling to fill, so SMT halves the ports and
  returns nothing. **A vectorised prefix-maximum inherits this**: it concentrates
  the same work into denser ports, so it should be expected to be *more*
  SMT-sensitive, and its 3.1x on a component will not survive intact in place.
- **Hand-interleaving a chain costs the autovectoriser, and that can outweigh
  the chain.** Carrying four of the rank filter's block scans in flight — the
  same disjoint-reductions argument that was worth 2.4x on the reconstruction —
  is **20% slower**, and the damage is done before any interleaving: rewriting
  the scan as indexed accesses into an array of four accumulators, then running
  *one* chain through it, is already 12% slower than the `zip` over two slices
  it replaced (.1295 against .1152 s). The slice-zip shape is what the compiler
  was turning into vector code, and taking it away gave back less than it cost.
  Widen a chain only where the loop was not vectorising in the first place —
  the reconstruction's raster pass had a genuine serial dependency across the
  whole row, this one has 33-element blocks the compiler was already handling.
- **A stage that costs more than a strictly larger stage is a stall.** The
  Laplacian's nine taps cost more than the separable twenty-nine-tap blur ahead
  of it, because both had the same loop-carried accumulator — the second one
  written after the first was fixed. The stage table shows this for free; read
  it that way when a stage lands.
- **This file's plan for a stage is not that stage's call site.** §1.5 said for
  two revisions that the object filters "need only per-label sums and counts
  over the label image — no tracing", and that the area filter could be
  estimated on raw basins. Both are wrong: the Wand traces a *threshold range*
  so touching basins merge into one object, and the area filter runs after the
  merge and shape split on objects that do not exist yet. The plan was written
  from a summary of the source; five minutes at the call site contradicted it.
  Every remaining item here was written the same way and deserves the same
  distrust — read the call site again when you pick one up, before writing code
  against what this file says about it.
- **A convention nothing exercises is not pinned, however carefully it is
  written down.** §1.5a's port skipped non-finite pixels in the statistics, the
  comment said so, and the mutation that removed it **passed** — no recorded
  case had such a pixel. The fixture gained a case and was re-recorded. So:
  mutate every convention you claim, and when the mutation passes, the fixture
  is missing a case rather than the convention being unimportant. Two other
  conventions were also caught this way and only one turned out to be genuinely
  unobservable. Note the converse is not a clean bill of
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
- **The unit a stage runs on is part of the stage.** For five items this file
  treated tiling as plumbing — a detail of how pixels reach the ported code,
  outside the parity story. It is not: QuPath detects on a 2048 px grid of its
  own and this pipeline was detecting on the TIFF's 256 px storage grid, so every
  stage ran on a different image from QuPath's and every count was inflated 1.33x.
  Nothing in a per-stage fixture can catch that, because each stage was
  bit-identical on the tile it was given. **Ask what QuPath's stage is called
  with, not only what it computes.**
- **A ratio measured at one size is not a ratio.** §4.7 recorded the background
  stage as 59% erosion / 40% reconstruction and pointed the next optimisation
  pass at the erosion. Measured at the tile size QuPath actually uses it is 12%
  and 86% — the reverse — because the two halves scale differently and the
  figures had been taken on 256 px tiles. A profile share carries the conditions
  it was taken under; record them beside it, and re-measure when they change.
- **A synthetic input can pick the wrong optimisation.** The repeat-fraction
  A/B was written first against a smooth generated surface, where the
  reconstruction settles in 4 raster passes and dropping the repeat loop looked
  like a free 1.47x. On a real tile it takes 14 passes and dropping the loop is
  **2x slower**. The synthetic case was not merely less representative; it
  reversed the sign of the answer. It also mis-sizes a *count*, and more gently:
  the merge and split take 87 candidates to 49 on `MergeAndSplit.json`'s
  realistic case, 1.78x, and 51,152 to 45,990 in place, 1.11x. Neither number is
  wrong. A 96x96 field of random pixels puts nearly every candidate within a
  pixel of a neighbour, and a real tile does not.
- **The objects a cross-tile rule compares are part of the rule.** §2 ported
  `resolveOverlaps` — right grid, right keep-the-larger rule, right 10%
  threshold — against nucleus pixel masks. QuPath runs it on the **cells**,
  because `cellExpansion` defaults to 5 µm and `preferNucleus` is `false`, using
  smoothed polygon geometry. A cell is strictly larger than its nucleus, so the
  port removes fewer duplicates than QuPath does and its object count is an
  over-count by that amount. This is the same shape as "the unit a stage runs on
  is part of the stage" below, one level up: the rule was read carefully and the
  thing it is applied to was not. See D-008.
- **A doc comment written before the recording is a prediction.**
  `RefineBoundary.java`'s comment claimed a flat image convolves to exactly zero,
  so `bp2` would be empty and the case would pin the erosion alone. The interior
  does; the ring one pixel inside the border comes back at ±2.98e-08, rounding
  left over from the blur's tail-sum edge handling, and `bp2` is exactly the
  pixels where that went positive. The recording corrected the comment before the
  port was written, which is the process working — but only because the fixture
  was recorded first and read, rather than recorded and assumed.
- **`map_init` is not per thread.** Rayon calls it once per *job* and splits into
  more jobs when there are more items, so per-worker scratch held under it scales
  with the item count. Invisible while tiles were 2 MB; unbounded once they were
  145 MB.
- **Do not nest `par_iter`, and this repository had for months.** The rank filter
  and the convolver parallelised rows inside a per-tile rayon job. It cost 1.17x
  at saturation, and it made a thread-local scratch unsound — rayon resumes
  another tile's job on the blocked thread, which re-enters whatever the outer
  job was holding. §6 says not to; the reason is both of those.
- **Two changes measured together are not a measurement.** The 1.17x above was
  first reported as "+14%" from one run before the session and one after, with
  the scratch strategy changed in between as well. Re-measured one variable at a
  time it is 1.17x for the nesting and *minus* 8% for the scratch, netting 1.08x.
  The direction happened to survive; the number did not, and nothing guaranteed
  the direction would.
