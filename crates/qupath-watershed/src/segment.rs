//! ImageJ's `MaximumFinder` in `SEGMENTED` mode: the shape split.
//!
//! The second half of `new EDM().toWatershed(bp)`, and the last thing standing
//! between the merged binary and the objects detection counts:
//!
//! ```java
//! FloatProcessor floatEdm = makeFloatEDM(ip, 0, false);
//! ByteProcessor maxIp = maxFinder.findMaxima(floatEdm, MAXFINDER_TOLERANCE,
//!         ImageProcessor.NO_THRESHOLD, MaximumFinder.SEGMENTED, false, true);
//! if (maxIp != null) ip.copyBits(maxIp, 0, 0, Blitter.AND);
//! ```
//!
//! [`crate::DistanceMap`] is the first line. This module is the second, and
//! [`Segmenter::segment`] does the third as well.
//!
//! # This is not the watershed already in this crate
//!
//! [`crate::Watershed`] floods a Laplacian from labelled markers and is what
//! detection runs to separate basins. This is a *different* watershed on a
//! *different* surface, and the two share no code: this one has no markers
//! handed to it, derives its own from the distance map, and dilates by a
//! 256-entry fate table rather than by a priority queue. Writing it in terms of
//! the other was tried on paper and abandoned — the fate table is what forbids
//! two regions from merging, and a queue-based flood has no equivalent of it.
//!
//! # The five stages, and which of them are the sharp edges
//!
//! 1. **Find every local maximum** of the EDM, comparing *corrected* heights.
//! 2. **Group them by tolerance**, so a ridge is one maximum rather than many.
//! 3. **Quantise the EDM to 8 bits**, with the grouped maxima pinned at 255.
//! 4. **Erase the maxima that lost**, flooding each down to its saddle.
//! 5. **Dilate the survivors level by level**, forbidden to merge.
//!
//! Three details decide the answer and none is visible from the call site:
//!
//! - **`isEDM` is `true`, and it is not cosmetic.** It replaces every height
//!   comparison with [`true_edm_height`], an interpolated ridge height, because
//!   a distance map sampled on a grid puts its true maximum between pixels. It
//!   also sets the sorting-error tolerance to `1.1 * sqrt(2)/2` rather than to
//!   something scaled from the value range, and it suppresses `cleanupExtraLines`
//!   entirely. A port passing `false` agrees on smooth images and diverges on
//!   every image with a plateau — which is every EDM.
//! - **The tolerance is `0.5`**, `EDM.MAXFINDER_TOLERANCE`, which is private and
//!   so cannot be read from the call site at all.
//! - **`strict` is `false`.** The six-argument `findMaxima` overload passes
//!   `excludeOnEdges` for it, and detection passes `false` for that, so a single
//!   maximum survives even when it does not stand a full tolerance above
//!   everything else.
//!
//! # What is deliberately not here
//!
//! `excludeOnEdges` is `false` and the output type is `SEGMENTED`, so
//! `deleteEdgeParticles` never runs; `isEDM` is `true`, so `cleanupExtraLines`
//! never runs; the processor carries no ROI or mask, so the final clip is a
//! no-op. None of the three is ported. §10 forbids stubs that return plausible
//! wrong values, and a branch nothing calls is a branch no fixture checks.
//!
//! Pinned by `parity/fixtures/MergeAndSplit.json`, whose `maxima` and
//! `segmented` arrays are recorded from a real ImageJ.

/// `EDM.MAXFINDER_TOLERANCE`, private in ImageJ and so restated here.
const TOLERANCE: f32 = 0.5;

/// `MaximumFinder.SQRT2`, which ImageJ writes out as the literal `1.4142135624f`.
///
/// That literal and Rust's constant are the same `f32` — both are `0x3fb504f3`,
/// checked rather than assumed, since this value is used in height arithmetic
/// that decides which maxima merge.
const SQRT2: f32 = std::f32::consts::SQRT_2;

/// How far the sorted order may be wrong, for an EDM.
///
/// `1.1f * (isEDM ? SQRT2/2f : (globalMax-globalMin)/2e9f)`. The maxima are
/// sorted by a value quantised into a 32-bit int and corrected by
/// [`true_edm_height`] afterwards, so neighbouring entries can be out of order
/// by this much; the analysis restarts when it finds one.
const MAX_SORTING_ERROR: f32 = 1.1 * SQRT2 / 2.0;

/// Marks a local maximum, whatever the tolerance says about it later.
const MAXIMUM: u8 = 1;
/// Marks a point currently in the working list.
const LISTED: u8 = 2;
/// Marks a point some earlier maximum already claimed.
const PROCESSED: u8 = 4;
/// Marks the area around a maximum that survived the tolerance grouping.
const MAX_AREA: u8 = 8;
/// Marks points of exactly the maximum's height, for finding its centre.
const EQUAL: u8 = 16;
/// Marks the single point standing for a maximum.
const MAX_POINT: u8 = 32;
/// Marks a maximum flooded away before the dilation.
const ELIMINATED: u8 = 64;

/// Clockwise from north, as `DIR_X_OFFSET` and `DIR_Y_OFFSET`.
const DIR_X: [i32; 8] = [0, 1, 1, 1, 0, -1, -1, -1];
/// Clockwise from north, as `DIR_X_OFFSET` and `DIR_Y_OFFSET`.
const DIR_Y: [i32; 8] = [-1, -1, 0, 1, 1, 1, 0, -1];

/// The order the dilation tries its eight directions: diagonals first.
///
/// ImageJ's `directionSequence`. The order changes which of two equally good
/// pixels a region claims, so it changes where a segmentation line lands.
const DIRECTION_SEQUENCE: [usize; 8] = [7, 3, 1, 5, 0, 4, 2, 6];

/// The shape split, with its scratch.
///
/// Every buffer here is image-sized and reused across tiles, which is what §6 of
/// the project guidelines asks for. It is also the largest scratch in the
/// pipeline: about 17 bytes per pixel, so roughly 80 MB for one 2168² padded
/// tile, held once per worker rather than once per tile.
#[derive(Debug)]
pub struct Segmenter {
    /// `typeP`: the per-pixel flag byte the whole algorithm communicates through.
    types: Vec<u8>,
    /// `outIp`: the EDM quantised to 8 bits, then flooded, then dilated.
    levels: Vec<u8>,
    /// `pList`: the working list, reused by both flood phases.
    list: Vec<u32>,
    /// `maxPoints`: local maxima, value in the high 32 bits and offset in the
    /// low, sorted ascending so the highest is processed first.
    maxima: Vec<i64>,
    /// `coordinates`: pixels of levels 1 to 254, bucketed by level.
    coordinates: Vec<u32>,
    /// `setPointList`: pixels one dilation pass will set, applied together.
    set_points: Vec<u32>,
    /// Per-level pixel counts, mutated as the dilation defers pixels downwards.
    histogram: Vec<u32>,
    /// Where each level's bucket starts in [`Segmenter::coordinates`].
    level_start: Vec<usize>,
    /// How far each level's bucket has been filled.
    level_offset: Vec<u32>,
    /// The 256-entry table that forbids a dilation from joining two regions.
    fate: [u8; 256],
}

impl Default for Segmenter {
    fn default() -> Self {
        Self {
            types: Vec::new(),
            levels: Vec::new(),
            list: Vec::new(),
            maxima: Vec::new(),
            coordinates: Vec::new(),
            set_points: Vec::new(),
            histogram: Vec::new(),
            level_start: Vec::new(),
            level_offset: Vec::new(),
            // 256 iterations of pure bit arithmetic, so there is nothing to
            // gain by deferring it and one less piece of state to get wrong.
            fate: make_fate_table(),
        }
    }
}

impl Segmenter {
    /// Creates a segmenter with no working buffers yet allocated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Splits `mask` by the shape of its distance map, in place.
    ///
    /// This is the whole of `EDM.toWatershed`: `edm` must be the output of
    /// [`crate::DistanceMap::make_float_edm`] for this same `mask`, and `mask` is
    /// left with background lines cut along the saddles between touching blobs.
    ///
    /// # Panics
    ///
    /// Panics if the buffers describe images of different sizes, or if `width`
    /// is zero or does not divide them.
    pub fn segment(&mut self, edm: &[f32], mask: &mut [u8], width: usize) {
        self.find_maxima(edm, width);
        // `ip.copyBits(maxIp, 0, 0, Blitter.AND)`, where the segmentation holds
        // only 0 and 255 — so a logical AND, cutting the lines into the mask.
        for (pixel, &level) in mask.iter_mut().zip(&self.levels) {
            *pixel &= level;
        }
    }

    /// `findMaxima(edm, 0.5, NO_THRESHOLD, SEGMENTED, false, true)`, leaving the
    /// segmentation in [`Segmenter::levels`] as 0 and 255.
    ///
    /// # Panics
    ///
    /// Panics if `width` is zero or does not divide `edm`.
    pub fn find_maxima(&mut self, edm: &[f32], width: usize) {
        assert!(width > 0, "width must be non-zero");
        assert_eq!(edm.len() % width, 0, "width must divide the buffer");
        let height = edm.len() / width;
        let image = Image { width, height };

        // `types` is the one that has to arrive zeroed: every stage reads a flag
        // before it writes one. `levels` is written for every pixel by
        // `make_8bit` and `list` is written before it is read, so clearing
        // either is 22 MB of memset per 2120² tile that nothing observes.
        self.types.clear();
        self.types.resize(edm.len(), 0);
        fit(&mut self.levels, edm.len(), 0);
        fit(&mut self.list, edm.len(), 0);

        let mut global_min = f32::MAX;
        let mut global_max = -f32::MAX;
        for &value in edm {
            global_min = global_min.min(value);
            global_max = global_max.max(value);
        }

        // `strict` is false, so this is the only thing that can rule out a
        // maximum before the search starts.
        if global_max > global_min {
            self.sorted_max_points(edm, image, global_min, global_max);
        } else {
            self.maxima.clear();
        }
        self.analyze_and_mark_maxima(edm, image);
        self.make_8bit(edm, global_max);
        self.cleanup_maxima(image);
        self.watershed_segment(image);
        // `watershedPostProcess`: everything that is not a particle becomes
        // background, so the lines the dilation left join it.
        for level in &mut self.levels {
            if *level < 255 {
                *level = 0;
            }
        }
    }

    /// The segmentation, as 0 and 255. Valid after [`Segmenter::find_maxima`].
    #[must_use]
    pub fn segmentation(&self) -> &[u8] {
        &self.levels
    }

    /// `getSortedMaxPoints`: every local maximum, sorted by corrected height.
    fn sorted_max_points(&mut self, edm: &[f32], image: Image, global_min: f32, global_max: f32) {
        let Image { width, height } = image;
        let mut count = 0usize;
        for y in 0..height {
            for x in 0..width {
                let at = y * width + x;
                let value = edm[at];
                if value == global_min {
                    continue;
                }
                // For an EDM every comparison is against the interpolated ridge
                // height, but the *raw* value still has to be higher too: the
                // pair of tests is what keeps both the corrected maxima that
                // `find_maxima` needs and the raw ones `cleanup_maxima` does.
                let corrected = true_edm_height(edm, image, x, y);
                let mut is_max = true;
                let inner = image.is_inner(x, y);
                for d in 0..8 {
                    if !(inner || image.is_within(x, y, d)) {
                        continue;
                    }
                    let (nx, ny) = image.step(x, y, d);
                    let neighbour = edm[ny * width + nx];
                    if neighbour > value && true_edm_height(edm, image, nx, ny) > corrected {
                        is_max = false;
                        break;
                    }
                }
                if is_max {
                    self.types[at] = MAXIMUM;
                    count += 1;
                }
            }
        }

        // The value is quantised into 32 bits so that one `i64` sort orders the
        // maxima by height and, on a tie, by raster position. The quantisation
        // is why `MAX_SORTING_ERROR` exists.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Java's (float) cast of the same double expression"
        )]
        let factor = (2e9 / f64::from(global_max - global_min)) as f32;
        self.maxima.clear();
        self.maxima.reserve(count);
        for y in 0..height {
            for x in 0..width {
                let at = y * width + x;
                if self.types[at] != MAXIMUM {
                    continue;
                }
                let corrected = true_edm_height(edm, image, x, y);
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "Java's (int) cast, whose saturation Rust's `as` matches"
                )]
                let quantised = ((corrected - global_min) * factor) as i32;
                self.maxima.push((i64::from(quantised) << 32) | at as i64);
            }
        }
        self.maxima.sort_unstable();
    }

    /// `analyzeAndMarkMaxima`: group the maxima by tolerance and mark the area
    /// each one owns.
    fn analyze_and_mark_maxima(&mut self, edm: &[f32], image: Image) {
        let Image { width, .. } = image;
        if self.maxima.is_empty() {
            // "no initial maxima at all? then consider all as 'within
            // tolerance'". A uniform image reaches this, and so does one with
            // no foreground at all.
            self.types.fill(PROCESSED | MAX_AREA);
            return;
        }

        for index in (0..self.maxima.len()).rev() {
            let mut offset0 = lower32(self.maxima[index]);
            if self.types[offset0] & PROCESSED != 0 {
                continue;
            }
            let mut x0 = offset0 % width;
            let mut y0 = offset0 / width;
            let mut v0 = true_edm_height(edm, image, x0, y0);

            loop {
                self.list[0] = truncate(offset0);
                self.types[offset0] |= EQUAL | LISTED;
                let mut list_len = 1usize;
                let mut list_i = 0usize;
                let mut sorting_error = false;
                let mut max_possible = true;
                let (mut x_equal, mut y_equal) = (x0 as f64, y0 as f64);
                let mut n_equal = 1u32;

                while list_i < list_len {
                    let offset = self.list[list_i] as usize;
                    let (x, y) = (offset % width, offset / width);
                    let inner = image.is_inner(x, y);
                    for d in 0..8 {
                        if !(inner || image.is_within(x, y, d)) {
                            continue;
                        }
                        let (x2, y2) = image.step(x, y, d);
                        let offset2 = y2 * width + x2;
                        if self.types[offset2] & LISTED != 0 {
                            continue;
                        }
                        // Background is not part of any particle.
                        if edm[offset2] <= 0.0 {
                            continue;
                        }
                        if self.types[offset2] & PROCESSED != 0 {
                            // A higher maximum already owns this, so the one
                            // being grown is not a maximum after all.
                            max_possible = false;
                            break;
                        }
                        let v2 = true_edm_height(edm, image, x2, y2);
                        if v2 > v0 + MAX_SORTING_ERROR {
                            max_possible = false;
                            break;
                        }
                        if v2 >= v0 - TOLERANCE {
                            if v2 > v0 {
                                // The sort put this one too low. Restart from
                                // it rather than letting the lower point win.
                                sorting_error = true;
                                offset0 = offset2;
                                v0 = v2;
                                x0 = x2;
                                y0 = y2;
                            }
                            self.list[list_len] = truncate(offset2);
                            list_len += 1;
                            self.types[offset2] |= LISTED;
                            // `v2 == v0` is tested *after* the update above, so
                            // the point that caused a sorting error is itself
                            // counted as equal. Reordering these two is a
                            // one-line change that moves the maximum's centre.
                            if v2 == v0 {
                                self.types[offset2] |= EQUAL;
                                x_equal += x2 as f64;
                                y_equal += y2 as f64;
                                n_equal += 1;
                            }
                        }
                    }
                    list_i += 1;
                }

                if sorting_error {
                    for &point in &self.list[..list_len] {
                        self.types[point as usize] = 0;
                    }
                    continue;
                }

                let reset = !(if max_possible { LISTED } else { LISTED | EQUAL });
                x_equal /= f64::from(n_equal);
                y_equal /= f64::from(n_equal);
                let mut min_distance = 1e20f64;
                let mut nearest = 0usize;
                for list_i in 0..list_len {
                    let offset = self.list[list_i] as usize;
                    let (x, y) = (offset % width, offset / width);
                    self.types[offset] &= reset;
                    self.types[offset] |= PROCESSED;
                    if max_possible {
                        self.types[offset] |= MAX_AREA;
                        if self.types[offset] & EQUAL != 0 {
                            let (dx, dy) = (x_equal - x as f64, y_equal - y as f64);
                            let distance = dx * dx + dy * dy;
                            if distance < min_distance {
                                min_distance = distance;
                                nearest = list_i;
                            }
                        }
                    }
                }
                if max_possible {
                    self.types[self.list[nearest] as usize] |= MAX_POINT;
                }
                break;
            }
        }
    }

    /// `make8bit` for an EDM: quantise to 1–254, background to 0, and the
    /// surviving maxima pinned at 255.
    fn make_8bit(&mut self, edm: &[f32], global_max: f32) {
        // The EDM branch fixes both of these: anything below half a pixel is
        // background, and one pixel of distance is the lowest real level.
        let threshold = 0.5f64;
        let min_value = 1.0f64;
        let span = f64::from(global_max) - min_value;
        let offset = min_value - span * (1.0 / 253.0 / 2.0 - 1e-6);
        // "with EDM, no better resolution": one grey level per pixel of
        // distance, never finer, however small the blob.
        let factor = (253.0 / span).min(1.0);

        for ((level, &value), &kind) in self.levels.iter_mut().zip(edm).zip(&self.types) {
            let raw = f64::from(value);
            *level = if raw < threshold {
                0
            } else if kind & MAX_AREA != 0 {
                // The dilation starts from here and nowhere else.
                255
            } else {
                // `1 + Math.round(v)`. Java's `Math.round` on a double is
                // `floor(v + 0.5)`, which is *not* Rust's `round` — the two
                // differ on negative halves, and this expression can go
                // negative before the clamp.
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the value is clamped to 1..=254 immediately"
                )]
                let scaled = 1 + ((raw - offset) * factor + 0.5).floor() as i64;
                scaled.clamp(1, 254) as u8
            };
        }
    }

    /// `cleanupMaxima`: flood every maximum that lost down to its saddle and
    /// flatten it, so the dilation cannot start from it.
    fn cleanup_maxima(&mut self, image: Image) {
        let Image { width, .. } = image;
        for index in (0..self.maxima.len()).rev() {
            let offset0 = lower32(self.maxima[index]);
            if self.types[offset0] & (MAX_AREA | ELIMINATED) != 0 {
                continue;
            }
            let level = i32::from(self.levels[offset0]);
            let mut low = level + 1;
            self.list[0] = truncate(offset0);
            self.types[offset0] |= LISTED;
            let mut list_len = 1usize;
            let mut last_len = 1usize;
            let mut saddle_found = false;

            while !saddle_found && low > 0 {
                low -= 1;
                // Everything listed before this level is above the saddle if
                // one is found now, which is exactly what gets flattened.
                last_len = list_len;
                let mut list_i = 0usize;
                while list_i < list_len {
                    let offset = self.list[list_i] as usize;
                    let (x, y) = (offset % width, offset / width);
                    let inner = image.is_inner(x, y);
                    for d in 0..8 {
                        if !(inner || image.is_within(x, y, d)) {
                            continue;
                        }
                        let (nx, ny) = image.step(x, y, d);
                        let offset2 = ny * width + nx;
                        if self.types[offset2] & LISTED != 0 {
                            continue;
                        }
                        let touched_a_winner = self.types[offset2] & MAX_AREA != 0;
                        let touched_a_flattened = self.types[offset2] & ELIMINATED != 0
                            && i32::from(self.levels[offset2]) >= low;
                        if touched_a_winner || touched_a_flattened {
                            saddle_found = true;
                            break;
                        }
                        if i32::from(self.levels[offset2]) >= low
                            && self.types[offset2] & ELIMINATED == 0
                        {
                            self.list[list_len] = truncate(offset2);
                            list_len += 1;
                            self.types[offset2] |= LISTED;
                        }
                    }
                    if saddle_found {
                        break;
                    }
                    list_i += 1;
                }
            }

            for &point in &self.list[..list_len] {
                self.types[point as usize] &= !LISTED;
            }
            for &point in &self.list[..last_len] {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "loLevel is a grey level, bounded to 0..=255 by construction"
                )]
                let saddle = low as u8;
                self.levels[point as usize] = saddle;
                self.types[point as usize] |= ELIMINATED;
            }
        }
    }

    /// `watershedSegment`: dilate the 255 regions downwards level by level,
    /// forbidden by the fate table to join two of them.
    fn watershed_segment(&mut self, image: Image) {
        let Image { width, height } = image;
        self.histogram.clear();
        self.histogram.resize(256, 0);
        for &level in &self.levels {
            self.histogram[level as usize] += 1;
        }
        let capacity =
            self.levels.len() - self.histogram[0] as usize - self.histogram[255] as usize;

        self.level_start.clear();
        self.level_start.resize(256, 0);
        let mut offset = 0usize;
        let mut highest = 0usize;
        let mut max_bin = 0u32;
        for value in 1..255 {
            self.level_start[value] = offset;
            offset += self.histogram[value] as usize;
            if self.histogram[value] > 0 {
                highest = value;
            }
            max_bin = max_bin.max(self.histogram[value]);
        }

        // Every entry below `capacity` is written by the bucket fill below, so
        // this one is dead memset too.
        fit(&mut self.coordinates, capacity, 0);
        self.level_offset.clear();
        self.level_offset.resize(highest + 1, 0);
        // The coordinate packing is ImageJ's: x in the low bits, y above,
        // with the shift chosen as the smallest that fits the width.
        let shift = encode_shift(width);
        for y in 0..height {
            for x in 0..width {
                let level = self.levels[y * width + x] as usize;
                if level == 0 || level == 255 {
                    continue;
                }
                let at = self.level_start[level] + self.level_offset[level] as usize;
                self.coordinates[at] = truncate(x | (y << shift));
                self.level_offset[level] += 1;
            }
        }

        self.set_points.clear();
        self.set_points
            .resize((max_bin as usize).min(self.levels.len().div_ceil(3)), 0);

        for level in (1..=highest).rev() {
            let mut remaining = self.histogram[level];
            let mut idle = 0u32;
            while remaining > 0 && idle < 8 {
                let mut direction = 0usize;
                loop {
                    let changed = self.process_level(
                        DIRECTION_SEQUENCE[direction % 8],
                        image,
                        shift,
                        self.level_start[level],
                        remaining,
                    );
                    remaining -= changed;
                    if changed > 0 {
                        idle = 0;
                    }
                    direction += 1;
                    // `while (remaining>0 && idle++<8)`: the post-increment only
                    // happens when the first test passes.
                    if remaining == 0 {
                        break;
                    }
                    let was = idle;
                    idle += 1;
                    if was >= 8 {
                        break;
                    }
                }
            }

            if remaining > 0 && level > 1 {
                // Pixels this level could not reach are handed to the next
                // level down, but only the ones on a border — ImageJ's own
                // comment says passing all of them is too slow, and that the
                // rest are corrected later by `cleanupExtraLines`, which for an
                // EDM never runs.
                let mut next = level;
                loop {
                    next -= 1;
                    if next <= 1 || self.histogram[next] != 0 {
                        break;
                    }
                }
                let mut end = self.level_start[next] + self.histogram[next] as usize;
                for i in 0..remaining as usize {
                    let packed = self.coordinates[self.level_start[level] + i];
                    let (x, y) = unpack(packed, shift);
                    let at = y * width + x;
                    let mut carry = x == 0 || y == 0 || x == width - 1 || y == height - 1;
                    if !carry {
                        for d in 0..8 {
                            if !image.is_within(x, y, d) {
                                continue;
                            }
                            let (nx, ny) = image.step(x, y, d);
                            if self.levels[ny * width + nx] == 0 {
                                carry = true;
                                break;
                            }
                        }
                    }
                    let _ = at;
                    if carry {
                        self.coordinates[end] = packed;
                        end += 1;
                    }
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the bucket cannot hold more points than the image has pixels"
                )]
                let count = (end - self.level_start[next]) as u32;
                self.histogram[next] = count;
            }
        }
    }

    /// `processLevel`: one dilation pass in one direction, returning how many
    /// pixels it set.
    ///
    /// Pixels that were not set are compacted to the front of the level's
    /// bucket, so the next pass over the same level sees only them.
    fn process_level(
        &mut self,
        pass: usize,
        image: Image,
        shift: usize,
        level_start: usize,
        points: u32,
    ) -> u32 {
        let Image { width, height } = image;
        let (xmax, ymax) = (width - 1, height - 1);
        let mut changed = 0usize;
        let mut unchanged = 0usize;

        for i in 0..points as usize {
            let packed = self.coordinates[level_start + i];
            let (x, y) = unpack(packed, shift);
            let at = y * width + x;

            // The neighbourhood as a bitmask, in the fate table's own order:
            // north is bit 0 and it runs clockwise.
            //
            // The offset is a signed delta applied only once the bounds flag has
            // passed. Computing it up front reads out of the image at the top
            // row — `at - width` underflows there — and the `&&` is what keeps
            // that from ever being evaluated.
            let mut index = 0usize;
            let row = width as isize;
            let mut set = |bit: usize, inside: bool, delta: isize| {
                if inside && self.levels[at.wrapping_add_signed(delta)] == 255 {
                    index ^= 1 << bit;
                }
            };
            set(0, y > 0, -row);
            set(1, x < xmax && y > 0, -row + 1);
            set(2, x < xmax, 1);
            set(3, x < xmax && y < ymax, row + 1);
            set(4, y < ymax, row);
            set(5, x > 0 && y < ymax, row - 1);
            set(6, x > 0, -1);
            set(7, x > 0 && y > 0, -row - 1);

            if self.fate[index] & (1 << pass) != 0 {
                self.set_points[changed] = truncate(at);
                changed += 1;
            } else {
                self.coordinates[level_start + unchanged] = packed;
                unchanged += 1;
            }
        }

        for &point in &self.set_points[..changed] {
            self.levels[point as usize] = 255;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a level holds no more points than the image has pixels"
        )]
        let changed = changed as u32;
        changed
    }
}

/// Gives `buffer` exactly `len` entries without rewriting the ones it already
/// has.
///
/// The length is exact rather than merely sufficient, so an index past the end
/// still panics instead of landing in a previous tile's leftovers — which is the
/// whole reason not to simply let the buffer stay long.
pub(crate) fn fit<T: Copy>(buffer: &mut Vec<T>, len: usize, fill: T) {
    if buffer.len() < len {
        buffer.resize(len, fill);
    } else {
        buffer.truncate(len);
    }
}

/// `trueEdmHeight`: the interpolated ridge height at a point of the distance
/// map.
///
/// A distance map sampled on a grid rarely has its true maximum on a pixel: the
/// ridge between two touching blobs runs between them, and the highest sampled
/// pixel can sit half a pixel off it. Comparing raw values would therefore find
/// several maxima along one ridge and split one nucleus into several. This
/// estimates where the ridge actually is.
///
/// The estimate is a ceiling — `v + sqrt(2)/2`, the furthest a true maximum can
/// be from a sample — lowered by what each of the four opposite-neighbour pairs
/// allows. A point that is not a ridge or a maximum in any direction keeps its
/// raw value.
///
/// ImageJ's comment concedes that it ignores the `sqrt(5)` neighbours "for
/// simplicity". That inaccuracy is reproduced, not corrected.
fn true_edm_height(edm: &[f32], image: Image, x: usize, y: usize) -> f32 {
    let Image { width, height } = image;
    let at = y * width + x;
    let value = edm[at];
    if x == 0 || y == 0 || x == width - 1 || y == height - 1 || value == 0.0 {
        return value;
    }

    let mut best = value + 0.5 * SQRT2;
    let mut ridge_or_max = false;
    for d in 0..4 {
        let (x1, y1) = image.step(x, y, d);
        let (x2, y2) = image.step(x, y, (d + 4) % 8);
        let v1 = edm[y1 * width + x1];
        let v2 = edm[y2 * width + x2];
        let mut height = if value >= v1 && value >= v2 {
            ridge_or_max = true;
            (v1 + v2) / 2.0
        } else {
            v1.min(v2)
        };
        // The step to a diagonal neighbour is longer, so the ridge it implies
        // is further away.
        height += if d % 2 == 0 { 1.0 } else { SQRT2 };
        if best > height {
            best = height;
        }
    }
    if ridge_or_max { best } else { value }
}

/// The image's shape, and the neighbourhood arithmetic that goes with it.
#[derive(Debug, Clone, Copy)]
struct Image {
    width: usize,
    height: usize,
}

impl Image {
    /// Whether every one of the eight neighbours exists.
    fn is_inner(self, x: usize, y: usize) -> bool {
        x > 0 && y > 0 && x < self.width - 1 && y < self.height - 1
    }

    /// `isWithin`: whether the neighbour in direction `d` exists.
    fn is_within(self, x: usize, y: usize, d: usize) -> bool {
        let (xmax, ymax) = (self.width - 1, self.height - 1);
        match d {
            0 => y > 0,
            1 => x < xmax && y > 0,
            2 => x < xmax,
            3 => x < xmax && y < ymax,
            4 => y < ymax,
            5 => x > 0 && y < ymax,
            6 => x > 0,
            7 => x > 0 && y > 0,
            _ => false,
        }
    }

    /// The neighbour's coordinates. Only valid where [`Image::is_within`] holds.
    fn step(self, x: usize, y: usize, d: usize) -> (usize, usize) {
        #[expect(
            clippy::cast_sign_loss,
            reason = "callers check is_within first, so the sum is non-negative"
        )]
        let stepped = (
            (x as isize + DIR_X[d] as isize) as usize,
            (y as isize + DIR_Y[d] as isize) as usize,
        );
        stepped
    }
}

/// The low 32 bits of a packed maximum: its pixel offset.
///
/// Java's `(int)` cast on a `long`, which keeps the low word and discards the
/// value in the high one.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Java's (int) cast: the low word is a pixel offset and non-negative"
)]
fn lower32(packed: i64) -> usize {
    (packed as u32) as usize
}

/// `intEncodeShift`: the smallest shift whose power of two reaches the width.
fn encode_shift(width: usize) -> usize {
    let mut shift = 0usize;
    let mut reach = 1usize;
    loop {
        shift += 1;
        reach *= 2;
        if reach >= width {
            return shift;
        }
    }
}

/// Undoes [`encode_shift`]'s packing.
fn unpack(packed: u32, shift: usize) -> (usize, usize) {
    let mask = (1usize << shift) - 1;
    let packed = packed as usize;
    (packed & mask, packed >> shift)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "tiles are far smaller than a u32 index"
)]
fn truncate(index: usize) -> u32 {
    index as u32
}

/// `makeFateTable`: for each of the 256 neighbourhoods, which dilation passes
/// may claim the centre pixel.
///
/// Two rules, and the second is the one that makes this a watershed rather than
/// a dilation:
///
/// - A pixel is claimed from the direction **opposite** an occupied neighbour,
///   so a region grows outwards rather than back into itself.
/// - If the occupied neighbours form more than one run — four or more
///   transitions once the side pixels have been widened — the pixel is claimed
///   by nobody. That is a point where two regions meet, and leaving it unset is
///   what leaves a segmentation line.
fn make_fate_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    for (item, fate) in table.iter_mut().enumerate() {
        let mut set = [false; 8];
        for (i, occupied) in set.iter_mut().enumerate() {
            *occupied = item & (1 << i) != 0;
        }
        for i in 0..8 {
            if set[(i + 4) % 8] {
                *fate |= 1 << i;
            }
        }
        // A set side pixel counts as if its two adjacent corners were set too,
        // so that a straight edge is one run and not three.
        for i in (0..8).step_by(2) {
            if set[i] {
                set[(i + 1) % 8] = true;
                set[(i + 7) % 8] = true;
            }
        }
        let transitions = (0..8).filter(|&i| set[i] != set[(i + 1) % 8]).count();
        if transitions >= 4 {
            *fate = 0;
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DistanceMap;

    fn split(mask: &[u8], width: usize) -> Vec<u8> {
        let mut edm = vec![0.0f32; mask.len()];
        DistanceMap::new().make_float_edm(mask, &mut edm, width);
        let mut out = mask.to_vec();
        Segmenter::new().segment(&edm, &mut out, width);
        out
    }

    /// Four-connected components, which is how the objects downstream are
    /// counted.
    fn components(mask: &[u8], width: usize) -> usize {
        let height = mask.len() / width;
        let mut seen = vec![false; mask.len()];
        let mut count = 0;
        let mut stack = Vec::new();
        for start in 0..mask.len() {
            if mask[start] == 0 || seen[start] {
                continue;
            }
            count += 1;
            seen[start] = true;
            stack.push(start);
            while let Some(at) = stack.pop() {
                let (x, y) = (at % width, at / width);
                for (dx, dy) in [(1isize, 0isize), (-1, 0), (0, 1), (0, -1)] {
                    let Some(nx) = x.checked_add_signed(dx).filter(|&n| n < width) else {
                        continue;
                    };
                    let Some(ny) = y.checked_add_signed(dy).filter(|&n| n < height) else {
                        continue;
                    };
                    let next = ny * width + nx;
                    if mask[next] != 0 && !seen[next] {
                        seen[next] = true;
                        stack.push(next);
                    }
                }
            }
        }
        count
    }

    fn disk(mask: &mut [u8], width: usize, cx: usize, cy: usize, r: usize) {
        let height = mask.len() / width;
        for y in 0..height {
            for x in 0..width {
                let (dx, dy) = (x.abs_diff(cx), y.abs_diff(cy));
                if dx * dx + dy * dy <= r * r {
                    mask[y * width + x] = 255;
                }
            }
        }
    }

    #[test]
    fn a_single_disk_is_not_split() {
        let width = 31;
        let mut mask = vec![0u8; width * 31];
        disk(&mut mask, width, 15, 15, 11);
        assert_eq!(components(&split(&mask, width), width), 1);
    }

    #[test]
    fn two_overlapping_disks_are_split_in_two() {
        let width = 41;
        let mut mask = vec![0u8; width * 25];
        disk(&mut mask, width, 14, 12, 8);
        disk(&mut mask, width, 26, 12, 8);
        assert_eq!(components(&mask, width), 1, "they start as one blob");
        assert_eq!(components(&split(&mask, width), width), 2);
    }

    /// A bar's distance map is one long ridge within tolerance of itself, so it
    /// is one maximum and must not be chopped into segments.
    #[test]
    fn a_thin_bar_is_one_object() {
        let width = 41;
        let mut mask = vec![0u8; width * 11];
        for y in 4..7 {
            for x in 3..38 {
                mask[y * width + x] = 255;
            }
        }
        assert_eq!(components(&split(&mask, width), width), 1);
    }

    #[test]
    fn an_empty_mask_stays_empty() {
        let out = split(&[0u8; 64], 8);
        assert!(out.iter().all(|&p| p == 0));
    }

    /// The fate table forbids a pixel whose occupied neighbours form two
    /// separate runs, which is what leaves a line between two regions.
    #[test]
    fn the_fate_table_forbids_pixels_between_two_regions() {
        let table = make_fate_table();
        // North and south set, east and west clear: two runs, so nobody may
        // claim it.
        assert_eq!(table[(1 << 0) | (1 << 4)], 0);
        // North alone: one run, and the pixel is claimed from the south.
        assert_ne!(table[1 << 0], 0);
    }
}
