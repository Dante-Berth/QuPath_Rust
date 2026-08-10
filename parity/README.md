# Parity harness

Records reference output from a real QuPath build, so Rust tests can be checked
against QuPath's actual behaviour rather than against our reading of its source.

Generated fixtures land in `fixtures/` as JSON and **are committed** — that is
what makes the Rust tests runnable without a JVM, and what makes a behavioural
change in either implementation show up as a diff.

## Regenerating

Needs JDK 25+ and a built `qupath-core`. From the repository root:

```bash
./parity/record.sh
```

That records everything that needs no slide. The two recorders that do — the
slide reader's own fixture, and the colour-deconvolution tile — need
`qupath-extension-bioformats` on the classpath as well, and have their own
script:

```bash
./parity/record-slide.sh            # defaults to data/CMU-1.svs
```

`record.sh` is enough when `java` on `PATH` is already 25 or newer. Otherwise
point `JAVA_HOME` at a JDK 25 installation, which takes precedence:

```bash
JAVA_HOME=/path/to/jdk-25 ./parity/record.sh
```

Gradle cannot auto-provision the toolchain here — Gradle itself needs a JVM to
start — so the JDK must be installed before the first run. On Debian/Ubuntu,
`sudo apt install openjdk-25-jdk` makes it the default `java`.

The script builds `qupath-core` if needed, resolves its runtime classpath from
Gradle, runs each recorder in `java/`, and writes `fixtures/<name>.json`.

A recorder that needs a slide must be excluded from `record.sh` and driven from
`record-slide.sh` instead. `record.sh` truncates its target fixture before
invoking a recorder, so running one there that cannot start does not merely
fail — it empties a committed fixture and, under `set -e`, takes every recorder
after it down too.

## Tolerances

Exact equality with QuPath does not hold in general: Java dropped `strictfp`
guarantees in 17, and Rust's libm differs from Java's `Math` in the last ULP.
Parity tests compare with documented tolerances — relative `1e-5` for scalar
measurements, Dice or IoU `>= 0.99` for masks and segmentations — except where
a fixture explicitly records bit-exact behaviour, as `measurement_precision`
does.

Never widen a tolerance to make a test pass. If parity cannot be reached,
report the numbers.
