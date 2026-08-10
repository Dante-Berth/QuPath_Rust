//! A/B probe: per-pixel deconvolution against the lookup table, interleaved.
//!
//! Colour deconvolution moved from `f32` to `f64` arithmetic to match QuPath's
//! own accumulation, which cost time: it went from 24-27% of pipeline time to
//! 36.5%. The lookup table is how that is paid back, and it is also what QuPath
//! does — `ColorDeconvolutionHelper.makeODLUT` builds the same 256-entry table.
//!
//! Both implementations run in one process on the same buffers, and the minimum
//! over many repetitions estimates the uncontended cost on a machine that is
//! not quiet (load average ~3). Every timed iteration asserts the two produce
//! **bit-identical** output — not merely close. The table holds the same
//! products, formed by the same multiply and summed in the same order, so any
//! difference at all would mean the fold changed the arithmetic.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "scratch probe")]

use std::hint::black_box;
use std::time::Instant;

use qupath_pipeline::Stains;

/// The deconvolution this change replaces: three `log10` calls and three
/// multiplies per pixel, going through the scalar accessor.
fn previous(stains: &Stains, rgb: &[u8], out: &mut Vec<f32>) {
    out.clear();
    out.reserve(rgb.len() / 3);
    out.extend(
        rgb.chunks_exact(3)
            .map(|pixel| stains.haematoxylin(pixel[0], pixel[1], pixel[2])),
    );
}

/// A tile of plausible H&E tissue: bright background with darker nuclei.
///
/// Synthetic, but in the right part of the colour space. Deconvolution cost is
/// data-independent — the same loads and arithmetic run whatever the bytes are
/// — so this measures the same thing a real tile would.
fn tile(width: usize, height: usize) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let nucleus = ((x / 8) % 3 == 0) && ((y / 8) % 3 == 0);
            if nucleus {
                rgb.extend_from_slice(&[110, 80, 150]);
            } else {
                let shade = 200 + u8::try_from((x + y) % 40).unwrap_or(0);
                rgb.extend_from_slice(&[shade, shade.saturating_sub(12), shade]);
            }
        }
    }
    rgb
}

fn main() {
    let reps = 200;
    println!(
        "H&E deconvolution, haematoxylin channel, \
         minimum of {reps} interleaved repetitions\n"
    );
    println!(
        "{:>11}  {:>12}  {:>12}  {:>9}  {:>12}",
        "tile", "per-pixel", "lookup table", "speedup", "throughput"
    );

    let stains = Stains::h_and_e();

    for &(width, height) in &[(256usize, 256usize), (512, 512), (1024, 1024)] {
        let rgb = tile(width, height);
        let pixels = width * height;

        // Both buffers allocated outside the timed region: for a memory-bound
        // kernel, allocator traffic inside it is a first-order term and has
        // already produced a wrong answer once in this repository.
        let mut from_previous = Vec::with_capacity(pixels);
        let mut from_current = Vec::with_capacity(pixels);

        let (mut old_best, mut new_best) = (f64::MAX, f64::MAX);

        for _ in 0..reps {
            let start = Instant::now();
            previous(&stains, &rgb, &mut from_previous);
            old_best = old_best.min(start.elapsed().as_secs_f64() * 1e3);
            black_box(&from_previous);

            let start = Instant::now();
            stains.haematoxylin_into(&rgb, &mut from_current);
            new_best = new_best.min(start.elapsed().as_secs_f64() * 1e3);
            black_box(&from_current);

            // A deconvolution that got fast by computing something else is not
            // a faster deconvolution. Bit equality, because the fold is claimed
            // to preserve the arithmetic exactly, not to approximate it.
            assert_eq!(
                from_current, from_previous,
                "deconvolutions diverged on {width}x{height}"
            );
        }

        #[expect(
            clippy::cast_precision_loss,
            reason = "pixel counts here are far below 2^53"
        )]
        let megapixels = pixels as f64 / 1e6;
        println!(
            "{:>8}²  {old_best:>9.3} ms  {new_best:>9.3} ms  {:>8.2}x  {:>8.0} Mpx/s",
            width,
            old_best / new_best,
            megapixels / (new_best / 1e3)
        );
    }
}
