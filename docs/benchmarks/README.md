# Benchmarks

Every performance claim in this repository is backed by output committed here,
from both implementations on the same machine.

Regenerate with:

```bash
./parity/bench.sh                              # QuPath side
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo bench --workspace
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-filters   --example previous_vs_current
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-watershed --example previous_vs_current
RUSTFLAGS="-C target-cpu=x86-64-v3" \
    cargo run --release -p qupath-pipeline  --example previous_vs_current
```

`bench.sh` needs JDK 25+ on `PATH`, or `JAVA_HOME` pointing at one — see
`parity/README.md`.

## How these numbers are taken, and why it changed

**This machine is not quiet.** Load average during these runs is around 3.
Medians move by up to 3x between runs at small radii, which is larger than
most of the effects being measured. Two consequences, both of which invalidate
how earlier figures here were produced:

- **A benchmark taken now cannot be compared against one taken earlier.** Every
  before/after claim below instead comes from an A/B harness that runs both
  implementations **interleaved in one process**, so they see the same machine,
  and **asserts they produce identical output on every timed iteration** — a
  version that got fast by computing something else fails rather than
  reporting a speedup. Sources are the `previous_vs_current` examples.
- **The statistic is the minimum**, not the median, over many repetitions. Under
  external load the median measures contention as much as it measures the code;
  the minimum estimates the uncontended cost. Both the Rust and the Java
  harnesses now report it, so the two are comparable.

**The benchmarks themselves had a defect.** The rank and watershed benchmarks
handed the routine a freshly cloned megabyte-scale input per iteration through
criterion's `iter_batched`. For a memory-bound kernel the resulting allocator
and page-fault traffic is a first-order term: the rank benchmark reported
radius 10 as *faster* than radius 5, and inflated the radius-2 figure roughly
4x against a direct timing of the same call. Both now allocate outside the
timed region. Figures recorded before this was fixed are not comparable with
figures recorded after it.

## Measurement scan and filter

Workload: 1,000,000 detections, 20 features each. Sum one feature across every
object, then count the objects whose value exceeds a threshold. The target
feature is `Feature 13` — deliberately not the first, so QuPath's name lookup is
not trivially cached.

| Operation | QuPath | Rust columnar | Ratio |
| --- | ---: | ---: | ---: |
| Scan one feature | 43.61 ms | 0.389 ms | 112× |
| Filter, count only | 43.84 ms | 0.079 ms | 555× |
| Filter to index list | 43.84 ms | 0.412 ms | 106× |

Medians. QuPath figures are the median of 50 timed iterations after 20 warmup
iterations; Rust figures are criterion's median estimate.

### What is excluded

- **JVM startup**, and the JIT warmup iterations. Only the steady-state loop is
  timed.
- **Construction** on both sides. Building the 1M objects takes QuPath ~1.6 s;
  the Rust table is built outside the timed region too. A pipeline that builds
  objects once and queries them repeatedly is the case this measures.
- **I/O and decode.** No image is read. This is purely the object/measurement
  layer.

### Why the gap is this large

The ratios exceed the 10–100× the design notes anticipate for this layer, so
they were checked rather than reported as-is:

- Both implementations **assert their computed result** (sum `499500000`, count
  `499000`) before timing. A scan that got fast by not happening fails the
  build. This is the check that matters most, and it is in both harnesses.
- The gap is a **layout difference, not a language difference**. QuPath resolves
  a string to an index per object and dereferences a per-object array, so a scan
  touches 1M scattered allocations. The columnar side reads one contiguous 4 MB
  slice. Java is not being penalised for being Java; it is being penalised for
  pointer chasing, and a Java program using a `float[]` column would land near
  the Rust figure.
- **`filter` at 555× is a cache-resident best case.** One 4 MB column fits in
  this machine's 24 MB L3, so it runs at ~49 GB/s — cache bandwidth, not DRAM.
  On a table large enough to spill to memory the ratio will fall. Treat 555× as
  the ceiling for a hot column, not a general figure.
- `scan` is slower than `filter` because it widens each `f32` to `f64` to match
  QuPath's accumulation; at ~10 GB/s it is conversion-bound rather than
  memory-bound.

The honest summary: **~100× on this layer, in this shape, with the data hot.**
It does not carry over to a whole pipeline, where decode dominates.

## Watershed: queue and flood

The profile (`docs/profiles/README.md`) puts `PriorityQueue.siftDownComparable`
plus `Watershed$PixelWithValue.compareTo` at **29.4% of cell-detection self
time** — the largest single cost.

### The queue, in isolation

Workload: 1,048,576 pixels of an EDM-like surface with plateaus, pushed then
drained in priority order. Criterion medians, `target-cpu=x86-64-v3`, raw
output in `rust-watershed.txt`.

| Structure | Time | Throughput | vs heap |
| --- | ---: | ---: | ---: |
| `BinaryHeap<PixelWithValue>` (QuPath's shape) | 109.54 ms | 9.57 Melem/s | — |
| `HierarchicalQueue::new` per flood | 5.33 ms | 196.8 Melem/s | 20.6x |
| `HierarchicalQueue::reset`, reused | **3.43 ms** | **306.1 Melem/s** | **32.0x** |

**Both sides are Rust.** The baseline is a `BinaryHeap<PixelWithValue>` with the
same field layout and the same ordering QuPath uses — value descending, ties
broken on an insertion counter. It is not a strawman and not a Java measurement:
it isolates the data structure, so the ratio is attributable to layout alone and
not to language, JIT, or GC.

The gain comes from three things, in the order §0 ranks them:

- **No comparator.** Priority is the bucket index, so `siftDown` and `compareTo`
  disappear rather than getting faster. This is most of the 20x.
- **No per-pixel allocation.** A queued pixel is one `u32` instead of a ~32-byte
  heap object — 8x less memory, contiguous instead of pointer-chased.
- **O(1) instead of O(log n)** per push and pop.

The third row is the one added since: `HierarchicalQueue::new` allocates one
`Vec` per level, so building a queue per tile is four thousand allocations per
tile. `reset` clears them instead. That is worth 1.55x *here*, where the
microbenchmark does nothing but push and pop — and much less on a real flood,
which is the next table.

### The flood

Marker-controlled flood over the same surface, 8-connected. Interleaved A/B,
minimum of 100 repetitions, with the two implementations asserted to produce
identical labels on every iteration. Raw output in `rust-watershed-ab.txt`.

| Image | Previous | Current | Current, reused | Speedup |
| --- | ---: | ---: | ---: | ---: |
| 256x256 | 1.86 ms | 1.42 ms | **1.27 ms** | **1.47x** |
| 512x512 | 7.07 ms | 5.24 ms | **5.09 ms** | **1.39x** |
| 1024x1024 | 28.02 ms | 21.11 ms | **20.02 ms** | **1.40x** |

The flood recovered `(x, y)` from the popped index with `index / width` and
`index % width` — hardware integer divisions by a runtime value, which the
compiler cannot strength-reduce, one of each per popped pixel — then reached
each neighbour through a bounds-checked coordinate shift, twice over.

Only pixels on a one-pixel frame can have a neighbour outside the image: 0.8%
of a 512x512 tile. Seeding already visits every pixel, so it marks that frame
in the same per-pixel byte that carries the queued flag — **no extra memory
over the `Vec<bool>` it replaces** — and the flood reaches an interior pixel's
neighbours by adding a precomputed flat offset. Neighbour order is unchanged,
which matters: it is what fixes FIFO tie-breaking on plateaus, and therefore
where the watershed line falls.

**Buffer reuse is the small part of this**, about 3% of the 1.40x. The queue
microbenchmark above makes allocation look like a third of the cost; on a whole
flood it is a rounding error, because the flood does much more work per queued
pixel than a push-pop loop does. Measuring the component in isolation
overstated what it was worth in place — which is the same mistake, one level
down, that the Amdahl note below is about.

### What this does *not* claim

**This is not a 20x, or a 1.4x, speedup of cell detection.** The queue figure is
20-32x on a component the profile measures at 29.4% of detection self time, and
Amdahl caps that at about **1.4x** end-to-end. The flood figure is 1.4x on the
Rust flood, which is not the flood QuPath runs. Both hold only once the
surrounding watershed is actually driven from Rust — which it is not. The Rust
`watershed` runs on in-memory buffers; nothing connects it to a slide.

The honest end-to-end number today remains the one in `docs/profiles/README.md`:
**15.87 s** for QuPath cell detection on a 4000x4000 region, unchanged, because
no ported code is in that path.

## Rank filter (MIN/MAX)

The profile puts `RankFilters.getSideMax` and `getAreaMax` at 11.6% of
cell-detection self time. This is the record of porting it, including the two
implementations that lost, because the losses are the informative part.

512x512 `f32`, MAX. Minimum over repetitions, both sides threaded, on the same
machine in the same session. Rust: `target-cpu=x86-64-v3`, 20 cores, raw output
in `rust-rank-ab.txt` and `rust-rank.txt`. ImageJ: minimum of 60 timed
iterations after 20 warmup, JVM startup excluded, ImageJ's own threading left
enabled, raw output in `BenchRankFilter.txt`. Both sides copy the image once
inside the timed region.

| Radius | ImageJ | Naive O(area) | Incremental | Rectangles | vs ImageJ |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1.0 | 1.060 ms | — | 0.439 ms | **0.110 ms** | **9.7x** |
| 2.0 | 0.790 ms | 9.95 ms | 0.630 ms | **0.122 ms** | **6.5x** |
| 3.0 | 0.680 ms | — | 0.823 ms | **0.159 ms** | **4.3x** |
| 5.0 | 0.903 ms | 53.09 ms | 1.296 ms | **0.215 ms** | **4.2x** |
| 8.0 | 1.053 ms | — | 2.252 ms | **0.314 ms** | **3.4x** |
| 10.0 | 1.265 ms | — | 2.811 ms | **0.411 ms** | **3.1x** |
| 20.0 | 3.414 ms | — | 8.436 ms | **1.003 ms** | **3.4x** |

The naive column is from the original recording and is not comparable with the
rest — it predates both the harness fix and the minimum statistic. It is kept
only to show the order of magnitude.

### Correction to what this file previously claimed

This section used to report the incremental implementation at **1.90x ImageJ at
radius 2 and 1.04x at radius 5**, from medians taken on a quieter machine. That
does not reproduce. Measured here, interleaved and against the same ImageJ
build, the incremental implementation **loses to ImageJ from radius 3 upward** —
by 1.2x at radius 3, 1.4x at radius 5 and 2.5x at radius 20. It was ahead only
at radius 1 and 2.

The old file did predict the shape of this ("at larger radii ImageJ may well
come out ahead"); it was the crossover point that was wrong, and it was wrong
because a median under load and a median on an idle machine were being compared
as though they were the same measurement.

### The first version was 7.6-35.6x slower

Writing the obvious thing — scan the whole kernel per pixel — lost badly, and
the scaling said why. Growing the kernel 4.2x in area (radius 2 to 5, 21 to 89
pixels) slowed ImageJ by 1.13x and the naive port by 5.34x, tracking the area
exactly. ImageJ was not doing the obvious thing.

**Rust was never the variable.** ImageJ was running ~4 operations per pixel in
Java while the port ran 89 in Rust. Rust makes each operation faster; it does
not make 89 fewer than 4. This is §0's ordering measured from the wrong side.

### The second version copied ImageJ, and inherited its scaling

ImageJ uses a *lazy* incremental extremum, read from `RankFilters.java` rather
than guessed: sliding one column right, the entering column wins outright if it
beats the running extremum, the leaving column decides otherwise, and only when
the leaving column *held* the extremum is a rescan needed. That is O(kernel
height) per pixel in the common case rather than O(kernel area), and it took the
port from 9.95 ms to 4.79 ms at radius 2.

But O(kernel height) still grows with the radius, and the table above is what
that costs: the incremental implementation is 19x slower at radius 20 than at
radius 1. Copying the reference implementation caps you at the reference
implementation.

### What the current version does

**A disc is exactly a union of nested rectangles.** A disc's half-width never
increases with `|dy|`, so for each distinct half-width `h` the rows at least
that wide form a single band centred on the middle row — and that band, `h`
wide either side, is a rectangle. The union over every distinct `h` is the
disc, with no approximation. Radius 5 (89 pixels, 11 rows) is four rectangles:
11x3, 9x7, 7x9 and 3x11.

A rectangle is *separable*, which a disc is not: the maximum over a box is the
maximum over its rows of the maximum over its columns. So each rectangle
becomes a vertical fold of whole source rows into a column accumulator, then a
one-dimensional windowed maximum along that accumulator. And because the
rectangle heights are nested, **one accumulator grown outward from the centre
serves every rectangle** — a source row is read once per output row, not once
per kernel row.

Three things follow, in the order §0 ranks them:

- **Less work.** Per-pixel cost stops tracking the kernel. Radius 20 is 9.1x
  the pixels of radius 2 but only 8.2x the time, against 13.4x for the
  incremental version.
- **Better layout.** Every inner loop is now a maximum between two contiguous
  `f32` slices, which the compiler vectorises **with no `unsafe`** — §6 asks
  for `chunks_exact`/`zip` before `get_unchecked`, and nothing here needed the
  latter. The previous version's inner loop read a strided column with a
  clamped index and a multiply per pixel.
- **Fewer copies.** `RankFilter::apply_into` filters between separate buffers,
  which for MAX copies nothing and allocates nothing — the shape a tile
  pipeline has. Worth a further 1.1-1.9x over the in-place form in the table,
  which pays for one copy of the image.

The horizontal pass switches to van Herk/Gil-Werman blocks above half-width 2,
where its fixed four passes beat `2h+1` shifted maxima.

### What this is and is not

It is a like-for-like comparison: same workload, same machine, same session,
both sides threaded, and **identical output on all 42 recorded ImageJ parity
cases**. MIN and MAX select an input value rather than computing one, so that
is bit equality and not a tolerance. The A/B harness also asserts the old and
new implementations agree on every timed iteration.

**It is a better algorithm, and that is the whole margin.** Unlike the previous
version, this is no longer 1-3x from layout on the same algorithm — it is
asymptotically less work. That is also why it is not evidence about Rust: the
same decomposition in Java would beat ImageJ's `RankFilters` too.

**Buffer reuse contributed almost nothing.** Holding the scratch buffers across
calls, which §0 ranks fourth, is worth under 5% here; the copy elision in
`apply_into` is worth more, and the algorithm is worth 4-8x. The ordering in §0
is not decoration.

**And it remains 11.6% of one stage.** End-to-end detection time is unchanged;
see the summary below.

## Colour deconvolution

Recorded after the parity fixture in `ROADMAP.md` §1.1 corrected the stain
vectors, so the figures below are for code that agrees with QuPath to 5.9e-8.
Raw output in `rust-deconvolve-ab.txt` and `rust-pipeline-region.txt`.

Matching QuPath meant accumulating in `f64` and narrowing to `f32` once, as
`ColorTransformer.deconvolve` does, where the port had used `f32` throughout.
That is a real cost: deconvolution went from 24-27% of pipeline time to
**36.5%**, briefly making it the second-largest stage.

The fix is the one this file's predecessor predicted, and it is also what QuPath
does. `ColorDeconvolutionHelper.makeODLUT` builds a 256-entry optical-density
table per channel, because the deconvolved value depends on the three channel
bytes and nothing else. Folding each stain row's coefficient into the table as
it is built removes the three multiplies as well as the three `log10` calls,
leaving three loads and two adds per pixel over 6 KB of L1-resident table.

Interleaved A/B, minimum of 200 repetitions, `target-cpu=x86-64-v3`, single
thread, both output buffers allocated outside the timed region.

| Tile | Per-pixel | Lookup table | Speedup | Throughput |
| --- | ---: | ---: | ---: | ---: |
| 256² | 0.953 ms | **0.036 ms** | **26.5x** | 1825 Mpx/s |
| 512² | 3.808 ms | **0.131 ms** | **29.1x** | 2004 Mpx/s |
| 1024² | 15.267 ms | **0.511 ms** | **29.9x** | 2053 Mpx/s |

**The harness asserts bit equality, not a tolerance.** The folded table holds
the same products, formed by the same multiply and summed in the same order, so
the two paths must agree exactly; anything else would mean the fold had changed
the arithmetic rather than just precomputing it.

### This one did carry over

The standing warning in this file is that a component measurement overstates
what it is worth in place, and it has been right twice. Here it was not:

| | Before | After |
| --- | ---: | ---: |
| Deconvolve, summed across threads | 0.768 s | **0.031 s** |
| Share of pipeline stage time | 36.5% | **2.7%** |
| Markers seeded | 211,728 | 211,728 |

The 24.8x in place matches the 26-30x measured in isolation, because unlike
buffer reuse this removes work rather than overhead, and the work removed was
the whole stage. The identical marker count is the check that it removed only
that: a deconvolution that got fast by computing something else would move it.

Two things this does **not** say. It is not an end-to-end speedup — the
pipeline's wall clock is not QuPath's 15.87 s and is not comparable with it, for
the reasons in the summary below. And it must not be read against the 0.114 s
recorded for the pipeline before §1.1, because that run was deconvolving with
the wrong stain vector and so was not doing the same work.

## Where this leaves the pipeline

Three components are now faster in isolation, and the profile says what each is
worth if it were in the path:

| Component | Isolated gain | Share of detection self time | Amdahl ceiling |
| --- | ---: | ---: | ---: |
| Watershed queue | 20-32x | 29.4% | 1.42x |
| Rank filter | 3.1-9.7x vs ImageJ | 11.6% | 1.12x |
| Measurement scan/filter | ~100x | 5.3% | 1.06x |

Those ceilings **multiply to about 1.7x**, and only if every one of the three
were driven from Rust at the measured ratio. None of them is. The Rust crates
run on in-memory buffers handed to them by tests and benchmarks; nothing reads
a slide, and no ported code executes during a QuPath detection run.

**So the honest end-to-end number is still 15.87 s**, exactly as it was before
any of this work — see `docs/profiles/README.md`. Every figure in this file is
a component measurement, and component measurements have now twice overstated
what they were worth in place: buffer reuse looked like a third of the queue
cost and was 3% of the flood, and the incremental rank filter looked like 1.9x
ahead of ImageJ and was behind it from radius 3.

The largest remaining piece of work is not a further optimisation of these
three. It is wiring any of them into a pipeline that reads a slide, at which
point §0.1 — doing less work, by way of the right pyramid level, the right ROI
and early rejection — becomes available, and it outranks everything measured
here.

## Environment

See `environment.txt`, regenerated by `bench.sh`.
