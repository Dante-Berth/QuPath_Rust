#!/usr/bin/env bash
# Record what QuPath makes of a project directory this project wrote.
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

modules=(qupath-core)
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

echo "==> done; $(wc -c <"$fixtures/ProjectImport.json") bytes in fixtures/ProjectImport.json" >&2
