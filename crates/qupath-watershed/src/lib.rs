//! Marker-controlled watershed matching QuPath's `qupath.imagej.processing.Watershed`.
//!
//! The algorithm is unchanged from QuPath: seed the queue with unlabelled
//! pixels adjacent to a label, then repeatedly take the highest-valued queued
//! pixel and adopt its neighbours' label if they agree. Only the priority queue
//! differs — see [`queue`] for why, and `docs/profiles/README.md` for the
//! measurement that motivated it.
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

mod queue;

pub use queue::{HierarchicalQueue, LEVELS, PixelIndex};

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

    let height = values.len() / width;

    // `queued` marks pixels that must never enter the queue: below threshold,
    // already labelled, or queued once already. QuPath keeps the same array.
    let mut queued = vec![false; values.len()];

    let (min_value, max_value) = value_range(values, min_threshold);
    let mut queue = HierarchicalQueue::new(min_value, max_value, values.len());

    seed(
        values,
        labels,
        width,
        height,
        min_threshold,
        &mut queued,
        &mut queue,
    );

    let offsets = connectivity.offsets();
    while let Some(pixel) = queue.pop() {
        let index = pixel as usize;
        let x = index % width;
        let y = index / width;

        let Some(label) = neighbour_label(labels, width, height, x, y, offsets) else {
            // Neighbours disagree: this pixel is on the watershed line.
            continue;
        };
        labels[index] = label;

        for &(dx, dy) in offsets {
            let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
                continue;
            };
            let neighbour = ny * width + nx;
            if queued[neighbour] {
                continue;
            }
            queued[neighbour] = true;
            // The cast is bounded by the length assertion in `seed`.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "image length is checked to fit u32 in seed"
            )]
            queue.push(neighbour as PixelIndex, values[neighbour]);
        }
    }
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
    queued: &mut [bool],
    queue: &mut HierarchicalQueue,
) {
    assert!(
        u32::try_from(values.len()).is_ok(),
        "image has more pixels than a u32 index can address"
    );

    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let value = values[index];

            // Below-threshold pixels are marked queued so they are never added
            // later, exactly as QuPath does.
            if value <= min_threshold {
                queued[index] = true;
                continue;
            }
            if labels[index] != 0.0 {
                queued[index] = true;
            } else if has_labelled_orthogonal_neighbour(labels, width, height, x, y) {
                queued[index] = true;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "length checked to fit u32 above"
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

    #[test]
    #[should_panic(expected = "same image")]
    fn mismatched_buffers_panic() {
        let mut labels = vec![0.0; 4];
        watershed(&[1.0; 5], &mut labels, 2, 0.0, Connectivity::Four);
    }
}
