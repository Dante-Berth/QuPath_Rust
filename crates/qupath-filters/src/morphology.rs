//! Grayscale morphological reconstruction, and the regional-maximum transform
//! built on it, matching QuPath's `MorphologicalReconstruction`.
//!
//! This is the stage between the Laplacian of Gaussian and the watershed:
//!
//! ```java
//! ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false);
//! ImageProcessor ipNucleusLabels = RoiLabeling.labelImage(ipTemp, 0, false);
//! Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false);
//! ```
//!
//! # What it computes, and why the pipeline needs it
//!
//! [`find_regional_maxima`] returns the **prominence** of every pixel: how far
//! it stands above the highest saddle that reaches it. It gets there in three
//! steps, and only the middle one is reconstruction:
//!
//! 1. every pixel at or above the threshold that is `>=` all eight of its
//!    neighbours is punched out to negative infinity;
//! 2. the holes are filled back up by geodesic dilation under the original
//!    image, which is [`morphological_reconstruction`];
//! 3. the absolute difference against the original is what is left.
//!
//! A hole refills to the level of the highest path leading into it. A genuine
//! peak refills only to its highest saddle, so its prominence is positive. A
//! plateau hanging off the side of a slope refills to its own value, so its
//! prominence is zero and the labelling that follows discards it.
//!
//! **That second kind is the whole point.** The maximum test in step 1 is not
//! strict — it has to be, because the filters upstream create plateaus by
//! construction — so it marks every shelf and terrace as well as every peak. A
//! port that stops after step 1 and labels what it marked reports a marker per
//! terrace, and one nucleus becomes several. Prominence is what tells them
//! apart, and nothing cheaper does.
//!
//! # Order independence
//!
//! Reconstruction by dilation has a unique fixed point: it is the largest image
//! below the mask that is reachable from the marker, and no traversal order
//! changes what that is. QuPath reaches it with raster passes followed by a
//! FIFO queue, and this port follows the same route — not because the answer
//! depends on it, but because the raster pass reads the four neighbours it has
//! already written and so converges in far fewer visits than a queue alone.
//!
//! This is a rare property here. Almost everything else ported in this crate is
//! order-dependent in a way that makes bug-for-bug fidelity the only option.

use std::collections::VecDeque;

/// QuPath's own repeat condition: keep going while a raster pass still moves
/// more than a tenth of the image.
const DEFAULT_REPEAT_FRACTION: usize = 10;

/// A reconstruction that owns its working buffers, for repeated use.
///
/// A tile pipeline reconstructs once per tile, and the queue can hold a
/// meaningful fraction of the image; rebuilding it per tile is allocation in
/// the tile loop, a defect under §6 of the project guidelines. Holding it here
/// turns that into a clear.
#[derive(Debug)]
pub struct Reconstructor {
    /// FIFO, matching QuPath's `IntDequeue`. Order does not decide the fixed
    /// point — see the module note — but it does decide how quickly it is
    /// reached.
    queue: VecDeque<u32>,
    /// Raster passes the last [`Reconstructor::reconstruct`] needed.
    passes: u32,
    /// Pixels the last [`Reconstructor::reconstruct`] pushed onto the queue.
    queued: u64,
    /// Repeat another raster pair while a pass still moves more than
    /// `1 / repeat_fraction` of the image. Zero runs no repeats at all.
    ///
    /// Purely a speed knob: the fixed point is the same whatever it is, so this
    /// trades raster work against queue work and nothing else. See
    /// [`Reconstructor::with_repeat_fraction`].
    repeat_fraction: usize,
}

impl Default for Reconstructor {
    /// Written out rather than derived: a derived `Default` would zero the
    /// repeat fraction, which silently turns off the raster repeats and leaves
    /// the whole reconstruction to the queue — twice as slow on a real tile, as
    /// `reconstruct_previous_vs_current` measures.
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            passes: 0,
            queued: 0,
            repeat_fraction: DEFAULT_REPEAT_FRACTION,
        }
    }
}

impl Reconstructor {
    /// Creates a reconstructor with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides how long the raster passes repeat before the queue takes over.
    ///
    /// The reconstruction has a unique fixed point and reaches it whatever the
    /// traversal, so this cannot change the result — only how long it takes.
    /// Provided so the trade-off can be measured rather than assumed; the
    /// A/B harness asserts the output is bit-identical across settings.
    #[must_use]
    pub fn with_repeat_fraction(mut self, fraction: usize) -> Self {
        self.repeat_fraction = fraction;
        self
    }

    /// Raises `marker` towards `mask` by geodesic dilation, in place.
    ///
    /// Semantics are identical to [`morphological_reconstruction`]; this reuses
    /// the queue instead of building one.
    ///
    /// # Panics
    ///
    /// Panics if `marker` and `mask` differ in length, if `width` is zero or
    /// does not divide that length, or if the image has more pixels than a
    /// `u32` can index.
    pub fn reconstruct(&mut self, marker: &mut [f32], mask: &[f32], width: usize) {
        let height = check(marker, mask, width);
        if height == 0 {
            return;
        }

        let pixels = marker.len();
        // One forward raster pass, then alternating passes for as long as they
        // are still moving a tenth of the image. QuPath's comment is the
        // justification: the queue is correct on its own but slow, so it is
        // worth repeating the cheap pass while it is doing bulk work.
        let mut changes = dilate_and_compare(marker, mask, width, height, false, None);
        self.passes = 1;
        while self.repeat_fraction > 0 && changes * self.repeat_fraction > pixels {
            dilate_and_compare(marker, mask, width, height, true, None);
            changes = dilate_and_compare(marker, mask, width, height, false, None);
            self.passes += 2;
        }

        // The backward pass that fills the queue with the pixels the raster
        // scans could not settle, then the queue itself.
        self.queue.clear();
        dilate_and_compare(marker, mask, width, height, true, Some(&mut self.queue));
        self.passes += 1;
        self.queued = self.queue.len() as u64;
        process_queue(marker, mask, width, height, &mut self.queue);
    }

    /// Raster passes the last [`Reconstructor::reconstruct`] needed.
    ///
    /// Diagnostic: the pass count is what makes this stage's cost scale with the
    /// tile rather than with the pixel count, since a geodesic dilation has to
    /// propagate across the whole tile and a raster pass carries it only as far
    /// as a monotone path reaches.
    #[must_use]
    pub fn last_passes(&self) -> u32 {
        self.passes
    }

    /// Pixels the last [`Reconstructor::reconstruct`] left for the queue.
    #[must_use]
    pub fn last_queued(&self) -> u64 {
        self.queued
    }

    /// Writes the prominence of every pixel of `image` into `out`.
    ///
    /// `threshold` is the value below which a pixel is not considered a maximum
    /// at all; detection passes `0.001`. See the module documentation for what
    /// prominence means and why it is the quantity that matters.
    ///
    /// `out` is resized to match `image` and fully overwritten.
    ///
    /// # Panics
    ///
    /// Panics if `width` is zero or does not divide `image.len()`, or if the
    /// image has more pixels than a `u32` can index.
    pub fn regional_maxima_into(
        &mut self,
        image: &[f32],
        out: &mut Vec<f32>,
        width: usize,
        threshold: f32,
    ) {
        out.clear();
        out.extend_from_slice(image);
        let height = check(out, image, width);

        punch_out_maxima(out, image, width, height, threshold);
        self.reconstruct(out, image, width);

        // `ip2.copyBits(ip, 0, 0, Blitter.DIFFERENCE)`, which for floats is the
        // absolute difference. The reconstruction can only have lowered the
        // holes relative to the original, so this is the drop from each maximum
        // to the saddle that refilled it.
        for (value, &original) in out.iter_mut().zip(image) {
            *value = (*value - original).abs();
        }
    }
}

/// Raises `marker` towards `mask` by geodesic dilation, in place.
///
/// `marker` and `mask` are row-major with the given `width`. Every pixel of
/// `marker` is raised as far as it can be by dilation without ever exceeding
/// `mask`, which is the standard grayscale reconstruction.
///
/// **The marker is only ever raised.** Reconstruction assumes `marker <= mask`
/// and does not check it: a pixel starting above its mask value is left exactly
/// where it is, and the result is then not a reconstruction of anything. QuPath
/// has `validateMarkerMask` for this and `morphologicalReconstruction` does not
/// call it, so the guarantee has to come from the caller. Both callers here
/// provide it structurally — [`Reconstructor::regional_maxima_into`] starts
/// from a copy of the mask, and an opening by reconstruction starts from an
/// erosion of it.
///
/// This allocates a queue per call. Callers reconstructing many tiles should
/// hold a [`Reconstructor`].
///
/// # Panics
///
/// Panics if `marker` and `mask` differ in length, if `width` is zero or does
/// not divide that length, or if the image has more pixels than a `u32` can
/// index.
pub fn morphological_reconstruction(marker: &mut [f32], mask: &[f32], width: usize) {
    Reconstructor::new().reconstruct(marker, mask, width);
}

/// Returns the prominence of every pixel of `image`.
///
/// See [`Reconstructor::regional_maxima_into`], which this calls; prefer that
/// when filtering more than one tile.
///
/// # Panics
///
/// Panics if `width` is zero or does not divide `image.len()`, or if the image
/// has more pixels than a `u32` can index.
#[must_use]
pub fn find_regional_maxima(image: &[f32], width: usize, threshold: f32) -> Vec<f32> {
    let mut out = Vec::new();
    Reconstructor::new().regional_maxima_into(image, &mut out, width, threshold);
    out
}

/// Shared shape validation. Returns the height.
fn check(a: &[f32], b: &[f32], width: usize) -> usize {
    assert_eq!(a.len(), b.len(), "marker and mask must be the same size");
    assert!(width > 0, "width must be non-zero");
    assert_eq!(a.len() % width, 0, "width must divide the buffer length");
    assert!(
        u32::try_from(a.len()).is_ok(),
        "image has more pixels than a u32 index can address"
    );
    a.len() / width
}

/// `getMaximaLabels`: sets every candidate maximum to negative infinity.
///
/// A candidate is a pixel at or above `threshold` that is `>=` all eight of its
/// neighbours. The test is deliberately non-strict, and the scan deliberately
/// skips the one-pixel border — QuPath's loops run `y1+1..y2-1`, so a peak
/// sitting on the image edge is never a candidate. Both are load-bearing and
/// both are pinned by the fixture.
fn punch_out_maxima(out: &mut [f32], image: &[f32], width: usize, height: usize, threshold: f32) {
    if width < 3 || height < 3 {
        // No interior, so QuPath's loops do not execute at all.
        return;
    }

    for y in 1..height - 1 {
        let row = y * width;
        let above = row - width;
        let below = row + width;
        // QuPath carries the horizontal neighbours along the row rather than
        // re-reading them, so that the two cheap rejections happen before any
        // of the six vertical loads.
        let mut value = image[row];
        let mut next = image[row + 1];
        for x in 1..width - 1 {
            let last = value;
            value = next;
            next = image[row + x + 1];
            if value < threshold || value < last || value < next {
                continue;
            }
            if value >= image[above + x - 1]
                && value >= image[above + x]
                && value >= image[above + x + 1]
                && value >= image[below + x - 1]
                && value >= image[below + x]
                && value >= image[below + x + 1]
            {
                out[row + x] = f32::NEG_INFINITY;
            }
        }
    }
}

/// One raster pass of `dilateAndCompare`, returning how many pixels moved.
///
/// Each pixel is raised to the largest of the four neighbours the scan has
/// already passed — the previous pixel in the row and the three above it in the
/// previous row — clipped to the mask. Running this forwards and then backwards
/// propagates along any path that does not double back, which is most of them.
///
/// When `queue` is given, a pixel is enqueued if one of those four neighbours is
/// now below it *and* below its own mask value, meaning the neighbour could
/// still rise and the raster order has already gone past it.
///
/// # Panics
///
/// The mask reads that decide enqueueing use the flat index QuPath computes,
/// which for a pixel in the first column of the scan leaves the row and lands
/// on the neighbouring one. That is what ImageJ's unchecked `getf(x, y)` does
/// and it is reproduced rather than corrected. The one index that would leave
/// the image entirely — before the first pixel — is unreachable, because that
/// pixel's four carried neighbours are all seeded from the pixel itself and so
/// it can never rise; a panic here would mean that reasoning is wrong.
fn dilate_and_compare(
    marker: &mut [f32],
    mask: &[f32],
    width: usize,
    height: usize,
    reverse: bool,
    mut queue: Option<&mut VecDeque<u32>>,
) -> usize {
    let stride = width as isize;
    let (inc, h_start, v_start, h_end, v_end) = if reverse {
        (-1isize, stride - 1, height as isize - 1, -1isize, -1isize)
    } else {
        (1isize, 0isize, 0isize, stride, height as isize)
    };

    let mut changes = 0usize;
    let mut first_row = true;
    let mut y = v_start;
    while y != v_end {
        let row = y * stride;
        let previous_row = (y - inc) * stride;

        // The four carried neighbours. On the first row of the scan they are
        // all seeded from the pixel itself, so they cannot raise it; in the
        // first column, `p1` falls outside and QuPath seeds it from `p2`.
        let mut value_previous = marker[at(row + h_start)];
        let (mut p1, mut p2) = if first_row {
            (value_previous, value_previous)
        } else {
            let above = marker[at(previous_row + h_start)];
            (above, above)
        };
        // Left uninitialised, as QuPath leaves it: every path through the loop
        // body assigns it before anything reads it.
        let mut p3;

        let mut first_column = true;
        let mut x = h_start;
        while x != h_end {
            let index = at(row + x);
            let mut value = marker[index];

            if first_row {
                p1 = value;
                p2 = value;
                p3 = value;
            } else if x + inc != h_end {
                p3 = marker[at(previous_row + x + inc)];
            } else {
                p3 = value;
            }

            // Written as QuPath writes it rather than with `f32::max`, because
            // the two disagree on NaN and the tie order is observable through
            // which value is carried forward.
            let mut neighbour_max = if p1 >= p2 { p1 } else { p2 };
            neighbour_max = if neighbour_max >= p3 {
                neighbour_max
            } else {
                p3
            };
            neighbour_max = if neighbour_max >= value_previous {
                neighbour_max
            } else {
                value_previous
            };

            if value < neighbour_max {
                let value_mask = mask[index];
                let value_new = if neighbour_max >= value_mask {
                    value_mask
                } else {
                    neighbour_max
                };
                if value_new > value {
                    value = value_new;
                    marker[index] = value;
                    changes += 1;
                }
            }

            if let Some(queue) = queue.as_deref_mut() {
                // The same four neighbours, asked whether any is now below this
                // pixel *and* below its own mask value — meaning it could still
                // rise and the scan has already gone past it. `||` short-
                // circuits exactly where QuPath's `else if` chain does, which
                // is load-bearing: several of these mask reads use a flat index
                // that lands outside the row it belongs to.
                let enqueue = (value_previous < value && value_previous < mask[at(row + x - inc)])
                    || (!first_row
                        && ((!first_column
                            && p1 < value
                            && p1 < mask[at(previous_row + x - inc)])
                            || (p2 < value && p2 < mask[at(previous_row + x)])
                            || (p3 < value && p3 < mask[at(previous_row + x + inc)])));
                if enqueue {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "image length checked to fit u32 in `check`"
                    )]
                    queue.push_back(index as u32);
                }
            }

            value_previous = value;
            p1 = p2;
            p2 = p3;
            first_column = false;
            x += inc;
        }
        first_row = false;
        y += inc;
    }
    changes
}

/// A flat pixel index, which the mask reads above may compute one step outside
/// the row they belong to. See [`dilate_and_compare`]'s panic note.
#[expect(
    clippy::cast_sign_loss,
    reason = "a negative index is the unreachable case the panic below reports"
)]
fn at(index: isize) -> usize {
    assert!(
        index >= 0,
        "reconstruction reached pixel {index}, before the start of the image"
    );
    index as usize
}

/// `processPoint`: raise one neighbour, and queue it if it moved.
fn process_point(
    marker: &mut [f32],
    mask: &[f32],
    index: usize,
    value: f32,
    queue: &mut VecDeque<u32>,
) {
    let current = marker[index];
    if current < value {
        let limit = mask[index];
        if current < limit {
            marker[index] = if limit <= value { limit } else { value };
            #[expect(
                clippy::cast_possible_truncation,
                reason = "image length checked to fit u32 in `check`"
            )]
            queue.push_back(index as u32);
        }
    }
}

/// Drains the queue, raising the eight neighbours of each pixel in turn.
///
/// This is what makes the result exact rather than merely close: the raster
/// passes settle everything that propagates monotonically in one direction, and
/// whatever is left — a path that doubles back on itself — is finished here.
fn process_queue(
    marker: &mut [f32],
    mask: &[f32],
    width: usize,
    height: usize,
    queue: &mut VecDeque<u32>,
) {
    while let Some(index) = queue.pop_front() {
        let index = index as usize;
        let x = index % width;
        let y = index / width;
        let value = marker[index];

        if x > 0 {
            process_point(marker, mask, index - 1, value, queue);
            if y > 0 {
                process_point(marker, mask, index - width - 1, value, queue);
            }
            if y < height - 1 {
                process_point(marker, mask, index + width - 1, value, queue);
            }
        }
        if x < width - 1 {
            process_point(marker, mask, index + 1, value, queue);
            if y > 0 {
                process_point(marker, mask, index - width + 1, value, queue);
            }
            if y < height - 1 {
                process_point(marker, mask, index + width + 1, value, queue);
            }
        }
        if y > 0 {
            process_point(marker, mask, index - width, value, queue);
        }
        if y < height - 1 {
            process_point(marker, mask, index + width, value, queue);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_floods_a_constant_mask() {
        let mask = vec![1.0f32; 25];
        let mut marker = vec![0.0f32; 25];
        marker[0] = 1.0;
        morphological_reconstruction(&mut marker, &mask, 5);
        assert_eq!(marker, mask, "nothing bounds the dilation here");
    }

    #[test]
    fn a_wall_of_zeros_stops_the_dilation() {
        // Reconstruction is geodesic: it travels only where the mask permits.
        // A port that dilates and then clips to the mask fills both halves.
        let width = 5;
        let mut mask = vec![1.0f32; width * 3];
        for y in 0..3 {
            mask[y * width + 2] = 0.0;
        }
        let mut marker = vec![0.0f32; width * 3];
        marker[0] = 1.0;
        morphological_reconstruction(&mut marker, &mask, width);
        assert_eq!(marker[1], 1.0, "the near side fills");
        assert_eq!(marker[3], 0.0, "the far side is unreachable");
    }

    #[test]
    fn a_marker_above_its_mask_is_left_alone() {
        // Not what a textbook reconstruction does, and not a mistake here:
        // QuPath's version only ever raises, and the clamp it does have —
        // `validateMarkerMask` — is a separate method it never calls. Pinned by
        // the fixture's `marker-above-mask`, which comes back untouched.
        let mask = vec![0.25f32; 9];
        let mut marker = vec![5.0f32; 9];
        morphological_reconstruction(&mut marker, &mask, 3);
        assert_eq!(marker, vec![5.0f32; 9]);
    }

    #[test]
    fn a_shoulder_has_no_prominence() {
        // The case that separates this from a plain maximum test: a terrace on
        // the side of a taller block is `>=` all its neighbours over its whole
        // interior, so every one of those pixels is a candidate, and every one
        // refills to its own value.
        let (width, height) = (11, 7);
        let mut image = vec![0.0f32; width * height];
        for y in 1..height - 1 {
            for x in 1..5 {
                image[y * width + x] = 0.5;
            }
            for x in 5..width - 1 {
                image[y * width + x] = 1.0;
            }
        }

        let prominence = find_regional_maxima(&image, width, 0.001);
        assert_eq!(
            prominence[3 * width + 2],
            0.0,
            "the terrace is not a maximum"
        );
        assert_eq!(
            prominence[3 * width + 7],
            0.5,
            "the block stands 0.5 above the terrace that reaches it"
        );
    }

    #[test]
    fn a_plateau_is_one_maximum_at_its_full_height() {
        let (width, height) = (9, 9);
        let mut image = vec![0.0f32; width * height];
        for y in 3..6 {
            for x in 3..6 {
                image[y * width + x] = 1.0;
            }
        }
        let prominence = find_regional_maxima(&image, width, 0.001);
        assert_eq!(prominence[4 * width + 4], 1.0);
        assert_eq!(
            prominence.iter().filter(|&&p| p > 0.0).count(),
            9,
            "all nine plateau pixels carry the prominence, and nothing else does"
        );
    }

    #[test]
    fn a_peak_below_the_threshold_is_not_a_maximum() {
        let (width, height) = (7, 7);
        let mut image = vec![0.0f32; width * height];
        image[3 * width + 3] = 0.0005;
        let prominence = find_regional_maxima(&image, width, 0.001);
        assert!(prominence.iter().all(|&p| p == 0.0));
    }

    #[test]
    fn a_peak_on_the_border_is_not_a_maximum() {
        // QuPath's scan starts at y1+1 and stops at y2-1, so the frame is never
        // examined. Reproduced deliberately; see the fixture's `border-peaks`.
        let (width, height) = (7, 7);
        let mut image = vec![0.0f32; width * height];
        image[0] = 1.0;
        image[3] = 1.0;
        let prominence = find_regional_maxima(&image, width, 0.001);
        assert!(prominence.iter().all(|&p| p == 0.0));
    }

    #[test]
    fn reconstruction_is_independent_of_the_traversal() {
        // The fixed point does not depend on the order pixels are visited, and
        // this asserts it rather than trusting the module note: the forward and
        // backward raster passes are run in the opposite order by transposing
        // the image, reconstructing, and transposing back.
        let (width, height) = (13, 9);
        let mask: Vec<f32> = (0..width * height)
            .map(|i| ((i * 37) % 23) as f32 / 23.0)
            .collect();
        let mut marker = vec![0.0f32; width * height];
        marker[6 * width + 6] = mask[6 * width + 6];

        let transpose = |src: &[f32]| -> Vec<f32> {
            let mut out = vec![0.0f32; src.len()];
            for y in 0..height {
                for x in 0..width {
                    out[x * height + y] = src[y * width + x];
                }
            }
            out
        };

        let mut direct = marker.clone();
        morphological_reconstruction(&mut direct, &mask, width);

        let mut rotated = transpose(&marker);
        morphological_reconstruction(&mut rotated, &transpose(&mask), height);
        let mut back = vec![0.0f32; rotated.len()];
        for y in 0..height {
            for x in 0..width {
                back[y * width + x] = rotated[x * height + y];
            }
        }

        assert_eq!(direct, back);
    }
}
