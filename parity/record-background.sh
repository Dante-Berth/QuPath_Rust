#!/usr/bin/env bash
# Record QuPath's background estimate and the subtraction after it, on real
# tissue, as an input image and an output image.
#
# Usage: ./parity/record-background.sh [slide]
#
# Needs JDK 25+ and a slide — see parity/README.md and docs/test-data.md. Writes
# parity/fixtures/BackgroundEstimate.json.
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
    "$root/parity/java/BackgroundEstimate.java"

# The crops, and why these.
#
# This is a **stage parity fixture**: the same input goes into both sides and
# the outputs are compared. It does not need to reproduce the density regime
# D-008 and D-009 measure, because it is not measuring a regime — a
# disagreement on any real tissue input is a disagreement. That is what makes
# small crops correct here where they were not correct for the stage ladder,
# which *was* measuring a regime and lost it at 320 px.
#
# Detection runs at ~1.0 downsample on this slide -- 0.499 µm/px against a
# requested 0.5 -- so a crop comes back at very nearly its full-resolution size.
# The first version of this script asked for 320 and produced a 12 MB fixture;
# 160 gives 159x159, which is ten background radii across. Two crops from tissue
# that
# disagrees end to end, two from tissue that agrees, and one deliberately on
# glass, for coverage of an empty field. `maxBackground` fires where the eroded
# optical density is still *above* the limit -- dense stain, a fold, ink -- not
# where the image is bright, since optical density runs the other way. The two
# crops after them exist for that branch; see the comment beside them.
crops=(
    "17152,20384,160,160,2.0,sparse-a"
    "17152,21152,160,160,2.0,sparse-b"
    "8384,17152,160,160,2.0,dense-a"
    "9152,16384,160,160,2.0,dense-b"
    "2000,2000,160,160,2.0,glass"
    # The same two tissue crops with the limit lowered until the mask fires --
    # a different limit each, found by recording and looking rather than
    # guessed: the erosion takes a minimum, so what decides is the darkest
    # *gap* in a crop and not how much tissue it holds. 0.15 fires on sparse-a
    # and not on dense-a. At
    # QuPath's default of 2.0 it fires nowhere on this slide -- D-009 found that
    # by checking directly -- so at the default these five record one branch of
    # a two-branch stage. The limit is carried per crop and the test applies the
    # recorded one, so this tests the branch rather than a different stage.
    "17152,20384,160,160,0.15,sparse-a-masked"
    "8384,17152,160,160,0.05,dense-a-masked"
)

out="$root/parity/fixtures/BackgroundEstimate.json"
echo "==> recording the background estimate over ${#crops[@]} crops" >&2
# The recorder writes the file itself rather than printing it, for
# record-cell-counts.sh's reason: QuPath and Bio-Formats log through SLF4J onto
# stdout and a redirect captures JSON with a stack trace in front of it.
"$java_bin" -cp "$cp$classes" BackgroundEstimate "$out" "$slide" "${crops[@]}"

echo "==> wrote $out" >&2
