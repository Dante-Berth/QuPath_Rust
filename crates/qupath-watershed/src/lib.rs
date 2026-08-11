//! Marker-controlled watershed matching QuPath's `qupath.imagej.processing.Watershed`.
//!
//! The algorithm is unchanged from QuPath: seed the queue with unlabelled
//! pixels adjacent to a label, then repeatedly take the highest-valued queued
//! pixel and adopt its neighbours' label if they agree. Two things differ, and
//! neither changes the result — see `docs/profiles/README.md` for the
//! measurement that motivated both:
//!
//! - the priority queue is a bucket queue rather than a comparison heap, which
//!   is the point of [`queue`];
//! - a pixel away from the image border reaches its neighbours by adding a
//!   precomputed flat offset, rather than by recovering `(x, y)` through an
//!   integer division and bounds-checking each shift.
//!
//! Neighbour order is preserved exactly, because it is what fixes FIFO
//! tie-breaking on plateaus and therefore where the watershed line falls.
//!
//! A pipeline flooding many tiles should hold a [`Watershed`] and call
//! [`Watershed::flood`], which reuses the per-pixel state and the queue instead
//! of rebuilding them per tile.
//!
//! ```
//! use qupath_watershed::{watershed, Connectivity};
//!
//! // Two markers on a ridge, flooded outward over the value image.
//! let width = 5;
//! let values = vec![
//!     0.0, 1.0, 2.0, 1.0, 0.0,
//!     0.0, 1.0, 2.0, 1.0, 0.0,
//! ];
//! let mut labels = vec![
//!     0.0, 0.0, 0.0, 0.0, 0.0,
//!     1.0, 0.0, 0.0, 0.0, 2.0,
//! ];
//! watershed(&values, &mut labels, width, f32::NEG_INFINITY, Connectivity::Four);
//! assert_eq!(labels[5], 1.0);
//! assert_eq!(labels[9], 2.0);
//! ```

mod edm;
mod label;
mod queue;
mod regions;

pub use edm::DistanceMap;
pub use label::{Labeller, label_image};
pub use queue::{HierarchicalQueue, LEVELS, PixelIndex};
pub use regions::{RegionTracer, filled_regions};

/// Neighbourhood used when propagating labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    /// Four-connected: the orthogonal neighbours.
    Four,
    /// Eight-connected: orthogonal and diagonal neighbours.
    Eight,
}

impl Connectivity {
    /// Neighbour offsets as `(dx, dy)`, in QuPath's insertion order.
    ///
    /// Order matters and is not arbitrary: it decides which neighbour is queued
    /// first, the queue breaks ties FIFO, and on a plateau that is what fixes
    /// where the watershed line falls. Getting this wrong is silent — the
    /// labelling stays plausible but differs from QuPath by a pixel.
    ///
    /// Four-connected follows `addNeighboursToQueue4` (up, left, right, down);
    /// eight-connected follows `addNeighboursToQueue8`, which is raster order
    /// over the 3×3 neighbourhood.
    #[must_use]
    fn offsets(self) -> &'static [(i32, i32)] {
        match self {
            Self::Four => &[(0, -1), (-1, 0), (1, 0), (0, 1)],
            Self::Eight => &[
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ],
        }
    }
}

/// Floods `labels` outward over `values`, in place.
///
/// `values` and `labels` are row-major with the given `width`. Non-zero entries
/// in `labels` are markers and are left untouched; zero entries are unlabelled.
/// Pixels whose value is `<= min_threshold` are never flooded, matching
/// QuPath's `minThreshold`.
///
/// A pixel adopts a label only when every labelled neighbour agrees; where two
/// labels meet, the pixel is left unlabelled, forming the watershed line.
///
/// # Panics
///
/// Panics if `values` and `labels` differ in length, or if `width` does not
/// divide that length — a caller passing mismatched buffers is a bug, not a
/// recoverable condition.
pub fn watershed(
    values: &[f32],
    labels: &mut [f32],
    width: usize,
    min_threshold: f32,
    connectivity: Connectivity,
) {
    Watershed::new().flood(values, labels, width, min_threshold, connectivity);
}

/// Pixel has been queued, or must never be queued. QuPath keeps the same flag
/// in a separate boolean array.
const QUEUED: u8 = 1;
/// Pixel lies on the image border, so a neighbour offset can leave the image
/// and the flood has to bounds-check it. Interior pixels skip that entirely.
const BORDER: u8 = 2;

/// A watershed that owns its working buffers, for repeated flooding.
///
/// A tile pipeline floods once per tile, and building the per-pixel state and
/// the queue's `LEVELS` level vectors per tile is allocation in the tile loop —
/// a defect under §6 of the project guidelines. Holding them here turns that
/// into a clear.
#[derive(Debug, Default)]
pub struct Watershed {
    /// Per-pixel [`QUEUED`] and [`BORDER`] flags.
    state: Vec<u8>,
    /// Reused across floods; see [`HierarchicalQueue::reset`].
    queue: Option<HierarchicalQueue>,
}

impl Watershed {
    /// Creates a watershed with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Floods `labels` outward over `values`, in place, reusing this instance's
    /// buffers. Semantics are identical to [`watershed`].
    ///
    /// # Panics
    ///
    /// Panics if `values` and `labels` differ in length, or if `width` does not
    /// divide that length.
    pub fn flood(
        &mut self,
        values: &[f32],
        labels: &mut [f32],
        width: usize,
        min_threshold: f32,
        connectivity: Connectivity,
    ) {
        assert_eq!(
            values.len(),
            labels.len(),
            "values and labels must describe the same image"
        );
        assert!(width > 0, "width must be non-zero");
        assert_eq!(
            values.len() % width,
            0,
            "width must divide the buffer length"
        );
        assert!(
            u32::try_from(values.len()).is_ok(),
            "image has more pixels than a u32 index can address"
        );

        let height = values.len() / width;

        self.state.clear();
        self.state.resize(values.len(), 0);

        let (min_value, max_value) = value_range(values, min_threshold);
        let queue = match &mut self.queue {
            Some(queue) => {
                queue.reset(min_value, max_value);
                queue
            }
            slot => slot.insert(HierarchicalQueue::new(min_value, max_value, values.len())),
        };

        seed(
            values,
            labels,
            width,
            height,
            min_threshold,
            &mut self.state,
            queue,
        );

        // Neighbour offsets as flat index deltas, in the same order as the
        // `(dx, dy)` pairs. The order is what fixes plateau tie-breaking, so it
        // must not change; only the arithmetic does.
        let pairs = connectivity.offsets();
        let mut flat = [0isize; 8];
        for (slot, &(dx, dy)) in flat.iter_mut().zip(pairs) {
            *slot = isize::try_from(dy).expect("offsets are +/-1") * width_step(width)
                + isize::try_from(dx).expect("offsets are +/-1");
        }
        let flat = &flat[..pairs.len()];

        while let Some(pixel) = queue.pop() {
            let index = pixel as usize;

            // Interior pixels — everything but a one-pixel frame, so all but
            // about 0.8% of a 512x512 tile — cannot have a neighbour outside
            // the image. They reach their neighbours by adding a precomputed
            // offset, skipping the `index / width` and `index % width` that the
            // coordinate form needs on every pop. Those are hardware integer
            // divisions by a runtime value, which the compiler cannot turn into
            // a multiply, and there is one of each per popped pixel.
            if self.state[index] & BORDER == 0 {
                let Some(label) = interior_label(labels, index, flat) else {
                    continue;
                };
                labels[index] = label;

                for &offset in flat {
                    let neighbour = index.wrapping_add_signed(offset);
                    if self.state[neighbour] & QUEUED != 0 {
                        continue;
                    }
                    self.state[neighbour] |= QUEUED;
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "image length checked to fit u32 above"
                    )]
                    queue.push(neighbour as PixelIndex, values[neighbour]);
                }
                continue;
            }

            let x = index % width;
            let y = index / width;

            let Some(label) = neighbour_label(labels, width, height, x, y, pairs) else {
                // Neighbours disagree: this pixel is on the watershed line.
                continue;
            };
            labels[index] = label;

            for &(dx, dy) in pairs {
                let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
                    continue;
                };
                let neighbour = ny * width + nx;
                if self.state[neighbour] & QUEUED != 0 {
                    continue;
                }
                self.state[neighbour] |= QUEUED;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "image length checked to fit u32 above"
                )]
                queue.push(neighbour as PixelIndex, values[neighbour]);
            }
        }
    }
}

/// `width` as a signed row stride.
fn width_step(width: usize) -> isize {
    isize::try_from(width).expect("width fits an isize on any supported target")
}

/// [`neighbour_label`] for a pixel known to have all its neighbours in bounds.
///
/// Identical in result to the coordinate form; it only reaches the neighbours
/// by flat offset instead of by `(x, y)` arithmetic with a bounds check each.
fn interior_label(labels: &[f32], index: usize, offsets: &[isize]) -> Option<f32> {
    let mut found: Option<f32> = None;
    for &offset in offsets {
        let label = labels[index.wrapping_add_signed(offset)];
        if label == 0.0 {
            continue;
        }
        match found {
            None => found = Some(label),
            Some(existing) if existing != label => return None,
            Some(_) => {}
        }
    }
    found
}

/// Returns the range of floodable values, used to size the quantiser.
fn value_range(values: &[f32], min_threshold: f32) -> (f32, f32) {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &v in values {
        if v <= min_threshold || !v.is_finite() {
            continue;
        }
        min = min.min(v);
        max = max.max(v);
    }
    if min > max {
        // No floodable pixel; the queue will stay empty, but it still needs a
        // usable range.
        return (0.0, 1.0);
    }
    (min, max)
}

/// Marks background and labelled pixels, and queues the initial flood front.
fn seed(
    values: &[f32],
    labels: &[f32],
    width: usize,
    height: usize,
    min_threshold: f32,
    state: &mut [u8],
    queue: &mut HierarchicalQueue,
) {
    for y in 0..height {
        let on_horizontal_edge = y == 0 || y + 1 == height;
        for x in 0..width {
            let index = y * width + x;

            // Marked once here rather than recomputed per pop; the flood reads
            // it to decide whether a neighbour can leave the image.
            if on_horizontal_edge || x == 0 || x + 1 == width {
                state[index] |= BORDER;
            }

            let value = values[index];

            // Below-threshold pixels are marked queued so they are never added
            // later, exactly as QuPath does.
            if value <= min_threshold {
                state[index] |= QUEUED;
                continue;
            }
            if labels[index] != 0.0 {
                state[index] |= QUEUED;
            } else if has_labelled_orthogonal_neighbour(labels, width, height, x, y) {
                state[index] |= QUEUED;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "length checked to fit u32 by the caller"
                )]
                queue.push(index as PixelIndex, value);
            }
        }
    }
}

/// Returns `true` if any orthogonal neighbour carries a label, treating
/// out-of-bounds neighbours as labelled.
///
/// Seeding is four-connected in QuPath regardless of the flood connectivity,
/// because the constructor tests only the four orthogonal neighbours.
///
/// The out-of-bounds rule is not a design choice; it reproduces QuPath. The
/// constructor uses `ImageProcessor.getPixelValue`, which returns **`NaN`**
/// outside a `FloatProcessor` — not `0` — and the test is `!= 0f`, which `NaN`
/// satisfies. Every border pixel is therefore seeded as though it bordered a
/// label. This is observable: on a flat plateau it makes the flood asymmetric,
/// and it is what the `plateau_four` parity fixture pins.
///
/// See `DIVERGENCES.md` D-003.
fn has_labelled_orthogonal_neighbour(
    labels: &[f32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
) -> bool {
    const ORTHOGONAL: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, -1), (0, 1)];
    ORTHOGONAL.iter().any(|&(dx, dy)| {
        shift(x, y, dx, dy, width, height).is_none_or(|(nx, ny)| labels[ny * width + nx] != 0.0)
    })
}

/// Returns the single label shared by all labelled neighbours.
///
/// `None` when neighbours carry differing labels, or when none is labelled —
/// both cases leave the pixel unlabelled, as in QuPath, where the disagreement
/// case returns `NaN` and the caller skips the pixel.
fn neighbour_label(
    labels: &[f32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    offsets: &[(i32, i32)],
) -> Option<f32> {
    let mut found: Option<f32> = None;
    for &(dx, dy) in offsets {
        let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
            continue;
        };
        let label = labels[ny * width + nx];
        if label == 0.0 {
            continue;
        }
        match found {
            None => found = Some(label),
            Some(existing) if existing != label => return None,
            Some(_) => {}
        }
    }
    found
}

/// Offsets `(x, y)` by `(dx, dy)`, returning `None` outside the image.
#[must_use]
fn shift(
    x: usize,
    y: usize,
    dx: i32,
    dy: i32,
    width: usize,
    height: usize,
) -> Option<(usize, usize)> {
    let nx = x.checked_add_signed(dx as isize)?;
    let ny = y.checked_add_signed(dy as isize)?;
    (nx < width && ny < height).then_some((nx, ny))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_markers_split_at_the_ridge() {
        // A valley between two markers: flooding from both sides should meet in
        // the middle and leave the ridge column unlabelled.
        let width = 5;
        #[rustfmt::skip]
        let values = vec![
            3.0, 2.0, 1.0, 2.0, 3.0,
            3.0, 2.0, 1.0, 2.0, 3.0,
            3.0, 2.0, 1.0, 2.0, 3.0,
        ];
        #[rustfmt::skip]
        let mut labels = vec![
            1.0, 0.0, 0.0, 0.0, 2.0,
            0.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        watershed(
            &values,
            &mut labels,
            width,
            f32::NEG_INFINITY,
            Connectivity::Four,
        );

        // Left half took label 1, right half label 2.
        assert_eq!(labels[0], 1.0);
        assert_eq!(labels[1], 1.0);
        assert_eq!(labels[4], 2.0);
        assert_eq!(labels[3], 2.0);
    }

    #[test]
    fn threshold_blocks_flooding() {
        let width = 4;
        let values = vec![5.0, 5.0, 0.5, 5.0, 5.0, 5.0, 0.5, 5.0];
        let mut labels = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        watershed(&values, &mut labels, width, 1.0, Connectivity::Four);

        // The low column is below threshold, so nothing crosses it.
        assert_eq!(labels[2], 0.0);
        assert_eq!(labels[3], 0.0);
        assert_eq!(labels[1], 1.0);
    }

    #[test]
    fn markers_are_not_overwritten() {
        let width = 3;
        let values = vec![1.0; 9];
        let mut labels = vec![0.0, 0.0, 0.0, 0.0, 7.0, 0.0, 0.0, 0.0, 0.0];
        watershed(
            &values,
            &mut labels,
            width,
            f32::NEG_INFINITY,
            Connectivity::Eight,
        );
        assert_eq!(labels[4], 7.0, "the marker itself must survive");
        assert!(
            labels.iter().all(|&l| l == 7.0),
            "one marker floods everything"
        );
    }

    #[test]
    fn empty_labels_leave_the_image_untouched() {
        let width = 3;
        let values = vec![1.0; 9];
        let mut labels = vec![0.0; 9];
        watershed(
            &values,
            &mut labels,
            width,
            f32::NEG_INFINITY,
            Connectivity::Four,
        );
        assert!(labels.iter().all(|&l| l == 0.0));
    }

    #[test]
    fn eight_connectivity_reaches_diagonals() {
        let width = 3;
        let values = vec![1.0; 9];
        let mut labels = vec![5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        watershed(
            &values,
            &mut labels,
            width,
            f32::NEG_INFINITY,
            Connectivity::Eight,
        );
        assert_eq!(labels[8], 5.0, "the far corner is reachable diagonally");
    }

    /// The flood written only in coordinate form — the path border pixels still
    /// take — so the interior fast path has something independent to agree with.
    fn flood_coordinates_only(
        values: &[f32],
        labels: &mut [f32],
        width: usize,
        min_threshold: f32,
        connectivity: Connectivity,
    ) {
        let height = values.len() / width;
        let mut state = vec![0u8; values.len()];
        let (min_value, max_value) = value_range(values, min_threshold);
        let mut queue = HierarchicalQueue::new(min_value, max_value, values.len());
        seed(
            values,
            labels,
            width,
            height,
            min_threshold,
            &mut state,
            &mut queue,
        );

        let pairs = connectivity.offsets();
        while let Some(pixel) = queue.pop() {
            let index = pixel as usize;
            let x = index % width;
            let y = index / width;

            let Some(label) = neighbour_label(labels, width, height, x, y, pairs) else {
                continue;
            };
            labels[index] = label;

            for &(dx, dy) in pairs {
                let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
                    continue;
                };
                let neighbour = ny * width + nx;
                if state[neighbour] & QUEUED != 0 {
                    continue;
                }
                state[neighbour] |= QUEUED;
                queue.push(
                    u32::try_from(neighbour).expect("test image is small"),
                    values[neighbour],
                );
            }
        }
    }

    /// An EDM-like surface with plateaus, and markers on a grid.
    fn plateau_image(width: usize, height: usize) -> (Vec<f32>, Vec<f32>) {
        let mut values = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let fx = (x % 16) as f32 - 8.0;
                let fy = (y % 16) as f32 - 8.0;
                // Floored, so equal values are common and tie-breaking matters.
                values.push(((256.0 - fx * fx - fy * fy).max(0.0) / 4.0).floor());
            }
        }

        let mut labels = vec![0.0f32; width * height];
        let mut next = 1.0;
        for y in (8..height).step_by(16) {
            for x in (8..width).step_by(16) {
                labels[y * width + x] = next;
                next += 1.0;
            }
        }
        (values, labels)
    }

    #[test]
    fn the_interior_fast_path_agrees_with_the_coordinate_path() {
        // The existing cases are a few pixels across, where almost every pixel
        // is on the border and the fast path barely runs. This one is 48x48, so
        // 92% of pixels take the interior path, and it has plateaus so the FIFO
        // tie-breaking that fixes the watershed lines is exercised too.
        let (width, height) = (48, 48);
        let (values, seeds) = plateau_image(width, height);

        for connectivity in [Connectivity::Four, Connectivity::Eight] {
            let mut fast = seeds.clone();
            watershed(&values, &mut fast, width, f32::NEG_INFINITY, connectivity);

            let mut slow = seeds.clone();
            flood_coordinates_only(&values, &mut slow, width, f32::NEG_INFINITY, connectivity);

            assert_eq!(fast, slow, "paths diverged for {connectivity:?}");
            assert!(
                fast.iter().filter(|&&l| l != 0.0).count() > width * height / 2,
                "the flood should have labelled most of the image"
            );
        }
    }

    #[test]
    fn a_reused_watershed_matches_a_fresh_one() {
        // The buffers survive between floods, so stale state leaking from one
        // image into the next is the failure mode this pins.
        let mut reused = Watershed::new();
        for &(width, height) in &[(48, 48), (16, 32), (48, 48)] {
            let (values, seeds) = plateau_image(width, height);
            for connectivity in [Connectivity::Four, Connectivity::Eight] {
                let mut from_reused = seeds.clone();
                reused.flood(
                    &values,
                    &mut from_reused,
                    width,
                    f32::NEG_INFINITY,
                    connectivity,
                );

                let mut fresh = seeds.clone();
                watershed(&values, &mut fresh, width, f32::NEG_INFINITY, connectivity);

                assert_eq!(from_reused, fresh, "{width}x{height} {connectivity:?}");
            }
        }
    }

    #[test]
    #[should_panic(expected = "same image")]
    fn mismatched_buffers_panic() {
        let mut labels = vec![0.0; 4];
        watershed(&[1.0; 5], &mut labels, 2, 0.0, Connectivity::Four);
    }
}
