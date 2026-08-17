//! What to do to a pixel between reading it and showing it.
//!
//! `docs/VIEWER.md` §8.2's third item. QuPath's brightness/contrast dialog
//! adjusts **display only** — it never changes what a detection reads — and
//! separately shows the deconvolved haematoxylin and eosin channels instead of
//! the original colour. Both are one operation on one pixel, which is what this
//! module is.
//!
//! # Display only, and why that has to be enforced rather than intended
//!
//! A brightness control that reached the pipeline would silently change every
//! count this project has recorded, and it would do so in a way no test
//! comparing counts to counts would notice — the parity fixtures would simply
//! start disagreeing with a slider position. The separation here is structural:
//! this module is in `qupath-viewer`, the detection path is in
//! `qupath-pipeline`, and nothing in the pipeline can see a [`Display`] because
//! the dependency only runs the other way. `ROADMAP.md` §5's rule about not
//! asserting on appearance has a mirror image, which is that appearance must
//! not be able to move an assertion.
//!
//! # Why the stain vectors arrive as numbers
//!
//! `qupath-pipeline::stain` already computes the deconvolution and this module
//! does not call it, for the reason `docs/VIEWER.md` §7 gives about
//! [`crate::Overlay`]: the library depends on `qupath-slide` alone, and taking
//! a `Stains` would put the whole pipeline into every build of the viewer to
//! reuse nine numbers and a logarithm. [`Display::with_stain`] takes the
//! coefficient row and the background, which is what `Stains::coefficients` and
//! its max values happen to hold, and would take them just as happily from a
//! vector typed in by hand — which is precisely what QuPath's dialog lets a
//! user do.
//!
//! # The transform
//!
//! From `AbstractChannelInfo` and `RGBDirectChannelInfo`: an offset and a
//! scale, applied per channel and clamped to a byte.
//!
//! ```text
//! offset = minDisplay
//! scale  = 255 / (maxDisplay - minDisplay)
//! out    = clamp((in - offset) * scale, 0, 255)
//! ```
//!
//! Ported rather than re-derived, because "brightness and contrast" has a dozen
//! reasonable definitions and the one that matters is the one that makes this
//! viewer and QuPath agree on the same slide at the same settings.

/// How a pixel is shown: the display range, and optionally a stain to isolate.
///
/// The default shows the slide as it was read — a full 0–255 range and no
/// stain — so a [`Display::default()`] costs nothing and changes nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Display {
    /// The value shown as black. QuPath's `minDisplay`.
    min: f32,
    /// The value shown as white. QuPath's `maxDisplay`.
    max: f32,
    /// The stain to isolate, if any.
    stain: Option<Stain>,
}

/// One stain's deconvolution, as the display path needs it.
///
/// `coefficients` is one row of the inverted stain matrix — the row
/// `Stains::coefficients()` returns for that stain — and `background` is the
/// per-channel white the optical density is measured against, QuPath's
/// `maxValues`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stain {
    /// The stain's row of the deconvolution matrix, per channel.
    pub coefficients: [f64; 3],
    /// The white point per channel, normally `[255, 255, 255]`.
    pub background: [f64; 3],
    /// The optical density shown as full black.
    ///
    /// Separate from [`Display::max`] because a stain amount is not a byte: it
    /// is an optical density, typically 0 to about 2, and reusing the 0–255
    /// range for it would put every real value in the bottom one percent of the
    /// scale. QuPath keeps a separate display range per channel for the same
    /// reason.
    pub full: f64,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 255.0,
            stain: None,
        }
    }
}

impl Display {
    /// The identity display: the slide as read.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The display showing `min` as black and `max` as white.
    ///
    /// `None` when the range is empty or not finite. An inverted or zero-width
    /// range makes `scale` infinite or negative, and a NaN reaching a pixel is
    /// the same class of bug as the NaN [`crate::scalebar`] refuses — except
    /// that here it would show as a frame of noise rather than a bad label, and
    /// be blamed on the decoder.
    #[must_use]
    pub fn range(min: f32, max: f32) -> Option<Self> {
        if !min.is_finite() || !max.is_finite() || max <= min {
            return None;
        }
        Some(Self {
            min,
            max,
            stain: None,
        })
    }

    /// The same display, isolating one stain.
    #[must_use]
    pub fn with_stain(self, stain: Stain) -> Self {
        Self {
            stain: Some(stain),
            ..self
        }
    }

    /// The same display, showing the original colour again.
    #[must_use]
    pub fn without_stain(self) -> Self {
        Self {
            stain: None,
            ..self
        }
    }

    /// Which stain is being shown, if any.
    #[must_use]
    pub fn stain(self) -> Option<Stain> {
        self.stain
    }

    /// The display range, as `(min, max)`.
    #[must_use]
    pub fn limits(self) -> (f32, f32) {
        (self.min, self.max)
    }

    /// Whether this changes anything at all.
    ///
    /// The fast path: [`Display::is_identity`] lets a caller skip the transform
    /// entirely rather than multiplying every pixel by one and adding zero,
    /// which is what keeps the default display exactly as fast as no display
    /// code at all — and byte-identical to it, which is the property
    /// `docs/VIEWER.md` §3.4b's two backends depend on.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.stain.is_none() && self.min == 0.0 && self.max == 255.0
    }

    /// QuPath's `getScaleToByte()`.
    #[must_use]
    pub fn scale(self) -> f32 {
        255.0 / (self.max - self.min)
    }

    /// Applies the display to one RGB pixel, returning RGB.
    ///
    /// With a stain set, all three channels come back equal — the stain amount
    /// as a grey — because an optical density has no colour of its own. QuPath
    /// shows these channels the same way.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "an optical density is a small number and the result is clamped to a byte"
    )]
    pub fn apply(self, red: u8, green: u8, blue: u8) -> (u8, u8, u8) {
        if let Some(stain) = self.stain {
            let value = stain.amount(red, green, blue);
            let grey = clamp_byte(255.0 - (value / stain.full) as f32 * 255.0);
            return (grey, grey, grey);
        }
        let scale = self.scale();
        (
            clamp_byte((f32::from(red) - self.min) * scale),
            clamp_byte((f32::from(green) - self.min) * scale),
            clamp_byte((f32::from(blue) - self.min) * scale),
        )
    }
}

impl Stain {
    /// This stain's optical density for one pixel.
    ///
    /// The same arithmetic as `qupath_pipeline::stain::Stains::stain`, and
    /// deliberately duplicated rather than shared — see the module
    /// documentation. It is nine multiplies and three logarithms per pixel; the
    /// pipeline's version folds them into a lookup table because it runs over
    /// whole regions, where this runs over a window's worth of pixels only when
    /// a stain channel is actually being shown.
    #[must_use]
    pub fn amount(self, red: u8, green: u8, blue: u8) -> f64 {
        let density = [
            optical_density(red, self.background[0]),
            optical_density(green, self.background[1]),
            optical_density(blue, self.background[2]),
        ];
        density[0] * self.coefficients[0]
            + density[1] * self.coefficients[1]
            + density[2] * self.coefficients[2]
    }
}

/// Optical density of one channel byte against a white point.
///
/// `-log10(value / max)`, with the same floor QuPath uses so that a zero byte
/// gives a large finite number rather than infinity.
fn optical_density(value: u8, max: f64) -> f64 {
    let normalised = (f64::from(value) / max).max(1.0 / 255.0);
    -normalised.log10()
}

/// QuPath's `do8BitRangeCheck`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 before the cast, which is what the check is"
)]
fn clamp_byte(value: f32) -> u8 {
    if value < 0.0 {
        0
    } else if value > 255.0 {
        255
    } else {
        value as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QuPath's H&E haematoxylin row and white point, so the stain path is
    /// checked against real numbers rather than a made-up vector.
    ///
    /// Taken from `qupath_pipeline::stain::Stains::h_and_e().coefficients()[0]`
    /// rather than typed from the stain vectors, because the row that matters
    /// is the *inverted* matrix's and inverting it by hand is how a plausible
    /// but wrong vector gets in. `matches_the_pipelines_own_arithmetic` is what
    /// keeps the copy honest.
    fn haematoxylin() -> Stain {
        Stain {
            coefficients: [
                1.838_330_820_296_71,
                0.033_906_046_767_148_236,
                -0.759_830_046_177_923,
            ],
            background: [255.0, 255.0, 255.0],
            full: 1.5,
        }
    }

    /// The duplicated arithmetic must agree with the pipeline's, which is the
    /// cost of the §7 dependency rule the module documentation explains. The
    /// expected values are `Stains::h_and_e().haematoxylin(..)` for the same
    /// pixels, recorded here because this crate cannot call it.
    #[test]
    fn matches_the_pipelines_own_arithmetic() {
        let stain = haematoxylin();
        assert!(
            (stain.amount(80, 70, 160) - 0.790_739_36).abs() < 1e-6,
            "got {}",
            stain.amount(80, 70, 160),
        );
        assert!(
            stain.amount(255, 255, 255).abs() < 1e-9,
            "clean background has no stain: {}",
            stain.amount(255, 255, 255),
        );
    }

    #[test]
    fn the_default_changes_nothing() {
        let display = Display::new();
        assert!(display.is_identity());
        for value in [0u8, 1, 63, 128, 200, 254, 255] {
            assert_eq!(
                display.apply(value, value, value),
                (value, value, value),
                "the identity display must be byte-exact, and {value} was not",
            );
        }
    }

    /// The formula, checked at the two ends and the middle: `minDisplay` goes
    /// to black and `maxDisplay` to white, whatever they are.
    #[test]
    fn the_range_maps_its_ends_to_black_and_white() {
        let display = Display::range(50.0, 200.0).expect("a real range");
        assert_eq!(display.apply(50, 50, 50).0, 0);
        assert_eq!(display.apply(200, 200, 200).0, 255);
        // And the middle of the range is the middle of the output.
        let middle = display.apply(125, 125, 125).0;
        assert!((i16::from(middle) - 127).abs() <= 1, "got {middle}");
    }

    #[test]
    fn values_outside_the_range_clamp_rather_than_wrap() {
        let display = Display::range(50.0, 200.0).expect("a real range");
        assert_eq!(display.apply(0, 0, 0).0, 0, "below the range stays black");
        assert_eq!(
            display.apply(255, 255, 255).0,
            255,
            "above the range stays white",
        );
    }

    /// An inverted or empty range is refused rather than producing a negative
    /// scale, which would show as an inverted image and be blamed on the
    /// decoder.
    #[test]
    fn a_degenerate_range_is_refused() {
        assert!(Display::range(200.0, 50.0).is_none());
        assert!(Display::range(128.0, 128.0).is_none());
        assert!(Display::range(f32::NAN, 255.0).is_none());
        assert!(Display::range(0.0, f32::INFINITY).is_none());
    }

    /// The channels are not swapped: raising only one channel must move only
    /// that channel.
    #[test]
    fn each_channel_is_transformed_independently() {
        let display = Display::range(0.0, 128.0).expect("a real range");
        let (red, green, blue) = display.apply(64, 0, 0);
        assert_eq!(green, 0);
        assert_eq!(blue, 0);
        assert!(red > 100, "the raised channel is the one that moved: {red}");
    }

    /// White is where there is no stain, and a strongly haematoxylin-coloured
    /// pixel must come back darker than white. This is the check that the sign
    /// is the right way up — an inverted density would make background darkest,
    /// and every individual pixel would still look like a plausible grey.
    #[test]
    fn a_stained_pixel_is_darker_than_background() {
        let display = Display::new().with_stain(haematoxylin());
        let (background, _, _) = display.apply(255, 255, 255);
        // A blue-purple pixel, which is what haematoxylin looks like.
        let (stained, _, _) = display.apply(80, 70, 160);
        assert_eq!(background, 255, "clean background is white");
        assert!(
            stained < background,
            "haematoxylin should darken: {stained} against {background}",
        );
    }

    /// Eosin-coloured pink must show *less* haematoxylin than blue-purple does.
    /// A transform that merely converted to grey would pass the test above and
    /// fail this one.
    #[test]
    fn the_stain_channel_separates_the_two_stains() {
        let display = Display::new().with_stain(haematoxylin());
        let (haem, _, _) = display.apply(80, 70, 160);
        let (eosin, _, _) = display.apply(200, 90, 150);
        assert!(
            haem < eosin,
            "haematoxylin channel should show more of the blue pixel ({haem}) \
             than of the pink one ({eosin})",
        );
    }

    #[test]
    fn a_stain_channel_is_grey() {
        let display = Display::new().with_stain(haematoxylin());
        let (red, green, blue) = display.apply(80, 70, 160);
        assert_eq!(
            (red, green),
            (red, blue),
            "an optical density has no colour"
        );
        assert_eq!(green, blue);
    }

    #[test]
    fn a_stain_display_is_not_the_identity() {
        assert!(!Display::new().with_stain(haematoxylin()).is_identity());
        assert!(
            Display::new()
                .with_stain(haematoxylin())
                .without_stain()
                .is_identity()
        );
    }
}
