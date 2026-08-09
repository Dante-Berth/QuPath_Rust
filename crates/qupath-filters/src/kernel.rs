//! The circular kernel ImageJ's `RankFilters` builds for a given radius.
//!
//! The shape is not the naive disc, and getting it wrong shifts every filtered
//! pixel. The rule below is derived from output recorded from a real ImageJ
//! build (`parity/fixtures/RankFilterMinMax.json`), then checked against every
//! recorded radius, rather than read off the source.

/// Per-row half-width of the kernel, indexed from the top row.
///
/// Row `i` covers `dy = i - radius_rows`, and spans `dx` in
/// `-half_widths[i] ..= half_widths[i]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kernel {
    /// Half-width per row, top to bottom. Always an odd number of rows.
    half_widths: Vec<i32>,
    /// Vertical extent: rows run `-radius_rows ..= radius_rows`.
    radius_rows: i32,
}

impl Kernel {
    /// Builds the kernel ImageJ uses for `radius`.
    ///
    /// ImageJ nudges two radius bands before squaring — `[1.5, 1.75)` becomes
    /// 1.75 and `[2.5, 2.85)` becomes 2.85 — so that those kernels gain a ring
    /// of pixels rather than staying identical to the band below. It then uses
    /// `r² + 1` as the squared cutoff, which is why radius 1 gives a full 3×3
    /// square rather than a plus shape.
    ///
    /// Both adjustments are observable in the recorded fixtures: without them,
    /// radii 1.5 and 2.5 produce 9 and 21 kernel pixels instead of the 13 and
    /// 29 ImageJ actually produces.
    #[must_use]
    pub fn new(radius: f64) -> Self {
        let radius = if (1.5..1.75).contains(&radius) {
            1.75
        } else if (2.5..2.85).contains(&radius) {
            2.85
        } else {
            radius
        };

        let r2 = radius * radius + 1.0;
        // The epsilon guards against a value landing a hair under an integer
        // after the squaring, which would drop a whole row or column.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "kernel radii are small and bounded by the caller's radius"
        )]
        let radius_rows = (r2 + 1e-10).sqrt() as i32;

        let mut half_widths = Vec::with_capacity((radius_rows * 2 + 1) as usize);
        for dy in -radius_rows..=radius_rows {
            let remaining = r2 - f64::from(dy) * f64::from(dy);
            let half = if remaining < 0.0 {
                -1
            } else {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "bounded by radius_rows above"
                )]
                let h = (remaining + 1e-10).sqrt() as i32;
                h
            };
            half_widths.push(half);
        }

        Self {
            half_widths,
            radius_rows,
        }
    }

    /// Vertical extent in pixels; rows run `-radius_rows() ..= radius_rows()`.
    #[must_use]
    pub fn radius_rows(&self) -> i32 {
        self.radius_rows
    }

    /// Half-width of the row at `dy`, or `-1` where the row is empty.
    #[must_use]
    pub fn half_width(&self, dy: i32) -> i32 {
        let index = (dy + self.radius_rows) as usize;
        self.half_widths.get(index).copied().unwrap_or(-1)
    }

    /// Number of pixels in the kernel, for tests and sizing.
    #[must_use]
    pub fn area(&self) -> usize {
        self.half_widths
            .iter()
            .filter(|&&h| h >= 0)
            .map(|&h| (h * 2 + 1) as usize)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_one_is_a_full_three_by_three() {
        // r^2 + 1 = 2, so dy = ±1 still admits dx = ±1: a square, not a plus.
        let k = Kernel::new(1.0);
        assert_eq!(k.radius_rows(), 1);
        assert_eq!(k.area(), 9);
    }

    #[test]
    fn imagej_radius_adjustments_change_the_area() {
        // These are the two bands ImageJ nudges. Without the adjustment they
        // would collapse onto the kernel of the band below.
        assert_eq!(Kernel::new(1.5).area(), 13);
        assert_eq!(Kernel::new(1.0).area(), 9);
        assert_eq!(Kernel::new(2.5).area(), 29);
        assert_eq!(Kernel::new(2.0).area(), 21);
    }

    #[test]
    fn areas_match_the_recorded_kernels() {
        // Counted from the impulse-response cases in the parity fixture.
        assert_eq!(Kernel::new(2.0).area(), 21);
        assert_eq!(Kernel::new(3.0).area(), 37);
        assert_eq!(Kernel::new(5.0).area(), 89);
    }

    #[test]
    fn rows_are_symmetric() {
        let k = Kernel::new(4.0);
        for dy in 1..=k.radius_rows() {
            assert_eq!(k.half_width(dy), k.half_width(-dy));
        }
    }

    #[test]
    fn out_of_range_rows_are_empty() {
        let k = Kernel::new(2.0);
        assert_eq!(k.half_width(k.radius_rows() + 1), -1);
    }
}
