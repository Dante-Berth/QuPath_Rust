import ij.plugin.filter.RankFilters;
import ij.process.FloatProcessor;

/**
 * Records ImageJ's circular-kernel MIN and MAX rank filters as QuPath invokes
 * them from WatershedCellDetection, so the Rust port is checked against real
 * output rather than against our reading of RankFilters.
 *
 * Radii are the ones that matter: the kernel ImageJ builds for a given radius
 * is not the naive disc, and the exact inclusion rule at the boundary is the
 * thing most likely to be got wrong.
 */
public class RankFilterMinMax {

    public static void main(String[] args) {
        var out = new StringBuilder();
        out.append("{\n  \"cases\": [\n");

        // Small deterministic images across a range of radii. Non-square sizes
        // and prime-ish dimensions catch row/column indexing mistakes.
        int[][] sizes = {{7, 5}, {11, 9}, {16, 16}};
        double[] radii = {1.0, 1.5, 2.0, 2.5, 3.0, 5.0};

        for (int[] size : sizes) {
            for (double radius : radii) {
                float[] pixels = ramp(size[0], size[1]);
                emit(out, size[0], size[1], radius, RankFilters.MIN, "min", pixels);
                emit(out, size[0], size[1], radius, RankFilters.MAX, "max", pixels);
            }
        }

        // A single bright pixel: MAX dilates it into exactly the kernel shape,
        // which pins the kernel geometry itself rather than just the result.
        for (double radius : radii) {
            float[] impulse = new float[15 * 15];
            impulse[7 * 15 + 7] = 100f;
            emit(out, 15, 15, radius, RankFilters.MAX, "max", impulse);
        }

        out.setLength(out.length() - 2);
        out.append("\n  ]\n}\n");
        System.out.print(out);

        // RankFilters leaves non-daemon worker threads alive, so the JVM would
        // otherwise hang here with the fixture already fully written.
        System.out.flush();
        System.exit(0);
    }

    /** A ramp with a couple of extrema, so MIN and MAX differ everywhere. */
    private static float[] ramp(int w, int h) {
        var a = new float[w * h];
        for (int y = 0; y < h; y++)
            for (int x = 0; x < w; x++)
                a[y * w + x] = (x * 3 + y * 7) % 11 + (x == y ? 20f : 0f);
        return a;
    }

    private static void emit(StringBuilder out, int w, int h, double radius,
                             int type, String name, float[] pixels) {
        var fp = new FloatProcessor(w, h, pixels.clone(), null);
        new RankFilters().rank(fp, radius, type);

        out.append("    {\n");
        out.append("      \"op\": \"").append(name).append("\",\n");
        out.append("      \"width\": ").append(w).append(",\n");
        out.append("      \"height\": ").append(h).append(",\n");
        out.append("      \"radius\": ").append(radius).append(",\n");
        append(out, "input", pixels);
        out.append(",\n");
        append(out, "output", (float[]) fp.getPixels());
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
