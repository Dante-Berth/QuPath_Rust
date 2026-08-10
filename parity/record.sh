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
for src in "$root"/parity/java/*.java; do
    case "$(basename "$src")" in Bench* | SlideRegion.java) continue ;; esac
    name="$(basename "$src" .java)"
    echo "==> recording $name" >&2
    # Recorders write JSON to stdout; SLF4J and JVM warnings go to stderr.
    "$java_bin" -cp "$cp." "$src" >"$fixtures/$name.json"
done

echo "==> done; fixtures in parity/fixtures/" >&2
