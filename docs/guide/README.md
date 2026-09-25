# User guide

How to open a slide, find cells in it, and get the result into QuPath.

This is the *user* documentation. It describes what the application does and
how to drive it. It is deliberately separate from the developer documentation,
which answers a different question — why the code is the way it is:

| File | Answers |
| --- | --- |
| [`docs/STATUS.md`](../STATUS.md) | Parity crate by crate, the bugs the fixtures caught, the speed breakdown |
| [`docs/dev/MEMO.md`](../dev/MEMO.md) | What the project is, what is finished, what to distrust |
| [`docs/dev/ROADMAP.md`](../dev/ROADMAP.md) | The plan, stage by stage |
| [`DIVERGENCES.md`](../../DIVERGENCES.md) | Every place this deliberately differs from QuPath |
| [`docs/dev/VIEWER.md`](../dev/VIEWER.md) | The viewer's design record |

## Before you rely on anything here

**This is a partial port and its parity with QuPath is incomplete.** Over ten
regions of CMU-1 it finds **1.065x** QuPath's object count — 156,979 against
147,351 — always over-counting, and more so on sparse tissue. Nuclei that do
correspond agree well (97.9% of QuPath's match one-to-one, mean Dice 0.921),
so the gap is a population of *extra* objects rather than boundaries in the
wrong place.

Read [Parity and what to trust](parity.md) before using the output for
anything that matters. It is the first page for a reason.

## The pages

1. **[Installing and building](installing.md)** — toolchain, build, and getting
   a slide to open.
2. **[Viewing a slide](viewing.md)** — opening, panning, zooming, brightness,
   and reading the status bar.
3. **[Detecting cells](detecting.md)** — running detection, what the
   parameters mean, and which pyramid level a run actually reads.
4. **[Projects](projects.md)** — opening a QuPath project, browsing and
   searching it, and editing entry metadata.
5. **[Exporting to QuPath](exporting.md)** — saving detections as GeoJSON and
   opening them in QuPath.
6. **[Parity and what to trust](parity.md)** — where this agrees with QuPath,
   where it does not, and how that was measured.
7. **[Command-line tools](command-line.md)** — the examples that run without a
   window, for measurement and scripting.

## The shortest possible start

```bash
cargo build --release
cargo run --release -p qupath-app -- data/CMU-1.svs
```

Then press `d` to detect cells in what is on screen, and `s` to save them as
GeoJSON that QuPath can open. [Detecting cells](detecting.md) explains what
that run did and which level it read.
