//! What clicking an object costs at whole-slide sizes.
//!
//! ```text
//! cargo run -p qupath-viewer --example pick_cost --release [-- OBJECTS]
//! ```
//!
//! `docs/dev/VIEWER.md` §3.5b. `pick::object_at` is a linear scan over the
//! overlay's bounding boxes, and this is the measurement that decided it needs
//! no spatial index: it builds `OBJECTS` (default 150,000 — a whole-slide run)
//! nucleus-sized rings of 36 vertices scattered over a CMU-1-sized slide, then
//! times the scan at points that land on objects and at points that land on
//! glass. It also times what choosing a column to colour by costs, which is the
//! other whole-column pass in §3.5b.
//!
//! Synthetic geometry rather than a run, because a whole-slide run of CMU-1
//! takes a minute and a half of every core, and what the scan's cost depends on
//! is the object count and the ring length — both matched here to what a run
//! produces (36.7 vertices per ring, `qupath_pipeline::detections`).
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]
#![expect(
    clippy::cast_possible_truncation,
    reason = "synthetic coordinates inside a slide, and query counts of a few thousand"
)]

use std::time::Instant;

use qupath_viewer::colour_map::Range;
use qupath_viewer::{Overlay, SlidePoint, pick};

/// CMU-1's level-0 extent, which is where a whole-slide run's objects live.
const SLIDE: (f64, f64) = (46_000.0, 32_914.0);

/// A small linear congruential generator, so the layout is the same every run
/// and needs no dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn main() {
    let objects: usize = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(150_000);

    let mut random = Lcg(0x5eed);
    let mut overlay = Overlay::new();
    let mut centres = Vec::with_capacity(objects);
    for _ in 0..objects {
        let (cx, cy) = (random.next() * SLIDE.0, random.next() * SLIDE.1);
        let radius = 4.0 + random.next() * 6.0;
        let ring: Vec<[u32; 2]> = (0..36)
            .map(|step| {
                let angle = f64::from(step) * std::f64::consts::TAU / 36.0;
                [
                    (cx + radius * angle.cos()).max(0.0) as u32,
                    (cy + radius * angle.sin()).max(0.0) as u32,
                ]
            })
            .collect();
        overlay.push_level_ring(&ring, 1.0);
        centres.push(SlidePoint::new(cx, cy));
    }
    println!(
        "{} objects, {} vertices, {} kB",
        overlay.len(),
        overlay.vertex_count(),
        overlay.bytes() / 1024
    );

    // On objects: the click a user actually makes.
    let queries = 2_000;
    let started = Instant::now();
    let mut hits = 0;
    for centre in centres.iter().step_by((objects / queries).max(1)) {
        hits += usize::from(pick::object_at(&overlay, *centre).is_some());
    }
    let on_objects = started.elapsed() / queries as u32;

    // On glass: every box is tested and no polygon is, the scan's floor.
    let started = Instant::now();
    let mut misses = 0;
    for _ in 0..queries {
        let point = SlidePoint::new(random.next() * SLIDE.0, random.next() * SLIDE.1);
        misses += usize::from(pick::object_at(&overlay, point).is_none());
    }
    let anywhere = started.elapsed() / queries as u32;

    println!(
        "object_at, over an object: {:.0} us per click ({hits} of {queries} hit)",
        on_objects.as_secs_f64() * 1e6
    );
    println!(
        "object_at, anywhere:       {:.0} us per click ({misses} of {queries} on glass)",
        anywhere.as_secs_f64() * 1e6
    );

    // Choosing a column: the percentile range and the per-row colours.
    let values: Vec<f32> = (0..objects).map(|_| random.next() as f32 * 200.0).collect();
    let started = Instant::now();
    let range = Range::robust(&values).expect("finite values");
    let ranged = started.elapsed();
    let mut colours = Vec::new();
    let started = Instant::now();
    range.colours_into(&values, &mut colours);
    let coloured = started.elapsed();
    println!(
        "choosing a column:         {:.2} ms for the range, {:.2} ms for the colours",
        ranged.as_secs_f64() * 1e3,
        coloured.as_secs_f64() * 1e3
    );
}
