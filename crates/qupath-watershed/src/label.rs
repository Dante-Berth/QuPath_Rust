//! Connected-component labelling, matching QuPath's `RoiLabeling.labelImage`.
//!
//! This is the call between the regional maxima and the flood:
//!
//! ```java
//! ImageProcessor ipNucleusLabels = RoiLabeling.labelImage(ipTemp, 0, false);
//! Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false);
//! ```
//!
//! `labelImage` selects every pixel **strictly above** the threshold and gives
//! each connected group of them one label, numbered from 1 in the raster order
//! of the first pixel of each group. That numbering is not cosmetic: it is what
//! the watershed floods outward, so a different order is a different labelling
//! of the same segmentation.
//!
//! # The connectivity is 4, and it disagrees with the stage before it
//!
//! `false` is `conn8`. The maxima that produced these pixels were found with an
//! **8**-connected test, so two maxima touching only at a corner arrive here as
//! one diagonal group and leave as two markers. Nothing at the call site says
//! so, and taking the connectivity from the stage above — which is the natural
//! assumption, since the two run back to back — merges them into one. Pinned by
//! `diagonal-touch` in `parity/fixtures/RegionalMaxima.json`.

use crate::Connectivity;

/// Marks a selected pixel that has not yet been reached by a flood fill.
///
/// Labels are positive, so a negative sentinel cannot collide with one. QuPath
/// does the same thing with `65535` in the label image rather than carrying a
/// second buffer, and the reason to copy that here is the same: the alternative
/// is a `Vec<bool>` the size of the tile, allocated in the tile loop.
const PENDING: f32 = -1.0;

/// A labeller that owns its working stack, for repeated use.
///
/// A tile pipeline labels once per tile; rebuilding the stack per tile is
/// allocation in the tile loop, a defect under §6 of the project guidelines.
#[derive(Debug, Default)]
pub struct Labeller {
    stack: Vec<u32>,
}

impl Labeller {
    /// Creates a labeller with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Labels the connected components of `values > threshold` into `labels`,
    /// returning how many were found.
    ///
    /// `labels` is fully overwritten: every pixel gets either its component's
    /// number, counting from 1, or zero. It is sized by the caller and must
    /// match `values`.
    ///
    /// # Panics
    ///
    /// Panics if `values` and `labels` differ in length, if `width` is zero or
    /// does not divide that length, or if the image has more pixels than a
    /// `u32` can index.
    pub fn label_into(
        &mut self,
        values: &[f32],
        labels: &mut [f32],
        width: usize,
        threshold: f32,
        connectivity: Connectivity,
    ) -> u32 {
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

        // `> threshold`, not `>=`: QuPath passes `d -> d > threshold` and calls
        // it with zero, so a pixel of exactly zero prominence — every pixel
        // that is not a maximum, and every maximum the reconstruction refilled
        // to its own height — is background.
        for (label, &value) in labels.iter_mut().zip(values) {
            *label = if value > threshold { PENDING } else { 0.0 };
        }

        let mut found = 0u32;
        for start in 0..labels.len() {
            if labels[start] != PENDING {
                continue;
            }
            found += 1;
            #[expect(
                clippy::cast_precision_loss,
                reason = "labels are exact in f32 to 2^24, far beyond the 65535 QuPath \
                          can hold in its ShortProcessor before it widens"
            )]
            let label = found as f32;

            labels[start] = label;
            self.stack.clear();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "image length checked to fit u32 above"
            )]
            self.stack.push(start as u32);

            while let Some(index) = self.stack.pop() {
                let index = index as usize;
                let x = index % width;
                let y = index / width;
                for &(dx, dy) in connectivity.offsets() {
                    let Some((nx, ny)) = shift(x, y, dx, dy, width, height) else {
                        continue;
                    };
                    let neighbour = ny * width + nx;
                    if labels[neighbour] != PENDING {
                        continue;
                    }
                    labels[neighbour] = label;
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "image length checked to fit u32 above"
                    )]
                    self.stack.push(neighbour as u32);
                }
            }
        }
        found
    }
}

/// Labels the connected components of `values > threshold`, returning how many
/// were found.
///
/// See [`Labeller::label_into`], which this calls; prefer that when labelling
/// more than one tile.
///
/// # Panics
///
/// Panics under the same conditions as [`Labeller::label_into`].
pub fn label_image(
    values: &[f32],
    labels: &mut [f32],
    width: usize,
    threshold: f32,
    connectivity: Connectivity,
) -> u32 {
    Labeller::new().label_into(values, labels, width, threshold, connectivity)
}

/// Offsets a coordinate, returning `None` outside the image.
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
    fn components_are_numbered_in_raster_order() {
        // The numbering is what the watershed floods, so it is part of the
        // answer rather than an implementation detail.
        let width = 5;
        let values = vec![
            0.0, 0.0, 0.0, 1.0, 0.0, //
            0.0, 0.0, 0.0, 0.0, 0.0, //
            1.0, 0.0, 0.0, 0.0, 0.0,
        ];
        let mut labels = vec![0.0f32; values.len()];
        assert_eq!(
            label_image(&values, &mut labels, width, 0.0, Connectivity::Four),
            2
        );
        assert_eq!(
            labels[3], 1.0,
            "the earlier pixel in raster order is label 1"
        );
        assert_eq!(labels[10], 2.0);
    }

    #[test]
    fn a_corner_touch_is_two_components_when_four_connected() {
        let width = 4;
        let values = vec![
            0.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0,
        ];
        let mut labels = vec![0.0f32; values.len()];
        assert_eq!(
            label_image(&values, &mut labels, width, 0.0, Connectivity::Four),
            2,
            "QuPath labels the maxima 4-connected, having found them 8-connected"
        );
        assert_eq!(
            label_image(&values, &mut labels, width, 0.0, Connectivity::Eight),
            1
        );
    }

    #[test]
    fn the_threshold_is_strict() {
        // `d -> d > threshold`, called with zero. A prominence of exactly zero
        // is a maximum the reconstruction refilled to its own height, and it
        // must not become a marker.
        let values = vec![0.0f32; 9];
        let mut labels = vec![0.0f32; 9];
        assert_eq!(
            label_image(&values, &mut labels, 3, 0.0, Connectivity::Four),
            0
        );
    }

    #[test]
    fn a_reused_labeller_matches_a_fresh_one() {
        let width = 7;
        let values: Vec<f32> = (0..width * 5)
            .map(|i| if (i * 7) % 5 < 2 { 1.0 } else { 0.0 })
            .collect();

        let mut fresh = vec![0.0f32; values.len()];
        let expected = label_image(&values, &mut fresh, width, 0.0, Connectivity::Four);

        let mut labeller = Labeller::new();
        let mut reused = vec![0.0f32; values.len()];
        labeller.label_into(&[1.0f32; 3], &mut [0.0f32; 3], 3, 0.0, Connectivity::Four);
        let count = labeller.label_into(&values, &mut reused, width, 0.0, Connectivity::Four);

        assert_eq!(count, expected);
        assert_eq!(reused, fresh);
    }
}
