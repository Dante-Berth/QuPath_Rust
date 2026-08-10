import ij.process.FloatProcessor;
import java.util.Random;

/**
 * Records ImageJ's {@code Convolver}, which is the line after the blur in
 * {@code WatershedCellDetection}:
 *
 * <pre>
 *     fpLoG.blurGaussian(sigma);
 *     fpLoG.convolve(new float[]{0, -1, 0, -1, 4, -1, 0, -1, 0}, 3, 3);
 * </pre>
 *
 * <p>{@code FloatProcessor.convolve} calls {@code snapshot()} and then a
 * <b>fresh</b> {@code Convolver}, so {@code normalize} is at its field default
 * of {@code true} and the snapshot the convolution reads is an exact copy of
 * the pixels it is about to overwrite. Neither is reachable from the call site,
 * and both decide the answer.
 *
 * <p>Three things make this more than a nine-tap stencil, and each has a case
 * below that fails without it:
 *
 * <ul>
 * <li><b>The Laplacian is the branch where normalisation does not happen.</b>
 *     {@code getScale} divides by the kernel sum, but returns 1.0 unchanged
 *     when that sum is exactly zero — and {0,-1,0,-1,4,-1,0,-1,0} sums to
 *     exactly zero. A port that always normalises divides by zero; one that
 *     never normalises passes the Laplacian and fails every other kernel. Both
 *     branches are recorded, with {@code box} and {@code sharpen} taking the
 *     dividing path.
 * <li><b>It correlates, it does not convolve.</b> {@code kernel[i++]} advances
 *     as {@code u} runs from {@code -uc} upwards, so {@code kernel[0]} meets
 *     the top-left neighbour and the kernel is never flipped. The Laplacian is
 *     symmetric and cannot show this, so {@code asymmetric} is here to.
 * <li><b>Edges clamp, and the clamp is per tap.</b> A pixel is an edge pixel
 *     when {@code y<vc || y>=height-vc || x<uc || x>=width-uc}, and every tap
 *     of such a pixel is fetched through a coordinate clamped to the image.
 *     This is not the blur's convention, which extends through a tail sum.
 * </ul>
 *
 * <p>Accumulation is {@code double} over {@code float} taps and is narrowed
 * once at the store, as in the deconvolution. Summation order is the kernel's
 * raster order and is recorded rather than assumed, because a port that
 * accumulates in {@code f32}, or reassociates the sum, drifts in the last bits.
 *
 * <p>Inputs run negative for the same reason the blur's do: this filter's real
 * input is blurred optical density, which is negative across most of a slide's
 * background now that the deconvolution no longer clamps at zero.
 */
public class LaplacianConvolution {

    /** The kernel {@code WatershedCellDetection} passes. Sums to exactly zero. */
    private static final float[] LAPLACIAN = {0, -1, 0, -1, 4, -1, 0, -1, 0};

    /** Sums to 9: takes the dividing branch of {@code getScale}. */
    private static final float[] BOX = {1, 1, 1, 1, 1, 1, 1, 1, 1};

    /** Sums to 1: scaled, but by a factor that changes nothing, so a port that
     *  drops the multiply passes this and fails {@code box}. */
    private static final float[] SHARPEN = {0, -1, 0, -1, 5, -1, 0, -1, 0};

    /** Sums to 10, and no two taps are equal, so a flipped or transposed
     *  kernel gives a different answer at every pixel. */
    private static final float[] ASYMMETRIC = {1, 2, 3, -1, 4, -2, 0, 5, -2};

    /** 5x3: separates kw from kh, which a square kernel cannot. */
    private static final float[] WIDE = {1, 0, -1, 0, 2, 0, -2, 0, 1, 0, 3, 0, -1, 0, -3};

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"scales\": [\n");

        // getScale on its own, so a failure says which half is wrong.
        emitScale(out, "laplacian", LAPLACIAN, true);
        emitScale(out, "box", BOX, false);
        emitScale(out, "sharpen", SHARPEN, false);
        emitScale(out, "asymmetric", ASYMMETRIC, false);
        emitScale(out, "wide", WIDE, false);
        out.append("\n  ],\n  \"images\": [\n");

        // An impulse returns the kernel itself, unflipped and unscaled: the
        // clearest statement of both conventions in one case.
        emitImage(out, "impulse-laplacian", impulse(9, 9), 9, 9, LAPLACIAN, 3, 3, true);
        emitImage(out, "impulse-asymmetric", impulse(9, 9), 9, 9, ASYMMETRIC, 3, 3, false);
        // A constant image is exactly zero under a zero-sum kernel everywhere,
        // edges included, because the clamped taps are the same constant. It is
        // the normalisation and the edge rule asserted together.
        emitImage(out, "constant-laplacian", constant(12, 10, 0.75f), 12, 10, LAPLACIAN, 3, 3, false);
        // The same image under a summing kernel must come back unchanged, which
        // is false unless the scale divides by 9.
        emitImage(out, "constant-box", constant(12, 10, 0.75f), 12, 10, BOX, 3, 3, false);
        // Steps in each direction, where the clamped edge taps actually differ
        // from the interior ones.
        emitImage(out, "step-x", stepX(16, 12), 16, 12, LAPLACIAN, 3, 3, false);
        emitImage(out, "step-y", stepY(12, 16), 12, 16, LAPLACIAN, 3, 3, false);
        // Non-square random data through every kernel: the general case.
        emitImage(out, "random-laplacian", random(37, 23, 1), 37, 23, LAPLACIAN, 3, 3, false);
        emitImage(out, "random-box", random(37, 23, 1), 37, 23, BOX, 3, 3, false);
        emitImage(out, "random-sharpen", random(37, 23, 1), 37, 23, SHARPEN, 3, 3, false);
        emitImage(out, "random-asymmetric", random(23, 37, 2), 23, 37, ASYMMETRIC, 3, 3, false);
        // 5x3 on a non-square image: kw and kh disagree, and so do the two edge
        // margins, so a port that uses one for both is wrong here and nowhere
        // above.
        emitImage(out, "random-wide", random(29, 19, 3), 29, 19, WIDE, 5, 3, false);
        // Images with no interior at all. Every pixel takes the edge branch,
        // and `xedge = width - uc` is at or below `uc`, which is where an
        // interior loop written as `uc..width-uc` walks backwards.
        emitImage(out, "thin-row", random(9, 1, 4), 9, 1, LAPLACIAN, 3, 3, false);
        emitImage(out, "thin-column", random(1, 9, 5), 1, 9, LAPLACIAN, 3, 3, false);
        emitImage(out, "two-by-two", random(2, 2, 6), 2, 2, LAPLACIAN, 3, 3, false);
        emitImage(out, "single-pixel", random(1, 1, 7), 1, 1, LAPLACIAN, 3, 3, false);
        // Narrower than the 5-wide kernel: taps clamp past both edges at once.
        emitImage(out, "narrow-wide-kernel", random(3, 6, 8), 3, 6, WIDE, 5, 3, false);
        // The real thing: a blurred tile, at the sigma CMU-1 puts detection at.
        // This is the only case whose input has the spectral content the
        // filter actually meets in the pipeline.
        emitImage(out, "blurred-tile", blurred(64, 64, 9, 3.0), 64, 64, LAPLACIAN, 3, 3, false);

        out.append("\n  ]\n}\n");
        System.out.print(out);
        System.out.flush();
        // See GaussianBlur.java: ImageJ's parallel filters leave non-daemon
        // workers alive, and this recorder blurs a tile to build its input.
        System.exit(0);
    }

    /** Records {@code Convolver.getScale}, which is the branch that decides
     *  whether the Laplacian is divided by its (zero) sum. */
    private static void emitScale(StringBuilder out, String name, float[] kernel, boolean first) {
        if (!first) out.append(",\n");
        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"scale\": ")
                .append(ij.plugin.filter.Convolver.getScale(kernel))
                .append(", \"kernel\": ");
        append(out, kernel);
        out.append("}");
    }

    /**
     * Convolves one image and records input beside output, so the test feeds in
     * the recorded bytes and depends on nothing but this file.
     */
    private static void emitImage(
            StringBuilder out, String name, float[] pixels, int width, int height,
            float[] kernel, int kw, int kh, boolean first) {
        if (!first) out.append(",\n");
        float[] input = pixels.clone();

        // Exactly what detection calls: FloatProcessor.convolve, which
        // snapshots and then builds its own Convolver with normalize=true.
        FloatProcessor fp = new FloatProcessor(width, height, pixels, null);
        fp.convolve(kernel, kw, kh);

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height)
                .append(", \"kernelWidth\": ").append(kw)
                .append(", \"kernelHeight\": ").append(kh)
                .append(",\n     \"kernel\": ");
        append(out, kernel);
        out.append(",\n     \"in\": ");
        append(out, input);
        out.append(",\n     \"out\": ");
        append(out, (float[]) fp.getPixels());
        out.append("}");
    }

    private static float[] impulse(int width, int height) {
        float[] pixels = new float[width * height];
        pixels[(height / 2) * width + width / 2] = 1.0f;
        return pixels;
    }

    private static float[] constant(int width, int height, float value) {
        float[] pixels = new float[width * height];
        java.util.Arrays.fill(pixels, value);
        return pixels;
    }

    /** A vertical edge: constant down each column, a step across the row. */
    private static float[] stepX(int width, int height) {
        float[] pixels = new float[width * height];
        for (int y = 0; y < height; y++)
            for (int x = 0; x < width; x++)
                pixels[y * width + x] = x < width / 2 ? -0.25f : 1.5f;
        return pixels;
    }

    /** A horizontal edge, which a port that filters the wrong axis gets wrong. */
    private static float[] stepY(int width, int height) {
        float[] pixels = new float[width * height];
        for (int y = 0; y < height; y++)
            for (int x = 0; x < width; x++)
                pixels[y * width + x] = y < height / 2 ? -0.25f : 1.5f;
        return pixels;
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

    /** Random data put through the blur first, which is what the Laplacian
     *  actually receives: smooth, so its second derivative is not noise. */
    private static float[] blurred(int width, int height, long seed, double sigma) {
        FloatProcessor fp = new FloatProcessor(width, height, random(width, height, seed), null);
        fp.blurGaussian(sigma);
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
