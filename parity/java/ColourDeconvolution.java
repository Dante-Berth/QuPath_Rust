import java.awt.image.BufferedImage;
import java.nio.file.Path;

import qupath.lib.color.ColorDeconvolutionStains;
import qupath.lib.color.ColorDeconvolutionStains.DefaultColorDeconvolutionStains;
import qupath.lib.color.ColorTransformer;
import qupath.lib.color.ColorTransformer.ColorTransformMethod;
import qupath.lib.images.servers.ImageServer;
import qupath.lib.images.servers.ImageServerProvider;
import qupath.lib.regions.RegionRequest;

/**
 * Records QuPath's colour deconvolution, so the Rust port is checked against
 * what QuPath computes rather than against our reading of its source.
 *
 * This is the last stage in the workspace making an unverified numerical claim,
 * and it is the one where being wrong is least visible. Deconvolution with the
 * wrong stain vectors still yields a smooth, plausible single-channel image;
 * it is simply a projection onto the wrong axis. Nothing downstream can detect
 * that, and every nucleus found afterwards inherits it.
 *
 * Three things are recorded, because a per-pixel comparison alone would not say
 * <i>which</i> of them was wrong when it failed:
 *
 * <ul>
 * <li>the normalised stain vectors, which pin the constants;
 * <li>the inverse matrix, which pins the inversion and its transpose
 *     convention — QuPath reads {@code invMat[channel][stain]}, a column;
 * <li>per-pixel output through {@link ColorTransformer#getTransformedPixels},
 *     which is the exact call {@code WatershedCellDetection} makes via
 *     {@code IJTools.colorDeconvolve}, LUT and all.
 * </ul>
 *
 * Both H&amp;E and H-DAB are recorded. They share a haematoxylin vector and
 * differ only in the second, so a port that has confused the two — they are
 * similar-looking triples in the same file — matches one and fails the other.
 *
 * <p>Two modes, writing two fixtures, so that neither script can overwrite the
 * other's output with a lesser version of it:
 *
 * <ul>
 * <li>no argument — the synthetic cases above, into
 *     {@code ColourDeconvolution.json}, from {@code record.sh}. Needs no slide.
 * <li>a slide path — a real tile alone, into
 *     {@code ColourDeconvolutionTile.json}, from {@code record-slide.sh},
 *     which is where {@code qupath-extension-bioformats} is on the classpath.
 * </ul>
 */
public class ColourDeconvolution {

    /**
     * Channel values swept in every combination, 1000 pixels in all.
     *
     * Includes 0 and 1, which bracket the clamp in {@code makeOD} — intensity
     * is forced to at least 1 before the logarithm, so those two must agree —
     * and 255, the white point, where optical density is exactly zero.
     */
    private static final int[] LEVELS = {0, 1, 32, 64, 96, 128, 160, 192, 224, 255};

    public static void main(String[] args) throws Exception {
        var out = new StringBuilder();
        out.append("{\n");

        if (args.length > 0) {
            emitTile(out, Path.of(args[0]));
        } else {
            emitStains(out, "H&E", ColorDeconvolutionStains
                    .makeDefaultColorDeconvolutionStains(DefaultColorDeconvolutionStains.H_E));
            out.append(",\n");
            emitStains(out, "H-DAB", ColorDeconvolutionStains
                    .makeDefaultColorDeconvolutionStains(DefaultColorDeconvolutionStains.H_DAB));
        }

        out.append("\n}\n");
        System.out.print(out);
        System.out.flush();
        // Bio-Formats leaves non-daemon threads behind; without this the
        // recorder writes its JSON and then hangs.
        System.exit(0);
    }

    /** Records the vectors, the inverse matrix, and the swept pixels. */
    private static void emitStains(StringBuilder out, String key, ColorDeconvolutionStains stains) {
        out.append("  \"").append(key).append("\": {\n");
        emitMatrix(out, stains);

        int[] rgb = new int[LEVELS.length * LEVELS.length * LEVELS.length];
        int n = 0;
        for (int r : LEVELS)
            for (int g : LEVELS)
                for (int b : LEVELS)
                    rgb[n++] = (r << 16) | (g << 8) | b;

        emitPixels(out, rgb, stains);
        out.append("\n  }");
    }

    /**
     * Records what built the transform, so a failure says which part is wrong.
     *
     * Emitted for the tile case too, though it is redundant there, so that both
     * fixtures parse the same way on the Rust side.
     */
    private static void emitMatrix(StringBuilder out, ColorDeconvolutionStains stains) {
        out.append("    \"vectors\": [");
        for (int i = 0; i < 3; i++) {
            if (i > 0) out.append(", ");
            append(out, stains.getStain(i + 1).getArray());
        }
        out.append("],\n");

        out.append("    \"inverse\": [");
        double[][] inverse = stains.getMatrixInverse();
        for (int i = 0; i < 3; i++) {
            if (i > 0) out.append(", ");
            append(out, inverse[i]);
        }
        out.append("],\n");

        out.append("    \"maxValues\": [")
                .append(stains.getMaxRed()).append(", ")
                .append(stains.getMaxGreen()).append(", ")
                .append(stains.getMaxBlue()).append("],\n");
    }

    /**
     * Records a real tile, whose channels are correlated in a way a synthetic
     * sweep is not, and whose values sit in the narrow bright range where
     * tissue actually lives.
     */
    private static void emitTile(StringBuilder out, Path slide) throws Exception {
        out.append("  \"tile\": {\n");
        out.append("    \"slide\": \"").append(slide.getFileName()).append("\",\n");

        // Inside the dense-tissue region the JFR profile used, matching the
        // `tissue` case in SlideRegion.json.
        int x = 27776, y = 5952, width = 64, height = 64;
        int[] rgb;
        try (ImageServer<BufferedImage> server =
                ImageServerProvider.buildServer(slide.toUri().toString(), BufferedImage.class)) {
            BufferedImage image = server.readRegion(RegionRequest.createInstance(
                    server.getPath(), 1.0, x, y, width, height));
            rgb = image.getRGB(0, 0, width, height, null, 0, width);
        }

        out.append("    \"x\": ").append(x).append(", \"y\": ").append(y)
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height).append(",\n");

        // H&E only: the slide is H&E, and this case exists to exercise real
        // correlated values, not to pin a second matrix. The pixels QuPath was
        // handed are recorded beside the result by emitPixels, so the test
        // feeds those bytes in and depends on nothing but this file.
        ColorDeconvolutionStains stains = ColorDeconvolutionStains
                .makeDefaultColorDeconvolutionStains(DefaultColorDeconvolutionStains.H_E);
        emitMatrix(out, stains);
        emitPixels(out, rgb, stains);
        out.append("\n  }");
    }

    /**
     * Deconvolves through the same call {@code IJTools.colorDeconvolve} makes,
     * so the recorded values carry QuPath's 8-bit LUT and its {@code double}
     * accumulation rather than a re-derivation of the formula.
     */
    private static void emitPixels(StringBuilder out, int[] rgb, ColorDeconvolutionStains stains) {
        out.append("    \"rgbIn\": [");
        for (int i = 0; i < rgb.length; i++) {
            if (i > 0) out.append(", ");
            out.append(rgb[i] & 0xffffff);
        }
        out.append("],\n");

        ColorTransformMethod[] methods = {
                ColorTransformMethod.Stain_1,
                ColorTransformMethod.Stain_2,
                ColorTransformMethod.Stain_3};
        for (int s = 0; s < 3; s++) {
            float[] pixels = ColorTransformer.getTransformedPixels(rgb, methods[s], null, stains);
            out.append("    \"stain").append(s + 1).append("\": [");
            for (int i = 0; i < pixels.length; i++) {
                if (i > 0) out.append(", ");
                out.append(pixels[i]);
            }
            out.append(s < 2 ? "],\n" : "]");
        }
    }

    private static void append(StringBuilder out, double[] v) {
        out.append("[").append(v[0]).append(", ").append(v[1]).append(", ").append(v[2]).append("]");
    }
}
