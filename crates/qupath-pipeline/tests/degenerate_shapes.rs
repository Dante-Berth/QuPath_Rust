//! Every stage, on every awkward shape, asserting only that it comes back.
//!
//! Detection runs on tiles of roughly two thousand pixels a side, so nothing
//! here is a shape the pipeline meets today. That is exactly why it is worth
//! having: these routines are `pub`, they are the ones a future caller reaches
//! for when the tiling changes or a smaller ROI is detected in, and every one of
//! them indexes neighbours by hand. A one-pixel row is where an `x - 1` written
//! for the interior shows up.
//!
//! **It found nothing**, which is worth stating plainly rather than leaving the
//! reader to assume it was written after a failure. Every stage came back on
//! every shape on the first run. It is kept as a guard on the next hand-written
//! neighbour loop, not as a record of one that went wrong.
//!
//! One thing it does *not* cover, because reproducing it would mean diverging
//! from ImageJ: `Segmenter`'s `set_points` is sized
//! `min(max_bin, pixels.div_ceil(3))`, which is `Math.min(maxBinSize,
//! (width*height+2)/3)` in `MaximumFinder.watershedSegment`. When one grey level
//! holds more than a third of the image and a single pass sets all of it, that
//! buffer is too short — an `ArrayIndexOutOfBoundsException` there and a panic
//! here. Inherited, not introduced, and left alone.
//!
//! It asserts liveness, not values. Shapes this small have no meaningful
//! expected output, and the fixtures in `parity/` are what pin behaviour; a test
//! that invented expectations here would be asserting its own arithmetic.

use qupath_filters::{
    Convolver, GaussianBlur, RankFilter, RankOp, Reconstructor, find_regional_maxima,
};
use qupath_pipeline::{BoundaryRefiner, FOREGROUND, Merger};
use qupath_watershed::{Connectivity, DistanceMap, Labeller, RegionTracer, Segmenter, Watershed};

/// The shapes worth trying, as (width, height).
///
/// One-pixel rows and columns are the point; the 1x1 and 2x2 cases catch a loop
/// that assumes it has an interior at all.
const SHAPES: [(usize, usize); 14] = [
    (1, 1),
    (1, 2),
    (2, 1),
    (1, 7),
    (7, 1),
    (2, 2),
    (2, 3),
    (3, 2),
    (3, 3),
    (4, 1),
    (1, 4),
    (5, 4),
    (16, 1),
    (1, 16),
];

/// A surface with a peak, a plateau and a background, whatever the shape.
fn surface(width: usize, height: usize) -> Vec<f32> {
    (0..width * height)
        .map(|i| {
            let (x, y) = (i % width, i / width);
            match (x + y * 3) % 5 {
                0 => 1.0,
                1 => 0.75,
                2 => -0.25,
                3 => 0.0,
                _ => 0.5,
            }
        })
        .collect()
}

/// A binary mask holding nothing but 0 and [`FOREGROUND`], as the merge requires.
fn binary(width: usize, height: usize) -> Vec<u8> {
    (0..width * height)
        .map(|i| if (i * 7) % 11 < 7 { FOREGROUND } else { 0 })
        .collect()
}

#[test]
fn the_filters_survive_every_shape() {
    for (width, height) in SHAPES {
        let image = surface(width, height);
        let mut out = vec![0.0f32; image.len()];

        for radius in [0.5f64, 1.0, 2.0, 5.0, 17.0] {
            for op in [RankOp::Min, RankOp::Max] {
                let mut pixels = image.clone();
                RankFilter::new().apply(&mut pixels, width, radius, op);
                RankFilter::new().apply_into(&image, &mut out, width, radius, op);
            }
        }

        for sigma in [0.5f64, 1.5, 3.006, 9.0] {
            let mut pixels = image.clone();
            GaussianBlur::new().blur(&mut pixels, width, sigma);
        }

        let laplacian = [0.0f32, -1.0, 0.0, -1.0, 4.0, -1.0, 0.0, -1.0, 0.0];
        Convolver::new().apply_into(&image, &mut out, width, &laplacian, 3, 3);

        // Both routes into the reconstruction: the marker/mask form, and the
        // regional-maximum transform that punches its own holes.
        let mut marker = vec![f32::NEG_INFINITY; image.len()];
        marker[0] = image[0];
        Reconstructor::new().reconstruct(&mut marker, &image, width);
        let _ = find_regional_maxima(&image, width, 0.001);
    }
}

#[test]
fn the_watershed_chain_survives_every_shape() {
    for (width, height) in SHAPES {
        let image = surface(width, height);

        for connectivity in [Connectivity::Four, Connectivity::Eight] {
            let mut labels = vec![0.0f32; image.len()];
            let _ = Labeller::new().label_into(&image, &mut labels, width, 0.0, connectivity);

            let mut flooded = labels;
            Watershed::new().flood(&image, &mut flooded, width, 0.0, connectivity);

            let mut tracer = RegionTracer::new();
            let mut regions = vec![0u32; image.len()];
            let _ = tracer.trace_into(&flooded, &mut regions, width, 0.5, f32::INFINITY);
        }
    }
}

#[test]
fn the_merge_and_split_survive_every_shape() {
    for (width, height) in SHAPES {
        let log = surface(width, height);
        let mut mask = binary(width, height);

        let mut tracer = RegionTracer::new();
        let _ = Merger::new().merge(&mut mask, &log, width, &mut tracer);

        // The split, on the merged mask: distance map, then the segmentation
        // that cuts lines into it.
        let mut edm = vec![0.0f32; mask.len()];
        DistanceMap::new().make_float_edm(&mask, &mut edm, width);
        Segmenter::new().segment(&edm, &mut mask, width);

        // And on a mask that is entirely foreground, which is the case with no
        // background for the distance map to grow from.
        let mut solid = vec![FOREGROUND; width * height];
        let mut edm = vec![0.0f32; solid.len()];
        DistanceMap::new().make_float_edm(&solid, &mut edm, width);
        Segmenter::new().segment(&edm, &mut solid, width);

        // And on one that is entirely background.
        let mut empty = vec![0u8; width * height];
        let mut edm = vec![0.0f32; empty.len()];
        DistanceMap::new().make_float_edm(&empty, &mut edm, width);
        Segmenter::new().segment(&edm, &mut empty, width);
    }
}

#[test]
fn the_boundary_refinement_survives_every_shape() {
    // Its erosion clamps in both directions and its blur runs at sigma 1, which
    // is wider than most of these shapes — a kernel radius larger than the image
    // is the case ImageJ handles through the tail sum and a port might not.
    for (width, height) in SHAPES {
        let detection = surface(width, height);
        let mut mask = binary(width, height);
        let (mut smoothed, mut cleanup) = (Vec::new(), Vec::new());
        BoundaryRefiner::new().refine(&detection, &mut mask, width, &mut smoothed, &mut cleanup);

        let mut solid = vec![FOREGROUND; width * height];
        BoundaryRefiner::new().refine(&detection, &mut solid, width, &mut smoothed, &mut cleanup);
        assert_eq!(
            solid,
            vec![FOREGROUND; width * height],
            "{width}x{height}: a mask with no boundary has nothing to refine, \
             because every out-of-image neighbour is a copy of a foreground one"
        );

        let mut empty = vec![0u8; width * height];
        BoundaryRefiner::new().refine(&detection, &mut empty, width, &mut smoothed, &mut cleanup);
        assert!(
            empty.iter().all(|&p| p == 0),
            "{width}x{height}: the refinement cannot add a pixel"
        );
    }
}
