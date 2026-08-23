# Evolution — what we learned, and what it cost

The lessons that outlived the commit that taught them.

`git log` already holds what changed and why, at length: the commit messages
here run to thirty lines and carry the reasoning. **This file does not repeat
them.** It exists for the two things git preserves badly:

- **A lesson that recurs.** The same mistake made in three places is one
  entry here, not three commits to notice a pattern in.
- **A change that was reverted.** Work that did not survive leaves no code
  behind, so the only trace is a commit message filed under some other
  title — "four changes it did not want" is inside a commit about the stage
  table. Nobody finds that a month later, and the experiment gets repeated.

Read `MEMO.md` for where the project stands, `ROADMAP.md` for the plan,
`VIEWER.md` for the viewer, `../DIVERGENCES.md` (repo root) for deliberate
differences from QuPath. This is the layer under all four: why the approach is what it is.

## How to read an entry

Newest first. Each names the commit that taught it, so `git show` gives the
full account. An entry earns its place by being **transferable** — something
that would change what the next person tries. A fix that is only interesting
inside one function belongs in that function's doc comment, not here.

---

## 2026-08 — Removing the reason not to is not a reason to

`PENDING`

`VIEWER.md` §8.8's stored image metadata block sat for a long time as *open*:
the block was not written, and the stated obstacle was that nobody had compared
this crate's level table against the one OpenSlide hands QuPath. The comparison
was then made — `parity/java/OpenSlideLevels.java` against
`parity_openslide_levels.rs`, two slides, every field agreeing — and the entry
was rewritten as "a decision, not a blocker", which felt like progress.

It was, but not toward writing the block. The comparison removed the *reason not
to* and was quietly read as an argument *for*, and those are different things.
Once the numbers agreed there was a pull to write the block because the work of
checking had been done — the evidence-gathering became its own justification.

What settles it is that the two sides of the trade are not the same kind of
thing. Writing the block buys a **performance optimisation**: QuPath does not
open the slide to learn a level table it could have been told, which is one file
open per entry on first open of a project. Not writing it avoids **silent wrong
pixels**: `setMetadata` *replaces* what OpenSlide read, so a block that goes
stale — the file replaced, re-scanned, swapped for a re-export — is the wrong
level table applied to the right path, with nothing anywhere to flag it. A
correctness risk does not trade at par against milliseconds, however carefully
the milliseconds were measured.

It is also §8.3b's rule pointed outward, which is what makes it recur rather
than being one decision. `project::read` keeps every byte it did not touch,
because a rebuilt index silently loses what it did not understand. Writing a
metadata block is claiming authority over a fact this application will not be
around to maintain — the same mistake, in the other direction.

**Transferable:** when a check comes back clean, ask what the clean result
changed. It usually converts *precaution* into *preference*, which is worth
having — the judgement is now made with the information rather than for lack of
it — but a preference still needs its own argument. Write the decision down with
the conditions that would reverse it (§8.8 names three: a vendor QuPath gets
wrong, a path that cannot go stale, a slide QuPath cannot read at all) so the
next person inherits a *decided* item rather than a *pending* one and re-opens
it on evidence rather than on appetite.

**Related:** the entry on blockers below — a note that records a conclusion
instead of the test behind it. This is the mirror: a measurement that records a
result without recording what question it answered.

## 2026-08 — Decompiled bytecode is a source you can misread, not a source of truth

`cd18b4e`

`smoothBoundaries` calls ImageJ's `getInterpolatedPolygon`, whose source is not
in the checkout — only the compiled `ij-1.54p` jar. `javap -c` gives the whole
method, so it looked like the problem was solved: read the bytecode, port it.

The first two branches of that method are

```text
0: dload_2        // interval
1: dconst_0
2: dcmpg
3: ifge 10        // -> push 0
6: iconst_1       // else push 1
```

which is `allowToAdjust = interval < 0`. It was read as `interval >= 0`, the
natural way round for a flag named "allow", and that inversion enables a
50-iteration convergence loop on **every** call. It does not fail loudly: a
unit square converges on an interval near 0.87 and comes back with four
vertices in roughly the right places, all slightly wrong. Hours went into the
convergence loop's control flow — which was correct — because the failure
looked like a convergence bug.

What settled it took one file: call the real `getInterpolatedPolygon` on a 3x2
rectangle at 1.0, 3.0 and 0.7, and look at the step lengths. They are exactly
1.0, 3.0 and 0.7, with a short final segment. That is only possible if the
convergence loop never runs, which pins the flag's sense in one observation
and says nothing about the loop at all.

**Transferable:** `javap -c` output reads like source and is not source — every
comparison in it is a sense you can invert, with no name, no comment and no
type to catch you. It is worth having, but as a *hypothesis generator*: the
thing that decides between two readings is running the method and looking at
what comes out. This is `read-it-with-their-reader` arriving from a third
direction — first their reader over our files, then their writer to get theirs,
now their function to settle what their code means. It also cost the most of
the three, because a decompiler feels authoritative in a way a summary does
not, so the reading went unquestioned longest.

**The corollary is where the fixture earned its keep.** `SmoothBoundaries.json`
records every intermediate of the chain, not just the polygon that comes out.
That is what made this debuggable at all: the failure reported
`single_pixel/interpolated: point 1 is (1.132, 2), QuPath has (1, 2)` — first
stage, first case, one coordinate. Had only the final polygon been recorded,
the same bug would have surfaced four stages downstream as a different point
count, and the interval's sign is not what anyone would have suspected.

## 2026-08 — A blocker is a claim about a test that was run

`875dd7d`, `94cfdc4`

Two items in `VIEWER.md` §8.8 — the stored image metadata block, and opening an
image from a project — sat marked blocked for several sessions on "a native
library, and there is none on this machine". The check behind that sentence was
`ldconfig` and `apt`. **QuPath uses neither.** `qupath-extension-openslide`
depends on `io.github.qupath:openslide:<version>:<platform>`, a jar carrying its
own `libopenslide.so` that JNA extracts at load time. Nothing needed installing.
OpenSlide 4.0.1 loaded on the first try and read the Aperio slides, and both
items were done in an afternoon.

The mistake is not that the wrong command was run. It is that the note recorded
a **conclusion** — "there is none on this machine" — where the evidence
supported only "no *system* library". A system package and a bundled native have
the same file name and are different artefacts. Written the other way round, "no
`libopenslide` in `ldconfig`" would have been re-checkable by anyone who
wondered, and the gap between that and what QuPath loads is one grep of a
`build.gradle`.

It cost more than the two items. §8.8 had also reasoned *forward* from the
blocker to a second claim — that opening the image "would be testing OpenSlide
rather than the format" — and that was wrong too: every field the recorder now
checks is one a bad project file would corrupt, and the pixels are what separate
a project QuPath can *work in* from one it can merely list. A blocker left
standing grows justifications.

**Transferable:** a blocker is a claim about a specific test, so write down which
test. Before believing one, find the line of code that does the loading and check
*that* artefact — not the system's version of something with the same name. The
same suspicion is worth spending on the blockers that survive: driving the real
window was re-checked in the same session and is genuinely blocked (no
`xdotool`, and installing needs a password), which is now a stronger statement
than it was.

**Related:** the entry below, which is the same shape one level over — there the
capability existed next door, here the artefact existed inside a jar. And
`read-the-layer-below`, where the work had already been done underneath. Three
instances now of *the thing you are blocked on already exists somewhere you did
not look*.

---

## 2026-08 — A warning written down is not a warning read

`0459585`

The metadata editor's first render was wrong three times over, and **each of
the three was already documented in the file being edited**:

- it read as transparent, because `egui` fades a modal in as it fades a popup
  in — the same finding, with its settling loop, thirty lines up in
  `examples/chrome`;
- the remove button drew as `□`, `✕` not being in the default font —
  `row_picture` had already chosen line segments over a glyph and says why in a
  comment in the same function's file;
- a `TextEdit`'s `desired_width` inside a `Grid` is a desire — the third
  member of §8.1c's width family, which has its own heading.

Nothing about these was unknown. What failed is that a comment explaining why
*this* code is the way it is does not announce itself to somebody writing new
code beside it — it reads as history, not as a rule that still binds.

**Transferable:** before adding a widget, grep the file for the trap rather
than trusting recall of it. The three greps that would have paid here are the
symbol characters in string literals, `desired_width` near `Grid`, and
`predicted_dt`. A codebase with good comments still needs the search, because
the comment is filed under the old bug and not under the new one.

**Related:** `Looking at a thing is a test that assertions are not`, below —
this is its sequel. Looking is what found all three; the cost was that looking
had to find what reading had already recorded.

## 2026-08 — The instrument that checks appearance can itself be wrong

`a34c40f`

`examples/chrome` is what "check appearance by looking" runs on: no window, no
GPU, `egui` laid out and rasterised through the software compositor. It has
found four real interface bugs and the entry below is about that.

Pressing the sort combo produced a picture with two symptoms — two labels
missing their first letter, and the rows behind the menu showing through it —
and **both were the example's, not the interface's**:

- it **skipped every partial texture delta**. `egui` sends the font atlas whole
  once and patches it afterwards; the comment said a render from scratch has
  nothing for a patch to apply to, and the first frame's whole-image set is
  exactly what it applies to. The two glyphs that arrived in one 35x11 patch
  were `P` and `E`, so exactly two words lost exactly their first letter;
- and it **photographed a popup mid-fade**. Not transparent — 44% opaque, one
  frame after the click, composited correctly.

**Transferable, and it is the sharper half of "render it and look":** an
offscreen render is a reimplementation of the host's renderer, and it fails in
ways that look like layout bugs rather than like missing pixels. A missing
glyph reads as a clipped label; an unfinished animation reads as a wrong
colour. So when a picture shows something wrong, **ask whether the picture is
wrong before changing the widget** — and answer it by reading the pixels rather
than the widget. Dumping the vertex alphas took a minute and said `{0, 112}`;
dumping the texture deltas took a minute and said `PARTIAL`. Neither was
reached for until an hour of reasoning about `ComboBox::width` had run out.

**Cost:** an hour, and a corrected diagnosis committed in `VIEWER.md` §8.3
before it went any further. **Related:** `MEMO.md` §4's second variant, where a
profile share was read as though it said *why* and the ten-minute experiment
that answered it was never run. Same shape, and the interface has it too.

## 2026-08 — A missing file is not a missing capability

`86d74e7`

`VIEWER.md` §8.8 kept the project browser's thumbnail under "the same missing
JPEG encoder as ever" for several sessions. The picture was assumed to be the
entry's `data/<id>/thumbnail.jpg`, this workspace has no JPEG codec, therefore
no pictures. Every clause of that is true and the conclusion was wrong.

QuPath's browser asks `entry.getThumbnail()`, and **when the answer is null it
opens the image and makes one**. The stored file is a cache. The capability the
entry was waiting on had been in the repository since §8.4 — the overview pane
draws exactly this picture, from the coarsest pyramid level, for the slide that
is open.

**Transferable:** when a feature is blocked on an artefact this cannot produce,
go and read what the other implementation does *when that artefact is absent*.
A format's optional file is optional for a reason, and the reason is usually
that somebody already wrote the fallback. The same shape as the memory
`read-the-layer-below`, one level sideways: there the work had already been
done underneath, here it had already been done next door.

**Cost of the mistake:** nothing was built wrong — the browser simply listed
names for four sessions while the answer sat in `ProjectBrowser.java`, thirty
lines from the class the format was ported from.

**The encoder itself has since landed** (`e0d9fb8`), which does not change the
lesson: it was never what the browser was waiting on, and writing it is a debt to
*QuPath* — the file is a cache, so a study opened there need not re-read every
slide. It brought its own version of the §6 trap. `Thumbnail::pixels` is RGBA
because a texture upload wants it and JPEG has no alpha, and handing four
channels to an encoder expecting three does not fail: it reads each pixel's
fourth byte as the next one's first, producing a valid JPEG of the right size in
false colour. The size-only test passes on that. Only decoding and comparing
against the source pixels catches it — confirmed by reintroducing the bug and
watching one test go red while the other stayed green.

**The sharper half:** §8.3's own first paragraph had planned it correctly —
"thumbnails from a `TileCache` ... the thumbnail generator is already written".
A later session read the four files an entry has on disk, found the one this
cannot write, and talked the plan out of a picture it already knew how to make.
A plan is worth re-reading before a blocker is believed, because the blocker is
often newer than the plan.

## 2026-08 — A widget that will not be narrow is a panel that clips

`86d74e7`, `a14dc59`

Third occurrence, which is the one that earns an entry. §8.1c had panels whose
contents refused the cap that bound them; §8.7 had slider labels laid out inline
and clipped to `Bac`; now a browser row's label, which stopped the *pane's
heading* from wrapping.

The mechanism is one line of egui and worth writing down: **inside a horizontal
layout a `Label` defaults to `TextWrapMode::Extend`**, so its desired width is
its whole text. A `SidePanel` sizes itself from its contents, grows to
`max_width`, and clips there. So a widget added to *one row* changes the width
every other widget in the panel is laid out against, and the symptom appears
three widgets away from the change.

**Transferable:** in a content-sized panel, every widget's *minimum* width is a
shared resource. When a panel starts clipping, the thing to look for is not the
clipped widget but whichever widget can no longer be narrow — and the fix is
always to let the new content wrap rather than to widen the cap.

## 2026-08 — Looking at a thing is a test that assertions are not

`a14dc59`, `25abc5d`

Four real interface bugs shipped under a green test suite: clipped slider
labels, a `▸` with no glyph in the atlas, a checkbox whose effect lived in a
closed panel, and a Reset button that read as another slider row.

The layout tests passed the whole time, and they were not weak tests. **They
asserted on rectangles, and the rectangles were correct.** What was wrong was
what the rectangles looked like once filled — text longer than its box, a glyph
the font did not have, a control in a panel nobody had opened.

Nothing in the project had ever looked at a pane. `examples/chrome` lays the
interface out with egui and rasterises it through the same software path the
application uses on a machine with no GPU, so no window and no adapter is
needed. Four bugs in the first look.

**Transferable:** when the output is an appearance, an assertion on geometry
is a proxy, and proxies fail quietly. Render it and look. The corollary is in
`VIEWER.md` §6 and it is stronger than it sounds — offscreen renders are still
not the running application, and hover, resize and trackpad remain unlooked-at.

**Related:** the checkbox bug is the same shape as the memory
`test-where-state-ends-up` — the test asserted the gesture (`show_appearance`
became true) rather than the property that matters
(`show_appearance && show_metadata`).

## 2026-08 — A cheap level to draw is not a fine enough level to measure

`a14dc59`

Detection read the pyramid level the viewer was drawing. That level is chosen
to keep *drawing* cheap and says nothing about whether a nucleus survives it.
At CMU-1's level 2 a pixel is 8 µm and a nucleus is 5–10, so a run started
while zoomed out found clumps of stained tissue and reported them as an object
count.

The evidence had been sitting in the module header, written down and read as a
curiosity: a fitted whole-slide view found **1638** objects where one screenful
of tissue found **1357**. The whole slide holds far more nuclei than one
screenful — the coarse run was not finding them.

**Transferable:** parameters in physical units are not enough. The pipeline
converted microns correctly at every level; what no conversion can do is put
back evidence the downsampling removed. When a unit conversion makes a
computation *arithmetically* valid at any scale, ask separately whether the
evidence survives that scale.

**And:** a number you wrote down and shrugged at is a bug report. Both figures
were in the file, in a paragraph explaining something else.

## 2026-08 — Verify against the artefact, not against the assumed artefact

`a14dc59`

The tests for the level cap were first written against an assumed CMU-1
pyramid: four levels, powers of four, level 1 clearing the limit at
0.998 µm/px. The real file has **three** levels — 0.499, 1.996, 7.984 µm/px —
and level 1 does not clear it. The test failed and the assumption was wrong,
not the code.

**Transferable:** a fixture invented from a plausible model of the data will
agree with code written from the same model, and both can be wrong together.
Read the real file once. It cost one throwaway example binary here.

## 2026-08 — A cap documented as never firing becomes load-bearing silently

`a14dc59`

`MAX_PIXELS` carried a paragraph explaining that it does not fire in practice,
which was true: the run read the level the viewer drew, so the pixels behind a
window stayed near the window's size at any zoom.

The level cap invalidated that in one line. A zoomed-out run now reads level 0,
a fitted view of CMU-1 is 1444 Mpx, and the cap became the only thing standing
between the button and a fifteen-second freeze — reading the middle 21% of the
slide instead.

**Transferable:** "this guard never fires" is a statement about the *callers*,
not the guard. It expires the moment a caller changes, and nothing fails when
it does. Grep for the guard's own justification when changing what feeds it.

## 2026-08 — Loop-level tuning ran out before memory traffic did

See the memory `loop-level-tuning-is-exhausted`, and `ROADMAP.md` §5.

Inner-loop rewrites came back at ~1.0x in place, repeatedly, across several
stages. What remained was memory traffic and doing less work — which is
`SKILL.md` §0's stated order, arrived at the expensive way.

**Transferable:** the ordering in §0 is not advice, it is a measured property
of this codebase. Starting at SIMD means measuring 1.0x several times first.

## 2026-08 — Two-binary A/B has a ~5% floor here

See the memory `measure-on-the-saturated-grid`.

Comparing two binaries cannot resolve anything under about 5% on this machine;
4-tile runs use only P-cores and flatter the result. Sweeping a runtime
parameter within one binary is the measurement that works.

**Transferable:** below 5%, a two-binary comparison reports noise with a
confident sign. This is why `ROADMAP.md` §5 asks for the same machine, the
same thread count and the same fixture on every claim.

## 2026-08 — The argument a port hardcodes is the one the next caller needs

`make_float_edm` fixed ImageJ's background value at `0` and said why in its own
doc comment: "a port of the branches nothing calls is a port nothing checks".
That was correct when written — the shape split is the only EDM the pipeline
had. Cell expansion calls the same ImageJ method with `(byte)255`, which does
not merely vary a parameter: it *inverts* which pixels the distance is measured
from, so reusing the `0` call would have produced a distance map of the wrong
region entirely and cells that are the gaps between nuclei.

Nothing would have failed loudly. The 0-background call returns a plausible
float image of the right shape, and without `CellExpansion.json` the port would
have looked like it worked.

**Transferable:** "this argument is always X here" and "this guard never fires"
are the same claim about callers, and they expire the same way — see the
2026-08 entry above and the memory `read-the-layer-below`. When porting a new
caller of an already-ported function, re-read the *original* signature, not the
port's. The port records the old caller's choices, and a hardcoded constant is
exactly where a new caller's difference hides.
