import ij.gui.PolygonRoi;
import ij.gui.Roi;
import ij.gui.Wand;
import ij.process.ByteProcessor;
import ij.process.FloatProcessor;
import ij.process.ImageProcessor;
import ij.process.ImageStatistics;
import qupath.imagej.processing.MorphologicalReconstruction;
import qupath.imagej.processing.RoiLabeling;
import qupath.imagej.processing.Watershed;

import java.awt.Rectangle;
import java.util.List;
import java.util.Random;

/**
 * Records the first object-level stage of {@code WatershedCellDetection}, which
 * is the one that turns flooded basins into candidate nuclei:
 *
 * <pre>
 *     ipNucleusLabels.setThreshold(0.5, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
 *     List&lt;PolygonRoi&gt; rois = RoiLabeling.getFilledPolygonROIs(ipNucleusLabels, Wand.FOUR_CONNECTED);
 *     ByteProcessor bp = new ByteProcessor(width, height);
 *     bp.setValue(255);
 *     for (Roi r : rois) {
 *         ipToMeasure.setRoi(r);
 *         double mean = ipToMeasure.getStatistics().mean;
 *         if (mean &lt;= threshold)
 *             continue;
 *         if (bpBackgroundMask != null) {
 *             bpBackgroundMask.setRoi(r);
 *             if (bpBackgroundMask.getStatistics().mean &gt; 0)
 *                 continue;
 *         }
 *         bp.fill(r);
 *     }
 * </pre>
 *
 * <p>Five things decide the answer and none of them is visible at the call site.
 * Each has a case here that fails without it.
 *
 * <ul>
 * <li><b>The tracing is not per label.</b> The Wand is given the threshold range
 *     {@code [0.5, inf)}, so it outlines whatever run of <i>labelled</i> pixels
 *     it can reach — not the pixels sharing one label. Two basins that touch
 *     with no watershed line between them come back as <b>one</b> ROI, and
 *     {@code adjacent-labels} is the case that says so. A port that iterates
 *     labels passes everything else here.
 * <li><b>Holes are filled.</b> The Wand returns an outer boundary only, so the
 *     ROI covers the basin plus anything it encloses; {@code ring-with-hole}
 *     measures a mean over pixels that carry no label at all.
 * <li><b>A basin enclosed by another is discarded outright.</b>
 *     {@code bpCompleted.fill(roi)} marks the <i>filled</i> polygon as done, so
 *     a basin sitting in another's hole is never traced and never becomes an
 *     object — see {@code nested-basin}.
 * <li><b>The mean test is {@code <=}</b>, so a basin whose mean lands exactly on
 *     the threshold is dropped. {@code mean-exactly-threshold} uses a threshold
 *     of 0.5, which is exact in both {@code float} and {@code double}, because
 *     the default 0.1 is exact in neither and the case would prove nothing.
 *     Note the <i>second</i> filter pass, after the merge, tests {@code <} on
 *     the same quantity — the two are not the same comparison.
 * <li><b>The background-mask test rejects on any overlap at all</b>, since a
 *     ByteProcessor mean over the ROI exceeds zero as soon as one pixel does.
 * </ul>
 *
 * <p>The mean is measured on {@code ipToMeasure}, which is
 * {@code fpLoG.duplicate()} taken <i>after</i> the background subtraction and
 * <i>before</i> the blur and the Laplacian. It is neither the raw optical
 * density nor the Laplacian of Gaussian the watershed ran on, and the images
 * recorded here are kept distinct from the label image for that reason.
 */
public class ObjectFilters {

    /** The kernel that completes the Laplacian of Gaussian. */
    private static final float[] LAPLACIAN = {0, -1, 0, -1, 4, -1, 0, -1, 0};

    /** {@code WatershedCellDetection}'s default threshold parameter. */
    private static final double THRESHOLD = 0.1;

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"cases\": [\n");

        // Two basins with a watershed line between them: the ordinary case, and
        // the shape every other case is a departure from.
        int[] twoBasins = blocks(15, 9, new int[][] {{2, 2, 5, 5, 1}, {8, 2, 5, 5, 2}});
        out.append(record("two-basins", 15, 9, twoBasins, uniform(15, 9, 0.4f), null, THRESHOLD, true));

        // The same two blocks with no gap. The Wand traces labelled pixels, not
        // one label, so this is a single ROI spanning both.
        int[] adjacent = blocks(15, 9, new int[][] {{2, 2, 5, 5, 1}, {7, 2, 5, 5, 2}});
        out.append(record("adjacent-labels", 15, 9, adjacent, uniform(15, 9, 0.4f), null, THRESHOLD, false));

        // A ring: the hole carries no label, and the filled ROI measures it
        // anyway. The hole is set well below the threshold so that including it
        // is the difference between keeping and dropping the object.
        int[] ring = ring(13, 13, 2, 2, 9, 9, 1);
        float[] holeIsDark = uniform(13, 13, 0.6f);
        fill(holeIsDark, 13, 3, 3, 7, 7, -1.0f);
        out.append(record("ring-with-hole", 13, 13, ring, holeIsDark, null, THRESHOLD, false));

        // A basin inside another basin's hole. `bpCompleted.fill` covers it
        // before the scan reaches it, so it is never traced at all.
        int[] nested = ring(13, 13, 2, 2, 9, 9, 1);
        fill(nested, 13, 5, 5, 3, 3, 2);
        out.append(record("nested-basin", 13, 13, nested, uniform(13, 13, 0.6f), null, THRESHOLD, false));

        // Two labelled pixels sharing only a corner, against a four-connected
        // Wand: two ROIs, not one.
        int[] diagonal = new int[11 * 11];
        diagonal[5 * 11 + 5] = 1;
        diagonal[6 * 11 + 6] = 2;
        out.append(record("diagonal-touch", 11, 11, diagonal, uniform(11, 11, 0.5f), null, THRESHOLD, false));

        // A ring broken by one pixel, so its interior reaches the outside
        // orthogonally and is not a hole. The pair with `ring-with-hole` is
        // what pins the fill rather than the tracing.
        int[] broken = ring(13, 13, 2, 2, 9, 9, 1);
        broken[6 * 13 + 10] = 0;
        out.append(record("broken-ring", 13, 13, broken, uniform(13, 13, 0.6f), null, THRESHOLD, false));

        // A ring whose interior escapes only by a diagonal step. Whether that
        // counts as a hole is the one thing here a reading of the source does
        // not settle, so it is recorded rather than reasoned about.
        int[] pinched = ring(13, 13, 2, 2, 9, 9, 1);
        pinched[6 * 13 + 10] = 0;
        pinched[7 * 13 + 10] = 0;
        pinched[6 * 13 + 11] = 1;
        out.append(record("diagonal-pinch", 13, 13, pinched, uniform(13, 13, 0.6f), null, THRESHOLD, false));

        // Basins running off every edge, where the traced polygon meets the
        // image bounds and the ROI's own mask is clipped.
        int[] border = blocks(12, 10, new int[][] {
                {0, 0, 4, 3, 1}, {8, 0, 4, 3, 2}, {0, 7, 4, 3, 3}, {8, 7, 4, 3, 4}});
        out.append(record("border-basins", 12, 10, border, uniform(12, 10, 0.5f), null, THRESHOLD, false));

        // Exactly on the threshold. 0.5 is exact in float and in double, so the
        // mean lands on the boundary rather than near it, and `<=` drops it.
        int[] one = blocks(9, 9, new int[][] {{2, 2, 5, 5, 1}});
        out.append(record("mean-exactly-threshold", 9, 9, one, uniform(9, 9, 0.5f), null, 0.5, false));

        // The same object with one pixel raised, so the mean clears the
        // threshold by the smallest amount the image can express.
        float[] justOver = uniform(9, 9, 0.5f);
        justOver[4 * 9 + 4] = 0.75f;
        out.append(record("mean-just-above-threshold", 9, 9, one, justOver, null, 0.5, false));

        // Pixels ImageJ's statistics refuse to count. The range test is
        // `v >= -Float.MAX_VALUE && v <= Float.MAX_VALUE`, so a NaN or an
        // infinity is left out of the sum *and* out of pixelCount — which means
        // the area a later filter compares against 10-400 um^2 is not the
        // region's size. The negative infinity is the discriminating one: a port
        // that accumulates it drops an object QuPath keeps.
        float[] nonFinite = uniform(9, 9, 0.5f);
        nonFinite[3 * 9 + 3] = Float.NaN;
        nonFinite[3 * 9 + 4] = Float.POSITIVE_INFINITY;
        nonFinite[4 * 9 + 3] = Float.NEGATIVE_INFINITY;
        out.append(record("non-finite-pixels", 9, 9, one, nonFinite, null, THRESHOLD, false));

        // The background mask, which rejects on a single overlapping pixel.
        byte[] maskHit = new byte[15 * 9];
        maskHit[4 * 15 + 3] = (byte) 255;
        out.append(record("background-mask-hit", 15, 9, twoBasins, uniform(15, 9, 0.4f), maskHit, THRESHOLD, false));

        // A mask that exists but touches neither basin: both survive, so a port
        // that rejects whenever a mask is present is caught here.
        byte[] maskMiss = new byte[15 * 9];
        maskMiss[0] = (byte) 255;
        out.append(record("background-mask-miss", 15, 9, twoBasins, uniform(15, 9, 0.4f), maskMiss, THRESHOLD, false));

        // Random binary fields, which produce diagonal pinches, nested holes and
        // border contacts in quantity and without anyone choosing them. These
        // are the cases that cover what the handmade ones did not think of.
        out.append(record("random-sparse", 32, 24, randomLabels(32, 24, 31, 0.30),
                randomImage(32, 24, 41), null, THRESHOLD, false));
        out.append(record("random-dense", 32, 24, randomLabels(32, 24, 32, 0.55),
                randomImage(32, 24, 42), null, THRESHOLD, false));

        // The real chain, on the input this stage meets in the pipeline: random
        // pixels through the blur at CMU-1's sigma, the Laplacian, the regional
        // maxima, the labelling and the flood — then measured, as detection
        // measures, on the image from before the blur.
        out.append(chained("log-chain", 96, 96, 51, 3.0, THRESHOLD));
        out.append(chained("log-chain-smooth", 64, 64, 52, 5.0, THRESHOLD));

        out.append("\n  ]\n}\n");
        System.out.print(out);
        System.out.flush();
        // See GaussianBlur.java: ImageJ's parallel filters leave non-daemon
        // workers alive, and the chained cases blur.
        System.exit(0);
    }

    /**
     * Records one run of the filter loop, exactly as detection runs it.
     *
     * <p>The traced ROIs are reported as a per-pixel region map rather than as
     * polygons: it is the filled mask that every later step consumes, and a
     * vertex list would pin the tracing without pinning the fill.
     */
    private static String record(
            String name,
            int width,
            int height,
            int[] labels,
            float[] toMeasure,
            byte[] backgroundMask,
            double threshold,
            boolean first) {

        FloatProcessor fpLabels = new FloatProcessor(width, height);
        for (int i = 0; i < labels.length; i++)
            fpLabels.setf(i, labels[i]);
        return emit(name, width, height, labels, fpLabels, toMeasure, backgroundMask, threshold, first);
    }

    /**
     * Builds the label image the way the pipeline does — blur, Laplacian,
     * regional maxima, labelling, flood — and then records the filter loop on
     * it. The measured image is the one from before the blur, which is what
     * {@code ipToMeasure} holds.
     */
    private static String chained(String name, int width, int height, long seed, double sigma, double threshold) {
        float[] density = randomImage(width, height, seed);

        FloatProcessor fpLoG = new FloatProcessor(width, height, density.clone(), null);
        fpLoG.blurGaussian(sigma);
        fpLoG.convolve(LAPLACIAN, 3, 3);

        ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false);
        ImageProcessor ipLabels = RoiLabeling.labelImage(ipTemp, 0, false);
        Watershed.doWatershed(fpLoG, ipLabels, 0, false);

        int[] labels = new int[width * height];
        for (int i = 0; i < labels.length; i++)
            labels[i] = (int) ipLabels.getf(i);

        return emit(name, width, height, labels, ipLabels, density, null, threshold, false);
    }

    /** The filter loop itself, shared by both kinds of case. */
    private static String emit(
            String name,
            int width,
            int height,
            int[] labels,
            ImageProcessor ipLabels,
            float[] toMeasure,
            byte[] backgroundMask,
            double threshold,
            boolean first) {

        var out = new StringBuilder();
        if (!first) out.append(",\n");

        FloatProcessor ipToMeasure = new FloatProcessor(width, height, toMeasure.clone(), null);
        ByteProcessor bpBackgroundMask = backgroundMask == null
                ? null
                : new ByteProcessor(width, height, backgroundMask.clone(), null);

        ipLabels.setThreshold(0.5, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        List<PolygonRoi> rois = RoiLabeling.getFilledPolygonROIs(ipLabels, Wand.FOUR_CONNECTED);

        int[] regions = new int[width * height];
        var areas = new StringBuilder();
        var means = new StringBuilder();
        var kept = new StringBuilder();

        ByteProcessor bp = new ByteProcessor(width, height);
        bp.setValue(255);
        int overlaps = 0;
        for (int i = 0; i < rois.size(); i++) {
            Roi r = rois.get(i);
            overlaps += paint(regions, width, height, r, i + 1);

            ipToMeasure.setRoi(r);
            ImageStatistics stats = ipToMeasure.getStatistics();
            double mean = stats.mean;

            boolean keep = mean > threshold;
            if (keep && bpBackgroundMask != null) {
                bpBackgroundMask.setRoi(r);
                if (bpBackgroundMask.getStatistics().mean > 0)
                    keep = false;
            }
            if (keep)
                bp.fill(r);

            if (i > 0) {
                areas.append(", ");
                means.append(", ");
                kept.append(", ");
            }
            areas.append(stats.pixelCount);
            means.append(mean);
            kept.append(keep ? 1 : 0);
        }

        float[] binary = new float[width * height];
        for (int i = 0; i < binary.length; i++)
            binary[i] = bp.get(i);

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height)
                .append(", \"threshold\": ").append(threshold)
                .append(", \"roiCount\": ").append(rois.size())
                .append(", \"overlaps\": ").append(overlaps)
                .append(", \"hasBackgroundMask\": ").append(bpBackgroundMask != null ? 1 : 0)
                .append(",\n     \"labels\": ");
        appendInts(out, labels);
        out.append(",\n     \"toMeasure\": ");
        appendFloats(out, toMeasure);
        out.append(",\n     \"backgroundMask\": ");
        appendInts(out, backgroundMask == null ? new int[0] : widen(backgroundMask));
        out.append(",\n     \"regions\": ");
        appendInts(out, regions);
        out.append(",\n     \"areas\": [").append(areas).append("]");
        out.append(",\n     \"means\": [").append(means).append("]");
        out.append(",\n     \"kept\": [").append(kept).append("]");
        out.append(",\n     \"binary\": ");
        appendFloats(out, binary);
        out.append("}");
        return out.toString();
    }

    /**
     * Paints an ROI's filled mask into the region map, returning how many pixels
     * it claimed from an earlier ROI.
     *
     * <p>The overlap count is recorded rather than assumed to be zero: the
     * region map is only a faithful summary of the ROI list if the filled
     * polygons are disjoint, and that is a property of the tracing rather than
     * something the call site promises.
     */
    private static int paint(int[] regions, int width, int height, Roi roi, int index) {
        Rectangle bounds = roi.getBounds();
        ImageProcessor mask = roi.getMask();
        int overlaps = 0;
        for (int y = 0; y < bounds.height; y++) {
            for (int x = 0; x < bounds.width; x++) {
                if (mask != null && mask.get(x, y) == 0)
                    continue;
                int px = bounds.x + x;
                int py = bounds.y + y;
                if (px < 0 || py < 0 || px >= width || py >= height)
                    continue;
                if (regions[py * width + px] != 0)
                    overlaps++;
                regions[py * width + px] = index;
            }
        }
        return overlaps;
    }

    /** Blocks as {@code {x, y, w, h, label}}. */
    private static int[] blocks(int width, int height, int[][] specs) {
        int[] labels = new int[width * height];
        for (int[] s : specs)
            fill(labels, width, s[0], s[1], s[2], s[3], s[4]);
        return labels;
    }

    /** A rectangular outline one pixel thick. */
    private static int[] ring(int width, int height, int x0, int y0, int w, int h, int label) {
        int[] labels = new int[width * height];
        for (int x = x0; x < x0 + w; x++) {
            labels[y0 * width + x] = label;
            labels[(y0 + h - 1) * width + x] = label;
        }
        for (int y = y0; y < y0 + h; y++) {
            labels[y * width + x0] = label;
            labels[y * width + x0 + w - 1] = label;
        }
        return labels;
    }

    private static void fill(int[] target, int width, int x0, int y0, int w, int h, int value) {
        for (int y = y0; y < y0 + h; y++)
            for (int x = x0; x < x0 + w; x++)
                target[y * width + x] = value;
    }

    private static void fill(float[] target, int width, int x0, int y0, int w, int h, float value) {
        for (int y = y0; y < y0 + h; y++)
            for (int x = x0; x < x0 + w; x++)
                target[y * width + x] = value;
    }

    private static float[] uniform(int width, int height, float value) {
        float[] pixels = new float[width * height];
        java.util.Arrays.fill(pixels, value);
        return pixels;
    }

    /** A random binary field, labelled the way the pipeline labels its maxima. */
    private static int[] randomLabels(int width, int height, long seed, double density) {
        Random random = new Random(seed);
        FloatProcessor fp = new FloatProcessor(width, height);
        for (int i = 0; i < width * height; i++)
            fp.setf(i, random.nextDouble() < density ? 1.0f : 0.0f);
        ImageProcessor ipLabels = RoiLabeling.labelImage(fp, 0, false);
        int[] labels = new int[width * height];
        for (int i = 0; i < labels.length; i++)
            labels[i] = (int) ipLabels.getf(i);
        return labels;
    }

    /**
     * Values spanning -0.5 to 2.0, the range deconvolved optical density
     * occupies once the clamp at zero is gone.
     */
    private static float[] randomImage(int width, int height, long seed) {
        Random random = new Random(seed);
        float[] pixels = new float[width * height];
        for (int i = 0; i < pixels.length; i++)
            pixels[i] = (float) (random.nextDouble() * 2.5 - 0.5);
        return pixels;
    }

    private static int[] widen(byte[] values) {
        int[] out = new int[values.length];
        for (int i = 0; i < values.length; i++)
            out[i] = values[i] & 0xff;
        return out;
    }

    private static void appendInts(StringBuilder out, int[] values) {
        out.append("[");
        for (int i = 0; i < values.length; i++) {
            if (i > 0) out.append(", ");
            out.append(values[i]);
        }
        out.append("]");
    }

    /** Float.toString round-trips exactly, so the Rust side parses back the same bits. */
    private static void appendFloats(StringBuilder out, float[] values) {
        out.append("[");
        for (int i = 0; i < values.length; i++) {
            if (i > 0) out.append(", ");
            out.append(values[i]);
        }
        out.append("]");
    }
}
