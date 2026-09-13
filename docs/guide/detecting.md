# Detecting cells

This is a port of QuPath's `WatershedCellDetection`. Read
[Parity and what to trust](parity.md) before using the numbers it produces.

## Running it

Open a slide, frame the tissue you want, and press **d** — or **Analysis →
Detect cells**.

The run happens on another thread, so the window keeps panning and zooming
while it works. A progress bar shows the fraction of **tiles** finished, not
time: tiles over glass are far cheaper than tiles over tissue, so no finish
estimate is offered beside it. **Escape** stops a run.

When it lands, the outlines are drawn over the slide. **h** hides and shows
them. **s** saves them — see [Exporting to QuPath](exporting.md).

Rough costs on CMU-1:

| View | Objects | Time |
| --- | ---: | ---: |
| A 1:1 screenful of tissue | 1,357 | 0.38 s |
| The whole slide, at level 0 | 382,432 | 33.5 s |

## Which pyramid level a run reads

**Not necessarily the one you are looking at**, and this is the single most
important thing on this page.

The viewer picks a level to *look* at, matched to the zoom. Detection picks a
level to *measure* at. Those are different questions the moment you zoom out.

The rule is: **the coarsest level whose pixels are still 1.0 µm or finer**,
never finer than what the viewer is already showing. On CMU-1, whose levels are
0.499, 2.0 and 8.0 µm/px, only level 0 clears that — so a zoomed-out run reads
level 0 over the view's rectangle, rather than the level 2 it is drawing.

Why it matters: the parameters are in microns and the pipeline converts them
with whatever level's pixel size it reads, so a coarse run is *arithmetically*
consistent. What no conversion can do is put back evidence the downsampling
threw away. The smallest surviving object is 10 µm², a nucleus about 3.6 µm
across — under four pixels at CMU-1's level 2, and a quarter of one pixel at
level 3. Below that scale the detector is not finding nuclei badly, it is
finding *something else*: clumps of stained tissue whose size and contrast at
that downsample happen to resemble a nucleus.

That failure is invisible unless you go looking, because it arrives as a
plausible object count. The measurement that exposed it: a fitted whole-slide
view at level 2 reported **1,638** objects where one screenful of tissue at
level 0 reported **1,357** — and the whole slide holds far more nuclei than one
screenful does.

**A slide recording no pixel size gets the viewer's level unchanged.** Without
`MPP` there is no way to know what any level resolves, and guessing the physical
scale is how you get a confident wrong answer.

Objects found above level 0 are also *coarse*: outlines traced in that level's
pixels and scaled up.

## The parameters

**Analysis → detection parameters.** Units are **microns**, as QuPath's dialog
uses, and each slider shows the pixel value it converts to on this slide —
because an area divides by the *square* of the pixel size where a length
divides by it once.

| Parameter | QuPath default | What it does |
| --- | ---: | --- |
| **Background radius** | 8 µm | Radius of the background estimate. The background is an opening by reconstruction, *subtracted* from the optical density — a top hat, so everything smaller than the kernel survives at full contrast and everything larger is flattened away. Zero skips it. |
| **Sigma** | 1.5 µm | Gaussian blur before the Laplacian. Larger merges nearby nuclei; smaller splits noise into objects. |
| **Threshold** | 0.1 | Mean optical density a candidate must **exceed** to survive, taken after background subtraction and before the blur. Dimensionless — the same number at any pixel size. A candidate landing exactly on it does not survive. |
| **Max background intensity** | 2.0 | Background level above which a region is masked out of the estimate. On H&E this is darker than tissue gets except on folds and ink. |
| **Minimum area** | 10 µm² | Smallest surviving object. Zero disables. |
| **Maximum area** | 400 µm² | Largest surviving object. Zero disables. |

On CMU-1 at 0.4990 µm/px those become 16.03 px, 3.01 px, 40.16 px² and
1606.42 px².

**Changing a parameter does not re-run detection.** A run is about a second and
a slider produces a value every frame, so re-running on change would start
dozens of runs across one drag. `Detect` stays the deliberate act, as it is in
QuPath's dialog.

What the controls do, measured on the profiled 4000×4000 region:

| Setting | Objects |
| --- | ---: |
| Defaults | 1,272 |
| Minimum area 60 µm² | 23 |
| Threshold 0.30 | 851 |
| Sigma 3 µm | 852 |

### Parameters QuPath has that are not here

A control for a parameter the pipeline ignores is worse than no control — it
reads as a knob that does nothing. These are therefore absent from the panel
rather than present and inert:

| QuPath parameter | Status here |
| --- | --- |
| Median filter radius | **Not implemented.** |
| Cell expansion | Ported in `qupath-watershed`, **not called by the pipeline**. |
| Smooth boundaries | Ported in `qupath-watershed`, **not called by the pipeline**. |
| Split by shape | Always on; not a parameter here. |

The consequence of the last three: **what you get are nuclei, not cells**, with
raw pixel boundaries rather than QuPath's smoothed ones.

## What the pipeline does, stage by stage

Every stage below is pinned against output recorded from a real QuPath build,
running on QuPath's own tile grid.

1. **Colour deconvolution** — RGB to haematoxylin/eosin optical density.
2. **Background subtraction** by morphological reconstruction (the top hat).
3. **Gaussian blur** at sigma.
4. **Laplacian of Gaussian** — a nucleus is a positive peak, its surroundings
   negative, so the boundary falls at zero rather than at a tuned constant.
5. **Regional maxima** — one marker per nucleus, 8-connected.
6. **Labelling** of those markers.
7. **Watershed flood**, 4-connected, thresholded at zero.
8. **Filled-region tracing** to candidate objects.
9. **Mean-intensity filter** — the *Threshold* parameter.
10. **Merge** across tile boundaries.
11. **Shape split** — the Euclidean distance map and `MaximumFinder`.
12. **`refineBoundary`**.
13. **Area filter** — *Minimum* and *Maximum area*.
14. **Tile-seam deduplication**.

Counts through those stages, on the profiled region:

```text
  intensity filter     80762 ->   51152  (-36.7%)
  merge                51152 ->   26397  (-48.4%)
  split and refine     26397 ->   45990  (+74.2%)
  area filter          45990 ->   28359  (-38.3%)
  tile seams           28359 ->   26647  (-6.0%)
```

The split stage *raising* the count by 74% is not a bug — it is what splitting
touching nuclei apart does.

## Reproducing that without a window

```bash
cargo run --release -p qupath-pipeline --example region
```

Prints the per-stage timing, the stage-by-stage counts above, and peak memory.
