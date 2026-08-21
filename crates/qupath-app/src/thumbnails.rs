//! Small pictures of whole slides: the overview's, and the browser's rows.
//!
//! `docs/VIEWER.md` §8.2 needed one picture — the slide that is open, under the
//! metadata pane — and it is read once when the slide opens. §8.3's project
//! browser needs one per row, which is a different problem: fifty of them, of
//! slides nothing has opened, wanted while the window stays live.
//!
//! # Why this is not blocked on a JPEG encoder
//!
//! §8.8 listed the thumbnail under "the same missing JPEG encoder as ever", on
//! the reasoning that a row's picture is `data/<id>/thumbnail.jpg` and that
//! nothing here writes or reads one. **QuPath's own browser does not require
//! that file either.** `ProjectBrowser.ProjectTreeRowCell.updateItem` asks
//! `entry.getThumbnail()`, and when the answer is null it submits a task that
//! *opens the image* and calls `ProjectCommands.getThumbnailRGB(server)` — the
//! stored file is a cache of a picture the browser is prepared to make from the
//! pixels. So is this. The encoder is still what a project written here owes
//! *QuPath*, and that is where §8.8 keeps it.
//!
//! # What is copied from QuPath's browser, and what is not
//!
//! Copied: one background thread — `Executors.newSingleThreadExecutor` with a
//! `thumbnail-loader` factory — walking the entries in the order the tree shows
//! them, and a slide that fails to open never being asked for a second time
//! (`serversFailed` there; here the queue is a list walked once, so a retry has
//! nowhere to come from).
//!
//! Not copied: the four sizes and the hidden setting, which are a preference
//! this has nowhere to keep; and the tooltip's larger picture, which wants a
//! second size of the same image.
//!
//! # The picture is the slide's, not the row's
//!
//! [`make`] is what the overview pane already used, unchanged: the coarsest
//! level, sampled down to fit [`qupath_viewer::overview::MAX_LENGTH`]. A row
//! and the overview pane therefore show the *same picture* of the same slide,
//! which is the property that makes clicking a row and looking at the pane
//! agree — and it is why [`Thumbnail`] is one type rather than two.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use qupath_slide::Slide;
use qupath_viewer::overview::Overview;

/// A whole-slide picture and the mapping onto it.
pub struct Thumbnail {
    /// The picture, RGBA, `width` by `height` as [`Overview::size`] gave them.
    pub pixels: Vec<u8>,
    /// Geometry: where the view is, and where a click lands.
    pub overview: Overview,
}

/// Reads a slide's overview picture, or `None` if it has none to give.
///
/// From the **coarsest** level, which is the whole point: the top of CMU-1's
/// pyramid is 1,024 px on its long edge, so the picture costs one small region
/// read rather than a decimation of level 0. Downsampled from there by nearest
/// sampling — the picture is 150 px wide and this runs once per slide, so a
/// better filter would be invisible and unmeasurable.
///
/// QuPath's `AbstractImageServer.getDefaultThumbnail` picks the coarsest level
/// whose downsample is under `sqrt(width / 1024 * height / 1024)` rather than
/// the coarsest level outright. On a pyramid that ends near 1,024 px the two
/// choose the same level; on one that ends much smaller QuPath reads a finer
/// level and gets a sharper picture at the same displayed size. That is a
/// difference in sharpness and not in what the picture is of.
///
/// # Errors
///
/// Returns what the slide reader said, as a sentence to put in front of the
/// user. A slide that opens and will not give up its coarsest level is the
/// ordinary case rather than the strange one — a JPEG 2000 pyramid opens and
/// decodes nothing — and a browser that only knew *that* there was no picture
/// could not tell it from one that had not arrived yet.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a source pixel index, bounded by the level it indexes into"
)]
pub fn make(slide: &Slide) -> Result<Thumbnail, String> {
    let (Some(base), Some(coarsest)) = (slide.levels().first(), slide.levels().last()) else {
        return Err(String::from("the slide has no pyramid levels"));
    };
    let Some(overview) = Overview::fit(base.width, base.height) else {
        return Err(format!("the slide is {} by {}", base.width, base.height));
    };
    let (width, height) = overview.size();
    let source = slide
        .read_region_rgb(coarsest.index, 0, 0, coarsest.width, coarsest.height)
        .map_err(|error| error.to_string())?;

    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for row in 0..height {
        // The centre of this destination row in source pixels, which is the
        // same "which pixel does this point fall in" rule `Framebuffer` uses.
        let source_row =
            ((f64::from(row) + 0.5) * f64::from(coarsest.height) / f64::from(height)) as usize;
        let source_row = source_row.min(coarsest.height as usize - 1);
        for column in 0..width {
            let source_column =
                ((f64::from(column) + 0.5) * f64::from(coarsest.width) / f64::from(width)) as usize;
            let source_column = source_column.min(coarsest.width as usize - 1);
            let at = (source_row * coarsest.width as usize + source_column) * 3;
            pixels.extend_from_slice(&[source[at], source[at + 1], source[at + 2], 255]);
        }
    }
    Ok(Thumbnail { pixels, overview })
}

/// One picture, on its way back from the worker.
struct Made {
    /// Which row asked for it, in `project::Stored::images` order.
    index: usize,
    /// The picture, or why the slide would not give one.
    made: Result<Arc<Thumbnail>, String>,
}

/// The pictures for the rows of one project, and the thread making them.
///
/// **Owned by the project it belongs to**, which is what makes a stale picture
/// impossible rather than merely unlikely. Opening a second project drops this
/// one, the worker's channel with it, and every picture still in flight for the
/// project that is no longer showing. A generation counter would answer the
/// same question later and by comparing numbers; this answers it by there being
/// nowhere for the answer to arrive.
pub struct Pictures {
    /// One slot per row, in the order the project lists its images.
    rows: Vec<Row>,
    /// The worker's end, until it finishes and hangs up.
    ///
    /// `None` once every picture that is coming has come, which is what
    /// [`Pictures::working`] reports and what stops the window asking for
    /// another frame.
    made: Option<Receiver<Made>>,
    /// Set when this is dropped; read by the worker between slides.
    ///
    /// Cancellation is bounded by one slide, exactly as the tile loader's is
    /// bounded by one tile: what a worker has already started, it finishes.
    stop: Arc<AtomicBool>,
}

/// One row's picture, and whether the renderer has it yet.
struct Row {
    picture: Option<Arc<Thumbnail>>,
    /// Why there is no picture, once that is settled.
    ///
    /// The distinction this exists for was invisible until the application was
    /// run on a real study: a JPEG 2000 slide is *there*, opens, and decodes
    /// nothing, so its row held the same empty frame as a row whose slide was
    /// still being read — permanently, and with nothing anywhere saying why.
    /// "No picture yet" and "no picture ever" are different rows and now look
    /// like it.
    unread: Option<String>,
    /// Whether the renderer holds this row's texture *for this project*.
    ///
    /// The browser draws a row's picture by naming a texture id derived from
    /// the row's index, and those ids are reused when another project is
    /// opened. So "there is a picture" is not the question a row may draw on —
    /// the question is whether the picture *at this id* is this project's, and
    /// this flag is that. Without it, the first frame after a picture lands
    /// draws whatever the previous project left at the same id.
    uploaded: bool,
}

impl Pictures {
    /// Starts making the pictures for `slides`, one row per element.
    ///
    /// `None` is a row whose entry names no file — one of the sixteen builder
    /// types that wrap another builder — and it is a row rather than a gap so
    /// that indices stay the project's own.
    ///
    /// `known` supplies rows whose picture has already been made, by path. That
    /// is what makes adding a slide to a fifty-slide project cost one picture
    /// rather than fifty: [`crate::state::App::add_to_project`] writes the file
    /// and re-reads it, and a browser that dropped its pictures there would
    /// blank the whole pane and fill it in again while nothing about the study
    /// had changed.
    #[must_use]
    pub fn start(slides: &[Option<PathBuf>], known: &[(PathBuf, Arc<Thumbnail>)]) -> Self {
        let mut rows: Vec<Row> = Vec::with_capacity(slides.len());
        let mut queue: Vec<(usize, PathBuf)> = Vec::new();
        for (index, slide) in slides.iter().enumerate() {
            let picture = slide.as_ref().and_then(|path| {
                known
                    .iter()
                    .find(|(known, _)| known == path)
                    .map(|(_, picture)| Arc::clone(picture))
            });
            if picture.is_none()
                && let Some(path) = slide
            {
                queue.push((index, path.clone()));
            }
            // A row naming no file has its answer already: nothing is queued
            // for it, so nothing will ever arrive, and an empty frame left
            // uncrossed would say "still reading" for as long as the project is
            // open. The reason is the entry's own, one layer up, so this says
            // only what this layer knows.
            let unread = slide
                .is_none()
                .then(|| String::from("this entry names no file"));
            rows.push(Row {
                picture,
                unread,
                uploaded: false,
            });
        }

        let stop = Arc::new(AtomicBool::new(false));
        let made = (!queue.is_empty()).then(|| spawn(queue, Arc::clone(&stop)));
        Self { rows, made, stop }
    }

    /// Takes delivery of whatever the worker has finished.
    ///
    /// Called before the interface is laid out rather than after, because a
    /// picture that arrives has to be a texture *before* the frame that names
    /// it — see [`Row::uploaded`].
    pub fn take(&mut self) {
        let Some(made) = &self.made else {
            return;
        };
        loop {
            match made.try_recv() {
                Ok(Made { index, made }) => {
                    if let Some(row) = self.rows.get_mut(index) {
                        match made {
                            Ok(picture) => {
                                row.picture = Some(picture);
                                row.uploaded = false;
                            }
                            Err(reason) => row.unread = Some(reason),
                        }
                    }
                }
                Err(TryRecvError::Empty) => return,
                // The worker has walked the whole queue and hung up. Nothing
                // more is coming, and saying so is what lets the window go back
                // to sleep.
                Err(TryRecvError::Disconnected) => {
                    self.made = None;
                    return;
                }
            }
        }
    }

    /// Rows holding a picture the renderer has not been given yet.
    ///
    /// Marks them given, so a picture is copied to a texture once rather than
    /// every frame.
    pub fn undelivered(&mut self) -> Vec<(usize, Arc<Thumbnail>)> {
        let mut pending = Vec::new();
        for (index, row) in self.rows.iter_mut().enumerate() {
            if let Some(picture) = &row.picture
                && !row.uploaded
            {
                row.uploaded = true;
                pending.push((index, Arc::clone(picture)));
            }
        }
        pending
    }

    /// Row `index`'s picture, if it has one the renderer can draw.
    #[must_use]
    pub fn ready(&self, index: usize) -> Option<&Thumbnail> {
        let row = self.rows.get(index)?;
        row.uploaded.then_some(row.picture.as_deref()).flatten()
    }

    /// Why row `index` has no picture, if that is settled.
    ///
    /// `None` while the slide is still being read, so a row that asks this and
    /// gets nothing is a row that should go on waiting.
    #[must_use]
    pub fn unread(&self, index: usize) -> Option<&str> {
        self.rows.get(index)?.unread.as_deref()
    }

    /// Row `index`'s picture, whether or not it can be drawn yet.
    ///
    /// What a re-read carries over, and what a test asserts on: a picture that
    /// has been made is made, and whether a renderer that does not exist has
    /// been handed it is not a fact about the project.
    #[must_use]
    pub fn made(&self, index: usize) -> Option<&Arc<Thumbnail>> {
        self.rows.get(index)?.picture.as_ref()
    }

    /// Whether more pictures are still coming.
    #[must_use]
    pub fn working(&self) -> bool {
        self.made.is_some()
    }
}

impl Drop for Pictures {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts the worker on `queue` and returns the end it delivers to.
///
/// One thread and not a pool, which is QuPath's choice and worth keeping for
/// its reason rather than for its authorship: the browser is read top to bottom,
/// so pictures are wanted in the order they are queued, and a pool delivers the
/// fiftieth before the second about as often as not. It also keeps the machine's
/// cores for the detection run, which is the work someone is actually waiting on.
fn spawn(queue: Vec<(usize, PathBuf)>, stop: Arc<AtomicBool>) -> Receiver<Made> {
    let (sender, receiver) = channel();
    let worker = std::thread::Builder::new().name(String::from("thumbnail-loader"));
    // Detached, and the handle dropped rather than kept: there is nothing to
    // join on: the receiver is how the work arrives and `stop` is how it ends.
    // A machine that will not give this a thread still gets a browser with
    // names and no pictures — the closure is returned in the error and dropped
    // with the sender in it, so the receiver reads as a worker that finished,
    // and it has.
    let _ = worker.spawn(move || {
        for (index, path) in queue {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let made = Slide::open(&path)
                .map_err(|error| error.to_string())
                .and_then(|slide| make(&slide))
                .map(Arc::new);
            if sender.send(Made { index, made }).is_err() {
                return;
            }
        }
    });
    receiver
}
