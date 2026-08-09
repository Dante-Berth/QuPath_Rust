#!/usr/bin/env bash
# Record reference output from a real QuPath build into parity/fixtures/.
#
# Requires JDK 25 (QuPath 0.8's toolchain). Note that the Gradle wrapper cannot
# auto-provision it: Gradle itself needs JVM 17+ to start, so a system JDK below
# that cannot bootstrap the download. Set JAVA_HOME explicitly.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
qupath="$root/qupath"
fixtures="$root/parity/fixtures"

if [[ ! -d "$qupath" ]]; then
    echo "error: $qupath not found (upstream QuPath checkout is untracked)" >&2
    exit 1
fi

java_bin="${JAVA_HOME:+$JAVA_HOME/bin/java}"
java_bin="${java_bin:-java}"
version="$("$java_bin" -version 2>&1 | head -1)"
if ! grep -qE '"(2[5-9]|[3-9][0-9])' <<<"$version"; then
    echo "error: need JDK 25+, got: $version" >&2
    echo "       set JAVA_HOME to a JDK 25 installation" >&2
    exit 1
fi

echo "==> building qupath-core"
(cd "$qupath" && ./gradlew -q :qupath-core:jar)

echo "==> resolving runtime classpath"
init_script="$(mktemp)"
trap 'rm -f "$init_script"' EXIT
cat >"$init_script" <<'EOF'
allprojects {
  tasks.register("printParityCp") {
    doLast {
      def c = configurations.findByName("runtimeClasspath")
      if (c) println("CPSTART:" + c.files.join(":"))
    }
  }
}
EOF
cp="$(cd "$qupath" && ./gradlew -q -I "$init_script" :qupath-core:printParityCp 2>/dev/null \
    | sed -n 's/^CPSTART://p')"
if [[ -z "$cp" ]]; then
    echo "error: could not resolve qupath-core runtime classpath" >&2
    exit 1
fi
cp="$cp:$(find "$qupath/qupath-core/build/libs" -name '*.jar' | tr '\n' ':')"

mkdir -p "$fixtures"
for src in "$root"/parity/java/*.java; do
    name="$(basename "$src" .java)"
    echo "==> recording $name"
    # Recorders write JSON to stdout; SLF4J and JVM warnings go to stderr.
    "$java_bin" -cp "$cp." "$src" >"$fixtures/$name.json"
done

echo "==> done; fixtures in parity/fixtures/"
