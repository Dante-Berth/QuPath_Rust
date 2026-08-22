/*
 * Records the order QuPath's own smart sort puts a list of names in.
 *
 * `ProjectBrowser` sorts its groups with `GeneralTools.smartStringSort`, so
 * that is the row order a project browser has to reproduce. The comparison is
 * not a string comparison: each name is split on `(\d+)` into digit runs and
 * the text between them, the runs are parsed to `BigDecimal` and compared by
 * value, and the last tiebreak is the number of parts.
 *
 * The names below are chosen for the parts of that a reader gets wrong rather
 * than for what a study looks like:
 *
 *  - `a2` before `a10`, which is the whole point of the sort;
 *  - `007` and `7`, which BigDecimal compares *equal* — scale is ignored, so
 *    the tie falls through to the part count;
 *  - `Block` against `abc`, because String.compareTo is case-sensitive where
 *    the search box in the same pane is not;
 *  - a digit run against a text part, which Java compares by `toString()` —
 *    and `new BigDecimal("007").toString()` is `7`, not `007`;
 *  - `a1` against `a1bbbbbbbb`, where the shorter name wins on part count and
 *    not on length, and `a1b1`, which loses to both on the same rule;
 *  - a run longer than a `long`, which BigDecimal takes and a parse to an
 *    integer type would not;
 *  - a non-ASCII name, because Java compares UTF-16 code units and a port
 *    comparing UTF-8 bytes agrees only up to U+FFFF.
 *
 * Emitted as the sorted list rather than as pairwise comparisons: the sort is
 * what the browser calls, and a total order recorded as a list is checkable
 * without also porting Java's sort.
 */
import qupath.lib.common.GeneralTools;

import java.util.ArrayList;
import java.util.List;

public class SmartStringSort {

    /** Deliberately not in any sensible order: the recording is the order. */
    private static final String[] NAMES = {
        "a10", "a2", "a1",
        "slide-007", "slide-7", "slide-70",
        "Block", "abc", "Abc", "block",
        "a1b1", "a1bbbbbbbb",
        "CMU-1", "CMU-1-Small-Region", "CMU-1-JP2K-33005",
        "12345678901234567890123", "12345678901234567891",
        "0", "000", "00a", "0a",
        "", " ", "  ", "9", "x",
        "éclair", "Ａ", "😀", "",
        "scan 2 of 10", "scan 10 of 10", "scan 1 of 10",
    };

    public static void main(String[] args) {
        List<String> sorted = new ArrayList<>(List.of(NAMES));
        GeneralTools.smartStringSort(sorted);

        StringBuilder out = new StringBuilder();
        out.append("{\n");
        out.append("  \"source\": \"qupath-core GeneralTools.smartStringSort\",\n");
        out.append("  \"note\": \"the order a ProjectBrowser group list is put in\",\n");
        out.append("  \"input\": [\n");
        appendNames(out, List.of(NAMES));
        out.append("\n  ],\n");
        out.append("  \"sorted\": [\n");
        appendNames(out, sorted);
        out.append("\n  ]\n}\n");

        // The property the fixture exists for: the recorded order really is a
        // total order under the comparator, so a port can be checked against
        // the list rather than against every pair.
        var comparator = GeneralTools.smartStringComparator();
        for (int i = 1; i < sorted.size(); i++) {
            if (comparator.compare(sorted.get(i - 1), sorted.get(i)) > 0) {
                throw new AssertionError(
                    "sorted list disagrees with the comparator at " + i);
            }
        }

        System.out.print(out);
    }

    /** Names as a JSON array body, escaped so a non-ASCII one survives. */
    private static void appendNames(StringBuilder out, List<String> names) {
        for (int i = 0; i < names.size(); i++) {
            if (i > 0) out.append(",\n");
            out.append("    ").append(escape(names.get(i)));
        }
    }

    /**
     * A JSON string, with every non-ASCII character as a `\\u` escape.
     *
     * Escaped rather than written out, because the fixture is compared byte for
     * byte and a lone surrogate is not something a UTF-8 file can hold.
     */
    private static String escape(String text) {
        StringBuilder out = new StringBuilder("\"");
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c == '"' || c == '\\') {
                out.append('\\').append(c);
            } else if (c >= 0x20 && c < 0x7f) {
                out.append(c);
            } else {
                out.append(String.format("\\u%04x", (int) c));
            }
        }
        return out.append('"').toString();
    }
}
