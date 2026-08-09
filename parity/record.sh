#!/usr/bin/env bash
# Record reference output from a real QuPath build into parity/fixtures/.
#
# Requires JDK 25 (QuPath 0.8's toolchain); set JAVA_HOME to one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=parity/classpath.sh
source "$root/parity/classpath.sh"

fixtures="$root/parity/fixtures"
mkdir -p "$fixtures"

# Bench* are timing harnesses driven by bench.sh, not fixture recorders.
for src in "$root"/parity/java/*.java; do
    case "$(basename "$src")" in Bench*) continue ;; esac
    name="$(basename "$src" .java)"
    echo "==> recording $name" >&2
    # Recorders write JSON to stdout; SLF4J and JVM warnings go to stderr.
    "$java_bin" -cp "$cp." "$src" >"$fixtures/$name.json"
done

echo "==> done; fixtures in parity/fixtures/" >&2
