//! Stops a whole-slide run partway, and reports where it got to.
//!
//! `docs/dev/VIEWER.md` §8.10: the claim is that a run stops within about one
//! tile. That is a claim about wall time on a real slide, which no unit test
//! reaches -- the integration tests cancel before the run starts, deliberately,
//! so they do not race. This is the other half.
#![allow(missing_docs, reason = "example binary")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use qupath_pipeline::{Params, PipelineError, Progress, Region, run_collecting_watched};
use qupath_slide::Slide;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "data/CMU-1.svs".to_owned());
    let slide = Slide::open(&path).expect("open the slide");
    let level = &slide.levels()[0];
    let region = Region {
        x: 0,
        y: 0,
        width: level.width,
        height: level.height,
    };

    let progress = Arc::new(Progress::new());
    let watcher = Arc::clone(&progress);
    // Stop after three seconds of a run that takes about thirty-four.
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let at = watcher.fraction();
        let asked = Instant::now();
        watcher.cancel();
        println!("cancelled at {:?}", at);
        (asked, at)
    });

    let started = Instant::now();
    let result = run_collecting_watched(&slide, region, Params::default(), &progress);
    let took = started.elapsed();
    let (asked, at) = stopper.join().expect("stopper");

    match result {
        Err(PipelineError::Cancelled { done, total }) => {
            println!("stopped after {done} of {total} tiles");
            println!("run lasted {:.2} s", took.as_secs_f64());
            println!(
                "stopped {:.2} s after being asked",
                asked.elapsed().as_secs_f64()
            );
            println!("fraction when asked: {at:?}");
        }
        Ok((outcome, detections)) => {
            println!(
                "NOT STOPPED: ran to completion, {} objects over {} tiles in {:.2} s",
                detections.len(),
                outcome.tiles,
                took.as_secs_f64()
            );
        }
        Err(other) => println!("failed: {other}"),
    }
}
