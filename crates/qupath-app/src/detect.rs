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
//! The one the viewer is showing, not level 0. That is what makes "detect what
//! is on screen" bounded work at every zoom — a fitted view reads level 2,
//! where the whole slide is 5.6 Mpx rather than the 1.5 Gpx it is at level 0.
//! Detecting at a fixed level 0 would make the button's cost depend on the zoom
//! by a factor of a thousand, which is the difference between a control that
//! responds and one that appears to hang.
//!
//! What this trades away is that objects found at a coarse level are coarse:
//! their outlines are traced in level pixels and scaled up, so a nucleus
//! detected at level 2 is a sixteen-pixel-blocky version of itself. That is
//! visible when zooming back in, and it is the honest thing to show — the
//! alternative is pretending a coarse run resolved detail it never read.
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
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use qupath_pipeline::{self as pipeline, Params, Region};
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
/// Measured on CMU-1 rather than extrapolated: a fitted view reads level 2 and
/// runs 5.64 Mpx in **1.24 s**, and a 1:1 view of tissue runs 1.05 Mpx in
/// 0.38 s. That is around 4.5 Mpx/s, *below* the 16.5 Mpx/s of
/// `docs/benchmarks/rust-pipeline-region.txt`, because that figure is for a
/// dense level-0 tissue region and these views are mostly glass — the pipeline
/// is not linear in area. At the measured rate this cap is about **fifteen
/// seconds**, which is the ceiling on a run that has gone wrong rather than a
/// duration anything normal reaches.
///
/// It is a cap on the region, not a refusal: [`region_for`] shrinks the view's
/// rectangle about its centre and the caller says so. Detecting the middle of
/// what you asked for beats detecting nothing.
///
/// In practice it does not fire. The viewer reads the level that matches the
/// zoom, so the pixels behind a window stay near the window's own size at any
/// scale — `the_level_the_viewer_shows_keeps_a_run_under_the_cap_at_any_zoom`
/// asserts exactly that. What is left for the cap to catch is a run at a level
/// far finer than the view, which nothing in the interface currently asks for
/// and which would otherwise be a viewer that stops responding.
const MAX_PIXELS: u64 = 64 << 20;

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
}

/// What a finished run sends back.
struct Message {
    generation: u64,
    result: Result<(Overlay, Summary), String>,
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
    pub fn start(&mut self, path: &Path, level: &Level, view: Viewport) {
        let Some((region, capped)) = region_for(view, level) else {
            self.message = Some(String::from(
                "Nothing to detect: the view is off the slide.",
            ));
            return;
        };

        self.generation += 1;
        let generation = self.generation;
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
            let result = detect(&path, index, region).map(|(overlay, objects)| {
                (
                    overlay,
                    Summary {
                        objects,
                        pixels: u64::from(region.width) * u64::from(region.height),
                        level: index,
                        took: started.elapsed(),
                        capped,
                    },
                )
            });
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
            Ok((overlay, summary)) => {
                self.message = Some(describe(&summary));
                self.overlay = Some(overlay);
            }
            Err(error) => {
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
    format!(
        "{} objects at level {} in {:.1} s over {:.1} Mpx{capped}",
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
fn detect(path: &PathBuf, level: usize, region: Region) -> Result<(Overlay, usize), String> {
    let slide = Slide::open(path).map_err(|error| error.to_string())?;
    let params = Params {
        level,
        ..Params::default()
    };
    let (outcome, detections) =
        pipeline::run_collecting(&slide, region, params).map_err(|error| error.to_string())?;

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

    #[test]
    fn an_enormous_region_is_capped_about_its_centre() {
        // The cap's own case: level 0 held fixed while the view is zoomed out,
        // which is not what the viewer does by itself but is what an explicit
        // "detect at full resolution" would ask for. Over a gigapixel, which is
        // ninety seconds of pipeline. What is asserted is the three things the
        // cap must get right — it fits, it is centred on what the uncapped
        // region was centred on, and it keeps the view's shape.
        let level = level(0, 46_000, 32_000, 1.0);
        let whole = view((23_000.0, 16_000.0), 0.02, (1280, 860));
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
            (centre_x - 23_000.0).abs() <= 1.0,
            "off centre at {centre_x}"
        );
        assert!(
            (centre_y - 16_000.0).abs() <= 1.0,
            "off centre at {centre_y}"
        );

        let aspect = f64::from(region.width) / f64::from(region.height);
        let wanted = f64::from(level.width) / f64::from(level.height);
        assert!((aspect - wanted).abs() < 0.01, "shape changed: {aspect}");
    }

    #[test]
    fn a_region_at_exactly_the_cap_is_not_capped() {
        // The boundary, from both sides. A cap that fires one pixel early is
        // invisible; a cap that fires one pixel late is a region the pipeline
        // spends longer on than the budget allows.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the square root of a 64 Mpx cap is 8192, which is not near u32's range"
        )]
        let side = (MAX_PIXELS as f64).sqrt() as u32;
        let level = level(0, side * 2, side * 2, 1.0);
        let centre = f64::from(side);

        let exact = view((centre, centre), 1.0, (side, side));
        let (region, capped) = region_for(exact, &level).expect("on the slide");
        assert_eq!(
            u64::from(region.width) * u64::from(region.height),
            MAX_PIXELS
        );
        assert!(!capped);

        let over = view((centre, centre), 1.0, (side + 2, side + 2));
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
