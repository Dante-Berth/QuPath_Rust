# Shared JDK check and QuPath classpath resolution.
# Sourced by record.sh and bench.sh; sets `java_bin` and `cp`.
#
# Set `modules` before sourcing to pull in more than qupath-core, e.g.
#   modules=(qupath-core qupath-core-processing)
# qupath-core-processing carries the ImageJ-backed detection code
# (WatershedCellDetection and friends), which qupath-core alone does not.
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

modules=("${modules[@]:-qupath-core}")

echo "==> building ${modules[*]}" >&2
jar_tasks=()
for m in "${modules[@]}"; do jar_tasks+=(":$m:jar"); done
(cd "$qupath" && ./gradlew -q "${jar_tasks[@]}")

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
cp_tasks=()
for m in "${modules[@]}"; do cp_tasks+=(":$m:printParityCp"); done
cp="$(cd "$qupath" && ./gradlew -q -I "$init_script" "${cp_tasks[@]}" 2>/dev/null \
    | sed -n 's/^CPSTART://p' | paste -sd: -)"
if [[ -z "$cp" ]]; then
    echo "error: could not resolve runtime classpath for ${modules[*]}" >&2
    exit 1
fi

# Each module's own jar sits outside its runtimeClasspath, so add them too.
for m in "${modules[@]}"; do
    libs="$qupath/$m/build/libs"
    [[ -d "$libs" ]] || { echo "error: $libs not found after build" >&2; exit 1; }
    cp="$cp:$(find "$libs" -name '*.jar' | tr '\n' ':')"
done
