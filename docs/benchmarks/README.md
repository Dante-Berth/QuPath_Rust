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

## Rank filter (MIN/MAX) — the Rust port is SLOWER

The profile puts `RankFilters.getSideMax` and `getAreaMax` at 11.6% of
cell-detection self time, so a port looked worthwhile. Measured, it is not:

| Radius | ImageJ | Rust (this port) | |
| --- | ---: | ---: | --- |
| 2.0 | 1.32 ms | 9.95 ms | **7.6× slower** |
| 5.0 | 1.49 ms | 53.09 ms | **35.6× slower** |

512×512 `f32`, MAX. ImageJ: median of 50 timed iterations after 20 warmup, JVM
startup excluded. Rust: criterion median, `target-cpu=x86-64-v3`. Raw output in
`BenchRankFilter.txt` and `rust-rank.txt`.

### Why

Two reasons, and the algorithmic one dominates:

- **ImageJ uses a running min/max; this port does not.** As the kernel slides
  one column, ImageJ updates the extremum incrementally instead of rescanning —
  O(1) per pixel against O(kernel area). The scaling shows it plainly: when the
  kernel grows 4.2× in area (radius 2 → 5, 21 → 89 pixels), ImageJ slows by
  1.13× while this port slows by 5.34×, tracking the area exactly.
- **ImageJ's `RankFilters` is multithreaded.** This port is single-threaded.

This is §0 of the guidelines demonstrated from the wrong side: the speedup comes
from doing less work, and Rust does not supply it. A naive O(area) filter in
Rust loses to a smarter O(1) algorithm in Java, by a lot.

### Status

**The port is kept for its parity value, not its speed**, and is not on any hot
path. Its 42 recorded cases pin ImageJ's kernel geometry exactly — including the
two radius bands ImageJ nudges (`[1.5, 1.75)` → 1.75, `[2.5, 2.85)` → 2.85) and
the `r² + 1` cutoff that makes radius 1 a full 3×3 square. That is the hard part
of the algorithm and it is now captured in fixtures, so a running-extremum
rewrite can be checked against it rather than against a fresh reading of ImageJ.

Replacing this with a van Herk/Gil-Werman running extremum is the obvious next
step for this filter. Until that is measured to beat ImageJ, **no speedup should
be claimed for rank filtering**, and this section stands as the record of it.

## Environment

See `environment.txt`, regenerated by `bench.sh`.
