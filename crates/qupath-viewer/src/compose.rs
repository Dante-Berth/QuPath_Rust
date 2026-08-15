//! Drawing tiles without a GPU.
//!
//! Two reasons, and the second one is the better one.
//!
//! # A machine with no adapter
//!
//! `docs/VIEWER.md` §2b says the window "needs a display and a GPU", and until
//! now the second half was load-bearing: no adapter meant a panic at startup.
//! That is not a rare machine — a headless server, a VM with no passthrough, an
//! X11 session over ssh, or anything without a Vulkan or GL driver installed.
//!
//! Falling back to a *software* Vulkan driver is the obvious fix and is the
//! worse one. It needs the user to have Mesa's lavapipe installed, and it pushes
//! a workload through a whole driver stack that does not need one: this viewer's
//! entire draw call is axis-aligned textured quads, with no rotation, no
//! blending, and no depth. That is a scaled blit, and the loop below is the
//! whole of it.
//!
//! # It makes the composite assertable
//!
//! `docs/VIEWER.md` §6 is built around this repository's rule to assert on
//! counts and recorded output rather than on appearance, and it concedes one
//! gap: what the shader does with a tile is checked by looking at it. Composing
//! on the CPU closes that gap for the geometry, because these are byte
//! assertions on a buffer and need no GPU in the test binary:
//!
//! - a tile drawn at its own size reproduces **its own bytes**, which is §6's
//!   golden check carried all the way to the frame rather than stopping at the
//!   tile;
//! - two tiles drawn edge to edge compose to the same pixels as one tile
//!   covering both, which is the seam bug in its observable form —
//!   `adjacent_tiles_are_placed_edge_to_edge` asserts the *rectangles* touch,
//!   and this asserts the pixels do;
//! - a fine tile covers the underlay beneath it, which is what §3.4's
//!   back-to-front painting claims.
//!
//! This does not make the *appearance* an assertion — colour and orientation are
//! still checked by looking, and the sampling below is nearest where the GPU
//! samples linearly, so the two paths are not pixel-identical when a tile is
//! magnified. What it pins is placement, which is where every bug so far has
//! been.
//!
//! # Sampling
//!
//! Nearest, deliberately, and it is the one visible difference from the window's
//! GPU path. It costs one source read per destination pixel, it makes the 1:1
//! case exact rather than approximately exact, and at the scales level selection
//! actually produces — a tile drawn between 1x and about 1.05x its own size —
//! there is nothing for a filter to do. The place it shows is the underlay,
//! which is the coarsest level stretched over the whole window and so is
//! magnified 16x or more; it reads as blocky rather than soft while the real
//! tiles load. That is a placeholder either way.

/// A frame being composed, in the pixel format a window takes.
///
/// Pixels are `0x00RR_GGBB` — the upper byte unused — because that is what
/// `softbuffer` presents and what X11 and Wayland want underneath it. Storing
/// them packed means the blit writes one `u32` per destination pixel rather
/// than four bytes, and the frame can be handed to the window with one copy.
#[derive(Debug, Clone)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

/// The colour an undrawn pixel keeps, matching the window's GPU path to within
/// a unit.
///
/// The window clears to `(0.08, 0.08, 0.09)` and `wgpu` takes that as *linear*,
/// then stores it into an `Rgba8UnormSrgb` surface — so what is actually in
/// memory is the sRGB *encoding* of those, which is `(80, 80, 85)` and not
/// `(20, 20, 23)`. Guessing that number is precisely the bug `docs/VIEWER.md`
/// §6 records: `render`'s coverage check compared against the linear form,
/// never matched, and reported a fully undrawn frame as fully drawn for two
/// milestones.
///
/// # The last unit, which was measured rather than assumed
///
/// The exact encoding of 0.09 is 84.62, so 85 is the correctly rounded byte and
/// is what this holds. **The Intel Vulkan driver here stores 84** — the same
/// value rounded the other way, decided inside the driver and not by anything
/// this crate can set. Rendering the same frame both ways puts the difference
/// exactly there and nowhere else: coverage agrees pixel for pixel, and every
/// undrawn pixel differs by one in blue.
///
/// Pinning this to 84 would pin it to one driver's rounding, so it stays at the
/// correct value and `render`'s GPU-side check keeps its one unit of slack.
/// [`Framebuffer::fraction_matching`] needs no slack, because on this path
/// there is no encoding step between the clear and the read-back to be wrong
/// about.
pub const CLEAR: u32 = pack(80, 80, 85);

/// Packs 8-bit channels into the `0x00RR_GGBB` a [`Framebuffer`] holds.
#[must_use]
pub const fn pack(red: u8, green: u8, blue: u8) -> u32 {
    ((red as u32) << 16) | ((green as u32) << 8) | (blue as u32)
}

impl Framebuffer {
    /// A frame of `width` by `height`, cleared to [`CLEAR`].
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![CLEAR; width as usize * height as usize],
        }
    }

    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The packed pixels, row-major, `width * height` of them.
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Resizes to `width` by `height`, keeping nothing.
    ///
    /// The contents are not preserved because a resized window redraws
    /// immediately anyway, and carrying stale pixels across would show the old
    /// frame stretched for one frame.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels.resize(width as usize * height as usize, CLEAR);
    }

    /// Fills every pixel with `colour`.
    pub fn clear(&mut self, colour: u32) {
        self.pixels.fill(colour);
    }

    /// Draws a tile into the screen rectangle [`crate::Viewport::tile_rect`]
    /// returns, nearest-sampled and clipped to the frame.
    ///
    /// `rect` is `(x, y, width, height)` in window pixels and may fall partly or
    /// wholly outside; `rgba` is tightly packed RGBA8 of `tile_width` by
    /// `tile_height`, which is exactly what [`crate::Loaded`] carries. The alpha
    /// channel is ignored — there is no blending here, just as there is none in
    /// the shader.
    ///
    /// A destination pixel is covered when **its centre** falls inside the
    /// rectangle. That rule is what makes tiling exact: two tiles sharing an
    /// edge cover every pixel between them exactly once, with no gap to show the
    /// clear colour through and no doubled column, whatever fractional position
    /// the pan leaves the edge at.
    ///
    /// # Panics
    ///
    /// Panics if `rgba` is shorter than `tile_width * tile_height * 4` bytes,
    /// which would mean the loader and the level table disagree about the size
    /// of a tile.
    pub fn draw_tile(
        &mut self,
        rect: (f32, f32, f32, f32),
        rgba: &[u8],
        tile_width: u32,
        tile_height: u32,
    ) {
        let (x, y, width, height) = (
            f64::from(rect.0),
            f64::from(rect.1),
            f64::from(rect.2),
            f64::from(rect.3),
        );

        // Written as a positive test so a NaN rectangle — which a degenerate
        // viewport can produce — falls out here rather than into the loop.
        if !(width > 0.0 && height > 0.0) || tile_width == 0 || tile_height == 0 {
            return;
        }
        assert!(
            rgba.len() >= tile_width as usize * tile_height as usize * 4,
            "a {tile_width}x{tile_height} tile needs {} bytes, got {}",
            tile_width as usize * tile_height as usize * 4,
            rgba.len()
        );

        let first_column = span_start(x, self.width);
        let last_column = span_end(x + width, self.width);
        let first_row = span_start(y, self.height);
        let last_row = span_end(y + height, self.height);
        if first_column >= last_column || first_row >= last_row {
            return;
        }

        // Source pixels per destination pixel. Above 1.0 the tile is being
        // minified and source rows are skipped; below it the tile is magnified
        // and they repeat.
        let across = f64::from(tile_width) / width;
        let down = f64::from(tile_height) / height;
        let (tile_width, tile_height) = (tile_width as usize, tile_height as usize);

        for destination_row in first_row..last_row {
            // The centre of this row of destination pixels, in tile pixels.
            // Non-negative and below `tile_height` by construction — the span
            // helpers only include pixels whose centre is inside the rectangle —
            // but clamped anyway, because the alternative to a clamp here is an
            // out-of-range read on a rounding edge.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "non-negative and below tile_height by construction, and clamped below"
            )]
            let source_row = ((f64::from(destination_row) + 0.5 - y) * down) as usize;
            let source_row = source_row.min(tile_height - 1);

            let tile_row = &rgba[source_row * tile_width * 4..(source_row + 1) * tile_width * 4];
            let start = destination_row as usize * self.width as usize;
            let out = &mut self.pixels[start + first_column as usize..start + last_column as usize];

            // One f64 add per pixel rather than a multiply and a subtract. At
            // 1:1 the step is exactly 1.0 and the start is an exact half-integer,
            // so this accumulates no error at all in the case that has to be
            // byte-exact.
            let mut source_column = (f64::from(first_column) + 0.5 - x) * across;
            for pixel in out {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "non-negative and below tile_width by construction, and clamped here"
                )]
                let at = (source_column as usize).min(tile_width - 1) * 4;
                *pixel = pack(tile_row[at], tile_row[at + 1], tile_row[at + 2]);
                source_column += across;
            }
        }
    }

    /// The fraction of the frame holding `colour`, in `0.0..=1.0`.
    ///
    /// Against [`CLEAR`] this is "how much of this frame was never drawn", which
    /// is what `docs/VIEWER.md` §3.4 measures the underlay with. On this path it
    /// is an exact equality rather than a threshold, because nothing encodes the
    /// colour between the clear and the read-back.
    #[must_use]
    pub fn fraction_matching(&self, colour: u32) -> f64 {
        if self.pixels.is_empty() {
            return 0.0;
        }
        let matching = self.pixels.iter().filter(|&&pixel| pixel == colour).count();
        matching as f64 / self.pixels.len() as f64
    }

    /// Unpacks the frame into tightly packed RGBA8, for writing a PNG.
    ///
    /// `out` is cleared and refilled, so a caller writing a sequence of frames
    /// allocates once.
    pub fn to_rgba(&self, out: &mut Vec<u8>) {
        out.clear();
        out.reserve(self.pixels.len() * 4);
        for &pixel in &self.pixels {
            // Each byte is masked to 8 bits before the cast, so nothing is lost.
            out.extend_from_slice(&[
                ((pixel >> 16) & 0xff) as u8,
                ((pixel >> 8) & 0xff) as u8,
                (pixel & 0xff) as u8,
                255,
            ]);
        }
    }
}

/// The first destination pixel whose centre is at or past `edge`, clamped to
/// the frame.
fn span_start(edge: f64, limit: u32) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to [0, limit] on the line above the cast"
    )]
    {
        (edge - 0.5).ceil().clamp(0.0, f64::from(limit)) as u32
    }
}

/// One past the last destination pixel whose centre is before `edge`, clamped
/// to the frame.
///
/// The same expression as [`span_start`] on purpose: a pixel centre landing
/// exactly on a shared edge belongs to the tile on the far side of it, and one
/// tile's `span_end` is the next tile's `span_start`, so the two agree by
/// construction rather than by a rounding rule that has to be got right twice.
fn span_end(edge: f64, limit: u32) -> u32 {
    span_start(edge, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tile whose every pixel is distinguishable, so a blit that reads the
    /// wrong source pixel produces the wrong colour rather than the right one
    /// by luck.
    fn tile(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::new();
        for row in 0..height {
            for column in 0..width {
                #[expect(clippy::cast_possible_truncation, reason = "test tiles are small")]
                rgba.extend_from_slice(&[column as u8, row as u8, (column + row) as u8, 255]);
            }
        }
        rgba
    }

    #[test]
    fn a_tile_drawn_at_its_own_size_reproduces_its_own_bytes() {
        // `docs/VIEWER.md` §6's golden check, carried to the frame: at 1:1 and
        // on an integer offset the compositor must be a copy. Anything else —
        // a half-pixel in the sampling, an off-by-one in the span — shows up
        // here as a shifted image, and this is the only test in the crate that
        // can see that at all.
        let mut frame = Framebuffer::new(64, 48);
        let rgba = tile(16, 12);
        frame.draw_tile((8.0, 6.0, 16.0, 12.0), &rgba, 16, 12);

        for row in 0..12 {
            for column in 0..16 {
                let at = (row * 16 + column) * 4;
                let expected = pack(rgba[at], rgba[at + 1], rgba[at + 2]);
                let pixel = frame.pixels()[(row + 6) * 64 + column + 8];
                assert_eq!(pixel, expected, "at tile pixel {column},{row}");
            }
        }
    }

    #[test]
    fn two_tiles_compose_to_the_same_pixels_as_one() {
        // The seam bug in its observable form. `Viewport`'s
        // `adjacent_tiles_are_placed_edge_to_edge` asserts the rectangles touch;
        // this asserts the pixels do, which is a strictly stronger claim — a
        // gap shows as clear colour and an overlap shows as a shifted right
        // half, and neither is visible from the geometry alone.
        let whole = tile(32, 8);
        let mut left = Vec::new();
        let mut right = Vec::new();
        for row in 0..8 {
            let start = row * 32 * 4;
            left.extend_from_slice(&whole[start..start + 16 * 4]);
            right.extend_from_slice(&whole[start + 16 * 4..start + 32 * 4]);
        }

        let mut one = Framebuffer::new(48, 16);
        one.draw_tile((5.0, 4.0, 32.0, 8.0), &whole, 32, 8);

        let mut two = Framebuffer::new(48, 16);
        two.draw_tile((5.0, 4.0, 16.0, 8.0), &left, 16, 8);
        two.draw_tile((21.0, 4.0, 16.0, 8.0), &right, 16, 8);

        assert_eq!(one.pixels(), two.pixels());
    }

    #[test]
    fn a_fractional_seam_leaves_no_gap() {
        // A pan leaves tile edges on fractional positions almost always, which
        // is when a rounding rule that works on integers starts showing a grid
        // of clear-colour lines. Swept over a subpixel offset, and over a scale
        // that is not 1:1 so the two edges are not the same arithmetic.
        for step in 0..16 {
            let offset = f64::from(step) / 16.0;
            #[expect(clippy::cast_possible_truncation, reason = "screen space is f32")]
            let offset = offset as f32;
            let mut frame = Framebuffer::new(64, 16);
            let rgba = tile(16, 16);
            // Three tiles of 17.3 px each, laid end to end across the frame.
            for index in 0..3u8 {
                let x = offset + f32::from(index) * 17.3;
                frame.draw_tile((x, 0.0, 17.3, 16.0), &rgba, 16, 16);
            }
            // Everything strictly inside the span the three cover must have been
            // written. The ends are excluded because the first tile starts at a
            // fractional offset and the frame is wider than the three tiles.
            let covered = frame.pixels()[2..50]
                .iter()
                .filter(|&&pixel| pixel == CLEAR)
                .count();
            assert_eq!(covered, 0, "offset {offset} left a gap in the first row");
        }
    }

    #[test]
    fn a_fine_tile_covers_the_underlay_beneath_it() {
        // What §3.4's back-to-front painting claims: the underlay is drawn
        // first at its own level's rectangle, and a fine tile that has arrived
        // paints over it. Drawn in that order, the fine tile must win on every
        // pixel it covers and on none that it does not.
        let coarse = vec![10, 20, 30, 255];
        let fine = vec![200, 210, 220, 255];
        let mut frame = Framebuffer::new(32, 32);
        frame.draw_tile((0.0, 0.0, 32.0, 32.0), &coarse, 1, 1);
        frame.draw_tile((8.0, 8.0, 16.0, 16.0), &fine, 1, 1);

        for row in 0..32 {
            for column in 0..32 {
                let inside = (8..24).contains(&column) && (8..24).contains(&row);
                let expected = if inside {
                    pack(200, 210, 220)
                } else {
                    pack(10, 20, 30)
                };
                assert_eq!(
                    frame.pixels()[row * 32 + column],
                    expected,
                    "at {column},{row}"
                );
            }
        }
    }

    #[test]
    fn a_tile_off_the_edge_is_clipped_rather_than_wrapped() {
        // Panning puts tiles past every edge on every frame. Writing outside the
        // frame is a panic, and writing to the wrong row is the classic
        // stride bug: a tile hanging off the left edge reappearing on the right.
        let rgba = tile(16, 16);
        for rect in [
            (-8.0, 4.0, 16.0, 16.0),
            (56.0, 4.0, 16.0, 16.0),
            (4.0, -8.0, 16.0, 16.0),
            (4.0, 56.0, 16.0, 16.0),
        ] {
            let mut frame = Framebuffer::new(64, 64);
            frame.draw_tile(rect, &rgba, 16, 16);
            for row in 0..64u32 {
                for column in 0..64u32 {
                    let inside = f64::from(column) + 0.5 >= f64::from(rect.0)
                        && f64::from(column) + 0.5 < f64::from(rect.0) + 16.0
                        && f64::from(row) + 0.5 >= f64::from(rect.1)
                        && f64::from(row) + 0.5 < f64::from(rect.1) + 16.0;
                    let drawn = frame.pixels()[(row * 64 + column) as usize] != CLEAR;
                    assert_eq!(drawn, inside, "rect {rect:?} at {column},{row}");
                }
            }
        }
    }

    #[test]
    fn a_tile_entirely_off_the_frame_writes_nothing() {
        let rgba = tile(16, 16);
        for rect in [
            (-100.0, 0.0, 16.0, 16.0),
            (200.0, 0.0, 16.0, 16.0),
            (0.0, -100.0, 16.0, 16.0),
            (0.0, 200.0, 16.0, 16.0),
            // Degenerate and non-finite rectangles a collapsed viewport makes.
            (0.0, 0.0, 0.0, 16.0),
            (0.0, 0.0, -16.0, 16.0),
            (f32::NAN, 0.0, 16.0, 16.0),
        ] {
            let mut frame = Framebuffer::new(64, 64);
            frame.draw_tile(rect, &rgba, 16, 16);
            assert_eq!(
                frame.fraction_matching(CLEAR),
                1.0,
                "rect {rect:?} drew something"
            );
        }
    }

    #[test]
    fn magnifying_repeats_source_pixels_and_minifying_skips_them() {
        // Nearest sampling, stated as an assertion rather than left implicit:
        // 4x magnification repeats each source pixel four times, and the
        // underlay is magnified far more than that.
        let rgba = tile(4, 1);
        let mut frame = Framebuffer::new(16, 1);
        frame.draw_tile((0.0, 0.0, 16.0, 1.0), &rgba, 4, 1);
        for column in 0..16u32 {
            let source = column / 4;
            #[expect(clippy::cast_possible_truncation, reason = "source is below 4")]
            let expected = pack(source as u8, 0, source as u8);
            assert_eq!(frame.pixels()[column as usize], expected, "at {column}");
        }

        // And 4x minification takes every fourth one, reading none outside.
        let rgba = tile(16, 1);
        let mut frame = Framebuffer::new(4, 1);
        frame.draw_tile((0.0, 0.0, 4.0, 1.0), &rgba, 16, 1);
        for column in 0..4u32 {
            let source = column * 4 + 2;
            #[expect(clippy::cast_possible_truncation, reason = "source is below 16")]
            let expected = pack(source as u8, 0, source as u8);
            assert_eq!(frame.pixels()[column as usize], expected, "at {column}");
        }
    }

    #[test]
    fn the_undrawn_fraction_is_exact() {
        // The number §3.4's underlay comparison is made of. On the GPU path it
        // is a threshold against an encoded colour and was wrong for two
        // milestones; here it is an equality and can be asserted outright.
        let mut frame = Framebuffer::new(10, 10);
        assert_eq!(frame.fraction_matching(CLEAR), 1.0);
        frame.draw_tile((0.0, 0.0, 10.0, 5.0), &[1, 2, 3, 255], 1, 1);
        assert_eq!(frame.fraction_matching(CLEAR), 0.5);
        frame.draw_tile((0.0, 5.0, 10.0, 5.0), &[1, 2, 3, 255], 1, 1);
        assert_eq!(frame.fraction_matching(CLEAR), 0.0);
        assert_eq!(frame.fraction_matching(pack(1, 2, 3)), 1.0);
    }

    #[test]
    fn resizing_clears_rather_than_stretching() {
        let mut frame = Framebuffer::new(8, 8);
        frame.clear(pack(1, 2, 3));
        frame.resize(16, 4);
        assert_eq!(frame.width(), 16);
        assert_eq!(frame.height(), 4);
        assert_eq!(frame.pixels().len(), 64);
        assert_eq!(frame.fraction_matching(CLEAR), 1.0);
    }

    #[test]
    fn rgba_round_trips_through_the_packed_form() {
        let mut frame = Framebuffer::new(2, 1);
        frame.draw_tile((0.0, 0.0, 2.0, 1.0), &[7, 8, 9, 255], 1, 1);
        let mut rgba = Vec::new();
        frame.to_rgba(&mut rgba);
        assert_eq!(rgba, vec![7, 8, 9, 255, 7, 8, 9, 255]);
    }
}
