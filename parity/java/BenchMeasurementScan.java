/*
 * Benchmarks QuPath's per-object MeasurementList against the workload the
 * columnar Rust table is designed for: build N detections with F measurements
 * each, then repeatedly scan one feature across all of them and filter on it.
 *
 * This is the Java side of the comparison. It prints JSON to stdout; the Rust
 * side is `cargo bench -p qupath-objects`. Both must be run on the same machine
 * with the same N and F for the numbers to mean anything.
 *
 * The JVM is warmed before timing and its startup is excluded. Timing covers
 * only the scan and filter phases, not construction, so the two sides measure
 * the same thing.
 */
import qupath.lib.measurements.MeasurementList;
import qupath.lib.measurements.MeasurementListFactory;

import java.util.ArrayList;
import java.util.List;

public class BenchMeasurementScan {

    private static final int N_OBJECTS = 1_000_000;
    private static final int N_FEATURES = 20;
    private static final int WARMUP_ITERS = 20;
    private static final int TIMED_ITERS = 50;

    /** The feature the scan and filter target; deliberately not the first. */
    private static final String TARGET = "Feature 13";

    /** Sum of TARGET over all objects, and the count above the threshold. */
    private static final double EXPECTED_SUM = 499_500_000.0;
    private static final long EXPECTED_COUNT = 499_000L;

    private static List<MeasurementList> build() {
        List<MeasurementList> objects = new ArrayList<>(N_OBJECTS);
        for (int i = 0; i < N_OBJECTS; i++) {
            // GENERAL is what PathObject uses by default.
            MeasurementList list = MeasurementListFactory.createMeasurementList(
                N_FEATURES, MeasurementList.MeasurementListType.GENERAL);
            for (int f = 0; f < N_FEATURES; f++) {
                list.put("Feature " + f, (i * 31 + f) % 1000);
            }
            list.close(); // compacts storage and interns names, as QuPath does
            objects.add(list);
        }
        return objects;
    }

    /** Sum one feature across every object. Returned so JIT cannot elide it. */
    private static double scan(List<MeasurementList> objects) {
        double total = 0;
        for (MeasurementList list : objects) {
            total += list.get(TARGET);
        }
        return total;
    }

    /** Count objects whose target feature exceeds a threshold. */
    private static long filter(List<MeasurementList> objects) {
        long n = 0;
        for (MeasurementList list : objects) {
            if (list.get(TARGET) > 500.0) {
                n++;
            }
        }
        return n;
    }

    public static void main(String[] args) {
        // JSON requires '.' as the decimal separator regardless of the host
        // locale, which would otherwise emit "42,15" and produce invalid JSON.
        java.util.Locale.setDefault(java.util.Locale.ROOT);

        long buildStart = System.nanoTime();
        List<MeasurementList> objects = build();
        long buildNanos = System.nanoTime() - buildStart;

        // Both sides must compute the same answers, or the comparison is
        // meaningless — the usual way a scan gets fast is by not happening.
        if (scan(objects) != EXPECTED_SUM) {
            throw new AssertionError("scan sum was " + scan(objects)
                + ", expected " + EXPECTED_SUM + " — benchmark is invalid");
        }
        if (filter(objects) != EXPECTED_COUNT) {
            throw new AssertionError("filter count was " + filter(objects)
                + ", expected " + EXPECTED_COUNT + " — benchmark is invalid");
        }

        // Warm up: let C2 compile the scan and filter loops before timing.
        double sink = 0;
        for (int i = 0; i < WARMUP_ITERS; i++) {
            sink += scan(objects);
            sink += filter(objects);
        }

        long[] scanNanos = new long[TIMED_ITERS];
        for (int i = 0; i < TIMED_ITERS; i++) {
            long t = System.nanoTime();
            sink += scan(objects);
            scanNanos[i] = System.nanoTime() - t;
        }

        long[] filterNanos = new long[TIMED_ITERS];
        for (int i = 0; i < TIMED_ITERS; i++) {
            long t = System.nanoTime();
            sink += filter(objects);
            filterNanos[i] = System.nanoTime() - t;
        }

        if (sink == Double.NEGATIVE_INFINITY) {
            System.err.println("unreachable, defeats dead-code elimination");
        }

        System.out.printf("{%n");
        System.out.printf("  \"n_objects\": %d,%n", N_OBJECTS);
        System.out.printf("  \"n_features\": %d,%n", N_FEATURES);
        System.out.printf("  \"warmup_iters\": %d,%n", WARMUP_ITERS);
        System.out.printf("  \"timed_iters\": %d,%n", TIMED_ITERS);
        System.out.printf("  \"list_type\": \"GENERAL\",%n");
        System.out.printf("  \"build_ms\": %.3f,%n", buildNanos / 1e6);
        System.out.printf("  \"scan_ms\": %s,%n", stats(scanNanos));
        System.out.printf("  \"filter_ms\": %s,%n", stats(filterNanos));
        System.out.printf("  \"jvm\": \"%s\"%n", System.getProperty("java.vm.version"));
        System.out.printf("}%n");
    }

    /** Median and minimum in milliseconds; median is the headline figure. */
    private static String stats(long[] nanos) {
        long[] sorted = nanos.clone();
        java.util.Arrays.sort(sorted);
        double median = sorted[sorted.length / 2] / 1e6;
        double min = sorted[0] / 1e6;
        return String.format("{\"median\": %.4f, \"min\": %.4f}", median, min);
    }
}
