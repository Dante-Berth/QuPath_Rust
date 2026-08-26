//! Walks both pipelines' object counts stage by stage over the same crops, to
//! find where the count gap enters.
//!
//! `DIVERGENCES.md` D-009 established two things and left one open. The gap
//! tracks tissue density inversely, and the merge and the shape split are the
//! stages whose ratios move with it — but given the *same input mask* those two
//! reproduce QuPath's output pixel for pixel on real tissue in both regimes
//! (`parity_merge_and_split_tile.rs`). A stage that faithfully processes a
//! different input produces different output, so the pair is where the gap
//! becomes visible, not where it is made. That puts the search upstream, and
//! upstream is where this looks.
//!
//! `MergeAndSplitTile.json` already carries QuPath's half of the ladder for
//! eight crops: the candidates its labelling found, how many survived the
//! mean-intensity filter, and how many regions the merge left. [`Outcome`]
//! carries the same three counts for this pipeline. Printing them side by side
//! says which *rung* the two first disagree on, which is a stage rather than a
//! suspicion.
//!
//! **A fourth rung is deliberately absent, having first been printed wrongly.**
//! The obvious pairing is [`Outcome::nuclei`] against the fixture's `objects`,
//! and it is not a pairing: `nuclei` is counted after the shape split *and the
//! boundary refinement*, while the recorder stops at the split. Compared anyway
//! they come back at 1.77-1.89x, which reads as a spectacular divergence and is
//! an artefact of counting two different things. Restoring the rung means
//! recording QuPath's count after `refineBoundary` as well — a field the
//! recorder does not yet write.
//!
//! ```text
//! cargo run --release -p qupath-pipeline --example stage_ladder
//! ```
//!
//! **This is a reported comparison, not a test**, for `CellCounts.json`'s
//! reason: there is no tolerance at which "the ladders match" is currently
//! true, and writing one with a bound picked to pass would be choosing a number
//! rather than measuring one.
//!
//! **Two things make the rungs approximate rather than exact**, and both are
//! recorded rather than corrected.
//!
//! `D-005`: detection here runs at the slide's own pixel size where QuPath runs
//! at its requested 0.5 µm, so QuPath's crop came back 319x319 where this one
//! is 320x320 — a 0.2% scale difference that moves sigma from 3.006 to 3.000
//! and the minimum area from 40.2 to 40.0 pixels. Sub-percent, against a gap of
//! six and a half, so it cannot be the explanation; it does mean a *pixel*
//! comparison is not available here and only the counts are.
//!
//! The rungs also do not line up perfectly by construction. QuPath's `seeds` is
//! the polygon count from its labelled image, taken after the flood; this
//! pipeline's `regions` is the basin count after hole-filling. They answer the
//! same question about the same stage and are not guaranteed to be the same
//! integer on an image where a basin encloses another. Read a rung's *ratio*,
//! and read a jump between consecutive ratios as the signal.
#![allow(missing_docs, reason = "example binary")]

use qupath_pipeline::{Params, Region, run};
use qupath_slide::Slide;

/// One crop of the fixture, parsed without a JSON crate — this workspace has no
/// dependency on one and the shape is fixed by `parity/java/MergeAndSplitTile.java`.
struct Recorded {
    name: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    log_components: u64,
    maxima: u64,
    seeds: u64,
    after_intensity: u64,
    merged: u64,
}

fn field(line: &str, key: &str) -> Option<String> {
    let at = line.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = line[at..].trim_start();
    let rest = rest.strip_prefix('"').map_or(rest, |rest| rest);
    let end = rest.find([',', '}', '"']).unwrap_or(rest.len());
    Some(rest[..end].trim().to_owned())
}

/// `here / theirs`, or `None` where QuPath's rung is zero.
///
/// A crop of glass divides by zero on every rung, and printing `inf` or `NaN`
/// across a row invites reading it as a divergence rather than as an empty
/// image. One of the eight crops is exactly that.
#[expect(
    clippy::cast_precision_loss,
    reason = "object counts are hundreds; f64 is exact well past that"
)]
fn ratio(here: u64, theirs: u64) -> Option<f64> {
    (theirs != 0).then(|| here as f64 / theirs as f64)
}

fn show(value: Option<f64>) -> String {
    value.map_or_else(|| "     —".to_owned(), |value| format!("{value:>6.3}"))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let fixture = args
        .next()
        .unwrap_or_else(|| "parity/fixtures/MergeAndSplitTile.json".to_owned());

    let text = std::fs::read_to_string(&fixture).unwrap_or_else(|error| {
        eprintln!("cannot read {fixture}: {error}");
        eprintln!("run ./parity/record-merge-tiles.sh first");
        std::process::exit(1);
    });

    // Split on the record marker rather than on lines: the recorder wraps one
    // crop across several lines, so `name` and `seeds` never share one.
    let recorded: Vec<Recorded> = text
        .split("{\"name\": ")
        .skip(1)
        .filter_map(|line| {
            Some(Recorded {
                name: line.split('"').nth(1)?.to_owned(),
                x: field(line, "x")?.parse().ok()?,
                y: field(line, "y")?.parse().ok()?,
                width: field(line, "requestedWidth")?.parse().ok()?,
                height: field(line, "requestedHeight")?.parse().ok()?,
                log_components: field(line, "logComponents")?.parse().ok()?,
                maxima: field(line, "maximaCount")?.parse().ok()?,
                seeds: field(line, "seeds")?.parse().ok()?,
                after_intensity: field(line, "afterIntensity")?.parse().ok()?,
                merged: field(line, "mergedCount")?.parse().ok()?,
            })
        })
        .collect();

    assert!(
        !recorded.is_empty(),
        "no crops parsed from {fixture}; has it been recorded?"
    );

    let slide = Slide::open(&path).expect("open the slide");

    println!(
        "Object counts at four stage boundaries, this pipeline over QuPath.\n\
         A rung's ratio is where the two stand; a jump between consecutive\n\
         ratios is the stage that made it.\n"
    );
    println!(
        "{:<11} {:>19} {:>19} {:>19} {:>19}",
        "", "maxima", "candidates", "after intensity", "after merge"
    );
    println!(
        "{:<11} {:>6} {:>5} {:>6} {:>6} {:>5} {:>6} {:>6} {:>5} {:>6} {:>6} {:>5} {:>6}",
        "crop",
        "QuPath",
        "here",
        "ratio",
        "QuPath",
        "here",
        "ratio",
        "QuPath",
        "here",
        "ratio",
        "QuPath",
        "here",
        "ratio",
    );

    // Totals per regime, because the claim under investigation is a difference
    // between them: the sparse crops are the ones that disagree end to end.
    let mut totals: [[u64; 8]; 2] = [[0; 8]; 2];

    for entry in &recorded {
        let region = Region {
            x: entry.x,
            y: entry.y,
            width: entry.width,
            height: entry.height,
        };
        let outcome = run(&slide, region, Params::default()).expect("run the pipeline");

        println!(
            "{:<11} {:>6} {:>5} {} {:>6} {:>5} {} {:>6} {:>5} {} {:>6} {:>5} {}",
            entry.name,
            entry.maxima,
            outcome.markers,
            show(ratio(outcome.markers, entry.maxima)),
            entry.seeds,
            outcome.regions,
            show(ratio(outcome.regions, entry.seeds)),
            entry.after_intensity,
            outcome.candidates,
            show(ratio(outcome.candidates, entry.after_intensity)),
            entry.merged,
            outcome.merged,
            show(ratio(outcome.merged, entry.merged)),
        );

        // QuPath's side only: the thresholded LoG's connected components, which
        // is what the maxima are found *inside*. Not a rung, because it is not
        // comparable to a count of maxima -- a blob holds several of them, and
        // here it runs about half. It is printed because it says how much
        // clumping the tile carries: maxima per LoG blob is 1.84 on the sparse
        // region against 1.46 and 1.42 on the dense ones, which is the clumping
        // D-009 describes, measured directly rather than inferred from how hard
        // the merge and the split had to work.
        println!(
            "{:<11} {:>6} LoG blobs, {:>5.2} maxima each",
            "",
            entry.log_components,
            f64::from(u32::try_from(entry.maxima).unwrap_or(u32::MAX))
                / f64::from(u32::try_from(entry.log_components.max(1)).unwrap_or(u32::MAX)),
        );

        let row = usize::from(!entry.name.starts_with("sparse"));
        for (slot, value) in [
            entry.maxima,
            outcome.markers,
            entry.seeds,
            outcome.regions,
            entry.after_intensity,
            outcome.candidates,
            entry.merged,
            outcome.merged,
        ]
        .into_iter()
        .enumerate()
        {
            totals[row][slot] += value;
        }
    }

    println!();
    for (row, label) in [(0usize, "sparse"), (1, "dense")] {
        let t = totals[row];
        println!(
            "{:<11} {:>6} {:>5} {} {:>6} {:>5} {} {:>6} {:>5} {} {:>6} {:>5} {}",
            label,
            t[0],
            t[1],
            show(ratio(t[1], t[0])),
            t[2],
            t[3],
            show(ratio(t[3], t[2])),
            t[4],
            t[5],
            show(ratio(t[5], t[4])),
            t[6],
            t[7],
            show(ratio(t[7], t[6])),
        );
    }

    println!(
        "\nHow to read it. The merge and the split are already pinned against\n\
         QuPath pixel for pixel on these very crops, given the same input mask\n\
         (parity_merge_and_split_tile.rs), so a ratio that is already wrong at\n\
         'candidates' is the answer and the two rungs after it are carrying it\n\
         forward. A ratio that only goes wrong later would contradict that test\n\
         and should be treated as a bug in this comparison before it is treated\n\
         as a finding -- which is how the fourth rung came out: it compared\n\
         post-refinement objects here against pre-refinement ones there, read\n\
         as 1.8x, and was removed rather than reported.\n\n\
         The rungs are approximate: D-005 puts QuPath's crop at 319x319 against\n\
         this one's 320x320, and 'candidates' counts hole-filled basins here\n\
         against traced polygons there. Sub-percent effects, against a gap of\n\
         six and a half percent -- read the jumps, not the third decimal."
    );
}
