# Benchmarks

Every performance claim in this repository is backed by output committed here,
from both implementations on the same machine.

Regenerate with:

```bash
./parity/bench.sh                              # QuPath side
RUSTFLAGS="-C target-cpu=x86-64-v3" cargo bench -p qupath-objects
```

`bench.sh` needs JDK 25+ on `PATH`, or `JAVA_HOME` pointing at one — see
`parity/README.md`.

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

## Watershed priority queue

The profile (`docs/profiles/README.md`) puts `PriorityQueue.siftDownComparable`
plus `Watershed$PixelWithValue.compareTo` at **29.4% of cell-detection self
time** — the largest single cost. This measures replacing that comparison heap
with a hierarchical (bucket) queue.

Workload: 1,048,576 pixels of an EDM-like surface with plateaus, pushed then
drained in priority order.

| Structure | Time | Throughput |
| --- | ---: | ---: |
| `BinaryHeap<PixelWithValue>` (QuPath's shape) | 113.27 ms | 9.26 Melem/s |
| `HierarchicalQueue` | 5.64 ms | 185.8 Melem/s |
| **Ratio** | **20.1×** | |

Criterion medians, `target-cpu=x86-64-v3`. Raw output in
`rust-watershed.txt`.

### What is being compared

**Both sides are Rust.** The baseline is a `BinaryHeap<PixelWithValue>` with the
same field layout and the same ordering QuPath uses — value descending, ties
broken on an insertion counter. It is not a strawman and not a Java measurement:
it isolates the data structure, so the ratio is attributable to layout alone and
not to language, JIT, or GC.

The gain comes from three things, in the order §0 ranks them:

- **No comparator.** Priority is the bucket index, so `siftDown` and `compareTo`
  disappear rather than getting faster. This is most of the 20×.
- **No per-pixel allocation.** A queued pixel is one `u32` instead of a ~32-byte
  heap object — 8× less memory, contiguous instead of pointer-chased.
- **O(1) instead of O(log n)** per push and pop.

### What this does *not* claim

**This is not a 20× speedup of cell detection.** It is 20× on a component that
the profile measures at 29.4% of detection self time. Amdahl caps what that is
worth end-to-end at about **1.4×**, and only once the surrounding watershed is
actually driven from Rust — which it is not yet. The Rust `watershed` runs on
in-memory buffers; nothing connects it to a slide.

The honest end-to-end number today remains the one in `docs/profiles/README.md`:
**15.87 s** for QuPath cell detection on a 4000×4000 region, unchanged, because
no ported code is in that path.

## Rank filter (MIN/MAX)

The profile puts `RankFilters.getSideMax` and `getAreaMax` at 11.6% of
cell-detection self time. This is the record of porting it, including the two
intermediate versions that lost, because the losses are the informative part.

| Radius | ImageJ | Naive O(area) | + incremental | + rayon | vs ImageJ |
| --- | ---: | ---: | ---: | ---: | ---: |
| 2.0 | 1.32 ms | 9.95 ms | 4.79 ms | **0.691 ms** | **1.90× faster** |
| 5.0 | 1.49 ms | 53.09 ms | 11.24 ms | **1.436 ms** | **1.04× faster** |

Run-to-run variation on the parallel figures is a few percent, so treat the
radius-5 result as parity with ImageJ rather than a win.

512×512 `f32`, MAX. ImageJ: median of 50 timed iterations after 20 warmup, JVM
startup excluded, and ImageJ's own threading left enabled. Rust: criterion
median, `target-cpu=x86-64-v3`, 20 cores. Raw output in `BenchRankFilter.txt`
and `rust-rank.txt`.

### The first version was 7.6–35.6× slower

Writing the obvious thing — scan the whole kernel per pixel — lost badly, and
the scaling said why. Growing the kernel 4.2× in area (radius 2 → 5, 21 → 89
pixels) slowed ImageJ by 1.13× and the naive port by 5.34×, tracking the area
exactly. ImageJ was not doing the obvious thing.

**Rust was never the variable.** ImageJ was running ~4 operations per pixel in
Java while the port ran 89 in Rust. Rust makes each operation faster; it does
not make 89 fewer than 4. This is §0's ordering measured from the wrong side.

### What ImageJ actually does

Not van Herk — a *lazy* incremental extremum, read from `RankFilters.java`
rather than guessed. Sliding one column right:

1. if the entering column's extremum beats the running one, it **is** the new
   extremum, and nothing else is read;
2. otherwise the leaving column decides — if its extremum is below the running
   one, the running extremum is still inside the kernel and stands;
3. only when the leaving column *held* the extremum is a full rescan needed.

The rescan is rare on real images, so the common case is O(kernel height), not
O(kernel area). Implementing this took the port from 9.95 ms to 4.79 ms at
radius 2, and from 53.09 ms to 11.24 ms at radius 5.

### Then parallelism, for a like-for-like comparison

Still 3.6× behind at that point, and the remaining difference was structural:
`RankFilters` threads its work and the port did not. Comparing a
single-threaded port against a threaded original is not a like-for-like
measurement.

Rows are independent — each reads the untouched source and writes only its own
output row — so `par_chunks_mut` parallelises them without synchronisation, at
row granularity per §6. MIN and MAX select an existing value rather than
accumulating, so no float reduction crosses a row boundary and the result does
not depend on how rayon splits the work; a test asserts bit-identical output at
1, 2, and 8 threads.

### What the 1.9× is and is not

It is a like-for-like comparison: same workload, same machine, both sides
threaded, same algorithm class, identical output on all 42 parity cases. The
margin comes from layout and lower per-operation cost, which is the 1–3× §0
predicts for equivalent code — **not** from a better algorithm, because the
algorithm is ImageJ's.

It shrinks as the radius grows (1.90× at r=2, 1.04× at r=5): the larger kernel
spends more time in the column scans, where ImageJ's cache-stripe layout does
the same work as this port. **At radius 5 this is parity, not a win**, and at
larger radii ImageJ may well come out ahead.

The radius that matters is whichever `WatershedCellDetection` actually passes —
it calls `rank` with several, derived from the user's cell-size parameters — so
the useful figure for a real pipeline sits somewhere between these two, closer
to parity than to 1.9×.

And it remains 11.6% of one stage. End-to-end detection time is unchanged —
see below.

## Environment

See `environment.txt`, regenerated by `bench.sh`.
