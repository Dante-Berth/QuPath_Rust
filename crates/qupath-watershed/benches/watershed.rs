//! Hierarchical queue against a binary-heap baseline shaped like QuPath's.
//!
//! The baseline is not a strawman: it is `BinaryHeap<PixelWithValue>` with the
//! same value-descending, insertion-order-tiebreak ordering QuPath uses, so the
//! measured difference is the data structure and nothing else.

// criterion's macros generate undocumented items.
#![allow(missing_docs, reason = "criterion_group/criterion_main expansion")]
// Indices here are bounded by a 1024×1024 benchmark image built in this file,
// so every cast below is provably in range.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    reason = "indices bounded by the fixed benchmark image size"
)]

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use qupath_watershed::{Connectivity, HierarchicalQueue, watershed};

/// QuPath's `PixelWithValue`: a heap object per queued pixel, ordered by value
/// descending with an insertion counter breaking ties.
#[derive(PartialEq)]
struct PixelWithValue {
    x: i32,
    y: i32,
    value: f32,
    count: u64,
}

impl Eq for PixelWithValue {}

impl Ord for PixelWithValue {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap, so this is QuPath's comparator inverted:
        // higher value first, then lower insertion count first.
        match self.value.partial_cmp(&other.value) {
            Some(Ordering::Equal) | None => other.count.cmp(&self.count),
            Some(order) => order,
        }
    }
}

impl PartialOrd for PixelWithValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A deterministic value image with the plateau structure that makes the
/// tiebreak matter — an EDM-like surface quantised so equal values are common.
fn value_image(width: usize, height: usize) -> Vec<f32> {
    let mut values = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let fx = (x % 64) as f32 - 32.0;
            let fy = (y % 64) as f32 - 32.0;
            // Quantised so plateaus form, as in a real distance map.
            values.push(((1024.0 - fx * fx - fy * fy).max(0.0) / 8.0).floor());
        }
    }
    values
}

fn bench_queue(c: &mut Criterion) {
    let width = 1024;
    let height = 1024;
    let values = value_image(width, height);
    let n = values.len();

    let mut group = c.benchmark_group("queue_push_pop");
    group.throughput(criterion::Throughput::Elements(n as u64));

    group.bench_function("hierarchical", |b| {
        b.iter(|| {
            let mut q = HierarchicalQueue::new(0.0, 128.0, n);
            for (i, &v) in values.iter().enumerate() {
                q.push(i as u32, v);
            }
            let mut sum = 0u64;
            while let Some(p) = q.pop() {
                sum += u64::from(p);
            }
            black_box(sum)
        });
    });

    group.bench_function("binary_heap", |b| {
        b.iter(|| {
            let mut heap = BinaryHeap::with_capacity(n);
            for (i, &v) in values.iter().enumerate() {
                heap.push(PixelWithValue {
                    x: (i % width) as i32,
                    y: (i / width) as i32,
                    value: v,
                    // QuPath's insertion counter starts at 1; the index gives
                    // the same ordering without a separate mutable counter.
                    count: i as u64 + 1,
                });
            }
            let mut sum = 0u64;
            while let Some(p) = heap.pop() {
                sum += (p.y as u64) * width as u64 + p.x as u64;
            }
            black_box(sum)
        });
    });

    group.finish();
}

fn bench_watershed(c: &mut Criterion) {
    let width = 512;
    let height = 512;
    let values = value_image(width, height);

    // Markers on a grid, as cell-detection maxima would be.
    let mut seeds = vec![0.0f32; width * height];
    let mut label = 1.0;
    for y in (32..height).step_by(64) {
        for x in (32..width).step_by(64) {
            seeds[y * width + x] = label;
            label += 1.0;
        }
    }

    c.bench_function("watershed_512x512", |b| {
        b.iter_batched(
            || seeds.clone(),
            |mut labels| {
                watershed(
                    &values,
                    &mut labels,
                    width,
                    f32::NEG_INFINITY,
                    Connectivity::Eight,
                );
                black_box(labels.iter().filter(|&&l| l != 0.0).count())
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, bench_queue, bench_watershed);
criterion_main!(benches);
