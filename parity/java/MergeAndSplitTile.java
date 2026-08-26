import ij.plugin.filter.EDM;
import ij.plugin.filter.MaximumFinder;
import ij.plugin.filter.RankFilters;
import ij.gui.PolygonRoi;
import ij.gui.Roi;
import ij.gui.Wand;
import ij.process.Blitter;
import ij.process.ByteProcessor;
import ij.process.ColorProcessor;
import ij.process.FloatProcessor;
import ij.process.ImageProcessor;

import qupath.imagej.processing.MorphologicalReconstruction;
import qupath.imagej.processing.RoiLabeling;
import qupath.imagej.processing.SimpleThresholding;
import qupath.imagej.processing.Watershed;
import qupath.imagej.tools.IJTools;
import qupath.lib.color.ColorDeconvolutionStains;
import qupath.lib.images.ImageData;
import qupath.lib.images.servers.ImageServer;
import qupath.lib.images.servers.ImageServerProvider;
import qupath.lib.regions.RegionRequest;

import java.awt.image.BufferedImage;
import java.util.List;
import java.util.Locale;

/**
 * Records the merge and the shape split on <b>real tissue</b>, at the point in
 * {@code WatershedCellDetection} where they run.
 *
 * <p>Usage: {@code MergeAndSplitTile <out.json> <slide> <x,y,w,h,name> ...},
 * with the crop given in full-resolution slide coordinates.
 *
 * <p>{@code MergeAndSplit.json} pins the same pair on hand-built shapes and on
 * random fields, and it passes. What it has no case for is the one thing
 * {@code DIVERGENCES.md} D-009 identifies as carrying the count gap: a
 * <b>clumped</b> mask, where the dilation bridges many candidates into one blob
 * and the EDM watershed has to cut them apart again. On the three single-tile
 * regions D-009 measures, the merge keeps 0.557 and 0.560 of its input on the
 * two dense ones and 0.439 on the sparse one, and the split recovers 1.57x and
 * 1.57x against 2.16x. Both stages do roughly twice the work, and the fixture
 * that is supposed to pin them contains nothing of the kind.
 *
 * <p><b>Crops from both sides of that measurement, not only the divergent one.</b>
 * A recording made only where the two disagree cannot say whether the stage is
 * wrong or the input is unusual. The regions here are the ones D-009 names, and
 * the ratio each came back with is carried in the fixture, so the comparison is
 * against a number rather than against an impression.
 *
 * <p><b>The chain above the merge is reproduced here rather than called.</b>
 * {@code WatershedCellDetector.doDetection} is private and its checkpoints are
 * local variables; {@code debugMode} shows images rather than returning them,
 * and adds no slice between the AND and the EDM in any case. So this walks the
 * same calls in the same order — the deconvolution, the background estimate and
 * its subtraction, the LoG, the regional maxima, the labelling, the flood, and
 * the mean-intensity filter.
 *
 * <p><b>What that reproduction is and is not trusted for.</b> It is not claimed
 * to be QuPath's mask pixel for pixel — nothing here checks that, and a fixture
 * should not imply a check it does not make. What it is relied on for is
 * weaker and sufficient: that the mask reaching the merge is real tissue in the
 * regime the crop was drawn from. That much is checkable from the recorded
 * counts alone, and the Rust test asserts it — the sparse crops must come back
 * with the merge keeping markedly less of its input and the split recovering
 * markedly more, which is the signature D-009 measured across whole regions. A
 * chain that had drifted into producing noise, or into producing the same thing
 * everywhere, would not reproduce that difference.
 *
 * <p><b>Two rungs above the flood are recorded as counts.</b> The stage ladder
 * built on this fixture is already wrong at its first rung -- the candidates the
 * flood produces -- so the divergence starts at or above the labelling, and the
 * counts alone cannot say which of the three stages between the LoG and the
 * flood it starts at. {@code logComponents} counts the connected components of
 * the thresholded LoG, and {@code maximaCount} counts the regional maxima
 * {@code findRegionalMaxima} leaves, both the same way {@code seeds} is counted,
 * so the three are comparable. The chain already computed both and discarded
 * them.
 *
 * <p><b>Masks are bit-packed</b>, one bit per pixel, little-endian within each
 * byte, as lowercase hex. Every recorded mask is binary, and the same crop
 * written as {@code [0, 255, 0, ...]} is forty times the bytes for no more
 * information. The EDM is deliberately <b>not</b> recorded: it is the one
 * checkpoint that is floating-point, {@code MergeAndSplit.json} already pins it
 * bit-for-bit on twelve cases, and what a real tile adds is scale and clumping
 * rather than a new path through {@code makeFloatEDM}.
 */
public class MergeAndSplitTile {

    /** The kernel that completes the Laplacian of Gaussian. */
    private static final float[] LAPLACIAN = {0, -1, 0, -1, 4, -1, 0, -1, 0};

    /** {@code EDM.MAXFINDER_TOLERANCE}, which is private and so restated here. */
    private static final double MAXFINDER_TOLERANCE = 0.5;

    // WatershedCellDetection's defaults, in microns, as getDefaultParameterList
    // sets them. Converted to pixels against the detection resolution below.
    private static final double REQUESTED_PIXEL_SIZE = 0.5;
    private static final double BACKGROUND_RADIUS_MICRONS = 8.0;
    private static final double MEDIAN_RADIUS_MICRONS = 0.0;
    private static final double SIGMA_MICRONS = 1.5;
    private static final double THRESHOLD = 0.1;
    private static final double MAX_BACKGROUND = 2.0;

    public static void main(String[] args) throws Exception {
        if (args.length < 3) {
            System.err.println("usage: MergeAndSplitTile <out.json> <slide> <x,y,w,h,name> ...");
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
            // ServerTools.getDownsampleFactor's own arithmetic: the requested
            // size over the actual one. Detection does not run at level 0.
            double downsample = REQUESTED_PIXEL_SIZE / fullPixelSize;
            double pixelSize = fullPixelSize * downsample;

            double sigma = SIGMA_MICRONS / pixelSize;
            double backgroundRadius = BACKGROUND_RADIUS_MICRONS / pixelSize;
            double medianRadius = MEDIAN_RADIUS_MICRONS / pixelSize;

            var out = new StringBuilder();
            out.append("{\n");
            out.append(String.format(Locale.ROOT, "  \"slide\": \"%s\",\n",
                    java.nio.file.Path.of(path).getFileName()));
            out.append(String.format(Locale.ROOT, "  \"downsample\": %.6f,\n", downsample));
            out.append(String.format(Locale.ROOT, "  \"pixelSize\": %.6f,\n", pixelSize));
            out.append(String.format(Locale.ROOT, "  \"sigma\": %.6f,\n", sigma));
            out.append(String.format(Locale.ROOT, "  \"backgroundRadius\": %.6f,\n", backgroundRadius));
            out.append(String.format(Locale.ROOT, "  \"threshold\": %.6f,\n", THRESHOLD));
            out.append("  \"tiles\": [\n");

            for (int a = 2; a < args.length; a++) {
                var parts = args[a].split(",");
                if (parts.length != 5)
                    throw new IllegalArgumentException("expected x,y,w,h,name got " + args[a]);
                int x = Integer.parseInt(parts[0]);
                int y = Integer.parseInt(parts[1]);
                int w = Integer.parseInt(parts[2]);
                int h = Integer.parseInt(parts[3]);
                String name = parts[4];

                if (a > 2) out.append(",\n");
                out.append(record(server, stains, name, x, y, w, h,
                        downsample, sigma, backgroundRadius, medianRadius));
                System.err.println("==> " + name);
            }

            out.append("\n  ]\n}\n");
            java.nio.file.Files.writeString(java.nio.file.Path.of(outPath), out.toString());
        }
        // Bio-Formats leaves non-daemon threads behind; without this the
        // recorder writes its JSON and then hangs. Every other recorder here
        // that opens a server does the same -- ColourDeconvolution.java has
        // carried the note since long before this file existed.
        System.exit(0);
    }

    private static String record(ImageServer<BufferedImage> server, ColorDeconvolutionStains stains,
                                 String name, int x, int y, int w, int h,
                                 double downsample, double sigma,
                                 double backgroundRadius, double medianRadius) throws Exception {

        var request = RegionRequest.createInstance(server.getPath(), downsample, x, y, w, h);
        var pathImage = IJTools.convertToImagePlus(server, request);
        ImageProcessor ip = pathImage.getImage().getProcessor();
        if (!(ip instanceof ColorProcessor cp))
            throw new IllegalStateException("expected an RGB tile for " + name);

        int width = ip.getWidth();
        int height = ip.getHeight();

        // Haematoxylin OD, which is what `detectionImageBrightfield` defaults to.
        FloatProcessor[] fps = IJTools.colorDeconvolve(cp, stains);
        FloatProcessor fpDetection = (FloatProcessor) fps[0].duplicate();

        // --- doDetection, from the top, on this tile ---
        FloatProcessor fpLoG = (FloatProcessor) fpDetection.duplicate();
        RankFilters rf = new RankFilters();
        if (medianRadius > 0)
            rf.rank(fpLoG, medianRadius, RankFilters.MEDIAN);

        ImageProcessor ipBackground = fpLoG.duplicate();
        ByteProcessor bpBackgroundMask =
                estimateBackground(fpLoG, ipBackground, backgroundRadius, MAX_BACKGROUND);
        fpLoG.copyBits(ipBackground, 0, 0, Blitter.SUBTRACT);
        ImageProcessor ipToMeasure = fpLoG.duplicate();

        fpLoG.blurGaussian(sigma);
        fpLoG.convolve(LAPLACIAN, 3, 3);

        ByteProcessor bpLoG = SimpleThresholding.thresholdAbove(fpLoG, 0.0f);

        ImageProcessor ipTemp = MorphologicalReconstruction.findRegionalMaxima(fpLoG, 0.001f, false);

        // Two rungs above the flood, which this chain already computes and used
        // to discard. The stage ladder's first rung is `seeds` -- after the
        // flood -- and it is already wrong there, so the divergence starts at or
        // above it. These say which of the three stages between the LoG and the
        // flood it starts at: the threshold, the regional maxima, or the
        // labelling. Counted as connected components the same way `seeds` is,
        // so the three numbers are comparable to each other.
        // Counted through labelImage, not getFilledPolygonROIs, and the
        // difference is not cosmetic. Three things about findRegionalMaxima
        // are easy to get wrong here and all three were, in order:
        //
        // It returns a *FloatProcessor*, so casting to ByteProcessor throws --
        // and it throws only after the slide is opened and the tile
        // deconvolved, which reads as a slow job rather than as a type error.
        //
        // With outputBinary false -- what this chain passes -- it does not
        // return a mask. The else branch is a Blitter.DIFFERENCE, so each pixel
        // carries the *height* of its maximum. Thresholding at 0.5 counts only
        // maxima taller than half a unit, which came back as 0 against 1 seed.
        //
        // And a regional maximum is frequently a *single pixel*, which a Wand
        // outline does not enclose the way it encloses a blob: counting the
        // same dense tile through getFilledPolygonROIs gave 1 against 470
        // seeds. Checked directly -- three isolated peaks in a flat float image
        // give three nonzero pixels and three labels from labelImage, and one
        // ROI from the Wand.
        //
        // So this counts the way the flood counts: labelImage with the
        // predicate the flood itself uses, `d > 0`, since the line below is
        // labelImage(ipTemp, 0, false). The maximum label is the count -- read
        // with `get`, not `getf`: labelImage returns a ShortProcessor, whose
        // getf applies the calibration and does not give back the label.
        ImageProcessor ipMaximaLabels = RoiLabeling.labelImage(ipTemp.duplicate(), 0, false);
        int maximaCount = 0;
        for (int i = 0; i < width * height; i++)
            maximaCount = Math.max(maximaCount, ipMaximaLabels.get(i));

        // bpLoG is genuinely a ByteProcessor -- thresholdAbove returns one --
        // and its components are blobs rather than points, so the Wand suits
        // it. Counted the same way as `seeds` for that reason.
        ImageProcessor ipLoGCount = bpLoG.duplicate();
        ipLoGCount.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        int logComponents = RoiLabeling.getFilledPolygonROIs(ipLoGCount, Wand.FOUR_CONNECTED).size();

        ImageProcessor ipNucleusLabels = RoiLabeling.labelImage(ipTemp, 0, false);
        Watershed.doWatershed(fpLoG, ipNucleusLabels, 0, false);

        ipNucleusLabels.setThreshold(0.5, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        List<PolygonRoi> rois = RoiLabeling.getFilledPolygonROIs(ipNucleusLabels, Wand.FOUR_CONNECTED);

        int seeds = rois.size();
        ByteProcessor bp = new ByteProcessor(width, height);
        bp.setValue(255);
        int survived = 0;
        for (Roi r : rois) {
            ipToMeasure.setRoi(r);
            if (ipToMeasure.getStatistics().mean <= THRESHOLD)
                continue;
            if (bpBackgroundMask != null) {
                bpBackgroundMask.setRoi(r);
                if (bpBackgroundMask.getStatistics().mean > 0)
                    continue;
            }
            bp.fill(r);
            survived++;
        }
        ipToMeasure.resetRoi();

        // The input the merge actually sees, on this tile.
        ByteProcessor binaryIn = (ByteProcessor) bp.duplicate();

        // --- the call site, verbatim ---
        bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        bp.filter(ImageProcessor.MAX);
        bp.copyBits(bpLoG, 0, 0, Blitter.AND);
        List<PolygonRoi> rois2 = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);
        bp.setValue(255);
        for (Roi r : rois2)
            bp.fill(r);
        int mergedCount = rois2.size();
        ByteProcessor merged = (ByteProcessor) bp.duplicate();

        // --- toWatershed, unrolled so the maxima are visible ---
        FloatProcessor edm = new EDM().makeFloatEDM(bp, 0, false);
        ByteProcessor maxima = new MaximumFinder().findMaxima(
                edm, MAXFINDER_TOLERANCE, ImageProcessor.NO_THRESHOLD,
                MaximumFinder.SEGMENTED, false, true);
        bp.copyBits(maxima, 0, 0, Blitter.AND);
        ByteProcessor segmented = (ByteProcessor) bp.duplicate();

        bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        List<PolygonRoi> after = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);

        var out = new StringBuilder();
        out.append(String.format(Locale.ROOT,
                "    {\"name\": \"%s\", \"x\": %d, \"y\": %d, \"requestedWidth\": %d, \"requestedHeight\": %d,\n"
                        + "     \"width\": %d, \"height\": %d,\n"
                        + "     \"logComponents\": %d, \"maximaCount\": %d,\n"
                        + "     \"seeds\": %d, \"afterIntensity\": %d, \"mergedCount\": %d, \"objects\": %d,\n",
                name, x, y, w, h, width, height, logComponents, maximaCount,
                seeds, survived, mergedCount, after.size()));
        out.append("     \"binary\": \"").append(pack(binaryIn)).append("\",\n");
        out.append("     \"logMask\": \"").append(pack(bpLoG)).append("\",\n");
        out.append("     \"merged\": \"").append(pack(merged)).append("\",\n");
        out.append("     \"maxima\": \"").append(pack(maxima)).append("\",\n");
        out.append("     \"segmented\": \"").append(pack(segmented)).append("\"}");
        return out.toString();
    }

    /**
     * {@code WatershedCellDetector.estimateBackground}, which is private.
     *
     * <p>Restated rather than reached, and it is short enough that restating it
     * is safer than reflection: an erosion, a mask of anything still above
     * {@code maxBackground} dilated by twice the radius and driven to negative
     * infinity, and then the opening completed by reconstruction.
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

    /** One bit per pixel, little-endian within a byte, as lowercase hex. */
    private static String pack(ImageProcessor ip) {
        int n = ip.getWidth() * ip.getHeight();
        var sb = new StringBuilder((n + 7) / 8 * 2);
        for (int base = 0; base < n; base += 8) {
            int b = 0;
            for (int k = 0; k < 8 && base + k < n; k++)
                if (ip.get(base + k) != 0)
                    b |= 1 << k;
            sb.append(Character.forDigit(b >>> 4, 16)).append(Character.forDigit(b & 0xf, 16));
        }
        return sb.toString();
    }
}
