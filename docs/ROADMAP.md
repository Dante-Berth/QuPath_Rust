# Roadmap

Where the port stands, what comes next, and what each next thing needs before
it can claim anything. Written to be picked up cold.

**Start with `docs/MEMO.md`** if you have not read it — twenty minutes of
orientation, including an honest accounting of what is genuinely faster and what
only looks it. This file is the detail underneath that.

## State of play

Five crates, all five now with parity fixtures recorded from a real QuPath
build.

| Crate | Does | Parity | Measured |
| --- | --- | --- | --- |
| `qupath-objects` | Columnar detection/measurement table | `MeasurementPrecision.json` | ~100x on scan/filter |
| `qupath-watershed` | Marker-controlled flood, bucket queue, 4/8-connected labelling, filled-region tracing | `WatershedFlood.json`, 3 cases; `RegionalMaxima.json` labels, 13 surfaces; `ObjectFilters.json` regions, 17 images | queue 20-32x vs heap; flood 1.40x vs previous; labelling and tracing identical |
| `qupath-filters` | MIN/MAX rank filter, Gaussian blur, convolution, morphological reconstruction | `RankFilterMinMax.json` 42 cases; `GaussianBlur.json` 10 kernels + 12 images; `LaplacianConvolution.json` 5 scales + 17 images; `RegionalMaxima.json` 11 reconstructions + 13 surfaces | rank 3.1-9.7x vs ImageJ; blur, convolution and reconstruction bit-identical |
| `qupath-slide` | Aperio SVS / tiled TIFF reader | `SlideRegion.json`, 5 regions | 4000x4000 region in 0.09 s |
| `qupath-pipeline` | Tiles a region QuPath's way, streams each tile through the above, filters and de-duplicates the candidates | `ColourDeconvolution*.json`; `ObjectFilters.json` statistics and decisions, 17 images | 42.6 Mpx/s on a saturated grid; 48,045 candidates against QuPath's 25,757 cells |

**The end-to-end number is still QuPath's 15.87 s**, because the pipeline is not
`WatershedCellDetection`. Every image-processing stage QuPath runs *before* its
watershed is ported and pinned, and so are the first two things it does *after*
— tracing basins to filled regions, and the intensity filter over them — and, as
of §2, so is the tile grid all of it runs on. What remains is the merge and
shape split, the area filter that runs on the objects *that* produces, and the
measurement tail. Do not compare the two figures until §1.5 below is done; the
pipeline currently produces **48,045 candidates** where QuPath detects 25,757
cells.

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
QuPath's grid, against QuPath's 25,757 cells.

**Nor is the throughput figure.** It has moved 0.215 s -> 0.95 s on the profiled
4000x4000 region, and that is not a regression: the region is smaller than one
of QuPath's grid steps, so it is now 4 tiles on 20 threads where it used to be
272. Per pixel the work fell 30% (0.253 to 0.178 s/Mpx summed across threads).
On a region large enough to fill the pool the pipeline does **42.6 Mpx/s**.
Quote that one, or quote both with the tile counts attached.

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

Cost in place: tracing 14.0% of pipeline stage time, the filter 1.1%. The §4.4
ratio check does not fire — tracing does about 1.1 image-equivalents of work
(6.1 M labelled pixels flooded, plus 13.7 M bounding-box pixels for the hole
test against a 17.8 M image), which is comparable to the watershed beside it at
14.3%.

### 1.5b What is left, and what it has to be worth

The intensity filter dropped 24.5% — 88,423 candidates to **66,739** — which is
within one object of the 66,740 the earlier estimate predicted for it. So that
estimate was sound, and the conclusion it pointed at stands: **this was not the
count fix either.** The gap to QuPath's 25,757 is still 2.6x, and it is now
provably in what remains:

| Stage | Effect on the profiled region |
| --- | --- |
| `getFilledPolygonROIs`, holes filled | 88,424 markers -> 88,423 regions, **measured** |
| mean intensity `> 0.1` on `ipToMeasure` | 66,739 survive, 24.5% dropped, **measured** |
| `bp.filter(MAX)`, AND with the thresholded LoG | merges adjacent candidates; not measured |
| `watershedPostProcess`: EDM watershed, splits by shape | splits some back; not measured |
| area outside 10–400 µm², *on the objects that produces* | not measurable before the two above |
| tile seams (§2) | 66,739 -> **48,045**, 28% dropped, **measured** |
| QuPath | **25,757** |

Order to port in, cheapest evidence first:

1. ~~**Tile seams (§2)**~~ — **done**, and it was worth 1.33x of the gap. See §2.
2. **Merge and shape split**, which needs a Euclidean distance map and a second
   watershed. Both halves change object identity, not only count, so this is
   where a Dice or IoU comparison first becomes meaningful. **Read the call site
   before writing anything**: this list has now been wrong about three stages in
   a row, and this entry was written from the same summary that produced the
   §1.5 and §2 errors.
3. **The area filter**, which is a histogram pass over whatever step 2 leaves —
   trivial once it has objects to run on, and meaningless before.
4. **ROI vertices.** The masks match now; the polygons do not exist. QuPath's
   `PathObject` geometry is the Wand's vertex list, so exporting objects needs
   the contour itself and not just what it encloses. Note §2 already needs a
   pixel mask per seam candidate, so the two overlap.
5. **Measurements**, into the columnar table `qupath-objects` already provides.

A fixture for each, recorded the same way as everything above.

### 1.6 The claim

Only once 1.1–1.5 are done: run both implementations on the same region and
compare **cell counts first, then time**. A run that is fast and finds a
different number of cells has not been ported. Success is Dice or IoU >= 0.99
on the object masks per §5, and a cell count within a documented margin.

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

Measured on the profiled region, changing only the grid:

| Grid | Tiles | Markers | Regions | Duplicates | Candidates |
| --- | ---: | ---: | ---: | ---: | ---: |
| storage-sized, 240 px, no halo | 289 | 79,399 | 79,398 | 0 | 62,102 |
| **QuPath's, 2052/3078, 60 px halo** | 4 | 80,763 | 80,762 | 3,107 | **48,045** |
| one tile, no seam | 1 | 75,736 | 75,735 | 0 | 46,772 |

Reproduce with `cargo run --release -p qupath-pipeline --example tile_seams`.

Three things worth keeping from this:

- **A halo cannot make tiling exact, and is not meant to.** Both reconstructions
  in this pipeline are geodesic dilations, which propagate without bound, so no
  finite halo reproduces a single-tile run. QuPath has the same property: its own
  4-tile run lands at 48,045 against a seamless 46,772, a 2.7% difference that is
  inherent to tiling rather than a defect. **The target is matching QuPath's
  grid, not choosing a better one.**
- **The count fell 28%**, 66,739 to 48,045, and the gap to QuPath's 25,757 went
  from 2.6x to **1.87x**. This is the first stage in three that removed anything
  like what a plan predicted — and it is the one whose prediction nobody wrote
  down, because the item was filed as a correctness bug rather than a count fix.
- **The intensity filter now drops 36.7% where it dropped 24.5%.** Not because
  the filter changed but because the background estimate did: an opening by
  reconstruction on a 256 px tile is a different image from one on a 2000 px
  tile, and the smaller one was wrong.

Still open here: an object placed exactly on a boundary is covered by
`overlaps::tests`, on hand-built candidates rather than through the pipeline. A
fixture recorded from QuPath's own `resolveOverlaps` would be stronger and does
not exist.

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
5. **The watershed is now about 14% of pipeline stage time** — down from 40%
   only because §1.4b and §1.5a added stages around it, not because it got
   faster. In absolute terms it is unchanged.
6. **Region tracing landed at 14.0%**, level with the watershed beside it. The
   §4.4 ratio check does **not** fire: it floods 6.1 M labelled pixels
   four-connected and then walks 13.7 M bounding-box pixels for the hole test,
   against a 17.8 M image, so it does about 1.1 image-equivalents of scattered
   work and costs what that implies. It has had no optimisation pass, and the
   bounding-box half is the place to start if one is ever wanted — that figure
   is the sum over regions, so it grows with the number of candidates rather
   than with the image.
7. **The background stage is 59% of pipeline stage time** and is the largest
   single lever. **The 59% erosion / 40% reconstruction split this file used to
   record was measured on 256 px tiles and does not survive §2.** At the tile
   size QuPath actually uses it is:

   | | 256 px tiles (old) | 2120 px tiles (measured) |
   | --- | ---: | ---: |
   | erosion | 59% | **12%** |
   | reconstruction | 40% | **86%** |

   So the erosion — the half this file pointed the next optimisation pass at —
   is not the problem, and its rectangle decomposition holds up at radius 16
   after all. Reproduce with
   `cargo run --release -p qupath-pipeline --example reconstruct_cost`.

   The reconstruction is 46.5% of pipeline stage time on its own, and the
   regional maxima add another 6.5% of the same code. Two things have now been
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

   What is left, and has *not* been tried: the raster pass is a transliteration
   of QuPath's index arithmetic — `isize` strides, a runtime `Option` for the
   queue checked per pixel, and `first_row`/`first_column` branches inside the
   inner loop. At 14 cycles per pixel for a max-of-five-and-clamp it is doing
   several times the work the arithmetic implies. Specialising forward and
   backward, and the queue and no-queue cases, is the obvious next pass, and
   `RegionalMaxima.json` is there to catch it if it changes an answer.

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
  reversed the sign of the answer.
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
