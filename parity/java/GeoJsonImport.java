import qupath.lib.io.PathIO;
import qupath.lib.objects.PathObject;

import java.nio.file.Path;
import java.util.List;

/**
 * Records what QuPath's own reader makes of a GeoJSON file <em>this project
 * wrote</em>.
 *
 * <p>Every other recorder here runs QuPath and pins the answer so the Rust port
 * can be checked against it. This one runs in the opposite direction: the input
 * is {@code parity/samples/detections.geojson}, produced by
 * {@code cargo run -p qupath-viewer --example geojson_sample}, and what is
 * recorded is QuPath's reading of it. {@code docs/VIEWER.md} §8.8 called this
 * out as the honest gap — both file formats had been checked against a JSON
 * parser and against the Java that writes them, which is a weaker claim than
 * "QuPath opened it".
 *
 * <p>The entry point is {@link PathIO#readObjects(Path)}, which is what
 * {@code Open objects} in the GUI reaches, so this is the same code path a
 * person double-clicking the file gets rather than a hand-driven Gson call.
 *
 * <p>Per object it records what a reader can get wrong without failing:
 *
 * <ul>
 * <li>{@code detection} — whether {@code objectType} was honoured. A file whose
 *     spelling QuPath does not recognise still opens; the objects come back as
 *     annotations, land in the annotation tools, and nothing says so.
 * <li>{@code roi} and {@code points} — what the geometry became. Two things
 *     here were guessed wrong before this was run, which is the argument for
 *     running it. QuPath <em>keeps</em> the closing position GeoJSON requires,
 *     so a ring of eight vertices comes back with nine points rather than
 *     eight. And an axis-aligned four-corner ring does not come back a polygon
 *     at all: the reader recognises it and builds a <b>Rectangle</b> ROI, whose
 *     four points are its corners. Neither changes the geometry — the areas and
 *     bounds below are exact — and both would have made a Rust-side assertion
 *     about vertex counts fail for a reason that is not a bug.
 * <li>{@code area}, {@code centroidX}, {@code centroidY}, and the bounds — the
 *     geometry itself, in the level-0 frame the file is written in. This is
 *     what would be silently wrong if the coordinates had been written at the
 *     level a run read: every object in the corner, at the wrong size.
 * <li>{@code valid} — {@code Geometry.isValid()} from JTS. Rings here are wound
 *     clockwise, which is the opposite of what the GeoJSON specification asks
 *     for an exterior ring, and this is where that would show up if it mattered.
 * <li>{@code measurements} — the names and values, which is the half of the
 *     export a picture of outlines does not carry, and the half that a skipped
 *     object would shift by one.
 * </ul>
 *
 * <p>Objects come back in file order, and that is asserted rather than assumed
 * on the Rust side: the fixture is a list, and a reader that reordered would
 * pair every measurement with the wrong outline.
 */
public class GeoJsonImport {

    public static void main(String[] args) throws Exception {
        Path path = Path.of(args.length > 0 ? args[0] : "parity/samples/detections.geojson");
        List<PathObject> objects = PathIO.readObjects(path);

        var out = new StringBuilder();
        out.append("{\n  \"source\": \"").append(path.getFileName()).append("\",\n");
        out.append("  \"objects\": [\n");
        for (int i = 0; i < objects.size(); i++) {
            PathObject object = objects.get(i);
            var roi = object.getROI();
            var measurements = object.getMeasurementList();

            out.append("    {\n");
            out.append("      \"detection\": ").append(object.isDetection()).append(",\n");
            out.append("      \"class\": ").append(quote(
                    object.getPathClass() == null ? null : object.getPathClass().toString())).append(",\n");
            out.append("      \"roi\": ").append(quote(roi == null ? null : roi.getRoiName())).append(",\n");
            if (roi != null) {
                out.append("      \"points\": ").append(roi.getNumPoints()).append(",\n");
                out.append("      \"area\": ").append(number(roi.getArea())).append(",\n");
                out.append("      \"centroidX\": ").append(number(roi.getCentroidX())).append(",\n");
                out.append("      \"centroidY\": ").append(number(roi.getCentroidY())).append(",\n");
                out.append("      \"boundsX\": ").append(number(roi.getBoundsX())).append(",\n");
                out.append("      \"boundsY\": ").append(number(roi.getBoundsY())).append(",\n");
                out.append("      \"boundsWidth\": ").append(number(roi.getBoundsWidth())).append(",\n");
                out.append("      \"boundsHeight\": ").append(number(roi.getBoundsHeight())).append(",\n");
                out.append("      \"valid\": ").append(roi.getGeometry().isValid()).append(",\n");
            }
            out.append("      \"measurements\": [");
            var names = measurements.getNames();
            for (int m = 0; m < names.size(); m++) {
                if (m > 0) {
                    out.append(", ");
                }
                out.append("{\"name\": ").append(quote(names.get(m)))
                   .append(", \"value\": ").append(number(measurements.get(names.get(m)))).append("}");
            }
            out.append("]\n    }");
            if (i + 1 < objects.size()) {
                out.append(",");
            }
            out.append("\n");
        }
        out.append("  ]\n}\n");
        System.out.print(out);
    }

    /**
     * A JSON string.
     *
     * <p>Control characters are escaped, and that is not hypothetical here:
     * QuPath's own entry summary contains a <b>tab</b> — {@code "ID:\t1"} —
     * and a raw one inside a string makes the fixture something a JSON parser
     * refuses, however well the tests that read it by hand cope.
     */
    private static String quote(String text) {
        if (text == null) {
            return "null";
        }
        var out = new StringBuilder("\"");
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            switch (c) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                case '\n' -> out.append("\\n");
                case '\r' -> out.append("\\r");
                case '\t' -> out.append("\\t");
                default -> {
                    if (c < 0x20) {
                        out.append(String.format("\\u%04x", (int) c));
                    } else {
                        out.append(c);
                    }
                }
            }
        }
        return out.append('"').toString();
    }

    /**
     * A number JSON can hold, at every digit QuPath holds.
     *
     * <p>{@link Double#toString} and not {@code String.format("%.6f", ...)}.
     * The first version rounded to six decimals, which would have recorded a
     * measurement's precision as agreeing whatever the writer did to it — and
     * a measurement being quietly rounded is exactly the bug this recorder
     * found. A fixture must not be formatted at a precision coarser than the
     * thing it is pinning.
     *
     * <p>{@code NaN} is not JSON, and it is reachable here: a measurement QuPath
     * did not find is {@code Double.NaN} rather than an absence. Recorded as the
     * string so the Rust side can tell "no such measurement" from a value.
     */
    private static String number(double value) {
        if (Double.isNaN(value)) {
            return "\"NaN\"";
        }
        if (Double.isInfinite(value)) {
            return value > 0 ? "\"Infinity\"" : "\"-Infinity\"";
        }
        return Double.toString(value);
    }
}
