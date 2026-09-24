#!/usr/bin/env bash
# Record the pyramid QuPath gets from OpenSlide, for `Slide::levels` to be
# checked against.
#
# This is the recorder `docs/dev/VIEWER.md` §8.8 asks for. The stored image metadata
# block replaces what QuPath would otherwise have read — `setMetadata` is called
# with it when the image is opened — so a level table that disagrees with
# OpenSlide's is QuPath reading the wrong pixels, silently. Nothing could say
# whether the two agreed until this ran.
#
# Separate from record.sh because it needs `qupath-extension-openslide` on the
# classpath, which drags in qupath-gui-fx and JavaFX. The native library is *not*
# a system package: the extension's `io.github.qupath:openslide` dependency
# carries its own `libopenslide.so` and JNA extracts it from the jar. `ldconfig`
# having no libopenslide says nothing about whether this works, which is the
# mistake that kept this entry marked blocked.
#
# Needs the slides in data/, which are untracked — see docs/test-data.md — and
# a JDK 25 (QuPath 0.8's toolchain); set JAVA_HOME to one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Both slides, because one of them answers a question the other cannot. CMU-1
# has three levels and the 256-pixel tile that is also OpenSlide's default;
# CMU-1-Small-Region has one level and a tile of 240, which is the only reason
# "the tile sizes agree" is a measurement rather than two defaults matching.
if [[ $# -gt 0 ]]; then
    slides=("$@")
else
    slides=("$root/data/CMU-1.svs" "$root/data/CMU-1-Small-Region.svs")
fi

for path in "${slides[@]}"; do
    if [[ ! -f "$path" ]]; then
        echo "error: $path not found" >&2
        echo "       slides are untracked; see docs/test-data.md" >&2
        exit 1
    fi
done

modules=(qupath-core qupath-extension-openslide)
# shellcheck source=parity/classpath.sh
source "$root/parity/classpath.sh"

fixtures="$root/parity/fixtures"
mkdir -p "$fixtures"

for path in "${slides[@]}"; do
    name="$(basename "$path" .svs)"
    out="$fixtures/OpenSlideLevels-$name.json"
    echo "==> recording $name" >&2

    # Written to a temporary file first: a recorder that fails must not leave
    # the committed fixture truncated. OpenSlide's loader logs to stdout as well
    # as stderr, so everything before the first line that is exactly "{" is
    # dropped — the same treatment record-project.sh gives QuPath's own logging.
    recorded="$(mktemp)"
    "$java_bin" -cp "$cp." "$root/parity/java/OpenSlideLevels.java" "$path" \
        | sed -n '/^{$/,$p' >"$recorded"

    if [[ ! -s "$recorded" ]]; then
        echo "error: recorder produced no JSON for $name" >&2
        rm -f "$recorded"
        exit 1
    fi

    # The fixture carries no path, only the file name — but the library version
    # is in it on purpose, so that a level table recorded against a different
    # OpenSlide is visible in the diff rather than silently replacing this one.
    if grep -qF "$HOME" "$recorded"; then
        echo "error: a home directory survived into the fixture" >&2
        rm -f "$recorded"
        exit 1
    fi

    mv "$recorded" "$out"
    echo "           $(wc -c <"$out") bytes in fixtures/$(basename "$out")" >&2
done
