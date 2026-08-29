---
# JOSS submission. The long argument lives in preprint/main.tex; this is the
# software summary that points at it. JOSS compiles this file directly, so it
# must stay Markdown and stay roughly 250-1000 words.
title: 'QuPath_Rust: a verified partial port of QuPath''s cell detection'
tags:
  - Rust
  - digital pathology
  - whole-slide imaging
  - cell detection
  - software verification
authors:
  # TODO: real name, affiliation index and ORCID. The repository currently
  # carries only the handle "Dante-Berth", which JOSS will not accept.
  - name: "TODO"
    orcid: "0000-0000-0000-0000"
    affiliation: 1
affiliations:
  - name: "TODO"
    index: 1
date: 29 August 2026
bibliography: paper.bib
---

# Summary

QuPath [@bankhead2017qupath] is a widely used open platform for whole-slide
image analysis, and its watershed cell detection is a standard first step in
quantitative digital pathology. `QuPath_Rust` reimplements that detection chain
in Rust with a columnar object layout: detections are a struct of arrays, one
contiguous column per feature, rather than QuPath's per-object string-keyed
measurement map. On a 4000x4000 region of the CMU-1 reference slide it runs
4.7x faster single-threaded than QuPath's 15.87 s, and 15.6x on twenty threads.
That difference comes from the data layout, not from the choice of language.

The software is distributed with the apparatus used to check it. Rather than
comparing final object counts, it records QuPath's own intermediate state at
each stage boundary and feeds this port the *same* input at that boundary, so
each stage is compared against the reference on identical data. Those recorded
fixtures are committed, so the suite runs without a JVM.

**Parity is measured here rather than assumed, and it is incomplete.** Over ten
regions of CMU-1 this port finds 156,979 objects against QuPath's 147,351 cells
— 1.065x, ranging 1.012-1.129 by region and always over-counting. Nuclei that
do correspond agree well (97.9% of QuPath's match one-to-one, mean Dice 0.921
[@dice1945measures]), so the gap is a population of extra objects on sparse
tissue rather than displaced boundaries. Seven of ten stages are ruled out
against QuPath on real tissue, pixel for pixel; three remain. This is tracked
openly as D-009 in the repository's `DIVERGENCES.md`, and anyone relying on the
output should read it first.

# Statement of need

Two needs, one of which is the more general.

**Bulk analysis.** QuPath is built for interactive use, and its per-object
string-keyed measurement map follows sensibly from that: a pathologist inspects
objects one at a time. Across hundreds of slides the same layout costs an
allocation and a hash lookup per measurement and scatters values that are read
together. Reorganising detections into columns removes that cost, which is what
this port exists to provide for batch workloads.

**Verifying a port at all.** Reimplementations of scientific image-analysis
tools are common, and are usually validated by eye or by agreement on an
aggregate count. Both accept wrong output. Agreeing counts are the weakest
available evidence, because two implementations can agree on how many objects
they found and disagree about where every one of them is. The stage-boundary
fixture method here is offered as the reusable part, and the repository is
structured so it can be read as a worked example: the recorders, the committed
fixtures, the mutation checks, and a `CONTRIBUTING.md` requiring both a fixture
and a mutation check for any new parity claim.

# Verification method

For each boundary between stages, small Java programs run inside a live QuPath
build record the intermediate state — actual pixel buffers, floats by their bit
patterns — and this port is given that state as input at the same boundary.
Because a stage that faithfully processes a different input still produces
different output, this is what makes a discrepancy attributable to one stage
instead of to everything downstream of it.

Every fixture is then **mutation-checked** [@jia2011mutation]: the code under
test is deliberately broken and the test confirmed to fail before its passing is
trusted. A parity test that cannot fail is not evidence, and there are several
ways for one to be silently vacuous — a truncated fixture still parses, and a
region containing no tissue cannot disagree about anything.

The method's value is best shown by three defects it caught, all of which had
been producing **entirely plausible images**: a morphological opening
substituted for a top hat (wrong operation, wrong radius, wrong direction); the
DAB stain vector used under the name EOSIN, a 52% error on the leading
coefficient [@ruifrok2001quantification]; and Aperio JPEG tiles carrying no
Adobe APP14 marker, so a conforming decoder applies the wrong colour transform.
Visual inspection passed all three. No test comparing against anything but
QuPath's own recorded output would have found them.

# Current limitations

The count gap above (D-009) is open. Stage-boundary comparison separates it into
a flat, density-independent ~4.7% excess already present in the regional maxima
and a distinct density-dependent effect appearing at the mean-intensity filter;
the blur, the Laplacian and the regional maxima are the three stages not yet
pinned on tissue at production size. Measurements are not ported, and the
interactive viewer claims neither parity nor speedup. This is a documented
partial port, not a drop-in replacement.

# Acknowledgements

We thank the authors and maintainers of QuPath [@bankhead2017qupath], whose
algorithms this work reimplements and against whose implementation every result
here is measured, and the OpenSlide project [@goode2013openslide] for the
freely-distributable CMU-1 slide (CC0 1.0) on which every number here depends.

# References
