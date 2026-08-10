#!/usr/bin/env bash
# Record slide pixels QuPath itself decodes, into parity/fixtures/.
#
# Separate from record.sh for two reasons: reading SVS needs
# qupath-extension-bioformats on the classpath, which the other recorders do
# not, and it needs a slide in data/, which is untracked because it is 177 MB.
#
# Requires JDK 25 (QuPath 0.8's toolchain); set JAVA_HOME to one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
slide="${1:-$root/data/CMU-1.svs}"

if [[ ! -f "$slide" ]]; then
    echo "error: $slide not found" >&2
    echo "       slides are untracked; see docs/test-data.md" >&2
    exit 1
fi

modules=(qupath-core qupath-core-processing qupath-extension-bioformats)
# shellcheck source=parity/classpath.sh
source "$root/parity/classpath.sh"

fixtures="$root/parity/fixtures"
mkdir -p "$fixtures"

echo "==> recording SlideRegion from $(basename "$slide")" >&2
# QuPath's logging goes to stdout, not stderr, so the JSON has a preamble.
# Everything before the first line that is exactly "{" is that preamble.
"$java_bin" -cp "$cp." "$root/parity/java/SlideRegion.java" "$slide" \
    | sed -n '/^{$/,$p' >"$fixtures/SlideRegion.json"

if [[ ! -s "$fixtures/SlideRegion.json" ]]; then
    echo "error: recorder produced no JSON" >&2
    exit 1
fi

# Colour deconvolution of a real tile. The synthetic half of this fixture is
# recorded by record.sh into ColourDeconvolution.json and needs no slide; this
# is the tile case, which does, and so writes a separate file.
echo "==> recording ColourDeconvolutionTile from $(basename "$slide")" >&2
"$java_bin" -cp "$cp." "$root/parity/java/ColourDeconvolution.java" "$slide" \
    | sed -n '/^{$/,$p' >"$fixtures/ColourDeconvolutionTile.json"

if [[ ! -s "$fixtures/ColourDeconvolutionTile.json" ]]; then
    echo "error: tile recorder produced no JSON" >&2
    exit 1
fi

echo "==> done; $(wc -c <"$fixtures/SlideRegion.json") bytes in fixtures/SlideRegion.json" >&2
echo "        $(wc -c <"$fixtures/ColourDeconvolutionTile.json") bytes in fixtures/ColourDeconvolutionTile.json" >&2
