#!/usr/bin/env bash
# Run the QuPath (Java) side of the benchmark comparison.
#
# The Rust side is `cargo bench -p qupath-objects`. Both must run on the same
# machine, at the same N and F, for a comparison to mean anything. Results land
# in docs/benchmarks/ and are committed alongside any performance claim.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Rank filtering lives in ImageJ, reached through qupath-core-processing.
modules=(qupath-core qupath-core-processing)
# shellcheck source=parity/classpath.sh
source "$root/parity/classpath.sh"

out="$root/docs/benchmarks"
mkdir -p "$out"

for src in "$root"/parity/java/Bench*.java; do
    name="$(basename "$src" .java)"
    echo "==> benchmarking $name (JVM warmed, startup excluded)" >&2
    # Older recorders emit JSON, newer ones plain text; keep each in its own
    # file and let the extension follow the content.
    case "$name" in
        BenchRankFilter) ext=txt ;;
        *) ext=json ;;
    esac
    "$java_bin" -cp "$cp." "$src" >"$out/$name.$ext"
    cat "$out/$name.$ext" >&2
done

{
    echo "machine: $(uname -sr)"
    echo "cpu: $(grep -m1 '^model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ //')"
    echo "jdk: $("$java_bin" -version 2>&1 | head -1)"
    echo "recorded: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >"$out/environment.txt"

echo "==> done; results in docs/benchmarks/" >&2
