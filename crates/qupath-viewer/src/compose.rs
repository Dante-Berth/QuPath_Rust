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

    /// Draws line segments in `colour`, clipped to the frame.
    ///
    /// `segments` is what [`crate::Overlay::segments_into`] produces: pairs of
    /// screen-space points, `segments[0]` to `segments[1]` being one line. A
    /// trailing odd point is ignored rather than joined to anything.
    ///
    /// Lines are one pixel wide and unblended, and a pixel is set when the line
    /// passes through it — the same "which pixel does this point fall in" rule
    /// [`Framebuffer::draw_tile`] uses, so an outline and the tile beneath it
    /// cannot be a half pixel apart.
    ///
    /// # What this deliberately does not match
    ///
    /// The GPU path draws these as a `LineList`, and GPU line rasterisation is
    /// implementation-defined — the specification allows a *range* of rules, so
    /// two drivers may light different pixels for the same segment and neither
    /// is wrong. Tiles are byte-identical across the two backends
    /// (`docs/VIEWER.md` §3.4b) and outlines are **not**, and that is a
    /// property of lines rather than a defect here. What stays assertable is
    /// where an outline lands, which is what the tests below check.
    pub fn draw_segments(&mut self, segments: &[crate::ScreenPoint], colour: u32) {
        for pair in segments.chunks_exact(2) {
            self.draw_line(pair[0], pair[1], colour);
        }
    }

    /// One clipped line, by a DDA over the longer axis.
    fn draw_line(&mut self, from: crate::ScreenPoint, to: crate::ScreenPoint, colour: u32) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let (mut x0, mut y0) = (f64::from(from.x), f64::from(from.y));
        let (mut x1, mut y1) = (f64::from(to.x), f64::from(to.y));
        if !(x0.is_finite() && y0.is_finite() && x1.is_finite() && y1.is_finite()) {
            return;
        }

        // Clipped *before* stepping, not by testing each pixel. A viewport
        // zoomed far in puts an outline's endpoints millions of pixels outside
        // the window, and a DDA that walks from one to the other spends the
        // frame discarding pixels it never had any chance of drawing.
        if !clip_to(&mut x0, &mut y0, &mut x1, &mut y1, self.width, self.height) {
            return;
        }

        let (dx, dy) = (x1 - x0, y1 - y0);
        // One step per pixel of the longer axis, so no pixel of the line is
        // skipped and none is written more than twice.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "both deltas are clipped to the frame, so this is below its diagonal"
        )]
        let steps = dx.abs().max(dy.abs()).ceil() as usize;
        for step in 0..=steps {
            let along = if steps == 0 {
                0.0
            } else {
                step as f64 / steps as f64
            };
            self.set(x0 + dx * along, y0 + dy * along, colour);
        }
    }

    /// Sets the pixel a screen-space point falls in, if it is on the frame.
    fn set(&mut self, x: f64, y: f64, colour: u32) {
        // `floor` and not a cast: a cast truncates towards zero, so everything
        // in `-1.0..0.0` would land in column 0 and an outline just off the left
        // edge would smear down it.
        let (column, row) = (x.floor(), y.floor());
        if column < 0.0 || row < 0.0 {
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "non-negative, and compared against the frame size below"
        )]
        let (column, row) = (column as usize, row as usize);
        if column >= self.width as usize || row >= self.height as usize {
            return;
        }
        self.pixels[row * self.width as usize + column] = colour;
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

/// Clips a segment to the frame in place, returning whether any of it survives.
///
/// Liang–Barsky, which trims the parameter range rather than walking the
/// endpoints in: one pass over four half-planes, no iteration, and it cannot
/// loop forever on a segment whose endpoints are both far outside.
///
/// The box is the frame's pixel area — `0..width` by `0..height` — because
/// [`Framebuffer::set`] floors a coordinate into a pixel, so a point at exactly
/// `width` is one past the end and a point at `width - 0.001` is the last
/// column.
fn clip_to(
    x0: &mut f64,
    y0: &mut f64,
    x1: &mut f64,
    y1: &mut f64,
    width: u32,
    height: u32,
) -> bool {
    let (dx, dy) = (*x1 - *x0, *y1 - *y0);
    let (mut enter, mut leave) = (0.0f64, 1.0f64);
    for (edge, distance) in [
        (-dx, *x0),
        (dx, f64::from(width) - *x0),
        (-dy, *y0),
        (dy, f64::from(height) - *y0),
    ] {
        if edge == 0.0 {
            // Parallel to this edge: either wholly inside it or wholly outside.
            if distance < 0.0 {
                return false;
            }
            continue;
        }
        let at = distance / edge;
        if edge < 0.0 {
            if at > leave {
                return false;
            }
            enter = enter.max(at);
        } else {
            if at < enter {
                return false;
            }
            leave = leave.min(at);
        }
    }
    if enter > leave {
        return false;
    }
    let (from_x, from_y) = (*x0, *y0);
    *x1 = from_x + dx * leave;
    *y1 = from_y + dy * leave;
    *x0 = from_x + dx * enter;
    *y0 = from_y + dy * enter;
    true
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

    fn point(x: f32, y: f32) -> crate::ScreenPoint {
        crate::ScreenPoint::new(x, y)
    }

    /// Which pixels of a frame are not the clear colour, as `(column, row)`.
    fn lit(frame: &Framebuffer) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for row in 0..frame.height() {
            for column in 0..frame.width() {
                if frame.pixels()[(row * frame.width() + column) as usize] != CLEAR {
                    out.push((column, row));
                }
            }
        }
        out
    }

    #[test]
    fn an_axis_aligned_segment_lights_exactly_the_pixels_it_crosses() {
        // The one case with a single right answer, so it is asserted as an
        // equality rather than as a property. Anything else — an inclusive end,
        // a truncating cast, a half-pixel offset — changes this list.
        let mut frame = Framebuffer::new(8, 4);
        frame.draw_segments(&[point(2.5, 1.5), point(5.5, 1.5)], pack(255, 0, 0));
        assert_eq!(lit(&frame), vec![(2, 1), (3, 1), (4, 1), (5, 1)]);

        let mut frame = Framebuffer::new(4, 8);
        frame.draw_segments(&[point(1.5, 6.5), point(1.5, 3.5)], pack(255, 0, 0));
        assert_eq!(lit(&frame), vec![(1, 3), (1, 4), (1, 5), (1, 6)]);
    }

    #[test]
    fn a_segment_is_drawn_the_same_way_round_either_way() {
        // A ring is walked in one direction and its edges in another; a
        // rasteriser whose output depends on the order of the endpoints leaves
        // a seam wherever two objects share one.
        let ends = [point(1.2, 0.8), point(6.7, 3.4)];
        let mut forwards = Framebuffer::new(8, 5);
        forwards.draw_segments(&ends, pack(0, 255, 0));
        let mut backwards = Framebuffer::new(8, 5);
        backwards.draw_segments(&[ends[1], ends[0]], pack(0, 255, 0));
        assert_eq!(lit(&forwards), lit(&backwards));
    }

    #[test]
    fn a_segment_has_no_gaps() {
        // A DDA that steps the shorter axis skips pixels on the longer one, and
        // the result is a dashed outline that reads as a rendering artefact.
        // Every lit pixel must touch the next along the major axis.
        for (from, to) in [
            (point(0.5, 0.5), point(31.5, 7.5)),
            (point(0.5, 7.5), point(31.5, 0.5)),
            (point(0.5, 0.5), point(31.5, 31.5)),
            (point(30.9, 1.1), point(1.1, 30.9)),
        ] {
            let mut frame = Framebuffer::new(32, 32);
            frame.draw_segments(&[from, to], pack(255, 255, 255));
            let mut columns: Vec<u32> = lit(&frame).iter().map(|&(x, _)| x).collect();
            columns.sort_unstable();
            columns.dedup();
            for pair in columns.windows(2) {
                assert_eq!(pair[1] - pair[0], 1, "column gap in {from:?}..{to:?}");
            }
        }
    }

    #[test]
    fn a_segment_outside_the_frame_writes_nothing() {
        // Panning puts most of an overlay off screen on every frame, and a
        // viewport zoomed in puts the endpoints millions of pixels away.
        for (from, to) in [
            (point(-50.0, -50.0), point(-10.0, -10.0)),
            (point(100.0, 5.0), point(200.0, 5.0)),
            (point(5.0, -100.0), point(5.0, -1.0)),
            (point(-1e9, 4.0), point(-1e6, 4.0)),
            (point(f32::NAN, 0.0), point(4.0, 4.0)),
            (point(0.0, 0.0), point(f32::INFINITY, 4.0)),
        ] {
            let mut frame = Framebuffer::new(16, 16);
            frame.draw_segments(&[from, to], pack(255, 0, 0));
            assert_eq!(
                frame.fraction_matching(CLEAR),
                1.0,
                "{from:?}..{to:?} drew something"
            );
        }
    }

    #[test]
    fn a_segment_crossing_the_frame_is_clipped_rather_than_wrapped() {
        // The stride bug in its overlay form: a line running off the right edge
        // reappearing on the left of the next row. Every lit pixel must be on
        // the row the line actually passes through.
        let mut frame = Framebuffer::new(16, 16);
        frame.draw_segments(&[point(-1000.0, 8.5), point(1000.0, 8.5)], pack(255, 0, 0));
        assert_eq!(
            lit(&frame),
            (0..16).map(|column| (column, 8)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_far_away_segment_costs_no_more_than_a_near_one() {
        // What the clip is for. Without it the DDA walks from one endpoint to
        // the other regardless of where they are, and an outline at 32x zoom
        // takes millions of steps to draw the four pixels it covers. Asserted
        // as time because the cost is the whole point and nothing else observes
        // it.
        let mut frame = Framebuffer::new(64, 64);
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            frame.draw_segments(&[point(-4e7, 32.5), point(4e7, 32.5)], pack(1, 2, 3));
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "1000 clipped segments took {elapsed:?}, so the clip is not running"
        );
    }

    #[test]
    fn a_degenerate_segment_lights_one_pixel() {
        let mut frame = Framebuffer::new(8, 8);
        frame.draw_segments(&[point(3.5, 4.5), point(3.5, 4.5)], pack(255, 0, 0));
        assert_eq!(lit(&frame), vec![(3, 4)]);
    }

    #[test]
    fn an_odd_trailing_point_is_ignored() {
        // `segments_into` always produces pairs, but this is public and a
        // caller building its own list should not get a line from the last
        // point to nowhere.
        let mut frame = Framebuffer::new(8, 8);
        frame.draw_segments(
            &[point(1.5, 1.5), point(5.5, 1.5), point(3.5, 6.5)],
            pack(9, 9, 9),
        );
        assert_eq!(lit(&frame), vec![(1, 1), (2, 1), (3, 1), (4, 1), (5, 1)]);
    }

    #[test]
    fn an_outline_lands_on_the_tile_pixels_it_covers() {
        // The claim `docs/VIEWER.md` §6 can actually make about an overlay:
        // not that it looks right, but that it is *where the slide says*. A
        // tile is drawn at 1:1 and a segment is drawn along the slide
        // coordinate of one of its rows; the lit pixels must be the ones the
        // tile put that row in.
        let mut rgba = Vec::new();
        for row in 0..8u8 {
            for column in 0..8u8 {
                rgba.extend_from_slice(&[column, row, 0, 255]);
            }
        }
        let mut frame = Framebuffer::new(16, 16);
        frame.draw_tile((4.0, 4.0, 8.0, 8.0), &rgba, 8, 8);

        // Tile row 3 occupies frame row 7, so a segment along its centre must
        // light exactly the frame pixels holding that row of the tile.
        frame.draw_segments(&[point(4.5, 7.5), point(11.5, 7.5)], pack(255, 0, 255));
        for column in 4..12u32 {
            assert_eq!(
                frame.pixels()[(7 * 16 + column) as usize],
                pack(255, 0, 255),
                "the outline missed frame column {column}"
            );
        }
        // And the tile row above it is untouched, so the line is one pixel wide
        // and on the row it was asked for.
        for column in 4..12u32 {
            #[expect(clippy::cast_possible_truncation, reason = "columns run 4..12")]
            let expected = pack((column - 4) as u8, 2, 0);
            assert_eq!(
                frame.pixels()[(6 * 16 + column) as usize],
                expected,
                "the outline bled onto frame column {column} of the row above"
            );
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
