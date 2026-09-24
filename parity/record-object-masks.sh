#!/usr/bin/env bash
# Record the mask of every nucleus QuPath detects, for a per-object comparison
# against the Rust pipeline's masks on the same regions.
#
# Usage: ./parity/record-object-masks.sh [slide]
#
# Needs JDK 25+ and a slide -- see parity/README.md and docs/test-data.md.
# Writes parity/fixtures/ObjectMasks.json.
#
# Separate from record.sh for the reason record-cell-counts.sh is: this needs a
# slide and the bioformats extension, and record.sh truncates its target fixture
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
    "$root/parity/java/qupath/imagej/detect/cells/RecordObjectMasks.java"

# **Single-tile regions only, and that is the point.** A region that fits in one
# tile has no seam overlaps to resolve, so nothing D-008 describes can enter the
# comparison this fixture feeds: any disagreement is detection disagreeing with
# detection, not two different overlap-resolution rules disagreeing.
#
# These three are the ones docs/dev/MEMO.md §2 measures, and they were chosen by the
# slide rather than by hand -- `cargo run --release --example tissue_scan -p
# qupath-slide` ranks candidates by optical density. Their counts come back at
# 1.0121, 1.0177 and 1.1130 against this pipeline, which is the spread the
# per-object comparison has to explain: two regions that nearly agree on the
# total and one that does not.
#
# The 4000x4000 regions CellCounts.json uses are deliberately absent. At ~26,000
# nuclei each they would make a fixture an order of magnitude larger, and every
# one of them has seams.
regions=(
    "28000,6000,2048,2048,single-tile-no-seams"
    "8000,16000,2048,2048,single-tile-no-seams-2"
    "16000,20000,2048,2048,single-tile-no-seams-3"
)

out="$root/parity/fixtures/ObjectMasks.json"
echo "==> recording QuPath nucleus masks over ${#regions[@]} regions" >&2
# The recorder writes the file itself rather than printing it: QuPath and
# Bio-Formats log through SLF4J onto stdout and neither the simpleLogger nor the
# logback property silences it here, so a redirect captures JSON with a stack
# trace in front of it.
"$java_bin" -cp "$cp$classes" qupath.imagej.detect.cells.RecordObjectMasks \
    "$out" "$slide" "${regions[@]}"

echo "==> wrote $out ($(wc -c < "$out") bytes)" >&2
