// Declared in QuPath's package because WatershedCellDetection.CellDetector is
// package-private; this calls runDetection directly, as RecordCellCounts and
// the profiler driver do, so what is recorded is the plugin's real output and
// not a reproduction of its chain.
package qupath.imagej.detect.cells;

import ij.gui.Roi;
import ij.process.ImageProcessor;

import qupath.imagej.tools.IJTools;
import qupath.lib.images.ImageData;
import qupath.lib.images.servers.ImageServer;
import qupath.lib.images.servers.ImageServerProvider;
import qupath.lib.objects.PathCellObject;
import qupath.lib.objects.PathObject;
import qupath.lib.plugins.parameters.ParameterList;
import qupath.lib.regions.ImagePlane;
import qupath.lib.roi.ROIs;
import qupath.lib.roi.interfaces.ROI;

import java.awt.Rectangle;
import java.awt.image.BufferedImage;
import java.util.ArrayList;
import java.util.Collection;
import java.util.Comparator;
import java.util.List;
import java.util.Locale;

/**
 * Records the <b>mask of every nucleus</b> QuPath detects over several regions
 * of one slide, as JSON.
 *
 * <p>Usage: {@code RecordObjectMasks <out.json> <slide> <x,y,w,h,name> ...},
 * with regions in full-resolution slide coordinates.
 *
 * <p><b>Why this exists.</b> {@code docs/dev/MEMO.md} §5 puts the count at the top
 * of what to distrust, and says why in one line: a count is the weakest
 * evidence of a match there is, because two runs can agree on how many objects
 * they found and disagree about where every one of them is. Every fixture in
 * this directory either pins one stage's pixels or compares a total. Nothing
 * has ever compared the *objects* — which is the comparison the parity claim
 * actually rests on, and the one a reader will ask for first.
 *
 * <p><b>What is recorded, and why it is masks.</b> One entry per nucleus: its
 * bounding box in region-local pixels, its area, and a bit-packed mask of the
 * box. Masks rather than polygon vertices because the pipeline's objects are
 * pixel masks — asking it for vertices would compare a thing it computes
 * against a thing QuPath computes differently, and asking QuPath's polygon to
 * be rasterised by us would put our rasteriser inside the comparison. The mask
 * here is produced by {@link Roi#getMask()}, which is ImageJ's own, so the
 * pixels compared are the pixels ImageJ says are inside the ROI.
 *
 * <p><b>Nuclei and not cells.</b> The pipeline's {@code Outcome::objects} is the
 * nucleus set after the area filter; the cell boundary is the expansion around
 * it, and comparing cells would fold {@code cellExpansion} into a measurement
 * meant to test detection. {@code CellExpansion.json} pins that stage
 * separately. Cells are what D-008 is about and want their own fixture.
 *
 * <p><b>Regions with no seams.</b> The defaults in {@code record-object-masks.sh}
 * are the three single-tile regions {@code docs/dev/MEMO.md} §2 measures, at
 * 1.0121, 1.0177 and 1.1130. A region that fits one tile has no seam overlaps
 * to resolve, so nothing D-008 describes can enter the comparison, and any
 * disagreement is detection disagreeing with detection.
 *
 * <p><b>Sorted before writing.</b> {@code runDetection} makes no ordering
 * promise and its tiles are processed in parallel. The entries are sorted by
 * box origin so that re-recording an unchanged pipeline gives a byte-identical
 * file, which is what makes a diff of this fixture mean something.
 */
public class RecordObjectMasks {

    public static void main(String[] args) throws Exception {
        if (args.length < 3) {
            System.err.println("usage: RecordObjectMasks <out.json> <slide> <x,y,w,h,name> ...");
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
            out.append(String.format(Locale.ROOT, "  \"slide\": \"%s\",\n",
                    java.nio.file.Path.of(path).getFileName()));
            // The stain vectors as numbers, and not
            // `ColorDeconvolutionStains.toString()`. That method formats through
            // the default locale, so on this machine (fr_FR) it writes "0,651"
            // where an en_US one writes "0.651" -- committing it makes the
            // fixture depend on who recorded it, and re-recording elsewhere a
            // spurious diff. CellCounts.json carries that leak today.
            out.append("  \"stains\": [");
            for (int s = 1; s <= 3; s++) {
                double[] v = imageData.getColorDeconvolutionStains().getStain(s).getArray();
                out.append(String.format(Locale.ROOT, "%s[%.17g, %.17g, %.17g]",
                        s == 1 ? "" : ", ", v[0], v[1], v[2]));
            }
            out.append("],\n");
            out.append("  \"regions\": [\n");

            for (int a = 2; a < args.length; a++) {
                var parts = args[a].split(",");
                if (parts.length != 5)
                    throw new IllegalArgumentException("expected x,y,w,h,name got " + args[a]);
                int x = Integer.parseInt(parts[0]);
                int y = Integer.parseInt(parts[1]);
                int w = Integer.parseInt(parts[2]);
                int h = Integer.parseInt(parts[3]);
                String name = parts[4];

                out.append(record(imageData, name, x, y, w, h));
                out.append(a == args.length - 1 ? "\n" : ",\n");
            }

            out.append("  ]\n}\n");
            java.nio.file.Files.writeString(java.nio.file.Path.of(outPath), out.toString());
        }

        // Bio-Formats leaves non-daemon threads behind; without this the
        // recorder writes its JSON and then hangs. Every other recorder here
        // that opens an ImageServer does the same -- see commit 110f346, which
        // is what two recorders missing this cost.
        System.exit(0);
    }

    private static String record(ImageData<BufferedImage> imageData, String name,
                                 int x, int y, int w, int h) throws Exception {

        ROI request = ROIs.createRectangleROI(x, y, w, h, ImagePlane.getDefaultPlane());
        var detector = new WatershedCellDetection.CellDetector();
        ParameterList params = new WatershedCellDetection().getDefaultParameterList(imageData);

        long t0 = System.nanoTime();
        Collection<PathObject> cells = detector.runDetection(imageData, params, request);
        double ms = (System.nanoTime() - t0) / 1e6;

        // Region-local pixels: convertXtoIJ is `x / downsample + xOrigin`, so a
        // negative origin at downsample 1 is a translation by the region corner.
        List<Nucleus> nuclei = new ArrayList<>(cells.size());
        long areaTotal = 0;
        for (PathObject cell : cells) {
            ROI nucleus = cell instanceof PathCellObject c ? c.getNucleusROI() : null;
            if (nucleus == null)
                throw new IllegalStateException(
                        "a detection with no nucleus ROI in " + name + "; "
                                + "the comparison this fixture feeds is over nuclei");
            Roi ij = IJTools.convertToIJRoi(nucleus, -x, -y, 1.0);
            Nucleus n = Nucleus.of(ij);
            areaTotal += n.area;
            nuclei.add(n);
        }

        // Deterministic order, so re-recording an unchanged QuPath is a no-op
        // in the diff rather than a reshuffle of six thousand lines.
        nuclei.sort(Comparator.<Nucleus>comparingInt(n -> n.y)
                .thenComparingInt(n -> n.x)
                .thenComparingInt(n -> n.w)
                .thenComparingInt(n -> n.h));

        System.err.printf("%-24s %5d,%-5d %5dx%-5d  %8.1f ms  %6d nuclei  %9d px%n",
                name, x, y, w, h, ms, nuclei.size(), areaTotal);

        var out = new StringBuilder();
        out.append(String.format(Locale.ROOT,
                "    {\"name\": \"%s\", \"x\": %d, \"y\": %d, \"width\": %d, \"height\": %d,\n"
                        + "     \"nuclei\": %d, \"areaTotal\": %d,\n"
                        + "     \"objects\": [\n",
                name, x, y, w, h, nuclei.size(), areaTotal));
        for (int i = 0; i < nuclei.size(); i++) {
            out.append(nuclei.get(i).toJson());
            out.append(i == nuclei.size() - 1 ? "\n" : ",\n");
        }
        out.append("     ]}");
        return out.toString();
    }

    /** One nucleus: its box in region-local pixels, and the pixels inside it. */
    private record Nucleus(int x, int y, int w, int h, int area, String mask) {

        /**
         * Takes the mask from {@link Roi#getMask()}, which is ImageJ's own
         * rasterisation of the ROI, so no rasteriser of ours sits inside the
         * comparison this feeds.
         *
         * <p>{@code getMask} returns null for a ROI that fills its bounding box
         * exactly -- a rectangle, which a traced nucleus is not, but a
         * one-pixel one would be. That case is written as a full box rather
         * than skipped, because dropping an object silently is the failure this
         * whole fixture exists to make impossible.
         */
        static Nucleus of(Roi roi) {
            Rectangle b = roi.getBounds();
            ImageProcessor mask = roi.getMask();
            int pixels = b.width * b.height;
            byte[] bits = new byte[(pixels + 7) / 8];
            int area = 0;
            for (int i = 0; i < pixels; i++) {
                boolean inside = mask == null || mask.get(i % b.width, i / b.width) != 0;
                if (inside) {
                    bits[i >> 3] |= (byte) (1 << (i & 7));
                    area++;
                }
            }
            return new Nucleus(b.x, b.y, b.width, b.height, area, hex(bits));
        }

        String toJson() {
            return String.format(Locale.ROOT,
                    "       {\"x\": %d, \"y\": %d, \"w\": %d, \"h\": %d, \"area\": %d, \"mask\": \"%s\"}",
                    x, y, w, h, area, mask);
        }
    }

    /** Little-endian within each byte, matching every other packed mask here. */
    private static String hex(byte[] bytes) {
        var sb = new StringBuilder(bytes.length * 2);
        for (byte b : bytes)
            sb.append(String.format(Locale.ROOT, "%02x", b & 0xff));
        return sb.toString();
    }
}
