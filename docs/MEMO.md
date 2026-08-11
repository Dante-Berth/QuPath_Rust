# Memo — read this first

Twenty minutes of orientation before `ROADMAP.md`. It says what this project is
for, what is actually finished, what is actually faster, and what to distrust.

`ROADMAP.md` is the detail and the plan. `DIVERGENCES.md` is every place the port
deliberately differs from QuPath. This file is the view from above, and it is
short on purpose.

---

## 1. What the project is

A Rust port of QuPath's `WatershedCellDetection`, checked against QuPath itself
rather than against a textbook. Two rules follow from that and they order every
decision:

- **Parity first, speed second.** A stage is not ported until a fixture recorded
  from a real QuPath build says its output matches. Speed work happens after,
  and only on stages a profile says are hot.
- **Matching QuPath includes matching its mistakes.** ImageJ's edge handling,
  a reconstruction that never validates its marker, a tiling scheme that costs
  QuPath accuracy — all reproduced deliberately. `DIVERGENCES.md` records each
  one and why. "More correct than QuPath" is a bug here.

The speedup, when it comes, is not expected to come from Rust. `.claude/skills/
ms-rust/SKILL.md` §0 puts it in order: doing less work, data layout, parallel
granularity, avoiding allocation, and only then SIMD. Rust-versus-Java on
equivalent code is 1–3x and that is not the lever.

---

## 2. Where it actually stands

**The detection chain is ported through the shape split, and the tiling around
it.** Deconvolution, background subtraction by reconstruction, Gaussian blur,
Laplacian, regional maxima, labelling, watershed flood, filled-region tracing,
the mean-intensity filter, the tile grid, the overlap resolution, and now the
merge, the Euclidean distance map and `MaximumFinder`'s segmentation. Every one
is pinned by a recorded fixture and most are bit-identical.

**It is not `WatershedCellDetection` yet.** Missing: `refineBoundary`, the area
filter that runs on *its* objects, ROI vertices, and the measurement tail. Until
those land there is no end-to-end claim to make.

**The merge and the split are ported but not wired in**, deliberately: they sit
between the intensity filter and `refineBoundary`, and running them without it
would report a count QuPath never computes. On the fixture's realistic case they
take 87 candidates to 49 objects — a 1.78x reduction, against the 2.6x still
outstanding.

**The count**, on the profiled 4000x4000 region of CMU-1:

| | |
| --- | ---: |
| pipeline candidates | 48,045 |
| QuPath cells | 25,757 |
| gap | **1.87x** |

The gap is now provably inside the merge-and-split and the area filter, because
everything else has been measured and accounted for.

**The time**, same region: 42.6 Mpx/s on a grid large enough to fill the thread
pool. Against QuPath's 15.87 s there is still nothing to compare, because the
two are not computing the same thing yet.

---

## 3. What is truly accelerated

The important section, and the one most easily overstated.

### Genuinely faster, with an interleaved A/B asserting identical output

These meet the bar in `docs/benchmarks/README.md` — both implementations run in
one process, interleaved, minimum of many repetitions, output asserted equal on
every timed iteration.

| What | Gain | Against |
| --- | ---: | --- |
| Columnar measurement scan/filter | ~100x | QuPath's per-object measurement map |
| Watershed priority queue | 20–32x | a binary heap of the same shape |
| Laplacian convolution | 3.69x in place | its own previous version |
| MIN/MAX rank filter | 3.1–9.7x | ImageJ's `RankFilters` |
| Gaussian blur | 2.30x in place | its own previous version |
| Deconvolution | 0.768 s to 0.031 s | its own previous version |

Two of those — the blur and the Laplacian — were the *same defect twice*: a
loop-carried accumulator carrying one output pixel, so the loop ran at the
latency of a multiply-add instead of its throughput. Neither needed SIMD.

### Faster this session

**One thing, worth 1.17x**, and only at saturation: the rank filter and the
convolver were parallelising rows *inside* the per-tile rayon job. Removing that
nesting is 2.053 s to 1.750 s on a 9000x9000 region, best of 5, same 142,957
objects out.

Against that, bounding per-worker scratch to the thread count costs **8%**. Net
on `main`: **1.08x**. Both figures come from separate processes rather than one
interleaved harness, so treat them as approximate.

### Not acceleration, however much it looks like it

- **"30% less work per pixel."** True (0.253 to 0.178 s/Mpx) and *not a
  speedup* — the pipeline is computing a different, correct result. Work that
  went away was work that should never have happened.
- **The headline region got 4.4x slower in wall clock**, 0.215 s to 0.95 s. Real,
  and not a regression: 4000x4000 is smaller than one of QuPath's grid steps, so
  it is 4 tiles across 20 threads where it used to be 272. Quote 42.6 Mpx/s on a
  saturated grid, or quote both with the tile counts attached. Never quote the
  0.215 s as a baseline — it was measured on the wrong tiling.
- **The background stage was measured, not accelerated.** Its two known levers
  were investigated and both came back negative: QuPath's raster-repeat heuristic
  is already optimal (the textbook alternative is 2x slower), and the erosion —
  which the roadmap had blamed — turned out to be 12% of the stage, not 59%.

**Nothing else in this session made anything faster.** The rest was a correctness
fix, two negative results, and two soundness fixes.

---

## 4. How this project goes wrong

The same failure has now happened six times, and it is worth recognising by
shape rather than by instance:

> **A plausible reading of the source, never checked against a run.**

- `EOSIN` held the DAB vector — a triple three lines away in the same file. 52%
  error, smooth plausible output, caught only by a recorded fixture.
- `MaximumFinder` was planned for two revisions. Detection never calls it.
- A morphological opening stood where QuPath does an opening by reconstruction
  and then *subtracts* it. Same family, opposite result.
- The object filters "need only per-label sums, no tracing". They need the
  tracing; the Wand traces a threshold range, so touching basins merge.
- Tiling was assumed to be plumbing. QuPath detects on a 2048 px grid of its own
  and the pipeline was using the TIFF's 256 px storage tiles — every count
  inflated 1.33x, and no per-stage fixture could see it, because each stage was
  bit-identical *on the tile it was given*.
- `MaximumFinder` was written off **twice** as something detection never calls.
  It does: `EDM.toWatershed` reaches it, and it is what splits the nuclei. The
  first reading was right about the maxima stage and wrong about the program.
- The roadmap's own step list said the area filter runs on what the shape split
  leaves. Between them stands `refineBoundary` — a second Gaussian and a second
  Laplacian, on an image the pipeline does not keep — guarded by a hard-coded
  `true` and a `sigma > 1.5` that CMU-1's 3.006 clears. A list of remaining
  stages is not a reading of the source, and this one had a stage missing.

Every one produced entirely reasonable-looking output. The defences that actually
worked, in order: recording a fixture from a real QuPath run, counting objects,
and mutating each claimed convention to check a case fails without it. Looking at
the image never once caught anything.

**So: read the call site before writing code against what `ROADMAP.md` says about
a stage.** Its plans for the stages that remain were written the same way as the
plans that were wrong.

---

## 5. What to distrust right now

- **`ROADMAP.md`'s remaining plans** — §1.5b steps 2b to 5. Written from a
  summary of the source, same as the four that turned out wrong. Step 2 has
  since been ported and its entry was accurate; the list it sat in was still
  missing a whole stage.
- **The merge and split are unwired**, so nothing measures them end to end yet.
  Their fixtures are strong — four checkpoints, pixel-exact, five of six
  mutations failing — but a stage that no pipeline run exercises has only ever
  been checked on 13 small images.
- **`resolve_overlaps` has no QuPath-recorded fixture.** Its eight tests use
  hand-built candidates. Given the record above, that is the weakest thing in the
  tree.
- **The 1.17x and the 8%** — separate processes, not interleaved. Directions are
  sound, magnitudes are approximate.
- **Peak memory is ~3.3 GB** at 20 threads on a saturated grid, bounded by
  threads x tile size and no longer growing with the region. That is a
  consequence of QuPath's 2048 px tiles and it is 45x what the pipeline used
  before. Bounded, documented, and larger than anyone would guess.

---

## 6. Where the next gain actually is

Measured, not assumed:

1. **The pipeline is memory-bandwidth-bound**, and this is new. Parallel
   efficiency runs 96% at 2 threads and 61% at 20; going 12 to 20 threads buys
   3% for 30% more memory. A tile's working set went from ~2 MB (L2-resident) to
   ~145 MB (streaming from DRAM) when the tiling was corrected. This is the
   biggest lever and it did not exist a week ago.
2. **The reconstruction raster pass**, 46% of stage time, is still a
   transliteration of QuPath's index arithmetic — `isize` strides, a runtime
   `Option` tested per pixel, first-row branches inside the inner loop. ~14
   cycles per pixel for a max-of-five-and-clamp. `RegionalMaxima.json` will
   catch it if a rewrite changes an answer.
3. **Doing less work** — still untouched, still the largest lever in the
   guidelines' own ordering. `Slide::level_for_width` exists and nothing uses it;
   the profiled region is 99% tissue but a whole slide is mostly glass.

---

## 7. Reading order

1. This file.
2. `.claude/skills/ms-rust/SKILL.md` — the rules, especially §0 and §10.
3. `docs/ROADMAP.md` §5, the standing constraints — the accumulated scar tissue.
4. `docs/ROADMAP.md` from the top, for the stage you are picking up.
5. `DIVERGENCES.md` for that stage.
6. **The QuPath call site itself**, in `qupath/`, before writing anything.

Reproduce the state of play:

```bash
cargo test --workspace
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo run --release -p qupath-pipeline --example region
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo run --release -p qupath-pipeline --example tile_seams
```
