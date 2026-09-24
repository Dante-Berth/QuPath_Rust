//! What the van Herk block scan actually costs, and how much of it is the
//! serial running maximum inside each block.
//!
//! ROADMAP §4.9 names one untried lever on the erosion's dominant half: replace
//! the block scan's serial running maximum — 33 dependent comparisons per block
//! — with a log-depth SIMD scan. Before writing that, this runs the diagnostic
//! this repository's history says should always come first, and which was
//! skipped three times at a cost of a day each: **break the dependency, keep
//! every load and store, accept the wrong answer, read the clock.**
//!
//! If the chain is the cost, cutting it is fast and a SIMD scan is worth
//! writing. If cutting it changes nothing, the scan is bound by its memory
//! traffic — four passes over `len` floats — and a log-depth rewrite of the
//! arithmetic cannot pay, however elegant. That is the same question the
//! reconstruction's raster pass answered at 3.87 -> 0.62 ns/px, and the same one
//! the tracer's 4 M divisions answered at 1.00x.
//!
//! Every variant here writes the same buffers in the same order, so they differ
//! only in the dependency, not in the traffic. `scan` and `scan_vector` are
//! both correct and agree bit for bit; the other two are diagnostics and their
//! output is deliberately wrong.
//!
//! # What it measured, and why nothing was shipped
//!
//! On a real 1920² tile, single-threaded, minimum of five interleaved
//! repetitions:
//!
//! | | ns/px |
//! | --- | ---: |
//! | scan as written | 6.40 |
//! | scan, chain cut (wrong answer) | 4.26 |
//! | **scan, log-depth AVX2 (correct)** | **5.37** |
//! | scan, no arithmetic (wrong answer) | 2.99 |
//! | extend + combine only | 1.11 |
//!
//! So the chain is real — cutting it is worth 1.50x — and the log-depth scan
//! collects **1.19x** of that, stable across five runs (1.16-1.19x). It is the first change
//! in this repository to attack the 3.1x the horizontal half vectorises at
//! rather than its pass count, and it works.
//!
//! **It is still not worth shipping, and the arithmetic says so before any
//! `unsafe` is written.** The scan saves ~0.9 ns/px where `erode_cost` measures
//! the whole filter at 19.37, so it is ~1.05x on the erosion; the erosion is
//! 28% of the saturated grid, which makes it **~1.013x end to end**. ROADMAP §5
//! puts the floor for a two-binary claim at several percent and this machine's
//! run-to-run spread above that, so the change could not be *shown* to work in
//! place even though it does work in isolation — and ROADMAP §4.9 already has
//! one entry (four rows in flight: 1.21x isolated, 0.83x at 25 tiles) where a
//! gain this size reversed sign once nine streams per tile met DRAM.
//!
//! Against that: it needs `unsafe`, which the workspace denies outright and
//! which `docs/dev/rust-guidelines/README.md` allows only behind "a committed
//! profile showing what it buys". This is that profile, and what it shows is
//! that the change does not buy enough. Kept as a probe so the next person to
//! reach for ROADMAP §4.9's last lever can see it was written, measured and
//! priced rather than skipped.
#![allow(missing_docs, reason = "scratch probe")]
#![allow(
    unsafe_code,
    reason = "measuring what a SIMD scan would buy, before proposing it"
)]
#![allow(
    unsafe_op_in_unsafe_fn,
    reason = "the body of an AVX2 function is intrinsics throughout; the one \
              precondition is the caller's, and it is documented per call site"
)]

use std::hint::black_box;
use std::time::Instant;

use qupath_pipeline::{Params, Region, Stains};
use qupath_slide::Slide;

/// The scan as `qupath-filters` writes it today: one running maximum carried
/// along each block, forwards for `prefix` and backwards for `suffix`.
fn scan(extended: &[f32], prefix: &mut [f32], suffix: &mut [f32], span: usize) {
    let len = extended.len();
    for start in (0..len).step_by(span) {
        let end = (start + span).min(len);

        let mut running = f32::NEG_INFINITY;
        for (acc, &value) in prefix[start..end].iter_mut().zip(&extended[start..end]) {
            if value > running {
                running = value;
            }
            *acc = running;
        }

        let mut running = f32::NEG_INFINITY;
        for (acc, &value) in suffix[start..end]
            .iter_mut()
            .zip(&extended[start..end])
            .rev()
        {
            if value > running {
                running = value;
            }
            *acc = running;
        }
    }
}

/// **Wrong answer, diagnostic only.** Identical loads and stores; the running
/// maximum no longer depends on the previous iteration, so the chain is gone
/// and every comparison is independent. The gap between this and `scan` is the
/// whole of what a log-depth SIMD scan could ever recover.
fn scan_chain_cut(extended: &[f32], prefix: &mut [f32], suffix: &mut [f32], span: usize) {
    let len = extended.len();
    for start in (0..len).step_by(span) {
        let end = (start + span).min(len);

        let seed = extended[start];
        for (acc, &value) in prefix[start..end].iter_mut().zip(&extended[start..end]) {
            // `seed` is loop-invariant, so this is a plain independent max.
            *acc = if value > seed { value } else { seed };
        }

        for (acc, &value) in suffix[start..end]
            .iter_mut()
            .zip(&extended[start..end])
            .rev()
        {
            *acc = if value > seed { value } else { seed };
        }
    }
}

/// **Wrong answer, diagnostic only.** The traffic floor: the same two passes
/// over the same buffers with no comparison at all. What the scan costs if the
/// arithmetic were free.
fn scan_copy_only(extended: &[f32], prefix: &mut [f32], suffix: &mut [f32], span: usize) {
    let len = extended.len();
    for start in (0..len).step_by(span) {
        let end = (start + span).min(len);
        prefix[start..end].copy_from_slice(&extended[start..end]);
        suffix[start..end].copy_from_slice(&extended[start..end]);
    }
}

/// The candidate: a log-depth Hillis–Steele scan, eight lanes at a time.
///
/// This is the change ROADMAP §4.9 proposes, written out so its cost can be
/// measured before the workspace's `unsafe_code = "deny"` is lifted for it.
/// The serial 31–33 comparisons per block become three shift-and-max steps per
/// eight lanes plus one broadcast carry.
///
/// The `NaN` convention has to survive: the scalar fold uses `value > acc`, so
/// a `NaN` never becomes the running maximum. `_mm256_max_ps(a, b)` returns
/// `b` whenever either operand is `NaN`, so the accumulator — the operand that
/// must survive one — is passed **first** throughout, and the incoming value
/// second. Getting this backwards is not a rounding difference: it propagates
/// a `NaN` along the lanes and down the carry, which `check_agreement` caught
/// at span 9 on the first run.
///
/// # Safety
///
/// Caller must have established AVX2.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn scan_vector(extended: &[f32], prefix: &mut [f32], suffix: &mut [f32], span: usize) {
    use std::arch::x86_64::*;

    let len = extended.len();
    let reverse = _mm256_setr_epi32(7, 6, 5, 4, 3, 2, 1, 0);

    // Inclusive prefix maximum across the eight lanes of one vector.
    #[inline]
    unsafe fn lanes(mut v: std::arch::x86_64::__m256) -> std::arch::x86_64::__m256 {
        use std::arch::x86_64::*;
        let fill = _mm256_set1_ps(f32::NEG_INFINITY);
        // Shift in by one, two, then four lanes, filling with -inf.
        //
        // `v` goes FIRST at every step. `_mm256_max_ps(a, b)` returns `b`
        // whenever either operand is `NaN`, and the scalar rule is that an
        // accumulated maximum survives a `NaN` rather than being replaced by
        // one — so the accumulator must be the operand that loses, which is
        // `a`. The opposite order silently propagates a `NaN` down the lanes;
        // `check_agreement` catches it at span 9, and did.
        let crossed = _mm256_permute2f128_ps(v, fill, 0x02);
        let s = _mm256_alignr_epi8(_mm256_castps_si256(v), _mm256_castps_si256(crossed), 12);
        v = _mm256_max_ps(v, _mm256_castsi256_ps(s));
        let crossed = _mm256_permute2f128_ps(v, fill, 0x02);
        let s = _mm256_alignr_epi8(_mm256_castps_si256(v), _mm256_castps_si256(crossed), 8);
        v = _mm256_max_ps(v, _mm256_castsi256_ps(s));
        let crossed = _mm256_permute2f128_ps(v, fill, 0x02);
        _mm256_max_ps(v, crossed)
    }

    let lane7 = _mm256_set1_epi32(7);

    // A `NaN` in the input is discarded by the scalar fold, because
    // `value > running` is false for it and `running` starts at `-inf`. In the
    // vector path there is no such seed protecting the *first* lane of a
    // block: a `NaN` there is the initial running maximum, and
    // `_mm256_max_ps` will carry it along the whole scan. Replacing every
    // `NaN` with `-inf` on load reproduces the scalar meaning exactly — a
    // value that can never win — and costs one compare and one blend per
    // vector. `_CMP_EQ_OQ` against itself is false only for `NaN`.
    let sanitise = |v: __m256| {
        let ordered = _mm256_cmp_ps(v, v, _CMP_EQ_OQ);
        _mm256_blendv_ps(_mm256_set1_ps(f32::NEG_INFINITY), v, ordered)
    };

    for start in (0..len).step_by(span) {
        let end = (start + span).min(len);
        let block = &extended[start..end];
        let n = block.len();

        // Forward. Every load below is `block[at..at + 8]` with `at + 8 <= n`
        // asserted by the loop condition, so it reads only within the block —
        // slicing to the exact eight rather than taking `block[at..]` is what
        // keeps that true at the clamped final block, where the earlier
        // spelling read past `end`.
        let mut carry = _mm256_set1_ps(f32::NEG_INFINITY);
        let mut at = 0;
        while at + 8 <= n {
            let v = sanitise(_mm256_loadu_ps(block[at..at + 8].as_ptr()));
            // `carry` is the accumulator, so it goes first — see `lanes`.
            let total = _mm256_max_ps(carry, lanes(v));
            _mm256_storeu_ps(prefix[start + at..start + at + 8].as_mut_ptr(), total);
            carry = _mm256_permutevar8x32_ps(total, lane7);
            at += 8;
        }
        let mut running = {
            let mut out = [0.0f32; 8];
            _mm256_storeu_ps(out.as_mut_ptr(), carry);
            out[0]
        };
        for i in at..n {
            let value = block[i];
            if value > running {
                running = value;
            }
            prefix[start + i] = running;
        }

        // Backward: the same scan on the reversed block.
        let mut carry = _mm256_set1_ps(f32::NEG_INFINITY);
        let mut remaining = n;
        while remaining >= 8 {
            let at = remaining - 8;
            let v = sanitise(_mm256_loadu_ps(block[at..at + 8].as_ptr()));
            let rev = _mm256_permutevar8x32_ps(v, reverse);
            let total = _mm256_max_ps(carry, lanes(rev));
            let restored = _mm256_permutevar8x32_ps(total, reverse);
            _mm256_storeu_ps(suffix[start + at..start + at + 8].as_mut_ptr(), restored);
            carry = _mm256_permutevar8x32_ps(total, lane7);
            remaining = at;
        }
        let mut running = {
            let mut out = [0.0f32; 8];
            _mm256_storeu_ps(out.as_mut_ptr(), carry);
            out[0]
        };
        for i in (0..remaining).rev() {
            let value = block[i];
            if value > running {
                running = value;
            }
            suffix[start + i] = running;
        }
    }
}

/// Asserts the vector scan matches the scalar one bit for bit, including `NaN`,
/// both zeros and both infinities. A timing of a wrong scan is worthless.
fn check_agreement() {
    let mut seed = 0x9e37_79b9u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for span in [3usize, 7, 9, 15, 17, 21, 25, 31, 33, 35, 41, 65] {
        for len in [1usize, 2, 7, 8, 9, 15, 16, 17, 31, 33, 64, 65, 100, 257] {
            let source: Vec<f32> = (0..len)
                .map(|_| match next() % 16 {
                    0 => f32::NAN,
                    1 => 0.0,
                    2 => -0.0,
                    3 => f32::INFINITY,
                    4 => f32::NEG_INFINITY,
                    n => (n as f32) * 3.5 - 20.0,
                })
                .collect();
            let (mut sp, mut ss) = (vec![0.0f32; len], vec![0.0f32; len]);
            scan(&source, &mut sp, &mut ss, span);
            let (mut vp, mut vs) = (vec![0.0f32; len], vec![0.0f32; len]);
            // SAFETY: this probe is built with `-C target-cpu=x86-64-v3`, which
            // implies AVX2, and is only ever run from the invocation list in
            // ROADMAP §0 with that flag set.
            unsafe { scan_vector(&source, &mut vp, &mut vs, span) };
            let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
            assert_eq!(bits(&sp), bits(&vp), "prefix, span {span}, len {len}");
            assert_eq!(bits(&ss), bits(&vs), "suffix, span {span}, len {len}");
        }
    }
}

/// The combine and the extend that surround the scan, so the scan's share of
/// the whole block scheme is visible rather than assumed.
fn extend(column: &[f32], extended: &mut [f32], half_width: usize) {
    let width = column.len();
    extended[..half_width].fill(column[0]);
    extended[half_width..half_width + width].copy_from_slice(column);
    extended[half_width + width..].fill(column[width - 1]);
}

fn combine(out: &mut [f32], prefix: &[f32], suffix: &[f32], half_width: usize) {
    let width = out.len();
    for ((acc, &left), &right) in out
        .iter_mut()
        .zip(&suffix[..width])
        .zip(&prefix[2 * half_width..])
    {
        let best = if right > left { right } else { left };
        if best > *acc {
            *acc = best;
        }
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let side: u32 = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(1920);

    let slide = match Slide::open(&path) {
        Ok(slide) => slide,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let region = Region {
        x: 27776,
        y: 5952,
        width: side,
        height: side,
    };
    let rgb = slide
        .read_region_rgb(0, region.x, region.y, region.width, region.height)
        .expect("read region");
    let mut density = Vec::new();
    Stains::h_and_e().haematoxylin_into(&rgb, &mut density);

    let width = side as usize;
    let height = density.len() / width;
    let _ = Params::default();

    // The ten half-widths `Plan::for_radius(16.03)` puts on the block path:
    // eleven rectangles, of which `half_width` 5 and 1 go to the shift path at
    // `BLOCK_THRESHOLD = 7`.
    let half_widths = [16usize, 15, 14, 13, 12, 11, 10, 9, 7];

    let repetitions = 5;
    println!(
        "{side}x{side}, one real tile, single-threaded, minimum of {repetitions} \
         interleaved repetitions.\n\
         Every row of the tile through every block-path rectangle, which is what\n\
         one erosion's horizontal half does.\n"
    );

    let mut extended = vec![0.0f32; width + 2 * 16 + 8];
    let mut prefix = vec![0.0f32; width + 2 * 16 + 8];
    let mut suffix = vec![0.0f32; width + 2 * 16 + 8];
    let mut out = vec![f32::NEG_INFINITY; width];

    check_agreement();
    println!("The vector scan agrees with the scalar one bit for bit.\n");

    let mut timings = [f64::INFINITY; 5];

    for _ in 0..repetitions {
        // Variant 4: the real scan replaced by the log-depth vector one.
        let mark = Instant::now();
        for y in 0..height {
            let column = &density[y * width..(y + 1) * width];
            for &half_width in &half_widths {
                let len = width + 2 * half_width;
                let span = 2 * half_width + 1;
                extend(column, &mut extended[..len], half_width);
                // SAFETY: as above — AVX2 is implied by the build flag this
                // probe is run with, and `check_agreement` has already
                // exercised this exact function on the same build.
                unsafe {
                    scan_vector(
                        &extended[..len],
                        &mut prefix[..len],
                        &mut suffix[..len],
                        span,
                    )
                };
                combine(&mut out, &prefix[..len], &suffix[..len], half_width);
            }
        }
        timings[4] = timings[4].min(mark.elapsed().as_secs_f64());
        black_box(&out);

        // Variant 0: the real scan, plus extend and combine — the whole block
        // scheme as the filter runs it.
        let mark = Instant::now();
        for y in 0..height {
            let column = &density[y * width..(y + 1) * width];
            for &half_width in &half_widths {
                let len = width + 2 * half_width;
                let span = 2 * half_width + 1;
                extend(column, &mut extended[..len], half_width);
                scan(
                    &extended[..len],
                    &mut prefix[..len],
                    &mut suffix[..len],
                    span,
                );
                combine(&mut out, &prefix[..len], &suffix[..len], half_width);
            }
        }
        timings[0] = timings[0].min(mark.elapsed().as_secs_f64());
        black_box(&out);

        // Variant 1: same, with the scan's dependency cut.
        let mark = Instant::now();
        for y in 0..height {
            let column = &density[y * width..(y + 1) * width];
            for &half_width in &half_widths {
                let len = width + 2 * half_width;
                let span = 2 * half_width + 1;
                extend(column, &mut extended[..len], half_width);
                scan_chain_cut(
                    &extended[..len],
                    &mut prefix[..len],
                    &mut suffix[..len],
                    span,
                );
                combine(&mut out, &prefix[..len], &suffix[..len], half_width);
            }
        }
        timings[1] = timings[1].min(mark.elapsed().as_secs_f64());
        black_box(&out);

        // Variant 2: same, with no arithmetic in the scan at all.
        let mark = Instant::now();
        for y in 0..height {
            let column = &density[y * width..(y + 1) * width];
            for &half_width in &half_widths {
                let len = width + 2 * half_width;
                let span = 2 * half_width + 1;
                extend(column, &mut extended[..len], half_width);
                scan_copy_only(
                    &extended[..len],
                    &mut prefix[..len],
                    &mut suffix[..len],
                    span,
                );
                combine(&mut out, &prefix[..len], &suffix[..len], half_width);
            }
        }
        timings[2] = timings[2].min(mark.elapsed().as_secs_f64());
        black_box(&out);

        // Variant 3: extend and combine only, no scan — the surrounding cost.
        let mark = Instant::now();
        for y in 0..height {
            let column = &density[y * width..(y + 1) * width];
            for &half_width in &half_widths {
                let len = width + 2 * half_width;
                extend(column, &mut extended[..len], half_width);
                combine(&mut out, &prefix[..len], &suffix[..len], half_width);
            }
        }
        timings[3] = timings[3].min(mark.elapsed().as_secs_f64());
        black_box(&out);
    }

    let megapixels = density.len() as f64 / 1e6;
    let per_pixel = |seconds: f64| seconds / (megapixels * 1e6) * 1e9;

    println!("{:>34} {:>10} {:>10}", "", "s", "ns/px");
    let labels = [
        "scan as written",
        "scan, chain cut (wrong)",
        "scan, no arithmetic (wrong)",
        "extend + combine only",
        "scan, log-depth AVX2 (correct)",
    ];
    for (label, &seconds) in labels.iter().zip(&timings) {
        println!("{label:>34} {seconds:>10.4} {:>10.2}", per_pixel(seconds));
    }

    println!(
        "\n{:>34} {:>21.2}x",
        "cutting the chain is worth",
        timings[0] / timings[1]
    );
    println!(
        "{:>34} {:>21.2}x",
        "the log-depth scan is worth",
        timings[0] / timings[4]
    );
    println!(
        "{:>34} {:>21.2}x",
        "removing the arithmetic entirely",
        timings[0] / timings[2]
    );
    println!(
        "\nThe first ratio is the ceiling on a log-depth SIMD scan: it keeps every\n\
         load and store and removes only the dependency. If it is near 1.00x the\n\
         scan is bound by its traffic and ROADMAP §4.9's remaining lever is not\n\
         there."
    );
}
