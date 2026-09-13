# Exporting to QuPath

Detections are saved as **GeoJSON**, which QuPath imports directly.

## Saving

Run a detection, then press **s** — or **File → Save detections…**. The menu
item is disabled until a run has found something, so a live item that does
nothing when pressed cannot happen.

## Opening it in QuPath

In QuPath: **File → Import objects…**, and pick the `.geojson` file. The
objects arrive as detections on whichever image is open.

They land in the right place only if the image is the slide they were found
on — see [Coordinates](#coordinates) below.

## What the file contains

A `FeatureCollection` of polygons, each with two measurements:

```json
{"type":"Feature",
 "geometry":{"type":"Polygon","coordinates":[[[10,20],[16,20],[16,26],[10,26],[10,20]]]},
 "properties":{"objectType":"detection",
               "measurements":{"Nucleus: Area (px)":36,
                               "Nucleus: Haematoxylin OD mean":0.35}}}
```

The shape follows QuPath's `QuPathTypeAdapters` rather than the GeoJSON
specification, and the distinction matters: GeoJSON is published, so writing
something a validator accepts takes no effort and still lands in QuPath showing
nothing.

- `objectType` is the camelCase spelling QuPath's reader wants. It warns on the
  older `object_type`.
- `measurements` is the post-0.4.0 name-to-number object, not the older array
  of `{name, value}` pairs.

The two measurement columns are QuPath's own — these are exactly what QuPath's
own export carries for a detection.

## Coordinates

**Every coordinate is in level-0 pixels**, whatever level the run actually
read.

This is the part that is wrong in a way that looks right if it is got wrong. A
file written in the read level's coordinates opens without complaint and puts
every object in the top-left corner at the wrong size — plausible geometry, in
the wrong frame.

The same applies one level in, to the area. `Nucleus: Area (px)` comes out of
the pipeline in the pixels of the level that was read, so a run at level 1 left
unscaled would write an area sixteen times too small next to a polygon of the
right size — a number of the right kind, in the wrong units, agreeing with
nothing. The area is therefore scaled by the square of the downsample. The mean
is an optical density and is left alone.

**"px" in an exported file means level-0 pixels throughout.** One frame per
file.

### Why µm² is not written

QuPath's calibrated area column has a name of its own. Guessing at that name is
how a file looks right and reads wrong, so the pixel column is written and the
calibrated one is not. Convert with the slide's `MPP` if you need microns —
`examples/info` prints it.

## What is not exported

**Measurements beyond those two.** QuPath's full measurement tail is not
implemented; see [Parity](parity.md).

**Cells.** What is exported are nuclei, with raw pixel boundaries. Cell
expansion and boundary smoothing are ported but not wired into the pipeline, so
there is no cell membrane to write and the outlines are not smoothed. See
[Detecting cells](detecting.md#parameters-qupath-has-that-are-not-here).

**`data.qpdata`.** Detections are never written into a project's binary object
store — see [Projects](projects.md#what-is-deliberately-not-written). GeoJSON
alongside the project is the supported route.

## Checking a file without QuPath

```bash
cargo run --release -p qupath-viewer --example geojson_sample
```

Writes a small sample to `parity/samples/detections.geojson` with known
geometry, which is useful for confirming an import path end to end before
spending a real run on it.
