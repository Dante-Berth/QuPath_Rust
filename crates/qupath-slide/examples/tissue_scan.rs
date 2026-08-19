//! Finds the densest tissue regions of a slide, on a coarse level.
//!
//! Picking comparison regions by guessing coordinates is how a parity sweep
//! ends up measuring glass: four of six regions chosen that way on CMU-1 came
//! back with 0, 15, 37 and 790 cells. This scans the coarsest pyramid level for
//! mean optical density and reports the darkest non-overlapping candidates, so
//! the regions are chosen by the slide rather than by the person.
//!
//! Usage: tissue_scan [slide] [side] [count]
#![allow(missing_docs, reason = "example binary")]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "coordinates on a coarse pyramid level; this ranks candidates rather than measuring"
)]

use qupath_slide::Slide;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let side: u32 = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4000);
    let wanted: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8);

    let slide = Slide::open(&path).expect("open the slide");
    let levels = slide.levels();
    let coarse = levels.last().expect("at least one level");
    let downsample = coarse.downsample;

    let mut rgb = Vec::new();
    let mut decoded = Vec::new();
    slide
        .read_region_into(
            coarse.index,
            0,
            0,
            coarse.width,
            coarse.height,
            &mut rgb,
            &mut decoded,
        )
        .expect("read the overview");

    // A summed-area table over the coarse level's darkness, so each candidate
    // costs four lookups rather than a rescan of its own pixels. The naive
    // version re-reads every pixel per candidate, which on this slide is a few
    // hundred candidates over a 2875x2057 level and does not finish in five
    // minutes -- measured, by writing it that way first.
    let (cw, ch) = (coarse.width as usize, coarse.height as usize);
    let mut integral = vec![0.0f64; (cw + 1) * (ch + 1)];
    for row in 0..ch {
        let mut running = 0.0f64;
        for column in 0..cw {
            let at = (row * cw + column) * 3;
            let darkness = if at + 2 < rgb.len() {
                let mean =
                    (f64::from(rgb[at]) + f64::from(rgb[at + 1]) + f64::from(rgb[at + 2])) / 3.0;
                255.0 - mean
            } else {
                0.0
            };
            running += darkness;
            integral[(row + 1) * (cw + 1) + column + 1] =
                integral[row * (cw + 1) + column + 1] + running;
        }
    }
    let sum_of = |x0: usize, y0: usize, x1: usize, y1: usize| -> f64 {
        integral[y1 * (cw + 1) + x1] - integral[y0 * (cw + 1) + x1] - integral[y1 * (cw + 1) + x0]
            + integral[y0 * (cw + 1) + x0]
    };

    // Stepping by half a side, so a dense patch straddling a boundary is still
    // found by some candidate.
    let step = side / 2;
    let mut scored: Vec<(f64, u32, u32)> = Vec::new();
    let mut y = 0;
    while y + side <= levels[0].height {
        let mut x = 0;
        while x + side <= levels[0].width {
            let (cx, cy) = (
                (f64::from(x) / downsample) as usize,
                (f64::from(y) / downsample) as usize,
            );
            let cside = (f64::from(side) / downsample) as usize;
            let (x1, y1) = ((cx + cside).min(cw), (cy + cside).min(ch));
            let area = x1.saturating_sub(cx) * y1.saturating_sub(cy);
            if area > 0 {
                scored.push((sum_of(cx, cy, x1, y1) / area as f64, x, y));
            }
            x += step;
        }
        y += step;
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("finite"));

    // Non-overlapping picks, so the sweep covers different tissue rather than
    // several views of the same dense patch.
    let mut picked: Vec<(f64, u32, u32)> = Vec::new();
    for &(score, x, y) in &scored {
        let overlaps = picked
            .iter()
            .any(|&(_, px, py)| x < px + side && px < x + side && y < py + side && py < y + side);
        if !overlaps {
            picked.push((score, x, y));
        }
        if picked.len() == wanted {
            break;
        }
    }

    println!("# {path}, {side}x{side} candidates ranked by mean optical density");
    println!(
        "# density is 255 - mean channel value on level {}",
        coarse.index
    );
    for (score, x, y) in &picked {
        println!("{x},{y},{side},{side}  # density {score:.1}");
    }
}
