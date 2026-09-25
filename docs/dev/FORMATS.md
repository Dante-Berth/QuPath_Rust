# Opening more whole-slide formats

What it would take for `qupath-slide` to open formats other than baseline-JPEG
Aperio SVS, and in what order to do them. Written 2026-09-25 against QuPath
0.8 (the `qupath/` checkout), the OpenSlide 4.0.1 that QuPath loads from its
jar, and the OpenSlide test-data index as of that date.

**Anything marked *unverified* was read, not run.** Everything else was either
run here or read from the source named beside it.

## What the reader does today

`qupath-slide` is a classic-TIFF (not BigTIFF) directory walker, about 370
lines in `tiff.rs`, plus a 80-line abbreviated-JPEG splicer in `jpeg.rs` on top
of `zune-jpeg`. It keeps every directory that has tile offsets as a pyramid
level, reads 8-bit chunky pixels only, and refuses JPEG 2000 by name. Anything
else about a format — XML metadata, sparse tiles, overlaps, bounds, a second
file — does not exist in it yet.

Three shared capabilities recur below, so they are costed once here:

| Capability | Needed by | Estimate |
| --- | --- | --- |
| **BigTIFF** (magic 43, 8-byte offsets, 20-byte entries, types 16/17/18) | Philips, Ventana, Leica, DICOM dual-personality files | ~120 lines in `tiff.rs`; testable on any BigTIFF sample |
| **Directory selection by metadata**, not "every tiled directory" (`NewSubfileType` 254, vendor XML) | generic TIFF, Philips, Leica, Ventana | ~50 lines each |
| **Region offset** (OpenSlide `openslide.bounds-*`, which QuPath crops to by default, `OpenslideServerBuilder.updateArgs` / `OpenSlideOptions.doCropBoundingBox`) | MRXS, Leica, Ventana, NDPI when bounds are present | ~40 lines, but it moves every coordinate — a factor-of-zero conversion until a slide with bounds is tested |

## Which QuPath reader wins

QuPath asks every builder for a support level and takes the highest (read from
`OpenslideServerBuilder.supportLevel` and `BioFormatsServerBuilder.supportLevel`):

- OpenSlide: **4** for DICOM; **3.5** for an RGB file whose largest image
  occurs once; 2.5 or 1 otherwise; 0 when `detect_vendor` returns nothing.
- Bio-Formats: **5** for OME-TIFF (by `<OME ` in the description or the
  `.ome.tif[f]` suffix); 3 for any other TIFF or known format; 0 for `.mrxs`.

So for every vendor format below, QuPath opens it with **OpenSlide**, and the
existing `record-openslide-levels.sh` / a pixel recorder built the same way
records exactly what QuPath shows. OME-TIFF is the exception: QuPath reads it
with **Bio-Formats**, which `record-slide.sh` already puts on the classpath.
The per-format winner below follows from those numbers and the vendor
detection; it was run only for Aperio (`ProjectOpen.json` records
`OpenslideImageServer` for both SVS files) and for the JP2K experiment below.

A correction to how the current pinning is often described: `SlideRegion.json`
(the five pixel regions) is recorded by `SlideRegion.java` through
`ImageServerProvider` with only `qupath-extension-bioformats` on the classpath
(`record-slide.sh`), so it pins **Bio-Formats'** decode. What is pinned against
OpenSlide is the level table (`OpenSlideLevels-*.json`) and the level-0 tissue
block in `ProjectOpen.json`.

## Per format

Sizes and licences are from `openslide-testdata/index.json`, fetched
2026-09-25. "CC0" is CC0-1.0; "distributable" is the index's own word for
files whose terms are not CC0, and those terms were not checked (*unverified*).

### Aperio SVS, JPEG 2000 — `CMU-1-JP2K-33005.svs` (already in `data/`)

- **Container:** the same tiled classic TIFF the reader already parses;
  compression 33005 (JPEG 2000 with the irreversible colour transform, decoded
  to RGB) or 33003 (YCbCr components, no MCT, possibly 4:2:2 — the
  application converts). Each tile is a raw J2K codestream. OpenSlide decodes
  with OpenJPEG. QuPath: OpenSlide.
- **Reuse:** all of the container. `Slide::open` already agrees with OpenSlide
  on this file's level table (46000x32893, 11500x8223, 2875x2055, tile 240) —
  run here, see below.
- **New:** a codec call in `read_tile_into`, ~60 lines; 33003 additionally
  needs a YCbCr→RGB step and chroma upsampling (~60 lines, *unverified*, no
  CC0 sample: `JP2K-33003-1.svs`, 63.8 MB, is "distributable").
- **Codec options, measured** (details in the next section): `hayro-jpeg2000`
  (pure Rust, `forbid(unsafe_code)`) **fails** parity; `jpeg2k` with its
  `openjp2` backend (a pure-Rust translation of OpenJPEG) is **byte-exact**
  against OpenSlide at level 0.
- **Test data:** present. `CMU-1-JP2K-33005.svs`, 132.6 MB, CC0.
- **Parity:** recordable with the existing classpath; done here as an
  experiment.

### Generic tiled / pyramidal TIFF — `Generic-TIFF/CMU-1.tiff`

- **Container:** tiled TIFF; OpenSlide's `generic-tiff` driver takes the first
  directory as level 0 and any other tiled directory with the reduced-resolution
  bit of `NewSubfileType` set as a level; it is the fallback when no vendor
  matches, has no associated images, and since 4.0.1 renders zero-length tiles
  as transparent. Pyramids stored in `SubIFDs` are not followed (*unverified* —
  the format page does not say either way). QuPath: OpenSlide (3.5 vs 3).
- **Reuse:** nearly all of it. **New:** filter levels by `NewSubfileType`
  (today every tiled directory counts), treat a zero byte count as a missing
  tile, and whatever this file's compression and photometric turn out to be
  (*unverified*; the index says only "conversion of CMU-1.svs to pyramidal tiled
  TIFF"). If it is classic TIFF with JPEG, ~50 lines; with BigTIFF, +120.
- **Test data:** `Generic-TIFF/CMU-1.tiff`, 204.1 MB (204,117,846 B), CC0.
  Being the same scan as `CMU-1.svs`, its tissue coordinates are already known.
- **Parity:** `detect_vendor` should return `generic-tiff`, so both existing
  OpenSlide recorders apply unchanged (*unverified* until the file is here).
- **Why it was not done here:** the download was refused in this environment
  (`curl` is not permitted to this agent), so nothing could be verified.

### Hamamatsu NDPI

- **Container:** TIFF-like but not valid TIFF: high 32 bits of offsets stored
  after each directory for files over 4 GB, `RowsPerStrip` equal to the image
  height, private tags 65420–65459. Level 0 is typically **one** JPEG for the
  whole plane, width or height possibly 0 in the SOF when over 65535, made
  randomly accessible by restart markers whose offsets are in tag 65426 (low)
  and 65432 (high). Lower levels may lack restart markers. Macro image is the
  directory with tag 65421 = −1. QuPath: OpenSlide.
- **Reuse:** directory walking (with the high-offset extension) and the JPEG
  decoder. **New:** a synthetic "tile" per restart interval — copy the header,
  patch the SOF dimensions, append one MCU-row span, renumber RST markers —
  which is what OpenSlide does; ~400–600 lines, with the tile boundary being a
  restart interval rather than a TIFF tile. `zune-jpeg` has to accept the
  spliced stream (*unverified*).
- **Test data:** `Hamamatsu/CMU-1.ndpi`, 198.0 MB, CC0 ("small scan with valid
  JPEG headers"); `Hamamatsu-2.ndpi`, 203.1 MB, CC0 (2026 scanner).
- **Parity:** OpenSlide recorders apply unchanged (*unverified*).

### 3DHISTECH MRXS

- **Container:** a `.mrxs` file plus a directory of `Slidedat.ini`,
  `Index.dat` and `Data*.dat`. Images (JPEG, PNG or BMP) are packed into the
  data files; the index is a linked list of pages per level. Tiles sit at
  recorded camera positions and **overlap** between photos; higher levels are
  2x2 concatenations whose embedded overlaps can be non-integral. Version 2.2+
  adds DEFLATE and UTF-16LE headers. QuPath: OpenSlide (Bio-Formats returns 0
  for `.mrxs`), cropped to `openslide.bounds-*` by default.
- **Reuse:** the JPEG decoder only. **New:** INI and binary index parsers,
  positioned (not gridded) tile compositing with overlap, PNG/BMP, bounds;
  ~1000+ lines. The compositing is where OpenSlide's exact blending would have
  to be matched.
- **Test data:** `Mirax/CMU-1-Saved-1_16.zip`, **3.8 MB**, CC0 — the smallest
  real slide in the index, but "multiple positions per image", i.e. the hard
  case; `CMU-1.zip`, 565.1 MB, CC0.
- **Parity:** OpenSlide recorders apply, after unzipping (*unverified*).

### Philips TIFF

- **Container:** tiled **BigTIFF** (all four samples); XML (`DataObject`,
  `DPUfsImport`) in the first `ImageDescription`, detected by `Software`
  starting with "Philips". Sparse tiles have offset and byte count 0 and are
  white when downsampled. Level dimensions include tile padding and are not
  proportional to the downsamples, which come from `DICOM_PIXEL_SPACING` in the
  XML; mpp likewise. QuPath: OpenSlide.
- **Reuse:** directory walking and JPEG once BigTIFF exists. **New:** BigTIFF
  (+120), an XML reader for a handful of keys (a dependency such as
  `quick-xml`, or ~100 lines), sparse tiles, and downsamples that do not come
  from widths — which contradicts `Level::downsample`'s current definition and
  the viewer's `parity_openslide_levels.rs` comment that both sides compute it
  from widths. ~350 lines.
- **Test data:** `Philips-4.tiff`, 291.0 MB, CC0 (sparse); `Philips-1.tiff`,
  326.6 MB, CC0.
- **Parity:** OpenSlide recorders apply (*unverified*).

### Ventana / Roche BIF

- **Container:** tiled BigTIFF; XML in the XMP tag with an `iScan` element;
  macro and thumbnail directories named in `ImageDescription`; tile overlaps
  described per "joint" that OpenSlide resolves when compositing; ICC profile
  on level 0. QuPath: OpenSlide.
- **Reuse:** JPEG and directory walking after BigTIFF. **New:** BigTIFF, XMP
  XML, overlap-aware tile placement (not a grid), ~500 lines.
- **Test data:** `Ventana/Ventana-1.bif`, 227.4 MB, CC0 (DP 200); the `OS-*`
  files are 2.7–3.9 GB and "distributable".
- **Parity:** OpenSlide recorders apply (*unverified*).

### Leica SCN

- **Container:** tiled BigTIFF; XML collection in `ImageDescription` naming
  a macro image and one or more main images, each with its own pyramid of
  directories; OpenSlide drops the macro from the pyramid, rejects fluorescence
  and slides whose main images differ in resolution, and exposes bounds.
  QuPath: OpenSlide.
- **Reuse:** as Philips. **New:** BigTIFF, XML, multi-image layout into one
  level-0 coordinate space, bounds; ~400 lines.
- **Test data:** no CC0 sample. `Leica-1.scn`, 291.8 MB, "distributable";
  the rest are 2.3–3.0 GB. A CC0 Leica GT450 slide exists only as DICOM
  (`DICOM/Leica-4.zip`).
- **Parity:** OpenSlide recorders apply (*unverified*).

### OME-TIFF, brightfield RGB

- **Container:** (Big)TIFF with OME-XML in the first `ImageDescription`;
  pyramids in **SubIFDs** (OME-TIFF 6) rather than the main directory chain;
  compression varies (JPEG, LZW, deflate, uncompressed are all common).
  QuPath: **Bio-Formats** (support 5).
- **Reuse:** directory walking and JPEG. **New:** SubIFD traversal (~60 lines),
  BigTIFF, at least one of LZW/deflate (pure Rust exists: `weezl`, `flate2`
  with `miniz_oxide`), OME-XML for size/mpp. ~300 lines.
- **Test data:** OpenSlide's index has none. OME publishes samples, but none
  was found that is a brightfield RGB pyramid with a stated licence
  (*unverified*).
- **Parity:** not with the OpenSlide recorders; `SlideRegion.java` (Bio-Formats)
  is the right recorder, and needs no change of classpath.

### DICOM WSI

- **Container:** a **directory** of DICOM Part 10 files sharing a Series
  Instance UID, one or more per level (4.0.1 added levels split across files);
  frames in encapsulated pixel data, TILED_FULL or TILED_SPARSE. Transfer
  syntaxes OpenSlide reads: explicit-VR uncompressed RGB, baseline JPEG (RGB or
  YBR_FULL_422), JPEG 2000 lossy (RGB, YBR_ICT, YBR_RCT) and lossless.
  OpenSlide parses with libdicom; support added in 4.0.0. QuPath: OpenSlide
  (support **4**, its highest).
- **Reuse:** the JPEG decoder; dual-personality files are also BigTIFF. **New:**
  a DICOM parser — `dicom-object` 0.10 (MIT/Apache, pure Rust) does it — frame
  indexing, multi-file assembly, sparse tiles; ~600 lines on top of the crate,
  plus JPEG 2000 for half the samples.
- **Test data:** `DICOM/Leica-4.zip`, 85.3 MB, CC0 (baseline JPEG, sparse) —
  the smallest CC0 JPEG DICOM; `3DHISTECH-1.zip`, 361.5 MB, CC0;
  `CMU-1-JP2K-33005-v2.zip`, 127.3 MB, CC0, the same pixels as the JP2K SVS
  already here.
- **Parity:** OpenSlide recorders apply, if the jar's library was built with
  libdicom (*unverified* — no DICOM slide was run).

## The JPEG 2000 experiment

Run here on 2026-09-25, not committed (a temporary branch of `read_tile_into`
and a scratch recorder; both reverted). The recorder built QuPath's
`OpenslideImageServer` through `OpenslideServerBuilder`, exactly as
`OpenSlideLevels.java` does, and dumped `readRegion` pixels at the level
downsample, as `SlideRegion.java` does. OpenSlide 4.0.1, vendor `aperio`.
Regions are on tissue (OpenSlide means 142–155, 28–36 % of samples below 128),
one crosses a 240-pixel tile corner in both axes.

| Decoder | Level 0, 1024x1024 tissue (3.1 M samples) | Level 0, 48x48 across a tile corner | Level 1, 64x64 | Level 2, 32x32 |
| --- | --- | --- | --- | --- |
| `hayro-jpeg2000` 0.3.5 and 0.4.0 (identical output) | worst 34, mean 4.54, 8 % exact | worst 27, mean 4.71 | worst 35, mean 5.91 | worst 30, mean 4.59 |
| `jpeg2k` 0.10.1 + `openjp2` 0.6.1 | **worst 0, 100 % exact** | **worst 0, 100 % exact** | worst 15, mean 1.69 | worst 43, mean 4.26 |

**`hayro-jpeg2000` is not a drop-in.** Its output is aligned (a ±1-pixel shift
search is best at 0,0) but softer: the mean absolute Laplacian of the green
channel is 10.2 against OpenSlide's 13.3, and the error is intensity-dependent
(where OpenSlide's value is below 100 ours is +5.3 on average; at 180 and above, −4.6). Against the existing JPEG
tolerance (worst 4, mean 0.75) it fails by an order of magnitude. Where the
loss happens was not found. Bumping 0.3.5 to 0.4.0 changed nothing, and 0.4.0
needs Rust 1.92 while the workspace declares 1.90.

**`openjp2` is byte-exact at level 0.** It is a C-to-Rust translation of
OpenJPEG, so this is the expected result, not a coincidence of tolerances. 1024²
decodes in ~0.075 s on one thread (JPEG on `CMU-1.svs`: ~0.023 s; one warm run
each, not a benchmark).

**The level 1 and 2 differences are OpenSlide's, not the codec's**, and they
apply to the existing JPEG reader too. QuPath asks OpenSlide for a level in
level-0 coordinates (`paintRegionARGB(x0, y0, level, …)`), and OpenSlide maps
them back with its own downsample, which averages the width and height ratios
and so is not the integer QuPath uses to build the request: 4.000061 at level
1 of this slide, 16.003163 at level 2. The result is a sub-pixel translation
that grows with x — 0.11 px and 0.34 px at the regions above — resampled
bilinearly. Modelling exactly that (bilinear sample of our decode at
`x0 / ds_openslide`) brings both decoders' reduced levels into line:

| Slide, decoder | Level | Shift (x, y) px | As read | Modelled |
| --- | --- | --- | --- | --- |
| JP2K, `openjp2` | 1 | 0.106, 0.023 | worst 15, mean 1.69 | worst 1, mean 0.15 |
| JP2K, `openjp2` | 2 | 0.343, 0.074 | worst 43, mean 4.26 | worst 3, mean 0.45 |
| `CMU-1.svs`, current JPEG reader | 1 | 0.211, 0.045 | worst 41, mean 4.51 | worst 2, mean 0.22 |
| `CMU-1.svs`, current JPEG reader | 2 | 0.053, 0.011 | worst 6, mean 0.95 | worst 2, mean 0.24 |

Two consequences:

1. **QuPath-through-OpenSlide shifts reduced levels by a fraction of a pixel**
   wherever a level's height ratio is not exactly its width ratio, which is
   every level of both CMU-1 files. `qupath-slide` reads level pixels at their
   integer positions. `DIVERGENCES.md` D-011 records this, and
   it means `SlideRegion.json` (recorded through Bio-Formats) would not pass
   against an OpenSlide recording at level 1.
2. **A reduced-level pixel fixture from OpenSlide cannot be compared
   byte-for-byte.** Pinning a new format at a non-zero level needs either the
   shift modelled in the test, or regions at the origin of a level where the
   shift is zero — and those are background. Level 0 can be pinned exactly.

## An OpenSlide backend behind a feature

The alternative to porting formats one at a time: link OpenSlide itself.

- **Unsafe.** FFI needs `unsafe`. The workspace sets `unsafe_code = "deny"`,
  not `forbid`, so a separate crate can keep `lints.workspace = true` and put
  `#![expect(unsafe_code, reason = "…")]` at its root; no Cargo lint override
  is needed. §9 already prescribes the shape: `openslide-sys` plus a safe
  wrapper (`M-FFI-NAMING`, `M-FFI-TRANSLATES`), with §10's `// SAFETY:`
  comments and a Miri run — Miri cannot execute foreign calls, so that rule
  would need an explicit exemption for the FFI crate (*a project decision*).
  `openslide-sys` 1.0.8 and `openslide-rs` 2.4.0 (MIT/Apache) exist on
  crates.io; neither was built here.
- **"No system dependencies".** That promise ends at link time: OpenSlide is a
  Meson build over glib, cairo, libxml2, sqlite, OpenJPEG, libtiff, libjpeg,
  libpng, zstd and libdicom (*unverified* list, from memory of OpenSlide's
  build). §9's "vendor, pin, build with `cc`, never a Makefile" is not
  practical for that tree. The realistic variant is **runtime loading** of a
  prebuilt self-contained library — the same kind QuPath extracts from
  `io.github.qupath:openslide` — via `libloading`, so the build stays pure Rust
  and the feature degrades to "not available" at run time. That still needs
  `unsafe`, and it moves the promise from "no system dependencies" to "no
  build-time dependencies, one optional runtime library".
- **Licence.** OpenSlide is LGPL-2.1; dynamically loading it from a
  GPL-3.0-or-later program is compatible. Vendoring and statically linking is
  also allowed but brings the LGPL relinking obligations (*not legal advice;
  unverified*).
- **What it buys:** every format in this page at once, with pixels that match
  QuPath by construction — including the sub-pixel shift above, which would
  then be reproduced rather than diverged from.
- **What it costs:** the one feature this crate has that QuPath does not —
  a slide reader with nothing to install — for users who enable it, and a
  second pixel path whose tests only run where the library exists.

## Recommended order

1. **Aperio JPEG 2000 via `jpeg2k` + `openjp2`.** The sample is already in
   `data/`, the container is already parsed and its level table already
   matches OpenSlide, and level 0 is byte-exact on 3.1 M tissue samples. It is
   ~60 lines. What is left before it can land: (a) the reduced-level fixture —
   record regions through OpenSlide and model the shift in the test, which also
   lets the existing JPEG path be pinned against OpenSlide at level 1 for the
   first time; (b) accepting `openjp2` — a machine translation of C, with at
   least 482 source lines containing `unsafe` in its top-level `src/*.rs`, no declared
   MSRV and BSD-2 licensing — which keeps this crate's own code safe but is
   not "memory-safe" in any sense `hayro-jpeg2000` is; (c) the fixtures in
   `qupath-viewer` that record JP2K as undecodable (`ProjectOpen.json`
   `pixelsComparable`, `ProjectImport.json` `thumbnailReadable`) would need
   re-recording by whoever owns them. Only 33005 is covered; 33003 stays
   refused until a sample can be tested.
2. **Generic pyramidal TIFF** (`Generic-TIFF/CMU-1.tiff`, 204 MB, CC0). The
   cheapest *new* container: `NewSubfileType` selection and missing tiles, with
   tissue coordinates already known from CMU-1. Blocked here only by the
   download.
3. **BigTIFF**, then **Philips TIFF** (`Philips-4.tiff`, 291 MB, CC0). BigTIFF
   is a prerequisite for Philips, Ventana, Leica and dual-personality DICOM,
   and Philips is the simplest of those — but its downsamples do not come from
   widths, which `Level` has to learn first.
4. **Ventana BIF** (`Ventana-1.bif`, 227 MB, CC0): BigTIFF plus overlap-aware
   placement.
5. **NDPI** (`CMU-1.ndpi`, 198 MB, CC0): self-contained but a different tiling
   model (restart intervals) and JPEG surgery; worth it because Hamamatsu is
   common.
6. **DICOM** (`Leica-4.zip`, 85 MB, CC0), with `dicom-object`; after JPEG 2000,
   because three of the seven current CC0 DICOM samples are JPEG 2000.
7. **OME-TIFF** — pinned against Bio-Formats, not OpenSlide, and no suitable
   sample was found.
8. **Leica SCN** — no CC0 sample.
9. **MRXS** — the largest port (its own container and overlap compositing),
   despite the smallest sample.

The OpenSlide backend is the answer if the goal becomes "open everything
QuPath opens" rather than "open the common formats with no install". It is not
an alternative to items 1 and 2, which are small and keep the promise.
