#!/usr/bin/env python3
"""Regenerates the figures in the slide deck.

Same rule as docs/benchmarks/plot.py, which this borrows its palette from:
every number is copied from a recorded measurement and the source is named
beside it. Nothing is estimated. A figure with no recording behind it does
not go in the deck.

    python3 presentation/figures.py
"""

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

OUT = "presentation/figures"

# docs/benchmarks/plot.py's palette, unchanged, so the deck and the committed
# charts in the repository cannot drift into two different visual languages.
QUPATH = "#4a7c59"
RUST = "#c1553b"
GRID = "#d6d3cd"
INK = "#2b2b2b"
MUTED = "#8a8580"

DPI = 200


def style(ax):
    ax.set_axisbelow(True)
    ax.grid(axis="y", color=GRID, linewidth=0.7)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax.spines[side].set_color(GRID)
    ax.tick_params(colors=INK, length=0)


def save(fig, name):
    path = f"{OUT}/{name}.png"
    fig.savefig(path, dpi=DPI, bbox_inches="tight", facecolor="white")
    plt.close(fig)
    print(f"wrote {path}")


def density():
    """The count ratio against cell density, which is the deck's core claim.

    docs/MEMO.md §2. Ten regions of CMU-1 ranked by the slide rather than by
    hand; the three single-tile regions are marked because they carry no seams
    at all, which is what rules out D-008 as the whole cause.
    """
    # (label, cells/Mpx, ratio, tiles) -- docs/MEMO.md §2 table, verbatim.
    rows = [
        ("single-tile-no-seams-2", 1741, 1.0177, 1),
        ("profiled-99pc-tissue", 1688, 1.0346, 4),
        ("single-tile-no-seams", 1597, 1.0121, 1),
        ("dense-2", 1542, 1.0337, 4),
        ("dense-5", 1135, 1.0817, 4),
        ("dense-4", 1066, 1.1293, 4),
        ("dense-7", 1038, 1.1041, 4),
        ("dense-3", 1007, 1.0624, 4),
        ("single-tile-no-seams-3", 931, 1.1130, 1),
    ]

    fig, ax = plt.subplots(figsize=(8.0, 4.6))
    style(ax)

    for _, dens, ratio, tiles in rows:
        single = tiles == 1
        ax.scatter(
            dens,
            ratio,
            s=150 if single else 95,
            color=RUST if single else QUPATH,
            marker="D" if single else "o",
            edgecolor="white",
            linewidth=1.4,
            zorder=3,
        )

    ax.axhline(1.0, color=INK, linewidth=1.2, linestyle="--", zorder=2)
    # The axis is inverted, so the *left* edge is the high-density end and
    # this anchors left there. Anchoring it right at the sparse end put it
    # under the legend.
    ax.text(1790, 1.003, "perfect agreement", fontsize=9, color=INK, ha="left")

    ax.set_xlabel("cell density (cells per megapixel)", color=INK, fontsize=11)
    ax.set_ylabel("objects found / QuPath cells", color=INK, fontsize=11)
    ax.set_ylim(0.975, 1.15)
    ax.invert_xaxis()

    # The legend says what the shapes mean, because the single-tile points are
    # the argument: no seams, and they still disagree.
    ax.scatter([], [], s=150, color=RUST, marker="D", edgecolor="white",
               linewidth=1.4, label="single tile (no seams at all)")
    ax.scatter([], [], s=95, color=QUPATH, marker="o", edgecolor="white",
               linewidth=1.4, label="4 tiles")
    # Below the baseline, in the band opened up by the ylim floor.
    ax.legend(frameon=False, fontsize=10, loc="lower right",
              labelcolor=INK, borderpad=0.1)

    ax.set_title(
        "Sparser tissue, worse agreement — and seams do not sort the table",
        color=INK, fontsize=12.5, pad=14, loc="left",
    )
    save(fig, "density")


def ladder():
    """Counts at four stage boundaries, on three whole regions at 2048x2048.

    docs/MEMO.md §2. Two effects, not one: the first rung is flat across
    density, and the split opens at the mean-intensity filter.
    """
    rungs = [
        "regional\nmaxima",
        "candidates\n(after flood)",
        "after mean-\nintensity filter",
        "after the\nmerge",
    ]
    sparse = [1.044, 1.044, 1.149, 1.139]
    dense2 = [1.047, 1.047, 1.049, 1.043]
    dense = [1.051, 1.051, 1.040, 1.028]

    fig, ax = plt.subplots(figsize=(8.0, 4.4))
    style(ax)

    ax.plot(rungs, sparse, marker="D", markersize=9, linewidth=2.4, color=RUST,
            label="sparse region (931 cells/Mpx)", zorder=3)
    ax.plot(rungs, dense2, marker="o", markersize=8, linewidth=2.0,
            color=QUPATH, label="dense-2 (1542)", zorder=3)
    ax.plot(rungs, dense, marker="s", markersize=8, linewidth=2.0,
            color=MUTED, label="dense (1688)", zorder=3)

    ax.axhline(1.0, color=INK, linewidth=1.0, linestyle="--", zorder=2)

    # The whole point of the figure is *where* the lines separate.
    ax.axvspan(1.5, 2.5, color=RUST, alpha=0.07, zorder=1)
    ax.annotate(
        "the density split\nopens here",
        xy=(2, 1.149), xytext=(2.35, 1.115),
        fontsize=9.5, color=RUST, fontweight="bold",
        arrowprops=dict(arrowstyle="->", color=RUST, linewidth=1.4),
    )

    ax.set_ylabel("objects found / QuPath", color=INK, fontsize=11)
    ax.set_ylim(0.99, 1.17)
    ax.legend(frameon=False, fontsize=9.5, loc="upper left", labelcolor=INK)
    ax.set_title(
        "Flat to the left of the filter, split to the right: two effects",
        color=INK, fontsize=12.5, pad=14, loc="left",
    )
    save(fig, "ladder")


def endtoend():
    """Wall clock on the region QuPath was profiled on.

    docs/MEMO.md §3 and docs/profiles/README.md: QuPath 15.87 s, mean of 5
    timed iterations, single-threaded, 4000x4000 at (27776, 5952).
    """
    labels = ["QuPath\n1 thread", "QuPath_Rust\n1 thread", "QuPath_Rust\n20 threads"]
    times = [15.87, 3.359, 1.019]
    colors = [QUPATH, RUST, RUST]

    fig, ax = plt.subplots(figsize=(7.6, 4.2))
    style(ax)
    bars = ax.bar(labels, times, color=colors, width=0.56)
    for bar, t, note in zip(bars, times, ["", "4.7x", "15.6x"]):
        ax.text(bar.get_x() + bar.get_width() / 2, t + 0.35, f"{t:.2f} s",
                ha="center", fontsize=11.5, color=INK, fontweight="bold")
        if not note:
            continue
        # Inside the bar where there is room, above the value label where there
        # is not -- docs/benchmarks/plot.py takes the same precaution, and for
        # this same 1.02 s bar: white-on-white overflowing a 1-second bar is
        # what happens when the label is placed unconditionally at t/2.
        if t > 2.5:
            ax.text(bar.get_x() + bar.get_width() / 2, t / 2, note, ha="center",
                    fontsize=13, color="white", fontweight="bold")
        else:
            ax.text(bar.get_x() + bar.get_width() / 2, t + 1.45, note,
                    ha="center", fontsize=13, color=RUST, fontweight="bold")

    ax.set_ylabel("wall clock (s)", color=INK, fontsize=11)
    ax.set_ylim(0, 18.5)
    ax.set_title(
        "Read the middle bar — and read it as generous to us",
        color=INK, fontsize=12.5, pad=14, loc="left",
    )
    save(fig, "endtoend")


def stages():
    """Object count through the pipeline, from `examples/region`.

    Run 2026-09-13 on the profiled 4000x4000 region. The split *raising* the
    count is what splitting touching nuclei does, and is the reason this is a
    figure rather than a sentence.
    """
    labels = [
        "candidates",
        "intensity\nfilter",
        "merge",
        "split and\nrefine",
        "area\nfilter",
        "tile\nseams",
    ]
    counts = [80762, 51152, 26397, 45990, 28359, 26647]

    fig, ax = plt.subplots(figsize=(9.0, 4.2))
    style(ax)
    bars = ax.bar(labels, counts, color=RUST, width=0.58)
    # The stage that adds objects is the surprising one; colour it apart.
    bars[3].set_color(QUPATH)

    for bar, c in zip(bars, counts):
        ax.text(bar.get_x() + bar.get_width() / 2, c + 1800, f"{c:,}",
                ha="center", fontsize=10, color=INK, fontweight="bold")

    ax.annotate(
        "+74%: touching nuclei\npulled apart",
        xy=(2.72, 40000), xytext=(1.30, 62000),
        fontsize=9.5, color=QUPATH, fontweight="bold",
        arrowprops=dict(arrowstyle="->", color=QUPATH, linewidth=1.4),
    )

    ax.set_ylabel("objects", color=INK, fontsize=11)
    ax.set_ylim(0, 92000)
    ax.set_title(
        "Every stage moves the count — which is what lets a ladder localise a gap",
        color=INK, fontsize=12, pad=14, loc="left",
    )
    save(fig, "stages")


def dice():
    """Per-object agreement, against QuPath's own recorded masks.

    docs/MEMO.md §2: pooled Dice 0.935 / 0.934 / 0.880 over three single-tile
    regions, 97.9% of 17,079 QuPath nuclei matched one-to-one, mean 0.921.
    """
    fig, ax = plt.subplots(figsize=(7.6, 4.0))
    style(ax)

    labels = ["region 1", "region 2", "region 3\n(the sparse one)"]
    pooled = [0.935, 0.934, 0.880]
    colors = [QUPATH, QUPATH, RUST]

    bars = ax.bar(labels, pooled, color=colors, width=0.5)
    for bar, d in zip(bars, pooled):
        ax.text(bar.get_x() + bar.get_width() / 2, d + 0.012, f"{d:.3f}",
                ha="center", fontsize=12, color=INK, fontweight="bold")

    ax.set_ylabel("pooled Dice vs QuPath's masks", color=INK, fontsize=11)
    ax.set_ylim(0, 1.06)
    ax.axhline(1.0, color=INK, linewidth=1.0, linestyle="--")
    ax.set_title(
        "The nuclei that correspond agree — pooled, so no matching decision",
        color=INK, fontsize=12.5, pad=14, loc="left",
    )
    save(fig, "dice")


if __name__ == "__main__":
    density()
    ladder()
    endtoend()
    stages()
    dice()
