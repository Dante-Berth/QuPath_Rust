/*
 * Records the colour QuPath's default colour map gives a value, for a display
 * range. Backs crates/qupath-viewer/src/colour_map.rs: the INFERNO table and
 * the index rule there are this, and tests/parity_colour_map.rs holds them to
 * it entry by entry.
 *
 * ColorMap.getColor(value, min, max) is the whole of what a measurement map
 * does per object once MeasurementMapper has picked the range (its data
 * minimum and maximum) and dropped NaN, so this records that call and nothing
 * around it.
 *
 * Inputs are floats, written with Float.toString so they read back bit for bit
 * as f32: QuPath's detection measurements are floats widened to double, and so
 * are the Rust side's.
 *
 * Four ranges, each for a reason:
 *   - an ordinary one whose width is not 1 and whose low end is not 0, swept
 *     past both ends so the clamping is recorded;
 *   - one where value * 256 / width lands exactly on k + 0.5, so every
 *     rounding tie is recorded rather than none of them;
 *   - a reversed one (min > max), which QuPath reads backwards;
 *   - one of no width, where the Java divides by zero.
 */
import qupath.lib.color.ColorMaps.ColorMap;
import qupath.lib.color.ColorMaps;

import java.util.ArrayList;
import java.util.List;

public class ColourMap {

    private static void range(StringBuilder out, ColorMap map, String name,
                              float min, float max, List<Float> values, boolean last) {
        out.append("    {\"name\": \"").append(name).append("\", ");
        out.append("\"min\": \"").append(Float.toString(min)).append("\", ");
        out.append("\"max\": \"").append(Float.toString(max)).append("\",\n");
        out.append("     \"values\": [");
        for (int i = 0; i < values.size(); i++) {
            if (i > 0) out.append(", ");
            out.append('"').append(Float.toString(values.get(i))).append('"');
        }
        out.append("],\n     \"colours\": [");
        for (int i = 0; i < values.size(); i++) {
            if (i > 0) out.append(", ");
            int rgb = map.getColor(values.get(i), min, max) & 0xffffff;
            out.append('"').append(String.format("%06x", rgb)).append('"');
        }
        out.append("]}").append(last ? "\n" : ",\n");
    }

    public static void main(String[] args) {
        ColorMap map = ColorMaps.getDefaultColorMap();

        List<Float> sweep = new ArrayList<>();
        float min = 0.3f, max = 2.7f;
        for (int i = -300; i <= 1500; i++) {
            sweep.add(min + (max - min) * (i / 1200.0f));
        }
        sweep.add(Float.NaN);
        sweep.add(Float.POSITIVE_INFINITY);
        sweep.add(Float.NEGATIVE_INFINITY);

        List<Float> ties = new ArrayList<>();
        for (int k = 0; k < 256; k++) {
            ties.add((k + 0.5f) * 2.0f);
        }

        List<Float> reversed = new ArrayList<>();
        for (int i = 0; i <= 64; i++) {
            reversed.add(0.3f + 2.4f * (i / 64.0f));
        }

        List<Float> flat = List.of(0.5f, 1.0f, 1.5f);

        StringBuilder out = new StringBuilder();
        out.append("{\n  \"source\": \"qupath-core ColorMaps.getDefaultColorMap().getColor\",\n");
        out.append("  \"map\": \"").append(map.getName()).append("\",\n");
        out.append("  \"ranges\": [\n");
        range(out, map, "sweep", min, max, sweep, false);
        range(out, map, "ties", 0.0f, 512.0f, ties, false);
        range(out, map, "reversed", 2.7f, 0.3f, reversed, false);
        range(out, map, "no width", 1.0f, 1.0f, flat, true);
        out.append("  ]\n}\n");
        System.out.print(out);
        System.exit(0);
    }
}
