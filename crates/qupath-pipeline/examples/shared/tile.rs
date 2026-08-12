//! One real tile, carried through the chain `qupath_pipeline::run` runs.
//!
//! Shared by the probes that need the pixels a late stage actually meets rather
//! than a generated image — ROADMAP §5's standing entry about synthetic inputs
//! picking the wrong optimisation. Every call below is the one the tile loop
//! makes, transcribed against the public API.
//!
//! Not an example of its own: cargo only auto-discovers `examples/*.rs`, so this
//! is reached by `#[path]` from the probes that want it.
#![allow(dead_code, reason = "each probe uses a different subset")]

use qupath_filters::{Convolver, GaussianBlur, RankFilter, RankOp, Reconstructor};
use qupath_pipeline::{Merger, ObjectFilter, Params, Region, Stains};
use qupath_slide::Slide;
use qupath_watershed::{Connectivity, Labeller, RegionTracer, Watershed};

/// The kernel completing the Laplacian of Gaussian, as `qupath_pipeline` has it.
const LAPLACIAN: [f32; 9] = [0.0, -1.0, 0.0, -1.0, 4.0, -1.0, 0.0, -1.0, 0.0];

/// The images one tile holds by the time the late stages run.
pub struct Tile {
    /// QuPath's `ipToMeasure`: the optical density after the background
    /// subtraction and before the blur.
    pub to_measure: Vec<f32>,
    /// The Laplacian of Gaussian the watershed floods.
    pub log: Vec<f32>,
    /// The flooded label image, which is what the first trace is handed.
    pub labels: Vec<f32>,
    /// QuPath's `bp` after the merge: the mask the shape split is handed.
    pub binary: Vec<u8>,
    /// How many regions the merge left.
    pub merged: u32,
}

impl Tile {
    /// Runs the chain from the deconvolution to the merge over `region`.
    ///
    /// # Panics
    ///
    /// Panics if the region cannot be read.
    pub fn build(slide: &Slide, region: Region, params: Params) -> Self {
        let width = region.width as usize;
        let rgb = slide
            .read_region_rgb(
                params.level,
                region.x,
                region.y,
                region.width,
                region.height,
            )
            .expect("read region");

        let stains = Stains::h_and_e();
        let mut density = Vec::new();
        stains.haematoxylin_into(&rgb, &mut density);

        // The background: erode, regrow under the original, subtract.
        let mut background = vec![0.0f32; density.len()];
        RankFilter::new().apply_into(
            &density,
            &mut background,
            width,
            params.background_radius,
            RankOp::Min,
        );
        Reconstructor::new().reconstruct(&mut background, &density, width);
        for (value, &estimate) in density.iter_mut().zip(&background) {
            *value -= estimate;
        }
        let to_measure = density.clone();

        GaussianBlur::new().blur(&mut density, width, params.sigma);
        let mut log = vec![0.0f32; density.len()];
        Convolver::new().apply_into(&density, &mut log, width, &LAPLACIAN, 3, 3);

        let mut prominence = Vec::new();
        Reconstructor::new().regional_maxima_into(&log, &mut prominence, width, 0.001);
        let mut labels = vec![0.0f32; log.len()];
        Labeller::new().label_into(&prominence, &mut labels, width, 0.0, Connectivity::Four);
        Watershed::new().flood(
            &log,
            &mut labels,
            width,
            params.threshold,
            params.connectivity,
        );

        let mut tracer = RegionTracer::new();
        let mut regions = vec![0u32; labels.len()];
        let traced = tracer.trace_into(&labels, &mut regions, width, 0.5, f32::INFINITY);

        let mut filter = ObjectFilter::new();
        filter.apply(
            &to_measure,
            &regions,
            traced,
            params.intensity_threshold,
            None,
        );
        let mut binary = vec![0u8; regions.len()];
        filter.write_binary(&regions, &mut binary);

        let merged = Merger::new().merge(&mut binary, &log, width, &mut tracer);
        Self {
            to_measure,
            log,
            labels,
            binary,
            merged,
        }
    }
}
