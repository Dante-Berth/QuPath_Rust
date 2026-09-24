//! Watching a run, and stopping one.
//!
//! [`crate::run_collecting`] over a whole slide is tens of seconds — 33.5 s for
//! CMU-1 at level 0 — and `docs/dev/VIEWER.md` §8.8 puts a run that long with no way
//! to stop it and no sense of how far along it is at the top of what is missing.
//! This is what a caller passes to get both.
//!
//! # Why a shared handle rather than a callback
//!
//! The obvious shape is a closure the tile loop calls. It is the wrong one
//! here. The loop is a rayon `par_iter`, so a callback would be invoked from
//! whichever worker finished a tile, concurrently with nineteen others: every
//! caller would have to make it `Sync` and do its own locking, and a caller that
//! wanted to *draw* the progress — which is the only reason to want it — would
//! be doing that from inside the pipeline's threads rather than on its own.
//!
//! A handle inverts it. The pipeline only ever *writes* two atomics, wait-free
//! and with no ordering between them and anything else. The reader polls when it
//! suits — once a frame, in the viewer's case — and a reader that never polls
//! costs the run two relaxed increments per tile, which is not measurable
//! against a tile that takes milliseconds.
//!
//! # What a fraction is honest about
//!
//! Tiles finished over tiles total, and nothing else. It is not time: tiles over
//! glass are far cheaper than tiles over tissue, so the fraction advances in
//! jumps and a linear extrapolation to a finish time would be a fiction. The
//! denominator is known before the first tile starts, which is what makes the
//! fraction real rather than a bar advancing on a timer — `docs/dev/VIEWER.md` §6 is
//! about exactly that distinction.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A handle on a run: how far it has got, and whether it should stop.
///
/// Created by the caller, handed to [`crate::run_collecting_watched`], and read
/// from any other thread while the run is going. Cloning is not offered; share
/// it behind an `Arc`, which is what a caller that outlives the run needs
/// anyway.
///
/// [`Progress::cancel`] is a request, not a guarantee of promptness: it is seen
/// when a worker starts its *next* tile, so a run stops within about one tile's
/// work rather than instantly. That is the granularity the tile loop offers
/// without threading a flag through every stage.
///
/// One tile is **not** negligible, and the figure is worth stating rather than
/// waving at. A 2052 px tile at level 0 of CMU-1 is about 1.7 s of work on one
/// thread. Measured end to end by `examples/stop_a_run.rs`: a whole-slide run
/// asked to stop three seconds in returned **1.45 s later**, having done 36 of
/// 391 tiles. So "about one tile" means a second or two at level 0 and less on
/// a coarser one — fast enough to feel like stopping, slow enough that an
/// interface should not promise it is instant.
#[derive(Debug, Default)]
pub struct Progress {
    /// Tiles finished. Written by the workers, read by anyone.
    done: AtomicU64,
    /// Tiles the run will process in total, written once before the loop
    /// starts. Zero until then, which is what [`Progress::fraction`] treats as
    /// "not started" rather than dividing by.
    total: AtomicU64,
    /// Set by [`Progress::cancel`], read at the top of each tile.
    cancelled: AtomicBool,
}

impl Progress {
    /// A handle on a run that has not started.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the run to stop.
    ///
    /// Takes effect within about one tile, per the type's documentation. Safe to
    /// call before the run starts, after it has finished, or twice.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether [`Progress::cancel`] has been called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Tiles finished so far.
    #[must_use]
    pub fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    /// Tiles the run will process, or `None` before it has started.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        match self.total.load(Ordering::Relaxed) {
            0 => None,
            total => Some(total),
        }
    }

    /// How far along the run is, in `0.0..=1.0`, or `None` before it starts.
    ///
    /// Tiles, not time — see the module documentation for why that distinction
    /// is the whole point.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total()?;
        // `done` is read after `total`, so it can only be more up to date, and
        // the clamp covers the case where a worker finished in between.
        #[expect(
            clippy::cast_precision_loss,
            reason = "tile counts are thousands; f64 is exact well past that"
        )]
        Some((self.done() as f64 / total as f64).min(1.0))
    }

    /// Clears a previous run, so the handle can be used for another.
    ///
    /// **The caller does this, not the pipeline**, and that is the whole of why
    /// it is public. Clearing the cancelled flag inside the run looks tidier and
    /// is a bug: `cancel` and the run's start happen on different threads — in
    /// the viewer, the UI thread and a worker — so a stop requested in the
    /// window between the two would be wiped by the run it was meant to stop.
    /// That is a cancellation that silently does nothing, which is the failure
    /// this whole file is careful about, and
    /// `cancelling_stops_the_run_before_it_finishes_its_tiles` caught exactly it.
    ///
    /// So the rule is: reset when you decide to start a run, before you hand the
    /// handle over. After that the flag is the caller's to set and nobody else's
    /// to clear.
    pub fn reset(&self) {
        self.done.store(0, Ordering::Relaxed);
        self.cancelled.store(false, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
    }

    /// Records the tile count, without touching the cancelled flag.
    ///
    /// Called by the pipeline once the grid is built. See [`Progress::reset`]
    /// for why clearing anything here would be wrong.
    pub(crate) fn start(&self, tiles: u64) {
        // `done` is cleared by `reset`, not here, for the same reason: a caller
        // that did not reset is reusing a handle deliberately and the pipeline
        // should not second-guess it.
        self.total.store(tiles, Ordering::Relaxed);
    }

    /// Records one finished tile.
    pub(crate) fn finished_tile(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_that_has_not_started_reports_nothing() {
        let progress = Progress::new();
        assert_eq!(progress.total(), None);
        assert_eq!(progress.fraction(), None);
        assert_eq!(progress.done(), 0);
        assert!(!progress.is_cancelled());
    }

    #[test]
    fn the_fraction_is_tiles_over_tiles() {
        let progress = Progress::new();
        progress.start(4);
        assert_eq!(progress.fraction(), Some(0.0));
        progress.finished_tile();
        assert_eq!(progress.fraction(), Some(0.25));
        progress.finished_tile();
        progress.finished_tile();
        progress.finished_tile();
        assert_eq!(progress.fraction(), Some(1.0));
    }

    /// The reason `fraction` clamps rather than trusting its own arithmetic.
    #[test]
    fn a_fraction_never_exceeds_one() {
        let progress = Progress::new();
        progress.start(2);
        for _ in 0..5 {
            progress.finished_tile();
        }
        assert_eq!(progress.fraction(), Some(1.0));
    }

    /// A handle is reusable, which is what lets a viewer keep one for its life.
    #[test]
    fn resetting_clears_the_previous_run() {
        let progress = Progress::new();
        progress.start(2);
        progress.finished_tile();
        progress.cancel();
        assert!(progress.is_cancelled());

        progress.reset();
        assert!(!progress.is_cancelled(), "a reset handle is uncancelled");
        assert_eq!(progress.done(), 0, "a reset handle is at no tiles done");
        assert_eq!(progress.total(), None, "a reset handle has not started");
    }

    /// The bug `reset` exists to prevent, as an assertion.
    ///
    /// A stop requested before the pipeline registers its tile count must
    /// survive that registration. `start` clearing the flag is the tidy-looking
    /// version that loses it, and loses it precisely in the race the viewer has:
    /// `cancel` on the UI thread against `start` on a worker.
    #[test]
    fn a_cancellation_survives_the_run_starting() {
        let progress = Progress::new();
        progress.cancel();
        progress.start(16);
        assert!(
            progress.is_cancelled(),
            "the pipeline registering its tiles must not clear a pending stop"
        );
    }
}
