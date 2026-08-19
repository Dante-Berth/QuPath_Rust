//! Running detection over what is on screen, without stopping the window.
//!
//! `docs/VIEWER.md` §3.6 left this as "a background thread and a progress bar
//! rather than anything unknown", and the thread is the part that carries a
//! decision. Everything else here — [`qupath_pipeline::run_collecting`],
//! [`Overlay`] — already existed and is called rather than reimplemented.
//!
//! # Why a thread and not a call
//!
//! Measured on CMU-1: a 1:1 view of tissue is **0.38 s** for 1357 nuclei, and a
//! fitted view of the whole slide is **1.24 s** for 1638. Both are far past the
//! 16.7 ms of a frame, so neither can happen on the main thread without the
//! window stopping — and stopping for a second while a button is held down is
//! how an application looks broken to the person using it.
//!
//! The run goes to a thread and the window keeps drawing, panning and zooming
//! while it happens. That the fast case is *only* 0.38 s is not a reason to
//! call it inline: it is 0.38 s on this slide at this zoom, and the thread
//! costs nothing to keep.
//!
//! # Which level a run reads
//!
//! **The coarsest level that can still resolve a nucleus**, which is not always
//! the level the viewer is drawing. See [`MAX_MICRONS_PER_PIXEL`].
//!
//! This used to be the viewer's own level, chosen so that "detect what is on
//! screen" was bounded work at every zoom. That is a real property and it is
//! why the rule is a *cap* rather than a fixed level 0: a run at a moderate
//! zoom still reads the cheap level it always did.
//!
//! What it got wrong is that a level cheap enough to draw is not always fine
//! enough to measure. Detection parameters are in microns and the pipeline
//! converts them with the level's pixel size, so a coarse run is arithmetically
//! consistent — but no conversion puts back evidence the downsampling removed.
//! At CMU-1's level 2 a pixel is 8 µm and a nucleus is 5–10, so what survived
//! the filters was tissue, reported as a plausible object count. The numbers
//! below say it: a fitted whole-slide view found 1638 objects where one
//! screenful of tissue found 1357, when the whole slide holds far more nuclei
//! than one screenful.
//!
//! The cost is real and is bounded by [`MAX_PIXELS`] rather than by the zoom.
//! That cap now sits above a whole slide: `qupath_pipeline` already tiles
//! internally, in parallel, and already removes the duplicates that fall on
//! tile seams, so a whole-slide run needs no tiling *here* — measured on CMU-1,
//! all 382,432 nuclei at level 0 in 33.5 s, with peak memory set by the thread
//! count rather than by the region. See [`MAX_PIXELS`] for the measurements and
//! for what the previous cap got wrong.
//!
//! Objects found at a coarse level are also coarse — outlines traced in level
//! pixels and scaled up — which is still true of any run above level 0 and is
//! the honest thing to show.
//!
//! # What is not offered, and why
//!
//! **No cancellation, and no progress fraction.** `run_collecting` is one
//! synchronous call with no callback and no interrupt flag, so there is nothing
//! to poll and nothing to interrupt without changing the pipeline. Rather than
//! grow a plausible-looking progress bar that advances on a timer, this reports
//! what it actually knows — a run is going, and here is how long it has been
//! going — and [`Detector::start`] supersedes rather than cancels: the old run
//! finishes into a result that is then dropped unread. That wastes at most one
//! run's work, and the alternative is a lie in the status bar.
//!
//! [`Detector::generation`] is what makes superseding safe, and it is the same
//! device `qupath_viewer::loader` uses for the same reason: a result that
//! answers a request nobody is waiting for any more must be discarded, not
//! drawn.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use qupath_pipeline::{self as pipeline, Micron, Params, Progress, Region};
use qupath_slide::{Level, Slide};
use qupath_viewer::{Overlay, Viewport};

/// The most pixels one run will read.
///
/// Counted in the pixels of the level being read, not in level-0 ones, because
/// that is what the run's cost is proportional to: reading a whole slide at
/// level 3 is a sixteenth of the work of one screenful of it at level 0, and a
/// cap expressed in level-0 pixels would refuse the cheap one and allow the
/// expensive one.
///
/// # What this does *not* bound, and why that was the mistake
///
/// This was 64 Mpx, chosen against a measured 4.5 Mpx/s to put a run's ceiling
/// at about fifteen seconds. Both halves of that were wrong, and the way they
/// were wrong is worth keeping.
///
/// The rate was read off two views that are mostly **glass**. The pipeline is
/// not linear in area and it is not linear in *content* either: a region with
/// nothing in it spends its time decoding and finds nothing to trace, so it
/// reports a low Mpx/s that says more about the tissue than the throughput.
/// Worse, neither view had tiles enough to occupy the thread pool. Measured on
/// a region that does — 212 Mpx over 48 tiles on 20 threads — the rate is
/// **40.8 Mpx/s**, nine times the figure this cap was set from. The whole of
/// CMU-1 at level 0 is 1514 Mpx and `run_collecting` returns all **382,432**
/// nuclei from it in **33.5 s**.
///
/// The other half was a guess about memory that turned out to be answerable.
/// `qupath_pipeline::run_collecting` streams tiles and accumulates only the
/// objects, so peak memory is set by **how many tiles are in flight at once**,
/// which is the thread count, and not by the region. Measured: 4 tiles peak at
/// 1092 MB, 48 tiles at 5657 MB, and the whole slide at 5609 MB — the same
/// ceiling as 48 tiles, because 20 threads cannot hold more than 20 tiles of
/// scratch whatever they are asked for. The detections themselves are 340 bytes
/// an object, so all 382,432 of them come to about 124 MB on top. **A whole
/// slide costs no more peak memory than a region large enough to fill the
/// threads.**
///
/// So a cap that shrinks the region buys nothing in memory and, at the real
/// rate, very little in time. It bounds only the wait.
///
/// # What it is now
///
/// 2048 Mpx, which admits the whole of CMU-1 at level 0 with room over. At the
/// measured 40.8 Mpx/s that is about **fifty seconds** at the very top — long,
/// and the run is on a worker thread with the window still live, the status bar
/// counting, and `Detector::start` free to supersede it. That is the ceiling on
/// a run somebody asked for deliberately, not a duration a careless gesture
/// reaches: the level cap keeps a zoomed-out run on a coarse level unless the
/// slide's own pixel size demands otherwise.
///
/// It is still a cap on the region, not a refusal: [`region_for`] shrinks the
/// view's rectangle about its centre and the caller says so. Detecting the
/// middle of what you asked for beats detecting nothing, and a slide larger
/// than CMU-1 still lands there rather than running unbounded.
///
/// It used to be true that this did not fire at all, because the run read the
/// level the viewer drew and the pixels behind a window stayed near the
/// window's own size at any scale —
/// `the_level_the_viewer_shows_keeps_a_run_under_the_cap_at_any_zoom` still
/// asserts that of `region_for` alone. [`MAX_MICRONS_PER_PIXEL`] changed what is
/// asked for: a run started while zoomed out now reads a finer level than the
/// view. With the cap where it is, a fitted view of CMU-1 no longer trips it —
/// which is the point, since that is exactly the gesture that means "detect
/// this slide".
const MAX_PIXELS: u64 = 2048 << 20;

/// The coarsest pixel size a run may read, in µm per pixel.
///
/// Detection parameters are in microns and the pipeline converts them with the
/// level's own pixel size, so a run at any level is *arithmetically* consistent.
/// What that conversion cannot do is put back evidence the downsampling threw
/// away: `Micron::min_area`'s 10 µm² is a nucleus about 3.6 µm across, so at
/// level 2 of CMU-1 — 2.0 µm/px — the smallest object worth finding is under
/// four pixels of area, and at level 3 it is a quarter of one pixel.
///
/// Below that scale the detector is not finding nuclei badly, it is finding
/// something else: clumps of stained tissue whose size and contrast at that
/// downsample match what a nucleus looks like at level 0. Those arrive in the
/// status bar as a plausible object count, which is the failure this module is
/// otherwise careful about — a result that looks like a result.
///
/// 1.0 µm/px keeps the smallest surviving object at ten pixels of area, which
/// is enough for a shape rather than a blob. On CMU-1 — whose levels are 0.499,
/// 2.0 and 8.0 µm/px — only level 0 clears it, so a zoomed-out run there
/// reads level 0 over the view's rectangle. That is what the counts in this
/// module's header show going wrong: a fitted whole-slide view at level 2 reported 1638
/// objects where one screenful of tissue at level 0 reported 1357 — the whole
/// slide holds far more nuclei than one screenful, and the coarse run was not
/// finding them.
const MAX_MICRONS_PER_PIXEL: f64 = 1.0;

/// The level a run should read for `view`, which is not always the level the
/// viewer is drawing.
///
/// The viewer picks a level to *look* at, matched to the zoom so that pixels on
/// screen stay near pixels in the file. Detection picks a level to *measure* at,
/// and those are different questions the moment the view is zoomed out: see
/// [`MAX_MICRONS_PER_PIXEL`] for what a coarse level does to the answer.
///
/// This returns the coarsest level at or under that limit — the cheapest level
/// that can still resolve a nucleus — rather than always level 0, so a run at a
/// moderate zoom stays as cheap as it honestly can be.
///
/// `mpp` is the slide's own level-0 pixel size. A slide that records none gets
/// the viewer's level unchanged: without a pixel size there is no way to know
/// what any level resolves, and refusing to run would be worse than the
/// existing behaviour. `Params::default`'s assumed pixel size is not reused
/// here, because guessing the physical scale is exactly what produces the
/// confident wrong answer this is meant to prevent.
fn level_for_detection<'a>(levels: &'a [Level], view: &'a Level, mpp: Option<f64>) -> &'a Level {
    let Some(mpp) = mpp else {
        return view;
    };
    // Levels are ordered finest first, so this walks from the cheap end and
    // takes the coarsest that still resolves a nucleus.
    let resolving = levels
        .iter()
        .rfind(|level| level.downsample * mpp <= MAX_MICRONS_PER_PIXEL)
        .unwrap_or(&levels[0]);
    // Never *finer* than the viewer is showing. Zoomed in past 1:1 the viewer
    // reads level 0 and this would agree; the `min` matters when the viewer is
    // already at a level finer than the cap requires, where forcing it coarser
    // would read less detail than the person is looking at.
    if view.downsample <= resolving.downsample {
        view
    } else {
        resolving
    }
}

/// A detection run in progress, or the one that last finished.
pub struct Detector {
    /// Results from the thread. `None` when nothing has ever been started.
    incoming: Option<Receiver<Message>>,
    /// Bumped by every [`Detector::start`]. A result carrying anything else
    /// answers a superseded run and is dropped.
    generation: u64,
    /// When the run in flight began, and `None` when none is.
    started: Option<Instant>,
    /// What the last finished run found, in slide coordinates.
    pub overlay: Option<Overlay>,
    /// A sentence about the last run, for the status bar.
    pub message: Option<String>,
    /// How far the run in flight has got, and the way to stop it.
    ///
    /// One handle for the life of the detector rather than one per run: the
    /// worker holds a clone of the `Arc` and writes to it, this side reads it
    /// once a frame and can `cancel` it from the UI thread. Reset by
    /// [`Detector::start`] *before* the worker is spawned, which is the order
    /// `qupath_pipeline::Progress::reset` documents and the reason it is the
    /// caller's job.
    progress: Arc<Progress>,
}

/// What a finished run sends back.
struct Message {
    generation: u64,
    result: Outcome,
}

/// How a run ended.
///
/// Cancellation is a third case rather than an error, because the status bar
/// must not call it a failure: it is what the person asked for. Keeping it
/// typed here rather than matching on an error string is the difference between
/// a distinction the compiler holds and one a message format could break.
enum Outcome {
    /// The run finished and found these.
    Found(Overlay, Summary),
    /// The run was stopped, having done this many of this many tiles.
    Cancelled { done: u64, total: u64 },
    /// The run failed.
    Failed(String),
}

/// The numbers a finished run reports, kept apart from the geometry because
/// the status bar wants these and the compositor wants that.
struct Summary {
    objects: usize,
    /// Pixels actually read, in the pixels of the level that was read — which
    /// is what the run's cost is proportional to, and is *not* a level-0
    /// figure unless the run was at level 0.
    pixels: u64,
    /// The pyramid level read, which the status bar reports because it is what
    /// the outlines' resolution depends on. See the module documentation.
    level: usize,
    took: Duration,
    /// Whether the region was shrunk to fit [`MAX_PIXELS`].
    capped: bool,
    /// Whether the run read a finer level than the viewer is drawing, because
    /// the drawn one is too coarse to resolve a nucleus. The status bar says
    /// so: it explains a run that took longer than the zoom suggests, and it
    /// is why the count can differ from what an earlier coarse run reported.
    deepened: bool,
}

impl Default for Detector {
    fn default() -> Self {
        Self::new()
    }
}

impl Detector {
    /// A detector that has never run.
    #[must_use]
    pub fn new() -> Self {
        Self {
            incoming: None,
            generation: 0,
            started: None,
            overlay: None,
            message: None,
            progress: Arc::new(Progress::new()),
        }
    }

    /// Whether a run is in flight.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.started.is_some()
    }

    /// How long the run in flight has been going, for the status bar.
    #[must_use]
    pub fn elapsed(&self) -> Option<Duration> {
        self.started.map(|started| started.elapsed())
    }

    /// How far the run in flight has got, in `0.0..=1.0`.
    ///
    /// `None` when nothing is running, and also for the moment between the
    /// worker being spawned and the pipeline building its tile grid — the
    /// status bar shows the elapsed time it already showed, rather than a zero
    /// that would read as a stalled run.
    ///
    /// Tiles finished over tiles total, which is not time: tiles over glass are
    /// far cheaper than tiles over tissue, so this advances in jumps. That is
    /// the honest number and the reason no finish time is offered with it.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        self.is_running()
            .then(|| self.progress.fraction())
            .flatten()
    }

    /// Asks the run in flight to stop.
    ///
    /// Takes effect within about one tile rather than instantly — measured at
    /// **1.45 s** on a whole-slide level-0 run, which is one tile's work on one
    /// thread. So [`Detector::is_running`] stays true for a second or two
    /// afterwards and the result arrives through [`Detector::poll`] like any
    /// other. Harmless when nothing is running.
    ///
    /// This is a *stop*, not a supersede: [`Detector::start`] already replaces a
    /// run, and the difference is that this one leaves no successor. A person
    /// who started a whole-slide run by mistake wants the core back, not another
    /// run.
    pub fn cancel(&self) {
        self.progress.cancel();
    }

    /// Throws away the overlay and anything in flight.
    ///
    /// Called when the slide changes: outlines are in *that* slide's
    /// coordinates, and drawing them over a different one puts plausible
    /// nuclei in arbitrary places — the failure this whole module is careful
    /// about, because it looks like a result.
    pub fn clear(&mut self) {
        // The thread is left to finish and its result is dropped on arrival,
        // which `generation` already handles. Detaching the receiver would work
        // too; keeping it means a superseded run's error still reaches the
        // `TryRecvError::Disconnected` arm rather than vanishing.
        self.generation += 1;
        self.started = None;
        self.overlay = None;
        self.message = None;
    }

    /// Starts a run over what `view` is showing of `slide`, replacing any run
    /// already going.
    ///
    /// The slide is re-opened on the worker thread rather than shared. `Slide`
    /// is `Sync` and could have been, but the run wants its own reader for the
    /// same reason the loader's workers do — a detection reading through the
    /// same handle the viewer pans with would put the two in each other's way
    /// for the whole run, which is precisely the interval this exists to keep
    /// smooth.
    pub fn start(
        &mut self,
        path: &Path,
        levels: &[Level],
        view: &Level,
        mpp: Option<f64>,
        viewport: Viewport,
        tuning: Micron,
    ) {
        // Not `view`: the level to measure at is not the level to look at. See
        // [`level_for_detection`].
        let level = level_for_detection(levels, view, mpp);
        let deepened = level.index != view.index;
        let Some((region, capped)) = region_for(viewport, level) else {
            self.message = Some(String::from(
                "Nothing to detect: the view is off the slide.",
            ));
            return;
        };

        self.generation += 1;
        let generation = self.generation;
        // Before the worker exists, so a `cancel` from this thread can never be
        // wiped by the run it was meant to stop. `Progress::reset` documents the
        // race this order avoids.
        self.progress.reset();
        let progress = Arc::clone(&self.progress);
        let (sender, receiver) = channel();
        self.incoming = Some(receiver);
        self.started = Some(Instant::now());
        self.message = None;

        let path = path.to_path_buf();
        let index = level.index;
        // Detached. Nothing joins it: the channel is how the result arrives,
        // and a run whose receiver has been dropped sends into a closed channel
        // and exits, which is the behaviour that lets the window close during a
        // run instead of waiting ninety seconds for one.
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = match detect(&path, index, region, tuning, &progress) {
                Ok((overlay, objects)) => Outcome::Found(
                    overlay,
                    Summary {
                        objects,
                        pixels: u64::from(region.width) * u64::from(region.height),
                        level: index,
                        took: started.elapsed(),
                        capped,
                        deepened,
                    },
                ),
                // Matched on the variant rather than on the message, so a
                // reworded error cannot turn a cancellation into a failure.
                Err(pipeline::PipelineError::Cancelled { done, total }) => {
                    Outcome::Cancelled { done, total }
                }
                Err(error) => Outcome::Failed(error.to_string()),
            };
            // Ignored deliberately: a closed channel means the viewer moved on
            // or shut down, and there is nobody left to tell.
            let _ = sender.send(Message { generation, result });
        });
    }

    /// Takes delivery of a finished run, and says whether anything changed.
    ///
    /// Call once a frame. Non-blocking — a run still going returns `false` and
    /// costs a `try_recv`.
    pub fn poll(&mut self) -> bool {
        let Some(receiver) = &self.incoming else {
            return false;
        };
        let message = match receiver.try_recv() {
            Ok(message) => message,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => {
                self.incoming = None;
                // A thread that died without sending. Nothing here panics, so
                // this is a panic in the pipeline, and saying so beats a
                // spinner that never stops.
                if self.started.take().is_some() {
                    self.message = Some(String::from("Detection stopped unexpectedly."));
                    return true;
                }
                return false;
            }
        };

        if message.generation != self.generation {
            // A superseded run that finished anyway. Its work is dropped, not
            // drawn: it answers a view that is no longer the one on screen.
            return false;
        }
        self.started = None;

        match message.result {
            Outcome::Found(overlay, summary) => {
                self.message = Some(describe(&summary));
                self.overlay = Some(overlay);
            }
            Outcome::Cancelled { done, total } => {
                // Said plainly and without the word "failed": stopping was the
                // instruction. The tile counts are in it because "stopped at 41
                // of 5776" tells the person how much of the slide the overlay
                // they are still looking at does *not* cover.
                self.message = Some(format!("Detection stopped after {done} of {total} tiles."));
                // The previous overlay is kept, for the same reason a failure
                // keeps it: a stopped run is not a reason to throw away results
                // the person is still looking at.
            }
            Outcome::Failed(error) => {
                self.message = Some(format!("Detection failed: {error}"));
                // The previous overlay is kept. A failed run is not a reason to
                // throw away results the user is still looking at.
            }
        }
        true
    }
}

/// A finished run as a sentence.
///
/// The level is in it because the outlines' resolution follows it, and a run at
/// level 2 producing blocky nuclei is a fact about what was asked for rather
/// than a defect to be puzzled over.
fn describe(summary: &Summary) -> String {
    let megapixels = summary.pixels as f64 / f64::from(1u32 << 20);
    let capped = if summary.capped {
        ", centre of the view only"
    } else {
        ""
    };
    // Said in words rather than left to the level number, because "level 0"
    // does not tell the person why a run they started while zoomed out read the
    // finest pixels in the file and took longer than the last one.
    let deepened = if summary.deepened {
        " (finest level that resolves a nucleus)"
    } else {
        ""
    };
    format!(
        "{} objects at level {}{deepened} in {:.1} s over {:.1} Mpx{capped}",
        summary.objects,
        summary.level,
        summary.took.as_secs_f64(),
        megapixels,
    )
}

/// Runs the pipeline and converts what it found into slide coordinates.
///
/// The whole of the detection side, in one function, so what runs on the worker
/// thread is visible in one place.
fn detect(
    path: &PathBuf,
    level: usize,
    region: Region,
    tuning: Micron,
    progress: &Progress,
) -> Result<(Overlay, usize), pipeline::PipelineError> {
    let slide = Slide::open(path)?;
    // The slide's own pixel size, not an assumed one. `Params::default`
    // hardcodes CMU-1's 0.4990 µm/px, so a slide scanned at any other size got
    // every micron-derived filter at the wrong scale -- see `VIEWER.md` §8.0.
    // A slide that records no `MPP` keeps the old behaviour rather than
    // refusing to run, and the panel says which of the two is happening.
    let params = Params {
        level,
        ..slide
            .microns_per_pixel()
            .map_or_else(Params::default, |mpp| tuning.at(mpp))
    };
    let (outcome, detections) = pipeline::run_collecting_watched(&slide, region, params, progress)?;

    // Read off the level table rather than assumed. A run at any level but 0
    // has a downsample that is not 1, and an overlay built as though it were
    // draws every nucleus at a fraction of its true position — which looks like
    // a plausible detection of something else.
    let downsample = slide.levels()[detections.level()].downsample;
    let mut overlay = Overlay::new();
    for object in 0..detections.len() {
        overlay.push_level_ring(detections.outline(object), downsample);
    }
    overlay.shrink_to_fit();

    debug_assert_eq!(
        overlay.len() as u64,
        outcome.objects,
        "every detected object should have an outline"
    );
    let objects = overlay.len();
    Ok((overlay, objects))
}

/// The region a run should read for `view`, in `level`'s pixels, and whether it
/// was capped.
///
/// Returns `None` when the view sees no part of the level at all, which is what
/// panning off the edge of a slide does.
///
/// Three things happen here, and the order matters:
///
/// 1. the view's bounds are converted from slide coordinates into `level`'s;
/// 2. the rectangle is **clipped** to the level, because a fitted view is wider
///    than the slide and `run_collecting` refuses a region that leaves it;
/// 3. what is left is **capped** to [`MAX_PIXELS`] about its own centre.
///
/// Clipping before capping is what makes the cap mean what it says. Capping the
/// unclipped rectangle would count the empty margins of a fitted view against
/// the budget and shrink the region for pixels that were never going to be
/// read.
#[must_use]
pub fn region_for(view: Viewport, level: &Level) -> Option<(Region, bool)> {
    if level.width == 0 || level.height == 0 {
        return None;
    }
    let (left, top, right, bottom) = view.bounds();
    let downsample = level.downsample;

    // Into level pixels, and outwards to whole ones: a detection asked for what
    // is on screen should not stop a third of a pixel short of the edge of it.
    let to_level = |slide: f64| slide / downsample;
    let left = to_level(left).floor().max(0.0);
    let top = to_level(top).floor().max(0.0);
    let right = to_level(right).ceil().min(f64::from(level.width));
    let bottom = to_level(bottom).ceil().min(f64::from(level.height));
    if right <= left || bottom <= top {
        return None;
    }

    // Every value is now inside `0..=level.width` by the clamps above, and a
    // level is `u32` by definition, so none of these can truncate.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to the level's own u32 extent immediately above"
    )]
    let (mut x, mut y, mut width, mut height) = (
        left as u32,
        top as u32,
        (right - left) as u32,
        (bottom - top) as u32,
    );

    // The cap, applied about the centre so that what is detected is what the
    // user was looking at rather than its top-left corner. Both axes shrink by
    // the same factor, so the region keeps the view's shape.
    let pixels = u64::from(width) * u64::from(height);
    let capped = pixels > MAX_PIXELS;
    if capped {
        let factor = (MAX_PIXELS as f64 / pixels as f64).sqrt();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "factor is in (0, 1), so the product is below a width that already fits"
        )]
        let (capped_width, capped_height) = (
            (f64::from(width) * factor) as u32,
            (f64::from(height) * factor) as u32,
        );
        // At least one pixel each way. A region of zero width is an error from
        // `run_collecting`, and `MAX_PIXELS` is far too large for this to
        // trigger on anything but a degenerate view.
        let (capped_width, capped_height) = (capped_width.max(1), capped_height.max(1));
        x += (width - capped_width) / 2;
        y += (height - capped_height) / 2;
        width = capped_width;
        height = capped_height;
    }

    Some((
        Region {
            x,
            y,
            width,
            height,
        },
        capped,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qupath_viewer::SlidePoint;

    /// A level of `width` by `height` at `downsample`, with the tile geometry
    /// filled in plausibly — `region_for` reads neither.
    fn level(index: usize, width: u32, height: u32, downsample: f64) -> Level {
        Level {
            index,
            width,
            height,
            tile_width: 256,
            tile_height: 256,
            columns: width.div_ceil(256),
            rows: height.div_ceil(256),
            downsample,
        }
    }

    fn view(centre: (f64, f64), scale: f64, size: (u32, u32)) -> Viewport {
        Viewport {
            centre: SlidePoint::new(centre.0, centre.1),
            scale,
            width: size.0,
            height: size.1,
        }
    }

    #[test]
    fn a_view_inside_the_slide_becomes_the_rectangle_it_shows() {
        // 1:1 at level 0: a 100x50 window centred at (500, 400) sees slide
        // 450..550 by 375..425, and that is the region exactly.
        let level = level(0, 4000, 3000, 1.0);
        let (region, capped) =
            region_for(view((500.0, 400.0), 1.0, (100, 50)), &level).expect("on the slide");

        assert_eq!(
            region,
            Region {
                x: 450,
                y: 375,
                width: 100,
                height: 50
            }
        );
        assert!(!capped);
    }

    #[test]
    fn a_region_is_expressed_in_the_levels_own_pixels() {
        // The conversion that puts every object in the wrong place if it is
        // skipped. The same view against a level with downsample 4 must give a
        // rectangle a quarter the size, at a quarter the offset — and the level
        // index is irrelevant to the arithmetic, which is why `downsample` is
        // what is read.
        let coarse = level(2, 1000, 750, 4.0);
        let (region, _) =
            region_for(view((500.0, 400.0), 1.0, (100, 50)), &coarse).expect("on the slide");

        assert_eq!(
            region,
            Region {
                x: 112,
                y: 93,
                width: 26,
                height: 14
            }
        );
    }

    #[test]
    fn a_view_wider_than_the_slide_is_clipped_to_it() {
        // A fitted view has the slide letterboxed inside it, so the bounds run
        // past both edges. `run_collecting` rejects a region that leaves the
        // level, so this is the difference between a working button and an
        // error message on every fitted slide.
        let level = level(0, 400, 300, 1.0);
        let (region, capped) =
            region_for(view((200.0, 150.0), 0.1, (400, 300)), &level).expect("overlaps the slide");

        assert_eq!(
            region,
            Region {
                x: 0,
                y: 0,
                width: 400,
                height: 300
            },
            "the whole level and not a pixel more"
        );
        assert!(!capped);
    }

    #[test]
    fn a_view_off_the_slide_has_no_region() {
        // Panned away entirely. `None` rather than an empty region, because an
        // empty region is an error out of the pipeline and this is an ordinary
        // thing to do with a mouse.
        let level = level(0, 400, 300, 1.0);
        assert!(region_for(view((5000.0, 5000.0), 1.0, (100, 100)), &level).is_none());
        // And a view whose right edge exactly touches the origin sees no pixel
        // of the level either.
        assert!(region_for(view((-50.0, 150.0), 1.0, (100, 300)), &level).is_none());
    }

    #[test]
    fn a_view_half_off_the_slide_keeps_the_half_that_is_on() {
        let level = level(0, 400, 300, 1.0);
        let (region, _) =
            region_for(view((0.0, 150.0), 1.0, (200, 100)), &level).expect("overlaps the slide");
        assert_eq!(
            region,
            Region {
                x: 0,
                y: 100,
                width: 100,
                height: 100
            }
        );
    }

    /// CMU-1's actual pyramid, read off the file rather than assumed: three
    /// levels at 0.499, 1.996 and 7.984 µm/px.
    fn cmu1() -> Vec<Level> {
        vec![
            level(0, 46000, 32914, 1.0),
            level(1, 11500, 8228, 4.0),
            level(2, 2875, 2057, 16.0),
        ]
    }

    const CMU1_MPP: f64 = 0.499;

    #[test]
    fn a_zoomed_out_view_is_detected_at_a_level_that_can_resolve_a_nucleus() {
        // The bug this cap exists for. A fitted view of the whole slide draws
        // level 2 — 8 µm per pixel, where a 10 µm² nucleus is a sixth of one
        // pixel — and detecting there finds clumps of tissue and reports
        // them as an object count. The run is moved to a level that can
        // actually see a nucleus.
        let levels = cmu1();
        let chosen = level_for_detection(&levels, &levels[2], Some(CMU1_MPP));
        assert!(
            chosen.downsample * CMU1_MPP <= MAX_MICRONS_PER_PIXEL,
            "a run at {} µm/px cannot resolve a nucleus",
            chosen.downsample * CMU1_MPP,
        );
        // Level 0 alone: CMU-1's level 1 is downsample 4, which is 2.0 µm/px
        // and already past the limit. This pyramid has no intermediate level to
        // fall back to, so resolving a nucleus here means the finest pixels in
        // the file.
        assert_eq!(chosen.index, 0);
    }

    #[test]
    fn a_view_already_fine_enough_is_detected_where_it_is_drawn() {
        // The cap only ever deepens a run. A viewer at 1:1 draws level 0 and a
        // detection there reads level 0: nothing to correct, and no reason to
        // make the cheap case more expensive than it is.
        let levels = cmu1();
        let chosen = level_for_detection(&levels, &levels[0], Some(CMU1_MPP));
        assert_eq!(chosen.index, 0);
    }

    #[test]
    fn the_cap_never_reads_coarser_than_the_viewer_is_showing() {
        // Whatever the levels say, a run must not read less detail than the
        // person is looking at — that would report fewer objects than the
        // screen plainly shows.
        let levels = cmu1();
        for view in &levels {
            let chosen = level_for_detection(&levels, view, Some(CMU1_MPP));
            assert!(
                chosen.downsample <= view.downsample,
                "level {} is coarser than the drawn level {}",
                chosen.index,
                view.index,
            );
        }
    }

    #[test]
    fn a_slide_with_no_pixel_size_keeps_the_level_the_viewer_shows() {
        // Without an `MPP` there is no way to know what any level resolves.
        // Guessing one is what `Params::default` does and what produces a
        // confident wrong answer, so the old behaviour is kept instead.
        let levels = cmu1();
        let chosen = level_for_detection(&levels, &levels[2], None);
        assert_eq!(chosen.index, 2);
    }

    #[test]
    fn a_coarse_slide_still_runs_rather_than_refusing() {
        // Every level of this pyramid is past the limit. Something must run,
        // and the finest level is the best available answer -- a button that
        // silently does nothing would be worse than a coarse result the status
        // bar describes.
        let levels = vec![level(0, 4000, 4000, 1.0), level(1, 1000, 1000, 4.0)];
        let chosen = level_for_detection(&levels, &levels[1], Some(8.0));
        assert_eq!(chosen.index, 0);
    }

    #[test]
    fn the_level_the_viewer_shows_keeps_a_run_under_the_cap_at_any_zoom() {
        // Why the cap almost never fires, stated as an assertion rather than
        // left as a hope. The viewer reads the level matched to the zoom, so
        // the *level* pixels behind a window stay near the window's own size
        // however far out it is zoomed — which is what makes "detect what is on
        // screen" a bounded amount of work at every scale rather than only when
        // zoomed in.
        //
        // CMU-1's pyramid, and a view fitted to it then zoomed in by powers of
        // four with the level following the scale.
        let window = (1280u32, 860u32);
        for step in 0..4u32 {
            let factor = 4u32.pow(step);
            let downsample = f64::from(factor);
            let level = level(step as usize, 46_000 / factor, 32_000 / factor, downsample);
            // Scale at which this level is the right one: one screen pixel per
            // level pixel.
            let scale = 1.0 / downsample;
            let fitted = view((23_000.0, 16_000.0), scale, window);
            let (region, capped) = region_for(fitted, &level).expect("on the slide");

            assert!(!capped, "capped at level {}", level.index);
            let pixels = u64::from(region.width) * u64::from(region.height);
            assert!(
                pixels <= u64::from(window.0 + 2) * u64::from(window.1 + 2),
                "level {} reads {pixels} px for a {}x{} window",
                level.index,
                window.0,
                window.1
            );
        }
    }

    /// A fitted view of CMU-1 at level 0 reads the whole slide, uncapped.
    ///
    /// This is the gesture the cap used to spoil: "fit, then detect" means
    /// "detect this slide", and under the old 64 Mpx cap it returned the middle
    /// 21% of the slide's width labelled as a partial answer. The numbers are
    /// CMU-1's real level-0 extent, so this fails if the cap is ever lowered
    /// back under a whole slide without that being a decision somebody made.
    /// A detector that has never run reports no progress and survives a stop.
    ///
    /// The reachable bad state here is a `fraction` of `Some(0.0)` before
    /// anything starts, which a status bar would draw as a bar sitting at zero
    /// — indistinguishable from a run that has stalled on its first tile. It
    /// must be `None`, so the interface can show nothing at all.
    #[test]
    fn an_idle_detector_reports_no_fraction() {
        let detector = Detector::new();
        assert!(!detector.is_running());
        assert_eq!(detector.fraction(), None);

        // Stopping nothing is not an error, because the `Stop` button and the
        // escape key can both arrive after a run has already finished — the
        // result is in flight and `is_running` is still true when the key goes
        // down. A guard that panicked or a flag left set would break the *next*
        // run, which is the failure this asserts against.
        detector.cancel();
        detector.cancel();
        assert!(!detector.is_running());
        assert_eq!(detector.fraction(), None);
    }

    /// `fraction` is gated on a run being in flight, not on the handle alone.
    ///
    /// A finished run leaves its handle reading 1.0 — that is what
    /// `a_finished_run_leaves_the_handle_complete` asserts in the pipeline's own
    /// suite. If `fraction` read the handle unconditionally, the status bar
    /// would show a full progress bar forever after the first run, next to a
    /// control that is no longer running. The `is_running` gate is what makes it
    /// disappear, and this pins that gate.
    #[test]
    fn a_detector_that_is_not_running_hides_a_completed_fraction() {
        let detector = Detector::new();
        // Standing in for a finished run: the handle carries a full fraction,
        // which is exactly the state a completed run leaves behind.
        detector.progress.reset();
        detector.progress.cancel();
        assert!(!detector.is_running());
        assert_eq!(
            detector.fraction(),
            None,
            "a completed run's fraction must not outlive the run"
        );
    }

    #[test]
    fn a_fitted_view_of_cmu1_reads_the_whole_slide() {
        let level = level(0, 46_000, 32_914, 1.0);
        // Fitted: the zoom that fits *both* axes, which is the smaller of the
        // two ratios and is height-limited for this slide in this window. The
        // view is then wider than the slide, and step 2 clips it back.
        let zoom = (1280.0 / 46_000.0f64).min(860.0 / 32_914.0);
        let fitted = view((23_000.0, 16_457.0), zoom, (1280, 860));
        let (region, capped) = region_for(fitted, &level).expect("on the slide");

        assert!(!capped, "a fitted CMU-1 must not be capped");
        assert_eq!(region.width, 46_000);
        assert_eq!(region.height, 32_914);
        // 1514 Mpx, which is what the 33.5 s measurement in `MAX_PIXELS` is of.
        let pixels = u64::from(region.width) * u64::from(region.height);
        assert!(pixels <= MAX_PIXELS, "{pixels} px is over the cap");
    }

    #[test]
    fn an_enormous_region_is_capped_about_its_centre() {
        // The cap's own case: level 0 held fixed while the view is zoomed out,
        // which is not what the viewer does by itself but is what an explicit
        // "detect at full resolution" would ask for. This level is deliberately
        // *larger than CMU-1* — 4.4 Gpx against its 1.5 — because the cap no
        // longer fires on CMU-1 at all, which is the point of where it now sits.
        // What is asserted is the three things the cap must get right when it
        // does fire: it fits, it is centred on what the uncapped region was
        // centred on, and it keeps the view's shape.
        let level = level(0, 80_000, 55_000, 1.0);
        let whole = view((40_000.0, 27_500.0), 0.02, (1280, 860));
        let (region, capped) = region_for(whole, &level).expect("on the slide");

        assert!(capped);
        let pixels = u64::from(region.width) * u64::from(region.height);
        assert!(pixels <= MAX_PIXELS, "{pixels} px is over the cap");
        // Within a pixel of the cap, so the cap is a cap and not a needless
        // shrink: a version that halved the region again would pass the
        // assertion above and fail this one.
        assert!(
            pixels * 100 > MAX_PIXELS * 99,
            "{pixels} px wastes most of the budget"
        );

        let (centre_x, centre_y) = (
            f64::from(region.x) + f64::from(region.width) / 2.0,
            f64::from(region.y) + f64::from(region.height) / 2.0,
        );
        assert!(
            (centre_x - 40_000.0).abs() <= 1.0,
            "off centre at {centre_x}"
        );
        assert!(
            (centre_y - 27_500.0).abs() <= 1.0,
            "off centre at {centre_y}"
        );

        // The view at this zoom is wider than the level, so step 2 clips it to
        // the level and the capped region keeps *that* shape rather than the
        // window's. Asserted against the level for exactly that reason.
        let aspect = f64::from(region.width) / f64::from(region.height);
        let wanted = f64::from(level.width) / f64::from(level.height);
        assert!(
            (aspect - wanted).abs() < 0.05,
            "shape changed: {aspect} against {wanted}"
        );
    }

    #[test]
    fn a_region_at_exactly_the_cap_is_not_capped() {
        // The boundary, from both sides. A cap that fires one pixel early is
        // invisible; a cap that fires one pixel late is a region the pipeline
        // spends longer on than the budget allows.
        // The cap is 2^31, which is not a perfect square, so a side derived by
        // truncating its root would be *under* it and would assert nothing
        // about the boundary. A power-of-two rectangle hits it exactly: this is
        // the pair of sides whose product is MAX_PIXELS.
        let (wide, high) = (1u32 << 16, 1u32 << 15);
        assert_eq!(u64::from(wide) * u64::from(high), MAX_PIXELS);
        let level = level(0, wide * 2, high * 2, 1.0);
        let (centre_x, centre_y) = (f64::from(wide), f64::from(high));

        let exact = view((centre_x, centre_y), 1.0, (wide, high));
        let (region, capped) = region_for(exact, &level).expect("on the slide");
        assert_eq!(
            u64::from(region.width) * u64::from(region.height),
            MAX_PIXELS
        );
        assert!(!capped);

        let over = view((centre_x, centre_y), 1.0, (wide + 2, high + 2));
        let (region, capped) = region_for(over, &level).expect("on the slide");
        assert!(capped);
        assert!(u64::from(region.width) * u64::from(region.height) <= MAX_PIXELS);
    }

    #[test]
    fn a_fresh_detector_has_nothing_and_polls_false() {
        let mut detector = Detector::new();
        assert!(!detector.is_running());
        assert!(detector.overlay.is_none());
        assert!(detector.elapsed().is_none());
        // Polling before anything has been started must be a cheap no-op rather
        // than a panic on an absent receiver.
        assert!(!detector.poll());
    }

    #[test]
    fn clearing_discards_the_overlay_and_supersedes_what_is_in_flight() {
        // The slide-change path. What is asserted is that the generation moved,
        // because that is what makes a run started against the *old* slide land
        // in `poll`'s superseded arm instead of being drawn over the new one.
        let mut detector = Detector::new();
        detector.overlay = Some(Overlay::new());
        detector.message = Some(String::from("something"));
        let before = detector.generation;

        detector.clear();
        assert!(detector.overlay.is_none());
        assert!(detector.message.is_none());
        assert!(!detector.is_running());
        assert!(detector.generation > before);
    }
}
