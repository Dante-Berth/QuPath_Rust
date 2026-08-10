import ij.process.FloatProcessor;
import java.util.Random;

/**
 * Records ImageJ's Gaussian blur, which is what {@code WatershedCellDetection}
 * applies before its maximum finder:
 *
 * <pre>
 *     fpLoG.blurGaussian(sigma);
 *     fpLoG.convolve(new float[]{0, -1, 0, -1, 4, -1, 0, -1, 0}, 3, 3);
 * </pre>
 *
 * {@code FloatProcessor.blurGaussian(sigma)} reaches
 * {@code GaussianBlur.blurGaussian(ip, sigma, sigma, accuracy)} with
 * <b>accuracy 0.0002</b> — the 0.002 in the same method is the byte and colour
 * case, and taking it would widen every kernel by a third.
 *
 * <p>Two things are recorded, because a per-pixel comparison alone would not
 * say which half was wrong when it failed:
 *
 * <ul>
 * <li>{@code makeGaussianKernel} output — the kernel and its running tail sum,
 *     for chosen (sigma, accuracy, maxRadius) triples. This kernel is not the
 *     textbook one: past a point the tail is replaced by a fitted parabola so
 *     the truncation has no step in it, and the fit picks its own break point
 *     by scanning inwards for a minimal slope. Nothing about that is guessable
 *     from the formula, which is why it is pinned separately.
 * <li>whole blurred images through {@code blurGaussian}, which is the call
 *     QuPath makes, edge handling and float accumulation order included.
 * </ul>
 *
 * <p>The image cases are chosen to separate mistakes that a single case would
 * let through: non-square sizes so an x/y transposition cannot pass, lines
 * shorter than the kernel radius so the short-line branch is entered, a
 * constant image so a kernel that fails to sum to one is immediately visible,
 * and sigmas either side of 8.5 — the threshold above which ImageJ downscales
 * the line, convolves, and upscales again, which is a different code path
 * reached by nothing but a large sigma.
 *
 * <p>Inputs run negative on purpose. The blur's input in detection is the
 * deconvolved optical density, and a stain amount is legitimately negative
 * across most of a slide's background, so a port tested only on positive data
 * would be tested on the wrong range.
 */
public class GaussianBlur {

    /** Accuracy {@code blurGaussian} uses for a FloatProcessor. */
    private static final double FLOAT_ACCURACY = 0.0002;

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"kernels\": [\n");

        // maxRadius is the line length ImageJ passes, floored at 50. The last
        // two entries sit either side of that floor and above it, which selects
        // the branch normalising by sigma*sqrt(2*pi) instead of by the computed
        // sum -- unreachable through blurGaussian, reachable here.
        emitKernel(out, 0.5, FLOAT_ACCURACY, 64, true);
        emitKernel(out, 1.0, FLOAT_ACCURACY, 64, false);
        emitKernel(out, 1.5, FLOAT_ACCURACY, 64, false);
        emitKernel(out, 2.0, FLOAT_ACCURACY, 64, false);
        emitKernel(out, 3.0, FLOAT_ACCURACY, 64, false);
        emitKernel(out, 5.0, FLOAT_ACCURACY, 64, false);
        emitKernel(out, 8.0, FLOAT_ACCURACY, 64, false);
        emitKernel(out, 3.0, FLOAT_ACCURACY, 7, false);
        emitKernel(out, 12.0, FLOAT_ACCURACY, 30, false);
        emitKernel(out, 20.0, FLOAT_ACCURACY, 64, false);
        out.append("\n  ],\n  \"images\": [\n");

        // An impulse returns the 2-D kernel itself, so a wrong kernel shows up
        // undiluted rather than averaged away against neighbouring pixels.
        emitImage(out, "impulse", impulse(33, 33), 33, 33, 3.0, true);
        // A constant image must come back constant: this is the normalisation,
        // and the edge handling, in one assertion.
        emitImage(out, "constant", constant(40, 40, 0.75f), 40, 40, 3.0, false);
        // Steps in each direction, which is where the edge extension and the
        // running-tail-sum correction actually bite.
        emitImage(out, "step-x", stepX(64, 32), 64, 32, 2.0, false);
        emitImage(out, "step-y", stepY(32, 64), 32, 64, 2.0, false);
        // Non-square, so that swapping width and height cannot pass.
        emitImage(out, "random-37x23", random(37, 23, 1), 37, 23, 1.5, false);
        // Lines shorter than the kernel radius: convolveLine's first loop then
        // covers the whole line and the interior loop never runs.
        emitImage(out, "short-lines", random(11, 9, 2), 11, 9, 4.0, false);
        // Kernel radius 3 or less, where the parabolic edge correction is
        // skipped entirely.
        emitImage(out, "small-sigma", random(64, 64, 3), 64, 64, 0.5, false);
        // QuPath's default sigma is 1.5 microns. CMU-1 records MPP = 0.4990,
        // so detection runs at 3.01 px on it; 6.0 covers a 0.25 um/px 40x scan.
        emitImage(out, "tile-sigma3", random(64, 64, 4), 64, 64, 3.0, false);
        emitImage(out, "tile-sigma6", random(64, 64, 5), 64, 64, 6.0, false);
        // Above 2*4+0.5, where blur1Direction downscales, convolves and
        // upscales instead. reduceBy is floor(sigma/4): 2, then 3.
        emitImage(out, "downscale-sigma9", random(64, 64, 6), 64, 64, 9.0, false);
        emitImage(out, "downscale-sigma12", random(128, 64, 7), 128, 64, 12.0, false);
        // reduceBy is clamped to the line length, so a line shorter than the
        // reduction factor is its own case.
        emitImage(out, "downscale-short", random(9, 40, 8), 9, 40, 9.0, false);

        out.append("\n  ]\n}\n");
        System.out.print(out);
        System.out.flush();
        // ImageJ's parallel filters leave a cached pool of non-daemon workers
        // behind — one per core — and never shut it down, so the JVM hangs here
        // with the fixture already fully written. `record.sh` runs the
        // recorders under `set -e` in sequence, so that hang stalled the whole
        // script and nothing after this file was ever recorded.
        System.exit(0);
    }

    /** Records one kernel and its running tail sum. */
    private static void emitKernel(
            StringBuilder out, double sigma, double accuracy, int maxRadius, boolean first) {
        if (!first) out.append(",\n");
        // makeGaussianKernel is an instance method but touches no state.
        float[][] kernel = new ij.plugin.filter.GaussianBlur()
                .makeGaussianKernel(sigma, accuracy, maxRadius);
        out.append("    {\"sigma\": ").append(sigma)
                .append(", \"accuracy\": ").append(accuracy)
                .append(", \"maxRadius\": ").append(maxRadius)
                .append(",\n     \"kernel\": ");
        append(out, kernel[0]);
        out.append(",\n     \"kernSum\": ");
        append(out, kernel[1]);
        out.append("}");
    }

    /**
     * Blurs one image and records input beside output, so the test feeds in the
     * recorded bytes and depends on nothing but this file.
     */
    private static void emitImage(
            StringBuilder out, String name, float[] pixels, int width, int height,
            double sigma, boolean first) {
        if (!first) out.append(",\n");
        float[] input = pixels.clone();

        FloatProcessor fp = new FloatProcessor(width, height, pixels, null);
        fp.blurGaussian(sigma);

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height)
                .append(", \"sigma\": ").append(sigma)
                .append(",\n     \"in\": ");
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

    /** A horizontal edge, which a port that blurs the wrong axis gets wrong. */
    private static float[] stepY(int width, int height) {
        float[] pixels = new float[width * height];
        for (int y = 0; y < height; y++)
            for (int x = 0; x < width; x++)
                pixels[y * width + x] = y < height / 2 ? -0.25f : 1.5f;
        return pixels;
    }

    /**
     * Values spanning -0.5 to 2.0, the range deconvolved optical density
     * actually occupies once the clamp at zero is gone.
     */
    private static float[] random(int width, int height, long seed) {
        Random random = new Random(seed);
        float[] pixels = new float[width * height];
        for (int i = 0; i < pixels.length; i++)
            pixels[i] = (float) (random.nextDouble() * 2.5 - 0.5);
        return pixels;
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
