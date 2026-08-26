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

**A recorder that opens an `ImageServer` must end with `System.exit(0)`.**
Bio-Formats leaves non-daemon threads behind, so without it the recorder writes
its JSON and then hangs — the work is finished and complete on disk, and the
process never returns, so the script sits until whatever `timeout` wraps it
fires. That reads as a failed recording and is not one:
`record-merge-tiles.sh` came back at exit 143 once for exactly this reason and
was misdiagnosed as a harness artefact. Fourteen recorders here already do it;
`ColourDeconvolution.java` has carried the note the longest. With the call, both
slide recorders return in about four seconds.

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

## The merge and the split, on real tissue

`MergeAndSplit.json` pins those two stages on hand-built shapes and on random
fields, and passes. `MergeAndSplitTile.json` pins them on tissue:

```bash
./parity/record-merge-tiles.sh                       # needs a slide and a JVM
cargo test -p qupath-pipeline --test parity_merge_and_split_tile
```

Two fixtures for one pair of stages, because the first has no case in the regime
that mattered. D-009 found the count gap tracking density, and the merge and the
split as the two stages whose ratios moved with it — on clumped tissue the merge
bridges about twice as much into shared blobs and the split cuts about twice as
much back apart. Nothing in `MergeAndSplit.json` is clumped: its largest case is
96x96 of blurred noise, and the rest are disks, bars and rings.

The crops come from **both** regions D-009 measured — four where QuPath and this
port disagree by 11.3% and four where they agree to 1.8%. A recording made only
where two things diverge cannot say whether the stage is wrong or the input is
unusual. Each region is taken whole rather than sampled within, which removes
the choice of where to sample: picking the clumpiest-looking crop by eye would
be choosing the sample most likely to show the effect being looked for, which is
the same failure as guessing regions by eye, one level in.

**The crops are the three regions, whole, at 2048x2048.** They were sixteen
320-pixel windows sampled inside two of them, and that grid could not see the
effect it was drawn to measure: its first-rung ratios came back at 1.058 sparse
against 1.079 dense, near enough identical, where the regions differ by eleven
points end to end. Two causes. The background estimate is computed over the
field it is given, so at 320 px the stage most suspected of carrying the gap ran
in a regime it never runs in. And the windows were small enough for where they
landed to decide the answer: `sparse-0` came back with one seed and zero objects
-- glass -- inside the sparse pool, flattening the signature.

Taken whole, they reproduce D-009's whole-region measurement: the merge keeps
0.443 of its input on sparse against 0.563 on dense, where the end-to-end
numbers are 0.439 against 0.557/0.560. That agreement is what says the fixture
measures the same thing those numbers did.

The masks are **bit-packed**, one bit per pixel as hex, because all five are
binary and the same three regions written as decimal arrays would be a 600 MB
fixture rather than a 16 MB one. The EDM is deliberately not recorded: it is the
one floating-point checkpoint, `MergeAndSplit.json` pins it bit-for-bit on twelve
cases, and what real tissue adds is scale and clumping rather than a new path
through `makeFloatEDM`.

The test asserts the regime as well as the pixels. Every comparison in it would
pass just as well on three regions of the same flat tissue — and the finding would
then rest on crops that were never in the regime it claims. So it also checks
that the sparse crops really do bridge more and split more, loosely enough that
a re-recording may move the ratios and tightly enough that it may not lose them.

## The background estimate, on real tissue

Same shape and the same reason as the merge and the split above — a stage whose
parts are pinned on synthetic input and whose *composition* was not pinned on
tissue:

```bash
./parity/record-background.sh                        # needs a slide and a JVM
cargo test -p qupath-pipeline --test parity_background_estimate
```

Seven crops: four ordinary, one of glass, and two recorded at a **lowered
`maxBackground`**. That last pair is not tuning, it is branch coverage. The mask
fires where the *eroded* optical density is still above the limit — dense stain,
a fold, ink — and at QuPath's default of 2.0 it fires nowhere on this slide, so
five crops at the default record one branch of a two-branch stage. The limit is
carried per crop and the test applies the recorded one.

**Checked at production size too, without committing it.** The crops are 160 px
because this is a stage comparison rather than a regime measurement — the same
input goes into both sides, so a disagreement on any real tissue is a
disagreement. That argument is sound but it does not cover this particular
stage: `morphologicalReconstruction` is a queue-driven propagation across the
whole image and switches strategy on `nChanges / nPixels > 0.1`, a condition on
the *fraction* of pixels changing, which depends on field size. So the three
D-009 regions were recorded whole at 2048x2048 and compared: **bit for bit on
12.5 million pixels**, with a deliberately corrupted float caught at its exact
coordinate.

That recording is ~200 MB (seven crops would be ~470 MB) and is deliberately
**not** committed. `parity_background_estimate.rs` reads
`QUPATH_RUST_BACKGROUND_FIXTURE`, so the committed comparison can be pointed at
a scratch recording rather than a second one being written to match it — a
re-implemented comparison would be checking the re-implementation. The two
coverage guards (seven crops, two masked) describe the committed fixture and are
skipped under the override; the pixel comparison, which is what checks the
stage, runs either way.

The first version of this fixture got both of those wrong and both were caught
by its own assertions: it asked for 320-pixel crops on the assumption that
detection downsamples by two — it does not on this slide, and the fixture came
out at 12 MB — and it added a crop of *glass* in the belief that the limit fires
where the image is bright. Optical density runs the other way.

## Reported, not asserted

Unlike the stage fixtures, `CellCounts.json` is **not** asserted by a Rust test. There is
no tolerance at which "the cell count matches" is currently true — it is 1.065x
pooled and up to 1.129x by region — and writing a test with a 1.15x bound would
be picking a number to pass rather than to measure. It is a reported comparison
until the remaining divergence is understood, per the rule below about never
widening a tolerance.

## And one that records an ordering

`SmartStringSort.json` is a fourth kind: not what QuPath computes from an
image, not what it reads, not what it writes, but **what one of its functions
does to a list**. `GeneralTools.smartStringSort` is what `ProjectBrowser` puts
its rows in order with, so it is the order a project browser has to reproduce —
and it is not a string sort. A name is split on `(\d+)`, the digit runs are
parsed to `BigDecimal` and compared by value, and the last tiebreak is the
number of parts.

```bash
./parity/record.sh                                       # SmartStringSort
cargo test -p qupath-viewer --test parity_smart_string_sort
```

The recorder hands the function thirty-three names chosen for the parts of that
a reader gets wrong, prints the order it returns, and asserts on its own side
that the printed order really is non-decreasing under the comparator — so the
Rust test can check a list rather than every pair.

It found something on the first run that no amount of reading `GeneralTools`
would have: **Java compares UTF-16 code units and Rust compares UTF-8 bytes**.
A character from U+10000 up is a surrogate pair in Java, both halves in
U+D800..U+E000, below every character from U+E000 to U+FFFF — so QuPath puts an
emoji *before* `\ue000` and `str::cmp` puts it after. One line of the fixture,
and the only one the byte order got wrong.

Two names in it compare **equal** (`0` and `000`, `slide-007` and `slide-7`,
because `BigDecimal.compareTo` ignores scale). Their order in the fixture is
Java's stable sort leaving them as they came, which the list alone cannot pin
down — so the test asserts the equality directly as well, and sorts with
`sort_by` rather than `sort_unstable_by`. A fixture that records a total order
records the ties too, and a comparator that breaks one of them would reproduce
the file and diverge on the next project.

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
cargo test -p qupath-viewer --test parity_project_open
```

Every path QuPath stores in a project is an absolute `file:` URI, so a
committed project would name the machine that recorded it. The script writes
one to a temporary directory, records QuPath's reading and deletes it; what
ties the fixture to the code is `examples/shared/project.rs`, which the writer
and the test both read the entries from.

Both recorders answer a question the other fixtures cannot: whether QuPath can
read what this project writes. The project one came back with four reasons it
could not.

`record-project.sh` also writes the entries' `thumbnail.jpg` before recording,
through `qupath-app`'s `project_thumbnails` example — the encoder lives in that
crate, because `qupath-viewer` depends on `qupath-slide` alone. Until it did,
`ProjectImport`'s `thumbnailReadable` had only ever read `false`, which measured
that QuPath tolerates the file's absence and could say nothing about whether a
JPEG written here is one `ImageIO.read` accepts. It now reads `true` for the
slide whose pixels can be decoded and `false` for the JPEG 2000 one, which gets
no thumbnail — both answers from one recording.

## And one that opens the pixels

`ProjectOpen.json` goes past the format to the image. Every other project
fixture stops before the pixels; this opens each entry through
`entry.readImageData()` — what double-clicking a row reaches, and the path that
runs `getServerBuilder()`, which is where a stored metadata block would be
applied. Recorded by the same script.

It answers a different question from `ProjectImport`'s: not whether QuPath can
*list* a project written here, but whether it can **work in** one. Those fail
differently — a project whose entries all read correctly can still carry a
builder that will not build, a URI resolving to the wrong slide, or a metadata
block laid over what OpenSlide read, so the image opens and every pixel after
that is wrong. The recorded fields are the ones that would expose exactly that,
ending in a block of pixels read from tissue.

**Where that block is read is the whole value.** Not the slide's centre and not
its origin: both are background here, and two readers both returning white agree
without either being right. The first version of this recorder clamped one
slide's tissue coordinates into a smaller one and landed on its corner — mean
243 against tissue's 99 — and a uniformity check missed it, because background
is not perfectly uniform. Tissue is now named per slide, and the recorder
refuses a block spanning fewer than 24 levels.

`CMU-1-JP2K-33005.svs` is recorded but not pixel-compared: it is Aperio JPEG
2000, which this project refuses by name. The fixture carries `pixelsComparable`
per entry rather than letting the test infer it from a file name, and a test
asserts some entry still is — an all-JP2K recording would otherwise make the
pixel check vacuous while passing.

## One that has nothing to do with a project

`OpenSlideLevels-*.json` compares pyramids, not formats, and has its own script:

```bash
./parity/record-openslide-levels.sh                 # needs the slides in data/
cargo test -p qupath-viewer --test parity_openslide_levels
```

`Slide::open` keeps the tiled TIFF directories and drops the label, macro and
thumbnail; OpenSlide's Aperio driver makes its own choice about what counts as a
level. Nothing had compared them, and it matters because a project's stored
metadata block *replaces* what QuPath would otherwise read — a level table that
disagrees is QuPath reading the wrong pixels, silently, for as long as the
project exists. They agree in every field.

Two things about it are deliberate. It records the **server**, not
`openslide_get_level_downsample`: `OpenslideImageServer` explicitly declines that
value — OpenSlide averages the width and height ratios, which is inexact whenever
the true downsample is an integer — and rebuilds the table from the level
dimensions, so the native call answers a question QuPath never asks. And it
records **two** slides, because OpenSlide's fallback tile size is 256: on CMU-1
alone "the tile sizes agree" would be two defaults coinciding, and
CMU-1-Small-Region's **240** is the only reason that comparison is a
measurement.

Both this and `ProjectOpen` need `qupath-extension-openslide` on the classpath,
which needs **no system package**: the extension depends on a jar carrying its
own `libopenslide.so`, extracted at load time by JNA. `ldconfig` having no
`libopenslide` says nothing about whether these run — an error made here, which
kept both of these marked blocked for several sessions.

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

**The end-to-end detection comparison is the standing example of that.**
`ObjectMasks.json` and `examples/object_masks` measure pooled Dice of 0.935,
0.934 and 0.880 over three regions, against the `>= 0.99` above. The criterion
is not met, the numbers are published in `README.md`, `docs/ROADMAP.md` §1.5 and
`DIVERGENCES.md` D-009, and the tolerance stays where it is.
`parity_object_masks.rs` gates at 0.80 — a **regression floor**, deliberately
well below the measurement, so that it fails when something breaks rather than
whenever a rounding boundary moves. A regression floor is not a success
criterion and is not quoted as one.

Where a port turns out to be **bit-identical** anyway — as the Gaussian blur is,
on all 22 of its cases — the tolerance stays the gate and the exact agreement is
*reported*, not asserted. A build whose `exp` or `sqrt` differs in the last ULP
would still be correct, and a test that failed there would be measuring libm.
The report is what makes a margin quietly eroding towards the limit visible
before it fails.
