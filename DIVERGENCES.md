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
