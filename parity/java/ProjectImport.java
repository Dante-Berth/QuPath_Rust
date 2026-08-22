import qupath.lib.projects.Project;
import qupath.lib.projects.ProjectIO;
import qupath.lib.projects.ProjectImageEntry;

import java.awt.image.BufferedImage;
import java.io.File;
import java.net.URI;
import java.nio.file.Files;
import java.nio.file.Path;

/**
 * Records what QuPath's own reader makes of a project directory <em>this
 * project wrote</em>.
 *
 * <p>The companion of {@code GeoJsonImport}, for the other half of
 * {@code docs/VIEWER.md} §8.8. The entry point is
 * {@link ProjectIO#loadProject(File, Class)}, which is what <i>Open project</i>
 * in the GUI reaches.
 *
 * <p><b>Nothing here opens an image.</b> That would need
 * {@code qupath-extension-openslide} on the classpath and a working native
 * library, and it would be testing OpenSlide rather than the format. What is
 * recorded is everything QuPath can tell about an entry <em>before</em> the
 * pixels: the entry list, the names, the URIs it resolved and whether they
 * point at a file that exists, and the metadata block — which is where a
 * width, a height and a pixel size go in and come back out.
 *
 * <p>{@code data.qpdata} is deliberately absent — Java serialisation, and a
 * guessed one is how a project becomes corrupt in a way somebody discovers much
 * later. {@code thumbnail.jpg} is now written, by {@code qupath-app}'s
 * {@code project_thumbnails} example, so {@code thumbnailReadable} records
 * something it never could before: whether {@code ImageIO.read} accepts a JPEG
 * that workspace encoded. It reads {@code true} for the slide whose pixels can
 * be decoded and {@code false} for the JPEG 2000 one, which has no thumbnail at
 * all — so one recording carries both the format being accepted and the absence
 * still being tolerated. {@code hasImageData} keeps the latter role for
 * {@code data.qpdata}.
 *
 * <p>Paths are <b>not</b> recorded, only file names and booleans. A project's
 * URIs are absolute, so recording them would put the machine that ran the
 * recorder into a committed fixture and make it disagree everywhere else.
 */
public class ProjectImport {

    public static void main(String[] args) throws Exception {
        File file = new File(args.length > 0 ? args[0] : "study/project.qpproj");

        // Load ImageServers before Gson is asked for a ServerBuilder.
        //
        // The subtype factory that maps `"builderType": "uri"` onto
        // DefaultImageServerBuilder is registered in a *static initializer* of
        // ImageServers, so in a JVM that has never touched that class Gson has
        // no way to instantiate the interface and the project fails to load
        // with "Interfaces can't be instantiated!". That is this recorder being
        // barer than QuPath rather than anything about the file: the GUI has
        // loaded ImageServers long before anyone opens a project. Naming the
        // class here is what makes the recorder resemble the caller it stands
        // in for.
        Class.forName("qupath.lib.images.servers.ImageServers");

        Project<BufferedImage> project = ProjectIO.loadProject(file, BufferedImage.class);

        var out = new StringBuilder();
        out.append("{\n");
        out.append("  \"version\": ").append(quote(project.getVersion())).append(",\n");
        out.append("  \"name\": ").append(quote(project.getName())).append(",\n");
        // The project's own URI is absolute; only its last element is stable.
        out.append("  \"uriFileName\": ").append(quote(Path.of(project.getURI()).getFileName().toString())).append(",\n");
        out.append("  \"movedFromPreviousUri\": ").append(project.getPreviousURI() != null
                && !project.getPreviousURI().equals(project.getURI())).append(",\n");
        out.append("  \"entries\": [\n");

        var images = project.getImageList();
        for (int i = 0; i < images.size(); i++) {
            ProjectImageEntry<BufferedImage> entry = images.get(i);
            out.append("    {\n");
            out.append("      \"id\": ").append(quote(entry.getID())).append(",\n");
            out.append("      \"imageName\": ").append(quote(entry.getImageName())).append(",\n");
            out.append("      \"entryDirectory\": ").append(
                    quote(entry.getEntryPath().getFileName().toString())).append(",\n");
            out.append("      \"hasImageData\": ").append(entry.hasImageData()).append(",\n");

            // The entry's own metadata map -- `DefaultProjectImageEntry
            // .metadata`, which the browser's right-click edits -- and *not*
            // the server metadata block below, which describes the pixels.
            // The two are different fields at different depths with the same
            // name; recording both is what keeps a reader honest about which
            // one it wrote. In the file's order, because Gson serialises a
            // LinkedHashMap and the order is the order somebody typed them.
            var entryMetadata = entry.getMetadata();
            out.append("      \"metadataKeys\": [");
            var keys = new java.util.ArrayList<>(entryMetadata.keySet());
            for (int k = 0; k < keys.size(); k++) {
                if (k > 0) out.append(", ");
                out.append(quote(keys.get(k)));
            }
            out.append("],\n");
            out.append("      \"metadataValues\": [");
            for (int k = 0; k < keys.size(); k++) {
                if (k > 0) out.append(", ");
                out.append(quote(entryMetadata.get(keys.get(k))));
            }
            out.append("],\n");
            out.append("      \"thumbnailReadable\": ").append(readableThumbnail(entry)).append(",\n");

            var uris = entry.getURIs();
            out.append("      \"uris\": ").append(uris.size()).append(",\n");
            URI uri = uris.isEmpty() ? null : uris.iterator().next();
            out.append("      \"slideFileName\": ").append(
                    quote(uri == null ? null : Path.of(uri).getFileName().toString())).append(",\n");
            out.append("      \"slideExists\": ").append(
                    uri != null && Files.exists(Path.of(uri))).append(",\n");

            var builder = entry.getServerBuilder();
            out.append("      \"builderClass\": ").append(
                    quote(builder == null ? null : builder.getClass().getSimpleName())).append(",\n");
            var metadata = builder == null ? java.util.Optional.<qupath.lib.images.servers.ImageServerMetadata>empty()
                    : builder.getMetadata();
            if (metadata.isPresent()) {
                var m = metadata.get();
                out.append("      \"width\": ").append(m.getWidth()).append(",\n");
                out.append("      \"height\": ").append(m.getHeight()).append(",\n");
                out.append("      \"sizeZ\": ").append(m.getSizeZ()).append(",\n");
                out.append("      \"sizeT\": ").append(m.getSizeT()).append(",\n");
                out.append("      \"channels\": ").append(m.getSizeC()).append(",\n");
                out.append("      \"rgb\": ").append(m.isRGB()).append(",\n");
                // `getPixelCalibration` returns the field, and Gson leaves it
                // null when the JSON has no `pixelCalibration` — which is the
                // second entry, written with no pixel size on purpose. Recorded
                // as three distinct states, because "absent" and "present but
                // not in microns" are different things to QuPath and only one
                // of them is what an unknown pixel size should look like.
                var pixels = m.getPixelCalibration();
                out.append("      \"pixelCalibration\": ").append(pixels == null ? "null" : "\"present\"").append(",\n");
                boolean known = pixels != null && pixels.hasPixelSizeMicrons();
                out.append("      \"pixelSizeKnown\": ").append(known).append(",\n");
                out.append("      \"pixelWidthMicrons\": ").append(
                        known ? Double.toString(pixels.getPixelWidthMicrons()) : "null").append(",\n");
                out.append("      \"pixelHeightMicrons\": ").append(
                        known ? Double.toString(pixels.getPixelHeightMicrons()) : "null").append(",\n");

                // Whether `setMetadata` could apply this block at all, which is
                // what happens when the image is opened.
                // `AbstractImageServer.setMetadata` calls
                // `metadata.getLevels()`, and `getLevels()` is
                // `Arrays.asList(levels)` over a field Gson leaves null when the
                // JSON carries no `levels`. Recorded rather than reasoned about:
                // it is the difference between a project that opens and an image
                // that opens.
                String levels;
                try {
                    levels = Integer.toString(m.getLevels().size());
                } catch (Exception | Error e) {
                    levels = quote(e.getClass().getSimpleName());
                }
                out.append("      \"levels\": ").append(levels).append(",\n");
            } else {
                out.append("      \"metadata\": null,\n");
            }
            out.append("      \"summary\": ").append(quote(oneLine(entry.getSummary()))).append("\n");
            out.append("    }");
            if (i + 1 < images.size()) {
                out.append(",");
            }
            out.append("\n");
        }
        out.append("  ]\n}\n");
        System.out.print(out);
    }

    /**
     * Whether a thumbnail could be read, without failing when there is none.
     *
     * <p>{@code project.rs} leaves {@code thumbnail.jpg} to its caller and
     * nothing writes one, so this is expected to be false. It is recorded
     * rather than assumed because "QuPath tolerates the absence" is the claim
     * that matters, and an exception here would be QuPath not tolerating it.
     */
    private static boolean readableThumbnail(ProjectImageEntry<BufferedImage> entry) {
        try {
            return entry.getThumbnail() != null;
        } catch (Exception e) {
            return false;
        }
    }

    /** The summary string, flattened — QuPath puts newlines in it. */
    private static String oneLine(String text) {
        return text == null ? null : text.replace("\n", " | ").replace("\r", "").trim();
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
}
