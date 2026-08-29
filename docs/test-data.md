# Test data

No slide images are committed to this repository — the smallest useful one is
larger than the entire source tree. Fetch them into `data/`, which is
gitignored.

## OpenSlide freely-distributable test data

The primary source. Slides scanned on a range of vendor scanners, redistributable
without registration.

- Index: <https://openslide.cs.cmu.edu/download/openslide-testdata/>
- Viewer, to preview before downloading: <https://openslide.org/demo/>

Vendor directories available: `Aperio`, `Generic-TIFF`, `Hamamatsu`,
`Hamamatsu-vms`, `Leica`, `Mirax`, `Olympus`, `Philips-TIFF`, `Trestle`,
`Ventana`, `Zeiss`, `DICOM`.

Sizes verified 2026-08-09:

| File | Size | Use |
| --- | ---: | --- |
| `Aperio/CMU-1-Small-Region.svs` | 1,938,955 B (1.9 MB) | Unit tests and CI. Small enough to fetch on every run. |
| `Aperio/CMU-1.svs` | 177,552,579 B (177.6 MB) | The default benchmark slide. Plain JPEG compression. |
| `Aperio/CMU-1-JP2K-33005.svs` | 132,565,343 B (132.6 MB) | Same slide, JPEG 2000 compressed — the case needing OpenJPEG/OpenSlide FFI. |

Exact byte counts, checked against `Content-Length` on the server, so a truncated
download is caught by `stat -c%s` rather than by a confusing decode failure later.

```bash
mkdir -p data
base=https://openslide.cs.cmu.edu/download/openslide-testdata/Aperio
curl -L -o data/CMU-1-Small-Region.svs "$base/CMU-1-Small-Region.svs"
curl -L -o data/CMU-1.svs             "$base/CMU-1.svs"
```

Start with `CMU-1-Small-Region.svs`. The 170 MB slide is only needed once
benchmarks move beyond the object layer to tile reading.

## Larger cohorts

Needed only for whole-pipeline benchmarking, where per-slide variation matters.
Both require accepting terms; neither is needed for anything currently in this
repository.

- **CAMELYON16/17** — <https://camelyon17.grand-challenge.org/Data/>.
  Breast cancer lymph node WSIs with lesion annotations. Individual slides are
  downloadable; the full set is ~3 TB. Registration required.
- **TCGA**, via the GDC portal — <https://portal.gdc.cancer.gov/>.
  Diagnostic slide images across many tumour types. Open-access tier needs no
  authorisation for the imaging data.

## Checking that they were actually used

Tests that need a slide skip where it is absent, and a skipped test reports `ok`
like a passing one. So a run in a checkout with no slides is green and means
much less than it looks.

    QUPATH_RUST_REQUIRE_DATA=1 cargo test --workspace --release --no-fail-fast

turns every such skip into a failure naming the file it wanted. Run it once
after fetching, to confirm the slides landed where the tests look for them —
`data/` at the repository root, under the names above.

## Licensing

The OpenSlide test data is released under
[CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/), declared per file
in the directory index (and machine-readable in `index.json`). No attribution or
registration is required.

CAMELYON is CC0. Check TCGA's data-use terms before redistributing anything
derived from it.
