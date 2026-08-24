import ij.plugin.filter.RankFilters;
import ij.process.Blitter;
import ij.process.ByteProcessor;
import ij.process.ColorProcessor;
import ij.process.FloatProcessor;
import ij.process.ImageProcessor;

import qupath.imagej.processing.MorphologicalReconstruction;
import qupath.imagej.tools.IJTools;
import qupath.lib.color.ColorDeconvolutionStains;
import qupath.lib.images.ImageData;
import qupath.lib.images.servers.ImageServer;
import qupath.lib.images.servers.ImageServerProvider;
import qupath.lib.regions.RegionRequest;

import java.awt.image.BufferedImage;
import java.util.Locale;

/**
 * Records {@code estimateBackground} and the subtraction after it, on real
 * tissue, as an input image and an output image.
 *
 * <p>Usage: {@code BackgroundEstimate <out.json> <slide> <x,y,w,h,name> ...},
 * with the crop given in full-resolution slide coordinates.
 *
 * <p>{@code DIVERGENCES.md} D-009's stage ladder is already wrong at its first
 * rung — this pipeline finds 5.8% and 7.9% more candidates than QuPath before
 * the intensity filter runs — and the filter then roughly doubles that, keeping
 * 0.530 and 0.659 of them where QuPath keeps 0.441 and 0.547. The filter itself
 * is pinned on 17 recorded images and passes, so what differs is what it
 * measures: {@code ipToMeasure}, which is the image this recorder produces.
 *
 * <p><b>The composition is the thing unpinned, not the parts.</b> The stage is
 * four operations — an erosion by {@code backgroundRadius}, a mask of anything
 * still above {@code maxBackground} dilated by twice the radius and driven to
 * negative infinity, a geodesic dilation back under the original, and a
 * subtraction. {@code RankFilterMinMax.json} pins the first on 42 cases and
 * {@code RegionalMaxima.json} pins the third on 24, both on synthetic input.
 * Nothing pins the four together, on tissue, which is exactly the gap the merge
 * and the split turned out to have too.
 *
 * <p><b>Small crops are correct here, and were not correct for the ladder.</b>
 * This is a stage parity fixture: same input, same output. It does not need to
 * reproduce the density regime, because it is not measuring a regime — a
 * disagreement on any real tissue input is a disagreement.
 *
 * <p>The size is set by what a committed fixture can carry. Detection runs at
 * ~1.0 downsample on this slide, not the ~2.0 a "requested 0.5 µm" suggests, so
 * a crop is recorded at very nearly its full-resolution size — the first
 * recording of this fixture asked for 320 and produced 12 MB. Two images of
 * 159x159 floats, five crops, is ~1.6 MB.
 *
 * <p>Only the input and the subtracted output are recorded. The estimate itself
 * is not: the subtraction is what {@code ipToMeasure} holds and what the object
 * filter measures, and carrying the estimate as well is half again the bytes for
 * a checkpoint no assertion needs.
 *
 * <p><b>{@code maxBackground} is per crop, and that is what makes its branch
 * testable.</b> It fires where the *eroded* optical density is still above the
 * limit — dense stain, a fold, ink — not where the image is bright, since
 * optical density runs the other way. At QuPath's default of 2.0 it fires on
 * none of this slide's ordinary tissue, which {@code DIVERGENCES.md} D-009 had
 * already found by checking directly. Hunting the slide for a fold would make
 * the fixture depend on one; lowering the limit on two otherwise ordinary crops
 * reaches the same branch deterministically, and the limit is recorded per crop
 * so the test applies the one that was used.
 *
 * <p><b>Floats are recorded by their bits</b>, as eight lowercase hex
 * characters, for {@code MergeAndSplit.json}'s reason: a background estimate
 * wrong in the last place is a mean wrong in the last place, and a candidate
 * sitting on the threshold then falls the other way. Comparing to a tolerance
 * would hide precisely the failure this is looking for.
 */
public class BackgroundEstimate {

    private static final double REQUESTED_PIXEL_SIZE = 0.5;
    private static final double BACKGROUND_RADIUS_MICRONS = 8.0;
    private static final double MAX_BACKGROUND = 2.0;

    public static void main(String[] args) throws Exception {
        if (args.length < 3) {
            System.err.println("usage: BackgroundEstimate <out.json> <slide> <x,y,w,h,name> ...");
            System.exit(2);
        }
        var outPath = args[0];
        var path = args[1];

        try (ImageServer<BufferedImage> server =
                     ImageServerProvider.buildServer(path, BufferedImage.class)) {

            var imageData = new ImageData<>(server);
            imageData.setImageType(ImageData.ImageType.BRIGHTFIELD_H_E);
            ColorDeconvolutionStains stains = imageData.getColorDeconvolutionStains();

            double fullPixelSize = server.getPixelCalibration().getAveragedPixelSizeMicrons();
            double downsample = REQUESTED_PIXEL_SIZE / fullPixelSize;
            double pixelSize = fullPixelSize * downsample;
            double backgroundRadius = BACKGROUND_RADIUS_MICRONS / pixelSize;

            var out = new StringBuilder();
            out.append("{\n");
            out.append(String.format(Locale.ROOT, "  \"slide\": \"%s\",\n",
                    java.nio.file.Path.of(path).getFileName()));
            out.append(String.format(Locale.ROOT, "  \"backgroundRadius\": %.6f,\n", backgroundRadius));
            out.append(String.format(Locale.ROOT, "  \"defaultMaxBackground\": %.6f,\n", MAX_BACKGROUND));
            out.append("  \"crops\": [\n");

            for (int a = 2; a < args.length; a++) {
                var parts = args[a].split(",");
                if (parts.length != 6)
                    throw new IllegalArgumentException(
                            "expected x,y,w,h,maxBackground,name got " + args[a]);
                if (a > 2) out.append(",\n");
                out.append(record(server, stains,
                        parts[5],
                        Integer.parseInt(parts[0]), Integer.parseInt(parts[1]),
                        Integer.parseInt(parts[2]), Integer.parseInt(parts[3]),
                        downsample, backgroundRadius, Double.parseDouble(parts[4])));
                System.err.println("==> " + parts[5]);
            }

            out.append("\n  ]\n}\n");
            java.nio.file.Files.writeString(java.nio.file.Path.of(outPath), out.toString());
        }
    }

    private static String record(ImageServer<BufferedImage> server, ColorDeconvolutionStains stains,
                                 String name, int x, int y, int w, int h,
                                 double downsample, double backgroundRadius,
                                 double maxBackground) throws Exception {

        var request = RegionRequest.createInstance(server.getPath(), downsample, x, y, w, h);
        var pathImage = IJTools.convertToImagePlus(server, request);
        ImageProcessor ip = pathImage.getImage().getProcessor();
        if (!(ip instanceof ColorProcessor cp))
            throw new IllegalStateException("expected an RGB tile for " + name);

        int width = ip.getWidth();
        int height = ip.getHeight();

        FloatProcessor[] fps = IJTools.colorDeconvolve(cp, stains);
        // fpDetection, and then fpLoG duplicated from it -- the median radius is
        // zero by default, so nothing sits between the two.
        FloatProcessor fpDetection = (FloatProcessor) fps[0].duplicate();
        FloatProcessor fpLoG = (FloatProcessor) fpDetection.duplicate();

        // The input, captured before the stage touches it.
        float[] input = (float[]) fpLoG.getPixels();
        String inputHex = pack(input.clone());

        ImageProcessor ipBackground = fpLoG.duplicate();
        ByteProcessor bpMask =
                estimateBackground(fpLoG, ipBackground, backgroundRadius, maxBackground);
        fpLoG.copyBits(ipBackground, 0, 0, Blitter.SUBTRACT);

        var out = new StringBuilder();
        out.append(String.format(Locale.ROOT,
                "    {\"name\": \"%s\", \"x\": %d, \"y\": %d,\n"
                        + "     \"width\": %d, \"height\": %d,\n"
                        + "     \"maxBackground\": %.6f, \"masked\": %s,\n",
                name, x, y, width, height, maxBackground, bpMask != null));
        out.append("     \"input\": \"").append(inputHex).append("\",\n");
        out.append("     \"subtracted\": \"").append(pack((float[]) fpLoG.getPixels())).append("\"}");
        return out.toString();
    }

    /**
     * {@code WatershedCellDetector.estimateBackground}, which is private.
     *
     * <p>Restated rather than reached: it is short, and reflection into a
     * private static of an inner class is a larger thing to get wrong than
     * fourteen lines are.
     */
    private static ByteProcessor estimateBackground(ImageProcessor ip, ImageProcessor ipBackground,
                                                    double radius, double maxBackground) {
        RankFilters rf = new RankFilters();
        ipBackground.setRoi(ip.getRoi());
        rf.rank(ipBackground, radius, RankFilters.MIN);

        ByteProcessor bpMask = null;
        if (!Double.isNaN(maxBackground) && maxBackground > 0) {
            int w = ip.getWidth();
            int h = ip.getHeight();
            for (int i = 0; i < w * h; i++) {
                if (ipBackground.getf(i) > maxBackground) {
                    if (bpMask == null)
                        bpMask = new ByteProcessor(w, h);
                    bpMask.setf(i, 1f);
                }
            }
            if (bpMask != null) {
                rf.rank(bpMask, radius * 2, RankFilters.MAX);
                for (int i = 0; i < w * h; i++) {
                    if (bpMask.getf(i) != 0f)
                        ipBackground.setf(i, Float.NEGATIVE_INFINITY);
                }
            }
        }
        MorphologicalReconstruction.morphologicalReconstruction(ipBackground, ip);
        return bpMask;
    }

    /** Each float as its raw bits, eight lowercase hex characters, in order. */
    private static String pack(float[] pixels) {
        var sb = new StringBuilder(pixels.length * 8);
        for (float value : pixels) {
            String hex = Integer.toHexString(Float.floatToRawIntBits(value));
            sb.append("00000000", 0, 8 - hex.length()).append(hex);
        }
        return sb.toString();
    }
}
