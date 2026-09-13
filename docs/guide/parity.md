# Parity and what to trust

**This is a partial port and its agreement with QuPath is incomplete.** This
page says exactly where it agrees, where it does not, and how that was
measured. Read it before using the output for anything that matters.

## The headline

Over ten regions of the CMU-1 reference slide, chosen by the slide rather than
by hand:

| | |
| --- | ---: |
| This pipeline, objects | 156,979 |
| QuPath, cells | 147,351 |
| Ratio | **1.065x** |
| Range across regions | **1.012 – 1.129** |

**The direction is always the same: this over-counts.** Never under.

A single number was quoted for several sessions — 1.035x on the profiled
region — and it was the wrong number to quote. It is still exactly right, and
it is the *best of ten*. Regions picked by eye put four of the first six on
glass, where there is nothing to disagree about.

## The nuclei that do correspond agree well

A count cannot tell a match from a coincidence, so the objects themselves were
compared against the mask QuPath recorded for every nucleus it detected, over
three single-tile regions:

| | |
| --- | ---: |
| Pooled Dice, by region | **0.935, 0.934, 0.880** |
| QuPath nuclei matched one-to-one at Dice ≥ 0.5 | **97.9%** of 17,079 |
| Mean Dice over matched pairs | **0.921** |

Read the pooled Dice first — it involves no matching decision at all.

Both sides are the pixels each pipeline actually labelled. Comparing QuPath's
mask against a rasterisation of our polygon would have measured our rasteriser
instead.

**So the gap is a population of extra objects on sparse tissue, not boundaries
falling in the wrong place.**

## What the gap tracks

Not seams. **Density, inversely.**

| Region | cells/Mpx | ratio | tiles |
| --- | ---: | ---: | ---: |
| single-tile-no-seams-2 | 1741 | 1.0177 | 1 |
| profiled-99pc-tissue | 1688 | 1.0346 | 4 |
| single-tile-no-seams | 1597 | 1.0121 | 1 |
| dense-2 | 1542 | 1.0337 | 4 |
| dense-5 | 1135 | 1.0817 | 4 |
| dense-4 | 1066 | 1.1293 | 4 |
| dense-7 | 1038 | 1.1041 | 4 |
| dense-3 | 1007 | 1.0624 | 4 |
| single-tile-no-seams-3 | 931 | 1.1130 | 1 |

Dense regions agree to within 1–3.5%; sparse ones diverge by 6–13%. The seam
count does not sort that table — density does. Three of the regions are a
**single tile**, with no seams and no duplicates removed at all, and they still
disagree.

**Practical consequence: the sparser your tissue, the more this over-counts.**

## How far the cause has been narrowed

Seven of the ten pipeline stages are **ruled out against QuPath on real tissue,
pixel for pixel**: the merge, the shape split, the colour deconvolution, the
JPEG decode beneath it, the labelling, the watershed flood, and the background
estimate.

Walking a ladder of stage boundaries splits the gap into two separate effects:

| Rung | sparse | dense-2 | dense |
| --- | ---: | ---: | ---: |
| Regional maxima | **1.044** | **1.047** | **1.051** |
| Candidates (after the flood) | **1.044** | **1.047** | **1.051** |
| After the mean-intensity filter | **1.149** | **1.049** | **1.040** |
| After the merge | **1.139** | **1.043** | **1.028** |

- The first rung is **flat across density**, so that ~4.7% is
  density-*independent* and is not what the headline gap tracks.
- The maxima and candidates rungs are **identical**, so the labelling and the
  flood add nothing.
- The density split appears at the **mean-intensity filter** — and that filter
  is itself pinned against QuPath on 17 recorded images. So what differs is not
  the filter but what it measures.

What remains between the deconvolution and the first rung: the **blur, the
Laplacian, and the regional maxima**. Tracked as **D-009** in
[`DIVERGENCES.md`](../../DIVERGENCES.md).

Each narrowing has cost a hypothesis rather than confirmed one. The merge and
the split, where the ratios *move*, were the leading suspects and have been
cleared: given the same input mask this port reproduces QuPath's merged mask,
its segmentation and its split pixel for pixel on all eight recorded crops.
They are where the gap becomes visible, not where it is made.

## Deliberate differences

These are choices, not defects. Each has an entry in
[`DIVERGENCES.md`](../../DIVERGENCES.md):

| | |
| --- | --- |
| **D-001** | Measurements store `f32` precision. |
| **D-002** | Measurement names interned per table, not globally. |
| **D-003** | The watershed seeds every border pixel, following a QuPath NaN quirk. |
| **D-004** | Slide pixels match QuPath's decode to within one channel step. |
| **D-005** | Detection runs at the slide's own pixel size, not QuPath's resampled 0.5 µm. |
| **D-006** | Reconstruction does not validate that the marker lies under the mask — as QuPath's does not. |
| **D-007** | Tiled runs do not reproduce a seamless run; neither does QuPath's. |
| **D-008** | Seam overlaps are resolved on nuclei, where QuPath resolves them on cells. |
| **D-009** | **The open count gap.** |
| **D-010** | The project reader stops at 255 levels of nesting, where Gson stops at 255 too. |

**Matching QuPath includes matching its mistakes.** ImageJ's edge handling, a
reconstruction that never validates its marker, a tiling scheme that costs
QuPath accuracy — all reproduced deliberately. "More correct than QuPath" is a
bug here, because the point is a port you can compare against, not an
improvement you cannot.

## What is not implemented at all

| | |
| --- | --- |
| **Measurements** | The tail of the pipeline. Does not change object counts. |
| **Cell expansion** | Ported in `qupath-watershed`, not called by the pipeline. |
| **Boundary smoothing** | Ported, not called. |
| **Median filter** | Not implemented. |

The consequence for output: **nuclei with raw pixel boundaries, not smoothed
cells**. The counts on this page compare our raw nuclei against QuPath's
smoothed cells.

**The viewer claims no parity and no speedup.** QuPath's viewer is not what is
being ported.

## Speed, and why to read it second

On the 4000×4000 region QuPath was profiled on, same slide, same parameters:

| | wall clock | vs QuPath |
| --- | ---: | ---: |
| QuPath, 1 thread | 15.87 s | — |
| This pipeline, 1 thread | **3.359 s** | **4.7x** |
| This pipeline, 20 threads | **1.019 s** | **15.6x** |

Read the **single-threaded row** when the question is whether the port is
faster: QuPath's profile was taken single-threaded, and QuPath parallelises
across tiles in real use.

And read both rows as **generous to this port**: QuPath's 15.87 s includes
computing the measurements this pipeline does not implement. The gap is
decisive either way; its exact size is not yet measurable.

A 6.5% density-dependent over-count is not a rounding difference. **What is
fast here is not yet what QuPath computes.**

The speedup is not from Rust. Rust-versus-Java on equivalent code is 1–3x. It
is from the columnar layout: detections are a struct of arrays, one contiguous
column per feature, rather than QuPath's per-object string-keyed measurement
map.

## How parity is checked

Not by reading QuPath's source and believing it. **Recorders run inside a live
QuPath build** and capture its intermediate state at each stage boundary; those
fixtures are committed; Rust tests feed this port the *same* input at the same
boundary and compare pixel for pixel.

Every fixture is **mutation-checked** — no test is trusted until it has been
shown able to fail. [`CONTRIBUTING.md`](../../CONTRIBUTING.md) requires both a
fixture and a mutation check for any new parity claim.

That rule exists because three defects passed visual inspection, each producing
entirely plausible images:

- a morphological **opening** substituted for a **top hat**;
- the **DAB** stain vector used under the name **EOSIN** — a 52% error on the
  leading coefficient;
- Aperio JPEG tiles carrying no Adobe **APP14** marker, so a conforming decoder
  applies the wrong colour transform.

None would have been caught by comparing counts, and none was caught by looking.

## Reproducing the numbers

```bash
./parity/record-cell-counts.sh
cargo run --release --example cell_counts   -p qupath-pipeline   # the ten regions
cargo run --release --example object_masks  -p qupath-pipeline   # the Dice figures
cargo run --release --example stage_ladder  -p qupath-pipeline   # the rungs
```

Recording fixtures needs a QuPath build; running the comparisons does not.
