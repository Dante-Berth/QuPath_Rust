import ij.gui.PolygonRoi;
import ij.gui.Roi;
import ij.gui.Wand;
import ij.plugin.filter.EDM;
import ij.process.ByteProcessor;
import ij.process.FloatProcessor;
import ij.process.ImageProcessor;
import qupath.imagej.processing.RoiLabeling;
import qupath.imagej.processing.Watershed;

import java.util.List;
import java.util.Random;

/**
 * Records cell expansion, which is what {@code WatershedCellDetection} does to
 * turn nuclei into the cells the seam resolution actually compares:
 *
 * <pre>
 *     if (cellExpansion &gt; 0) {
 *         FloatProcessor fpEDM = new EDM().makeFloatEDM(bp, (byte)255, false);
 *         fpEDM.multiply(-1);
 *         double cellExpansionThreshold = -cellExpansion;
 *         ImageProcessor ipLabelsCells = ipLabels.duplicate();
 *         Watershed.doWatershed(fpEDM, ipLabelsCells, cellExpansionThreshold, false);
 *         PolygonRoi[] roisCells = RoiLabeling.labelsToFilledROIs(ipLabelsCells, roisNuclei.size());
 *     }
 * </pre>
 *
 * <p>{@code cellExpansion} is 5 µm by default and is divided by the pixel size
 * before it gets here, so on CMU-1 it is a little over 20 px. It is never zero
 * on a default run, which is what makes {@code DIVERGENCES.md} D-008 real: the
 * 10% overlap test runs on these polygons, not on the nuclei.
 *
 * <p>Four checkpoints, because each is a separable piece of work and a single
 * end-to-end mask would say only that the whole thing disagrees:
 *
 * <ul>
 * <li>{@code edm} — {@code makeFloatEDM(bp, (byte)255, false)}, <b>before</b>
 *     the negation.
 * <li>{@code surface} — the same after {@code multiply(-1)}, which is the image
 *     the flood sees.
 * <li>{@code cellLabels} — what {@code doWatershed} leaves, one label per cell.
 * <li>{@code cytoplasmLabels} — the cell labels with every nucleus pixel
 *     zeroed, which is the next thing the call site builds.
 * </ul>
 *
 * <p>Three things decide the answer and none is visible at the call site:
 *
 * <ul>
 * <li><b>The EDM's background argument is 255, not 0.</b> Every other EDM in
 *     this pipeline passes 0. Here the sense is inverted: distances are measured
 *     <i>outward from the nuclei into the background</i>, so the value that
 *     counts as "background" for the distance transform is the foreground of the
 *     mask. A port that reuses the 0-background EDM gets a distance map of the
 *     wrong region entirely and the cells come out as the gaps between nuclei.
 *     {@code two-nuclei-apart} is the case that separates them.
 * <li><b>The negation is what makes a distance a priority.</b> The flood pops
 *     high values first, so negating makes it expand from each nucleus outward
 *     in increasing distance — nearest pixels claimed first — which is what
 *     makes the boundary between two cells fall on the midline rather than
 *     wherever the queue happened to arrive. {@code two-nuclei-touching} pins
 *     that midline.
 * <li><b>The threshold is {@code -cellExpansion}, so the bound is a distance.</b>
 *     {@code doWatershed} never floods a pixel whose value is {@code <=}
 *     the threshold, and after the negation that is exactly "further than
 *     {@code cellExpansion} from any nucleus". The comparison is strict on the
 *     wrong side of what one would guess: a pixel at <i>exactly</i>
 *     {@code cellExpansion} has value {@code -cellExpansion}, which is not
 *     {@code >} the threshold, so it is <b>not</b> claimed.
 *     {@code exact-distance-boundary} is built to sit on that tie.
 * </ul>
 *
 * <p>{@code labelsToFilledROIs} is recorded by its count and its per-label
 * filled masks rather than by polygons: the polygons are
 * {@code WandOutline.json}'s subject and the smoothing chain is
 * {@code SmoothBoundaries.json}'s, so what is new here is which pixels each cell
 * ends up owning.
 */
public class CellExpansion {

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"cases\": [\n");

        // One nucleus in open space: the cell is a disk of radius
        // (nucleus radius + expansion), and nothing competes for the pixels.
        // This is the case that isolates the EDM's background argument, because
        // a 0-background EDM would expand the background instead.
        int[] single = new int[41 * 41];
        disk(single, 41, 20, 20, 5);
        out.append(record("single-nucleus", 41, 41, single, 6.0, true));

        // Two nuclei far enough apart that neither reaches the other: both
        // cells are full disks and the watershed line never forms. The pair
        // with the next case is what shows the expansion is bounded.
        int[] apart = new int[61 * 31];
        disk(apart, 61, 14, 15, 4);
        disk(apart, 61, 46, 15, 4);
        out.append(record("two-nuclei-apart", 61, 31, apart, 5.0, false));

        // Two nuclei close enough that the expansions collide. The boundary
        // must land on the midline between them, and the watershed line is the
        // pixel column where two labels meet and neither wins.
        int[] near = new int[41 * 31];
        disk(near, 41, 14, 15, 4);
        disk(near, 41, 26, 15, 4);
        out.append(record("two-nuclei-touching", 41, 31, near, 8.0, false));

        // A nucleus whose expansion runs off every edge. `false` for the EDM's
        // edge flag means the border is not background, so the cell is cut by
        // the image bound rather than by a distance — the same halo argument
        // the nucleus EDM already turns on.
        int[] border = new int[21 * 21];
        disk(border, 21, 2, 2, 3);
        disk(border, 21, 18, 18, 3);
        out.append(record("border-nucleus", 21, 21, border, 7.0, false));

        // An integer expansion against a nucleus with a flat side, so pixels sit
        // at exactly `cellExpansion` from it. That is the tie the threshold
        // comparison decides, and it is decided against claiming them.
        int[] flat = new int[31 * 21];
        for (int y = 8; y < 13; y++)
            for (int x = 13; x < 18; x++)
                flat[y * 31 + x] = 1;
        out.append(record("exact-distance-boundary", 31, 21, flat, 4.0, false));

        // Three nuclei of different sizes in a row: the big one's expansion
        // reaches further, so the two watershed lines are not equidistant and a
        // port that splits the gap evenly disagrees here.
        int[] unequal = new int[71 * 31];
        disk(unequal, 71, 14, 15, 7);
        disk(unequal, 71, 36, 15, 3);
        disk(unequal, 71, 56, 15, 5);
        out.append(record("unequal-nuclei", 71, 31, unequal, 9.0, false));

        // A nucleus with a concavity, so the EDM inside the notch is not a
        // simple radial ramp and the expanded boundary is not a dilation of the
        // outline.
        int[] notched = new int[41 * 41];
        for (int y = 12; y < 29; y++)
            for (int x = 12; x < 29; x++)
                notched[y * 41 + x] = 1;
        for (int y = 12; y < 21; y++)
            for (int x = 18; x < 23; x++)
                notched[y * 41 + x] = 0;
        out.append(record("notched-nucleus", 41, 41, notched, 5.0, false));

        // A fractional expansion, which is what a real run passes: 5 µm over
        // CMU-1's 0.2299 µm/px is 21.75 px, never an integer. Rounding it would
        // move every boundary.
        int[] fractional = new int[51 * 31];
        disk(fractional, 51, 16, 15, 4);
        disk(fractional, 51, 34, 15, 4);
        out.append(record("fractional-expansion", 51, 31, fractional, 6.37, false));

        // Random fields of small nuclei, which produce the three-way meetings,
        // one-pixel corridors and plateau ties nobody would think to write down.
        out.append(record("random-sparse", 48, 32, randomNuclei(48, 32, 81, 14, 3), 6.0, false));
        out.append(record("random-dense", 48, 32, randomNuclei(48, 32, 82, 30, 2), 5.0, false));

        out.append("\n  ]\n}\n");
        System.out.print(out);
        System.out.flush();
    }

    /**
     * Records the expansion for one nucleus mask.
     *
     * <p>{@code nuclei} is given as 0/1 and widened to 0/255 here, so the cases
     * above read as shapes rather than as byte values. The labels the flood
     * starts from are the connected components of that mask, which is what
     * {@code ipLabels} holds at the call site.
     */
    private static String record(String name, int width, int height, int[] nuclei,
            double cellExpansion, boolean first) {
        var out = new StringBuilder();
        if (!first) out.append(",\n");

        ByteProcessor bp = new ByteProcessor(width, height);
        for (int i = 0; i < nuclei.length; i++)
            bp.set(i, nuclei[i] != 0 ? 255 : 0);

        // The nucleus labels the call site already has, built the way it builds
        // them: trace the mask, then label the traced ROIs.
        bp.setThreshold(127, Double.POSITIVE_INFINITY, ImageProcessor.NO_LUT_UPDATE);
        List<PolygonRoi> roisNuclei = RoiLabeling.getFilledPolygonROIs(bp, Wand.FOUR_CONNECTED);
        ImageProcessor ipLabels = new FloatProcessor(width, height);
        RoiLabeling.labelROIs(ipLabels, roisNuclei);

        // --- the call site, verbatim ---
        FloatProcessor fpEDM = new EDM().makeFloatEDM(bp, (byte) 255, false);
        float[] edm = ((float[]) fpEDM.getPixels()).clone();

        fpEDM.multiply(-1);
        float[] surface = ((float[]) fpEDM.getPixels()).clone();

        double cellExpansionThreshold = -cellExpansion;
        ImageProcessor ipLabelsCells = ipLabels.duplicate();
        Watershed.doWatershed(fpEDM, ipLabelsCells, cellExpansionThreshold, false);
        PolygonRoi[] roisCells = RoiLabeling.labelsToFilledROIs(ipLabelsCells, roisNuclei.size());

        float[] cellLabels = new float[width * height];
        for (int i = 0; i < cellLabels.length; i++)
            cellLabels[i] = ipLabelsCells.getf(i);

        // The cytoplasm image the call site builds next: cell labels with every
        // nucleus pixel removed.
        ImageProcessor ipCytoplasm = ipLabelsCells.duplicate();
        for (int i = 0; i < width * height; i++)
            if (ipLabels.getf(i) != 0)
                ipCytoplasm.setf(i, 0f);
        float[] cytoplasmLabels = new float[width * height];
        for (int i = 0; i < cytoplasmLabels.length; i++)
            cytoplasmLabels[i] = ipCytoplasm.getf(i);

        // How many pixels each cell owns, which is the number the overlap test
        // turns on and the one thing a mask comparison could get right
        // pixel-by-pixel and still report wrongly. A null entry means
        // labelsToFilledROIs found no pixels for that label.
        int[] cellAreas = new int[roisCells.length];
        for (int i = 0; i < roisCells.length; i++) {
            if (roisCells[i] == null) {
                cellAreas[i] = -1;
                continue;
            }
            ByteProcessor filled = new ByteProcessor(width, height);
            filled.setValue(255);
            filled.fill(roisCells[i]);
            int count = 0;
            for (int j = 0; j < width * height; j++)
                if (filled.get(j) != 0)
                    count++;
            cellAreas[i] = count;
        }

        float[] nucleusLabels = new float[width * height];
        for (int i = 0; i < nucleusLabels.length; i++)
            nucleusLabels[i] = ipLabels.getf(i);

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"width\": ").append(width)
                .append(", \"height\": ").append(height)
                .append(", \"cellExpansion\": ").append(cellExpansion)
                .append(", \"nucleusCount\": ").append(roisNuclei.size())
                .append(", \"cellCount\": ").append(roisCells.length)
                .append(",\n     \"cellAreas\": ");
        appendInts(out, cellAreas);
        out.append(",\n     \"nuclei\": ");
        appendInts(out, toBytes(nuclei));
        out.append(",\n     \"nucleusLabels\": ");
        appendFloats(out, nucleusLabels);
        out.append(",\n     \"edm\": ");
        appendFloats(out, edm);
        out.append(",\n     \"surface\": ");
        appendFloats(out, surface);
        out.append(",\n     \"cellLabels\": ");
        appendFloats(out, cellLabels);
        out.append(",\n     \"cytoplasmLabels\": ");
        appendFloats(out, cytoplasmLabels);
        out.append("}");
        return out.toString();
    }

    private static int[] toBytes(int[] mask) {
        int[] bytes = new int[mask.length];
        for (int i = 0; i < mask.length; i++)
            bytes[i] = mask[i] != 0 ? 255 : 0;
        return bytes;
    }

    /** A filled disk, which is the shape a nucleus approximates. */
    private static void disk(int[] mask, int width, int cx, int cy, int r) {
        int height = mask.length / width;
        for (int y = Math.max(0, cy - r); y < Math.min(height, cy + r + 1); y++)
            for (int x = Math.max(0, cx - r); x < Math.min(width, cx + r + 1); x++)
                if ((x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r)
                    mask[y * width + x] = 1;
    }

    /** Scattered disks, placed without regard to whether they overlap. */
    private static int[] randomNuclei(int width, int height, long seed, int count, int radius) {
        Random random = new Random(seed);
        int[] mask = new int[width * height];
        for (int i = 0; i < count; i++)
            disk(mask, width, random.nextInt(width), random.nextInt(height), radius);
        return mask;
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
