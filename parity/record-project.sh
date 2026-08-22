#!/usr/bin/env bash
# Record a project going through QuPath, in both directions.
#
# ProjectImport records what QuPath's reader makes of a project directory this
# project wrote. ProjectExport records a project.qpproj QuPath *wrote*, for this
# project's reader to be checked against — a format checked only against the
# code that writes it is untested, and that direction is the one a browser
# actually meets.
#
# Separate from record.sh because the input is not committed. Every path QuPath
# stores in a project is an absolute `file:` URI, so a committed project would
# name the machine that recorded it; the directory is written fresh here, read
# back by ProjectImport, and thrown away.
#
# Needs the slides in data/, which are untracked — see docs/test-data.md — and
# a JDK 25 (QuPath 0.8's toolchain); set JAVA_HOME to one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
slide="${1:-$root/data/CMU-1-Small-Region.svs}"
second="${2:-$root/data/CMU-1-JP2K-33005.svs}"

for path in "$slide" "$second"; do
    if [[ ! -f "$path" ]]; then
        echo "error: $path not found" >&2
        echo "       slides are untracked; see docs/test-data.md" >&2
        echo "       QuPath resolves the URI in the project, so a missing slide" >&2
        echo "       records slideExists: false rather than testing the format." >&2
        exit 1
    fi
done

# A fixed directory name, because QuPath's `Project.getName()` is derived from
# the path — a temporary directory with a random name would change the fixture
# on every run for a reason that has nothing to do with the format.
study="$(mktemp -d)/study"
trap 'rm -rf "$(dirname "$study")"' EXIT

echo "==> writing a project to $study" >&2
(cd "$root" && cargo run --quiet -p qupath-viewer --example project_sample -- \
    "$study" "$slide" "$second" >/dev/null)

modules=(qupath-core qupath-extension-openslide)
# shellcheck source=parity/classpath.sh
source "$root/parity/classpath.sh"

fixtures="$root/parity/fixtures"
mkdir -p "$fixtures"

echo "==> recording ProjectImport" >&2
# Recorders write JSON to stdout; SLF4J and JVM warnings go to stderr. Written
# to a temporary file first: a recorder that fails must not leave the committed
# fixture truncated, which is the failure record.sh's own header warns about.
# QuPath's own logging goes to stdout too — `ProjectIO` prints a "Loading
# project" line there — so everything before the first line that is exactly "{"
# is dropped. A fixture with that preamble is not JSON and carries a clock,
# which is a diff on every recording.
recorded="$(mktemp)"
trap 'rm -rf "$(dirname "$study")" "$recorded"' EXIT
"$java_bin" -cp "$cp." "$root/parity/java/ProjectImport.java" "$study/project.qpproj" \
    | sed -n '/^{$/,$p' >"$recorded"

if [[ ! -s "$recorded" ]]; then
    echo "error: recorder produced no JSON" >&2
    exit 1
fi
mv "$recorded" "$fixtures/ProjectImport.json"

# ---------------------------------------------------------------------------
# The other direction: a project QuPath wrote.
#
# On a copy, and not on $study, because ProjectExport adds an entry and rewrites
# the file. Running it in place would leave ProjectImport recording whichever
# project happened to be there, which is an ordering that breaks silently the
# first time these two are reordered.
export_dir="$(dirname "$study")/exported"
cp -r "$study" "$export_dir"

echo "==> recording ProjectExport" >&2
exported="$(mktemp)"
trap 'rm -rf "$(dirname "$study")" "$recorded" "$exported"' EXIT
"$java_bin" -cp "$cp." "$root/parity/java/ProjectExport.java" \
    "$export_dir/project.qpproj" "$second" | sed -n '/^{$/,$p' >"$exported"

if [[ ! -s "$exported" ]]; then
    echo "error: ProjectExport produced no JSON" >&2
    exit 1
fi

# The fixture is the file QuPath wrote, with the two absolute paths in it
# replaced by placeholders — that is the whole edit, and it is checked below.
#
# `randomizedName` goes too, and it is worth saying why rather than lumping it
# in with the paths: QuPath's copy constructor for an entry does *not* carry it
# over, so `UUID.randomUUID()` runs again on every load and the field is a fresh
# value every recording. A fixture that changes on every run for a reason that
# has nothing to do with the format is one nobody reads the diff of.
slides_dir="$(dirname "$second")"
sed -e "s|$export_dir|/PROJECT|g" \
    -e "s|$slides_dir|/SLIDES|g" \
    -e 's|"randomizedName": "[0-9a-fA-F-]\{36\}"|"randomizedName": "randomized-name"|g' \
    "$exported" >"$exported.clean"

# Whatever the placeholders did not catch would be the recording machine, in a
# committed file, disagreeing with every other machine. A path with a character
# that percent-encodes in a URI is the way that happens, so this refuses rather
# than commits it.
for leaked in "$export_dir" "$slides_dir" "$HOME"; do
    if grep -qF "$leaked" "$exported.clean"; then
        echo "error: $leaked survived into the fixture" >&2
        echo "       a path that percent-encodes in a URI will not match the" >&2
        echo "       literal substitution; record from a path without spaces" >&2
        echo "       or other characters a file: URI escapes." >&2
        exit 1
    fi
done
if ! grep -q '"uri": "file:/PROJECT/project.qpproj"' "$exported.clean"; then
    echo "error: the project's own uri is not where it was expected" >&2
    exit 1
fi
if grep -qE '"randomizedName": "[0-9a-fA-F-]{36}"' "$exported.clean"; then
    echo "error: a randomized name survived, and would change every recording" >&2
    exit 1
fi

mv "$exported.clean" "$fixtures/ProjectExport.json"

# ---------------------------------------------------------------------------
# The pixels, which §8.8 listed as "an image opened from a project".
#
# ProjectImport above stops before them on purpose. This opens every entry
# through `entry.readImageData()` -- what double-clicking a row reaches -- and
# records what the server ended up with, including one pixel from the middle of
# level 0. That last field is the only one a metadata block cannot make agree
# with itself: a level table that is plausible and wrong opens fine and reads
# the wrong pixels, which is the failure this whole area is about.
#
# On $study rather than the export copy, because it is the project *this*
# project wrote that the claim is about.
echo "==> recording ProjectOpen" >&2
opened="$(mktemp)"
trap 'rm -rf "$(dirname "$study")" "$recorded" "$exported" "$opened"' EXIT
"$java_bin" -cp "$cp." "$root/parity/java/ProjectOpen.java" "$study/project.qpproj" \
    | sed -n '/^{$/,$p' >"$opened"

if [[ ! -s "$opened" ]]; then
    echo "error: ProjectOpen produced no JSON" >&2
    exit 1
fi
if grep -qF "$HOME" "$opened"; then
    echo "error: a home directory survived into the fixture" >&2
    exit 1
fi
mv "$opened" "$fixtures/ProjectOpen.json"

echo "==> done; $(wc -c <"$fixtures/ProjectImport.json") bytes in fixtures/ProjectImport.json" >&2
echo "           $(wc -c <"$fixtures/ProjectExport.json") bytes in fixtures/ProjectExport.json" >&2
echo "           $(wc -c <"$fixtures/ProjectOpen.json") bytes in fixtures/ProjectOpen.json" >&2
