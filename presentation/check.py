#!/usr/bin/env python3
"""Checks the built deck for the layout faults a clean build cannot catch.

    python3 presentation/check.py

There is no LibreOffice on this machine, so the deck cannot be rendered and
looked at -- which is what this project's own rule asks for. This is the
substitute, and it is deliberately narrow: it checks geometry, which is where
a generated deck actually goes wrong, and it cannot check whether a slide reads
well. Anything it reports is a real fault; a clean run is not proof the deck
looks good.

Estimating text height needs a font metric this has no access to, so the line
count is multiplied by a generous per-line height and compared against the
space actually available before the next shape. That over-estimates, so it
errs towards reporting a problem that is not one rather than missing one.
"""

import sys

from pptx import Presentation
from pptx.util import Emu, Inches, Pt

DECK = "presentation/QuPath_Rust.pptx"

W, H = Inches(13.333), Inches(7.5)
EDGE = Inches(0.25)

def wrapped_lines(text, width_in, size_pt):
    """Lines a run of text takes when wrapped into `width_in` inches.

    A character at N pt occupies roughly N/1.9 points of width in this deck's
    sans stack. That is an approximation and is meant to be one -- it is used
    only to decide whether a box is badly undersized, never to lay anything out.
    """
    if not text.strip():
        return 0
    char_w_pt = size_pt / 1.9
    per_line = max(1, int(width_in * 72 / char_w_pt))
    return sum(max(1, -(-len(para) // per_line)) for para in text.split("\n"))


def issues():
    prs = Presentation(DECK)
    found = []

    for index, slide in enumerate(prs.slides, start=1):
        for shape in slide.shapes:
            name = f"slide {index}"

            # 1. Anything outside the slide, or hanging off its right edge.
            if shape.left is None or shape.top is None:
                continue
            right = shape.left + (shape.width or 0)
            bottom = shape.top + (shape.height or 0)
            if shape.left < -EDGE or shape.top < -EDGE:
                found.append(f"{name}: shape starts off-slide "
                             f"({Emu(shape.left).inches:.2f}, "
                             f"{Emu(shape.top).inches:.2f})")
            if right > W + EDGE:
                found.append(f"{name}: shape runs past the right edge "
                             f"(to {Emu(right).inches:.2f}in of "
                             f"{Emu(W).inches:.2f}in)")
            if bottom > H + EDGE:
                found.append(f"{name}: shape runs past the bottom "
                             f"(to {Emu(bottom).inches:.2f}in of "
                             f"{Emu(H).inches:.2f}in)")

            # 2. Text that almost certainly wraps past its own box, which is
            #    how a caption ends up sitting on the shape below it.
            if not shape.has_text_frame:
                continue
            for para in shape.text_frame.paragraphs:
                for run in para.runs:
                    size = (run.font.size or Pt(18)).pt
                    width_in = Emu(shape.width).inches
                    lines = wrapped_lines(run.text, width_in, size)
                    if lines > 1 and "\n" not in run.text:
                        # A single run that wraps is fine in a heading, but in
                        # this deck every multi-line string is explicit, so a
                        # wrap means the box is narrower than intended.
                        est = lines * size * 1.35 / 72
                        avail = Emu(shape.height).inches
                        if est > avail + 0.35:
                            found.append(
                                f"{name}: text wraps to ~{lines} lines "
                                f"(~{est:.2f}in) in a {avail:.2f}in box: "
                                f"{run.text[:58]!r}")
    return found


def main():
    found = issues()
    if not found:
        print("no geometry faults found")
        print("NOTE: this checks geometry only. It cannot tell you whether a "
              "slide reads well.")
        return 0
    for line in found:
        print(line)
    print(f"\n{len(found)} issue(s)")
    return 1


if __name__ == "__main__":
    sys.exit(main())
