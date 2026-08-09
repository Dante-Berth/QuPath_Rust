//! Decomposition of ImageJ's circular kernel into a union of rectangles.
//!
//! A rank filter over a disc costs O(kernel area) done naively and O(kernel
//! height) with ImageJ's incremental scheme. Both grow with the radius. A
//! rectangle, by contrast, is *separable*: the max over a `w x h` box is the
//! max over `h` rows of the max over `w` columns, so it splits into two
//! one-dimensional passes that need no clamping arithmetic per pixel and
//! vectorise as plain slice-to-slice maxima.
//!
//! The disc splits into rectangles exactly, with no approximation, because its
//! half-width never increases with `|dy|`. For each distinct half-width `h`,
//! the rows whose half-width is at least `h` form one band centred on
//! `dy = 0`; that band, `h` wide either side, is a rectangle, and the union of
//! those rectangles over every distinct `h` is the disc.
//!
//! Concretely, radius 5 (89 pixels, 11 rows) becomes four rectangles —
//! 11x3, 9x7, 7x9 and 3x11 — and the filter reads each source row a constant
//! number of times instead of once per kernel row.

use crate::kernel::Kernel;

/// One rectangle of the decomposition: every offset with
/// `|dx| <= half_width` and `|dy| <= half_height`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    /// Horizontal extent either side of centre. Zero means a single column.
    pub(crate) half_width: usize,
    /// Vertical extent either side of centre. Zero means a single row.
    pub(crate) half_height: usize,
}

/// A kernel expressed as a union of rectangles, ordered by increasing height.
///
/// The order is what makes the vertical pass cheap: heights are nested, so a
/// single column accumulator grown from the centre outwards serves every
/// rectangle in turn, and each source row is folded in exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    rects: Vec<Rect>,
}

impl Plan {
    /// Decomposes the kernel ImageJ uses for `radius`.
    pub(crate) fn for_radius(radius: f64) -> Self {
        Self::from_kernel(&Kernel::new(radius))
    }

    /// Decomposes an explicit kernel.
    pub(crate) fn from_kernel(kernel: &Kernel) -> Self {
        let rows = kernel.radius_rows();

        let mut widths: Vec<i32> = (-rows..=rows)
            .map(|dy| kernel.half_width(dy))
            .filter(|&h| h >= 0)
            .collect();
        widths.sort_unstable();
        widths.dedup();
        // Descending half-width gives ascending half-height, which is the order
        // the vertical accumulator needs.
        widths.reverse();

        let rects = widths
            .into_iter()
            .map(|half_width| {
                let half_height = (-rows..=rows)
                    .filter(|&dy| kernel.half_width(dy) >= half_width)
                    .map(i32::abs)
                    .max()
                    .unwrap_or(0);
                Rect {
                    half_width: half_width.unsigned_abs() as usize,
                    half_height: half_height.unsigned_abs() as usize,
                }
            })
            .collect();

        Self { rects }
    }

    /// The rectangles, shortest first.
    pub(crate) fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Widest horizontal extent over all rectangles, for scratch sizing.
    pub(crate) fn max_half_width(&self) -> usize {
        self.rects.iter().map(|r| r.half_width).max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every offset the rectangles cover, as a sorted list.
    fn covered(plan: &Plan) -> Vec<(i32, i32)> {
        let mut points = Vec::new();
        for rect in plan.rects() {
            let hw = i32::try_from(rect.half_width).expect("kernel radii are small");
            let hh = i32::try_from(rect.half_height).expect("kernel radii are small");
            for dy in -hh..=hh {
                for dx in -hw..=hw {
                    points.push((dy, dx));
                }
            }
        }
        points.sort_unstable();
        points.dedup();
        points
    }

    /// Every offset the kernel itself contains, as a sorted list.
    fn kernel_points(kernel: &Kernel) -> Vec<(i32, i32)> {
        let mut points = Vec::new();
        for dy in -kernel.radius_rows()..=kernel.radius_rows() {
            let half = kernel.half_width(dy);
            if half < 0 {
                continue;
            }
            for dx in -half..=half {
                points.push((dy, dx));
            }
        }
        points.sort_unstable();
        points
    }

    #[test]
    fn rectangles_reconstruct_the_kernel_exactly() {
        // This is the correctness argument for the whole filter: if the union
        // of rectangles is not the disc, every filtered pixel is wrong. Checked
        // against `Kernel`, which is itself pinned to recorded ImageJ output.
        for radius in [0.0, 1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 7.5, 10.0, 20.0] {
            let kernel = Kernel::new(radius);
            let plan = Plan::from_kernel(&kernel);
            assert_eq!(
                covered(&plan),
                kernel_points(&kernel),
                "radius {radius} decomposed to the wrong shape"
            );
        }
    }

    #[test]
    fn heights_ascend_so_the_accumulator_only_grows() {
        for radius in [1.0, 2.0, 5.0, 10.0, 20.0] {
            let plan = Plan::for_radius(radius);
            let heights: Vec<usize> = plan.rects().iter().map(|r| r.half_height).collect();
            let mut sorted = heights.clone();
            sorted.sort_unstable();
            assert_eq!(heights, sorted, "radius {radius} is not ordered by height");
        }
    }

    #[test]
    fn radius_five_is_four_rectangles() {
        // 89 pixels over 11 rows, and the naive scan reads all 89 per pixel.
        // Four rectangles is the number that replaces it.
        let plan = Plan::for_radius(5.0);
        assert_eq!(
            plan.rects(),
            [
                Rect {
                    half_width: 5,
                    half_height: 1
                },
                Rect {
                    half_width: 4,
                    half_height: 3
                },
                Rect {
                    half_width: 3,
                    half_height: 4
                },
                Rect {
                    half_width: 1,
                    half_height: 5
                },
            ]
        );
    }
}
