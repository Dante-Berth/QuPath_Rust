import ij.plugin.filter.RankFilters;
import ij.process.FloatProcessor;

/**
 * Times ImageJ's MIN/MAX rank filter on the same workload as the Rust
 * criterion benchmark, so the two numbers are comparable.
 *
 * Driven by bench.sh, not by record.sh. ImageJ's RankFilters uses a thread
 * pool internally; that is left as-is, because it is what QuPath actually runs.
 */
public class BenchRankFilter {

    public static void main(String[] args) {
        int width = 512;
        int height = 512;
        int warmup = 20;
        int timed = 50;

        float[] source = new float[width * height];
        for (int i = 0; i < source.length; i++) {
            source[i] = (i * 37) % 251;
        }

        for (double radius : new double[]{2.0, 5.0}) {
            // Warm the JIT before timing; these iterations are discarded.
            for (int i = 0; i < warmup; i++) {
                run(source, width, height, radius);
            }

            long[] times = new long[timed];
            double guard = 0;
            for (int i = 0; i < timed; i++) {
                long t0 = System.nanoTime();
                float[] out = run(source, width, height, radius);
                times[i] = System.nanoTime() - t0;
                guard += out[0];
            }

            java.util.Arrays.sort(times);
            double medianMs = times[timed / 2] / 1e6;
            System.out.printf("rank_512x512/max_r%.0f  median %.3f ms  (%.3f Melem/s)  guard=%.1f%n",
                    radius, medianMs, (width * height) / (medianMs * 1000.0), guard);
        }

        // RankFilters leaves non-daemon worker threads alive, so the JVM does
        // not exit on its own. Everything is written by now.
        System.out.flush();
        System.exit(0);
    }

    private static float[] run(float[] source, int width, int height, double radius) {
        var fp = new FloatProcessor(width, height, source.clone(), null);
        new RankFilters().rank(fp, radius, RankFilters.MAX);
        return (float[]) fp.getPixels();
    }
}
