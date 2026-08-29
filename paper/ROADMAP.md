# Publication roadmap

Two artifacts, one body of evidence:

- **`paper.md` + `paper.bib`** — the JOSS submission. Markdown, ~700 words, is
  what JOSS's own tooling compiles. This is the format the journal requires;
  it is not a stylistic choice.
- **`preprint/`** — the arXiv preprint. LaTeX, longer, carries the parity
  tables and the method that JOSS's word limit has no room for.

They are not two papers. The preprint is the full argument; `paper.md` is the
software summary that points at it.

## The framing decision, which everything else follows from

**This is not a "we reproduced QuPath" paper, and must not be written as one.**
D-009 is open: over ten regions of CMU-1 this finds 1.065x QuPath's object
count (1.012–1.129 by region), always over-counting. That gap is real,
density-dependent, and unexplained.

Writing a parity claim now would be refuted by the repository's own
`DIVERGENCES.md`, which a reviewer will read.

**Write the method paper instead.** The contribution is *how a port of a
scientific image-analysis pipeline is verified stage by stage, and what that
method finds* — including the hypotheses it refuted. Concretely, what is
defensible today:

1. A stage-level parity methodology: record the reference implementation's
   intermediate state at each stage boundary, feed the port the *same* input at
   that boundary, compare pixel for pixel. (See `parity/README.md`.)
2. **Mutation testing of the fixtures themselves** — every parity test is shown
   able to fail before it is trusted. This is the part most ports skip, and it
   is what makes the negative results credible.
3. Seven stages ruled out on real tissue, three remaining, and a documented
   trail of refuted hypotheses (D-009's own history: the merge/split pair was
   the named suspect and was exonerated pixel-for-pixel).
4. Three bugs the fixtures caught that produced *entirely plausible images* —
   the opening-for-tophat substitution, the DAB-vector-as-EOSIN error, the
   missing-APP14 JPEG decode. This is the strongest argument for the method:
   visual inspection passed all three.

The open gap is a **result**, not an embarrassment. Frame it as: the method
localised a discrepancy that visual comparison could not see, to three named
stages out of ten.

## Venue

**arXiv + JOSS, in that order.** They do not conflict; JOSS explicitly permits
preprints.

- **arXiv**, category `eess.IV` (Image and Video Processing), cross-list
  `cs.CV`. Not bioRxiv: the contribution is software verification and numerical
  parity, not a biological finding, and bioRxiv's readership judges biological
  novelty this does not claim. Note arXiv requires endorsement for a first
  submission in a category.
- **JOSS** — reviews the *software*, not novelty; open review on GitHub; free;
  issues a DOI. The best fit for what this is.
- Considered and rejected for now: **SoftwareX** carries a ~$2,500 APC despite
  being widely assumed free; **Bioinformatics** Application Notes would want
  D-009 closed first.

## Blockers for JOSS

Checked against the repository on 2026-08-29.

### Already satisfied

- OSI-approved licence (GPL-3.0-or-later), correct as a derivative work of
  QuPath and argued as such in the README.
- Substantial scholarly effort — eight crates, a full detection chain, a
  recorded fixture suite.
- Automated tests: 594 across the workspace, gated in CI.
- `CITATION.cff`, which already names the open count gap in its abstract.
- Documented reproduction paths for every headline number.
- CI covering fmt, clippy, rustdoc and the suite.

### Must be added

- [ ] **`CONTRIBUTING.md`** — JOSS requires documented contribution guidelines.
      State how to run the parity suite, and that a new parity claim needs a
      recorded fixture plus a mutation check.
- [ ] **`CODE_OF_CONDUCT.md`** — required. Contributor Covenant is standard.
- [ ] **Statement of need** — required section of `paper.md`. Who is this for?
      Bulk/batch analysis where QuPath's per-object layout is the bottleneck.
      Be precise that the speedup is the columnar layout, not Rust as such;
      the README already makes this distinction and the paper must not lose it.
- [ ] **A tagged release with a DOI** (Zenodo), created at submission time.
- [ ] **Installation instructions a stranger can follow** — README §Building
      exists; verify it works from a clean checkout on a machine without the
      177 MB slide, and say plainly which tests skip without it.

### Must be resolved before submission

- [ ] **Authorship.** `CITATION.cff` lists only the handle `Dante-Berth`. JOSS
      requires real names, affiliations and ORCIDs.
- [ ] **The QuPath relationship.** This is a derivative work reimplementing
      another project's algorithms. Notify the QuPath maintainers before
      submitting — a JOSS reviewer may well be one of them, and the courtesy
      costs nothing. Cite QuPath's own paper (Bankhead et al. 2017) prominently.
- [ ] **Slide provenance.** CMU-1 is from the OpenSlide freely-distributable
      test set; state its licence and where it comes from, since every headline
      number depends on it.

## Preprint outline

Target ~8–10 pages.

1. **Introduction** — QuPath's role in digital pathology; why bulk analysis
   hits a wall on the per-object string-keyed measurement map; what a port
   must prove before anyone can trust it.
2. **The verification method** — stage-boundary fixtures, recorded from a live
   QuPath build. Why comparing *final counts* is the weakest possible evidence
   (two runs can agree on how many objects they found and disagree about where
   every one of them is). Mutation testing as the check on the checks.
3. **The port** — columnar object layout; the seven-stage detection chain;
   what is ported and what is deliberately not (the viewer claims no parity).
4. **Results: what the method found** — the three plausible-looking bugs; the
   seven stages ruled out; D-009's refuted hypotheses; the Dice analysis
   separating population error from shape error.
5. **The open gap** — D-009 in full: the ladder, the density signature, the
   three remaining stages. State it as a localisation result.
6. **Discussion** — what transfers to other scientific-software ports; the
   limits (one slide, one detection algorithm).

## Figures

Reuse, do not regenerate:

- `docs/benchmarks/compare-threads.svg` — wall clock against thread count,
  with the tile-count saturation explained.
- The D-009 stage-ladder table — the strongest single figure, since it splits
  the gap into a flat ~4.7% maxima excess and a density-dependent jump at the
  mean-intensity filter.
- The Dice/unmatched-objects table — shows the gap is a population of extra
  objects, not displaced boundaries.
- A method schematic (new): the stage-boundary fixture idea, drawn once.

## Order of work

1. Draft `paper.md` against JOSS's required sections (summary, statement of
   need, references). ~700 words is a hard constraint; the preprint carries
   the rest.
2. Add `CONTRIBUTING.md` and `CODE_OF_CONDUCT.md`.
3. Resolve authorship and affiliation.
4. Notify the QuPath maintainers.
5. Draft the preprint from `DIVERGENCES.md` D-009 and `docs/MEMO.md` §2 —
   both already contain the argument in prose; this is largely restructuring,
   not new writing.
6. Post to arXiv.
7. Tag a release, archive to Zenodo, submit to JOSS citing the preprint.

## What would change the framing

If the three remaining stages (blur, Laplacian, regional maxima) are pinned on
tissue at production size and D-009 closes, this becomes a parity paper and the
venue calculation changes — a methods journal becomes reachable. Until then,
the method is the contribution and the gap is a finding. Do not wait for it:
the method paper stands on its own, and publishing invites the fix.
