import ij.gui.PolygonRoi;
import ij.gui.Roi;
import ij.gui.Wand;
import ij.plugin.filter.EDM;
import ij.plugin.filter.MaximumFinder;
import ij.process.Blitter;
import ij.process.ByteProcessor;
import ij.process.FloatProcessor;
import ij.process.ImageProcessor;
import qupath.imagej.processing.MorphologicalReconstruction;
import qupath.imagej.processing.RoiLabeling;
import qupath.imagej.processing.SimpleThresholding;
import qupath.imagej.processing.Watershed;

import java.util.List;
import java.util.Random;

/**
 * Records the merge and the shape split, which is everything
 * {@code WatershedCellDetection} does between the intensity filter
 * ({@code ObjectFilters.json}) and the area filter:
 *
 * <pre>
 *     bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
 *     if (mergeAll) {
 *         bp.filter(ImageProcessor.MAX);
 *         bp.copyBits(bpLoG, 0, 0, Blitter.AND);
 *         if (watershedPostProcess) {
 *             List&lt;PolygonRoi&gt; rois2 = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);
 *             bp.setValue(255);
 *             for (Roi r : rois2)
 *                 bp.fill(r);
 *             new EDM().toWatershed(bp);
 *         }
 *     }
 * </pre>
 *
 * <p>Neither flag is a parameter a user can reach. {@code mergeAll} is passed
 * the literal {@code true} at the call site, with the comment "always use 'merge
 * all'"; {@code watershedPostProcess} is the "Split by shape" checkbox and
 * defaults on. So both branches are taken on a default run and the port needs
 * both.
 *
 * <p>Four checkpoints are recorded rather than one, because each is a separable
 * piece of work and a single end-to-end mask would say only that the whole thing
 * disagrees:
 *
 * <ul>
 * <li>{@code merged} — after the dilation, the AND, and the hole fill: the input
 *     the EDM sees.
 * <li>{@code edm} — {@code EDM.makeFloatEDM(merged, 0, false)}, the Euclidean
 *     distance map as floats.
 * <li>{@code maxima} — {@code MaximumFinder.findMaxima(edm, 0.5, NO_THRESHOLD,
 *     SEGMENTED, false, true)}, the segmentation ImageJ derives from it.
 * <li>{@code segmented} — {@code merged AND maxima}, which is what
 *     {@code toWatershed} leaves in {@code bp}.
 * </ul>
 *
 * <p>Three things decide the answer and none is visible at the call site:
 *
 * <ul>
 * <li><b>The dilation is a 3x3 maximum with a replicated border</b>, not the
 *     binary {@code DILATE} ImageJ also offers under the same
 *     {@code filter(int)} entry point. {@code MAX} takes the plain maximum of
 *     nine neighbours and reads out-of-image neighbours by clamping the
 *     coordinate ({@code ByteProcessor.getEdgePixel}); {@code DILATE} counts
 *     foreground neighbours against {@code binaryCount} and treats outside as
 *     background. {@code border-blob} is the case that separates them.
 * <li><b>The AND undoes most of the dilation.</b> The dilated mask is
 *     intersected with the thresholded LoG, so the merge only bridges a gap
 *     where the LoG was already positive — it is not a dilation of the objects.
 *     {@code gap-log-positive} and {@code gap-log-negative} are the same two
 *     blobs one pixel apart, differing only in the LoG between them, and they
 *     come back as one object and as two.
 * <li><b>The hole fill happens between the AND and the EDM</b>, so a ring closed
 *     up by the dilation is solid before the distance map is computed, and its
 *     EDM has no interior zero. {@code ring-closed-by-dilation} fails without
 *     it.
 * </ul>
 *
 * <p>The EDM's tolerance is {@code EDM.MAXFINDER_TOLERANCE}, a private
 * {@code 0.5}, and {@code isEDM} is passed {@code true} — which is not
 * cosmetic: it selects a {@code maxSortingError} of {@code 1.1 * sqrt(2)/2}
 * rather than one scaled from the value range, and it suppresses
 * {@code cleanupExtraLines}. A port that passes {@code false} agrees on most
 * images and diverges on the ones with plateaus, which is every EDM.
 */
public class MergeAndSplit {

    /** The kernel that completes the Laplacian of Gaussian. */
    private static final float[] LAPLACIAN = {0, -1, 0, -1, 4, -1, 0, -1, 0};

    /** {@code EDM.MAXFINDER_TOLERANCE}, which is private and so restated here. */
    private static final double MAXFINDER_TOLERANCE = 0.5;

    /** {@code WatershedCellDetection}'s default threshold parameter. */
    private static final double THRESHOLD = 0.1;

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"cases\": [\n");

        // Two blocks one background pixel apart, with the LoG positive in the
        // gap: the dilation bridges them and the AND lets the bridge stand, so
        // this is one object where the intensity filter left two.
        out.append(record("gap-log-positive", 15, 9,
                blocks(15, 9, new int[][] {{2, 2, 5, 5}, {8, 2, 5, 5}}),
                blocks(15, 9, new int[][] {{0, 0, 15, 9}}), true));

        // The same two blocks with the LoG negative in the gap. The dilation
        // still bridges them; the AND cuts the bridge back out. Two objects.
        // The pair is what pins the AND as doing real work rather than tidying.
        int[] logGap = blocks(15, 9, new int[][] {{0, 0, 15, 9}});
        for (int y = 0; y < 9; y++)
            logGap[y * 15 + 7] = 0;
        out.append(record("gap-log-negative", 15, 9,
                blocks(15, 9, new int[][] {{2, 2, 5, 5}, {8, 2, 5, 5}}), logGap, false));

        // A blob against every border, where the 3x3 maximum reads neighbours
        // that do not exist. MAX clamps the coordinate, so the border row
        // behaves as if the image repeated; binary DILATE would treat outside as
        // background and leave a different edge.
        out.append(record("border-blob", 12, 10,
                blocks(12, 10, new int[][] {{0, 0, 3, 3}, {9, 0, 3, 3}, {0, 7, 3, 3}, {9, 7, 3, 3}}),
                blocks(12, 10, new int[][] {{0, 0, 12, 10}}), false));

        // A ring whose gap the dilation closes. Between the AND and the EDM the
        // interior becomes solid, so the distance map has no zero inside it and
        // the split sees one blob rather than an annulus.
        int[] ringPixels = ring(15, 15, 3, 3, 9, 9);
        out.append(record("ring-closed-by-dilation", 15, 15, ringPixels,
                blocks(15, 15, new int[][] {{0, 0, 15, 15}}), false));

        // Two overlapping disks: the case the shape split exists for. The EDM
        // has two maxima with a saddle between them, and the watershed puts a
        // background line along the saddle.
        int[] dumbbell = new int[41 * 25];
        disk(dumbbell, 41, 14, 12, 8);
        disk(dumbbell, 41, 26, 12, 8);
        out.append(record("two-disks-overlapping", 41, 25, dumbbell,
                blocks(41, 25, new int[][] {{0, 0, 41, 25}}), false));

        // One disk, which must come back whole. A split here would be the
        // classic over-segmentation bug and nothing else in the fixture reports
        // it.
        int[] single = new int[31 * 31];
        disk(single, 31, 15, 15, 11);
        out.append(record("single-disk", 31, 31, single,
                blocks(31, 31, new int[][] {{0, 0, 31, 31}}), false));

        // Three disks in a row, so a split has to choose two lines and the
        // middle object is bounded on both sides.
        int[] chain = new int[57 * 25];
        disk(chain, 57, 14, 12, 8);
        disk(chain, 57, 28, 12, 8);
        disk(chain, 57, 42, 12, 8);
        out.append(record("three-disks", 57, 25, chain,
                blocks(57, 25, new int[][] {{0, 0, 57, 25}}), false));

        // A square, whose EDM is a pyramid with a ridge rather than a point
        // maximum. Plateaus are where the maximum finder's tie-breaking shows,
        // and an EDM is nothing but plateaus.
        out.append(record("square-plateau", 21, 21,
                blocks(21, 21, new int[][] {{4, 4, 13, 13}}),
                blocks(21, 21, new int[][] {{0, 0, 21, 21}}), false));

        // A long thin bar: the EDM ridge runs the length of it, and the whole
        // ridge is one maximum within tolerance. It must not be chopped up.
        out.append(record("thin-bar", 41, 11,
                blocks(41, 11, new int[][] {{3, 4, 35, 3}}),
                blocks(41, 11, new int[][] {{0, 0, 41, 11}}), false));

        // Random binary fields against random LoG masks. These produce the
        // pinches, one-pixel bridges and plateau ties nobody would think to
        // write down, and they are the cases that caught the tie-breaking.
        out.append(record("random-sparse", 48, 32, randomMask(48, 32, 61, 0.35),
                randomMask(48, 32, 71, 0.80), false));
        out.append(record("random-dense", 48, 32, randomMask(48, 32, 62, 0.60),
                randomMask(48, 32, 72, 0.90), false));

        // The real chain, on the input this stage actually meets: random pixels
        // through the blur at CMU-1's sigma, the Laplacian, the regional maxima,
        // the labelling, the flood, the tracing and the intensity filter — and
        // then the merge and the split on what that leaves.
        out.append(chained("log-chain", 96, 96, 51, 3.0));
        out.append(chained("log-chain-smooth", 80, 80, 52, 5.0));

        out.append("\n  ]\n}\n");
        System.out.print(out);
        System.out.flush();
        // See GaussianBlur.java: ImageJ's parallel filters leave non-daemon
        // workers alive, and the chained cases blur.
        System.exit(0);
    }

    /**
     * Builds the binary the way the pipeline does — blur, Laplacian, regional
     * maxima, labelling, flood, tracing, intensity filter — and then records the
     * merge and the split on it.
     *
     * <p>{@code bpLoG} here is the real {@code thresholdAbove(fpLoG, 0.0)} from
     * the same chain rather than a mask chosen by hand, which is the only way
     * the AND is exercised against a LoG that has anything to do with the
     * objects.
     */
    private static String chained(String name, int width, int height, long seed, double sigma) {
        float[] density = randomImage(width, height, seed);

        FloatProcessor fpLoG = new FloatProcessor(width, height, density.clone(), null);
        fpLoG.blurGaussian(sigma);
        fpLoG.convolve(LAPLACIAN, 3, 3);

        ByteProcessor bpLoG = SimpleThresholding.thresholdAbove(fpLoG, 0.0f);

        ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false);
        ImageProcessor ipNucleusLabels = RoiLabeling.labelImage(ipTemp, 0, false);
        Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false);

        // The intensity filter, exactly as ObjectFilters records it: the mean is
        // taken on the pre-blur density, and the test is `<=` drops.
        FloatProcessor ipToMeasure = new FloatProcessor(width, height, density.clone(), null);
        ipNucleusLabels.setThreshold(0.5, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        List<PolygonRoi> rois = RoiLabeling.getFilledPolygonROIs(ipNucleusLabels, Wand.FOUR_CONNECTED);
        ByteProcessor bp = new ByteProcessor(width, height);
        bp.setValue(255);
        for (Roi r : rois) {
            ipToMeasure.setRoi(r);
            if (ipToMeasure.getStatistics().mean <= THRESHOLD)
                continue;
            bp.fill(r);
        }
        ipToMeasure.resetRoi();

        int[] binary = new int[width * height];
        int[] log = new int[width * height];
        for (int i = 0; i < binary.length; i++) {
            binary[i] = bp.get(i);
            log[i] = bpLoG.get(i);
        }
        return emit(name, width, height, binary, log, false);
    }

    /**
     * Records the merge and the split for one hand-built pair of masks.
     *
     * <p>{@code binary} and {@code logMask} are given as 0/1 and widened to
     * 0/255 here, so the cases above read as shapes rather than as byte values.
     */
    private static String record(String name, int width, int height, int[] binary, int[] logMask, boolean first) {
        int[] bytes = new int[binary.length];
        int[] logBytes = new int[logMask.length];
        for (int i = 0; i < bytes.length; i++) {
            bytes[i] = binary[i] != 0 ? 255 : 0;
            logBytes[i] = logMask[i] != 0 ? 255 : 0;
        }
        return emit(name, width, height, bytes, logBytes, first);
    }

    /** The merge and the split themselves, shared by both kinds of case. */
    private static String emit(String name, int width, int height, int[] binary, int[] logMask, boolean first) {
        var out = new StringBuilder();
        if (!first) out.append(",\n");

        ByteProcessor bp = new ByteProcessor(width, height);
        ByteProcessor bpLoG = new ByteProcessor(width, height);
        for (int i = 0; i < binary.length; i++) {
            bp.set(i, binary[i]);
            bpLoG.set(i, logMask[i]);
        }

        // --- the call site, verbatim ---
        bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        bp.filter(ImageProcessor.MAX);
        bp.copyBits(bpLoG, 0, 0, Blitter.AND);
        List<PolygonRoi> rois2 = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);
        bp.setValue(255);
        for (Roi r : rois2)
            bp.fill(r);
        // --- toWatershed, unrolled so the EDM and the maxima are visible ---
        int[] merged = new int[width * height];
        for (int i = 0; i < merged.length; i++)
            merged[i] = bp.get(i);

        FloatProcessor floatEdm = new EDM().makeFloatEDM(bp, 0, false);
        float[] edm = (float[]) floatEdm.getPixels();

        ByteProcessor maxIp = new MaximumFinder().findMaxima(floatEdm, MAXFINDER_TOLERANCE,
                ImageProcessor.NO_THRESHOLD, MaximumFinder.SEGMENTED, false, true);
        int[] maxima = new int[width * height];
        for (int i = 0; i < maxima.length; i++)
            maxima[i] = maxIp == null ? 255 : maxIp.get(i);

        if (maxIp != null)
            bp.copyBits(maxIp, 0, 0, Blitter.AND);
        int[] segmented = new int[width * height];
        for (int i = 0; i < segmented.length; i++)
            segmented[i] = bp.get(i);

        // How many objects the split leaves, which is the number the count gap
        // is measured against — and the one thing a mask comparison would let a
        // port get right pixel-by-pixel and still report wrongly.
        bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        int objects = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED).size();

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height)
                .append(", \"mergedCount\": ").append(rois2.size())
                .append(", \"objects\": ").append(objects)
                .append(",\n     \"binary\": ");
        appendInts(out, binary);
        out.append(",\n     \"logMask\": ");
        appendInts(out, logMask);
        out.append(",\n     \"merged\": ");
        appendInts(out, merged);
        out.append(",\n     \"edm\": ");
        appendFloats(out, edm);
        out.append(",\n     \"maxima\": ");
        appendInts(out, maxima);
        out.append(",\n     \"segmented\": ");
        appendInts(out, segmented);
        out.append("}");
        return out.toString();
    }

    /** Blocks as {@code {x, y, w, h}}, set to 1. */
    private static int[] blocks(int width, int height, int[][] specs) {
        int[] mask = new int[width * height];
        for (int[] s : specs)
            for (int y = s[1]; y < s[1] + s[3]; y++)
                for (int x = s[0]; x < s[0] + s[2]; x++)
                    mask[y * width + x] = 1;
        return mask;
    }

    /** A rectangular outline one pixel thick. */
    private static int[] ring(int width, int height, int x0, int y0, int w, int h) {
        int[] mask = new int[width * height];
        for (int x = x0; x < x0 + w; x++) {
            mask[y0 * width + x] = 1;
            mask[(y0 + h - 1) * width + x] = 1;
        }
        for (int y = y0; y < y0 + h; y++) {
            mask[y * width + x0] = 1;
            mask[y * width + x0 + w - 1] = 1;
        }
        return mask;
    }

    /** A filled disk, which is the shape the split is tuned for. */
    private static void disk(int[] mask, int width, int cx, int cy, int r) {
        int height = mask.length / width;
        for (int y = Math.max(0, cy - r); y < Math.min(height, cy + r + 1); y++)
            for (int x = Math.max(0, cx - r); x < Math.min(width, cx + r + 1); x++)
                if ((x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r)
                    mask[y * width + x] = 1;
    }

    private static int[] randomMask(int width, int height, long seed, double density) {
        Random random = new Random(seed);
        int[] mask = new int[width * height];
        for (int i = 0; i < mask.length; i++)
            mask[i] = random.nextDouble() < density ? 1 : 0;
        return mask;
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

    private static void appendInts(StringBuilder out, int[] values) {
        out.append('[');
        for (int i = 0; i < values.length; i++) {
            if (i > 0) out.append(", ");
            out.append(values[i]);
        }
        out.append(']');
    }

    private static void appendFloats(StringBuilder out, float[] values) {
        out.append('[');
        for (int i = 0; i < values.length; i++) {
            if (i > 0) out.append(", ");
            out.append(values[i]);
        }
        out.append(']');
    }
}
