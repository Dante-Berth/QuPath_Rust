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
# So the crops come from **both** — the region that disagrees and the two that
# agree. A recording made only where the two diverge cannot say whether the
# stage is wrong or the input is unusual, which is the same reason the browser's
# hover fill needed three layouts rather than one.
#
# The regions are taken whole rather than sampled within, which also removes the
# choice of where to sample. Picking the clumpiest-looking crop by eye is how
# the first cell-count sweep ended up measuring glass (see
# record-cell-counts.sh), and here it would additionally be picking the crop
# most likely to show the effect being looked for.
#
# **The crop is the whole region, at production size**, and the 320 px grid it
# replaces is why.
#
# D-009 measures the density-dependent gap over 2048x2048 regions -- 1.0121 and
# 1.0177 on the dense ones against 1.1130 on the sparse one. The 320 px crops
# did not reproduce that: their ratios came back at 1.058 and 1.079 on the first
# rung, near enough identical, where the regions they were drawn from differ by
# eleven points. A fixture that cannot see the effect cannot localise it, and
# every number taken from those crops carried that caveat.
#
# Two things caused it. The background estimate is computed over the field it is
# given, and at 320 px that field is an order of magnitude smaller than the tile
# QuPath actually processes, so the stage most suspected of carrying the gap was
# being measured in a regime it never runs in. And the crops were small enough
# to be dominated by where they happened to land: `sparse-0` came back with one
# seed and zero objects -- it is glass, and it contributed nothing but a
# division by zero to every ratio computed from it.
#
# 2048 is what `Params::tile_preferred` is derived from and what
# `record-cell-counts.sh` and `record-object-masks.sh` already use for these
# same three regions. A region no larger than `MAX_TILE_SIZE` is not tiled at
# all, so this is one whole tile with no seam and no halo -- the same thing
# D-009's end-to-end numbers were measured on, which is what makes the rungs
# comparable to them.
crops=()
crops+=("16000,20000,2048,2048,sparse")
crops+=("8000,16000,2048,2048,dense-2")
crops+=("28000,6000,2048,2048,dense")

out="$root/parity/fixtures/MergeAndSplitTile.json"
echo "==> recording the merge and the split over ${#crops[@]} crops" >&2
# The recorder writes the file itself rather than printing it, for
# record-cell-counts.sh's reason: QuPath and Bio-Formats log through SLF4J onto
# stdout and a redirect captures JSON with a stack trace in front of it.
"$java_bin" -cp "$cp$classes" MergeAndSplitTile "$out" "$slide" "${crops[@]}"

echo "==> wrote $out" >&2
