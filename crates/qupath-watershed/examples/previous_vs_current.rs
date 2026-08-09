//! A/B probe: the previous flood against the current one, interleaved.
//!
//! The benchmark harness changed in the same commit as the flood loop, so the
//! criterion figures before and after are not measuring the same thing. This
//! isolates the code change: both implementations run in one process, on the
//! same buffers, and the minimum over many repetitions estimates the
//! uncontended cost on a machine that is not quiet.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "scratch probe")]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "indices bounded by the fixed probe image size"
)]

use std::hint::black_box;
use std::time::Instant;

use qupath_watershed::{Connectivity, HierarchicalQueue, PixelIndex, Watershed};

/// The flood this change replaces, verbatim from commit aa7b9eb: coordinates
/// recovered per pop with an integer division, and every neighbour reached
/// through a bounds-checked `(x, y)` shift.
mod previous {
    use super::{Connectivity, HierarchicalQueue, PixelIndex};

    pub fn watershed(
        values: &[f32],
        labels: &mut [f32],
        width: usize,
        min_threshold: f32,
        connectivity: Connectivity,
    ) {
        let height = values.len() / width;
        let mut queued = vec![false; values.len()];

        let (min_value, max_value) = value_range(values, min_threshold);
        let mut queue = HierarchicalQueue::new(min_value, max_value, values.len());

        seed(
            values,
            labels,
            width,
            height,
            min_threshold,
            &mut queued,
            &mut queue,
        );

        let offsets = offsets(connectivity);
        while let Some(pixel) = queue.pop() {
            let index = pixel as usize;
            let x = index % width;
            let y = index / width;

            let Some(label) = neighbour_label(labels, width, height, x, y, offsets) else {
                continue;
            };
            labels[index] = label;

            for &(dx, dy) in offsets {
                let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
                    continue;
                };
                let neighbour = ny * width + nx;
                if queued[neighbour] {
                    continue;
                }
                queued[neighbour] = true;
                queue.push(neighbour as PixelIndex, values[neighbour]);
            }
        }
    }

    fn offsets(connectivity: Connectivity) -> &'static [(i32, i32)] {
        match connectivity {
            Connectivity::Four => &[(0, -1), (-1, 0), (1, 0), (0, 1)],
            Connectivity::Eight => &[
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ],
        }
    }

    fn value_range(values: &[f32], min_threshold: f32) -> (f32, f32) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &v in values {
            if v <= min_threshold || !v.is_finite() {
                continue;
            }
            min = min.min(v);
            max = max.max(v);
        }
        if min > max {
            return (0.0, 1.0);
        }
        (min, max)
    }

    fn seed(
        values: &[f32],
        labels: &[f32],
        width: usize,
        height: usize,
        min_threshold: f32,
        queued: &mut [bool],
        queue: &mut HierarchicalQueue,
    ) {
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                let value = values[index];
                if value <= min_threshold {
                    queued[index] = true;
                    continue;
                }
                if labels[index] != 0.0 {
                    queued[index] = true;
                } else if has_labelled_orthogonal_neighbour(labels, width, height, x, y) {
                    queued[index] = true;
                    queue.push(index as PixelIndex, value);
                }
            }
        }
    }

    fn has_labelled_orthogonal_neighbour(
        labels: &[f32],
        width: usize,
        height: usize,
        x: usize,
        y: usize,
    ) -> bool {
        const ORTHOGONAL: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, -1), (0, 1)];
        ORTHOGONAL.iter().any(|&(dx, dy)| {
            shift(x, y, dx, dy, width, height).is_none_or(|(nx, ny)| labels[ny * width + nx] != 0.0)
        })
    }

    fn neighbour_label(
        labels: &[f32],
        width: usize,
        height: usize,
        x: usize,
        y: usize,
        offsets: &[(i32, i32)],
    ) -> Option<f32> {
        let mut found: Option<f32> = None;
        for &(dx, dy) in offsets {
            let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
                continue;
            };
            let label = labels[ny * width + nx];
            if label == 0.0 {
                continue;
            }
            match found {
                None => found = Some(label),
                Some(existing) if existing != label => return None,
                Some(_) => {}
            }
        }
        found
    }

    fn shift(
        x: usize,
        y: usize,
        dx: i32,
        dy: i32,
        width: usize,
        height: usize,
    ) -> Option<(usize, usize)> {
        let nx = x.checked_add_signed(dx as isize)?;
        let ny = y.checked_add_signed(dy as isize)?;
        (nx < width && ny < height).then_some((nx, ny))
    }
}

fn value_image(width: usize, height: usize) -> Vec<f32> {
    let mut values = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let fx = (x % 64) as f32 - 32.0;
            let fy = (y % 64) as f32 - 32.0;
            values.push(((1024.0 - fx * fx - fy * fy).max(0.0) / 8.0).floor());
        }
    }
    values
}

fn main() {
    let reps = 100;
    println!("EDM-like plateau surface, 8-connected, minimum of {reps} interleaved repetitions\n");
    println!(
        "{:>11}  {:>10}  {:>10}  {:>10}  {:>8}",
        "image", "previous", "current", "reused", "speedup"
    );

    for &(width, height) in &[(256usize, 256usize), (512, 512), (1024, 1024)] {
        let values = value_image(width, height);

        let mut seeds = vec![0.0f32; width * height];
        let mut label = 1.0;
        for y in (32..height).step_by(64) {
            for x in (32..width).step_by(64) {
                seeds[y * width + x] = label;
                label += 1.0;
            }
        }

        let mut labels = seeds.clone();
        let mut flooder = Watershed::new();
        flooder.flood(
            &values,
            &mut labels,
            width,
            f32::NEG_INFINITY,
            Connectivity::Eight,
        );

        let (mut old_best, mut new_best, mut reused_best) = (f64::MAX, f64::MAX, f64::MAX);

        for _ in 0..reps {
            labels.copy_from_slice(&seeds);
            let start = Instant::now();
            previous::watershed(
                &values,
                &mut labels,
                width,
                f32::NEG_INFINITY,
                Connectivity::Eight,
            );
            old_best = old_best.min(start.elapsed().as_secs_f64() * 1e3);
            let from_previous = labels.clone();

            labels.copy_from_slice(&seeds);
            let start = Instant::now();
            qupath_watershed::watershed(
                &values,
                &mut labels,
                width,
                f32::NEG_INFINITY,
                Connectivity::Eight,
            );
            new_best = new_best.min(start.elapsed().as_secs_f64() * 1e3);
            black_box(&labels);

            // A watershed that got fast by labelling something else is not a
            // faster watershed. Labels are exact small integers in f32.
            assert_eq!(labels, from_previous, "floods diverged on {width}x{height}");

            labels.copy_from_slice(&seeds);
            let start = Instant::now();
            flooder.flood(
                &values,
                &mut labels,
                width,
                f32::NEG_INFINITY,
                Connectivity::Eight,
            );
            reused_best = reused_best.min(start.elapsed().as_secs_f64() * 1e3);
            assert_eq!(labels, from_previous, "reuse diverged on {width}x{height}");
        }

        println!(
            "{:>11}  {old_best:>9.2}ms  {new_best:>9.2}ms  {reused_best:>9.2}ms  {:>7.2}x",
            format!("{width}x{height}"),
            old_best / reused_best
        );
    }
}
