#!/usr/bin/env bash
# Record reference output from a real QuPath build into parity/fixtures/.
#
# Requires JDK 25 (QuPath 0.8's toolchain); set JAVA_HOME to one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Recorders reach into qupath-core-processing for the ImageJ-backed algorithms
# (Watershed and friends), not just qupath-core.
modules=(qupath-core qupath-core-processing)
# shellcheck source=parity/classpath.sh
source "$root/parity/classpath.sh"

fixtures="$root/parity/fixtures"
mkdir -p "$fixtures"

# Bench* are timing harnesses driven by bench.sh, not fixture recorders.
# SlideRegion needs qupath-extension-bioformats and a slide, which is why it has
# its own script; running it here truncated its committed fixture and then
# aborted under `set -e`, taking the recorders after it with it.
# ProjectImport is excluded for the same reason and did the same thing: its
# input is a project directory written by `cargo run --example project_sample`,
# which nothing here does, so it fell back to a relative default that does not
# exist, emptied a committed fixture and took the run down. See
# record-project.sh.
for src in "$root"/parity/java/*.java; do
    case "$(basename "$src")" in Bench* | SlideRegion.java | ProjectImport.java) continue ;; esac
    name="$(basename "$src" .java)"
    # GeoJsonImport reads a file this project wrote rather than computing one,
    # so it takes the sample's path. Passed absolutely, because the recorder is
    # run from wherever the caller happens to be and a relative default would
    # empty the fixture rather than fail loudly.
    args=()
    if [[ "$name" == GeoJsonImport ]]; then
        sample="$root/parity/samples/detections.geojson"
        if [[ ! -f "$sample" ]]; then
            echo "error: $sample not found; regenerate it with" >&2
            echo "       cargo run -p qupath-viewer --example geojson_sample" >&2
            exit 1
        fi
        args=("$sample")
    fi
    echo "==> recording $name" >&2
    # Recorders write JSON to stdout, and so does some of QuPath's own logging:
    # anything that touches GeometryTools prints two `Setting -Djts.*` lines
    # there, not to stderr. A fixture with that preamble is not JSON and carries
    # a clock, so it produces a diff on every recording. Everything before the
    # first line that is exactly "{" is dropped, as record-slide.sh does.
    "$java_bin" -cp "$cp." "$src" "${args[@]}" | sed -n '/^{$/,$p' >"$fixtures/$name.json"

    if [[ ! -s "$fixtures/$name.json" ]]; then
        echo "error: $name produced no JSON" >&2
        exit 1
    fi
done

echo "==> done; fixtures in parity/fixtures/" >&2
