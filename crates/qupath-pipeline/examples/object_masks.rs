//! Compares this pipeline's nuclei against QuPath's **per object**, by Dice and
//! IoU over the pixel masks both sides actually decided on.
//!
//! # Why this exists
//!
//! Every parity figure this repository quotes about detection is a *count*:
//! 156,979 objects against QuPath's 147,351, a pooled 1.065x. `README.md` says
//! plainly that a matching count is the weakest evidence there is, and it is
//! right — two segmentations can agree on how many nuclei there are while
//! disagreeing about which pixels each one covers, and a count cannot tell the
//! difference. This is the measurement that can.
//!
//! `parity/fixtures/ObjectMasks.json` has held the hard half since it was
//! recorded: every nucleus QuPath detects over three regions of CMU-1, as a
//! bounding box and `Roi::getMask` — ImageJ's own pixel mask. Nothing read it.
//! This is the consumer.
//!
//! # What is compared, and what is deliberately not
//!
//! **Masks, not polygons.** Both sides are the pixels each pipeline labelled.
//! Rasterising [`qupath_pipeline::Detections::outline`] to compare against
//! QuPath's mask would put our polygon filler inside the comparison, so the
//! masks here come out of the label map through
//! [`qupath_pipeline::run_collecting_masks`] instead.
//!
//! **Single-tile regions, so no seam can enter.** The three recorded regions
//! are 2048x2048 and fit one tile, which is why `record-object-masks.sh` chose
//! them: `DIVERGENCES.md` D-008 is about two overlap-resolution rules
//! disagreeing at seams, and none of that is reachable here. Whatever this
//! measures is detection disagreeing with detection.
//!
//! **Greedy matching by overlap, reported with its own leftovers.** Each of our
//! objects is paired with the QuPath object it overlaps most, best pair first,
//! one-to-one. The pairing is a choice and could flatter the result, so the
//! things it cannot hide are printed beside it: how many objects on each side
//! got no partner at all, and the pooled Dice over *all* pixels, which needs no
//! matching and no threshold.
//!
//! # Reading the output
//!
//! Pooled Dice is the honest headline: it is one number over every pixel in the
//! region and no pairing decision reaches it. Mean Dice over matched pairs says
//! how well the objects that *do* correspond agree in shape. The unmatched
//! counts say how much of each side the pairing had to give up on, and a high
//! mean Dice with many unmatched objects is a worse result than it looks.
//!
//! Run with `./parity/record-object-masks.sh` first, then
//! `cargo run --release --example object_masks -p qupath-pipeline`.
#![allow(missing_docs, reason = "example binary")]

use std::collections::HashMap;
use std::path::Path;

use qupath_pipeline::{Params, Region, run_collecting_masks};
use qupath_slide::Slide;

/// One nucleus as QuPath recorded it: a region-local box and a packed mask.
struct Recorded {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    area: u32,
    /// Row-major over the box, bit `row * w + column`, least significant bit of
    /// the first byte first — the packing `RecordObjectMasks.java` writes.
    mask: Vec<u8>,
}

impl Recorded {
    fn covers(&self, column: u32, row: u32) -> bool {
        let bit = (row * self.w + column) as usize;
        self.mask[bit / 8] & (1 << (bit % 8)) != 0
    }
}

/// One region of the fixture.
struct RecordedRegion {
    name: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    objects: Vec<Recorded>,
}

/// Pulls a `"key": value` out of a slice of the fixture, without a JSON crate.
///
/// This workspace depends on no JSON crate and every other fixture reader here
/// does the same. The shape is fixed and written by a recorder we control.
fn field<'a>(text: &'a str, key: &str, from: usize) -> Option<(&'a str, usize)> {
    let needle = format!("\"{key}\":");
    let at = text[from..].find(&needle)? + from + needle.len();
    let rest = &text[at..];
    let trimmed = rest.trim_start();
    let lead = rest.len() - trimmed.len();
    let (value, len) = if let Some(inner) = trimmed.strip_prefix('"') {
        let end = inner.find('"').expect("closing quote");
        (&inner[..end], end + 2)
    } else {
        let end = trimmed.find([',', '}', ']', '\n']).unwrap_or(trimmed.len());
        (trimmed[..end].trim(), end)
    };
    Some((value, at + lead + len))
}

fn number(text: &str, key: &str, from: usize) -> (u32, usize) {
    let (value, at) = field(text, key, from).unwrap_or_else(|| panic!("{key} missing"));
    (
        value.parse().unwrap_or_else(|_| panic!("{key} numeric")),
        at,
    )
}

fn parse(text: &str) -> Vec<RecordedRegion> {
    let mut regions = Vec::new();
    let mut at = text.find("\"regions\"").expect("regions");
    while let Some((name, next)) = field(text, "name", at) {
        let name = name.to_owned();
        let (x, next) = number(text, "x", next);
        let (y, next) = number(text, "y", next);
        let (width, next) = number(text, "width", next);
        let (height, next) = number(text, "height", next);
        let (nuclei, next) = number(text, "nuclei", next);
        let (_area_total, next) = number(text, "areaTotal", next);

        let mut objects = Vec::with_capacity(nuclei as usize);
        let mut cursor = next;
        for _ in 0..nuclei {
            let (ox, c) = number(text, "x", cursor);
            let (oy, c) = number(text, "y", c);
            let (ow, c) = number(text, "w", c);
            let (oh, c) = number(text, "h", c);
            let (area, c) = number(text, "area", c);
            let (mask, c) = field(text, "mask", c).expect("mask");
            let bytes: Vec<u8> = (0..mask.len() / 2)
                .map(|i| u8::from_str_radix(&mask[i * 2..i * 2 + 2], 16).expect("hex byte"))
                .collect();
            assert_eq!(
                bytes.len(),
                ((ow * oh) as usize).div_ceil(8),
                "mask length must match the bounding box"
            );
            objects.push(Recorded {
                x: ox,
                y: oy,
                w: ow,
                h: oh,
                area,
                mask: bytes,
            });
            cursor = c;
        }
        // The recorder writes `area` as the mask's popcount; checking it here
        // means a packing that drifted is caught by this reader rather than
        // quietly producing a low Dice that reads as a detection difference.
        for object in &objects {
            let popcount: u32 = object.mask.iter().map(|b| b.count_ones()).sum();
            assert_eq!(
                popcount, object.area,
                "recorded area must be the mask's popcount"
            );
        }
        regions.push(RecordedRegion {
            name,
            x,
            y,
            width,
            height,
            objects,
        });
        at = cursor;
    }
    regions
}

/// Dice and IoU of two pixel sets given their intersection and sizes.
///
/// Kept as one function so the two never drift apart: Dice is
/// `2|A∩B| / (|A|+|B|)` and IoU is `|A∩B| / (|A|+|B|-|A∩B|)`, and the second is
/// a monotone function of the first — both are printed because reviewers ask
/// for one or the other by habit, not because they say different things.
fn dice_iou(intersection: u64, ours: u64, theirs: u64) -> (f64, f64) {
    let union = ours + theirs - intersection;
    let dice = if ours + theirs == 0 {
        1.0
    } else {
        2.0 * intersection as f64 / (ours + theirs) as f64
    };
    let iou = if union == 0 {
        1.0
    } else {
        intersection as f64 / union as f64
    };
    (dice, iou)
}

/// What one region's comparison came to.
struct Report {
    name: String,
    ours: usize,
    theirs: usize,
    /// Over every pixel of the region, with no matching involved.
    pooled_dice: f64,
    pooled_iou: f64,
    matched: usize,
    mean_dice: f64,
    mean_iou: f64,
    /// Matched pairs whose Dice is at least 0.5 — the threshold the
    /// segmentation literature usually calls a true positive.
    strong: usize,
    unmatched_ours: usize,
    unmatched_theirs: usize,
}

/// Paints one side into a region-sized label grid: `0` is background and `n + 1`
/// is object `n`.
///
/// A grid rather than pairwise box intersection tests. Both sides are a few
/// thousand objects over 2048x2048, so one pass over 4 M cells is cheaper than
/// the pairwise sweep and — more to the point — it cannot get the geometry
/// subtly wrong the way an index-arithmetic shortcut can. Overlapping objects
/// on the same side would have the later one win; neither side produces them,
/// since both are traced from a label map.
fn paint(
    width: u32,
    height: u32,
    boxes: impl Iterator<Item = (u32, u32, u32, u32)>,
    inside: impl Fn(usize, u32, u32) -> bool,
) -> Vec<u32> {
    let mut grid = vec![0u32; (width as usize) * (height as usize)];
    for (n, (x, y, w, h)) in boxes.enumerate() {
        for row in 0..h {
            let gy = y + row;
            if gy >= height {
                continue;
            }
            for column in 0..w {
                let gx = x + column;
                if gx >= width || !inside(n, column, row) {
                    continue;
                }
                grid[gy as usize * width as usize + gx as usize] =
                    u32::try_from(n + 1).expect("object count fits u32");
            }
        }
    }
    grid
}

fn compare(region: &RecordedRegion, slide: &Slide) -> Report {
    let request = Region {
        x: region.x,
        y: region.y,
        width: region.width,
        height: region.height,
    };
    let (_outcome, ours) =
        run_collecting_masks(slide, request, Params::default()).expect("run the pipeline");
    assert!(
        ours.has_masks(),
        "the comparison is over masks; a set without them would silently compare nothing"
    );

    let theirs = paint(
        region.width,
        region.height,
        region.objects.iter().map(|o| (o.x, o.y, o.w, o.h)),
        |n, column, row| region.objects[n].covers(column, row),
    );
    // Ours come back in slide coordinates; the fixture is region-local.
    let mine = paint(
        region.width,
        region.height,
        ours.bounds()
            .iter()
            .map(|b| (b.x - region.x, b.y - region.y, b.width, b.height)),
        |n, column, row| {
            let mask = ours.mask(n).expect("masks were asked for");
            let bit = (row * ours.bounds()[n].width + column) as usize;
            mask[bit / 64] & (1u64 << (bit % 64)) != 0
        },
    );

    // One pass builds every pairwise intersection that is non-zero, which is a
    // few per object rather than the full product.
    let mut sizes_ours = vec![0u64; ours.len()];
    let mut sizes_theirs = vec![0u64; region.objects.len()];
    let mut overlap: HashMap<(u32, u32), u64> = HashMap::new();
    let mut intersection_total = 0u64;
    for (&a, &b) in mine.iter().zip(&theirs) {
        if a != 0 {
            sizes_ours[a as usize - 1] += 1;
        }
        if b != 0 {
            sizes_theirs[b as usize - 1] += 1;
        }
        if a != 0 && b != 0 {
            *overlap.entry((a, b)).or_default() += 1;
            intersection_total += 1;
        }
    }
    let ours_total: u64 = sizes_ours.iter().sum();
    let theirs_total: u64 = sizes_theirs.iter().sum();
    let (pooled_dice, pooled_iou) = dice_iou(intersection_total, ours_total, theirs_total);

    // Greedy one-to-one: best overlapping pair first. Not the optimal
    // assignment, which is Hungarian and would need a dependency; on a
    // near-diagonal problem like this the two agree on all but a handful of
    // pairs, and the unmatched counts below bound how much that could matter.
    let mut pairs: Vec<((u32, u32), u64)> = overlap.into_iter().collect();
    pairs.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut taken_ours = vec![false; ours.len()];
    let mut taken_theirs = vec![false; region.objects.len()];
    let (mut matched, mut sum_dice, mut sum_iou, mut strong) = (0usize, 0.0, 0.0, 0usize);
    for ((a, b), shared) in pairs {
        let (a, b) = (a as usize - 1, b as usize - 1);
        if taken_ours[a] || taken_theirs[b] {
            continue;
        }
        taken_ours[a] = true;
        taken_theirs[b] = true;
        let (dice, iou) = dice_iou(shared, sizes_ours[a], sizes_theirs[b]);
        sum_dice += dice;
        sum_iou += iou;
        if dice >= 0.5 {
            strong += 1;
        }
        matched += 1;
    }

    Report {
        name: region.name.clone(),
        ours: ours.len(),
        theirs: region.objects.len(),
        pooled_dice,
        pooled_iou,
        matched,
        mean_dice: if matched == 0 {
            0.0
        } else {
            sum_dice / matched as f64
        },
        mean_iou: if matched == 0 {
            0.0
        } else {
            sum_iou / matched as f64
        },
        strong,
        unmatched_ours: taken_ours.iter().filter(|t| !**t).count(),
        unmatched_theirs: taken_theirs.iter().filter(|t| !**t).count(),
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("parity/fixtures/ObjectMasks.json");
    let text = std::fs::read_to_string(&fixture).unwrap_or_else(|error| {
        eprintln!("error: cannot read {}: {error}", fixture.display());
        eprintln!("       run ./parity/record-object-masks.sh first");
        std::process::exit(1);
    });
    let regions = parse(&text);

    let slide_path = std::env::args()
        .nth(1)
        .map_or_else(|| root.join("data/CMU-1.svs"), std::path::PathBuf::from);
    if !slide_path.exists() {
        eprintln!("error: slide not found: {}", slide_path.display());
        eprintln!("       see docs/test-data.md for how to fetch it");
        std::process::exit(1);
    }
    let slide = Slide::open(&slide_path).expect("open the slide");

    println!(
        "Per-object parity against QuPath, over {} single-tile regions of {}",
        regions.len(),
        slide_path.file_name().unwrap_or_default().to_string_lossy()
    );
    println!(
        "\n{:<24} {:>7} {:>7} {:>7} {:>7} {:>8} {:>7} {:>7} {:>7} {:>7}",
        "region", "ours", "theirs", "pDice", "pIoU", "matched", "mDice", "mIoU", "d>=.5", "unmat"
    );

    let mut reports = Vec::new();
    for region in &regions {
        let report = compare(region, &slide);
        println!(
            "{:<24} {:>7} {:>7} {:>7.4} {:>7.4} {:>8} {:>7.4} {:>7.4} {:>7} {:>3}/{:<3}",
            report.name,
            report.ours,
            report.theirs,
            report.pooled_dice,
            report.pooled_iou,
            report.matched,
            report.mean_dice,
            report.mean_iou,
            report.strong,
            report.unmatched_ours,
            report.unmatched_theirs
        );
        reports.push(report);
    }

    // Pooled across regions by object count, not a mean of means: the regions
    // differ by a factor of two in how many nuclei they hold, and averaging the
    // per-region numbers would weight the sparse one as heavily as the dense
    // one. README.md's count figure is pooled for the same reason.
    let matched: usize = reports.iter().map(|r| r.matched).sum();
    let ours: usize = reports.iter().map(|r| r.ours).sum();
    let theirs: usize = reports.iter().map(|r| r.theirs).sum();
    let strong: usize = reports.iter().map(|r| r.strong).sum();
    let mean_dice = reports
        .iter()
        .map(|r| r.mean_dice * r.matched as f64)
        .sum::<f64>()
        / matched as f64;
    let mean_iou = reports
        .iter()
        .map(|r| r.mean_iou * r.matched as f64)
        .sum::<f64>()
        / matched as f64;
    println!(
        "\npooled: {ours} objects against QuPath's {theirs} ({:.4}x), {matched} matched one-to-one",
        ours as f64 / theirs as f64
    );
    println!("        mean Dice {mean_dice:.4}, mean IoU {mean_iou:.4} over matched pairs");
    println!(
        "        {strong} pairs at Dice >= 0.5 ({:.2}% of QuPath's objects)",
        100.0 * strong as f64 / theirs as f64
    );
    println!(
        "        unmatched: {} of ours, {} of QuPath's",
        reports.iter().map(|r| r.unmatched_ours).sum::<usize>(),
        reports.iter().map(|r| r.unmatched_theirs).sum::<usize>()
    );
    println!(
        "\nPooled Dice is the figure that involves no matching decision; read it first.\n\
         Regions are single-tile, so no seam resolution (D-008) can reach these numbers."
    );
}
