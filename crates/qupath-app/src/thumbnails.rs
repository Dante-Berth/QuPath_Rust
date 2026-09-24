//! Small pictures of whole slides: the overview's, and the browser's rows.
//!
//! `docs/dev/VIEWER.md` §8.2 needed one picture — the slide that is open, under the
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

/// JPEG quality for `thumbnail.jpg`.
///
/// QuPath writes the file with `ImageIO.write(img, "JPEG", stream)` and so takes
/// the JDK's default, which is 0.75. Matching it is deliberate: this file is a
/// *cache* of a picture either side can regenerate, so the only thing a
/// different quality would buy is a project whose thumbnails visibly change
/// depending on which application last touched them.
const THUMBNAIL_QUALITY: u8 = 75;

/// Encodes a thumbnail as the JPEG QuPath stores at `data/<id>/thumbnail.jpg`.
///
/// # What this is for, and what it is not
///
/// It is **not** what makes the browser's rows show a picture. That was the
/// reasoning §8.8 carried for a long time, and the module comment above has why
/// it was wrong: QuPath's own browser opens the image when the file is absent,
/// and so does this one. Nothing here is waiting on this function.
///
/// What it buys is QuPath not re-reading every slide in a study the first time
/// somebody opens a project written here — the file is a cache, and writing it
/// is a debt to *QuPath*, not to this browser.
///
/// # The alpha channel, which is the whole trap
///
/// [`Thumbnail::pixels`] is **RGBA**, because that is what a texture upload
/// wants. JPEG has no alpha. Handing four-channel data to an encoder expecting
/// three does not fail — it reads the fourth byte of each pixel as the first of
/// the next, so every row shears by one channel and the picture comes out in
/// false colour, sliding further out of register the further down it goes. That
/// is a `docs/dev/VIEWER.md` §6 failure exactly: plausible output, wrong. The alpha
/// is dropped here explicitly, and `a_thumbnail_round_trips_through_a_decoder`
/// checks the result against the source pixels rather than merely checking that
/// bytes were produced.
///
/// # Errors
///
/// Fails if `pixels` is not `width * height * 4` bytes, or if the encoder does.
pub fn encode_jpeg(thumbnail: &Thumbnail) -> Result<Vec<u8>, String> {
    let (width, height) = thumbnail.overview.size();
    let expected = width as usize * height as usize * 4;
    if thumbnail.pixels.len() != expected {
        return Err(format!(
            "the thumbnail is {} bytes, not the {expected} that {width} by {height} RGBA needs",
            thumbnail.pixels.len()
        ));
    }

    // RGBA to RGB. See the note above: this is the step that cannot be skipped.
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for pixel in thumbnail.pixels.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }

    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, THUMBNAIL_QUALITY)
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|error| error.to_string())?;
    Ok(out)
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
    /// it — see `Row::uploaded`.
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

#[cfg(test)]
mod tests {
    use qupath_viewer::overview::Overview;
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    use super::{Thumbnail, encode_jpeg};

    /// A thumbnail with a horizontal red ramp and a vertical green one.
    ///
    /// A gradient rather than flat colour on purpose: a flat picture survives
    /// the channel shear this test exists to catch, because every pixel is the
    /// same and sliding the phase changes nothing. `ROADMAP.md` §5's "plausible
    /// wrong output" applies to test *inputs* too.
    fn ramp(width: u32, height: u32) -> Thumbnail {
        let overview = Overview::fit(width * 4, height * 4).expect("a slide size that fits");
        let (width, height) = overview.size();
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for row in 0..height {
            for column in 0..width {
                let red = u8::try_from(column * 255 / width.max(1)).unwrap_or(255);
                let green = u8::try_from(row * 255 / height.max(1)).unwrap_or(255);
                pixels.extend_from_slice(&[red, green, 64, 255]);
            }
        }
        Thumbnail { pixels, overview }
    }

    /// The bytes are a JPEG a decoder accepts, at the size that was asked for.
    #[test]
    fn a_thumbnail_encodes_to_a_jpeg() {
        let thumbnail = ramp(64, 48);
        let (width, height) = thumbnail.overview.size();
        let encoded = encode_jpeg(&thumbnail).expect("it encodes");

        assert_eq!(
            &encoded[..2],
            &[0xFF, 0xD8],
            "a JPEG starts with SOI, and QuPath reads this with ImageIO"
        );

        let mut decoder = JpegDecoder::new_with_options(
            &encoded[..],
            DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB),
        );
        let decoded = decoder.decode().expect("it decodes");
        assert_eq!(
            decoded.len(),
            width as usize * height as usize * 3,
            "the decoded picture is not the size that was encoded"
        );
    }

    /// The picture that comes back is the picture that went in.
    ///
    /// This is the test that matters, and the alpha channel is why. Handing
    /// RGBA to an encoder that wants RGB does not fail: it reads each pixel's
    /// fourth byte as the next pixel's first, so the image shears by one
    /// channel and slides further out of register down the picture. The result
    /// is a plausible-looking JPEG in false colour — `docs/dev/VIEWER.md` §6's
    /// recurring failure. Comparing corners against the source is what sees it;
    /// checking that bytes were produced is not.
    ///
    /// The tolerance is for JPEG being lossy, and is far tighter than the shear:
    /// dropping a channel moves red at the right-hand edge by nearly the full
    /// range.
    #[test]
    fn a_thumbnail_round_trips_through_a_decoder() {
        let thumbnail = ramp(64, 48);
        let (width, height) = thumbnail.overview.size();
        let encoded = encode_jpeg(&thumbnail).expect("it encodes");

        let mut decoder = JpegDecoder::new_with_options(
            &encoded[..],
            DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB),
        );
        let decoded = decoder.decode().expect("it decodes");

        for (row, column) in [
            (0, 0),
            (0, width - 1),
            (height - 1, 0),
            (height - 1, width - 1),
            (height / 2, width / 2),
        ] {
            let source = (row as usize * width as usize + column as usize) * 4;
            let target = (row as usize * width as usize + column as usize) * 3;
            for channel in 0..3 {
                let want = i32::from(thumbnail.pixels[source + channel]);
                let got = i32::from(decoded[target + channel]);
                assert!(
                    (want - got).abs() <= 24,
                    "channel {channel} at {row},{column}: wrote {want}, read back {got}"
                );
            }
        }
    }

    /// Pixels that are not RGBA are refused rather than encoded as something.
    ///
    /// The failure this prevents is silent: a buffer one pixel short still
    /// encodes, into a picture shifted by a channel.
    #[test]
    fn a_thumbnail_of_the_wrong_length_is_refused() {
        let mut thumbnail = ramp(64, 48);
        thumbnail.pixels.truncate(thumbnail.pixels.len() - 4);
        let error = encode_jpeg(&thumbnail).expect_err("a short buffer is refused");
        assert!(
            error.contains("RGBA"),
            "the error should say what was wrong: {error}"
        );
    }
}
