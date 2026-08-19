// Declared in QuPath's package because WatershedCellDetection.CellDetector is
// package-private; this calls runDetection directly, as the profiler driver
// does, to keep the plugin's task machinery out of the comparison.
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
 * Records QuPath's cell count over several regions of one slide, as JSON.
 *
 * Usage: RecordCellCounts <out.json> <slide> <x,y,w,h,name> ...
 *
 * The output path is an argument rather than stdout because QuPath and
 * Bio-Formats both log through SLF4J at DEBUG onto stdout, and neither
 * -Dorg.slf4j.simpleLogger.defaultLogLevel nor -Dlogback.configurationFile
 * silences it here -- the fixture came out as JSON with a stack trace in front
 * of it. Writing the file directly sidesteps the question.
 *
 * This exists because every parity fixture in this directory pins one *stage*
 * against QuPath, and none of them pins the number the whole pipeline exists to
 * produce. docs/MEMO.md §2 quotes 25,757 cells against 26,647 objects on one
 * 4000x4000 region -- one region, measured once. A ratio from a single sample
 * cannot distinguish a systematic offset from a coincidence, and D-008 predicts
 * a *systematic* one whose size should track how much tissue a region holds.
 * Several regions is what makes that testable.
 *
 * No timing and no profiler: the question here is the count. One detection per
 * region, because detection is deterministic for a fixed region -- which
 * ProfileCellDetection asserts across iterations, so it does not need
 * re-asserting here.
 */
public class RecordCellCounts {

    public static void main(String[] args) throws Exception {
        if (args.length < 3) {
            System.err.println("usage: RecordCellCounts <out.json> <slide> <x,y,w,h,name> ...");
            System.exit(2);
        }
        var outPath = args[0];
        var path = args[1];

        try (ImageServer<BufferedImage> server =
                     ImageServerProvider.buildServer(path, BufferedImage.class)) {

            var imageData = new ImageData<>(server);
            imageData.setImageType(ImageData.ImageType.BRIGHTFIELD_H_E);

            var out = new StringBuilder();
            out.append("{\n");
            // The basename, not the path given: this fixture is committed, and
            // an absolute path bakes one machine's checkout into it.
            out.append(String.format(java.util.Locale.ROOT, "  \"slide\": \"%s\",\n",
                    java.nio.file.Path.of(path).getFileName()));
            out.append(String.format(java.util.Locale.ROOT, "  \"width\": %d,\n", server.getWidth()));
            out.append(String.format(java.util.Locale.ROOT, "  \"height\": %d,\n", server.getHeight()));
            out.append(String.format(java.util.Locale.ROOT, "  \"microns_per_pixel\": %.6f,\n",
                    server.getPixelCalibration().getAveragedPixelSizeMicrons()));
            out.append(String.format(java.util.Locale.ROOT, "  \"stains\": \"%s\",\n",
                    imageData.getColorDeconvolutionStains()));
            out.append("  \"regions\": [\n");

            for (int a = 2; a < args.length; a++) {
                var parts = args[a].split(",");
                if (parts.length != 5) {
                    throw new IllegalArgumentException("expected x,y,w,h,name got " + args[a]);
                }
                int x = Integer.parseInt(parts[0]);
                int y = Integer.parseInt(parts[1]);
                int w = Integer.parseInt(parts[2]);
                int h = Integer.parseInt(parts[3]);
                String name = parts[4];

                ROI roi = ROIs.createRectangleROI(x, y, w, h, ImagePlane.getDefaultPlane());
                var detector = new WatershedCellDetection.CellDetector();
                ParameterList params =
                        new WatershedCellDetection().getDefaultParameterList(imageData);

                long t0 = System.nanoTime();
                Collection<PathObject> cells = detector.runDetection(imageData, params, roi);
                double ms = (System.nanoTime() - t0) / 1e6;

                System.err.printf("%-22s %5d,%-5d %5dx%-5d  %8.1f ms  %7d cells\n",
                        name, x, y, w, h, ms, cells.size());

                out.append(String.format(java.util.Locale.ROOT,
                        "    {\"name\": \"%s\", \"x\": %d, \"y\": %d, \"width\": %d, "
                                + "\"height\": %d, \"cells\": %d, \"ms\": %.1f}%s\n",
                        name, x, y, w, h, cells.size(), ms,
                        a == args.length - 1 ? "" : ","));
            }

            out.append("  ]\n}\n");
            java.nio.file.Files.writeString(java.nio.file.Path.of(outPath), out.toString());
        }
    }
}
