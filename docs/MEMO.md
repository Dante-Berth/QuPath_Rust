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

**The detection chain is closed.** Every stage `WatershedCellDetection` runs
from the colour deconvolution to the objects it goes on to measure is ported,
pinned by a fixture recorded from a real QuPath build, and running in the
pipeline on QuPath's own tile grid: deconvolution, background subtraction by
reconstruction, Gaussian blur, Laplacian, regional maxima, labelling, watershed
flood, filled-region tracing, the mean-intensity filter, the tile grid and its
overlap resolution, the merge, the Euclidean distance map, `MaximumFinder`'s
segmentation, `refineBoundary`, and the area filter.

**What is missing is the tail**, and none of it changes how many objects there
are: ROI vertices, cell expansion, the measurements.

**The count**, on the profiled 4000x4000 region of CMU-1:

| | |
| --- | ---: |
| pipeline objects | 26,647 |
| QuPath cells | 25,757 |
| gap | **1.035x** |

From 1.86x, and every step of the way there was measured against the step before
it rather than predicted. **Do not read 1.035x as clean.** `DIVERGENCES.md`
D-008: QuPath resolves its seam overlaps on *cells*, using smoothed polygon
geometry, where this compares nucleus pixel masks. A cell is larger than its
nucleus, so QuPath removes at least as many duplicates as this does — and 1,712
were removed here against a residual gap of 890. Same order of magnitude, so
that is the first place to look, not the last.

**The time**: 70.1 Mpx/s on a 9000x9000 region, 25 tiles on 20 threads — a grid
large enough to fill the pool. That is **down** from 80.4 and it is not a
regression: four stages that did not exist are now in the path. Nothing got
slower and nothing got faster this session. Do not compare either figure against
the 42.6 Mpx/s this file used to quote: that was a different region with more
tissue in it, and only the ratio within one region means anything.

Against QuPath's 15.87 s there is still nothing to compare, because QuPath
spends part of it on the tail this pipeline does not have.

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
| Reconstruction raster pass | 2.42x | its own previous version |
| MIN/MAX rank filter | 3.1–9.7x | ImageJ's `RankFilters` |
| Gaussian blur | 2.30x in place | its own previous version |
| Deconvolution | 0.768 s to 0.031 s | its own previous version |

**Three of those are the same defect**, and it is now the pattern to check for
first. The blur and the Laplacian both carried one output pixel in a loop-carried
accumulator, so the loop ran at the latency of a multiply-add rather than its
throughput. The reconstruction carried one pixel's result into the next pixel's
input, which is a real data dependency rather than an accidental one — but it was
three comparisons long where the algebra needs two, and shortening it was worth
as much as widening the other two was. None of the three needed SIMD.

### Faster this session

**Nothing.** This session closed the detection chain — `refineBoundary`, and the
wiring of the merge, the shape split and the area filter into the pipeline — and
made no attempt to make anything faster. The throughput figure moved 80.4 to
70.1 Mpx/s because four stages that did not exist are now in the path. Read that
as the pipeline doing more, not as it doing worse; nothing that was there before
changed.

### Faster in the session before this one

**The reconstruction's raster pass, 2.42x**, which was the first end-to-end wall
clock gain this project had: **1.19x** on a 9000x9000 region at 25 tiles on
20 threads, 66.7 to 80.4 Mpx/s, with 82,997 objects out either way.

It is worth reading §4 of this file before the number. The roadmap had said for
two revisions why that pass was slow — index arithmetic, a per-pixel `Option`,
edge branches in the inner loop — and **all of it together is worth 1.03x**. The
cost was one loop-carried dependency, and the measurement that found it took ten
minutes: feed the loop a constant instead of its left neighbour, accept that the
answer is wrong, read the clock. 3.87 ns/px became 0.62.

### Faster two sessions ago

**One thing, worth 1.17x**, and only at saturation: the rank filter and the
convolver were parallelising rows *inside* the per-tile rayon job. Removing that
nesting is 2.053 s to 1.750 s on a 9000x9000 region, best of 5, same 142,957
objects out.

Against that, bounding per-worker scratch to the thread count costs **8%**. Net:
**1.08x**. Both figures come from separate processes rather than one interleaved
harness, so treat them as approximate.

### Not acceleration, however much it looks like it

- **"30% less work per pixel."** True (0.253 to 0.178 s/Mpx) and *not a
  speedup* — the pipeline is computing a different, correct result. Work that
  went away was work that should never have happened.
- **The headline region got 4.4x slower in wall clock**, 0.215 s to 0.95 s. Real,
  and not a regression: 4000x4000 is smaller than one of QuPath's grid steps, so
  it is 4 tiles across 20 threads where it used to be 272. Quote the Mpx/s on a
  saturated grid, or quote both with the tile counts attached. Never quote the
  0.215 s as a baseline — it was measured on the wrong tiling.
- **The background stage's other two levers came back negative.** QuPath's
  raster-repeat heuristic is already optimal (the textbook alternative is 2x
  slower), and the erosion — which the roadmap had blamed — turned out to be 12%
  of the stage, not 59%. Its third lever, the raster pass, is the 2.42x above,
  and it took the stage from 57.6% of pipeline time to 52.9%.
- **Every stage's share of pipeline time fell this session and none of them got
  faster.** The background went 52.9% to 37.6%, tracing 14.0% to 7.1%, the
  watershed 11.7%. Four new stages went into the denominator. A share is a
  ratio; read it with the stage list that was in the total when it was taken.
- **The peak resident set went 3.3 GB to 4.5 GB**, for the same reason and in the
  same direction: more stages, more buffers. Three of the four new stages reuse
  a buffer an earlier one has finished with, which is why it is 1.2 GB rather
  than 2.

**Nothing in this session made anything faster.** It closed the chain, recorded
one fixture, and found one divergence (D-008) by reading a call site.

---

## 4. How this project goes wrong

The same failure has now happened nine times, and it is worth recognising by
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
- `resolve_overlaps` skipped a candidate another tile's copy had already
  deleted, which is the obvious thing and not what QuPath does: its `listSecond`
  is a snapshot, and a deleted copy goes on deleting. Twelve objects on the
  profiled region, thirty-seven on a larger one. Every unit test passed both
  ways, because they were all written from the same reading of the Java that the
  code was.
- `resolve_overlaps` compares nucleus masks. QuPath compares **cells** — its
  `cellExpansion` defaults to 5 µm and `preferNucleus` is `false` — with smoothed
  polygon geometry. The grid was read carefully, the rule was read carefully, and
  what the rule is applied to was not. D-008; found by re-reading the call site
  while checking whether a 1.035x count was too good, not by any test.

The seventh has a variant worth naming separately, because it is about speed
rather than output:

> **A profile share, read as though it said why.**

The roadmap recorded for two revisions that the reconstruction's raster pass was
slow because of its index arithmetic, and named five specific things in the
inner loop. All five together are worth 1.03x. It was one dependency chain, and
the experiment that showed it — break the chain, keep everything else, accept
the wrong answer, read the clock — cost ten minutes and was never run. The same
shape as the blur's transpose in ROADMAP §4.3, which was also reasoned from a
share and was also wrong.

Every one produced entirely reasonable-looking output. The defences that actually
worked, in order: recording a fixture from a real QuPath run, counting objects,
and mutating each claimed convention to check a case fails without it. Looking at
the image never once caught anything.

**A tenth was avoided and is worth the same attention.** `RefineBoundary.java`'s
own comment predicted that a flat image would convolve to exactly zero and leave
`bp2` empty. The interior does; the ring one pixel inside the border comes back
at ±2.98e-08. The comment was written before the recording, the recording
contradicted it, and the comment was corrected before a line of Rust existed.
That is the only reason it is not the tenth entry above.

**So: read the call site before writing code against what `ROADMAP.md` says about
a stage.** Its plans for the stages that remain were written the same way as the
plans that were wrong.

---

## 5. What to distrust right now

- **The 1.035x count, above everything else on this list.** It is a count, and
  a count is the weakest evidence of a match there is: two runs can agree on how
  many objects they found and disagree about where every one of them is. The
  strong test — Dice or IoU >= 0.99 on the object masks — needs QuPath's
  per-object masks recorded and no fixture holds them. **A matching count is
  necessary and is not sufficient.**
- **`resolve_overlaps`, which is where the residual gap most likely is.** It has
  no QuPath-recorded fixture; its nine tests use hand-built candidates; one real
  divergence was found in it by re-reading the Java rather than by any of them
  failing; and D-008 says it is comparing the wrong objects anyway. It is still
  the weakest thing in the tree and now it is also load-bearing for the headline
  number. A recorded fixture would pay more here than anywhere else.
- **`ROADMAP.md`'s remaining plans** — the tail, ROI vertices and cell expansion
  and measurements. Written from a summary of the source, same as the five that
  turned out wrong. Every stage that has since been ported found something the
  plan did not have.
- **The 1.17x, the 8%, and the 1.19x wall clock** — separate processes, not
  interleaved. Directions are sound, magnitudes are approximate. The 2.42x on the
  reconstruction pass itself does meet the bar.
- **Peak memory is ~4.5 GB** at 20 threads on a saturated grid, up from 3.3 with
  this session's stages, bounded by threads x tile size and not growing with the
  region. That is a consequence of QuPath's 2048 px tiles and it is 60x what the
  pipeline used before §2. Bounded, documented, and larger than anyone would
  guess.

---

## 6. Where the next gain actually is

Measured, not assumed:

1. **The pipeline is memory-bandwidth-bound.** Parallel efficiency runs 96% at
   2 threads and 61% at 20; going 12 to 20 threads buys 3% for 30% more memory.
   A tile's working set went from ~2 MB (L2-resident) to ~145 MB (streaming from
   DRAM) when the tiling was corrected, and this session's stages took it to
   ~240 MB. This is the biggest lever and it did not exist a week ago.
2. **The shape split, 14.4% of stage time** — the largest single stage after the
   background's two halves, and no optimisation pass has touched it. The only
   new lever this session created, and the §4.4 stall check does not fire on it:
   an exact distance map in four sweeps plus `MaximumFinder`'s five stages is
   genuinely more work than the flood beside it at 11.7%.
3. ~~**The reconstruction raster pass**~~ — **done, 2.42x**, and the reason this
   entry gave was wrong; see §4. What is left there is another 2.6x in principle
   — the chain-cut probe runs at 0.62 ns/px against the current 1.62 — but
   collecting it means a parallel prefix scan over the max-min semiring, which
   is a different algorithm rather than a tighter loop, and in place the stage is
   already running into §6.1's bandwidth wall (2.03x isolated, 1.71x at 25
   tiles). Not obviously the next thing to spend a day on.
4. **Doing less work** — still untouched, still the largest lever in the
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
