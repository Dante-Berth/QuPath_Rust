# Contributing

Thanks for looking. This is a port of another project's algorithms, so the bar
for a change here is a little unusual: **the question is not whether the code is
good, it is whether it does what QuPath does** — and the way that gets answered
is with a recorded fixture, not with a reading of QuPath's source.

## The gate

Four commands. CI runs exactly these, on a pinned toolchain, so a clean run here
is a clean run there:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo doc --workspace --no-deps --locked          # with RUSTDOCFLAGS="-D warnings"
cargo test --workspace --release --locked
```

The toolchain is pinned to the version in `RUST_PINNED` in
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) — 1.96.0 at the time of
writing. This is deliberate: clippy gains lints every six weeks, and an
unpinned gate turns red on a release nobody asked for. A separate advisory job
reports what a bump to current stable would cost. Bumping the pin is its own
commit, which also fixes whatever the new version found.

`missing_docs` is `warn` across the workspace, and the rustdoc step is built
with `-D warnings`. A doc link to a renamed or private item compiles, passes
clippy, and reads correctly in the source — it fails only in the generated
page, so that step is the only thing that catches it.

### A green test run is not a full test run

No slides are committed ([`docs/test-data.md`](docs/test-data.md)); the smallest
useful one is larger than the source tree. Tests that need a slide skip when it
is absent — and **a skipped test in Rust prints `ok` and counts toward the
total**. A checkout with no slides reports the same green as a full run, having
exercised materially less.

Before you quote a number, or read green as evidence:

```sh
QUPATH_RUST_REQUIRE_DATA=1 cargo test --workspace --release --no-fail-fast
```

That turns every such skip into a failure naming the file it wanted. Leave it
unset for ordinary local work — that is what lets the suite run at all without
hundreds of megabytes of slides. CI runs this pass too, discards its exit
status, and posts what it named to the job summary.

## Changing behaviour that QuPath also has

This is the rule that matters here.

**A new parity claim needs a recorded fixture and a mutation check.** Not one or
the other.

1. **Record QuPath's side.** Add a recorder under `parity/java/` and regenerate
   with `./parity/record.sh` (or `./parity/record-slide.sh` if it opens a
   slide). Fixtures land in `parity/fixtures/` as JSON and **are committed** —
   that is what lets the Rust tests run without a JVM, and what makes a
   behavioural change on either side show up as a diff. See
   [`parity/README.md`](parity/README.md), which also documents the
   `System.exit(0)` requirement for any recorder that opens an `ImageServer`.

2. **Compare at the stage boundary, not at the end.** Feed this port the *same*
   input QuPath was given at that boundary and compare pixel for pixel.
   Agreeing on a final count is the weakest available evidence: two runs can
   agree on how many objects they found and disagree about where every one of
   them is.

3. **Mutate the thing under test and watch the fixture catch it.** Break the
   code on purpose — flip a sign, drop a term, shift an index — and confirm the
   new test fails. Then revert. A test that has never been seen to fail is not
   evidence, and this suite has more than one way to be silently vacuous: a
   fixture truncated to nothing still parses, and a region of glass cannot
   disagree about anything.

Say in the pull request which mutation you ran and that it was caught.

### Do not check against your reading of QuPath's source

Run their reader, their writer, their function. The recorded-fixture approach
exists because reading the Java and reimplementing what it seems to say has
produced bugs here that looked entirely correct — an opening substituted for a
top hat, the DAB stain vector used as EOSIN, a JPEG colour-transform error on
tiles carrying no APP14 marker. **All three produced plausible images and passed
visual inspection.** The fixtures caught them.

## Divergences

Where this deliberately or unexplainedly differs from QuPath, it is recorded in
[`DIVERGENCES.md`](DIVERGENCES.md) with what is known and what is not. If you
find a new difference, add an entry there even if you cannot explain it — an
open, honestly-described gap is worth more than a silent one. The count
difference tracked as D-009 is open and density-dependent; work on it is
welcome, and the stage ladder in that entry is where to start.

If you close a divergence, say what the evidence is. "The counts match now" is
not it.

## Style

Match the surrounding code. Comments here tend to explain *why* — particularly
why something differs from QuPath, or why an obvious-looking simplification was
tried and reverted. That history is worth keeping.

The tree is `rustfmt`-clean and CI enforces it, so format everything; there is
no hand-picking rule.

## Licence

GPL-3.0-or-later, as a derivative work of QuPath. Contributions are accepted
under the same terms.
