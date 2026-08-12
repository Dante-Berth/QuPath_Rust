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
 * Records {@code refineBoundary}, the stage between the shape split
 * ({@code MergeAndSplit.json}) and the area filter:
 *
 * <pre>
 *     if (refineBoundary &amp;&amp; sigma &gt; 1.5) {
 *         FloatProcessor fpBoundaryCleanup = (FloatProcessor)fpDetection.duplicate();
 *         fpBoundaryCleanup.blurGaussian(1);
 *         fpBoundaryCleanup.convolve(new float[]{0, -1, 0, -1, 4, -1, 0, -1, 0}, 3, 3);
 *         ByteProcessor bp2 = SimpleThresholding.thresholdAbove(fpBoundaryCleanup, 0.0);
 *         bp2.copyBits(bp, 0, 0, Blitter.MIN); // Remove everything not detected in bp
 *         bp.filter(ByteProcessor.MIN);
 *         bp.copyBits(bp2, 0, 0, Blitter.MAX);
 *     }
 * </pre>
 *
 * <p>Both halves of the guard hold on a default brightfield run.
 * {@code refineBoundary} is a hard-coded {@code true} with a {@code TODO} beside
 * it, and {@code sigma} is in <em>pixels</em> — {@code sigmaMicrons / pixelSize},
 * which on CMU-1 is {@code 1.5 / 0.4990 = 3.006}. So this fires, and it is not
 * an optional refinement that a port can leave for later.
 *
 * <p><b>The image it runs on is not one the rest of the stage has.</b>
 * {@code fpDetection} is the raw deconvolved channel: before the median, before
 * the background subtraction, before the blur. It is neither {@code fpLoG} nor
 * {@code ipToMeasure}, so it is a third distinct image the pipeline has to keep.
 *
 * <p>Five checkpoints are recorded rather than one, because a single output mask
 * would say only that something disagrees:
 *
 * <ul>
 * <li>{@code detection} — the input image, so a failure can be reproduced.
 * <li>{@code binary} — {@code bp} on the way in, which is what the shape split
 *     left.
 * <li>{@code cleanup} — {@code fpBoundaryCleanup} after the sigma-1 blur and the
 *     Laplacian, as floats. Both halves are pinned already
 *     ({@code GaussianBlur.json}, {@code LaplacianConvolution.json}), so this is
 *     here to localise a failure rather than to pin anything new.
 * <li>{@code bp2} — the thresholded cleanup after {@code MIN} with {@code bp}.
 * <li>{@code refined} — {@code bp} after the whole block.
 * </ul>
 *
 * <p>Four conventions decide the answer and none is visible at the call site:
 *
 * <ul>
 * <li><b>{@code bp.filter(MIN)} reads out-of-image neighbours by clamping the
 *     coordinate</b> ({@code ByteProcessor.getEdgePixel}), so the border behaves
 *     as though the image repeated. This is the same entry point as the merge's
 *     {@code MAX} and the distinction is <em>live</em> here where it was not
 *     there: a replicated neighbour can never raise a maximum, but it very much
 *     can raise a minimum. {@code all-foreground} and
 *     {@code flat-density-border-blob} are the cases, and zero-padding the
 *     border — or using the binary {@code ERODE} that shares
 *     {@code filter(int)} and treats outside as background — fails both.
 * <li><b>{@code bp2} is masked by {@code bp} before the erosion.</b> The
 *     {@code MIN} on the line after the threshold is what stops the refinement
 *     growing an object into wherever the sigma-1 Laplacian happens to be
 *     positive. {@code positive-outside-bp} puts a bright blob where {@code bp}
 *     is empty and fails without it.
 * <li><b>The final {@code MAX} restores what the erosion removed</b>, so a
 *     boundary only moves where the finer Laplacian disagrees with it.
 *     {@code lone-pixel-kept} and {@code lone-pixel-erased} are the same single
 *     pixel with and without a peak under it.
 * <li><b>The threshold is {@code > 0.0}, strictly</b>, and on a flat image that
 *     is a question about the last bit. The interior of a constant image comes
 *     back exactly zero — the blur preserves it and the kernel sums to zero —
 *     but the ring one pixel inside the border comes back at
 *     {@code +/-2.98e-08}, rounding left over from the blur's tail-sum edge
 *     handling. So {@code all-foreground} records a {@code bp2} that is neither
 *     empty nor full but is exactly the pixels where that rounding went
 *     positive, and reproducing it needs the blur to be bit-identical at the
 *     edge rather than merely close. {@code >=} instead of {@code >} would
 *     leave {@code bp2} full and the stage would do nothing at all.
 * </ul>
 *
 * <p>{@code all-foreground} pins the clamp independently of that: its outer
 * border is where {@code bp2} is <em>not</em> set, and it survives anyway, so
 * the erosion must have read its missing neighbours as copies of itself.
 *
 * <p>{@code ByteProcessor.filter} has a separate code path for a one-pixel-wide
 * image and separate passes for its four border lines, so {@code single-column}
 * and {@code single-row} are here to check that a port with one general clamped
 * loop agrees with all of them.
 */
public class RefineBoundary {

    /** The kernel that completes the Laplacian of Gaussian. */
    private static final float[] LAPLACIAN = {0, -1, 0, -1, 4, -1, 0, -1, 0};

    /** {@code EDM.MAXFINDER_TOLERANCE}, which is private and so restated here. */
    private static final double MAXFINDER_TOLERANCE = 0.5;

    /** {@code WatershedCellDetection}'s default threshold parameter. */
    private static final double THRESHOLD = 0.1;

    /** The sigma the boundary cleanup blurs at, hard-coded at the call site. */
    private static final double CLEANUP_SIGMA = 1;

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"cases\": [\n");

        // Everything foreground, on a flat image. The Laplacian of a constant is
        // exactly zero, so bp2 is empty and the result is the bare erosion — and
        // a 3x3 minimum with a replicated border erodes nothing here, because
        // every neighbour a border pixel reads is a copy of a foreground pixel.
        // Zero-padding the border, or the binary ERODE that shares filter(int),
        // strips the outer ring and fails.
        out.append(record("all-foreground", 12, 9,
                flat(12, 9, 0.5f), blocks(12, 9, new int[][] {{0, 0, 12, 9}}), true));

        // Blobs in all four corners on a flat image: the erosion runs, and what
        // it leaves at the border is what pins the clamp.
        out.append(record("flat-density-border-blob", 14, 12, flat(14, 12, 0.5f),
                blocks(14, 12, new int[][] {{0, 0, 4, 4}, {10, 0, 4, 4}, {0, 8, 4, 4}, {10, 8, 4, 4}}),
                false));

        // The stage's own case: a bright nucleus whose boundary the sigma-3 blur
        // overestimated by two pixels. The sigma-1 Laplacian is positive over a
        // tighter region, so the erosion is allowed to stand where the two
        // disagree and is taken back where they agree.
        float[] overestimated = new float[33 * 33];
        disk(overestimated, 33, 16, 16, 8, 1.0f);
        out.append(record("blob-overestimated", 33, 33, overestimated,
                disk(new int[33 * 33], 33, 16, 16, 10), false));

        // The same blob with a binary that matches it, which is the case where
        // the stage should do as little as possible.
        out.append(record("blob-matched", 33, 33, overestimated,
                disk(new int[33 * 33], 33, 16, 16, 8), false));

        // A bright blob on the right, with bp only on the left. The sigma-1
        // Laplacian is positive over the blob, and the MIN against bp is the
        // only thing that stops the refinement claiming it.
        float[] elsewhere = new float[41 * 21];
        disk(elsewhere, 41, 30, 10, 6, 1.0f);
        out.append(record("positive-outside-bp", 41, 21, elsewhere,
                blocks(41, 21, new int[][] {{4, 6, 9, 9}}), false));

        // One isolated pixel with a peak under it: the erosion removes it and
        // the final MAX puts it back, because bp2 has it.
        float[] peak = new float[15 * 15];
        peak[7 * 15 + 7] = 4.0f;
        out.append(record("lone-pixel-kept", 15, 15, peak,
                blocks(15, 15, new int[][] {{7, 7, 1, 1}}), false));

        // The same pixel with nothing under it: the erosion removes it and
        // nothing puts it back. The pair is what pins the final MAX as doing
        // work rather than tidying.
        out.append(record("lone-pixel-erased", 15, 15, flat(15, 15, 0.5f),
                blocks(15, 15, new int[][] {{7, 7, 1, 1}}), false));

        // ByteProcessor.filter returns early for a one-pixel-wide image, and
        // handles its four border lines in passes separate from the interior. A
        // port with one general clamped loop has to agree with all of that.
        out.append(record("single-column", 1, 12, flat(1, 12, 0.5f),
                blocks(1, 12, new int[][] {{0, 2, 1, 7}}), false));
        out.append(record("single-row", 12, 1, flat(12, 1, 0.5f),
                blocks(12, 1, new int[][] {{2, 0, 7, 1}}), false));
        out.append(record("single-pixel-image", 1, 1, flat(1, 1, 0.5f),
                blocks(1, 1, new int[][] {{0, 0, 1, 1}}), false));

        // Random density against a random binary: the pinches, one-pixel
        // bridges and ties nobody would think to write down.
        out.append(record("random-sparse", 48, 32, randomImage(48, 32, 81),
                randomMask(48, 32, 91, 0.35), false));
        out.append(record("random-dense", 48, 32, randomImage(48, 32, 82),
                randomMask(48, 32, 92, 0.70), false));

        // The real chain, on the input this stage actually meets: random pixels
        // through the blur at CMU-1's sigma, the Laplacian, the regional maxima,
        // the labelling, the flood, the tracing, the intensity filter, the merge
        // and the shape split — and then the refinement on what that leaves.
        out.append(chained("log-chain", 96, 96, 51, 3.0));
        out.append(chained("log-chain-smooth", 80, 80, 52, 5.0));

        out.append("\n  ]\n}\n");
        System.out.print(out);
        System.out.flush();
        // See GaussianBlur.java: ImageJ's parallel filters leave non-daemon
        // workers alive, and every case here blurs.
        System.exit(0);
    }

    /**
     * Builds {@code bp} the way detection does — blur, Laplacian, regional
     * maxima, labelling, flood, tracing, intensity filter, merge, shape split —
     * and then records the refinement on it.
     *
     * <p>With no background subtraction {@code ipToMeasure} is {@code fpDetection}
     * itself, which is the {@code backgroundRadius == 0} branch of the call site,
     * so one image serves as both here.
     */
    private static String chained(String name, int width, int height, long seed, double sigma) {
        float[] density = randomImage(width, height, seed);

        FloatProcessor fpLoG = new FloatProcessor(width, height, density.clone(), null);
        fpLoG.blurGaussian(sigma);
        fpLoG.convolve(LAPLACIAN, 3, 3);

        ByteProcessor bpLoG = SimpleThresholding.thresholdAbove(fpLoG, 0.0);

        ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false);
        ImageProcessor ipNucleusLabels = RoiLabeling.labelImage(ipTemp, 0, false);
        Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false);

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

        // The merge and the shape split, as MergeAndSplit records them.
        bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        bp.filter(ImageProcessor.MAX);
        bp.copyBits(bpLoG, 0, 0, Blitter.AND);
        List<PolygonRoi> rois2 = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);
        bp.setValue(255);
        for (Roi r : rois2)
            bp.fill(r);
        FloatProcessor floatEdm = new EDM().makeFloatEDM(bp, 0, false);
        ByteProcessor maxIp = new MaximumFinder().findMaxima(floatEdm, MAXFINDER_TOLERANCE,
                ImageProcessor.NO_THRESHOLD, MaximumFinder.SEGMENTED, false, true);
        if (maxIp != null)
            bp.copyBits(maxIp, 0, 0, Blitter.AND);

        int[] binary = new int[width * height];
        for (int i = 0; i < binary.length; i++)
            binary[i] = bp.get(i);
        return emit(name, width, height, density, binary, false);
    }

    /**
     * Records the refinement for one hand-built pair.
     *
     * <p>{@code binary} is given as 0/1 and widened to 0/255 here, so the cases
     * above read as shapes rather than as byte values.
     */
    private static String record(String name, int width, int height, float[] detection, int[] binary, boolean first) {
        int[] bytes = new int[binary.length];
        for (int i = 0; i < bytes.length; i++)
            bytes[i] = binary[i] != 0 ? 255 : 0;
        return emit(name, width, height, detection, bytes, first);
    }

    /** The refinement itself, shared by both kinds of case. */
    private static String emit(String name, int width, int height, float[] detection, int[] binary, boolean first) {
        var out = new StringBuilder();
        if (!first) out.append(",\n");

        FloatProcessor fpDetection = new FloatProcessor(width, height, detection.clone(), null);
        ByteProcessor bp = new ByteProcessor(width, height);
        for (int i = 0; i < binary.length; i++)
            bp.set(i, binary[i]);
        bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        int objectsBefore = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED).size();

        // --- the call site, verbatim ---
        FloatProcessor fpBoundaryCleanup = (FloatProcessor) fpDetection.duplicate();
        fpBoundaryCleanup.blurGaussian(CLEANUP_SIGMA);
        fpBoundaryCleanup.convolve(LAPLACIAN, 3, 3);
        float[] cleanup = ((float[]) fpBoundaryCleanup.getPixels()).clone();

        ByteProcessor bp2 = SimpleThresholding.thresholdAbove(fpBoundaryCleanup, 0.0);
        bp2.copyBits(bp, 0, 0, Blitter.MIN); // Remove everything not detected in bp
        int[] masked = new int[width * height];
        for (int i = 0; i < masked.length; i++)
            masked[i] = bp2.get(i);

        bp.filter(ByteProcessor.MIN);
        bp.copyBits(bp2, 0, 0, Blitter.MAX);
        // --- end of the call site ---

        int[] refined = new int[width * height];
        for (int i = 0; i < refined.length; i++)
            refined[i] = bp.get(i);

        // What the area filter would be handed. A mask can agree everywhere and
        // still trace to a different number of objects if one pixel moves.
        bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        int objectsAfter = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED).size();

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height)
                .append(", \"objectsBefore\": ").append(objectsBefore)
                .append(", \"objectsAfter\": ").append(objectsAfter)
                .append(",\n     \"detection\": ");
        appendFloats(out, detection);
        out.append(",\n     \"binary\": ");
        appendInts(out, binary);
        out.append(",\n     \"cleanup\": ");
        appendFloats(out, cleanup);
        out.append(",\n     \"bp2\": ");
        appendInts(out, masked);
        out.append(",\n     \"refined\": ");
        appendInts(out, refined);
        out.append("}");
        return out.toString();
    }

    /** An image of one value, whose Laplacian is exactly zero. */
    private static float[] flat(int width, int height, float value) {
        float[] pixels = new float[width * height];
        java.util.Arrays.fill(pixels, value);
        return pixels;
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

    /** A filled disk in a binary mask. */
    private static int[] disk(int[] mask, int width, int cx, int cy, int r) {
        int height = mask.length / width;
        for (int y = Math.max(0, cy - r); y < Math.min(height, cy + r + 1); y++)
            for (int x = Math.max(0, cx - r); x < Math.min(width, cx + r + 1); x++)
                if ((x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r)
                    mask[y * width + x] = 1;
        return mask;
    }

    /** A filled disk in a float image, which is the nucleus the stage is about. */
    private static void disk(float[] image, int width, int cx, int cy, int r, float value) {
        int height = image.length / width;
        for (int y = Math.max(0, cy - r); y < Math.min(height, cy + r + 1); y++)
            for (int x = Math.max(0, cx - r); x < Math.min(width, cx + r + 1); x++)
                if ((x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r)
                    image[y * width + x] = value;
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
