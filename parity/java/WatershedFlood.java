import ij.process.FloatProcessor;
import qupath.imagej.processing.Watershed;

/**
 * Records QuPath's watershed output for a set of small fixed images, so the
 * Rust implementation is checked against QuPath's real labelling rather than
 * against our reading of its source.
 *
 * The cases deliberately include plateaus, where the FIFO tie-breaking that
 * QuPath buys with its insertion counter decides where the watershed line
 * lands. A queue that gets the ordering wrong produces different labels here
 * even though it is "sorted correctly".
 */
public class WatershedFlood {

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"cases\": [\n");

        // Ridge between two markers: the classic split.
        emit(out, "ridge", 5, 3,
                new float[]{
                        3, 2, 1, 2, 3,
                        3, 2, 1, 2, 3,
                        3, 2, 1, 2, 3},
                new float[]{
                        1, 0, 0, 0, 2,
                        0, 0, 0, 0, 0,
                        0, 0, 0, 0, 0},
                Float.NEGATIVE_INFINITY, false);

        // Flat plateau: every value equal, so insertion order alone decides.
        emit(out, "plateau_four", 6, 4,
                flat(6 * 4, 1f),
                new float[]{
                        1, 0, 0, 0, 0, 2,
                        0, 0, 0, 0, 0, 0,
                        0, 0, 0, 0, 0, 0,
                        0, 0, 0, 0, 0, 0},
                Float.NEGATIVE_INFINITY, false);

        // Same plateau, eight-connected: diagonal propagation changes the line.
        emit(out, "plateau_eight", 6, 4,
                flat(6 * 4, 1f),
                new float[]{
                        1, 0, 0, 0, 0, 2,
                        0, 0, 0, 0, 0, 0,
                        0, 0, 0, 0, 0, 0,
                        0, 0, 0, 0, 0, 0},
                Float.NEGATIVE_INFINITY, true);

        // A threshold that cuts the image in half.
        emit(out, "threshold", 5, 3,
                new float[]{
                        5, 5, 0, 5, 5,
                        5, 5, 0, 5, 5,
                        5, 5, 0, 5, 5},
                new float[]{
                        1, 0, 0, 0, 2,
                        0, 0, 0, 0, 0,
                        0, 0, 0, 0, 0},
                1f, false);

        // Three markers meeting at a point.
        emit(out, "three_way", 7, 7,
                radial(7, 7),
                marker(7, 7, new int[][]{{0, 0, 1}, {6, 0, 2}, {3, 6, 3}}),
                Float.NEGATIVE_INFINITY, true);

        out.setLength(out.length() - 2);
        out.append("\n  ]\n}\n");
        System.out.print(out);
    }

    private static float[] flat(int n, float v) {
        var a = new float[n];
        java.util.Arrays.fill(a, v);
        return a;
    }

    private static float[] radial(int w, int h) {
        var a = new float[w * h];
        for (int y = 0; y < h; y++)
            for (int x = 0; x < w; x++) {
                double dx = x - (w - 1) / 2.0, dy = y - (h - 1) / 2.0;
                a[y * w + x] = (float) Math.floor(10 - Math.sqrt(dx * dx + dy * dy));
            }
        return a;
    }

    private static float[] marker(int w, int h, int[][] seeds) {
        var a = new float[w * h];
        for (int[] s : seeds) a[s[1] * w + s[0]] = s[2];
        return a;
    }

    private static void emit(StringBuilder out, String name, int w, int h,
                             float[] values, float[] labels, float minThreshold, boolean conn8) {
        var fpValues = new FloatProcessor(w, h, values.clone(), null);
        var fpLabels = new FloatProcessor(w, h, labels.clone(), null);

        Watershed.doWatershed(fpValues, fpLabels, minThreshold, conn8);

        out.append("    {\n");
        out.append("      \"name\": \"").append(name).append("\",\n");
        out.append("      \"width\": ").append(w).append(",\n");
        out.append("      \"height\": ").append(h).append(",\n");
        out.append("      \"conn8\": ").append(conn8).append(",\n");
        out.append("      \"minThreshold\": ").append(
                Float.isInfinite(minThreshold) ? "\"-inf\"" : String.valueOf(minThreshold)).append(",\n");
        append(out, "values", values);
        out.append(",\n");
        append(out, "labelsIn", labels);
        out.append(",\n");
        append(out, "labelsOut", (float[]) fpLabels.getPixels());
        out.append("\n    },\n");
    }

    private static void append(StringBuilder out, String key, float[] a) {
        out.append("      \"").append(key).append("\": [");
        for (int i = 0; i < a.length; i++) {
            if (i > 0) out.append(", ");
            out.append(a[i]);
        }
        out.append("]");
    }
}
