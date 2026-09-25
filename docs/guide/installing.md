# Installing and building

There are no binary releases. This is built from source.

## What you need

| Requirement | Version | Why |
| --- | --- | --- |
| Rust toolchain | **1.90 or newer** | `rust-version` in `Cargo.toml`. The project is developed and CI-gated against **1.96.0**. |
| A C toolchain | any | Some dependencies build C. |
| X11 or Wayland | — | Only for the windowed application. The command-line tools need no display. |

Nothing else. There is **no Java, no QuPath install and no OpenSlide** needed to
run this: the slide reader is pure Rust. QuPath itself is needed only to
*record new parity fixtures*, which is a developer task — see
[`CONTRIBUTING.md`](../../CONTRIBUTING.md).

### System libraries for the window

The windowed application pulls `winit`, `softbuffer` and `rfd`'s XDG-portal
backend. On a bare Debian/Ubuntu machine:

```bash
sudo apt-get install -y libxkbcommon-dev libwayland-dev pkg-config
```

`rfd` is built with `default-features = false` and the XDG-portal backend on
purpose, so **GTK is not required**. File dialogs go through the desktop
portal.

## Build

```bash
git clone https://github.com/Dante-Berth/QuPath_Rust
cd QuPath_Rust
cargo build --release
```

**Use `--release`.** A debug build decodes JPEG tiles slowly enough that panning
is unpleasant and detection timings are meaningless.

## Get a slide

No slide images are committed — the smallest useful one is larger than the
entire source tree. They are fetched into `data/`, which is gitignored.

```bash
mkdir -p data
base=https://openslide.cs.cmu.edu/download/openslide-testdata/Aperio
curl -L -o data/CMU-1-Small-Region.svs "$base/CMU-1-Small-Region.svs"
curl -L -o data/CMU-1.svs              "$base/CMU-1.svs"
```

| File | Size | Use |
| --- | ---: | --- |
| `CMU-1-Small-Region.svs` | 1.9 MB | Quick check; what CI runs on. |
| `CMU-1.svs` | 177.6 MB | The slide every number in this documentation was measured on. |

Verify the download by size rather than trusting it — a truncated file fails
later as a confusing decode error:

```bash
stat -c%s data/CMU-1.svs     # expect 177552579
```

[`docs/test-data.md`](../test-data.md) has the full list, other vendors, and
the checksums.

## Check it works

```bash
cargo run --release -p qupath-slide --example info -- data/CMU-1.svs
```

On CMU-1 this prints the pyramid and a timed read:

```text
data/CMU-1.svs
level           size        tiles       tile  downsample
    0    46000x32914      180x129    256x256        1.0x
    1     11500x8228        45x33    256x256        4.0x
    2      2875x2057         12x9    256x256       16.0x

level 0: Aperio Image Library v10.0.51
46920x33014 [0,100 46000x32914] (256x256) JPEG/RGB Q=30

region 27776,5952 4000x4000 at level 0: 0.118 s (405.1 MB/s decoded)
```

If that prints, the reader works. Now open the window:

```bash
cargo run --release -p qupath-app -- data/CMU-1.svs
```

## Formats that open

| Format | Status |
| --- | --- |
| Aperio SVS, plain JPEG | **Supported.** Checked against OpenSlide over 5 regions, the level table and a tissue block. |
| Tiled TIFF | **Supported.** |
| Aperio SVS, JPEG 2000 (`CMU-1-JP2K-33005.svs`) | **Not supported.** Needs an OpenJPEG binding this does not have. |
| Hamamatsu, Leica, Mirax, Zeiss, DICOM | **Not supported.** |

A slide that will not open reports why in the window rather than exiting.

## No GPU? It still runs

The viewer composes on the GPU when there is an adapter and in software when
there is not — including the entire interface, which is tessellated to
triangles and rasterised by the same path. On a machine with no driver you get
the same window, not a fallback. To force it:

```bash
cargo run --release -p qupath-viewer --example window -- data/CMU-1.svs --cpu
```

## Running the tests

```bash
cargo test --workspace --no-fail-fast
```

Tests needing a slide **skip** when it is absent — 14 want the 1.9 MB one and
4 the 177 MB one — and a skipped test in Rust reports `ok`. To make the skips visible:

```bash
QUPATH_RUST_REQUIRE_DATA=1 cargo test --workspace --no-fail-fast
```

`--no-fail-fast` is not optional here: without it the run stops at the first
failure and the summary names only a prefix of what was skipped.
