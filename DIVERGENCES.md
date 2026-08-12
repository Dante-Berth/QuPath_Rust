# Divergences from QuPath

Every intentional behavioural difference from QuPath, with its reason. The
target is matching QuPath, not matching the textbook.

## D-001 — `DoubleList` stores only `f32` precision

**QuPath:** `NumericMeasurementList.DoubleList` is backed by a `double[]`, but
its `setValue` writes `values[index] = (float)value`
(`qupath-core/src/main/java/qupath/lib/measurements/NumericMeasurementList.java:350`).
Every value stored through the `DOUBLE` measurement list is therefore rounded to
`f32` precision, while occupying 8 bytes. This looks like a copy-paste from the
neighbouring `FloatList` rather than a deliberate choice.

**Here:** `qupath_objects::Table` stores `f32` columns.

**Measured**, against `qupath-core-0.8.0-SNAPSHOT` on JDK 25, storing
`0.1234567890123456789`:

| List type | Value read back | Equals input | Equals `(float)` input |
| --- | --- | --- | --- |
| `GENERAL` | `0.12345678901234568` | yes | no |
| `DOUBLE` | `0.12345679104328156` | no | **yes** |
| `FLOAT` | `0.12345679104328156` | no | **yes** |

So `DOUBLE` and `FLOAT` are bit-identical in QuPath, and `f32` columns here
reproduce both exactly. `GENERAL` — which boxes `Double` objects — is the one
type that retains full `f64` precision.

**Consequence:** `PathObject` defaults to `GENERAL`
(`qupath-core/src/main/java/qupath/lib/objects/PathObject.java:223`), so
measurements set on a default `PathObject` and read straight back can differ
from our columns in the bits below `f32`. This is bounded by `f32` epsilon
(~1.2e-7 relative) and sits well inside the 1e-5 relative tolerance used for
scalar parity, but it must be accounted for in any test that claims bit
equality. Values that have made a round trip through QuPath's `DOUBLE`/`FLOAT`
lists, or through a saved `.qpdata`, match exactly.

**Status:** matches QuPath's observable behaviour for `DOUBLE`/`FLOAT`. If
QuPath fixes the truncation upstream, this entry must be revisited — parity
would then require `f64` columns, and the decision should be re-made against a
profile rather than assumed.

## D-003 — Watershed seeds every border pixel, following a QuPath NaN quirk

**QuPath:** `Watershed`'s queue constructor decides whether an unlabelled pixel
starts on the flood front by testing its four orthogonal neighbours with
`ipLabels.getPixelValue(...) != 0f`
(`qupath-core-processing/src/main/java/qupath/imagej/processing/Watershed.java:228-233`).
`ImageProcessor.getPixelValue` returns **`NaN`** outside a `FloatProcessor`, and
`NaN != 0f` is true, so every pixel on the image border is seeded as though it
were adjacent to a label — whether or not any label is nearby.

**Measured**, against `qupath-core-processing` at `e7662d8` on JDK 25, flooding
a flat 6×4 plateau from markers at the two top corners:

```text
labels in           QuPath out          if OOB were treated as 0
1 0 0 0 0 2         1 1 1 1 0 2         1 1 1 0 2 2
0 0 0 0 0 0         1 1 1 1 0 2         1 1 1 0 2 2
0 0 0 0 0 0         1 1 1 1 0 2         1 1 1 0 2 2
0 0 0 0 0 0         1 1 1 1 1 0         1 1 1 0 2 2
```

The right-hand result is what a symmetric reading of the algorithm produces. The
middle is what QuPath actually does: the watershed line shifts a column and the
bottom-right corner is left unlabelled.

**Here:** `has_labelled_orthogonal_neighbour` returns `true` for out-of-bounds
neighbours, reproducing the middle column exactly.

**Consequence:** on interior regions — the overwhelmingly common case, since
detection runs on tiles whose objects are away from the edge — this is
unobservable. It matters only for markers within one pixel of the image border,
where it changes which side of a plateau a boundary falls on. Pinned by the
`plateau_four` and `plateau_eight` cases in `parity/fixtures/WatershedFlood.json`.

**Status:** deliberate bug-for-bug parity. The target is matching QuPath, not
matching the textbook. If QuPath ever bounds-checks the seeding, this and the
fixture must both be revisited.

## D-002 — Measurement names are interned per table, not globally

**QuPath:** each `MeasurementList` owns its own name list, deduplicated across
objects through a global `namesPool` keyed on the whole name list, plus a
per-list `Map<String,Integer>` for lookup.

**Here:** names are interned once per `Table` and resolved to a `FeatureId`
index. There is no global pool and no per-object map.

**Consequence:** feature lookup by name is not available in an inner loop by
design — callers resolve a `FeatureId` once, outside the loop. Observable
values are unchanged.

## D-004 — Slide pixels match QuPath's decode to within one channel step

**QuPath:** reads SVS through Bio-Formats, which decodes the tiles' baseline
JPEG with its own IDCT.

**Here:** `qupath_slide` decodes the same tiles with `zune-jpeg`.

**Measured**, against `qupath-extension-bioformats` on JDK 25, over the five
regions in `parity/fixtures/SlideRegion.json`:

| Case | Level | Worst channel difference | Mean |
| --- | ---: | ---: | ---: |
| `tile_interior` | 0 | 0 | 0.000 |
| `tile_seam` | 0 | 0 | 0.000 |
| `tissue` | 0 | **1** | 0.009 |
| `level_1` | 1 | 0 | 0.000 |
| `level_2` | 2 | 0 | 0.000 |

**Consequence:** four of the five cases are bit-identical, and the fifth differs
by one step on a small number of channels. JPEG fixes the inverse DCT only to a
stated accuracy, not bit for bit, so two conforming decoders are permitted to
differ here; requiring exact equality would be requiring one specific IDCT
implementation. The parity test tolerates 4 and asserts the mean stays under
0.75, which no rounding difference can breach but a systematic colour error
would — and did, before it was fixed.

**That colour error is worth recording**, because it is the failure mode this
fixture exists to catch. Aperio sets `PhotometricInterpretation = 2` (RGB) and
stores JPEG components that are already R, G and B, but writes no Adobe APP14
marker saying so. A decoder following the JFIF convention treats three
components as YCbCr and converts, turning a white (243,243,243) pixel into
(255,121,255). The output still looks like a plausible H&E slide. Reading the
TIFF tag rather than trusting the JPEG's markers is what fixes it.

**Status:** matches QuPath within documented tolerance. Geometry — level sizes,
level count, tile placement — is asserted exactly, with no tolerance.

## D-005 — Detection runs at the slide's own pixel size, not QuPath's 0.5 µm

**QuPath:** `WatershedCellDetection` does not analyse the pixels it is given. It
resolves `requestedPixelSizeMicrons` (default 0.5) against the slide's own pixel
size, takes the larger of the two, and requests the region at that downsample
(`getPreferredPixelSizeMicrons` →
`ServerTools.getDownsampleFactor`,
`qupath-core-processing/src/main/java/qupath/imagej/detect/cells/WatershedCellDetection.java:402-431`).
It then converts every micron-valued parameter against the **resampled** pixel
size.

On CMU-1, whose `MPP` is 0.4990, that is a downsample of 1.002 — an interpolated
resample by a factor just above one — after which the effective pixel size is
exactly 0.5 µm and QuPath's parameters come out as round numbers:

| Parameter | QuPath, at 0.5 µm/px | Here, at 0.4990 µm/px |
| --- | ---: | ---: |
| Background radius | 16.00 px | 16.03 px |
| Sigma | 3.000 px | 3.006 px |
| Minimum area | 40.0 px² | 40.2 px² |
| Maximum area | 1600 px² | 1606.4 px² |

**Here:** `qupath-pipeline` reads level 0 as it is and converts the same micron
values against the pixel size it actually has. The parameters differ in the
third significant figure, and the pixels themselves are not interpolated.

**Consequence:** unquantified, and it is the one entry in this file that is not
yet measured. Both effects are small — a 0.2% difference in every radius, and a
resample by 1.002 — but they are not zero, and they fall on a stage whose output
feeds a maximum test, which is where small differences have previously turned
into different object counts. It must be quantified before §1.6 compares counts,
because a count that differs by a few per cent cannot be attributed until this
is ruled out.

**Status:** deliberate, and provisional. Matching QuPath exactly would mean
porting its resample, which is an ImageJ `ImageProcessor.resize` with its own
interpolation convention and would need its own fixture. Reading the slide at
its native resolution is the more defensible default for this pipeline —
resampling by 1.002 destroys information for nothing — but "more defensible" is
not "the same", and the target here is the same.

## D-006 — Reconstruction does not validate that the marker lies under the mask

**QuPath:** `MorphologicalReconstruction.morphologicalReconstruction(marker,
mask)` only ever *raises* the marker. It has a `validateMarkerMask` that would
clamp a marker lying above its mask (`copyBits(..., Blitter.MIN)`) and **does
not call it**. A marker pixel above its mask value is therefore left exactly
where it is, and the result is not a reconstruction of anything.

**Measured**, against `qupath-core-processing` on JDK 25: a uniform marker of
5.0 under a ramp mask spanning -0.5 to 0.15 comes back as 5.0 everywhere, in
`parity/fixtures/RegionalMaxima.json`'s `marker-above-mask` case.

**Here:** reproduced exactly. `Reconstructor::reconstruct` raises and never
lowers, and says so in its documentation.

**Consequence:** none for either caller, and both satisfy the precondition
structurally rather than by checking it — the regional-maximum transform starts
from a copy of the mask with holes punched in it, and the opening by
reconstruction starts from an erosion of the mask. A third caller would have to
establish it for itself.

**Status:** deliberate bug-for-bug parity. Adding the clamp would be correct in
general and wrong here.

## D-007 — Tiled runs do not reproduce a seamless run, and neither does QuPath's

**QuPath:** `AbstractTileableDetectionPlugin` breaks a region into ~2048 px
tiles, grows each by `getTileOverlap` (60 px on CMU-1) on all four sides, detects
independently in each, and then deletes whichever of two overlapping detections
is smaller when the intersection exceeds 10% of it
(`ParallelTileObject.resolveOverlaps`).

**Here:** reproduced, in `qupath-pipeline`'s `tiles` and `overlaps` modules —
same grid arithmetic, same 60 px halo, same keep-the-larger rule at the same
strict 10% threshold.

**The divergence is not against QuPath — it is that neither implementation's
tiled result equals its own untiled one.** Two stages of the chain are
morphological reconstructions, and a geodesic dilation propagates without bound,
so a tile boundary changes pixel values arbitrarily far from it. No finite halo
can fix that; it is a property of the algorithm QuPath chose.

**Measured** on the profiled 4000x4000 region, changing only the grid:

| Grid | Candidates |
| --- | ---: |
| storage-sized, 256 px, no halo | 62,102 |
| QuPath's, 60 px halo, overlaps resolved | 48,045 |
| one tile, no seam | 46,772 |

So QuPath's own grid sits **2.7% above** a seamless run of the same code, and a
storage-sized grid sat 33% above it.

**Consequence:** the §1.6 comparison must be run at the same region size on both
sides, because the tile count is part of the answer. A Dice or IoU comparison
against a QuPath run on a *differently sized* region is not measuring the port.

**Status:** deliberate. Matching QuPath means matching its seams, including the
ones that cost it accuracy. The seamless figure is recorded here as the
reference, not as the target.

---

## D-008 — Seam overlaps are resolved on nuclei, where QuPath resolves them on cells

**QuPath:** `ParallelTileObject.resolveOverlaps` runs after the whole detection,
on the `PathObject`s a tile produced. Two things about those objects matter and
neither is visible from the rule itself:

- **They are cells, not nuclei.** `cellExpansion` defaults to 5 µm, so each
  detection is a cell object with its nucleus inside it, and
  `PathObjectTools.getROI(pathObject, false)` — `preferNucleus` is `false` —
  returns the *cell* boundary. The 10% test is therefore an overlap between two
  expanded regions.
- **Their boundaries are smoothed.** `smoothBoundaries` defaults to `true`, so
  each polygon has been interpolated, smoothed, interpolated again and
  simplified before `geometry.getArea()` is taken. That area is not the enclosed
  pixel count.

**Here:** the overlap is resolved on the nucleus masks, comparing filled pixel
counts. Same grid, same keep-the-larger rule, same strict 10% threshold, on
different objects.

**Why it is here:** cell expansion is not ported, and neither is the vertex list
that smoothing operates on. Nothing in `overlaps.rs` can be made to match until
both are, and stubbing either would be §10's plausible wrong value.

**Direction of the error, and it is not neutral.** A cell is strictly larger than
its nucleus, so two copies of the same object across a 60 px band intersect more
readily as cells than as nuclei. QuPath therefore removes **at least** as many
duplicates as this does, and this pipeline's object count is an over-count by
whatever that difference is. Measured on the profiled region: 1,712 of 28,359
nuclei are resolved away here, 6.0%, against a residual gap to QuPath of 890
objects — so the two are the same order of magnitude and this is the first place
to look for that gap, not the last.

**Status:** known, quantified in direction, unfixable before cell expansion and
ROI vertices land. `docs/ROADMAP.md` §1.5b steps 4 and 5.
