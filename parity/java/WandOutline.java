import ij.gui.Wand;
import ij.process.ByteProcessor;
import ij.process.ImageProcessor;

/**
 * Records the vertex lists ImageJ's Wand traces around filled regions, so the
 * Rust port is checked against the outline QuPath's objects actually carry
 * rather than against our reading of `Wand.java` — whose source is not in this
 * checkout, only the compiled class.
 *
 * <p>{@code ROADMAP.md} §1.5b step 4: the masks match and the polygons do not
 * exist. QuPath's {@code PathObject} geometry is this vertex list, so exporting
 * real object geometry needs the contour and not merely what it encloses, and so
 * does {@code DIVERGENCES.md} D-008 — the seam resolution compares the *area of
 * a smoothed polygon*, which is not the enclosed pixel count.
 *
 * <p><b>The call site, not the class that sounds right.</b>
 * {@code WatershedCellDetection} reaches the Wand three times and only the last
 * survives as geometry: the ROIs at its lines 798 and 831 are intermediate, used
 * for a mean-intensity check and then filled back into a binary mask. Line 870's
 * {@code RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED)} is the one
 * whose polygons become objects. Four-connected, and that is not the default —
 * {@code autoOutline}'s no-mode overloads use the legacy mode, which traces a
 * different boundary.
 *
 * <p><b>What "filled" means here, and why it is in the name.</b> The Wand walks
 * an outer boundary, so a region with a hole in it comes back as though the hole
 * were solid. That is QuPath's behaviour and not an approximation of it —
 * `getFilledPolygonROIs` says so in its own comment — and it is worth recording
 * explicitly, because a port that reproduced holes faithfully would be *wrong*
 * against QuPath while looking more correct in isolation. The `donut` case
 * exists to pin exactly that.
 *
 * <p>The cases are small and hand-checkable, and each one isolates a decision a
 * tracer has to make: where the vertices sit relative to the pixels, which
 * direction the walk goes, whether collinear runs are emitted as points, and
 * what happens at a diagonal touch — which is the case four- and eight-connected
 * disagree about, and the reason the mode is pinned rather than assumed.
 */
public class WandOutline {

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"cases\": [\n");

        // One pixel. The smallest closed outline there is, and it answers the
        // question every other case depends on: whether a vertex sits on the
        // pixel's centre or on its corner. A tracer that is half a pixel out
        // is half a pixel out everywhere, and on a single pixel that is the
        // whole shape.
        emit(out, "single_pixel", 3, 3, new int[] {
            0, 0, 0,
            0, 1, 0,
            0, 0, 0,
        });

        // A 2x2 block: still convex, but now every side has length two, so a
        // tracer that emits one vertex per pixel rather than one per corner
        // shows up here and not above.
        emit(out, "square", 4, 4, new int[] {
            0, 0, 0, 0,
            0, 1, 1, 0,
            0, 1, 1, 0,
            0, 0, 0, 0,
        });

        // A horizontal bar. Collinear pixels along a straight edge: does the
        // Wand emit a vertex at each, or only at the corners where the
        // direction changes?
        emit(out, "bar", 6, 3, new int[] {
            0, 0, 0, 0, 0, 0,
            0, 1, 1, 1, 1, 0,
            0, 0, 0, 0, 0, 0,
        });

        // An L. The first case with a concave corner, which is where a walk
        // that only ever turns one way goes wrong.
        emit(out, "ell", 5, 5, new int[] {
            0, 0, 0, 0, 0,
            0, 1, 0, 0, 0,
            0, 1, 0, 0, 0,
            0, 1, 1, 1, 0,
            0, 0, 0, 0, 0,
        });

        // A plus: four concave corners, and the outline has to come back to
        // where it started having turned both ways repeatedly.
        emit(out, "plus", 5, 5, new int[] {
            0, 0, 0, 0, 0,
            0, 0, 1, 0, 0,
            0, 1, 1, 1, 0,
            0, 0, 1, 0, 0,
            0, 0, 0, 0, 0,
        });

        // A ring. The hole is *not* traced: `getFilledPolygonROIs` fills it,
        // and an object here has no interior boundary. See the class comment —
        // reproducing the hole would be wrong against QuPath.
        emit(out, "donut", 5, 5, new int[] {
            0, 0, 0, 0, 0,
            0, 1, 1, 1, 0,
            0, 1, 0, 1, 0,
            0, 1, 1, 1, 0,
            0, 0, 0, 0, 0,
        });

        // Two blocks touching only at a corner. Four-connected says these are
        // two regions and the outline started at the upper one closes without
        // reaching the lower; eight-connected would swallow both. This is the
        // case that makes `FOUR_CONNECTED` a recorded fact rather than a
        // parameter somebody guessed.
        emit(out, "diagonal_touch", 5, 5, new int[] {
            0, 0, 0, 0, 0,
            0, 1, 0, 0, 0,
            0, 0, 1, 0, 0,
            0, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        });

        // A region running into the image edge on two sides, where the Wand
        // cannot step outside the raster to walk around it.
        emit(out, "edge", 4, 4, new int[] {
            1, 1, 0, 0,
            1, 1, 0, 0,
            0, 0, 0, 0,
            0, 0, 0, 0,
        });

        // A staircase: alternating single-pixel steps, the densest sequence of
        // direction changes a small case can have.
        emit(out, "staircase", 5, 5, new int[] {
            0, 0, 0, 0, 0,
            0, 1, 1, 0, 0,
            0, 0, 1, 1, 0,
            0, 0, 0, 1, 0,
            0, 0, 0, 0, 0,
        });

        out.setLength(out.length() - 2);
        out.append("\n  ]\n}\n");
        System.out.print(out);
        // See GaussianBlur.java: exiting does not drain System.out's buffer,
        // and a truncated fixture is worse than a hang because it looks like a
        // recording.
        System.out.flush();
        System.exit(0);
    }

    /**
     * Traces one image the way `getFilledPolygonROIs` does and records every
     * outline found, in the order that scan finds them.
     *
     * <p>The scan order is part of the recording. `getFilledPolygonROIs` walks
     * rows top to bottom and, within a row, left to right, tracing from the
     * first pixel of a region it has not already completed — so which pixel an
     * outline *starts* at is determined, and a port that starts elsewhere
     * produces the same closed shape rotated into a different list. Recording
     * the list as written is what makes that difference visible.
     */
    private static void emit(StringBuilder out, String name, int width, int height, int[] mask) {
        var ip = new ByteProcessor(width, height);
        for (int i = 0; i < mask.length; i++) {
            ip.set(i % width, i / width, mask[i] == 0 ? 0 : 255);
        }
        // The threshold QuPath sets at the call site, on a labelled image where
        // every foreground value is at least 1.
        ip.setThreshold(0.5, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);

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
        out.append("      \"outlines\": [\n");

        // `getFilledPolygonROIs`'s own loop, with the completed-mask bookkeeping
        // it uses to avoid tracing one region twice. Reproduced rather than
        // called so that each outline can be recorded separately; filling is by
        // flood from the traced polygon's own points, since `ByteProcessor.fill`
        // needs a Roi and the point of this fixture is the polygon before it
        // becomes one.
        var completed = new ByteProcessor(width, height);
        boolean first = true;
        for (int y = 0; y < height; y++) {
            for (int x = 0; x < width; x++) {
                if (completed.get(x, y) != 0 || mask[y * width + x] == 0) {
                    continue;
                }
                var wand = new Wand(ip);
                wand.autoOutline(x, y, 0.5, Double.POSITIVE_INFINITY, Wand.FOUR_CONNECTED);

                if (!first) {
                    out.append(",\n");
                }
                first = false;
                out.append("        {\"startX\": ").append(x)
                        .append(", \"startY\": ").append(y)
                        .append(", \"npoints\": ").append(wand.npoints)
                        .append(", \"x\": [");
                for (int i = 0; i < wand.npoints; i++) {
                    if (i > 0) {
                        out.append(", ");
                    }
                    out.append(wand.xpoints[i]);
                }
                out.append("], \"y\": [");
                for (int i = 0; i < wand.npoints; i++) {
                    if (i > 0) {
                        out.append(", ");
                    }
                    out.append(wand.ypoints[i]);
                }
                out.append("]}");

                markCompleted(completed, mask, width, height, x, y);
            }
        }
        out.append("\n      ]\n    },\n");
    }

    /**
     * Marks the four-connected component containing `(x, y)` as done.
     *
     * <p>Stands in for `bpCompleted.fill(roi)`, and is the same thing for these
     * cases: every one is a filled four-connected region, so the component and
     * the polygon's interior coincide. A case where they would not — a region
     * whose hole contains another region — is deliberately not here, because
     * `getFilledPolygonROIs` would report the inner one as a separate outline
     * and that is a behaviour worth its own fixture rather than a footnote in
     * this one.
     */
    private static void markCompleted(
            ByteProcessor completed, int[] mask, int width, int height, int x, int y) {
        var stack = new java.util.ArrayDeque<int[]>();
        stack.push(new int[] {x, y});
        while (!stack.isEmpty()) {
            int[] at = stack.pop();
            int cx = at[0];
            int cy = at[1];
            if (cx < 0 || cy < 0 || cx >= width || cy >= height) {
                continue;
            }
            if (completed.get(cx, cy) != 0 || mask[cy * width + cx] == 0) {
                continue;
            }
            completed.set(cx, cy, 255);
            stack.push(new int[] {cx + 1, cy});
            stack.push(new int[] {cx - 1, cy});
            stack.push(new int[] {cx, cy + 1});
            stack.push(new int[] {cx, cy - 1});
        }
    }
}
