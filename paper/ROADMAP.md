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

- [x] **`CONTRIBUTING.md`** — added 2026-08-29. Carries the gate CI runs
      (quoted from `ci.yml`, not from memory), the fixture-plus-mutation rule
      for any new parity claim, and the warning that a green `cargo test` is
      not a full run. Linked from the README's own §Contributing, since
      GitHub only surfaces it at the pull-request box — after the code is
      written, which is too late for the rule it carries.
- [x] **`CODE_OF_CONDUCT.md`** — added 2026-08-29, Contributor Covenant 2.1,
      enforcement contact set. **Reproduced from memory** (the fetch was
      blocked): diff it against the published 2.1 text before submitting.
- [ ] **Statement of need** — required section of `paper.md`. Who is this for?
      Bulk/batch analysis where QuPath's per-object layout is the bottleneck.
      Be precise that the speedup is the columnar layout, not Rust as such;
      the README already makes this distinction and the paper must not lose it.
- [ ] **A tagged release with a DOI** (Zenodo), created at submission time.
- [x] **Installation instructions a stranger can follow** — verified
      2026-08-29 by cloning to a fresh directory with no `data/` and following
      README §Building. `cargo test --workspace --release --locked` builds and
      passes with no system dependencies, as claimed: **594 pass, 0 fail**.
      Of those, **18 did not actually run** — 14 want the 1.9 MB
      `CMU-1-Small-Region.svs`, 4 the 177 MB `CMU-1.svs` — and the README now
      says so with those numbers.

      This exercise found a real bug: the witness pass lacked
      `--no-fail-fast`, so it named 7 skipped tests out of 18 and CI's job
      summary under-reported the very thing it exists to report. Fixed in
      `ci.yml` and in all three places the command is documented.

### Must be resolved before submission

- [ ] **Authorship.** `CITATION.cff` lists only the handle `Dante-Berth`. JOSS
      requires real names, affiliations and ORCIDs.
- [ ] **The QuPath relationship.** This is a derivative work reimplementing
      another project's algorithms. Notify the QuPath maintainers before
      submitting — a JOSS reviewer may well be one of them, and the courtesy
      costs nothing. Cite QuPath's own paper (Bankhead et al. 2017) prominently.
- [x] **Slide provenance** — already documented and verified in
      `docs/test-data.md` §Licensing: the OpenSlide test data is CC0 1.0,
      declared per file in the directory index and machine-readable in
      `index.json`, no attribution or registration required. Exact byte counts
      are recorded there too. The paper needs to *cite* this, not establish
      it.

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
2. ~~Add `CONTRIBUTING.md` and `CODE_OF_CONDUCT.md`.~~ Done 2026-08-29.
3. Resolve authorship and affiliation. **This is the gating item** — nothing
   downstream (arXiv, Zenodo, JOSS) can be submitted under a handle, and it
   is the one blocker no amount of repository work removes.
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
