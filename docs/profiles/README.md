# Profiles

Required by §3 of the project guidelines: no subsystem is ported before a
profile of the equivalent QuPath path exists, with the top three frames by self
time named.

Regenerate with:

```bash
./profile/run.sh                                          # defaults below
./profile/run.sh data/CMU-1.svs 27776 5952 4000 4000 2 5  # explicit region
```

## CMU-1, watershed cell detection

**Workload:** `WatershedCellDetection.CellDetector.runDetection` over a
4000×4000 full-resolution region of `CMU-1.svs` at (27776, 5952), brightfield
H&E, default parameters. That region was chosen by scanning the lowest pyramid
level for tissue density — it is 99% tissue. Detection is called directly rather
than through the plugin's task machinery, so thread scheduling is not in the
profile.

**Result:** 25,757 cells, 15.87 s mean over 5 timed iterations (2 warmup
discarded), range 15.71–15.98 s. The cell count is identical across all
iterations and asserted by the harness — a run that got fast by detecting
nothing fails rather than producing a misleading flame graph.

JFR at `settings=profile` with `-XX:+DebugNonSafepoints`, 10,759 execution
samples.

### Top frames by self time

| Samples | Share | Frame |
| ---: | ---: | --- |
| 2258 | 21.0% | `java.util.PriorityQueue.siftDownComparable` |
| 1003 | 9.3% | `ij.plugin.filter.RankFilters.getSideMax` |
| 902 | 8.4% | `qupath.imagej.processing.Watershed$PixelWithValue.compareTo` |
| 652 | 6.1% | `ij.process.FloatProcessor.getf` |
| 550 | 5.1% | `qupath.imagej.processing.MorphologicalReconstruction.dilateAndCompare` |
| 334 | 3.1% | `ij.plugin.filter.Convolver.convolveFloat` |
| 254 | 2.4% | `ij.plugin.filter.GaussianBlur.convolveLine` |
| 245 | 2.3% | `ij.plugin.filter.RankFilters.getAreaMax` |

Full list in `CMU-1-cells-hot.txt`; raw recording in `CMU-1-cells.jfr`.

### Reading

**The top three frames are the watershed priority queue, rank filtering, and
the queue's comparator.** `siftDownComparable` plus `PixelWithValue.compareTo`
is **29.4% of self time in one data structure** — a `PriorityQueue<PixelWithValue>`
of boxed objects, each holding a pixel coordinate and a value, compared through
an interface call. That is a pointer-chasing heap doing per-pixel work, and it
is the single largest cost in cell detection.

This is the same class of problem as the measurement layer, and the same fix
applies: the queue's contents are a fixed-size set of (value, index) pairs that
belong in a flat array. A radix or bucket sort over quantised values, or a
hierarchical queue keyed on the 8-bit EDM height, removes both the boxing and
the comparator entirely. Expected gain here is large and is a **layout** gain,
not a language gain — §0's ordering, not "Rust is faster".

**Rank filtering and convolution together are ~17%** — `getSideMax`,
`getAreaMax`, `convolveFloat`, `convolveLine`. These are dense per-pixel stencil
operations over `float[]`, exactly the shape §3 rates at 2–5× and up to 15% with
SIMD. Straightforward to port, well-defined, easy to test differentially.

**Decode is not the bottleneck.** §3 warns that if the profile shows time in
JPEG/JPEG2000 decode, the answer is a codec or pyramid-level decision rather
than a Rust port. Measured here, frames under `loci.formats` / `imageio` /
`AWTImageTools` are **2.1%** of samples. For this workload — one region, read
once, then heavy processing — decode is not where the time goes, and a port is a
defensible answer. That conclusion is specific to this shape: a whole-slide run
that streams thousands of tiles and does less work per tile would shift the
balance toward decode, and should be profiled separately before anything is
concluded about it.

**Measurement computation is 5.3%** by whole-stack attribution. The columnar
table in `crates/qupath-objects` targets storage and query, which this profile
shows is not a dominant cost inside detection itself. Its ~100× benchmark stands
on its own terms, but this profile does not show it moving end-to-end detection
time much — the win would come from porting the frames above.

### Caveats

- **Single-threaded.** `runDetection` is called directly on one region, so this
  is per-tile cost, not a parallel whole-slide run. QuPath parallelises across
  tiles; that changes throughput but not the relative shape of this profile.
- **One region, one slide, one stain.** Tissue density drives the watershed
  cost, and this region is deliberately dense (99%). A sparser region shifts
  weight toward filtering and away from the queue: an earlier run on a mostly
  background region at (20000, 15000) detected 37 cells in 4.0 s and had
  `RankFilters.getAreaMax` on top, with the priority queue absent from the top
  frames entirely.
- **Self time attribution only.** The per-area percentages that would need
  whole-stack attribution are unreliable in the presence of shared utility
  classes — `PriorityQueue` appears under several callers — so only the self-time
  table above is used for conclusions.
