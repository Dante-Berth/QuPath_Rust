# Publication roadmap

Two artifacts, one body of evidence:

- **`paper.md` + `paper.bib`** — the JOSS submission. Markdown is what JOSS's
  own tooling compiles; the ~1000-word ceiling is the journal's, not a
  stylistic choice. Currently 626 words.
- **`preprint/`** — the arXiv preprint. LaTeX, 11 pages, carries the parity
  tables and the method that JOSS's word limit has no room for.

They are not two papers, and they are not the same paper at two lengths. The
preprint argues the **method**, with the port as its case study. `paper.md`
describes the **software** — the port and its parity harness as one artefact —
because that is what JOSS reviews.

**Both are drafted, and re-cut on 2026-08-30** around the framing below. What
is still marked `\todo{}` in the LaTeX is not writing: the author block and
the Zenodo DOI. Everything else is prose. Figures are converted from the tracked SVGs in `docs/benchmarks/` by
the Makefile and gitignored, so there is no second copy of a recording.

The numbers in both were copied from `DIVERGENCES.md` D-009, `docs/MEMO.md`
§2 and the README rather than re-derived. **If a number in the papers ever
disagrees with the repository, the repository is right** — the papers are
downstream, and `main.tex` says so in its header.

## The framing decision, which everything else follows from

**This is not a "we reproduced QuPath" paper, and not a "we made QuPath fast"
paper either.** Both are refutable from the repository's own documentation,
and the second is the more tempting mistake because the speed figure is real.

The speedup is 3.359 s against QuPath's 15.87 s single-threaded, 4.7x. It is
also an upper bound of unknown tightness — QuPath's time includes computing
measurements this port does not implement — and it is attached to output that
finds **6.5% more objects than the reference**, density-dependently, so it
cannot be corrected for with a constant. A faster implementation that computes
something measurably different is not yet useful to anyone doing research, and
a paper selling the speed invites exactly the review that says so.

**So the method is the subject and the port is the case study.** What is
defensible today, in the order the papers now present it:

1. **The usual validation practices fail, with evidence.** Three bugs that
   produced entirely plausible images — the opening-for-tophat substitution,
   the DAB-vector-as-EOSIN error, the missing-APP14 JPEG decode. Visual
   inspection passed all three; count comparison would have caught none. This
   is the paper's opening claim because it is the part that holds for readers
   who do not care about cell detection.
2. **A stage-level parity methodology**: record the reference's intermediate
   state at each stage boundary, feed the port the *same* input there, compare
   pixel for pixel. (See `parity/README.md`.)
3. **Mutation testing of the fixtures themselves** — every parity test shown
   able to fail before it is trusted. The part most ports skip, and what makes
   the negative results credible.
4. **A localisation result**: seven stages ruled out on real tissue, three
   remaining, the gap split into a density-independent ~4.7% maxima excess and
   a density-dependent effect at the mean-intensity filter, and the merge/split
   pair — which every aggregate statistic implicated — exonerated pixel for
   pixel.

The open gap is a **result**, not an embarrassment: the method localised a
discrepancy that visual comparison could not see and that end-to-end counts
could not attribute. A method for finding subtle discrepancies is best
evidenced by one it found.

**The corollary for the speed figure**: report it, with its conditions, in the
case-study section. Do not lead with it, do not put it in Results, and do not
let a redraft quietly promote it. A port reporting a speedup without reporting
how equivalence was established has not reported its main risk.

## Venue, and the order

**arXiv first, and soon. JOSS second, and only after the claims are clean.**

This reverses an earlier version of this file, which said JOSS was the best
fit and put it first. The correction matters: **JOSS reviews whether the
software does what it claims.** A submission whose README claimed a QuPath
port, while `DIVERGENCES.md` recorded a 6.5% over-count, would stall in review
on the discrepancy rather than on anything about the software's quality. Fix
what is claimed, then submit. That is now done in `paper.md`, which offers the
port *and the parity harness* as one artefact and states the gap in its
Summary.

- **arXiv**, category `eess.IV` (Image and Video Processing), cross-list
  `cs.CV`. Costs nothing, gatekeeps only through endorsement, and timestamps
  the method. Not bioRxiv: the contribution is software verification, not a
  biological finding, and bioRxiv's readership judges a novelty this does not
  claim. Note arXiv requires endorsement for a first submission in a category.
- **JOSS** — reviews the software, open review on GitHub, free, issues a DOI.
  Right venue, second in order. Its blockers are listed below.
- Considered and rejected: **SoftwareX**, ~$2,500 APC despite being widely
  assumed free. **Bioinformatics** Application Notes would want D-009 closed.

### If D-009 never closes

The method paper still stands, and this is the point of the re-framing. Target
venues where verification methodology and negative results are the subject
rather than an embarrassment — empirical software engineering, research
software engineering, or a reproducibility track. The paper's claim there is
not "we ported QuPath" but "here is how to check a port, and here is what it
catches that inspection and counts do not".

**Do not wait for D-009 before posting the preprint.** Publishing invites the
fix: a QuPath maintainer reading the stage ladder may recognise the flat 4.7%
maxima excess on sight. The gap being open is evidence the method works, not
evidence against it.

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
- [x] **Statement of need** — written. Two halves: the columnar layout for
      batch workloads (stated as the layout's doing, not Rust's), and the
      general one — reimplementations are usually validated by eye or by
      aggregate count, and both accept wrong output.
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

Drafting is done. What remains is one blocker, one courtesy, and the
mechanics.

1. ~~Draft `paper.md`.~~ Done; re-cut 2026-08-30 around the port-plus-harness
   framing, 626 words.
2. ~~Add `CONTRIBUTING.md` and `CODE_OF_CONDUCT.md`.~~ Done 2026-08-29.
3. ~~Draft the preprint.~~ Done; re-cut 2026-08-30 with the method as subject
   and the port as case study. 11 pages, clean build.
4. **Resolve authorship and affiliation.** The gating item. Nothing downstream
   — arXiv, Zenodo, JOSS — can be submitted under a handle, and no amount of
   repository work removes it.
5. **Check `paper.bib` against doi.org.** Entries were written from memory and
   the header says so. Two errors have already surfaced by using the file
   rather than checking it (a key dated wrong, an article typed as a
   conference paper), which is reason to expect more.
6. **Diff `CODE_OF_CONDUCT.md` against the published Contributor Covenant 2.1.**
   Reproduced from memory; the fetch was blocked.
7. **Notify the QuPath maintainers.** Before submitting, not after. This
   reimplements their algorithms, a JOSS reviewer may well be one of them, and
   the courtesy costs nothing.
8. **Post to arXiv** (`eess.IV`, cross-list `cs.CV`). Needs an endorsement for
   a first submission in the category.
9. **Audit the README and `CITATION.cff` against the paper's framing.** The
   papers now lead with the method; the README still opens as a port with a
   speed section. It is honest — the gap is above the fold — but it sells the
   wrong thing to someone arriving from the preprint, and JOSS reviewers read
   the repository, not the paper. This is real work, not a tidy-up.
10. **Tag a release, archive to Zenodo** for the DOI, fill the two remaining
    `\todo{}` markers in `main.tex` (author block, DOI), rebuild.
11. **Submit to JOSS**, citing the preprint.

## What would change the framing

If the three remaining stages (blur, Laplacian, regional maxima) are pinned on
tissue at production size and D-009 closes, three things change at once:

- the port becomes a **usable substitute** rather than a documented
  approximation, and the speed figure becomes a claim someone can act on;
- the README's framing becomes true as written, and JOSS's review has nothing
  to catch on;
- a **parity result** joins the method result, which makes a methods journal
  reachable in a way it is not today.

That is the outcome worth wanting. It is not a reason to hold the preprint:
the method paper stands on its own, the gap is evidence the method works, and
publishing is the fastest way to get another pair of eyes on a 4.7% excess
that has resisted this one. If it closes after the preprint is posted, that is
a second paper, not a wasted first one.

**What would *not* change**: the three bugs, the refuted merge/split
hypothesis, and the argument that inspection and counts accept wrong output.
Those are findings about validation practice and they survive D-009 either
way. That is the test of whether the re-framing was right — the paper's
central claims should not depend on how the case study ends.
