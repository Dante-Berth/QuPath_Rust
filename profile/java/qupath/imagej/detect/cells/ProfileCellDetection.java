// Declared in QuPath's package because WatershedCellDetection.CellDetector is
// package-private; this driver calls runDetection directly to keep the plugin
// task-scheduling machinery out of the profile.
package qupath.imagej.detect.cells;

import qupath.lib.images.ImageData;
import qupath.lib.images.servers.ImageServer;
import qupath.lib.images.servers.ImageServerProvider;
import qupath.lib.objects.PathObject;
import qupath.lib.plugins.parameters.ParameterList;
import qupath.lib.regions.ImagePlane;
import qupath.lib.roi.ROIs;
import qupath.lib.roi.interfaces.ROI;

import java.awt.image.BufferedImage;
import java.util.Collection;

/**
 * Drives QuPath's watershed cell detection over a fixed region of a slide, so a
 * profiler attached to this JVM sees the detection path and nothing else.
 *
 * Usage: ProfileCellDetection <slide> <x> <y> <w> <h> <warmup> <timed>
 *
 * The region is given explicitly rather than derived from the slide, so the
 * profiled workload is identical across runs and across slides of different
 * sizes. Detection counts are printed per iteration: a run that "gets fast" by
 * detecting nothing is visible in the output rather than hidden in the flame
 * graph.
 */
public class ProfileCellDetection {

    public static void main(String[] args) throws Exception {
        if (args.length != 7) {
            System.err.println("usage: ProfileCellDetection <slide> <x> <y> <w> <h> <warmup> <timed>");
            System.exit(2);
        }
        var path = args[0];
        int x = Integer.parseInt(args[1]);
        int y = Integer.parseInt(args[2]);
        int w = Integer.parseInt(args[3]);
        int h = Integer.parseInt(args[4]);
        int warmup = Integer.parseInt(args[5]);
        int timed = Integer.parseInt(args[6]);

        try (ImageServer<BufferedImage> server =
                     ImageServerProvider.buildServer(path, BufferedImage.class)) {

            var imageData = new ImageData<>(server);
            imageData.setImageType(ImageData.ImageType.BRIGHTFIELD_H_E);

            ROI roi = ROIs.createRectangleROI(x, y, w, h, ImagePlane.getDefaultPlane());

            System.err.printf("slide:  %s%n", path);
            System.err.printf("size:   %dx%d, %d levels, %.3f um/px%n",
                    server.getWidth(), server.getHeight(), server.nResolutions(),
                    server.getPixelCalibration().getAveragedPixelSizeMicrons());
            System.err.printf("region: %d,%d %dx%d%n", x, y, w, h);
            System.err.printf("stain:  %s%n", imageData.getColorDeconvolutionStains());

            // Warmup exists to let the JIT reach steady state before the
            // profiler's samples start counting; its timings are reported but
            // should be ignored.
            for (int i = 0; i < warmup; i++) {
                run(imageData, roi, "warmup", i);
            }

            System.err.println("=== timed ===");
            long total = 0;
            int lastCount = -1;
            for (int i = 0; i < timed; i++) {
                long t0 = System.nanoTime();
                int n = run(imageData, roi, "timed", i);
                total += System.nanoTime() - t0;

                // Detection is deterministic for a fixed region; a varying count
                // means the workload is not what it appears to be.
                if (lastCount >= 0 && n != lastCount) {
                    throw new IllegalStateException(
                            "detection count changed between iterations: " + lastCount + " -> " + n);
                }
                lastCount = n;
            }

            if (lastCount <= 0) {
                throw new IllegalStateException(
                        "no cells detected; the profile would show setup cost, not detection");
            }
            System.err.printf("mean:   %.1f ms over %d iterations, %d cells%n",
                    total / 1e6 / timed, timed, lastCount);
        }
    }

    private static int run(ImageData<BufferedImage> imageData, ROI roi, String phase, int i) throws Exception {
        var detector = new WatershedCellDetection.CellDetector();
        ParameterList params = new WatershedCellDetection().getDefaultParameterList(imageData);

        long t0 = System.nanoTime();
        Collection<PathObject> cells = detector.runDetection(imageData, params, roi);
        double ms = (System.nanoTime() - t0) / 1e6;

        System.err.printf("%-6s %2d  %6.1f ms  %6d cells%n", phase, i, ms, cells.size());
        return cells.size();
    }
}
