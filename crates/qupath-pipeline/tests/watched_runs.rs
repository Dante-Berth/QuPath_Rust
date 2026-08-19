//! Watching a run and stopping one, against a real slide.
//!
//! `docs/VIEWER.md` §8.8 asks for these two because a whole-slide run is tens
//! of seconds. What can go wrong is not a crash:
//!
//! - a watched run that quietly detects something *different* from an unwatched
//!   one, which would make the feature cost correctness for a progress bar;
//! - a cancellation that returns promptly while the run keeps going behind it,
//!   which looks like it worked and leaves a thread burning a core;
//! - a fraction that is a timer in disguise, advancing whether or not work
//!   happened — §6's rule, applied to a number this file now exposes.
//!
//! The first is asserted field by field, the second by observing that the run
//! stopped early *and* did not finish its tiles, and the third by the unit
//! tests in `progress` plus the denominator check here.

use std::path::Path;
use std::sync::Arc;

use qupath_pipeline::{
    Params, PipelineError, Progress, Region, run_collecting, run_collecting_watched,
};
use qupath_slide::Slide;

/// The fixture, which is not in the repository. Returns `None` so the suite
/// stays green on a checkout without it, as the other integration tests do.
fn slide() -> Option<Slide> {
    let path = Path::new("../../data/CMU-1-Small-Region.svs");
    path.exists()
        .then(|| Slide::open(path).expect("open fixture"))
}

/// Small enough to run quickly, tiled finely enough that there are many tiles
/// to count and to stop between.
fn params() -> (Region, Params) {
    let region = Region {
        x: 0,
        y: 0,
        width: 1024,
        height: 1024,
    };
    let params = Params {
        tile_preferred: 256,
        tile_max: 320,
        ..Params::default()
    };
    (region, params)
}

#[test]
fn a_watched_run_finds_what_an_unwatched_one_finds() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();

    let (plain, plain_objects) = run_collecting(&slide, region, params).expect("run_collecting");
    let progress = Progress::new();
    let (watched, watched_objects) =
        run_collecting_watched(&slide, region, params, &progress).expect("watched run");

    // Field by field for the reason `collected_detections` gives: a path that
    // quietly runs a stage differently shows up first in whichever count nobody
    // thought to compare.
    assert_eq!(watched.tiles, plain.tiles);
    assert_eq!(watched.pixels, plain.pixels);
    assert_eq!(watched.markers, plain.markers);
    assert_eq!(watched.labelled, plain.labelled);
    assert_eq!(watched.regions, plain.regions);
    assert_eq!(watched.candidates, plain.candidates);
    assert_eq!(watched.merged, plain.merged);
    assert_eq!(watched.nuclei, plain.nuclei);
    assert_eq!(watched.objects, plain.objects);
    assert_eq!(watched.duplicates, plain.duplicates);
    assert_eq!(watched_objects.len(), plain_objects.len());
}

#[test]
fn a_finished_run_leaves_the_handle_complete() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();

    let progress = Progress::new();
    let (outcome, _) =
        run_collecting_watched(&slide, region, params, &progress).expect("watched run");

    // The denominator is the run's own tile count, not a guess -- this is what
    // makes the fraction a real one rather than a bar on a timer.
    assert_eq!(progress.total(), Some(outcome.tiles as u64));
    assert_eq!(progress.done(), outcome.tiles as u64);
    assert_eq!(progress.fraction(), Some(1.0));
    assert!(!progress.is_cancelled());
}

/// Cancelling from another thread stops the run, and stops it *early*.
///
/// The second half is the one worth having. A test that only asserted the
/// error would pass against an implementation that ran every tile and then
/// reported cancellation at the end -- which is exactly the failure mode this
/// feature exists to prevent, and it would look identical from the outside.
#[test]
fn cancelling_stops_the_run_before_it_finishes_its_tiles() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();

    // How many tiles the run has, from a completed one, so the assertion below
    // compares against the real total rather than an assumed one.
    let reference = Progress::new();
    let (finished, _) =
        run_collecting_watched(&slide, region, params, &reference).expect("reference run");
    let total = finished.tiles as u64;
    assert!(
        total >= 4,
        "the fixture must tile into several tiles for this to mean anything, got {total}"
    );

    let progress = Arc::new(Progress::new());
    // Cancelled before the run starts. Racing a sleeping thread against a
    // fixture that takes well under a second is how this test would become
    // flaky; asking for the stop up front tests the same branch -- a worker
    // reading the flag at the top of a tile -- without depending on timing.
    progress.cancel();

    let error = run_collecting_watched(&slide, region, params, &progress)
        .expect_err("a cancelled run must not return an outcome");

    match error {
        PipelineError::Cancelled { done, total: seen } => {
            assert_eq!(seen, total, "cancellation reports the run's real total");
            assert!(
                done < total,
                "stopped after {done} of {total} tiles, which is not stopping early"
            );
        }
        other => panic!("expected Cancelled, got {other}"),
    }

    // And the handle agrees with the error: the work really did not happen.
    assert!(progress.done() < total);
}

/// A handle cancelled for one run does not poison the next.
///
/// `Progress::reset` clears the flag, which is what lets a caller keep one
/// handle for the life of a viewer rather than allocating one per run. Without
/// the reset the first cancellation would stop every run thereafter -- a bug
/// that only appears on the *second* gesture, which is the kind this suite is
/// for. The reset is the caller's to do, deliberately: see `Progress::reset`.
#[test]
fn a_handle_reused_after_a_cancellation_runs_again() {
    let Some(slide) = slide() else {
        return;
    };
    let (region, params) = params();

    let progress = Progress::new();
    progress.cancel();
    run_collecting_watched(&slide, region, params, &progress).expect_err("cancelled");

    progress.reset();
    let (outcome, detections) =
        run_collecting_watched(&slide, region, params, &progress).expect("the second run goes");
    assert!(!progress.is_cancelled());
    assert_eq!(progress.done(), outcome.tiles as u64);
    assert!(
        !detections.is_empty(),
        "the second run must actually detect something"
    );
}
