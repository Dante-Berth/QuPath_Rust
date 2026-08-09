//! Rank filter throughput, for the record required by §10.
#![allow(missing_docs, reason = "criterion_group/criterion_main expansion")]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use qupath_filters::{RankOp, rank};

fn bench_rank(c: &mut Criterion) {
    let width = 512;
    let pixels: Vec<f32> = (0..width * width)
        .map(|i| ((i * 37) % 251) as f32)
        .collect();

    let mut group = c.benchmark_group("rank_512x512");
    group.throughput(criterion::Throughput::Elements((width * width) as u64));
    for radius in [2.0f64, 5.0] {
        group.bench_function(format!("max_r{radius}"), |b| {
            b.iter_batched(
                || pixels.clone(),
                |mut buf| {
                    rank(&mut buf, width, radius, RankOp::Max);
                    black_box(buf[0])
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_rank);
criterion_main!(benches);
