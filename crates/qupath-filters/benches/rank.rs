//! Rank filter throughput, for the record required by §10.
//!
//! The buffers are allocated once, outside the timed region. An earlier version
//! of this benchmark used `iter_batched` to hand the routine a fresh clone of a
//! 1 MB input per iteration, and the resulting allocator and page-fault traffic
//! dominated the measurement — it reported radius 10 as *faster* than radius 5,
//! and inflated the radius-2 figure by roughly 4x against a direct timing of
//! the same call. Benchmarks of a memory-bound kernel have to own their memory.
#![allow(missing_docs, reason = "criterion_group/criterion_main expansion")]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use qupath_filters::{RankFilter, RankOp};

fn bench_rank(c: &mut Criterion) {
    let width = 512;
    let pixels: Vec<f32> = (0..width * width)
        .map(|i| ((i * 37) % 251) as f32)
        .collect();

    let mut group = c.benchmark_group("rank_512x512");
    group.throughput(criterion::Throughput::Elements((width * width) as u64));
    // 2.0 and 5.0 are the radii ImageJ was recorded at, so they are the
    // like-for-like comparison. The rest exist to expose the *scaling*: a
    // per-pixel cost that grows with the kernel shows up here and nowhere else.
    for radius in [1.0f64, 2.0, 3.0, 5.0, 8.0, 10.0, 20.0] {
        // Out of place, which is the shape a tile pipeline has: a decoded input
        // tile and a separate output tile. Nothing is copied and nothing is
        // allocated inside the loop.
        group.bench_function(format!("max_r{radius}"), |b| {
            let mut filter = RankFilter::new();
            let mut out = vec![0.0f32; pixels.len()];
            b.iter(|| {
                filter.apply_into(&pixels, &mut out, width, radius, RankOp::Max);
                black_box(out[0])
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_rank);
criterion_main!(benches);
