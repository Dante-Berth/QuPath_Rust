# Presentation

A 20-slide deck on the port, the parity gap, and the verification method.

| File | |
| --- | --- |
| [`QuPath_Rust.pptx`](QuPath_Rust.pptx) | The deck. Self-contained — every image is embedded. |
| `build.py` | Builds it. |
| `figures.py` | Draws the five charts. |
| `check.py` | Checks the built deck for layout faults. |
| `figures/` | Generated PNGs. Committed so the deck rebuilds without matplotlib. |

## Rebuilding

```bash
python3 presentation/figures.py   # only if the underlying numbers changed
python3 presentation/build.py
python3 presentation/check.py
```

Needs `python-pptx`, `matplotlib` and `pillow`.

**The deck is generated, so edit the script rather than the `.pptx`.** Hand
edits survive until the next `build.py`, which overwrites the file.

## What is on it

| Slides | |
| --- | --- |
| 1–3 | What the project is, and why the port exists — the layout argument, not a language argument. |
| 4–6 | The ten-stage detection chain and what each stage does to the count. |
| 7–12 | **Parity.** The 1.065x gap, that it tracks density rather than seams, how far it has been narrowed, and the per-object Dice figures. |
| 13–15 | The method: three bugs that looked fine, and the fixture-and-mutation rule that followed. |
| 16–20 | Using it, the two things users get wrong, the speed figures, and where to read next. |

The parity gap is stated on slide 8, not buried in a limitations slide at the
end. A reader will open `DIVERGENCES.md`, and a deck that appeared to hide it
would be worth less than one that leads with it.

## The rule these scripts follow

Every number on a slide comes from a recorded measurement, and `figures.py`
names the source of each in the function that draws it. Nothing is estimated
for effect. This is the same rule [`docs/benchmarks/plot.py`](../docs/benchmarks/plot.py)
runs on, and the deck borrows that file's palette so the two cannot drift into
different visual languages.

## On `check.py`

There is no LibreOffice on the machine this was built on, so the deck could not
be rendered and looked at — which is what this project's rule for anything
visual asks for. `check.py` is the substitute and is deliberately narrow: it
finds shapes running off the slide and text too large for its box, which is
where a *generated* deck actually goes wrong. It found one real fault, a stat
tile whose value wrapped onto its own label.

**A clean run is not evidence the deck looks good.** It cannot judge whether a
slide reads well. Open it once in Impress, PowerPoint or Google Slides before
presenting.
