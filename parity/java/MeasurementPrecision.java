/*
 * Records the precision actually retained by each QuPath MeasurementList type,
 * and the value returned for an absent measurement.
 *
 * This backs DIVERGENCES.md D-001: DOUBLE and FLOAT both truncate to float, so
 * f32 columns reproduce them bit-exactly, while GENERAL retains full double.
 *
 * Values are emitted as raw IEEE-754 bit patterns as well as decimal, because
 * the whole point of the fixture is the bits below the decimal representation.
 */
import qupath.lib.measurements.MeasurementList;
import qupath.lib.measurements.MeasurementListFactory;

public class MeasurementPrecision {

    /** Values chosen to need more than a float's 24-bit mantissa. */
    private static final double[] VALUES = {
        0.1234567890123456789,
        1.0 / 3.0,
        Math.PI,
        1e-8,
        123456789.123456789,
        0.0,
        -1.5,          // exactly representable, must survive both paths
        Double.NaN,
    };

    public static void main(String[] args) {
        StringBuilder out = new StringBuilder();
        out.append("{\n  \"source\": \"qupath-core MeasurementList\",\n");
        out.append("  \"absent_measurement_returns\": \"NaN\",\n");
        out.append("  \"cases\": [\n");

        boolean firstCase = true;
        for (double v : VALUES) {
            if (!firstCase) out.append(",\n");
            firstCase = false;
            out.append("    {\n");
            out.append(String.format("      \"input_bits\": \"%016x\",%n",
                Double.doubleToRawLongBits(v)));
            out.append("      \"stored\": {");
            boolean firstType = true;
            for (var type : MeasurementList.MeasurementListType.values()) {
                MeasurementList list =
                    MeasurementListFactory.createMeasurementList(4, type);
                list.put("x", v);
                double got = list.get("x");
                if (!firstType) out.append(",");
                firstType = false;
                out.append(String.format("%n        \"%s\": \"%016x\"",
                    type, Double.doubleToRawLongBits(got)));
            }
            out.append("\n      }\n    }");
        }

        out.append("\n  ]\n}\n");

        // Confirm the invariant this fixture exists to pin down, so a change
        // upstream fails loudly here rather than silently shifting the fixture.
        for (double v : VALUES) {
            MeasurementList d =
                MeasurementListFactory.createMeasurementList(4, MeasurementList.MeasurementListType.DOUBLE);
            MeasurementList f =
                MeasurementListFactory.createMeasurementList(4, MeasurementList.MeasurementListType.FLOAT);
            d.put("x", v);
            f.put("x", v);
            if (Double.doubleToRawLongBits(d.get("x")) != Double.doubleToRawLongBits(f.get("x"))) {
                throw new AssertionError(
                    "DOUBLE and FLOAT diverged for " + v + " — D-001 no longer holds");
            }
        }

        // Absent measurement must be NaN, not zero and not an exception.
        MeasurementList empty =
            MeasurementListFactory.createMeasurementList(4, MeasurementList.MeasurementListType.DOUBLE);
        if (!Double.isNaN(empty.get("nope"))) {
            throw new AssertionError("absent measurement was not NaN");
        }

        System.out.print(out);
    }
}
