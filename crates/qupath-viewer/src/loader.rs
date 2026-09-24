//! Tile decoding off the main thread, with cancellation.
//!
//! `docs/dev/VIEWER.md` §2 settled the shape: a thread pool and channels, not
//! async, because decode is CPU-bound and what is actually needed is a request
//! queue with **cancellation** — panning makes half the in-flight requests
//! garbage before they land. A pool of its own, never a `par_iter` nested
//! inside anything (`ROADMAP.md` §5).
//!
//! # What cancellation means here
//!
//! [`TileLoader::request`] *replaces* the queue rather than appending to it.
//! Everything still waiting is dropped unread, so a fast pan across a slide
//! decodes the tiles of the frame it stopped on, not of every frame it passed
//! through. The only work that survives a cancellation is what a worker had
//! already started — at most one tile per worker, which is the bound that
//! matters.
//!
//! A tile that *finishes* after being superseded is still delivered. Throwing
//! away a completed decode buys nothing: it cost what it cost, and the next
//! frame may well want it. [`Loaded::generation`] says which request it answers
//! so a caller that cares can tell.
//!
//! # Testing without a slide
//!
//! [`TileSource`] is a trait so the tests can drive the pool with a fake that
//! blocks on command and counts its decodes. Cancellation is otherwise
//! untestable — asserting it needs a decode that is still running when the next
//! request arrives, and a real JPEG is far too fast to schedule against.
//! [`SlideTiles`] is the real implementation and needs no GPU either.

use std::collections::{HashSet, VecDeque};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use qupath_slide::Slide;

use crate::coords::TileIndex;

/// Somewhere tiles come from, decoded into the packed RGBA the GPU takes.
///
/// Implemented by [`SlideTiles`] for a real slide. The trait exists so the pool
/// can be tested without one; see the module documentation.
pub trait TileSource: Send + Sync + 'static {
    /// Decodes `tile` into `out` as tightly packed RGBA8, returning its size in
    /// pixels.
    ///
    /// `out` is cleared and refilled, then handed to the caller, so it is a
    /// fresh buffer each time. `scratch` is *not* — it belongs to the calling
    /// worker thread and persists across calls, which is where an
    /// implementation puts its decode buffer so a tile loop allocates nothing
    /// beyond the output it gives away.
    ///
    /// # Errors
    ///
    /// Returns a description of what went wrong. The pool drops the tile and
    /// carries on; one undecodable tile must not stall the queue.
    fn load(
        &self,
        tile: TileIndex,
        scratch: &mut Vec<u8>,
        out: &mut Vec<u8>,
    ) -> Result<(u32, u32), String>;
}

/// A decoded tile on its way to the cache.
#[derive(Debug)]
pub struct Loaded {
    /// Which tile.
    pub tile: TileIndex,
    /// The [`TileLoader::request`] this answers. Compare against
    /// [`TileLoader::generation`] to tell whether the view has moved on.
    pub generation: u64,
    /// Width in pixels of the packed image.
    pub width: u32,
    /// Height in pixels of the packed image.
    pub height: u32,
    /// Tightly packed RGBA8, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// The queue, shared by the main thread and every worker.
#[derive(Debug)]
struct Queue {
    /// Bumped by every [`TileLoader::request`]. Tags results, and is what a
    /// worker stamps onto the tile it claims.
    generation: u64,
    /// Waiting to be claimed, in request order — the caller's priority.
    pending: VecDeque<TileIndex>,
    /// The same tiles as `pending`, for a cheap membership test.
    queued: HashSet<TileIndex>,
    /// Claimed by a worker and not yet delivered.
    claimed: HashSet<TileIndex>,
    /// Delivered into the channel and not yet drained by the caller.
    ///
    /// The state between "a worker is done with it" and "the caller has it",
    /// which is invisible to everything else: the tile is not claimed, and the
    /// caller cannot have made it resident because it has not seen it yet. A
    /// request landing in that window re-queues a tile that is already decoded,
    /// which is the same bug the delivery order below is written to avoid, one
    /// step further along. Entered under the same lock that releases the claim,
    /// so a tile is never in neither set.
    delivered: HashSet<TileIndex>,
    /// Tiles that failed to decode, so they are never asked for again. A
    /// caller drives requests from what is *not* resident, and a tile that can
    /// never become resident would otherwise be re-decoded every redraw.
    failed: HashSet<TileIndex>,
    shutdown: bool,
    decoded: u64,
    cancelled: u64,
}

#[derive(Debug)]
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
}

impl Shared {
    /// The queue, ignoring poisoning.
    ///
    /// A panicking worker must not take the viewer down with it: the queue is
    /// plain data and a half-finished decode leaves it consistent, because
    /// every field is updated under one lock at a time.
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A pool of decoding threads fed by a cancellable queue.
///
/// Dropping it stops and joins every worker.
#[derive(Debug)]
pub struct TileLoader {
    shared: Arc<Shared>,
    done: Receiver<Loaded>,
    workers: Vec<JoinHandle<()>>,
}

impl TileLoader {
    /// Starts `workers` threads decoding from `source`.
    ///
    /// `workers` is clamped to at least one — a pool of zero would accept
    /// requests and never answer them, which looks exactly like a hung viewer.
    #[must_use]
    pub fn new<S: TileSource>(source: &Arc<S>, workers: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                generation: 0,
                pending: VecDeque::new(),
                queued: HashSet::new(),
                claimed: HashSet::new(),
                delivered: HashSet::new(),
                failed: HashSet::new(),
                shutdown: false,
                decoded: 0,
                cancelled: 0,
            }),
            wake: Condvar::new(),
        });
        let (sender, done) = channel();

        let workers = (0..workers.max(1))
            .map(|index| {
                let shared = Arc::clone(&shared);
                let source = Arc::clone(source);
                let sender = sender.clone();
                std::thread::Builder::new()
                    .name(format!("tile-loader-{index}"))
                    .spawn(move || work(&shared, source.as_ref(), &sender))
                    .expect("spawn tile loader")
            })
            .collect();

        Self {
            shared,
            done,
            workers,
        }
    }

    /// Replaces the queue with `tiles`, in priority order, and returns the new
    /// generation.
    ///
    /// This is the cancellation: whatever was still waiting is dropped. Tiles
    /// a worker has already claimed are left alone and are not re-queued, so
    /// asking for the same tile every frame does not decode it twice; a tile
    /// that has already failed to decode is dropped for the same reason.
    ///
    /// The caller is expected to have filtered out what is already cached —
    /// this has no idea what is resident.
    pub fn request(&self, tiles: impl IntoIterator<Item = TileIndex>) -> u64 {
        let mut queue = self.shared.lock();
        queue.generation += 1;

        let dropped = queue.pending.len() as u64;
        queue.cancelled += dropped;
        queue.pending.clear();
        queue.queued.clear();

        for tile in tiles {
            let known = queue.claimed.contains(&tile)
                || queue.delivered.contains(&tile)
                || queue.failed.contains(&tile);
            if !known && queue.queued.insert(tile) {
                queue.pending.push_back(tile);
            }
        }

        let generation = queue.generation;
        drop(queue);
        self.shared.wake.notify_all();
        generation
    }

    /// Everything finished since the last call, in completion order.
    ///
    /// Never blocks. Stale results are included on purpose; see the module
    /// documentation for why a finished decode is not thrown away.
    pub fn drain(&self) -> Vec<Loaded> {
        let mut loaded = Vec::new();
        while let Ok(tile) = self.done.try_recv() {
            loaded.push(tile);
        }
        if !loaded.is_empty() {
            // The caller has them now, so they stop being the loader's problem
            // and a later request for one is a genuine request. One lock for
            // the whole batch, not one per tile.
            let mut queue = self.shared.lock();
            for tile in &loaded {
                queue.delivered.remove(&tile.tile);
            }
        }
        loaded
    }

    /// The generation of the most recent [`TileLoader::request`].
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.shared.lock().generation
    }

    /// Tiles queued and not yet claimed by a worker.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.shared.lock().pending.len()
    }

    /// Tiles a worker is decoding right now.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.shared.lock().claimed.len()
    }

    /// Tiles decoded and sent, and not yet taken delivery of by the caller.
    ///
    /// The state between a worker finishing and [`TileLoader::drain`] being
    /// called. It is work still owed to the caller — the tile exists but no
    /// frame has drawn it — which is why [`TileLoader::is_idle`] counts it and
    /// why a status line counting what is outstanding should too.
    #[must_use]
    pub fn undrained(&self) -> usize {
        self.shared.lock().delivered.len()
    }

    /// Tiles that failed to decode and will not be attempted again.
    ///
    /// A hole in the frame that stays a hole. Zero on a slide `qupath-slide`
    /// can read at all; anything else means the codec check in
    /// [`Slide::read_tile_into`] let something through per tile rather than per
    /// slide, which is worth seeing rather than watching redraw silently.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.shared.lock().failed.len()
    }

    /// Whether every requested tile has been decoded *and taken delivery of*.
    ///
    /// The render loop uses this to decide whether it still needs to be woken:
    /// while this is false the frame is incomplete and another redraw is owed.
    ///
    /// `delivered` is counted, and leaving it out was a bug. A tile that has
    /// been sent and not yet drained is in none of the other sets — that is
    /// what `delivered` is for — so the instant the last worker released its
    /// claim this said "nothing left", the loop slept, and the tile stayed in
    /// the channel with no frame coming to draw it. `drain` is what empties the
    /// set, and the render loop drains at the top of every frame, so this still
    /// settles to true after one more redraw rather than asking for frames
    /// forever.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        let queue = self.shared.lock();
        queue.pending.is_empty() && queue.claimed.is_empty() && queue.delivered.is_empty()
    }

    /// Tiles decoded since the pool started, successfully or not.
    #[must_use]
    pub fn decoded(&self) -> u64 {
        self.shared.lock().decoded
    }

    /// Tiles dropped from the queue by a later request without being decoded.
    ///
    /// This is the work cancellation *saved*. A pan that raises it is doing its
    /// job; a stationary view that raises it is re-requesting what it already
    /// asked for.
    #[must_use]
    pub fn cancelled(&self) -> u64 {
        self.shared.lock().cancelled
    }
}

impl Drop for TileLoader {
    fn drop(&mut self) {
        {
            let mut queue = self.shared.lock();
            queue.shutdown = true;
            queue.pending.clear();
            queue.queued.clear();
        }
        self.shared.wake.notify_all();
        for worker in self.workers.drain(..) {
            // A worker that panicked has already reported it; joining is about
            // not outliving the source they borrow, not about the result.
            drop(worker.join());
        }
    }
}

/// What became of one tile a worker claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Decoded and handed to the caller, who has not drained it yet.
    Delivered,
    /// Undecodable, and recorded so it is never asked for again.
    Failed,
    /// The caller has gone: the receiver is dropped and this worker is done.
    Abandoned,
}

/// One worker: claim a tile, decode it, deliver it, repeat.
fn work<S: TileSource>(shared: &Arc<Shared>, source: &S, done: &Sender<Loaded>) {
    let mut scratch = Vec::new();
    let mut out = Vec::new();

    loop {
        let (tile, generation) = {
            let mut queue = shared.lock();
            loop {
                if queue.shutdown {
                    return;
                }
                if let Some(tile) = queue.pending.pop_front() {
                    queue.queued.remove(&tile);
                    queue.claimed.insert(tile);
                    break (tile, queue.generation);
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };

        // Decoded outside the lock, which is the whole point of the pool.
        let size = source.load(tile, &mut scratch, &mut out);

        // Delivered *before* the tile stops counting as in flight, and the
        // order is not cosmetic. A caller decides what to request from what is
        // resident, and a tile that is neither claimed nor delivered is
        // invisible to both tests — so releasing the claim first opens a window
        // in which the next request queues a tile that is already decoded and
        // on its way. That window is small and a redraw hits it constantly:
        // `a_stationary_view_decodes_each_tile_once` caught it decoding ten
        // tiles for a nine-tile view.
        // Three outcomes, and they are kept apart because each leaves the tile
        // in a different set: a decoded tile is the caller's until it drains,
        // an undecodable one is refused forever, and a closed channel means the
        // loader is being dropped and this thread is finished.
        let outcome = match size {
            Ok((width, height)) => {
                let sent = done.send(Loaded {
                    tile,
                    generation,
                    width,
                    height,
                    pixels: std::mem::take(&mut out),
                });
                if sent.is_ok() {
                    Outcome::Delivered
                } else {
                    Outcome::Abandoned
                }
            }
            Err(_) => {
                // One bad tile is a hole in the frame, not a stalled queue —
                // but it must not be a hole the caller asks about forever, so
                // it is remembered rather than retried on the next redraw.
                shared.lock().failed.insert(tile);
                Outcome::Failed
            }
        };

        {
            let mut queue = shared.lock();
            // Into `delivered` before out of `claimed`, under one lock, so
            // there is no instant at which the tile is in neither and a request
            // could queue it a second time. A failed tile is not entered: it is
            // in `failed` already, and nothing will ever drain it back out.
            if outcome == Outcome::Delivered {
                queue.delivered.insert(tile);
            }
            queue.claimed.remove(&tile);
            queue.decoded += 1;
        }

        if outcome == Outcome::Abandoned {
            return;
        }
    }
}

/// A [`TileSource`] over a real slide.
///
/// Owns the [`Slide`] because the pool needs it for as long as the pool lives.
/// `Slide` reads through positioned reads and so is `Sync`: the workers share
/// one open file with no lock, which `docs/dev/VIEWER.md` §1 counted on.
#[derive(Debug)]
pub struct SlideTiles {
    slide: Slide,
}

impl SlideTiles {
    /// Wraps an open slide as a tile source.
    #[must_use]
    pub fn new(slide: Slide) -> Self {
        Self { slide }
    }

    /// The slide underneath, for the viewport arithmetic that needs the level
    /// table.
    #[must_use]
    pub fn slide(&self) -> &Slide {
        &self.slide
    }
}

impl TileSource for SlideTiles {
    fn load(
        &self,
        tile: TileIndex,
        scratch: &mut Vec<u8>,
        out: &mut Vec<u8>,
    ) -> Result<(u32, u32), String> {
        let level = self
            .slide
            .levels()
            .get(tile.level)
            .ok_or_else(|| format!("no level {}", tile.level))?;

        self.slide
            .read_tile_into(tile.level, tile.column, tile.row, scratch)
            .map_err(|error| error.to_string())?;

        // Cropping and expanding here rather than on the main thread: it is
        // per-pixel work on a tile that has just been decoded and is still warm
        // in this thread's cache, and it is exactly the kind of thing that
        // makes a pan stutter when it happens between frames.
        let (width, height) = tile.visible_size(level);
        expand_to_rgba_into(scratch, level.tile_width, width, height, out);
        Ok((width, height))
    }
}

/// RGB8 rows of `stride` pixels to tightly packed RGBA8 of `width` by `height`.
///
/// Two jobs at once, and both are forced. The padding TIFF stores in edge tiles
/// is dropped, which is `docs/dev/VIEWER.md` §1's "decide once" decided in favour of
/// the viewer cropping rather than the reader changing; and the alpha channel is
/// added because no GPU takes a three-byte texture — `wgpu` has no `Rgb8Unorm`,
/// and the detection pipeline reads the same tiles and wants no alpha.
///
/// `out` is cleared first and ends up exactly `width * height * 4` bytes.
///
/// # Panics
///
/// Panics if `rgb` is shorter than `stride * height * 3` bytes, which would
/// mean the decoder returned a tile smaller than the level says it is.
pub fn expand_to_rgba_into(rgb: &[u8], stride: u32, width: u32, height: u32, out: &mut Vec<u8>) {
    let (stride, width, height) = (stride as usize, width as usize, height as usize);
    out.clear();
    out.reserve(width * height * 4);
    for row in 0..height {
        let start = row * stride * 3;
        for column in 0..width {
            let pixel = start + column * 3;
            out.extend_from_slice(&[rgb[pixel], rgb[pixel + 1], rgb[pixel + 2], 255]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::*;

    /// A source that decodes nothing and can be held shut.
    ///
    /// The gate is what makes cancellation testable: a real JPEG decode
    /// finishes far too fast to still be running when the next request lands,
    /// so there would be nothing to cancel.
    struct Fake {
        decodes: AtomicU64,
        gate: Mutex<bool>,
        opened: Condvar,
        /// Tiles in this column fail, so a failure can be aimed.
        failing_column: u32,
    }

    impl Fake {
        fn open() -> Arc<Self> {
            Arc::new(Self {
                decodes: AtomicU64::new(0),
                gate: Mutex::new(true),
                opened: Condvar::new(),
                failing_column: u32::MAX,
            })
        }

        fn shut() -> Arc<Self> {
            Arc::new(Self {
                decodes: AtomicU64::new(0),
                gate: Mutex::new(false),
                opened: Condvar::new(),
                failing_column: u32::MAX,
            })
        }

        fn unlatch(&self) {
            *self.gate.lock().expect("gate") = true;
            self.opened.notify_all();
        }

        fn decodes(&self) -> u64 {
            self.decodes.load(Ordering::SeqCst)
        }
    }

    impl TileSource for Fake {
        fn load(
            &self,
            tile: TileIndex,
            _scratch: &mut Vec<u8>,
            out: &mut Vec<u8>,
        ) -> Result<(u32, u32), String> {
            let mut open = self.gate.lock().expect("gate");
            while !*open {
                open = self.opened.wait(open).expect("gate");
            }
            drop(open);

            self.decodes.fetch_add(1, Ordering::SeqCst);
            if tile.column == self.failing_column {
                return Err("aimed failure".to_owned());
            }
            // One RGBA pixel: the pool is being tested, not the pixels.
            out.clear();
            out.extend_from_slice(&[0, 0, 0, 255]);
            Ok((1, 1))
        }
    }

    /// Spins until `condition` holds, failing the test rather than hanging.
    fn until(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("timed out waiting for {what}");
    }

    /// Whether the *workers* have nothing left, drained or not.
    ///
    /// Distinct from [`TileLoader::is_idle`], which also waits for the caller
    /// to take delivery: a test that wants a tile sitting undrained in the
    /// channel is asking this question and not that one.
    fn settled(loader: &TileLoader) -> bool {
        loader.pending() == 0 && loader.in_flight() == 0
    }

    fn tiles(count: u32) -> Vec<TileIndex> {
        (0..count)
            .map(|column| TileIndex::new(column, 0, 0))
            .collect()
    }

    #[test]
    fn every_requested_tile_comes_back_exactly_once() {
        let source = Fake::open();
        let loader = TileLoader::new(&source, 4);
        loader.request(tiles(64));

        let mut delivered = Vec::new();
        until("the queue to drain", || {
            delivered.extend(loader.drain());
            loader.is_idle() && delivered.len() == 64
        });

        delivered.sort_by_key(|loaded| loaded.tile.column);
        let columns: Vec<u32> = delivered.iter().map(|loaded| loaded.tile.column).collect();
        assert_eq!(columns, (0..64).collect::<Vec<_>>());
        assert_eq!(source.decodes(), 64);
    }

    #[test]
    fn a_delivered_tile_is_not_decoded_again_before_it_is_drained() {
        // The last window in the hand-off, and the one that survived the fix
        // the delivery order above records. That fix moved the release of the
        // claim to *after* the send, which closes the gap between "decoded" and
        // "sent" — but not the gap between "sent" and "the caller has looked".
        //
        // A caller decides what to request from what is resident, and a tile
        // sitting in the channel is not resident yet. So it asks again, and
        // before `delivered` existed the loader had no reason to say no.
        //
        // This is deterministic where the symptom was not: the flake it fixes
        // is `a_stationary_view_decodes_each_tile_once` in `tests/loading.rs`,
        // which passes alone and fails under a loaded `cargo test --workspace`
        // because the window is only ever hit when a worker finishes at exactly
        // the wrong moment. Waiting for `is_idle` puts the tile in that window
        // on purpose rather than by luck.
        let source = Fake::open();
        let loader = TileLoader::new(&source, 1);

        loader.request(tiles(1));
        until("the tile to be delivered", || settled(&loader));
        assert_eq!(source.decodes(), 1);

        // Requested again, undrained. `request` decides synchronously under the
        // lock, so `pending` is the whole answer and no sleep is involved.
        loader.request(tiles(1));
        assert_eq!(loader.pending(), 0, "a delivered tile was queued again");
        assert_eq!(source.decodes(), 1);

        // And once drained it belongs to the caller, so asking is a real
        // request again — otherwise an evicted tile could never be reloaded,
        // which is a worse bug than the one being fixed.
        assert_eq!(loader.drain().len(), 1);
        loader.request(tiles(1));
        until("the second decode", || {
            settled(&loader) && source.decodes() == 2
        });
    }

    /// A tile in the channel and not yet drained is still work owed.
    ///
    /// [`TileLoader::is_idle`] is what the render loop asks to decide whether
    /// it needs another frame, and `delivered` is the set that exists precisely
    /// because a tile between "a worker sent it" and "the caller drained it" is
    /// invisible to the other two. `is_idle` did not consult it — so the moment
    /// the last worker released its claim the loop was told there was nothing
    /// left, went to sleep, and the tile sat in the channel undrawn.
    ///
    /// What that looks like is a pan that stops one tile short of finished:
    /// the underlay covers the hole, so the frame is soft in one square until
    /// the next mouse movement wakes the loop and the tile appears. It reads as
    /// a slow decode rather than as a bug, which is why it is asserted here
    /// rather than looked for on screen — this is the fourth of §3.4's bugs
    /// "about *when* state becomes visible", and they are all invisible in
    /// exactly this way.
    #[test]
    fn a_tile_in_the_channel_is_not_idle() {
        let source = Fake::open();
        let loader = TileLoader::new(&source, 1);
        loader.request(tiles(1));

        // Decoded, sent, and deliberately not drained. `decoded` is bumped
        // under the same lock that moves the tile out of `claimed`, so once
        // the claim is released the tile is in `delivered` and nowhere else —
        // which is the state the render loop was reading as "nothing to do".
        until("the workers to finish", || settled(&loader));
        assert_eq!(source.decodes(), 1);
        assert_eq!(loader.undrained(), 1, "the tile is in the channel");
        assert!(
            !loader.is_idle(),
            "a tile the caller has not seen yet is still owed to it",
        );

        // And once the caller has taken delivery, there is nothing left.
        assert_eq!(loader.drain().len(), 1);
        assert!(loader.is_idle(), "nothing is left once the caller drains");
    }

    #[test]
    fn a_failed_tile_is_not_held_as_delivered() {
        // A tile that failed is refused by `failed`, and must not *also* be
        // entered into `delivered` — nothing ever drains it back out, so it
        // would sit there for the life of the loader. Invisible from the
        // outside, which is why it is asserted from the inside.
        let source = Arc::new(Fake {
            decodes: AtomicU64::new(0),
            gate: Mutex::new(true),
            opened: Condvar::new(),
            failing_column: 0,
        });
        let loader = TileLoader::new(&source, 1);
        loader.request(tiles(1));
        until("the failure to be recorded", || {
            loader.is_idle() && loader.failed() == 1
        });

        assert!(loader.drain().is_empty());
        assert!(
            loader.shared.lock().delivered.is_empty(),
            "a failed tile was left in the delivered set"
        );
    }

    #[test]
    fn a_superseded_request_is_never_decoded() {
        // The claim milestone 3.4 rests on. One worker, held shut on its first
        // tile, then the view moves: everything still queued must be dropped
        // unread, so the pool decodes one stale tile — the one already claimed
        // — and then only what the new view asked for.
        let source = Fake::shut();
        let loader = TileLoader::new(&source, 1);

        loader.request(tiles(500));
        until("the worker to claim a tile", || loader.in_flight() == 1);
        assert_eq!(loader.pending(), 499);

        let wanted = vec![
            TileIndex::new(900, 0, 0),
            TileIndex::new(901, 0, 0),
            TileIndex::new(902, 0, 0),
        ];
        loader.request(wanted.clone());
        assert_eq!(loader.pending(), 3, "the stale queue survived the request");
        assert_eq!(loader.cancelled(), 499);

        source.unlatch();
        let mut delivered = Vec::new();
        until("the new request to finish", || {
            delivered.extend(loader.drain());
            loader.is_idle() && delivered.len() == 4
        });

        // One in-flight tile plus the three that were actually wanted. Without
        // cancellation this would be 503.
        assert_eq!(source.decodes(), 4);
        let mut columns: Vec<u32> = delivered.iter().map(|loaded| loaded.tile.column).collect();
        columns.sort_unstable();
        assert_eq!(columns, vec![0, 900, 901, 902]);
    }

    #[test]
    fn a_result_carries_the_request_it_answers() {
        // A stale decode is delivered rather than thrown away, so the caller
        // needs to be able to tell. This is what makes that possible.
        let source = Fake::shut();
        let loader = TileLoader::new(&source, 1);

        let first = loader.request(tiles(20));
        until("the worker to claim a tile", || loader.in_flight() == 1);
        let second = loader.request(vec![TileIndex::new(900, 0, 0)]);
        assert_eq!(second, first + 1);

        source.unlatch();
        let mut delivered = Vec::new();
        until("both tiles", || {
            delivered.extend(loader.drain());
            loader.is_idle() && delivered.len() == 2
        });

        let stale = delivered
            .iter()
            .find(|loaded| loaded.tile.column == 0)
            .expect("the claimed tile");
        assert_eq!(stale.generation, first);
        assert_eq!(loader.generation(), second);
    }

    #[test]
    fn asking_twice_for_a_claimed_tile_does_not_decode_it_twice() {
        // A stationary view re-requests its visible tiles every frame while the
        // first decode is still running. Queueing them again would decode the
        // whole frame once per frame, which is the opposite of a cache.
        let source = Fake::shut();
        let loader = TileLoader::new(&source, 1);

        loader.request(tiles(1));
        until("the worker to claim it", || loader.in_flight() == 1);
        for _ in 0..10 {
            loader.request(tiles(1));
        }
        assert_eq!(loader.pending(), 0, "the claimed tile was queued again");

        source.unlatch();
        until("the tile", || settled(&loader));
        assert_eq!(source.decodes(), 1);
    }

    #[test]
    fn one_undecodable_tile_does_not_stall_the_queue() {
        let source = Arc::new(Fake {
            decodes: AtomicU64::new(0),
            gate: Mutex::new(true),
            opened: Condvar::new(),
            failing_column: 7,
        });
        let loader = TileLoader::new(&source, 2);
        loader.request(tiles(16));

        let mut delivered = Vec::new();
        until("the rest of the queue", || {
            delivered.extend(loader.drain());
            loader.is_idle() && delivered.len() == 15
        });
        assert_eq!(source.decodes(), 16, "the failing tile was not attempted");
        assert!(!delivered.iter().any(|loaded| loaded.tile.column == 7));
    }

    #[test]
    fn a_tile_that_cannot_decode_is_not_asked_for_again() {
        // The caller decides what to request from what is *not* resident, and a
        // tile that fails never becomes resident. Without remembering the
        // failure it would be decoded once per redraw forever — a busy core
        // and a frame rate that quietly halves, with nothing on screen to say
        // why.
        let source = Arc::new(Fake {
            decodes: AtomicU64::new(0),
            gate: Mutex::new(true),
            opened: Condvar::new(),
            failing_column: 0,
        });
        let loader = TileLoader::new(&source, 1);

        for _ in 0..20 {
            loader.request(tiles(1));
            until("the request to settle", || loader.is_idle());
        }
        assert_eq!(source.decodes(), 1, "the failing tile was retried");
        assert_eq!(loader.failed(), 1);
    }

    #[test]
    fn dropping_the_loader_with_work_queued_returns() {
        // The workers hold an `Arc` to the source and block on a condvar. If
        // shutdown did not wake them the drop would hang here forever, which is
        // a deadlock at every process exit.
        let source = Fake::open();
        let loader = TileLoader::new(&source, 4);
        loader.request(tiles(10_000));
        drop(loader);
        assert_eq!(
            Arc::strong_count(&source),
            1,
            "a worker outlived the loader"
        );
    }

    #[test]
    fn expanding_a_tile_drops_the_padding_and_adds_alpha() {
        // Three pixels per row stored, two of them real: the third column is
        // JPEG padding at a level edge and must not reach the texture.
        let rgb = vec![
            1, 2, 3, 4, 5, 6, 250, 250, 250, // row 0, last pixel is padding
            7, 8, 9, 10, 11, 12, 250, 250, 250, // row 1
        ];
        let mut out = Vec::new();
        expand_to_rgba_into(&rgb, 3, 2, 2, &mut out);
        assert_eq!(
            out,
            vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255]
        );
    }

    #[test]
    fn expanding_reuses_the_output_buffer() {
        // The buffer is handed to the caller and a fresh one is used next time,
        // but within one call it must not append to what was there.
        let rgb = vec![1, 2, 3, 4, 5, 6];
        let mut out = vec![9; 100];
        expand_to_rgba_into(&rgb, 2, 2, 1, &mut out);
        assert_eq!(out, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }
}
