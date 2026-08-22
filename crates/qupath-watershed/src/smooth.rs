//! The `smoothBoundaries` chain QuPath runs over a traced outline.
//!
//! [`outline`](crate::outline) produces the vertex list `Wand.autoOutline`
//! traces. That list is *not* what a `PathObject` carries: `smoothBoundaries`
//! defaults to `true`, and `WatershedCellDetection`'s lines 925-933 put four
//! stages between the trace and the object.
//!
//! ```java
//! r = new PolygonRoi(rOrig.getInterpolatedPolygon(1, false), Roi.POLYGON);
//! r = smoothPolygonRoi(r);
//! r = new PolygonRoi(r.getInterpolatedPolygon(Math.min(2, r.getNCoordinates()*0.1), false), Roi.POLYGON);
//! pathROI = IJTools.convertToPolygonROI(r, cal, downsample, plane);
//! pathROI = ShapeSimplifier.simplifyPolygon(pathROI, downsampleSqrt/2);
//! ```
//!
//! [`smooth_boundary`] is that, and `DIVERGENCES.md` D-008 is why it exists:
//! QuPath's seam resolution compares `geometry.getArea()` of the polygon this
//! produces, which is not the enclosed pixel count and is not the traced ring's
//! area either.
//!
//! # What is recorded and what is read
//!
//! Two of the four stages are ImageJ's. `getInterpolatedPolygon` lives in
//! `ij.gui.Roi` and this checkout has only the compiled `ij-1.54p` jar — the
//! situation [`outline`](crate::outline) is in with `Wand`, handled the same
//! way. `parity/java/SmoothBoundaries.java` records every intermediate and
//! `parity_smooth_boundaries.rs` checks each of them, so a stage that is wrong
//! fails at that stage rather than at the end.
//!
//! Recording each intermediate rather than only the result is not caution for
//! its own sake. Stage three's interval is `min(2, n * 0.1)` over the count
//! **stage two** produced, so a stage two whose shape is right and whose
//! *length* is wrong silently respaces stage three.
//!
//! # Four things here are not what they look like
//!
//! - **Interpolation closes the ring.** `getInterpolatedPolygon` appends the
//!   first point to the end before walking, for any ROI that is not a line. A
//!   single pixel's four vertices go in and five come out. That extra point is
//!   what stage two's stride counts over, so dropping it as redundant changes
//!   which vertices survive the halving.
//! - **`smoothPolygonRoi` is not a 3-point moving average**, though
//!   `ShapeSimplifier.smoothPoints` in the same codebase is exactly that, is
//!   public, and is the one actually named "smooth". The loop steps `i += 2`,
//!   so it emits a point for every *other* input point: it averages each kept
//!   point with both neighbours **and halves the ring**. Using `smoothPoints`
//!   gives the right shape at twice the length, which then feeds a different
//!   interval into stage three.
//! - **A coarser downsample simplifies *less*.** The threshold is
//!   `sqrt(downsample) / 2` while the coordinates are scaled by `downsample`
//!   itself, so the polygon grows faster than the tolerance. Recorded: `ell`
//!   keeps 3 points at downsample 1 and 7 at downsample 4. At downsample 1 the
//!   scaling is the identity and the whole interaction is invisible, which is
//!   why the fixture records two.
//! - **Simplification removes points by value.** `simplifyPolygonPoints` ends
//!   at `points.removeAll(toRemove)` over a `Set<Point2>`, so if the ring
//!   visits one coordinate twice, both visits go when either is chosen. Its
//!   queue orders by `(area, x, y)` rather than by area alone, and finds
//!   entries to remove by binary search under that comparator rather than by
//!   identity. All of that is reproduced, because it decides *which* points
//!   survive on any shape with symmetry — which is what the `plus` case is for.

/// A polygon vertex, in the `f64` the chain computes in.
///
/// QuPath's `Point2` holds `f64` and ImageJ's `FloatPolygon` holds `f32`, and
/// the chain crosses between them at every stage boundary. [`smooth_boundary`]
/// rounds to `f32` where QuPath does and not elsewhere; see [`Stages`].
pub type Point = (f64, f64);

/// Every intermediate of the chain, in order.
///
/// Returned whole rather than as a final polygon because
/// `parity_smooth_boundaries.rs` checks each stage separately — see the module
/// comment on why the length of stage two matters to stage three.
#[derive(Debug, Clone, PartialEq)]
pub struct Stages {
    /// Stage 1: the ring closed and interpolated at unit spacing.
    pub interpolated: Vec<Point>,
    /// Stage 2: the averaging that halves the ring.
    pub smoothed: Vec<Point>,
    /// The interval stage three used, `min(2, n * 0.1)` over stage two's count.
    pub reinterpolate_interval: f64,
    /// Stage 3: interpolated again at that interval.
    pub reinterpolated: Vec<Point>,
    /// Stage 4a: `convertToPolygonROI`'s scaling by the downsample.
    pub scaled: Vec<Point>,
    /// Stage 4b: Visvalingam, at `sqrt(downsample) / 2`.
    pub simplified: Vec<Point>,
}

/// Runs the whole `smoothBoundaries` chain over a traced ring.
///
/// `points` is the outline as [`outline`](crate::outline) produces it: each
/// vertex once, the last joining back to the first. `downsample` is the
/// detection's, which sets both the coordinate scaling and — through its square
/// root — the simplification threshold.
#[must_use]
pub fn smooth_boundary(points: &[Point], downsample: f64) -> Stages {
    let interpolated = interpolate_polygon(points, 1.0);
    let smoothed = smooth_polygon(&interpolated);

    // `Math.min(2, r.getNCoordinates() * 0.1)`, over stage two's count.
    let reinterpolate_interval = 2.0_f64.min(smoothed.len() as f64 * 0.1);
    let reinterpolated = interpolate_polygon(&smoothed, reinterpolate_interval);

    // `IJTools.convertToPolygonROI` scales by the downsample and adds the
    // region origin. The origin is the caller's business — a tile's outline is
    // in tile coordinates — so only the scaling is here.
    let scaled: Vec<Point> = reinterpolated
        .iter()
        .map(|&(x, y)| (x * downsample, y * downsample))
        .collect();

    let mut simplified = scaled.clone();
    simplify_polygon_points(&mut simplified, downsample.sqrt() / 2.0);

    Stages {
        interpolated,
        smoothed,
        reinterpolate_interval,
        reinterpolated,
        scaled,
        simplified,
    }
}

/// `WatershedCellDetection.smoothPolygonRoi`.
///
/// The `i += 2` is the whole of it: each output point is the mean of an input
/// point and its two neighbours, but only every *other* input point produces
/// one. See the module comment — this is not a 3-point moving average, and it
/// is not `ShapeSimplifier.smoothPoints`.
///
/// The arithmetic runs in `f32`, because `FloatPolygon` holds `f32` and the
/// three-way mean is where that rounding is visible.
#[must_use]
fn smooth_polygon(points: &[Point]) -> Vec<Point> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n.div_ceil(2));
    let mut i = 0;
    while i < n {
        let minus = (i + n - 1) % n;
        let plus = (i + 1) % n;
        // The narrowing to `f32` is the behaviour, not a lapse: `FloatPolygon`
        // holds `f32`, and a three-way mean is exactly where that rounding
        // shows up in the low bits of every later stage.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "reproducing FloatPolygon's f32 storage, which the fixture pins"
        )]
        let mean = |get: fn(&Point) -> f64| -> f64 {
            let a = get(&points[minus]) as f32;
            let b = get(&points[plus]) as f32;
            let c = get(&points[i]) as f32;
            f64::from((a + b + c) / 3.0)
        };
        out.push((mean(|p| p.0), mean(|p| p.1)));
        i += 2;
    }
    out
}

/// `ij.gui.Roi.getInterpolatedPolygon(FloatPolygon, double, boolean)`, for a
/// closed non-line ROI with `smooth` false.
///
/// Ported from the bytecode of `ij-1.54p`, since the checkout has no ImageJ
/// source, and pinned by `SmoothBoundaries.json` at every call the chain makes.
///
/// # The sign of the interval chooses between two different algorithms
///
/// A closed ring's perimeter is rarely a whole multiple of the interval, so
/// something has to give at the end of the walk. ImageJ lets the caller pick,
/// and the switch is the interval's **sign**:
///
/// - **Positive** — walk at exactly this spacing and let the last segment come
///   up short. This is what the whole `smoothBoundaries` chain uses.
/// - **Negative** — iterate on the interval itself, up to 50 times, nudging it
///   by `0.66 * error / segments` until the final segment lands within `0.05`
///   of the target, keeping the closest interval if none does.
///
/// The convergence path is therefore unreachable from this module. It is
/// ported rather than stubbed for the reason given at `allow_to_adjust`.
///
/// The two also differ in the *closing point*: the adjusting path keeps it and
/// the fixed one drops it, so this returns a ring whose last vertex is not a
/// repeat of its first.
///
/// The walk itself steps by [`line_circle_intersection`] rather than by
/// arc-length accumulation — a circle of the current radius centred on the last
/// emitted point, intersected with the next segment. That is a different
/// floating-point path from `t`-parameterised interpolation and gives different
/// low bits, which is why it is followed rather than replaced.
#[must_use]
fn interpolate_polygon(points: &[Point], interval: f64) -> Vec<Point> {
    // `if (npoints < 2) return p;` — before the closing point is appended.
    if points.len() < 2 {
        return points.to_vec();
    }
    // `if (Math.abs(interval) < 0.01)` errors and returns the input unchanged.
    if interval.abs() < 0.01 {
        return points.to_vec();
    }

    // `allowToAdjust` is `interval < 0`, and the sign is the whole of it: a
    // *negative* interval asks ImageJ to nudge the spacing until the ring
    // closes evenly, a positive one asks for exactly that spacing with a short
    // last segment.
    //
    // This is the one thing in the chain the bytecode misled me on and the
    // fixture settled. Reading `dcmpg; ifge` as "adjust when non-negative" is
    // the natural way round and is backwards, and it does not fail quietly:
    // the convergence loop then runs on every call, converges on an interval
    // near 0.87 for a unit square, and returns a ring whose vertices are all
    // slightly off. What settled it was asking ImageJ directly — a 3x2
    // rectangle interpolated at 1.0, 3.0 and 0.7 comes back with steps of
    // exactly 1.0, 3.0 and 0.7 and a short final segment, which is only
    // possible if the loop never runs.
    //
    // Every call in this chain passes a positive interval, so the convergence
    // path is dead code here. It is ported anyway rather than left as a
    // `todo!()`, because a caller reaching it would otherwise get a panic in
    // place of an answer, and because the branch is what makes the sign
    // meaningful to the next reader.
    let allow_to_adjust = interval < 0.0;
    let interval = interval.abs();

    // The ring is closed by appending the first point — see the module comment.
    // `isLine()` is false for a POLYGON, so this always happens.
    let mut path: Vec<Point> = Vec::with_capacity(points.len() + 1);
    path.extend_from_slice(points);
    path.push(points[0]);
    let n = path.len();

    let length = polygon_length(&path);
    // `(int)(10 + (length * 1.5) / interval)`, the output capacity ImageJ
    // allocates. It is also the *bound* on how many points the walk may emit:
    // reproduced because a walk that would exceed it writes past the array in
    // Java, and here it decides where the walk stops.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "ImageJ's own `(int)` cast; the bound must truncate the same way"
    )]
    let max_points = (10.0 + (length * 1.5) / interval) as usize;

    let mut best_interval = 0.0_f64;
    let mut min_diff = 1.0e9_f64;
    let mut current = interval;
    let mut n_out = 0_usize;

    let mut xs = vec![0.0_f64; max_points];
    let mut ys = vec![0.0_f64; max_points];

    let max_attempts = 50_usize;
    let mut attempt = 0_usize;
    while attempt <= max_attempts {
        xs[0] = path[0].0;
        ys[0] = path[0].1;
        let mut segment = 0_usize;
        n_out = 0;
        let mut last = path[0];

        while segment < n - 1 {
            let target = (xs[n_out], ys[n_out]);
            let next = path[segment + 1];
            let hit = line_circle_intersection(
                last.0, last.1, next.0, next.1, target.0, target.1, current, true,
            );
            if let Some(point) = hit {
                last = point;
                n_out += 1;
                if n_out >= max_points {
                    break;
                }
                xs[n_out] = point.0;
                ys[n_out] = point.1;
            } else {
                segment += 1;
                last = path[segment];
            }
        }
        n_out += 1;
        if n_out >= max_points {
            break;
        }
        xs[n_out] = path[n - 1].0;
        ys[n_out] = path[n - 1].1;
        n_out += 1;

        if !allow_to_adjust {
            // The fixed-spacing path stops after one walk. The closing point is
            // dropped at the shared exit below, which both paths reach; the
            // bytecode appears to decrement here as well, and taking it at face
            // value costs a vertex against the fixture.
            break;
        }

        // The convergence: how far the *last* emitted step is from the interval.
        let segments = n_out - 1;
        let dx = xs[n_out - 2] - xs[n_out - 1];
        let dy = ys[n_out - 2] - ys[n_out - 1];
        let last_step = dx.hypot(dy);
        let diff = last_step - current;
        if diff.abs() < min_diff {
            min_diff = diff.abs();
            best_interval = current;
        }
        let feedback = 0.66_f64;
        current += feedback * diff / segments as f64;

        let converged = current < 0.8 * interval || diff.abs() < 0.05;
        if (converged || attempt == max_attempts - 1) && attempt < max_attempts {
            // Re-run once with the best interval found, then stop.
            attempt = max_attempts;
            current = best_interval;
            continue;
        }
        attempt += 1;
    }

    // `if (!isLine()) nOut--;` — the closing point is dropped from the result.
    n_out = n_out.saturating_sub(1);

    // The result is a `FloatPolygon`, so every coordinate is narrowed to `f32`.
    // The walk itself runs in `f64` — ImageJ's arrays are `double[]` — and only
    // the return narrows, so doing this earlier would be wrong in both
    // directions.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "reproducing FloatPolygon's f32 storage, which the fixture pins"
    )]
    (0..n_out)
        .map(|i| (f64::from(xs[i] as f32), f64::from(ys[i] as f32)))
        .collect()
}

/// `FloatPolygon.getLength(false)`: the closed perimeter.
///
/// The path handed in already carries its closing point, so this is a plain sum
/// over consecutive pairs and not a wrap-around one.
fn polygon_length(path: &[Point]) -> f64 {
    path.windows(2)
        .map(|pair| (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1))
        .sum()
}

/// `ij.gui.Roi.lineCircleIntersection`, in the one shape the walk uses it:
/// `ignoreOutside` true, wanting the first intersection along the segment.
///
/// Returns where a circle of radius `radius` centred at `(cx, cy)` crosses the
/// segment from `(ax, ay)` to `(bx, by)`, or `None` when it does not reach.
///
/// # Why the trigonometry is kept
///
/// This is solvable with a quadratic and a dot product, and that would be both
/// faster and better conditioned. ImageJ instead takes two `atan2`s, a
/// difference of angles, and a `cos`/`sin` pair to project — and the results
/// differ in the low bits from the algebraic form. Since the whole point of
/// this module is to produce the polygon QuPath produces, the transcendental
/// path is followed rather than improved on.
#[allow(
    clippy::too_many_arguments,
    reason = "ImageJ's signature; grouping the coordinates into pairs would make \
              this harder to check against the bytecode it was ported from"
)]
fn line_circle_intersection(
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    cx: f64,
    cy: f64,
    radius: f64,
    ignore_outside: bool,
) -> Option<Point> {
    let dx_c = cx - ax;
    let dy_c = cy - ay;
    let len_c = dx_c.hypot(dy_c);

    let dx_line = bx - ax;
    let dy_line = by - ay;
    let len_line = dx_line.hypot(dy_line);

    let line_angle = dy_line.atan2(dx_line);
    let circle_angle = dy_c.atan2(dx_c);
    let angle = line_angle - circle_angle;

    // The centre, resolved along and across the segment's direction.
    let along = len_c * angle.cos();
    let across = len_c * angle.sin();

    if across.abs() > radius {
        return None;
    }
    let half_chord = (radius * radius - across * across).sqrt();
    let first = along - half_chord;
    let second = along + half_chord;

    // Both roots are considered in order, and the first that survives the
    // `ignoreOutside` test is the one the walk takes. `first` is the nearer
    // one, which for a forward walk is behind the centre and normally outside
    // the segment; taking it when it is *not* is the behaviour, not a bug.
    for distance in [first, second] {
        let outside = distance < 0.0 || distance > len_line;
        if outside && ignore_outside {
            continue;
        }
        return Some((
            ax + line_angle.cos() * distance,
            ay + line_angle.sin() * distance,
        ));
    }
    None
}

/// `ShapeSimplifier.simplifyPolygonPoints`: Visvalingam, with QuPath's own
/// tie-breaking and its by-value removal.
///
/// Modifies `points` in place, as QuPath's does.
///
/// # Three details that decide the output and are not the algorithm
///
/// - **Ordering is `(area, x, y)`, not area.** `PointWithArea.compareTo` falls
///   through to the coordinates when areas are equal, so removal order is fully
///   determined even on a symmetric shape. A port breaking ties by insertion
///   order agrees on every asymmetric input and diverges on `plus`.
/// - **Removal is by value.** The last line is `points.removeAll(toRemove)`
///   over a `Set<Point2>`, so a coordinate the ring visits twice loses both
///   visits when either is chosen. Reproduced with the same test.
/// - **The area is monotonic, not local.** When a point's area is below the
///   running maximum it is *raised* to it before being recorded, so an
///   inflection cannot make a later point cheaper than an earlier one. That is
///   Visvalingam's standard fix and it is in QuPath, but it changes what
///   `maxArea` means to anything reading it.
///
/// The loop stops at the first point whose *altitude* — twice the area over the
/// base joining its neighbours — exceeds `altitude_threshold`. It `break`s
/// rather than skipping, so one expensive point ends the simplification even if
/// cheaper ones remain behind it in the queue.
fn simplify_polygon_points(points: &mut Vec<Point>, altitude_threshold: f64) {
    if points.len() <= 1 {
        return;
    }
    remove_duplicates(points);
    if points.len() <= 3 {
        return;
    }
    let n = points.len();

    // The doubly-linked ring, by index. Java threads `PointWithArea` objects
    // together; indices are the same structure without the allocation, and the
    // queue below holds indices for the same reason.
    let mut previous: Vec<usize> = (0..n).map(|i| (i + n - 1) % n).collect();
    let mut next: Vec<usize> = (0..n).map(|i| (i + 1) % n).collect();
    let mut areas: Vec<f64> = (0..n)
        .map(|i| triangle_area(points[previous[i]], points[i], points[next[i]]))
        .collect();

    // `MinimalPriorityQueue`: a list kept sorted in *reverse* order, polled
    // from the back. Sorting is lazy — it happens on the first poll, not on
    // insertion — but every later insert and remove is a binary search, so the
    // observable ordering is the comparator's throughout.
    let mut queue: Vec<usize> = (0..n).collect();
    let key = |areas: &[f64], points: &[Point], i: usize| (areas[i], points[i].0, points[i].1);
    queue.sort_by(|&a, &b| {
        // Reverse order, so the smallest is last and `poll` pops the back.
        compare_key(key(&areas, points, b), key(&areas, points, a))
    });

    let mut max_area = 0.0_f64;
    let min_size = (n / 100).max(3);
    let mut to_remove: Vec<Point> = Vec::new();

    while queue.len() > min_size {
        let Some(current) = queue.pop() else { break };

        let base = distance(points[next[current]], points[previous[current]]);
        let altitude = areas[current] * 2.0 / base;
        if altitude > altitude_threshold {
            break;
        }

        if areas[current] < max_area {
            areas[current] = max_area;
        } else {
            max_area = areas[current];
        }

        to_remove.push(points[current]);

        let before = previous[current];
        let after = next[current];

        // `queue.remove` is a binary search under the comparator, so it finds
        // *an* entry comparing equal rather than this one. Same here: identical
        // area and coordinates make two entries interchangeable, which is the
        // recorded behaviour and not something to tighten to an identity test.
        remove_from_queue(&mut queue, before, &areas, points);
        remove_from_queue(&mut queue, after, &areas, points);

        next[before] = after;
        previous[after] = before;
        areas[before] = triangle_area(
            points[previous[before]],
            points[before],
            points[next[before]],
        );
        areas[after] = triangle_area(points[previous[after]], points[after], points[next[after]]);

        insert_into_queue(&mut queue, before, &areas, points);
        insert_into_queue(&mut queue, after, &areas, points);
    }

    // `points.removeAll(toRemove)`, by value. See the doc comment.
    points.retain(|p| !to_remove.iter().any(|q| q == p));
}

/// The comparator behind `PointWithArea.compareTo`: area, then x, then y.
///
/// Java's `compareTo` uses `<` and `>` on primitives, so it never sees a NaN as
/// anything but "equal on this key and fall through". `partial_cmp` with an
/// `Equal` fallback is that.
fn compare_key(a: (f64, f64, f64), b: (f64, f64, f64)) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let step = |x: f64, y: f64| x.partial_cmp(&y).unwrap_or(Ordering::Equal);
    step(a.0, b.0)
        .then_with(|| step(a.1, b.1))
        .then_with(|| step(a.2, b.2))
}

/// `Collections.binarySearch` under the reversed comparator, then `remove`.
fn remove_from_queue(queue: &mut Vec<usize>, index: usize, areas: &[f64], points: &[Point]) {
    let target = (areas[index], points[index].0, points[index].1);
    if let Ok(at) = queue.binary_search_by(|&other| {
        compare_key((areas[other], points[other].0, points[other].1), target).reverse()
    }) {
        queue.remove(at);
    }
    // Java throws when the entry is absent. It cannot be here — every index in
    // the ring is in the queue until it is polled, and a polled index is never
    // a neighbour of a later one, since removing it unlinked it. Silently
    // skipping rather than panicking is the one deliberate difference, and it
    // is unreachable rather than lenient.
}

/// `Collections.binarySearch`, then insert at the found or insertion point.
fn insert_into_queue(queue: &mut Vec<usize>, index: usize, areas: &[f64], points: &[Point]) {
    let target = (areas[index], points[index].0, points[index].1);
    let at = match queue.binary_search_by(|&other| {
        compare_key((areas[other], points[other].0, points[other].1), target).reverse()
    }) {
        Ok(found) | Err(found) => found,
    };
    queue.insert(at, index);
}

/// `ShapeSimplifier.removeDuplicates`: consecutive equal points, and the last
/// if it equals the first.
fn remove_duplicates(points: &mut Vec<Point>) {
    if points.is_empty() {
        return;
    }
    points.dedup();
    if points.len() > 1 && points.last() == points.first() {
        points.pop();
    }
}

/// `ShapeSimplifier.calculateArea`: the absolute area of a triangle.
fn triangle_area(p1: Point, p2: Point, p3: Point) -> f64 {
    (0.5 * (p1.0 * (p2.1 - p3.1) + p2.0 * (p3.1 - p1.1) + p3.0 * (p1.1 - p2.1))).abs()
}

fn distance(a: Point, b: Point) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit square's perimeter is exactly four intervals, so interpolating it
    /// at 1.0 must return its own corners and nothing else.
    ///
    /// This is the case that caught the inverted `allowToAdjust` sense: with the
    /// convergence loop wrongly enabled, it converges near 0.87 and every vertex
    /// comes back slightly off. Kept as a unit test as well as a fixture case
    /// because it fails with a readable coordinate rather than a stage name.
    #[test]
    fn a_unit_square_interpolates_to_its_own_corners() {
        let ring = [(2.0, 2.0), (1.0, 2.0), (1.0, 1.0), (2.0, 1.0)];
        let out = interpolate_polygon(&ring, 1.0);
        // Five, not four: the ring comes back closed, the first vertex repeated
        // as the last. That extra point is what stage two's stride counts over.
        assert_eq!(
            out,
            vec![(2.0, 2.0), (1.0, 2.0), (1.0, 1.0), (2.0, 1.0), (2.0, 2.0)]
        );
    }

    /// Interpolation walks at the interval it was given, leaving the last
    /// segment short rather than respacing to make it fit.
    ///
    /// A 3x2 rectangle has perimeter 10, which 0.7 does not divide. Every step
    /// is 0.7 and the ring simply stops when the next one would overshoot.
    #[test]
    fn a_positive_interval_is_the_spacing_and_not_a_target() {
        let ring = [(0.0, 0.0), (3.0, 0.0), (3.0, 2.0), (0.0, 2.0)];
        let out = interpolate_polygon(&ring, 0.7);
        for pair in out.windows(2) {
            let step = (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1);
            assert!((step - 0.7).abs() < 1e-4, "step {step} in {out:?}");
        }
    }

    /// Smoothing halves the ring. See the module comment: this is the property
    /// that distinguishes `smoothPolygonRoi` from the 3-point moving average
    /// sitting next to it in QuPath, and it is what sets stage three's spacing.
    #[test]
    fn smoothing_emits_one_point_per_two() {
        for n in 1..20_usize {
            let ring: Vec<Point> = (0..n).map(|i| (i as f64, 0.0)).collect();
            assert_eq!(smooth_polygon(&ring).len(), n.div_ceil(2), "n = {n}");
        }
    }

    /// Simplification removes a point by *value*, so a coordinate the ring
    /// visits twice loses both visits.
    ///
    /// Not a hypothetical: `points.removeAll(toRemove)` over a `Set<Point2>` is
    /// QuPath's last line, and a ring that doubles back — which the smoothing
    /// chain can produce on a thin shape — has repeated coordinates. Pinned so
    /// that a port "fixing" it to remove by index fails here rather than
    /// diverging silently on real tissue.
    #[test]
    fn simplification_removes_duplicated_coordinates_together() {
        // A near-flat spike visited twice, either side of a wide detour. Both
        // copies of (1, 0) are cheap and identical, so choosing one removes
        // both.
        let mut points = vec![
            (0.0, 0.0),
            (1.0, 0.0),
            (2.0, 0.0),
            (3.0, 5.0),
            (2.0, 10.0),
            (1.0, 0.0),
            (0.0, 10.0),
        ];
        simplify_polygon_points(&mut points, 1.0);
        assert!(
            !points.contains(&(1.0, 0.0)),
            "both visits should go together, got {points:?}"
        );
    }

    /// An empty or degenerate ring does not panic anywhere in the chain.
    ///
    /// Real masks never produce one, but every stage indexes modulo a length
    /// and `smooth_boundary` is public.
    #[test]
    fn degenerate_rings_survive_the_chain() {
        for ring in [vec![], vec![(1.0, 1.0)], vec![(1.0, 1.0), (2.0, 2.0)]] {
            let stages = smooth_boundary(&ring, 1.0);
            assert!(stages.simplified.len() <= ring.len().max(1) * 4);
        }
    }
}
