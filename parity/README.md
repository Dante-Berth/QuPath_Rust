# Parity harness

Records reference output from a real QuPath build, so Rust tests can be checked
against QuPath's actual behaviour rather than against our reading of its source.

Generated fixtures land in `fixtures/` as JSON and **are committed** — that is
what makes the Rust tests runnable without a JVM, and what makes a behavioural
change in either implementation show up as a diff.

## Regenerating

Needs JDK 25 and a built `qupath-core`. From the repository root:

```bash
export JAVA_HOME=/path/to/jdk-25
./parity/record.sh
```

The script builds `qupath-core` if needed, resolves its runtime classpath from
Gradle, runs each recorder in `java/`, and writes `fixtures/<name>.json`.

## Tolerances

Exact equality with QuPath does not hold in general: Java dropped `strictfp`
guarantees in 17, and Rust's libm differs from Java's `Math` in the last ULP.
Parity tests compare with documented tolerances — relative `1e-5` for scalar
measurements, Dice or IoU `>= 0.99` for masks and segmentations — except where
a fixture explicitly records bit-exact behaviour, as `measurement_precision`
does.

Never widen a tolerance to make a test pass. If parity cannot be reached,
report the numbers.
