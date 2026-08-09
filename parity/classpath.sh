# Shared JDK check and qupath-core classpath resolution.
# Sourced by record.sh and bench.sh; sets `java_bin` and `cp`.
#
# shellcheck shell=bash

qupath="$root/qupath"

if [[ ! -d "$qupath" ]]; then
    echo "error: $qupath not found (upstream QuPath checkout is untracked)" >&2
    exit 1
fi

java_bin="${JAVA_HOME:+$JAVA_HOME/bin/java}"
java_bin="${java_bin:-java}"
java_version="$("$java_bin" -version 2>&1 | head -1)"
if ! grep -qE '"(2[5-9]|[3-9][0-9])' <<<"$java_version"; then
    echo "error: need JDK 25+, got: $java_version" >&2
    echo "       set JAVA_HOME to a JDK 25 installation" >&2
    echo "       note: the Gradle wrapper cannot auto-provision it, because" >&2
    echo "       Gradle itself needs JVM 17+ to start." >&2
    exit 1
fi

echo "==> building qupath-core" >&2
(cd "$qupath" && ./gradlew -q :qupath-core:jar)

echo "==> resolving runtime classpath" >&2
init_script="$(mktemp)"
trap 'rm -f "$init_script"' EXIT
cat >"$init_script" <<'GRADLE'
allprojects {
  tasks.register("printParityCp") {
    doLast {
      def c = configurations.findByName("runtimeClasspath")
      if (c) println("CPSTART:" + c.files.join(":"))
    }
  }
}
GRADLE
cp="$(cd "$qupath" && ./gradlew -q -I "$init_script" :qupath-core:printParityCp 2>/dev/null \
    | sed -n 's/^CPSTART://p')"
if [[ -z "$cp" ]]; then
    echo "error: could not resolve qupath-core runtime classpath" >&2
    exit 1
fi
cp="$cp:$(find "$qupath/qupath-core/build/libs" -name '*.jar' | tr '\n' ':')"
