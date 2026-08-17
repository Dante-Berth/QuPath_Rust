//! How long a scalebar should be, and what to write on it.
//!
//! Ported from QuPath's `Scalebar.java`, and one of the few pieces of
//! `qupath-gui-fx` that genuinely *is* portable: the rule for picking a bar
//! length is arithmetic over a fixed table, with no JavaFX anywhere in it. What
//! is not ported is the drawing — a `Canvas`, a `Label` and an `AnchorPane`
//! there, and the caller's business here.
//!
//! The rule, from `Scalebar.updateScalebar`: take a preferred length in *screen*
//! pixels, convert it to microns at the current downsample, snap that to the
//! nearest entry in a table of round numbers, and convert back to find how long
//! the bar is on screen. Snapping in microns rather than in pixels is what makes
//! the label a round number, which is the whole point of a scalebar — the bar is
//! whatever length "50 µm" happens to be, not the other way round.

/// The lengths a scalebar is allowed to claim, in microns.
///
/// QuPath's `permittedScales`, copied exactly rather than generated. A 1-2-5
/// progression would be the obvious way to produce something like this and would
/// be wrong: the table also carries 250, 400 and 800, so a generated sequence
/// would disagree with QuPath at those scales and the disagreement would show up
/// as a differently-labelled bar on the same slide.
const PERMITTED: [f64; 22] = [
    0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 250.0, 400.0, 500.0, 800.0,
    1000.0, 2000.0, 5000.0, 10000.0, 20000.0, 50000.0, 100000.0,
];

/// QuPath's default preferred length, in screen pixels.
///
/// `new Scalebar(viewer)` delegates to `new Scalebar(viewer, 100)`.
pub const PREFERRED_PIXELS: f64 = 100.0;

/// A scalebar to draw: how long it is on screen, and what it says.
#[derive(Debug, Clone, PartialEq)]
pub struct Scalebar {
    /// How wide the bar is, in screen pixels.
    ///
    /// Not the preferred length: it is whatever the snapped round number works
    /// out to, so it is near the preference rather than equal to it.
    pub pixels: f64,
    /// The label, already formatted with its unit.
    pub label: String,
}

/// The nearest permitted length to `length`, in microns.
///
/// Nearest in absolute difference, scanning the table in order, which is
/// QuPath's `getPermittedScale` — and it means a length exactly between two
/// entries takes the *smaller*, because a later equal difference does not
/// replace an earlier one.
fn permitted(length: f64) -> f64 {
    let mut best = f64::NAN;
    let mut smallest = f64::INFINITY;
    for candidate in PERMITTED {
        let difference = (candidate - length).abs();
        if difference < smallest {
            best = candidate;
            smallest = difference;
        }
    }
    best
}

/// Formats a length the way QuPath's `DecimalFormat("#.##")` does.
///
/// Up to two decimal places, with trailing zeros and a trailing point removed,
/// so 0.25 stays `0.25` where 50.0 is `50` rather than `50.00`.
fn format(value: f64) -> String {
    let text = format!("{value:.2}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_owned()
}

/// The scalebar for a view, or `None` if the slide has no pixel size.
///
/// `microns_per_pixel` is the slide's level-0 pixel size and `scale` is the
/// viewport's screen pixels per level-0 pixel. `None` rather than a bar labelled
/// in pixels: QuPath falls back to a `px` unit here, but this crate's callers
/// have `Option<f64>` from `Slide::microns_per_pixel` and a bar that silently
/// changes what it measures is worse than no bar. The caller can say so.
///
/// Returns `None` too for a non-finite or non-positive scale, which is what a
/// viewport reports before the first frame has sized the window.
#[must_use]
pub fn scalebar(microns_per_pixel: Option<f64>, scale: f64) -> Option<Scalebar> {
    let microns_per_pixel = microns_per_pixel?;
    if !scale.is_finite() || scale <= 0.0 || !microns_per_pixel.is_finite() {
        return None;
    }
    // QuPath works in `downsample`, level-0 pixels per screen pixel, where the
    // viewport holds its reciprocal.
    let downsample = 1.0 / scale;
    // Microns per *screen* pixel, which is what the bar is measured in.
    let per_pixel = microns_per_pixel * downsample;
    let microns = permitted(per_pixel * PREFERRED_PIXELS);
    let pixels = microns / per_pixel;
    // Millimetres above 1000 µm, as QuPath does, because "2000 µm" is a number
    // nobody reads as two millimetres.
    let label = if microns >= 1000.0 {
        format!("{} mm", format(microns / 1000.0))
    } else {
        format!("{} µm", format(microns))
    };
    Some(Scalebar { pixels, label })
}

#[cfg(test)]
mod tests {
    use super::{PERMITTED, PREFERRED_PIXELS, format, permitted, scalebar};

    #[test]
    fn a_length_snaps_to_the_nearest_permitted_one() {
        assert_eq!(permitted(48.0), 50.0);
        assert_eq!(permitted(0.11), 0.1);
        assert_eq!(permitted(90.0), 100.0);
        // Inside the irregular part of the table, which a 1-2-5 sequence would
        // get wrong: 300 is nearer 250 than 400, and 420 nearer 400 than 500.
        assert_eq!(permitted(300.0), 250.0);
        assert_eq!(permitted(420.0), 400.0);
        assert_eq!(permitted(700.0), 800.0);
    }

    #[test]
    fn a_length_past_the_table_takes_its_end() {
        assert_eq!(permitted(0.0), 0.1);
        assert_eq!(permitted(1.0e9), 100_000.0);
    }

    /// The bar is near the preferred length, which is the property that makes it
    /// usable: it must not come out a few pixels wide or wider than the window.
    ///
    /// Swept across every scale a viewer reaches on a 0.499 µm/px slide, because
    /// the snapping is what could break this and the table's gaps are uneven —
    /// between 800 and 1000 the ratio is 1.25, between 0.1 and 0.25 it is 2.5.
    ///
    /// The lower bound is `0.005`, which is a quarter of the scale `Viewport::fit`
    /// picks for `CMU-1.svs` in a 1024px window — so the sweep covers zooming
    /// well past a whole slide on screen, and stops short of where the table
    /// runs out. `the_table_runs_out_past_any_real_slide` is that limit.
    #[test]
    fn the_bar_stays_near_the_preferred_length_at_every_reachable_scale() {
        let mpp = 0.4990;
        let mut scale = 8.0;
        while scale > 0.005 {
            let bar = scalebar(Some(mpp), scale).expect("a slide with a pixel size has a bar");
            assert!(
                bar.pixels > PREFERRED_PIXELS / 4.0 && bar.pixels < PREFERRED_PIXELS * 4.0,
                "at scale {scale} the bar is {} px for {}",
                bar.pixels,
                bar.label
            );
            scale /= 1.2;
        }
    }

    /// Past the end of the table the bar shrinks, and that is QuPath's behaviour
    /// rather than a defect in the port.
    ///
    /// `permittedScales` stops at 100,000 µm — 10 cm, wider than any slide — so
    /// zoomed out far enough the bar cannot grow and starts under-reporting its
    /// share of the window. Pinned rather than fixed: extending the table would
    /// be diverging from QuPath to serve a scale no slide reaches, and finding
    /// this by sweeping past the reachable range is exactly why the sweep above
    /// is bounded deliberately rather than by taste.
    #[test]
    fn the_table_runs_out_past_any_real_slide() {
        // 100 mm of slide across a 1024px window: a metre-wide specimen.
        let bar = scalebar(Some(0.4990), 0.0001).expect("a bar");
        assert_eq!(bar.label, "100 mm", "the table's last entry is 100,000 µm");
        assert!(
            bar.pixels < PREFERRED_PIXELS / 4.0,
            "the table no longer runs out at this scale: {} px",
            bar.pixels
        );
    }

    /// Zooming out must never *shorten* the bar's claim.
    ///
    /// The invariant that says the arithmetic is the right way up. An inverted
    /// downsample would still produce plausible round numbers at every single
    /// scale — 50 µm, 20 µm — and only a comparison across two scales catches
    /// it. That is `docs/VIEWER.md` §3.3's level bug in miniature.
    #[test]
    fn zooming_out_never_claims_a_shorter_length() {
        let mpp = 0.4990;
        let mut previous = 0.0;
        let mut scale = 8.0;
        while scale > 1.0e-6 {
            let bar = scalebar(Some(mpp), scale).expect("a bar");
            let microns = claimed(&bar.label);
            assert!(
                microns >= previous,
                "zooming out from {previous} µm claimed {microns} µm at scale {scale}"
            );
            previous = microns;
            scale /= 1.2;
        }
    }

    /// The length a label claims, in microns, parsed back out of it.
    fn claimed(label: &str) -> f64 {
        let (value, unit) = label.split_once(' ').expect("a label has a unit");
        let value: f64 = value.parse().expect("a label's value is a number");
        match unit {
            "mm" => value * 1000.0,
            "µm" => value,
            other => panic!("unexpected unit {other}"),
        }
    }

    #[test]
    fn millimetres_take_over_above_a_thousand_microns() {
        // 1000 µm is the boundary and belongs to mm, as QuPath's `>=` has it.
        assert!(label_at(1000.0).ends_with(" mm"));
        assert_eq!(label_at(1000.0), "1 mm");
        assert_eq!(label_at(2000.0), "2 mm");
        assert!(label_at(800.0).ends_with(" µm"));
    }

    /// The label for a bar whose snapped length is `microns`, found by choosing
    /// a scale that lands on it.
    fn label_at(microns: f64) -> String {
        // One micron per screen pixel makes the preferred length 100 µm, so
        // scaling the pixel size puts the snapped length wherever is wanted.
        let mpp = microns / PREFERRED_PIXELS;
        scalebar(Some(mpp), 1.0).expect("a bar").label
    }

    #[test]
    fn a_slide_with_no_pixel_size_has_no_bar() {
        assert!(scalebar(None, 1.0).is_none());
    }

    /// A viewport that has not been sized yet must not produce a bar.
    ///
    /// `0.0` would divide to an infinite downsample and NaN its way into a
    /// label; `docs/VIEWER.md` §8.1b records what a NaN reaching the viewport
    /// cost, and this is the same refusal applied at the other end.
    #[test]
    fn a_degenerate_scale_has_no_bar() {
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                scalebar(Some(0.4990), scale).is_none(),
                "a scale of {scale} produced a bar"
            );
        }
    }

    #[test]
    fn a_label_drops_the_zeros_a_round_number_does_not_need() {
        assert_eq!(format(50.0), "50");
        assert_eq!(format(0.25), "0.25");
        assert_eq!(format(0.5), "0.5");
        assert_eq!(format(0.1), "0.1");
    }

    /// Every entry in the table must format to something a person would read.
    #[test]
    fn every_permitted_length_has_a_clean_label() {
        for length in PERMITTED {
            let text = format(length);
            assert!(!text.contains(".00"), "{length} formatted as {text}");
            assert!(!text.ends_with('.'), "{length} formatted as {text}");
        }
    }
}
