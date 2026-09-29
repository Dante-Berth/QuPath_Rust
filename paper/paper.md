---
# JOSS submission. The long argument lives in preprint/main.tex; this is the
# software summary that points at it. JOSS compiles this file directly, so it
# must stay Markdown and stay roughly 250-1000 words.
#
# NOTE ON FRAMING: JOSS reviews the software, not novelty, so this cannot be
# the preprint in miniature. What is submitted here is the port *and its
# verification apparatus* as one artefact. The open count gap is stated in the
# Summary rather than in a limitations section -- a reviewer will open
# DIVERGENCES.md, and the paper must not read as having hidden it.
title: 'QuPath_Rust: a stage-verified partial port of QuPath''s cell detection'
tags:
  - Rust
  - digital pathology
  - whole-slide imaging
  - cell detection
  - software verification
authors:
  # TODO: real name and affiliation (ORCID set). The repository currently
  # carries only the handle "Dante-Berth", which JOSS will not accept.
  - name: "TODO"
    orcid: "0009-0000-7566-1979"
    affiliation: 1
affiliations:
  - name: "TODO"
    index: 1
date: 30 August 2026
bibliography: paper.bib
---

# Summary

`QuPath_Rust` is a partial Rust reimplementation of the watershed cell
detection in QuPath [@bankhead2017qupath], a widely used open platform for
whole-slide image analysis, together with the apparatus used to verify it
against QuPath itself.

The software has two parts, and the second is the reason it is offered here.
The first is the port: the ten-stage detection chain, with detections stored
columnarly — a struct of arrays, one contiguous column per feature, rather than
QuPath's per-object string-keyed measurement map. The second is a **parity
harness**: recorders that run inside a live QuPath build and capture its
intermediate state at each stage boundary, committed fixtures holding that
state, and Rust tests that feed this port the *same* input at the same boundary
and compare pixel for pixel. Every fixture is mutation-checked, so no test is
trusted until it has been shown able to fail.

**Parity is measured here rather than assumed, and it is incomplete.** Over ten
regions of the CMU-1 reference slide this port finds 156,979 objects against
QuPath's 147,351 cells — 1.065x, ranging 1.012–1.129 by region and always
over-counting. Nuclei that do correspond agree well (97.9% of QuPath's match
one-to-one, mean Dice 0.921 [@dice1945measures]), so the discrepancy is a
population of extra objects on sparse tissue rather than displaced boundaries.
Seven of ten stages are excluded against QuPath on real tissue, pixel for
pixel; three remain. This is tracked as D-009 in `DIVERGENCES.md`.

Users should read that entry before relying on the output. This is a documented
partial port, not a drop-in replacement: measurements are not implemented, the
interactive viewer claims no parity, and the count gap above is open.

# Statement of need

The port addresses a layout problem. QuPath is built for interactive use, and a
per-object string-keyed measurement map follows sensibly from that — a
pathologist inspects objects one at a time. Across hundreds of slides the same
layout costs an allocation and a hash lookup per measurement and scatters
values that are read together. Columnar storage removes that cost; on QuPath's
own profiled 4000x4000 region this port takes 3.359 s single-threaded against
QuPath's 15.87 s. That figure is an **upper bound**: QuPath's time includes
computing measurements this port does not implement, and the port does not yet
reproduce QuPath's object count, so it is not yet a usable substitute for
research work.

The verification apparatus addresses a more general need, and is why the
software is submitted while the port is incomplete. Reimplementations of
scientific image-analysis tools are common and are usually validated by eye or
by an aggregate count. Both accept wrong output: a count is a one-dimensional
projection in which errors cancel, and visual inspection checks whether output
is plausible rather than whether it is the same. Three defects in this port
demonstrate the point, each having produced entirely plausible images — a
morphological opening substituted for a top hat; the DAB stain vector used
under the name EOSIN, a 52% error on the leading coefficient
[@ruifrok2001quantification]; and Aperio JPEG tiles carrying no Adobe APP14
marker, so a conforming decoder applies the wrong colour transform. None would
have been caught by count comparison, and none was caught by inspection.

The repository is therefore usable as a worked example of stage-boundary
verification [@jia2011mutation] as well as a detection library: the recorders,
the committed fixtures, the mutation checks and a `CONTRIBUTING.md` requiring
both a fixture and a mutation check for any new parity claim. The method and
the full analysis of the open gap are described in the accompanying preprint.

# Acknowledgements

We thank the authors and maintainers of QuPath [@bankhead2017qupath], whose
algorithms this work reimplements and against whose implementation every result
here is measured, and the OpenSlide project [@goode2013openslide] for the
freely-distributable CMU-1 slide (CC0 1.0) on which every number here depends.

# References
