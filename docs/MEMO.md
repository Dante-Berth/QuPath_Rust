# Memo — read this first

Twenty minutes of orientation before `ROADMAP.md`. It says what this project is
for, what is actually finished, what is actually faster, and what to distrust.

`ROADMAP.md` is the detail and the plan. `DIVERGENCES.md` is every place the port
deliberately differs from QuPath. `EVOLUTION.md` is what we learned and what it
cost — the reverted experiments and the recurring mistakes, which is the part
`git log` preserves badly. This file is the view from above, and it is short on
purpose.

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
are: the measurements. The ROI vertices, the boundary smoothing over them and the
cell expansion were the rest of that list. All three are ported and pinned in
`qupath-watershed` — against ImageJ's Wand and against QuPath's own chain stage
by stage — but **only the contour is wired into the pipeline**. `smooth_boundary`,
`expand_labels` and `cell_areas` are called by nothing but their own parity
tests, so no end-to-end figure here reflects them.

**The count.** This was quoted for several sessions as 26,647 objects against
QuPath's 25,757 cells — **1.035x** — on the profiled 4000x4000 region. That
number is still exactly right and it was the wrong number to quote, because it
is one region measured once, and it is the **best of ten**.

Measured across ten regions of CMU-1, chosen by the slide rather than by hand
(`cargo run --release --example tissue_scan -p qupath-slide` ranks them by
optical density; picking coordinates by eye put four of the first six on glass,
where there is nothing to disagree about):

| | |
| --- | ---: |
| pipeline objects | 156,979 |
| QuPath cells | 147,351 |
| gap, pooled | **1.0653** |
| range across regions | **1.012 – 1.129** |

Reproduce with `./parity/record-cell-counts.sh` then
`cargo run --release --example cell_counts -p qupath-pipeline`.

**D-008 is real but is not the whole gap, and the sweep is what shows it.** The
prediction was that the gap comes from resolving seam overlaps on nucleus masks
where QuPath resolves them on larger, smoothed cells. Three of the ten regions
are small enough to be a **single tile** — no seams at all, zero duplicates
removed — so their gap is everything that is *not* seam resolution. They still
disagree: 1.0121, 1.0177, and **1.1130**.

**What the gap actually tracks is cell density, inversely:**

| region | cells/Mpx | ratio | tiles |
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

Dense regions agree to within 1–3.5%, sparse ones diverge by 6–13%, and the
seam count does not sort the table — density does. That points away from
`overlaps.rs` and at the **filters**: a sparse region is mostly background, so
what differs is which faint candidates survive the mean-intensity and area
tests. `ObjectFilters.json` pins those stages against QuPath on 17 recorded
images and they pass, which means the disagreement is in what reaches them.

**Narrowed further in D-009**, on the three single-tile regions where no seam
resolution happens at all. It is not the background mask (never fires), not
object scale (127.2, 127.2, 127.8 px/object), and not over-large blobs
(`max_area` rejects zero). What moves with the gap is the **merge and the shape
split**, together and only on the clumped region: the merge keeps 0.557/0.560 of
its input on the dense regions against **0.439** on the sparse one, and the split
recovers 1.57x/1.57x against **2.16x**.

**And that pair has now been ruled out.** `MergeAndSplitTile.json` records
QuPath's merge and split over eight crops of real CMU-1 tissue, four from the
region that disagrees and four from one that agrees; given the same input mask
this port reproduces the merged mask, the segmentation and the split **pixel for
pixel on all eight**, with the object counts matching on each. The crops carry
the regional signature — keep 0.425 sparse against 0.591 dense, recover 1.65x
against 1.24x — so the two stages do visibly more work on clumped tissue and do
QuPath's work while doing it. Confirmed by three mutations, each caught.

So the merge and the split are where the gap becomes **visible**, not where it
is **made**: their ratios move because their input does. The search is upstream
of them — the background estimate and its subtraction, the LoG, the regional
maxima, the labelling and the flood — with the mean-intensity filter the least
likely, being the one link already pinned (`ObjectFilters.json`, 17 images).

**Walking that ladder names a suspect.** `--example stage_ladder` prints both
sides' counts at three boundaries over those crops. It is **already wrong at the
first rung** — candidates, before the intensity filter and well before the merge
(1.058 sparse, 1.079 dense) — and the intensity filter then roughly doubles it
(1.270, 1.299), because QuPath keeps 0.441/0.547 of its candidates where this
keeps 0.530/0.659. The filter itself is pinned, so what differs is what it
measures: `ipToMeasure`, the **background-subtracted image**. A background
estimate that differs moves every candidate's mean at once and in one direction.

**With one caveat that kept it a suspect rather than a conclusion:** at 320x320
the sparse and dense ratios are nearly identical, where whole regions have them
at 1.012 against 1.113. These crops do not reproduce the density regime, so what
is localised there is a stage-level difference at that crop size, not shown to be
the same effect.

**And the suspect was wrong.** `BackgroundEstimate.json` records the stage over
seven crops of real tissue as an input image and an output image, and this
pipeline reproduces it **bit for bit on every pixel of every crop**, both
branches. Three stages are now ruled out this way — the merge, the split, and
the background estimate — and what is left between the deconvolution and the
ladder's first rung is the blur, the Laplacian, the regional maxima, the
labelling and the flood, plus the deconvolution and the decode under it. Each
has a synthetic fixture and none has one on tissue. See D-009.

**So the honest statement is: 1.065x pooled, 1.012–1.129 by region, direction
always the same (this over-counts), cause identified as density-dependent, not
primarily D-008, and not the merge or the split either.** That is a weaker claim
than the file used to make and a better-founded one, and each narrowing has cost
a hypothesis rather than confirmed one.

**The time**: 70.1 Mpx/s on a 9000x9000 region, 25 tiles on 20 threads — a grid
large enough to fill the pool. That is **down** from 80.4 and it is not a
regression: four stages that did not exist are now in the path. Nothing got
slower and nothing got faster this session. Do not compare either figure against
the 42.6 Mpx/s this file used to quote: that was a different region with more
tissue in it, and only the ratio within one region means anything.

**Against QuPath, on the region QuPath was profiled on** — 4000x4000 at
(27776, 5952), same slide, same default parameters:

| | wall clock | vs QuPath |
| --- | ---: | ---: |
| QuPath, 1 thread | 15.87 s | — |
| this pipeline, 1 thread | **3.359 s** | **4.7x** |
| this pipeline, 20 threads | **1.019 s** | **15.6x** |

Read the single-threaded row, not the twenty-threaded one, when the question is
whether the port is faster: QuPath's profile was taken single-threaded, and
QuPath parallelises across tiles in real use. And read both as generous to us,
because QuPath's 15.87 s includes the tail this pipeline has not ported — the
measurements. The gap is decisive either way; what is not yet measurable is its
exact size.

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

**Nothing, and the session's result is a lever closed rather than opened.**
ROADMAP §4.9's last untried idea — a log-depth SIMD prefix-maximum for the
erosion's van Herk scan, the only change identified that attacks the 3.1x the
horizontal half vectorises at rather than its pass count — was written and
measured. It **works**: bit-identical to the scalar scan over every span and
length including `NaN`, and **1.19x** on the scan against a 1.50x ceiling from
cutting the dependency outright.

**It is worth ~1.013x end to end, so it was not shipped.** The scan saves ~1.0
ns/px where the whole filter costs 19.37, the erosion is 28% of the saturated
grid, and the product is below this machine's noise. It needs `unsafe`, which
the guidelines allow only behind a committed profile showing what it buys —
`scan_chain_cut` is that profile and it prices the change out. **The erosion's
horizontal half is now closed on all three fronts**: pass count, block-scheme
crossover, and the scan itself.

The differential test earned its keep on the first run: `_mm256_max_ps` returns
its second operand on `NaN` where the scalar fold discards one, so the two
paths disagreed until the accumulator was moved to the first operand and
incoming `NaN`s were blended to `-inf` on load. Neither bug was visible by
reading the code.

### Faster the session before

**~1.3%, from deleting memsets, and nothing else.** Seven tile buffers were
cleared and zero-filled before the stage that overwrites every pixel of them —
the background estimate, the Laplacian, the labels, the region map twice, the
merge's region map, the binary mask — and the tracer cleared 18 MB of escape
stamps three times a tile to erase numbers a counter can step past instead.
About 150 MB per tile of writes nothing read. On the saturated grid, stage time
18.079 s to 17.839 s, lower in all three alternating pairs. Object count
unchanged.

That is the whole of it. **Four other changes were measured and reverted**, and
they are the more useful half of the session — see §4's second entry, and
ROADMAP §4 for the numbers. One of them, four rows of the reconstruction's
raster pass in flight at once, is **1.21x in isolation and 0.83x where the
pipeline actually runs**, which is the first time this project has had a
speedup change sign.

**The stage table everybody optimises against turned out to be a
configuration.** It comes from the 4000x4000 profiled region — four tiles, so
four cores of a twenty-thread pool, on a machine with six fast cores and eight
slow ones. On a grid that fills the pool the **erosion is 28%** and the largest
stage in the pipeline, where that table says 10.4% and the roadmap says "the
erosion is not the problem". The stage this file has pointed three optimisation
passes at, the reconstruction, is 10.8% there against 26.2% here. Read ROADMAP
§4's new subsection before picking anything up.

### Faster two sessions before

**The region tracer, 1.1x, and nothing else.** The hole test walked each
region's bounding box twice — once to flood the background in from the border,
once more to claim what the flood missed — and the second pass is only useful
when the first left something behind. The flood can tell: count what it reached
inside the box, add the region's own area, and if that reaches the box's area
there is nothing to find. It fires for 26,976 of 27,308 regions on a real tile.
Interleaved, with the region map and the region count asserted identical: 1.15x
on the recorded run and 1.08x to 1.16x across five, so read it as 1.1x. The
tracer is ~15% of stage time across its three stages, which puts it at ~1.015x on
the pipeline — inside this machine's run-to-run spread end to end.

**Four things were tried and reverted** in that session too: see §4's second
entry.

### Faster three sessions ago

**The reconstruction's raster pass, 2.42x**, which was the first end-to-end wall
clock gain this project had: **1.19x** on a 9000x9000 region at 25 tiles on
20 threads, 66.7 to 80.4 Mpx/s, with 82,997 objects out either way.

It is worth reading §4 of this file before the number. The roadmap had said for
two revisions why that pass was slow — index arithmetic, a per-pixel `Option`,
edge branches in the inner loop — and **all of it together is worth 1.03x**. The
cost was one loop-carried dependency, and the measurement that found it took ten
minutes: feed the loop a constant instead of its left neighbour, accept that the
answer is wrong, read the clock. 3.87 ns/px became 0.62.

### Faster four sessions ago

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

  **That erosion figure is now known to be a figure about four tiles.** On a
  grid that fills the thread pool the erosion is 28% of the whole pipeline and
  its largest single stage — larger than the reconstruction the three sentences
  above are about. Both numbers are correct measurements of different
  configurations, which is the point: see ROADMAP §4.
- **Every stage's share of pipeline time fell this session and none of them got
  faster.** The background went 52.9% to 37.6%, tracing 14.0% to 7.1%, the
  watershed 11.7%. Four new stages went into the denominator. A share is a
  ratio; read it with the stage list that was in the total when it was taken.
- **The peak resident set went 3.3 GB to 4.5 GB**, for the same reason and in the
  same direction: more stages, more buffers. Three of the four new stages reuse
  a buffer an earlier one has finished with, which is why it is 1.2 GB rather
  than 2.

**Nothing in that session made anything faster.** It closed the chain, recorded
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

**That variant now has five instances and they say something sharper than "read
the share carefully".** The reconstruction's index arithmetic: 1.03x. The shape
split's branchless height, tree-shaped minimum and branchless neighbourhood:
1.02x together. The tracer's 4 M hardware divisions per tile, removed entirely:
1.00x. The rank filter's prefix and suffix scans, paired so four chains run in
flight: 1.00x. Meanwhile the changes that *did* pay were a stage that ran twice
(1.1x on the tracer), a memset nothing read — twice now — and three loop-carried
dependencies (2.3x, 3.7x, 2.4x). **Expensive-looking instructions on a covered
path are free; work that happens and is not needed is not.** Ask which of the
two a change is before writing it, and prefer the diagnostic that answers it in
ten minutes.

**A third variant arrived this session and it is worse, because the answer was
not merely small but the wrong sign.** Putting four rows of the reconstruction's
raster pass in flight at once is bit-identical, 1.21x on the pass in isolation
and 1.14x on the whole reconstruction — a proper interleaved A/B, output
asserted equal on every timed iteration, exactly the bar this repository sets —
and on the grid the pipeline actually runs it is **0.83x**. It does not do one
instruction more; it reads nine streams per tile where the old one read three,
and twenty tiles of that is a different machine. **The component harness is
necessary and it is not sufficient. Measure on the saturated grid before
keeping anything.**

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
- **`ROADMAP.md`'s remaining plans** — the tail and measurements. Written from a
  summary of the source, same as the five that turned out wrong. Every stage
  that has since been ported found something the plan did not have — cell
  expansion, the most recent, found two: the EDM's inverted background argument
  and which side of the expansion threshold a tie falls on.
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
2. ~~**The shape split, 14.4% of stage time**~~ — **looked at, and there is no
   hotspot in it.** It is 26% distance map and 74% segmentation, and the
   segmentation is five stages whose largest is a third of it. Three
   transformations bought 1.02x together, which is noise, and were reverted; see
   ROADMAP §4. Making it faster means changing what it touches — five sequential
   passes over buffers that do not fit in cache — and that belongs with §6.1
   rather than with a tighter loop.
3. ~~**The reconstruction raster pass**~~ — **done, 2.42x**, and the reason this
   entry gave was wrong; see §4. What is left there is another 2.6x in principle
   — the chain-cut probe runs at 0.62 ns/px against the current 1.62 — but
   collecting it means a parallel prefix scan over the max-min semiring, which
   is a different algorithm rather than a tighter loop, and in place the stage is
   already running into §6.1's bandwidth wall (2.03x isolated, 1.71x at 25
   tiles). Not obviously the next thing to spend a day on.
4. ~~**The erosion's van Herk scan**~~ — **closed.** It was the largest single
   stage on the saturated grid at 28%, and 86% of it is the horizontal half.
   All three ways in are now measured rather than open: the pass count is
   argued shut (eleven rectangles is the minimum for an exact cover), the
   block-scheme crossover was swept inside one binary, and the serial scan
   itself was rewritten as a log-depth AVX2 prefix-maximum — which **works,
   1.19x on the scan, and is ~1.013x on the pipeline**, below what this machine
   can resolve. `scan_chain_cut` has the numbers. What remains in that stage is
   traffic, which is entry 1, not a loop.
5. **Doing less work** — still the largest lever in the guidelines' own
   ordering, and the only kind of change that has paid in any session.
   `Slide::level_for_width` exists and nothing uses it; the profiled
   region is 99% tissue but a whole slide is mostly glass. The tracer's 1.1x
   (§3) is the small version of the same idea: a pass that ran and was not
   needed.

**Read 1 and 5 together, because four sessions now point the same way.** Every
loop-level change measured since the tiling was corrected has come back between
1.00x and 1.02x in place, including three that were genuinely faster in
isolation and one that reversed sign. The stages are not stalling on
arithmetic any more; they are waiting on DRAM. The next real gain is either
touching less memory per tile or not processing the tile at all.

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
