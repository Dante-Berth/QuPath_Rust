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

## End-to-end cell counts

Every other fixture here pins one *stage* against QuPath. `CellCounts.json` pins
the number the whole pipeline exists to produce, over several regions of one
slide:

```bash
./parity/record-cell-counts.sh                       # QuPath's side, needs a JVM
cargo run --release --example cell_counts -p qupath-pipeline   # ours, and the table
```

The regions are chosen **by the slide**, from
`cargo run --release --example tissue_scan -p qupath-slide`, which ranks
non-overlapping candidates by optical density on the coarsest level. This is not
fussiness: the first version of the script guessed six regions by eye and four
came back with 0, 15, 37 and 790 cells. A region of glass cannot disagree about
anything, so a sweep full of them reports agreement it has not tested.

Three regions are deliberately small enough to be a **single tile**. They have no
seams, so their gap isolates everything that is not seam resolution — which is
what showed that `DIVERGENCES.md` D-008, the standing explanation for the gap,
does not account for most of it. A control that cannot come out differently from
the treatment is not a control; these can, and did.

Unlike the stage fixtures, this one is **not** asserted by a Rust test. There is
no tolerance at which "the cell count matches" is currently true — it is 1.065x
pooled and up to 1.129x by region — and writing a test with a 1.15x bound would
be picking a number to pass rather than to measure. It is a reported comparison
until the remaining divergence is understood, per the rule below about never
widening a tolerance.

## A recorder that runs the other way

Every fixture above records what QuPath *computes*, so a Rust test can be
checked against it. `GeoJsonImport.json` records what QuPath **reads**, from a
file this project wrote:

```bash
cargo run -p qupath-viewer --example geojson_sample   # parity/samples/detections.geojson
./parity/record.sh                                    # GeoJsonImport reads it back
cargo test -p qupath-viewer --test parity_geojson_import
```

`parity/samples/` holds the input, and it is committed for the same reason the
fixtures are: it is what makes the recorded reading mean anything. The order
matters — the recorder reads the sample, so regenerate the sample *first* and
record *second*, or the fixture describes QuPath's reading of the previous file.

`the_committed_sample_is_what_the_writer_produces` is what keeps the pair
honest. Without it a change to `geojson::write` would leave the fixture
describing a file nothing produces any more, and every other assertion in that
test would keep passing about the wrong thing.

`ProjectImport.json` runs the same way and has its own script, because its
input **cannot** be committed:

```bash
./parity/record-project.sh                          # needs the slides in data/
cargo test -p qupath-viewer --test parity_project_import
cargo test -p qupath-viewer --test parity_project_export
```

Every path QuPath stores in a project is an absolute `file:` URI, so a
committed project would name the machine that recorded it. The script writes
one to a temporary directory, records QuPath's reading and deletes it; what
ties the fixture to the code is `examples/shared/project.rs`, which the writer
and the test both read the entries from.

Both recorders answer a question the other fixtures cannot: whether QuPath can
read what this project writes. The project one came back with four reasons it
could not.

## And one that records QuPath's writing

`ProjectExport.json` is the third direction, and the one a browser actually
meets: a `project.qpproj` **QuPath wrote**, for this project's reader to be
checked against. Recorded by the same script, because it needs the same input —
it loads the sample project, adds an entry carrying the metadata block an opened
image would have, and writes the whole thing back with `syncChanges()`.

The fixture **is that file**, which is what makes it worth having and also the
one fixture here that is edited after recording. Three substitutions, all made
by `record-project.sh` and all checked by it afterwards: the project directory
becomes `/PROJECT`, the slides' directory becomes `/SLIDES`, and the
`randomizedName` UUIDs become a fixed string. The first two are the same reason
`ProjectImport`'s input cannot be committed. The third is because QuPath's copy
constructor for an entry does not carry `randomizedName` over, so
`UUID.randomUUID()` runs again on every load — a fixture that changes on every
recording for a reason unrelated to the format is one nobody reads the diff of.
The script fails rather than commits if any of the three did not take.

What it caught on the first recording: both URI spellings appear in one project
QuPath wrote — `file:///` for the entries it read from this project's file and
`file:/` for the one it created itself — so a reader that took only the spelling
this project writes would silently drop rows. It also recorded that QuPath names
an entry with no name `"Image null"`, because the constructor builds the string
from a null parameter rather than the field beside it, and that a project's own
`uri` precedes every entry's, so the first `"uri"` in a project never names a
slide.

## Tolerances

Exact equality with QuPath does not hold in general: Java dropped `strictfp`
guarantees in 17, and Rust's libm differs from Java's `Math` in the last ULP.
Parity tests compare with documented tolerances — relative `1e-5` for scalar
measurements, Dice or IoU `>= 0.99` for masks and segmentations — except where
a fixture explicitly records bit-exact behaviour, as `measurement_precision`
does.

Never widen a tolerance to make a test pass. If parity cannot be reached,
report the numbers.

Where a port turns out to be **bit-identical** anyway — as the Gaussian blur is,
on all 22 of its cases — the tolerance stays the gate and the exact agreement is
*reported*, not asserted. A build whose `exp` or `sqrt` differs in the last ULP
would still be correct, and a test that failed there would be measuring libm.
The report is what makes a margin quietly eroding towards the limit visible
before it fails.
