#!/usr/bin/env python3
"""Builds the slide deck.

    python3 presentation/figures.py     # first, if the figures are stale
    python3 presentation/build.py

Every figure and every number on a slide comes from a recorded measurement
named in `figures.py` or in the docs this deck points at. The rule the rest of
the repository runs on applies here too: a claim with nothing behind it does
not go on a slide.

Written with python-pptx so the deck is regenerable. Editing the .pptx by hand
works, but the next run of this script overwrites it -- change the script.
"""

from pptx import Presentation
from pptx.dml.color import RGBColor
from pptx.enum.text import MSO_ANCHOR, PP_ALIGN
from pptx.util import Emu, Inches, Pt
from PIL import Image

OUT = "presentation/QuPath_Rust.pptx"

# figures.py's palette, which is docs/benchmarks/plot.py's.
QUPATH = RGBColor(0x4A, 0x7C, 0x59)
RUST = RGBColor(0xC1, 0x55, 0x3B)
INK = RGBColor(0x2B, 0x2B, 0x2B)
MUTED = RGBColor(0x6E, 0x6A, 0x66)
PALE = RGBColor(0xF4, 0xF2, 0xEF)
WHITE = RGBColor(0xFF, 0xFF, 0xFF)

# 16:9. QuPath's own documentation screenshots are 4:3, but a deck is projected.
W, H = Inches(13.333), Inches(7.5)

MARGIN = Inches(0.72)
BODY_W = W - 2 * MARGIN


def deck():
    prs = Presentation()
    prs.slide_width, prs.slide_height = W, H
    return prs


def blank(prs):
    """A slide with no placeholders at all.

    Layout 6 is python-pptx's blank. Using a titled layout and then leaving the
    placeholder empty leaves "Click to add title" in the file, which shows up in
    the editor and in some renderers.
    """
    return prs.slides.add_slide(prs.slide_layouts[6])


def box(slide, left, top, width, height, text, size, *, color=INK, bold=False,
        align=PP_ALIGN.LEFT, anchor=MSO_ANCHOR.TOP, spacing=1.0, font=None):
    """A text box that does not autofit, so nothing silently resizes."""
    shape = slide.shapes.add_textbox(left, top, width, height)
    frame = shape.text_frame
    frame.word_wrap = True
    frame.vertical_anchor = anchor
    frame.margin_left = frame.margin_right = 0
    frame.margin_top = frame.margin_bottom = 0

    lines = text.split("\n")
    for i, line in enumerate(lines):
        para = frame.paragraphs[0] if i == 0 else frame.add_paragraph()
        para.alignment = align
        para.line_spacing = spacing
        run = para.add_run()
        run.text = line
        run.font.size = Pt(size)
        run.font.bold = bold
        run.font.color.rgb = color
        if font:
            run.font.name = font
    return shape


def rule(slide, top, *, color=RUST, width=Inches(1.5), thickness=Pt(3)):
    """The short accent rule under a title. Purely a visual anchor."""
    from pptx.enum.shapes import MSO_SHAPE

    shape = slide.shapes.add_shape(MSO_SHAPE.RECTANGLE, MARGIN, top,
                                   width, thickness)
    shape.fill.solid()
    shape.fill.fore_color.rgb = color
    shape.line.fill.background()
    shape.shadow.inherit = False
    return shape


def band(slide, left, top, width, height, color):
    from pptx.enum.shapes import MSO_SHAPE

    shape = slide.shapes.add_shape(MSO_SHAPE.RECTANGLE, left, top, width, height)
    shape.fill.solid()
    shape.fill.fore_color.rgb = color
    shape.line.fill.background()
    shape.shadow.inherit = False
    return shape


def title_slide(prs, title, subtitle, footer):
    slide = blank(prs)
    band(slide, 0, 0, W, Inches(0.32), RUST)
    box(slide, MARGIN, Inches(2.05), BODY_W, Inches(1.3), title, 46, bold=True)
    box(slide, MARGIN, Inches(3.45), BODY_W, Inches(1.5), subtitle, 21,
        color=MUTED, spacing=1.25)
    box(slide, MARGIN, H - Inches(1.15), BODY_W, Inches(0.6), footer, 13,
        color=MUTED)
    return slide


def section(prs, kicker, title):
    slide = blank(prs)
    band(slide, 0, 0, W, H, PALE)
    band(slide, 0, 0, Inches(0.3), H, RUST)
    box(slide, MARGIN, Inches(2.9), BODY_W, Inches(0.5), kicker.upper(), 14,
        color=RUST, bold=True)
    box(slide, MARGIN, Inches(3.4), BODY_W, Inches(1.4), title, 36, bold=True)
    return slide


def heading(slide, title, subtitle=None):
    box(slide, MARGIN, Inches(0.52), BODY_W, Inches(0.7), title, 29, bold=True)
    rule(slide, Inches(1.28))
    if subtitle:
        box(slide, MARGIN, Inches(1.52), BODY_W, Inches(0.5), subtitle, 15,
            color=MUTED)
    return Inches(2.12) if subtitle else Inches(1.75)


def bullets(slide, top, items, *, size=17, width=None, left=None, gap=0.30):
    """Bullets as separate boxes, so a lead-in can be bold and its gloss not.

    `items` is a list of (lead, gloss). An empty gloss is a plain line.
    """
    left = MARGIN if left is None else left
    width = BODY_W if width is None else width
    y = top
    for lead, gloss in items:
        band(slide, left, y + Inches(0.09), Inches(0.075), Inches(0.16), RUST)
        text_left = left + Inches(0.28)
        text_w = width - Inches(0.28)
        shape = box(slide, text_left, y, text_w, Inches(0.4), lead, size,
                    bold=True)
        y += Inches(0.34)
        if gloss:
            shape = box(slide, text_left, y, text_w, Inches(0.4), gloss,
                        size - 3, color=MUTED, spacing=1.18)
            # Two lines of gloss need the room a one-line box does not.
            y += Inches(0.30 * (1 + gloss.count("\n")))
        y += Inches(gap)
    return y


def picture(slide, path, top, *, max_h, center_on=None, max_w=None):
    """Place an image scaled to fit a box, preserving its aspect ratio.

    The five figures have five different aspect ratios, so a fixed width and
    height would stretch four of them. Fitting is the whole reason this helper
    exists.
    """
    with Image.open(path) as im:
        aspect = im.size[0] / im.size[1]
    max_w = BODY_W if max_w is None else max_w
    height = max_h
    width = Emu(int(height * aspect))
    if width > max_w:
        width = max_w
        height = Emu(int(width / aspect))
    span = W if center_on is None else center_on
    left = Emu(int((span - width) / 2)) if center_on is not None else \
        Emu(int((W - width) / 2))
    return slide.shapes.add_picture(path, left, top, width=width, height=height)


def figure_slide(prs, title, subtitle, image, caption):
    slide = blank(prs)
    top = heading(slide, title, subtitle)
    # Reserve the caption's band before sizing the picture, so a tall figure
    # cannot grow into the text underneath it.
    cap_top = H - Inches(1.30)
    picture(slide, image, top + Inches(0.06),
            max_h=cap_top - top - Inches(0.28))
    box(slide, MARGIN, cap_top, BODY_W, Inches(0.9), caption, 14,
        color=MUTED, spacing=1.2)
    return slide


def stat_row(slide, top, stats, *, height=Inches(1.5)):
    """Big-number tiles. `stats` is a list of (value, label, colour)."""
    n = len(stats)
    gap = Inches(0.26)
    width = Emu(int((BODY_W - gap * (n - 1)) / n))
    inner = Emu(width - Inches(0.44))
    for i, (value, label, colour) in enumerate(stats):
        left = MARGIN + Emu(int(i * (width + gap)))
        band(slide, left, top, width, height, PALE)
        band(slide, left, top, width, Inches(0.06), colour)
        # Each value sizes itself to its own tile rather than every tile
        # sharing one point size. "1.012-1.129" is eleven glyphs where "1.065x"
        # is six, and at a shared 33pt it wrapped onto the label beneath it.
        # Shrinking only the long one would leave four mismatched sizes; this
        # way the rule is uniform even though the results differ.
        size = 33.0
        while size > 17 and len(value) * (size / 1.9) / 72 > Emu(inner).inches:
            size -= 0.5
        box(slide, left + Inches(0.22), top + Inches(0.26),
            inner, Inches(0.7), value, size, bold=True,
            color=colour)
        box(slide, left + Inches(0.22), top + Inches(0.94),
            inner, Inches(0.5), label, 12.5, color=MUTED, spacing=1.15)


def table_slide(slide, top, headers, rows, widths, *, size=14):
    """A hand-drawn table.

    python-pptx's real tables inherit a theme style with banding and a heavy
    header that fights everything else in the deck, and restyling one cell by
    cell is more code than drawing it.
    """
    total = sum(widths)
    xs, x = [], MARGIN
    for w in widths:
        xs.append(x)
        x += Emu(int(BODY_W * w / total))

    y = top
    for i, head in enumerate(headers):
        w = Emu(int(BODY_W * widths[i] / total))
        box(slide, xs[i], y, w - Inches(0.14), Inches(0.36), head, size - 1,
            bold=True, color=RUST)
    y += Inches(0.40)
    band(slide, MARGIN, y, BODY_W, Pt(1), RGBColor(0xD6, 0xD3, 0xCD))
    y += Inches(0.16)

    for row in rows:
        tallest = Inches(0.34)
        for i, cell in enumerate(row):
            w = Emu(int(BODY_W * widths[i] / total))
            lines = 1 + cell.count("\n")
            box(slide, xs[i], y, w - Inches(0.14), Inches(0.34 * lines), cell,
                size, color=INK if i == 0 else MUTED,
                bold=(i == 0), spacing=1.15)
            tallest = max(tallest, Inches(0.30 * lines))
        y += tallest + Inches(0.20)
    return y


def build():
    prs = deck()

    # ------------------------------------------------------------------ 1
    title_slide(
        prs,
        "QuPath_Rust",
        "A stage-verified partial port of QuPath's cell detection\n"
        "— and the apparatus used to verify it",
        "GPLv3 · github.com/Dante-Berth/QuPath_Rust · every number here is a "
        "recorded measurement",
    )

    # ------------------------------------------------------------------ 2
    slide = blank(prs)
    top = heading(slide, "What this is",
                  "Two parts, and the second is the reason it is interesting")
    bullets(slide, top, [
        ("A port of QuPath's WatershedCellDetection",
         "The ten-stage detection chain, with detections stored columnarly — a struct of\n"
         "arrays, one contiguous column per feature, not a per-object string-keyed map."),
        ("A parity harness",
         "Recorders that run inside a live QuPath build and capture its intermediate state\n"
         "at every stage boundary. Committed fixtures. Tests that feed this port the same\n"
         "input at the same boundary and compare pixel for pixel."),
        ("Parity is measured, not assumed — and it is incomplete",
         "The open count gap is stated on slide 6 rather than in a limitations section at\n"
         "the end. It is the first thing a user needs to know."),
    ])

    # ------------------------------------------------------------------ 3
    slide = blank(prs)
    top = heading(slide, "Why port it at all",
                  "Not because Java is slow — Rust vs Java on equivalent code is 1–3x")
    bullets(slide, top, [
        ("QuPath is built for interactive use",
         "A pathologist inspects objects one at a time, and a per-object string-keyed\n"
         "measurement map follows sensibly from that."),
        ("Across hundreds of slides the same layout costs",
         "An allocation and a hash lookup per measurement, and it scatters values that are\n"
         "read together."),
        ("Columnar storage removes that cost",
         "This is the source of the speedup — the data layout, not the language."),
    ])
    band(slide, MARGIN, Inches(5.55), BODY_W, Inches(1.15), PALE)
    box(slide, MARGIN + Inches(0.3), Inches(5.78), BODY_W - Inches(0.6),
        Inches(0.8),
        "The port is not yet a usable substitute for research work: measurements are not\n"
        "implemented, and the object count does not yet match. Both are on slide 6.",
        14, color=MUTED, spacing=1.2)

    # ------------------------------------------------------------------ 4
    section(prs, "Part one", "The port")

    # ------------------------------------------------------------------ 5
    slide = blank(prs)
    top = heading(slide, "The detection chain",
                  "Every stage pinned against output recorded from a real QuPath build")
    left_items = [
        ("1 · Colour deconvolution", "RGB to haematoxylin/eosin optical density"),
        ("2 · Background subtraction", "Opening by reconstruction — a top hat"),
        ("3 · Gaussian blur", "ImageJ's, including its edge handling"),
        ("4 · Laplacian of Gaussian", "Boundary falls at zero, not a tuned constant"),
        ("5 · Regional maxima", "One marker per nucleus, 8-connected"),
    ]
    right_items = [
        ("6 · Labelling and watershed flood", "4-connected, thresholded at zero"),
        ("7 · Filled-region tracing", "Against ImageJ's own Wand"),
        ("8 · Mean-intensity filter", "Pinned on 17 recorded images"),
        ("9 · Merge, shape split, refineBoundary", "EDM and MaximumFinder"),
        ("10 · Area filter and seam dedup", "On QuPath's own tile grid"),
    ]
    half = Emu(int((BODY_W - Inches(0.5)) / 2))
    bullets(slide, top, left_items, width=half, size=15, gap=0.22)
    bullets(slide, top, right_items, width=half,
            left=MARGIN + half + Inches(0.5), size=15, gap=0.22)

    # ------------------------------------------------------------------ 6
    figure_slide(
        prs,
        "Every stage moves the count",
        "One 4000×4000 region of CMU-1, from `cargo run --example region`",
        "presentation/figures/stages.png",
        "The split raising the count by 74% is not a bug — it is what pulling touching nuclei "
        "apart does.\nBecause each stage moves the count, a ladder of stage boundaries can "
        "localise where two pipelines diverge.",
    )

    # ------------------------------------------------------------------ 7
    section(prs, "Part two", "Parity — and the gap")

    # ------------------------------------------------------------------ 8
    slide = blank(prs)
    top = heading(slide, "The headline, stated plainly",
                  "Ten regions of CMU-1, chosen by the slide rather than by hand")
    stat_row(slide, top, [
        ("1.065x", "objects found vs QuPath cells,\npooled over ten regions", RUST),
        ("1.012–1.129", "range by region — and always\nin the same direction", RUST),
        ("97.9%", "of QuPath's 17,079 nuclei\nmatched one-to-one", QUPATH),
        ("0.921", "mean Dice over those\nmatched pairs", QUPATH),
    ])
    box(slide, MARGIN, Inches(4.15), BODY_W, Inches(2.2),
        "So the gap is a population of extra objects on sparse tissue —\n"
        "not boundaries falling in the wrong place.",
        24, bold=True, spacing=1.25)
    box(slide, MARGIN, Inches(5.55), BODY_W, Inches(1.3),
        "A single region was quoted for several sessions — 1.035x. That number is still exactly "
        "right, and it\nwas the wrong number to quote: it is one region, measured once, and it is "
        "the best of ten.",
        14, color=MUTED, spacing=1.25)

    # ------------------------------------------------------------------ 9
    figure_slide(
        prs,
        "The gap tracks density, not seams",
        "Each point is one region. Diamonds are single-tile: no seams, no duplicates removed",
        "presentation/figures/density.png",
        "Dense regions agree to within 1–3.5%; sparse ones diverge by 6–13%. The seam count does "
        "not sort the table — density does.\nThat is what rules out tile-seam resolution (D-008) "
        "as the whole cause: three of these regions have no seams at all.",
    )

    # ----------------------------------------------------------------- 10
    figure_slide(
        prs,
        "Walking the ladder splits the gap in two",
        "Both sides' counts at four stage boundaries, three whole regions at 2048×2048",
        "presentation/figures/ladder.png",
        "The first rung is flat across density (1.044 / 1.047 / 1.051), so that ~4.7% is "
        "density-independent and is not what the gap tracks.\nThe maxima and candidate rungs are "
        "identical, which rules out the labelling and the flood. The density split opens at the "
        "mean-intensity filter —\nand that filter is itself pinned on 17 images, so what differs "
        "is not the filter but what it measures.",
    )

    # ----------------------------------------------------------------- 11
    slide = blank(prs)
    top = heading(slide, "Where the search stands",
                  "Each narrowing has cost a hypothesis rather than confirmed one")
    table_slide(slide, top,
                ["Stage", "Status", "How it was ruled out"],
                [
                    ["Colour deconvolution", "Ruled out", "Composed with the decode beneath it, on real tissue"],
                    ["JPEG decode", "Ruled out", "Against OpenSlide, to within one channel step"],
                    ["Background estimate", "Ruled out", "Bit for bit at 2048×2048, on 12.5 million pixels"],
                    ["Labelling and flood", "Ruled out", "Maxima and candidate rungs are identical"],
                    ["Merge and shape split", "Ruled out", "Pixel for pixel on all eight recorded crops"],
                    ["Blur, Laplacian, regional maxima", "Remaining", "The ~4.7% density-independent excess lives here"],
                    ["What the filter measures", "Remaining", "The density-dependent half — ipToMeasure"],
                ],
                [3.0, 1.6, 5.4])
    box(slide, MARGIN, H - Inches(1.02), BODY_W, Inches(0.6),
        "Seven of ten stages excluded against QuPath on real tissue, pixel for pixel. "
        "Tracked as D-009 in DIVERGENCES.md.",
        14, color=MUTED)

    # ----------------------------------------------------------------- 12
    figure_slide(
        prs,
        "The nuclei themselves, not just how many",
        "Against the mask QuPath recorded for every nucleus it detected, over three single-tile regions",
        "presentation/figures/dice.png",
        "Pooled Dice involves no matching decision, so read it first. Both sides are the pixels "
        "each pipeline actually labelled —\ncomparing QuPath's mask against a rasterisation of our "
        "polygon would have measured our rasteriser instead.\nThe third region has the lowest Dice "
        "and the worst count ratio: the same density-dependent gap, now visible as shape.",
    )

    # ----------------------------------------------------------------- 13
    section(prs, "Part three", "The method")

    # ----------------------------------------------------------------- 14
    slide = blank(prs)
    top = heading(slide, "Three bugs that looked completely fine",
                  "Each produced an entirely plausible image. None would have been caught by a count.")
    bullets(slide, top, [
        ("A morphological opening, where a top hat was wanted",
         "The output is a smooth, believable background estimate either way."),
        ("The DAB stain vector, used under the name EOSIN",
         "A 52% error on the leading coefficient. The channel still looked like a stain channel."),
        ("Aperio JPEG tiles carry no Adobe APP14 marker",
         "So a conforming decoder applies the wrong colour transform. The tissue still looked\n"
         "like tissue."),
    ])
    band(slide, MARGIN, Inches(5.35), BODY_W, Inches(1.45), PALE)
    box(slide, MARGIN + Inches(0.3), Inches(5.60), BODY_W - Inches(0.6),
        Inches(1.1),
        "A count is a one-dimensional projection in which errors cancel.\n"
        "Visual inspection checks whether output is plausible, not whether it is the same.",
        18, bold=True, spacing=1.25)

    # ----------------------------------------------------------------- 15
    slide = blank(prs)
    top = heading(slide, "So parity is the test suite",
                  "The rule CONTRIBUTING.md enforces, and why each half is needed")
    bullets(slide, top, [
        ("Record, don't read",
         "Recorders run inside a live QuPath build and capture its intermediate state at each\n"
         "stage boundary. Checking against a reading of QuPath's source is not a test."),
        ("Compare at the boundary, pixel for pixel",
         "Feed this port the same input at the same stage boundary — so a divergence is\n"
         "localised to one stage instead of showing up as a different total."),
        ("Mutation-check every fixture",
         "No test is trusted until it has been shown able to fail. A fixture that cannot fail is\n"
         "a green light wired to nothing."),
        ("Record the deliberate differences too",
         "Matching QuPath includes matching its mistakes. \"More correct than QuPath\" is a bug\n"
         "here — ten entries in DIVERGENCES.md."),
    ], size=16, gap=0.20)

    # ----------------------------------------------------------------- 16
    section(prs, "Part four", "Using it")

    # ----------------------------------------------------------------- 17
    slide = blank(prs)
    top = heading(slide, "The application",
                  "A viewer, a project browser, detection, and GeoJSON out")
    half = Emu(int((BODY_W - Inches(0.55)) / 2))
    bullets(slide, top, [
        ("Open a slide", "Aperio SVS and tiled TIFF, pure Rust — no OpenSlide, no Java."),
        ("Pan, zoom, detect", "`d` detects in what is on screen, on a thread, with a real\nprogress fraction and a way to stop."),
        ("Save as GeoJSON", "`s` writes what QuPath's importer reads. Level-0 pixels throughout."),
        ("Projects", "Opens and writes QuPath's own .qpproj layout."),
    ], width=half, size=15, gap=0.20)
    pic_left = MARGIN + half + Inches(0.55)
    slide.shapes.add_picture("docs/images/viewer-detail.jpg", pic_left,
                             top + Inches(0.1), width=half,
                             height=Emu(int(half / (900 / 675))))
    box(slide, pic_left, top + Inches(0.1) + Emu(int(half / (900 / 675))) + Inches(0.16),
        half, Inches(0.6),
        "The viewer claims no parity and no speedup — QuPath's viewer is not what is\nbeing ported.",
        12, color=MUTED, spacing=1.2)

    # ----------------------------------------------------------------- 18
    slide = blank(prs)
    top = heading(slide, "Two things users get wrong",
                  "Both produce a plausible answer, which is why they are on a slide")
    band(slide, MARGIN, top, BODY_W, Inches(2.05), PALE)
    box(slide, MARGIN + Inches(0.3), top + Inches(0.26), BODY_W - Inches(0.6),
        Inches(0.45), "Detection does not read the level you are looking at", 20,
        bold=True, color=RUST)
    box(slide, MARGIN + Inches(0.3), top + Inches(0.82), BODY_W - Inches(0.6),
        Inches(1.1),
        "It reads the coarsest level still at 1.0 µm/px or finer. The viewer picks a level to look at; "
        "detection picks one to\nmeasure at. At CMU-1's level 2 a pixel is 8 µm and a nucleus is 5–10, "
        "so what survives the filters is tissue —\nreported as a plausible object count. A fitted "
        "whole-slide view once found 1,638 objects where one screenful found 1,357.",
        14, color=MUTED, spacing=1.25)

    second = top + Inches(2.35)
    band(slide, MARGIN, second, BODY_W, Inches(2.05), PALE)
    box(slide, MARGIN + Inches(0.3), second + Inches(0.26), BODY_W - Inches(0.6),
        Inches(0.45), "What comes out are nuclei, not cells", 20, bold=True,
        color=RUST)
    box(slide, MARGIN + Inches(0.3), second + Inches(0.82), BODY_W - Inches(0.6),
        Inches(1.1),
        "Cell expansion and boundary smoothing are ported and pinned in qupath-watershed, but nothing "
        "in the pipeline\ncalls them. So the outlines are raw pixel boundaries, and the counts on "
        "slide 8 compare our raw nuclei against\nQuPath's smoothed cells. A parameter the pipeline "
        "ignores gets no control in the panel, rather than an inert one.",
        14, color=MUTED, spacing=1.25)

    # ----------------------------------------------------------------- 19
    figure_slide(
        prs,
        "And it is fast — read the middle bar",
        "The 4000×4000 region QuPath was profiled on, same slide, same parameters",
        "presentation/figures/endtoend.png",
        "QuPath's profile was taken single-threaded, and QuPath parallelises across tiles in real "
        "use, so the 15.6x is not the honest comparison.\nRead both bars as generous to us: "
        "QuPath's 15.87 s includes computing the measurements this pipeline does not implement.\n"
        "A 6.5% density-dependent over-count is not a rounding difference — what is fast here is "
        "not yet what QuPath computes.",
    )

    # ----------------------------------------------------------------- 20
    slide = blank(prs)
    top = heading(slide, "Where to go next")
    table_slide(slide, top,
                ["Read", "For"],
                [
                    ["docs/guide/", "The user guide — opening a slide, detecting, exporting"],
                    ["docs/guide/parity.md", "What to trust, before relying on any output"],
                    ["DIVERGENCES.md", "Every deliberate difference from QuPath, D-001 to D-010"],
                    ["docs/MEMO.md", "Twenty minutes of orientation; §2 is the gap"],
                    ["CONTRIBUTING.md", "The fixture-and-mutation rule for a new parity claim"],
                ],
                [3.1, 6.9], size=15)
    band(slide, MARGIN, Inches(5.5), BODY_W, Inches(1.25), PALE)
    box(slide, MARGIN + Inches(0.3), Inches(5.78), BODY_W - Inches(0.6),
        Inches(0.9),
        "The repository is usable as a worked example of stage-boundary verification\n"
        "as much as it is a detection library.",
        18, bold=True, spacing=1.25)

    prs.save(OUT)
    print(f"wrote {OUT} — {len(prs.slides._sldIdLst)} slides")


if __name__ == "__main__":
    build()
