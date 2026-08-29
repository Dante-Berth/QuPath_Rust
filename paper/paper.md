---
# JOSS submission. STATUS: skeleton -- the prose below is an outline, not a
# draft. JOSS compiles this file directly; keep it Markdown, and keep the body
# to roughly 250-1000 words. The long argument lives in preprint/main.tex.
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

<!-- TODO. ~150 words, for a non-specialist. What the software does: a Rust
port of the cell-detection chain from QuPath [@bankhead2017qupath], with a
columnar object layout suited to bulk analysis, and a fixture suite that checks
each stage against output recorded from QuPath itself.

Do not claim parity. State the 1.065x count difference here, in the summary --
it is in CITATION.cff already and a reviewer will find DIVERGENCES.md. -->

# Statement of need

<!-- TODO. REQUIRED SECTION -- JOSS rejects submissions without it.

Who needs this and why. The audience is batch analysis over many slides, where
QuPath's per-object string-keyed measurement map is the bottleneck. Be precise
that the speedup comes from the columnar layout rather than from Rust as such;
the README makes this distinction and the paper must not lose it.

The second half of the need is the verification problem: reimplementations of
scientific image-analysis tools are common and are usually validated by eye or
by aggregate counts, both of which pass on wrong output. This project's fixture
method is offered as the reusable part. -->

# Verification method

<!-- TODO. ~200 words. The stage-boundary fixture idea: record QuPath's
intermediate state at each stage boundary, feed this port the same input there,
compare pixel for pixel. Mutation-check every fixture so a test that cannot
fail is not trusted [@jia2021mutation].

Name what it found: three bugs that produced entirely plausible images (an
opening substituted for a top hat, the DAB stain vector used as EOSIN, and a
JPEG colour-transform error on Aperio tiles carrying no APP14 marker). This is
the argument for the method -- visual inspection passed all three. -->

# Current limitations

<!-- TODO. Required by honesty and by the repository's own documentation.

The open count gap: 1.065x over ten regions of CMU-1, 1.012-1.129 by region,
always over-counting, tracked as D-009. Seven of ten stages ruled out against
QuPath on real tissue pixel for pixel; three remain (the blur, the Laplacian,
the regional maxima). Nucleus masks that do correspond agree well (mean Dice
0.921 over matched pairs), so the gap is a population of extra objects on
sparse tissue rather than misplaced boundaries.

Also: measurements are not ported, and the viewer claims no parity. -->

# Acknowledgements

<!-- TODO. The QuPath authors, whose algorithms this reimplements. Notify them
before submitting. -->

# References
