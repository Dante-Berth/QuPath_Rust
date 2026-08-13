#!/usr/bin/env python3
"""Regenerates the comparison charts in README.md.

Every number here is copied from a recorded measurement in this directory or in
docs/profiles/README.md, and the source is named next to it. Nothing is
estimated: if a figure has no recording behind it, it does not go in a chart.

    python3 docs/benchmarks/plot.py
"""

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

OUT = "docs/benchmarks"

# QuPath's brand green, and a warmer tone for the port. Chosen to stay
# distinguishable in greyscale, which is how these get printed.
QUPATH = "#4a7c59"
RUST = "#c1553b"
GRID = "#d6d3cd"
INK = "#2b2b2b"


def style(ax):
    ax.set_axisbelow(True)
    ax.grid(axis="y", color=GRID, linewidth=0.7)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    for side in ("left", "bottom"):
        ax.spines[side].set_color(GRID)
    ax.tick_params(colors=INK, length=0)


def endtoend():
    """Wall clock on the region QuPath was profiled on.

    QuPath: docs/profiles/README.md, 15.87 s mean of 5 timed iterations,
    single-threaded. Ours: `region` on the same 4000x4000 region at
    (27776, 5952), measured 2026-08-13.
    """
    labels = ["QuPath\n1 thread", "QuPath_Rust\n1 thread", "QuPath_Rust\n20 threads"]
    times = [15.87, 3.359, 1.019]
    colors = [QUPATH, RUST, RUST]

    fig, ax = plt.subplots(figsize=(7.2, 4.0))
    bars = ax.bar(labels, times, color=colors, width=0.58)
    for bar, t in zip(bars, times):
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            t + 0.35,
            f"{t:.2f} s",
            ha="center",
            fontsize=11,
            color=INK,
            fontweight="bold",
        )
    # The speedup sits inside the bar where there is room for it, and just
    # above the value label where there is not -- the 20-thread bar is only
    # 1.02 s tall and swallows the text otherwise.
    for bar, t in zip(bars[1:], times[1:]):
        inside = t > 2.5
        ax.text(
            bar.get_x() + bar.get_width() / 2,
            t / 2 if inside else t + 1.35,
            f"{15.87 / t:.1f}x",
            ha="center",
            fontsize=13,
            color="white" if inside else RUST,
            fontweight="bold",
        )

    ax.set_ylabel("wall clock (s), lower is better", color=INK)
    ax.set_ylim(0, 18.5)
    ax.set_title(
        "Cell detection, CMU-1 4000x4000 at (27776, 5952)",
        color=INK,
        fontsize=12,
        pad=12,
    )
    style(ax)
    fig.text(
        0.5,
        0.005,
        "QuPath's 15.87 s includes the tail this port has not written "
        "(ROI vertices, cell expansion, measurements),\nso these ratios are an "
        "upper bound on the true gap. Both sides detect the same region of the "
        "same slide.",
        ha="center",
        fontsize=7.5,
        color="#6b6b6b",
    )
    fig.tight_layout(rect=(0, 0.075, 1, 1))
    fig.savefig(f"{OUT}/compare-endtoend.svg")
    plt.close(fig)


def rank():
    """MIN/MAX rank filter against ImageJ's RankFilters, by radius.

    Both columns from the table in README.md, whose raw output is in
    rust-rank.txt and BenchRankFilter.txt. 512x512 f32, minimum over
    repetitions, both sides threaded.
    """
    radii = ["1", "2", "3", "5", "8", "10", "20"]
    imagej = [1.060, 0.790, 0.680, 0.903, 1.053, 1.265, 3.414]
    rust = [0.110, 0.122, 0.159, 0.215, 0.314, 0.411, 1.003]

    x = range(len(radii))
    width = 0.38
    fig, ax = plt.subplots(figsize=(7.6, 4.0))
    ax.bar([i - width / 2 for i in x], imagej, width, label="ImageJ RankFilters", color=QUPATH)
    ax.bar([i + width / 2 for i in x], rust, width, label="QuPath_Rust", color=RUST)

    for i, (a, b) in enumerate(zip(imagej, rust)):
        ax.text(i, max(a, b) + 0.10, f"{a / b:.1f}x", ha="center", fontsize=9.5, color=INK)

    ax.set_xticks(list(x))
    ax.set_xticklabels(radii)
    ax.set_xlabel("kernel radius (px)", color=INK)
    ax.set_ylabel("time (ms), lower is better", color=INK)
    ax.set_ylim(0, 4.0)
    ax.set_title("MAX rank filter, 512x512 f32", color=INK, fontsize=12, pad=12)
    ax.legend(frameon=False, loc="upper left")
    style(ax)
    fig.tight_layout()
    fig.savefig(f"{OUT}/compare-rank.svg")
    plt.close(fig)


def hotframes():
    """Where QuPath spends its time, by self time, and what replaced each.

    Shares from docs/profiles/CMU-1-cells-hot.txt over 10,759 samples.
    Grouped by the stage this port wrote to replace them.
    """
    groups = [
        ("watershed queue\n(PriorityQueue + comparator)", 29.4),
        ("rank filter\n(getSideMax, getAreaMax)", 11.6),
        ("pixel access\n(FloatProcessor.getf)", 6.1),
        ("reconstruction\n(dilateAndCompare)", 5.1),
        ("convolution + blur\n(convolveFloat, convolveLine)", 5.5),
        ("EDM + maxima\n(trueEdmHeight, processLevel)", 3.3),
    ]
    names = [g[0] for g in groups]
    shares = [g[1] for g in groups]

    fig, ax = plt.subplots(figsize=(7.6, 4.2))
    bars = ax.barh(names, shares, color=QUPATH, height=0.6)
    for bar, s in zip(bars, shares):
        ax.text(s + 0.35, bar.get_y() + bar.get_height() / 2, f"{s}%", va="center", fontsize=10, color=INK)

    ax.invert_yaxis()
    ax.set_xlabel("share of QuPath self time", color=INK)
    ax.set_xlim(0, 34)
    ax.set_title(
        "What QuPath's detection spends time on (10,759 samples)",
        color=INK,
        fontsize=12,
        pad=12,
    )
    style(ax)
    ax.grid(axis="y", visible=False)
    ax.grid(axis="x", color=GRID, linewidth=0.7)
    fig.text(
        0.5,
        0.005,
        "Self time, single-threaded, on the profiled region. Every group here is "
        "ported and bit-identical;\nthe port's own time is distributed "
        "differently, which is why the two profiles are shown separately.",
        ha="center",
        fontsize=7.5,
        color="#6b6b6b",
    )
    fig.tight_layout(rect=(0, 0.07, 1, 1))
    fig.savefig(f"{OUT}/compare-hotframes.svg")
    plt.close(fig)


def stages():
    """The port's own stage breakdown, on the same region.

    From docs/benchmarks/rust-pipeline-region.txt: 4 tiles, 20 threads,
    stage seconds summed across threads.
    """
    data = [
        ("background regrow", 1.003),
        ("shape split", 0.540),
        ("watershed flood", 0.443),
        ("background erode", 0.378),
        ("tracing", 0.244),
        ("merge", 0.223),
        ("maxima", 0.190),
        ("nuclei", 0.155),
        ("refine", 0.122),
        ("decode", 0.121),
        ("gaussian blur", 0.098),
        ("deconvolve", 0.046),
        ("laplacian", 0.041),
        ("labelling", 0.033),
        ("object filter", 0.020),
    ]
    names = [d[0] for d in data]
    secs = [d[1] for d in data]
    total = sum(secs)

    fig, ax = plt.subplots(figsize=(7.6, 5.0))
    bars = ax.barh(names, secs, color=RUST, height=0.66)
    for bar, s in zip(bars, secs):
        ax.text(
            s + 0.012,
            bar.get_y() + bar.get_height() / 2,
            f"{s:.3f} s  ({100 * s / total:.1f}%)",
            va="center",
            fontsize=8.5,
            color=INK,
        )

    ax.invert_yaxis()
    ax.set_xlabel("stage seconds, summed across threads", color=INK)
    ax.set_xlim(0, 1.25)
    ax.set_title("Where QuPath_Rust spends time, same region", color=INK, fontsize=12, pad=12)
    style(ax)
    ax.grid(axis="y", visible=False)
    ax.grid(axis="x", color=GRID, linewidth=0.7)
    fig.text(
        0.5,
        0.005,
        "4 tiles on 20 threads. Shares move with the tile count and the tissue "
        "density -- see ROADMAP.md §4,\nwhich records the same table on a "
        "25-tile grid where the erosion is the largest stage instead.",
        ha="center",
        fontsize=7.5,
        color="#6b6b6b",
    )
    fig.tight_layout(rect=(0, 0.06, 1, 1))
    fig.savefig(f"{OUT}/compare-stages.svg")
    plt.close(fig)


def threads():
    """Wall clock against thread count, both regions.

    Measured 2026-08-13 with taskset pinning; P-cores are CPUs 0-11 (6 cores,
    SMT siblings) and E-cores 12-19. Recorded in rust-threads.txt.
    """
    counts = [1, 6, 8, 12, 16, 20]
    saturated = [7.260, 1.554, 1.689, 1.427, 1.311, 1.203]
    profiled = [3.359, 1.028, None, 1.017, None, 1.019]

    fig, ax = plt.subplots(figsize=(7.2, 4.0))
    ax.plot(counts, saturated, marker="o", color=RUST, linewidth=2, label="9000x9000, 25 tiles")
    px = [c for c, p in zip(counts, profiled) if p]
    py = [p for p in profiled if p]
    ax.plot(px, py, marker="s", color=QUPATH, linewidth=2, label="4000x4000, 4 tiles")

    ax.annotate(
        "flat past 6 threads:\nonly 4 tiles to give",
        xy=(12, 1.017),
        xytext=(13.2, 2.9),
        fontsize=8.5,
        color=INK,
        arrowprops={"arrowstyle": "->", "color": "#8a8a8a", "linewidth": 0.9},
    )

    ax.set_xlabel("threads", color=INK)
    ax.set_ylabel("wall clock (s), lower is better", color=INK)
    ax.set_xticks(counts)
    ax.set_ylim(0, 7.8)
    ax.set_title("Scaling depends on the tile count, not the core count", color=INK, fontsize=12, pad=12)
    ax.legend(frameon=False)
    style(ax)
    fig.tight_layout()
    fig.savefig(f"{OUT}/compare-threads.svg")
    plt.close(fig)


if __name__ == "__main__":
    endtoend()
    rank()
    hotframes()
    stages()
    threads()
    print("wrote compare-{endtoend,rank,hotframes,stages,threads}.svg")
