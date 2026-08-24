#!/usr/bin/env bash
# Record QuPath's merge and shape split on real tissue, at the point in
# WatershedCellDetection where they run.
#
# Usage: ./parity/record-merge-tiles.sh [slide]
#
# Needs JDK 25+ and a slide — see parity/README.md and docs/test-data.md. Writes
# parity/fixtures/MergeAndSplitTile.json.
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
    "$root/parity/java/MergeAndSplitTile.java"

# The crops, and why these.
#
# DIVERGENCES.md D-009 measures three regions small enough to be a single tile,
# so no seam resolution happens in them and D-008 cannot apply. Two agree with
# QuPath to within 1.2-1.8%; the third is out by 11.3%. The merge and the split
# are the two stages that move together across that difference.
#
# So the crops come from **both** — four from the region that disagrees and four
# from one that agrees. A recording made only where the two diverge cannot say
# whether the stage is wrong or the input is unusual, which is the same reason
# the browser's hover fill needed three layouts rather than one.
#
# Within each region the offsets are a fixed grid rather than a choice. Picking
# the clumpiest-looking crop by eye is how the first cell-count sweep ended up
# measuring glass (see record-cell-counts.sh), and here it would additionally be
# picking the crop most likely to show the effect being looked for.
#
# 320 full-resolution pixels comes back as ~162 at the detection downsample of
# ~1.98, which is ~80 microns: around ten nuclei across, enough for the dilation
# to bridge several into one blob and for the split to have real work to do.
crops=()
for spec in "16000,20000,sparse" "8000,16000,dense"; do
    IFS=, read -r rx ry label <<<"$spec"
    i=0
    for dy in 384 1152; do
        for dx in 384 1152; do
            crops+=("$((rx + dx)),$((ry + dy)),320,320,${label}-${i}")
            i=$((i + 1))
        done
    done
done

out="$root/parity/fixtures/MergeAndSplitTile.json"
echo "==> recording the merge and the split over ${#crops[@]} crops" >&2
# The recorder writes the file itself rather than printing it, for
# record-cell-counts.sh's reason: QuPath and Bio-Formats log through SLF4J onto
# stdout and a redirect captures JSON with a stack trace in front of it.
"$java_bin" -cp "$cp$classes" MergeAndSplitTile "$out" "$slide" "${crops[@]}"

echo "==> wrote $out" >&2
