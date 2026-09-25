# Viewing a slide

```bash
cargo run --release -p qupath-app -- data/CMU-1.svs
```

Paths on the command line open at startup. Without one the window comes up
empty and asks for a slide. A `.qpproj` opens as a project (see
[Projects](projects.md)), and the two can be given together in either order —
which is the ordinary way to start work:

```bash
cargo run --release -p qupath-app -- study/project.qpproj data/CMU-1.svs
```

## Mouse and keyboard

This table is the one built into the application, under **View → Mouse and
keyboard help**.

| Gesture | Effect |
| --- | --- |
| **Left-drag** | Pan. The view stays on the slide. |
| **Click** | Select the outline under the pointer; its values are in the Objects pane, under View. Click empty slide to clear. |
| **Right-drag** | Draw a box; releasing zooms to it. |
| **Escape** | Abandon a box mid-drag, or stop a detection run. |
| **Scroll** | Zoom about the cursor. Two fingers on a trackpad. |
| **Ctrl-scroll** | Zoom faster; this is what a pinch sends. |
| **+ / −** | Zoom about the cursor. |
| **f** | Fit the whole slide. |
| **d** | Detect cells in what is on screen. |
| **h** | Show or hide the outlines. |
| **s** | Save what was found as GeoJSON. |
| **o** | Open a slide. |
| **p** | Open a QuPath project and list its images. |

Two things that surprise people:

- **The toolbar zoom buttons work about the middle of the slide area**, while
  the wheel and the `+`/`−` keys work about the cursor. A button press says
  nothing about where you are looking; a keypress with the mouse over tissue
  does.
- **The view is held on the slide.** You cannot pan the tissue off the edge of
  the window and lose it.

## The menus

**File**

| Item | |
| --- | --- |
| Open slide… | `o` |
| Open recent | Slides opened before. |
| New project… | Creates a QuPath project directory. |
| Open project… | `p` |
| Save detections… | `s`. Disabled until a run has found something. |

**View** — all checkboxes, because these are states rather than actions:

| Item | Shows |
| --- | --- |
| Analysis | The detection controls and results. |
| Slide panel | The metadata pane, which carries the two below. |
| Overview map | A thumbnail of the whole slide; click it to jump. |
| Objects | The selected detection's values, and colouring the outlines by a measurement — see below. It opens by itself when you click an outline. |
| Brightness and contrast | The display transform — see below. |
| Mouse and keyboard help | The table above. |

## The status bar

The scalebar is **snapped to a round number**, so it changes length as you zoom
rather than changing what it says. Beside it sit the cursor's slide coordinates
and the current pyramid level.

## Selecting a detection, and colouring by a measurement

After a run (`d`), **click an outline** to select it. It turns cyan and three
pixels wide, and the **Objects** pane opens with that object's values: its
number in the run, its centroid (in pixels, and in µm when the slide records a
pixel size) and every measurement column the run carries. A click that moves
more than a few pixels is a pan, not a selection. Clicking empty slide clears
it, and a new run or a closed slide drops it — the row it named belongs to
another object then.

Under **Colour outlines by**, pick a column to colour every outline by it.
The colours are QuPath's default map (Inferno, dark to pale yellow), checked
against colours recorded from QuPath itself. The legend gives the range, which
is the **2nd to 98th percentile** of the values rather than QuPath's full
minimum to maximum, so one outlier does not turn every other object the same
colour; objects beyond either end are drawn in the end colours, and the legend
counts them. An object with no value is grey. *Nothing (plain)* goes back to
the default outlines, which is also where it starts.

**Whose numbers these are.** QuPath's per-cell measurements are not ported.
The two columns a run has today are this project's:

| Column | What it is |
| --- | --- |
| `Nucleus: Area (px)` | Pixels in the nucleus mask, in full-resolution (level 0) pixels. Not QuPath's calibrated nucleus area. |
| `Nucleus: Haematoxylin OD mean` | Mean background-subtracted haematoxylin optical density over the nucleus — the value the detection threshold tests. Not QuPath's `Hematoxylin OD mean`. |

Neither, nor the centroid, has been compared with a QuPath measurement, and
the pane says so under each value. Hover a value for the details.

## Brightness, contrast and stains

Open the **slide panel**, and under the overview are three channel buttons and
two sliders.

| Control | Does |
| --- | --- |
| **Original** | The slide's own colours. |
| **Haematoxylin** | The haematoxylin stain pulled out on its own — nuclei, with nearly everything else dropped. |
| **Eosin** | The other half: stroma and cytoplasm, with the nuclei faint. |
| **Black / white sliders** | Which pixel values count as fully black and fully white. On a stain channel, how much of the stain counts as fully dark. |

Two properties worth knowing:

- **None of this touches detection.** The transform is applied on the way from
  the tile cache to the frame; the cache holds the slide's own pixels, and
  nothing the detector is handed has been through it. Moving a slider costs a
  redraw, not a reload, and cannot change an object count.
- **The stain vectors are read from the slide, not typed in.** A slide carrying
  its own stain matrix gets its own numbers.

Dragging black past white pushes white along rather than inverting, as QuPath's
dialog does.

## Pyramid levels

The viewer picks the level whose downsample best matches the zoom, loads tiles
on a background thread, and keeps drawing the coarser level underneath until
the finer tiles land — so zooming never shows a blank.

Zooming out should *drop* the resident tile count, never raise it. If you want
to watch that happen, `examples/window` puts the backend, level, tile count,
resident texture memory and per-frame cost in the title bar:

```bash
cargo run --release -p qupath-viewer --example window -- data/CMU-1.svs
```

The optional second argument is the texture budget in MB, and it is the one
knob worth sweeping: set it low and the frame still draws, because the coarser
underlay covers whatever was evicted.

## When a slide will not open

The window says so and stays open. It does not exit. The commonest causes are
a format that is not supported — JPEG 2000 SVS, Hamamatsu, Mirax — and a
truncated download; see [Installing](installing.md#formats-that-open).
