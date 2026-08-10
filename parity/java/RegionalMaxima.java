import ij.process.FloatProcessor;
import ij.process.ImageProcessor;
import qupath.imagej.processing.MorphologicalReconstruction;
import qupath.imagej.processing.RoiLabeling;

import java.util.Random;

/**
 * Records the stage between the Laplacian of Gaussian and the watershed, which
 * is the one {@code qupath-pipeline} was missing:
 *
 * <pre>
 *     ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false);
 *     ImageProcessor ipNucleusLabels = RoiLabeling.labelImage(ipTemp, 0, false);
 *     Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false);
 * </pre>
 *
 * <p>Two levels are recorded, so a failure says which half is wrong:
 * {@code morphologicalReconstruction} on hand-built marker/mask pairs, and
 * {@code findRegionalMaxima} followed by {@code labelImage} on whole surfaces.
 * {@code getMaximaLabels} sits between them and is package-private, so it is
 * pinned only through what the pair of them produce.
 *
 * <p>Four things decide the answer and none of them is visible at the call site:
 *
 * <ul>
 * <li><b>The maximum test is not strict.</b> {@code getMaximaLabels} marks every
 *     pixel {@code >=} threshold that is {@code >=} all eight neighbours, so a
 *     whole plateau is marked, and so is a plateau hanging off the side of a
 *     slope. The second kind is removed later, not here — see the next point.
 * <li><b>What removes a maximum is its prominence, not its shape.</b> Marked
 *     pixels are set to negative infinity, reconstruction fills them back up by
 *     geodesic dilation under the original image, and the difference against
 *     the original is how far each maximum stands above the highest saddle that
 *     reaches it. A shoulder refills to its own value, so its prominence is
 *     zero and the labelling drops it. This is the entire reason a nucleus
 *     yields one marker rather than several, and {@code shoulder-plateau} is
 *     the case that fails without it.
 * <li><b>The labelling is 4-connected</b> — {@code labelImage(ipTemp, 0, false)}
 *     — where the maximum test that produced the pixels was 8-connected. Two
 *     maxima touching only at a corner are two markers, not one, and
 *     {@code diagonal-touch} is the case that says so.
 * <li><b>The threshold is on the value, the labelling threshold is on the
 *     prominence.</b> 0.001 gates which pixels may be maxima at all; 0 gates
 *     which survive. A port that uses one value for both passes most of these
 *     cases.
 * </ul>
 *
 * <p>Inputs run negative for the reason the blur's and the convolution's do:
 * the real input here is a Laplacian of Gaussian of unclamped optical density,
 * which is negative over most of a slide.
 */
public class RegionalMaxima {

    /** What {@code WatershedCellDetection} passes to {@code findRegionalMaxima}. */
    private static final float THRESHOLD = 0.001f;

    /** The kernel that completes the Laplacian of Gaussian. */
    private static final float[] LAPLACIAN = {0, -1, 0, -1, 4, -1, 0, -1, 0};

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"reconstructions\": [\n");

        // Reconstruction on its own. The marker is raised towards the mask by
        // geodesic dilation; every case below is a statement about which paths
        // the dilation may travel.

        // One seed under a constant mask floods everything: the fixed point is
        // the mask itself. Seeded top-left, so the forward raster pass alone
        // changes >99% of pixels and the >10% loop runs.
        out.append(reconstruction("flood-forward", 40, 40,
                seed(40, 40, 0, 0, 1.0f), constant(40, 40, 1.0f), true));
        // The same thing seeded bottom-right: the forward pass changes nothing,
        // so this goes down the backward-plus-queue path instead.
        out.append(reconstruction("flood-backward", 40, 40,
                seed(40, 40, 39, 39, 1.0f), constant(40, 40, 1.0f), false));
        // A wall the dilation cannot cross, so half the image stays at zero.
        // A port that dilates without the mask minimum fills the whole thing.
        out.append(reconstruction("blocked-by-wall", 21, 9,
                seed(21, 9, 0, 4, 1.0f), wall(21, 9, 10), false));
        // The path is longer than any raster pass can follow in one direction,
        // which is what the queue exists for.
        out.append(reconstruction("serpentine", 31, 21,
                seed(31, 21, 1, 1, 1.0f), serpentine(31, 21), false));
        // The same corridor with every turn narrowed to a diagonal step, so
        // the path leaves each leg only by a corner. Reconstruction is
        // 8-connected, and this is the case that says so where `serpentine`
        // cannot: the raster passes read both diagonals of the row above for
        // free, so a queue that propagates only orthogonally still passes
        // every other case here.
        out.append(reconstruction("diagonal-serpentine", 31, 21,
                seed(31, 21, 1, 1, 1.0f), diagonalSerpentine(31, 21), false));
        // Marker above mask: `valNew` takes the minimum, so the marker is
        // pulled down to the mask wherever it started above it.
        out.append(reconstruction("marker-above-mask", 12, 8,
                constant(12, 8, 5.0f), ramp(12, 8), false));
        // Negative infinity holes, which is the only marker findRegionalMaxima
        // ever passes. A port storing the marker as anything narrower than the
        // image breaks here.
        out.append(reconstruction("holes-at-negative-infinity", 37, 23,
                holes(random(37, 23, 11), 37, 23, 3), random(37, 23, 11), false));
        // Random marker under random mask, unrelated to each other: the general
        // case, and the only one where the two images disagree everywhere.
        out.append(reconstruction("random-pair", 29, 19,
                random(29, 19, 12), random(29, 19, 13), false));
        // Shapes with no interior. Every dilateAndCompare neighbour is either
        // the first row or the first column, which is where its four carried
        // values are seeded from the pixel itself rather than from a neighbour.
        out.append(reconstruction("thin-row", 9, 1,
                seed(9, 1, 0, 0, 1.0f), constant(9, 1, 1.0f), false));
        out.append(reconstruction("thin-column", 1, 9,
                seed(1, 9, 0, 0, 1.0f), constant(1, 9, 1.0f), false));
        out.append(reconstruction("single-pixel", 1, 1,
                constant(1, 1, 0.0f), constant(1, 1, 1.0f), false));

        out.append("\n  ],\n  \"maxima\": [\n");

        // findRegionalMaxima followed by labelImage, which is the pair of calls
        // detection makes.

        // One peak, one marker, and its prominence is its whole height.
        out.append(maxima("impulse", impulse(9, 9, 1.0f), 9, 9, true));
        // A 5x5 plateau is one maximum, not twenty-five. The non-strict test is
        // what makes this work at all: a strict one finds nothing here.
        out.append(maxima("plateau", plateau(13, 13, 4, 4, 5, 5, 1.0f), 13, 13, false));
        // The case that separates this stage from a plain maximum test. The
        // plateau touches a higher region, so every one of its pixels is marked
        // as a candidate and every one refills to its own value: prominence
        // zero, no marker. A port without the reconstruction reports a marker
        // here, and that is the over-segmentation.
        out.append(maxima("shoulder-plateau", shoulder(21, 11), 21, 11, false));
        // Two peaks over one saddle: two markers, each with prominence
        // measured from the saddle rather than from zero.
        out.append(maxima("two-peaks-one-saddle", twoPeaks(25, 9, 0.4f), 25, 9, false));
        // The same pair with the saddle raised until it is no lower than the
        // peaks, which merges them into one maximum.
        out.append(maxima("two-peaks-no-saddle", twoPeaks(25, 9, 1.0f), 25, 9, false));
        // Two single-pixel maxima touching only at a corner. 8-connected
        // labelling calls these one marker; QuPath's 4-connected call makes
        // them two.
        out.append(maxima("diagonal-touch", diagonalTouch(11, 11), 11, 11, false));
        // A peak below 0.001 is never a candidate, so the image comes back with
        // no markers at all however it is shaped.
        out.append(maxima("below-threshold", impulse(9, 9, 0.0005f), 9, 9, false));
        // And one just above it, so the comparison is pinned from both sides.
        out.append(maxima("just-above-threshold", impulse(9, 9, 0.0015f), 9, 9, false));
        // A monotone ramp has no interior maximum anywhere.
        out.append(maxima("ramp", ramp(16, 12), 16, 12, false));
        // Peaks on the image border, which the maxima scan never visits: its
        // loops start at y1+1 and stop at y2-1. They produce no marker, and a
        // port that scans the full image produces several.
        out.append(maxima("border-peaks", borderPeaks(11, 11), 11, 11, false));
        // Raw noise: every pixel is its own maximum or its own slope, so this
        // is the case with the most candidates per pixel.
        out.append(maxima("random", random(23, 17, 21), 23, 17, false));
        // The real thing, and the only case whose input has the spectral
        // content this stage meets in the pipeline: random pixels through the
        // blur at the sigma CMU-1 puts detection at, then the Laplacian.
        out.append(maxima("log-tile", logOf(96, 96, 22, 3.0), 96, 96, false));
        // The same, blurred harder, so the maxima are fewer and broader.
        out.append(maxima("log-tile-smooth", logOf(64, 64, 23, 5.0), 64, 64, false));

        out.append("\n  ]\n}\n");
        System.out.print(out);
        System.out.flush();
        // See GaussianBlur.java: ImageJ's parallel filters leave non-daemon
        // workers alive, and this recorder blurs to build its last two inputs.
        System.exit(0);
    }

    /** Records one {@code morphologicalReconstruction}, which changes the marker. */
    private static String reconstruction(
            String name, int width, int height, float[] marker, float[] mask, boolean first) {
        var out = new StringBuilder();
        if (!first) out.append(",\n");
        float[] markerIn = marker.clone();

        FloatProcessor fpMarker = new FloatProcessor(width, height, marker, null);
        FloatProcessor fpMask = new FloatProcessor(width, height, mask, null);
        MorphologicalReconstruction.morphologicalReconstruction(fpMarker, fpMask);

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height)
                .append(",\n     \"marker\": ");
        append(out, markerIn);
        out.append(",\n     \"mask\": ");
        append(out, mask);
        out.append(",\n     \"out\": ");
        append(out, (float[]) fpMarker.getPixels());
        out.append("}");
        return out.toString();
    }

    /**
     * Records {@code findRegionalMaxima} and the {@code labelImage} that
     * follows it, exactly as detection chains them.
     */
    private static String maxima(String name, float[] pixels, int width, int height, boolean first) {
        var out = new StringBuilder();
        if (!first) out.append(",\n");
        float[] input = pixels.clone();

        FloatProcessor fp = new FloatProcessor(width, height, pixels, null);
        ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fp, THRESHOLD, false);
        ImageProcessor ipLabels = RoiLabeling.labelImage(ipTemp, 0, false);

        float[] prominence = new float[width * height];
        float[] labels = new float[width * height];
        int count = 0;
        for (int i = 0; i < prominence.length; i++) {
            prominence[i] = ipTemp.getf(i);
            labels[i] = ipLabels.getf(i);
            count = Math.max(count, (int) labels[i]);
        }

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height)
                .append(", \"threshold\": ").append(THRESHOLD)
                .append(", \"labelCount\": ").append(count)
                .append(",\n     \"in\": ");
        append(out, input);
        out.append(",\n     \"prominence\": ");
        append(out, prominence);
        out.append(",\n     \"labels\": ");
        append(out, labels);
        out.append("}");
        return out.toString();
    }

    private static float[] constant(int width, int height, float value) {
        float[] pixels = new float[width * height];
        java.util.Arrays.fill(pixels, value);
        return pixels;
    }

    /** Zero everywhere but one pixel. */
    private static float[] seed(int width, int height, int x, int y, float value) {
        float[] pixels = new float[width * height];
        pixels[y * width + x] = value;
        return pixels;
    }

    private static float[] impulse(int width, int height, float value) {
        return seed(width, height, width / 2, height / 2, value);
    }

    /** A constant field split by a column of zeros the dilation cannot cross. */
    private static float[] wall(int width, int height, int column) {
        float[] pixels = constant(width, height, 1.0f);
        for (int y = 0; y < height; y++)
            pixels[y * width + column] = 0.0f;
        return pixels;
    }

    /**
     * A one-pixel corridor folded back on itself, so the geodesic path from one
     * end to the other runs against the raster direction on every other leg.
     */
    private static float[] serpentine(int width, int height) {
        float[] pixels = new float[width * height];
        for (int y = 1; y < height - 1; y += 2) {
            for (int x = 1; x < width - 1; x++)
                pixels[y * width + x] = 1.0f;
            // The turn at alternating ends, joining this leg to the next.
            int turn = (y / 2) % 2 == 0 ? width - 2 : 1;
            if (y + 1 < height - 1)
                pixels[(y + 1) * width + turn] = 1.0f;
        }
        return pixels;
    }

    /**
     * A serpentine whose legs are joined by a single pixel set one row over and
     * one column beyond the end of the leg, so the only way between two legs is
     * a pair of diagonal steps. The bridge pixel is placed outside the legs'
     * column range on purpose: put it in line with them and it acquires an
     * orthogonal neighbour, and the case stops testing anything.
     */
    private static float[] diagonalSerpentine(int width, int height) {
        float[] pixels = new float[width * height];
        int last = width - 4;   // rightmost leg column
        int bridgeRight = width - 3;
        for (int y = 1; y < height; y += 2) {
            for (int x = 1; x <= last; x++)
                pixels[y * width + x] = 1.0f;
            if (y + 2 >= height)
                continue;
            // Alternating ends, and never in a leg's column.
            int bridge = ((y - 1) / 2) % 2 == 0 ? bridgeRight : 0;
            pixels[(y + 1) * width + bridge] = 1.0f;
        }
        return pixels;
    }

    /** Increases left to right and top to bottom, so no interior pixel is a maximum. */
    private static float[] ramp(int width, int height) {
        float[] pixels = new float[width * height];
        for (int y = 0; y < height; y++)
            for (int x = 0; x < width; x++)
                pixels[y * width + x] = (x + y) * 0.05f - 0.5f;
        return pixels;
    }

    private static float[] plateau(
            int width, int height, int x0, int y0, int w, int h, float value) {
        float[] pixels = new float[width * height];
        for (int y = y0; y < y0 + h; y++)
            for (int x = x0; x < x0 + w; x++)
                pixels[y * width + x] = value;
        return pixels;
    }

    /**
     * A staircase: a flat shelf against the side of a taller block. Every shelf
     * pixel is {@code >=} all eight of its neighbours except where it meets the
     * block, so the scan marks the shelf's interior and the reconstruction
     * refills it from the one pixel it did not mark.
     */
    private static float[] shoulder(int width, int height) {
        float[] pixels = new float[width * height];
        for (int y = 2; y < height - 2; y++) {
            for (int x = 2; x < width / 2; x++)
                pixels[y * width + x] = 0.5f;
            for (int x = width / 2; x < width - 2; x++)
                pixels[y * width + x] = 1.0f;
        }
        return pixels;
    }

    /** Two peaks joined across a saddle of the given height. */
    private static float[] twoPeaks(int width, int height, float saddle) {
        float[] pixels = new float[width * height];
        int y = height / 2;
        for (int x = 2; x < width - 2; x++)
            pixels[y * width + x] = saddle;
        pixels[y * width + 4] = 1.0f;
        pixels[y * width + width - 5] = 1.0f;
        return pixels;
    }

    /** Two single-pixel maxima sharing only a corner. */
    private static float[] diagonalTouch(int width, int height) {
        float[] pixels = new float[width * height];
        pixels[5 * width + 5] = 1.0f;
        pixels[6 * width + 6] = 1.0f;
        return pixels;
    }

    /** Peaks on the first and last row and column, which the scan never reaches. */
    private static float[] borderPeaks(int width, int height) {
        float[] pixels = new float[width * height];
        pixels[0] = 1.0f;
        pixels[width / 2] = 1.0f;
        pixels[(height - 1) * width + width / 2] = 1.0f;
        pixels[(height / 2) * width] = 1.0f;
        pixels[(height / 2) * width + width - 1] = 1.0f;
        return pixels;
    }

    /** Sets `every`th pixel to negative infinity, as getMaximaLabels does. */
    private static float[] holes(float[] pixels, int width, int height, int every) {
        float[] out = pixels.clone();
        for (int y = 1; y < height - 1; y++)
            for (int x = 1; x < width - 1; x++)
                if ((y * width + x) % every == 0)
                    out[y * width + x] = Float.NEGATIVE_INFINITY;
        return out;
    }

    /**
     * Values spanning -0.5 to 2.0, the range deconvolved optical density
     * occupies once the clamp at zero is gone.
     */
    private static float[] random(int width, int height, long seed) {
        Random random = new Random(seed);
        float[] pixels = new float[width * height];
        for (int i = 0; i < pixels.length; i++)
            pixels[i] = (float) (random.nextDouble() * 2.5 - 0.5);
        return pixels;
    }

    /** Random pixels through the blur and the Laplacian, which is what the
     *  maxima stage actually receives from the pipeline. */
    private static float[] logOf(int width, int height, long seed, double sigma) {
        FloatProcessor fp = new FloatProcessor(width, height, random(width, height, seed), null);
        fp.blurGaussian(sigma);
        fp.convolve(LAPLACIAN, 3, 3);
        return (float[]) fp.getPixels();
    }

    /** Float.toString round-trips exactly, so the Rust side parses back the same bits. */
    private static void append(StringBuilder out, float[] values) {
        out.append("[");
        for (int i = 0; i < values.length; i++) {
            if (i > 0) out.append(", ");
            out.append(values[i]);
        }
        out.append("]");
    }
}
