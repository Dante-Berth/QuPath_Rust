#!/usr/bin/env bash
# Record QuPath's cell count over several regions, for comparison against the
# Rust pipeline's object count on the same regions.
#
# Usage: ./parity/record-cell-counts.sh [slide]
#
# Needs JDK 25+ and a slide — see parity/README.md and docs/test-data.md. Writes
# parity/fixtures/CellCounts.json.
#
# Separate from record.sh for the reason record-slide.sh is: this needs a slide
# and the bioformats extension, and record.sh truncates its target fixture
# before running a recorder, so a recorder that cannot start there empties a
# committed file.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
slide="${1:-$root/data/CMU-1.svs}"

if [[ ! -f "$slide" ]]; then
    echo "error: slide not found: $slide" >&2
    echo "       see docs/test-data.md for how to fetch it" >&2
    exit 1
fi

modules=(qupath-core qupath-core-processing qupath-extension-bioformats)
# shellcheck source=parity/classpath.sh
source "$root/parity/classpath.sh"

classes="$root/parity/build"
mkdir -p "$classes"
"${java_bin%java}javac" -nowarn -cp "$cp" -d "$classes" \
    "$root/parity/java/qupath/imagej/detect/cells/RecordCellCounts.java"

# The regions, and why these. The first is the one docs/profiles/README.md
# profiled and every published count refers to, so it anchors the comparison
# against a number already in the documentation.
#
# The rest are **chosen by the slide, not by hand**, from
# `cargo run --release --example tissue_scan -p qupath-slide`, which ranks
# non-overlapping candidates by mean optical density on the coarsest level.
# Picking coordinates by eye is how a sweep ends up measuring glass: the first
# attempt at this file guessed six regions and four of them came back with 0,
# 15, 37 and 790 cells, which says nothing about parity because there is
# nothing there to disagree about.
#
# The last entry is deliberately small enough to be a single tile. It has no
# seams, so the gap there isolates everything that is *not* seam resolution --
# which is the control D-008 needs, since D-008 predicts the gap comes from
# resolving overlaps on nuclei rather than on cells.
regions=(
    "27776,5952,4000,4000,profiled-99pc-tissue"
    "8000,16000,4000,4000,dense-2"
    "36000,4000,4000,4000,dense-3"
    "16000,20000,4000,4000,dense-4"
    "12000,26000,4000,4000,dense-5"
    "40000,8000,4000,4000,dense-6"
    "30000,12000,4000,4000,dense-7"
    "28000,6000,2048,2048,single-tile-no-seams"
    "8000,16000,2048,2048,single-tile-no-seams-2"
    "16000,20000,2048,2048,single-tile-no-seams-3"
)

out="$root/parity/fixtures/CellCounts.json"
echo "==> recording QuPath cell counts over ${#regions[@]} regions" >&2
# The recorder writes the file itself rather than printing it: QuPath and
# Bio-Formats log through SLF4J onto stdout and neither the simpleLogger nor the
# logback property silences it here, so a redirect captures JSON with a stack
# trace in front of it.
"$java_bin" -cp "$cp$classes" qupath.imagej.detect.cells.RecordCellCounts \
    "$out" "$slide" "${regions[@]}"

echo "==> wrote $out" >&2
