import ij.gui.PolygonRoi;
import ij.gui.Roi;
import ij.gui.Wand;
import ij.process.ByteProcessor;
import ij.process.FloatPolygon;
import ij.process.ImageProcessor;
import qupath.lib.geom.Point2;
import qupath.lib.roi.ShapeSimplifier;

import java.util.ArrayList;
import java.util.List;

/**
 * Records the four-stage chain {@code smoothBoundaries} runs over a traced
 * outline, so the Rust port is checked against what QuPath's objects actually
 * carry rather than against a reading of the two halves of it whose source is
 * not in this checkout.
 *
 * <p>{@code WatershedCellDetection}'s lines 925-933, which is the whole of it:
 *
 * <pre>{@code
 * r = new PolygonRoi(rOrig.getInterpolatedPolygon(1, false), Roi.POLYGON);
 * r = smoothPolygonRoi(r);
 * r = new PolygonRoi(r.getInterpolatedPolygon(Math.min(2, r.getNCoordinates()*0.1), false), Roi.POLYGON);
 * pathROI = IJTools.convertToPolygonROI(r, cal, downsample, plane);
 * pathROI = ShapeSimplifier.simplifyPolygon(pathROI, downsampleSqrt/2);
 * }</pre>
 *
 * <p><b>Why this is recorded and not ported from a reading.</b> Two of the four
 * stages are ImageJ's, not QuPath's: {@code getInterpolatedPolygon} lives in
 * {@code ij.gui.Roi}, and the checkout has only the compiled {@code ij-1.54p}
 * jar — the same situation {@code WandOutline.java} is in, handled the same
 * way. The other two are QuPath's own and their source *is* here, but they are
 * recorded alongside so the composition is pinned and not just the parts: the
 * third stage's interpolation interval depends on the point count the second
 * stage produced, so getting stage two's *length* wrong changes stage three's
 * spacing even if stage two's shape is right.
 *
 * <p><b>Three things in this chain are not what their names suggest, and each
 * gets its own recorded stage so a port that gets one wrong fails at that stage
 * rather than at the end.</b>
 *
 * <ul>
 * <li><b>{@code smoothPolygonRoi} is not a 3-point moving average</b>, despite
 * {@code ShapeSimplifier.smoothPoints} sitting in the same codebase being
 * exactly that. Its loop steps {@code i += 2}, so it emits a point for every
 * *other* input point and roughly <b>halves the ring</b> while averaging each
 * kept point with both its neighbours. Using {@code smoothPoints} here — the
 * method that is actually named "smooth", is public, and is documented as a
 * 3-point moving average — gives a polygon of the right shape with twice as
 * many points, which then feeds a different interval into stage three.</li>
 *
 * <li><b>The stage-three interval is {@code min(2, n*0.1)}</b>, so it is 2 for
 * any ring of 20 points or more and shrinks below that only for small ones.
 * {@code n} is the count *after* the halving, which is where the previous
 * bullet's error would surface.</li>
 *
 * <li><b>{@code simplifyPolygon} removes points by value, not by position.</b>
 * Its last line is {@code points.removeAll(toRemove)} where {@code toRemove} is
 * a {@code Set<Point2>} — so if the ring visits the same coordinate twice, both
 * visits die when either is chosen. The queue's ordering is
 * {@code (area, x, y)} rather than area alone, and its {@code remove} finds
 * entries by {@code binarySearch} under that comparator rather than by
 * identity, so two points with identical area and coordinates are
 * interchangeable there too. This is recorded behaviour, not a bug to fix in
 * the port.</li>
 * </ul>
 *
 * <p><b>The threshold arguments are the call site's, not defaults.</b> Stage
 * five's {@code altitudeThreshold} is {@code sqrt(downsample)/2}, so it is a
 * function of the downsample the detection ran at and not a constant. Both
 * {@code downsample = 1} (where it is 0.5) and a coarser one are recorded,
 * because a factor that happens to be 1 elsewhere in this project has more than
 * once hidden a conversion that was never exercised.
 */
public class SmoothBoundaries {

    /**
     * The downsamples recorded. 1.0 is what every small fixture here runs at
     * and gives an altitude threshold of 0.5; 4.0 gives 1.0, which is a
     * threshold large enough to actually remove points from these shapes, so
     * the simplification stage is exercised rather than merely called.
     */
    private static final double[] DOWNSAMPLES = {1.0, 4.0};

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"cases\": [\n");
        var first = new boolean[] {true};

        // A single pixel: four points in, and the chain has to survive a ring
        // shorter than the interpolation interval it will later ask for. This
        // is where `min(2, n*0.1)` is not 2, and where `simplifyPolygon`'s
        // `points.size() <= 3` early return can be reached.
        emit(out, first, "single_pixel", 3, 3, new int[] {
            0, 0, 0,
            0, 1, 0,
            0, 0, 0,
        });

        // A 2x2 block. Still four points from the Wand, but each side is two
        // units long, so stage one's interpolation at interval 1 actually adds
        // points where the single pixel's did not.
        emit(out, first, "square", 4, 4, new int[] {
            0, 0, 0, 0,
            0, 1, 1, 0,
            0, 1, 1, 0,
            0, 0, 0, 0,
        });

        // An L: the concave corner, which is the feature smoothing visibly
        // rounds and simplification is most likely to cut.
        emit(out, first, "ell", 5, 5, new int[] {
            0, 0, 0, 0, 0,
            0, 1, 0, 0, 0,
            0, 1, 0, 0, 0,
            0, 1, 1, 1, 0,
            0, 0, 0, 0, 0,
        });

        // A plus: four concave corners, and an outline whose points come in
        // pairs of equal area about two axes of symmetry — which is what makes
        // the `(area, x, y)` tiebreak in the priority queue observable rather
        // than theoretical. A port that breaks ties by area alone, or by
        // insertion order, can diverge here and nowhere simpler.
        emit(out, first, "plus", 5, 5, new int[] {
            0, 0, 0, 0, 0,
            0, 0, 1, 0, 0,
            0, 1, 1, 1, 0,
            0, 0, 1, 0, 0,
            0, 0, 0, 0, 0,
        });

        // A staircase: the densest run of direction changes a small case has,
        // so the halving in stage two lands on a different set of corners
        // depending on where the ring starts.
        emit(out, first, "staircase", 5, 5, new int[] {
            0, 0, 0, 0, 0,
            0, 1, 1, 0, 0,
            0, 0, 1, 1, 0,
            0, 0, 0, 1, 0,
            0, 0, 0, 0, 0,
        });

        // A blob large enough that the ring clears 20 points and the stage
        // three interval is pinned at 2 rather than at `n*0.1`, and large
        // enough that `minSize = max(n/100, 3)` is still 3 — the two regimes
        // the simplification loop has.
        emit(out, first, "blob", 12, 12, new int[] {
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0,
            0, 0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 0,
            0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0,
            0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0,
            0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0,
            0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0,
            0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0,
            0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0,
            0, 0, 0, 1, 1, 1, 1, 1, 1, 0, 0, 0,
            0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        });

        // Each case after the first prepends its own separator, so unlike
        // WandOutline.java there is no trailing comma to trim here.
        out.append("\n  ]\n}\n");
        System.out.print(out);
        // See GaussianBlur.java: exiting does not drain System.out's buffer,
        // and a truncated fixture is worse than a hang because it looks like a
        // recording.
        System.out.flush();
        System.exit(0);
    }

    /**
     * Traces one image's first region the way `getFilledPolygonROIs` does, then
     * records every intermediate of the smoothing chain over it, once per
     * downsample.
     *
     * <p>Only the first region found is recorded. Which regions a mask contains
     * and in what order the scan reaches them is `WandOutline.json`'s subject
     * and is already pinned there; repeating it here would duplicate a fixture
     * rather than add one.
     */
    private static void emit(StringBuilder out, boolean[] first, String name,
                             int width, int height, int[] mask) {
        var ip = new ByteProcessor(width, height);
        for (int i = 0; i < mask.length; i++) {
            ip.set(i % width, i / width, mask[i] == 0 ? 0 : 255);
        }
        // The threshold QuPath sets at the call site, on a labelled image where
        // every foreground value is at least 1.
        ip.setThreshold(0.5, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);

        int seedX = -1;
        int seedY = -1;
        for (int i = 0; i < mask.length && seedX < 0; i++) {
            if (mask[i] != 0) {
                seedX = i % width;
                seedY = i / width;
            }
        }
        var wand = new Wand(ip);
        wand.autoOutline(seedX, seedY, 0.5, Double.POSITIVE_INFINITY, Wand.FOUR_CONNECTED);
        var rOrig = new PolygonRoi(toFloatPolygon(wand), Roi.POLYGON);

        for (double downsample : DOWNSAMPLES) {
            if (!first[0]) {
                out.append(",\n");
            }
            first[0] = false;

            out.append("    {\n");
            out.append("      \"name\": \"").append(name).append("\",\n");
            out.append("      \"width\": ").append(width).append(",\n");
            out.append("      \"height\": ").append(height).append(",\n");
            out.append("      \"mask\": [");
            for (int i = 0; i < mask.length; i++) {
                if (i > 0) {
                    out.append(", ");
                }
                out.append(mask[i] == 0 ? 0 : 1);
            }
            out.append("],\n");
            out.append("      \"downsample\": ").append(downsample).append(",\n");
            out.append("      \"altitude_threshold\": ")
               .append(Math.sqrt(downsample) / 2).append(",\n");

            // Stage 0: the traced outline, before anything smooths it. Recorded
            // so a stage-one mismatch can be told apart from a Wand mismatch
            // without cross-referencing WandOutline.json's seed ordering.
            appendPolygon(out, "outline", rOrig.getFloatPolygon());

            // Stage 1: interpolate at unit spacing. `false` is `smooth`.
            var r1 = new PolygonRoi(rOrig.getInterpolatedPolygon(1, false), Roi.POLYGON);
            appendPolygon(out, "interpolated", r1.getFloatPolygon());

            // Stage 2: the averaging that halves the ring. See the class
            // comment — this is not `ShapeSimplifier.smoothPoints`.
            var r2 = smoothPolygonRoi(r1);
            appendPolygon(out, "smoothed", r2.getFloatPolygon());

            // Stage 3: interpolate again, at an interval that depends on the
            // count stage two produced.
            double interval = Math.min(2, r2.getNCoordinates() * 0.1);
            out.append("      \"reinterpolate_interval\": ").append(interval).append(",\n");
            var r3 = new PolygonRoi(r2.getInterpolatedPolygon(interval, false), Roi.POLYGON);
            appendPolygon(out, "reinterpolated", r3.getFloatPolygon());

            // Stage 4: Visvalingam. `IJTools.convertToPolygonROI` divides by the
            // downsample and adds the region origin, which is zero here; that
            // scaling is applied directly rather than through IJTools so this
            // recorder needs only qupath-core, matching what record.sh's
            // classpath provides for a ShapeSimplifier fixture.
            var poly3 = r3.getFloatPolygon();
            List<Point2> scaled = new ArrayList<>(poly3.npoints);
            for (int i = 0; i < poly3.npoints; i++) {
                scaled.add(new Point2(poly3.xpoints[i] * downsample, poly3.ypoints[i] * downsample));
            }
            appendPoints(out, "scaled", scaled);

            List<Point2> simplified = new ArrayList<>(scaled);
            ShapeSimplifier.simplifyPolygonPoints(simplified, Math.sqrt(downsample) / 2);
            appendPointsLast(out, "simplified", simplified);

            out.append("\n    }");
        }
    }

    /**
     * ImageJ's `Wand` exposes its result as `xpoints`/`ypoints` int arrays with
     * a separate `npoints`, and the arrays are longer than the outline; taking
     * their `length` records trailing zeros as vertices.
     */
    private static FloatPolygon toFloatPolygon(Wand wand) {
        var poly = new FloatPolygon();
        for (int i = 0; i < wand.npoints; i++) {
            poly.addPoint(wand.xpoints[i], wand.ypoints[i]);
        }
        return poly;
    }

    /**
     * `WatershedCellDetection.smoothPolygonRoi`, copied because it is private.
     * The `i += 2` is the whole point; see the class comment.
     */
    private static PolygonRoi smoothPolygonRoi(PolygonRoi r) {
        FloatPolygon poly = r.getFloatPolygon();
        FloatPolygon poly2 = new FloatPolygon();
        int nPoints = poly.npoints;
        for (int i = 0; i < nPoints; i += 2) {
            int iMinus = (i + nPoints - 1) % nPoints;
            int iPlus = (i + 1) % nPoints;
            poly2.addPoint((poly.xpoints[iMinus] + poly.xpoints[iPlus] + poly.xpoints[i]) / 3,
                    (poly.ypoints[iMinus] + poly.ypoints[iPlus] + poly.ypoints[i]) / 3);
        }
        return new PolygonRoi(poly2, Roi.POLYGON);
    }

    private static void appendPolygon(StringBuilder out, String key, FloatPolygon poly) {
        List<Point2> points = new ArrayList<>(poly.npoints);
        for (int i = 0; i < poly.npoints; i++) {
            points.add(new Point2(poly.xpoints[i], poly.ypoints[i]));
        }
        appendPoints(out, key, points);
    }

    private static void appendPoints(StringBuilder out, String key, List<Point2> points) {
        appendPointsInner(out, key, points);
        out.append(",\n");
    }

    private static void appendPointsLast(StringBuilder out, String key, List<Point2> points) {
        appendPointsInner(out, key, points);
    }

    private static void appendPointsInner(StringBuilder out, String key, List<Point2> points) {
        out.append("      \"").append(key).append("\": [");
        for (int i = 0; i < points.size(); i++) {
            if (i > 0) {
                out.append(", ");
            }
            out.append("[").append(points.get(i).getX())
               .append(", ").append(points.get(i).getY()).append("]");
        }
        out.append("]");
    }
}
