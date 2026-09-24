import qupath.lib.images.servers.ImageServer;
import qupath.lib.images.servers.ImageServerMetadata;
import qupath.lib.images.servers.openslide.OpenslideServerBuilder;
import qupath.lib.images.servers.openslide.jna.OpenSlideLoader;

import java.awt.image.BufferedImage;
import java.nio.file.Path;

/**
 * Records the pyramid QuPath gets from OpenSlide, so that the metadata block
 * {@code docs/dev/VIEWER.md} §8.8 asks about can be compared against it rather than
 * reasoned about.
 *
 * <p>The entry this closes is not "write the block". A stored metadata block
 * <em>replaces</em> what QuPath would otherwise have read:
 * {@code AbstractServerBuilder.build} calls {@code setMetadata} with it when the
 * image is opened, so a level table that disagrees with OpenSlide's is not a
 * worse description of the slide — it is QuPath reading the wrong pixels,
 * silently, for as long as the project exists. {@code Slide::open} keeps the
 * tiled directories and drops the label, macro and thumbnail; OpenSlide's Aperio
 * driver makes its own choice, and until this recorder existed <b>the two had
 * never been compared</b>.
 *
 * <p><b>This records the server, not the native call.</b> The obvious recorder
 * — {@code openslide_get_level_downsample} for each level — would answer a
 * question QuPath never asks. {@code OpenslideImageServer} explicitly declines
 * that value, because OpenSlide averages the width and height ratios and so
 * returns something inexact whenever the true downsample is an integer, and
 * rebuilds the table from each level's dimensions through
 * {@code ImageResolutionLevel.Builder}. What reaches a project, and what
 * {@code setMetadata} would overwrite, is the rebuilt table — so that is what is
 * printed. The comparison is against {@code Slide::levels}, which computes its
 * downsample the same way, from the widths.
 *
 * <p><b>The native library comes from the jar, not the system.</b>
 * {@code OpenSlideLoader} calls {@code Native.extractFromResourcePath}, so
 * {@code io.github.qupath:openslide:<version>:<platform>} carries its own
 * {@code libopenslide.so} and no system package is involved. Worth saying
 * because this entry spent a long time recorded as blocked on "a native library,
 * and there is none on this machine" — which was true of {@code ldconfig} and
 * false of the thing QuPath actually loads.
 *
 * <p>Downsamples are printed with {@code Double.toString} and not rounded:
 * whether a level's downsample is exactly 4 or 4.0000598 is the substance of the
 * comparison, and a recorder that rounds it away answers the question it was
 * written to ask.
 */
public class OpenSlideLevels {

    public static void main(String[] args) throws Exception {
        var path = Path.of(args.length > 0 ? args[0] : "data/CMU-1.svs");

        // Fail loudly rather than recording a fixture that says "no levels".
        // A recorder that cannot load its library must not be mistaken for a
        // slide that has no pyramid.
        if (!OpenSlideLoader.tryToLoad()) {
            throw new IllegalStateException("OpenSlide could not be loaded");
        }

        var out = new StringBuilder();
        out.append("{\n");
        out.append("  \"slide\": ").append(quote(path.getFileName().toString())).append(",\n");
        out.append("  \"libraryVersion\": ").append(quote(OpenSlideLoader.getLibraryVersion())).append(",\n");
        out.append("  \"vendor\": ").append(quote(OpenSlideLoader.detectVendor(path.toString()))).append(",\n");

        // The builder the extension registers, rather than
        // ImageServerProvider.buildServer: asking the provider would let
        // whichever other reader is on the classpath answer, and then the
        // fixture would silently stop being about OpenSlide.
        var builder = new OpenslideServerBuilder();
        try (ImageServer<BufferedImage> server = builder.buildServer(path.toUri())) {
            ImageServerMetadata m = server.getOriginalMetadata();

            out.append("  \"width\": ").append(m.getWidth()).append(",\n");
            out.append("  \"height\": ").append(m.getHeight()).append(",\n");
            out.append("  \"rgb\": ").append(m.isRGB()).append(",\n");
            out.append("  \"channels\": ").append(m.getSizeC()).append(",\n");

            // The two fields the stored block would have to carry, and the two
            // that decide which pixels get read.
            out.append("  \"preferredTileWidth\": ").append(m.getPreferredTileWidth()).append(",\n");
            out.append("  \"preferredTileHeight\": ").append(m.getPreferredTileHeight()).append(",\n");

            var pixels = m.getPixelCalibration();
            boolean known = pixels != null && pixels.hasPixelSizeMicrons();
            out.append("  \"pixelSizeKnown\": ").append(known).append(",\n");
            out.append("  \"pixelWidthMicrons\": ").append(
                    known ? Double.toString(pixels.getPixelWidthMicrons()) : "null").append(",\n");
            out.append("  \"pixelHeightMicrons\": ").append(
                    known ? Double.toString(pixels.getPixelHeightMicrons()) : "null").append(",\n");

            var levels = m.getLevels();
            out.append("  \"levelCount\": ").append(levels.size()).append(",\n");
            out.append("  \"levels\": [\n");
            for (int i = 0; i < levels.size(); i++) {
                var level = levels.get(i);
                out.append("    {")
                        .append("\"index\": ").append(i)
                        .append(", \"width\": ").append(level.getWidth())
                        .append(", \"height\": ").append(level.getHeight())
                        .append(", \"downsample\": ").append(Double.toString(level.getDownsample()))
                        .append("}");
                if (i + 1 < levels.size()) {
                    out.append(",");
                }
                out.append("\n");
            }
            out.append("  ]\n");
        }
        out.append("}\n");
        System.out.print(out);
    }

    /** A JSON string. */
    private static String quote(String text) {
        if (text == null) {
            return "null";
        }
        var out = new StringBuilder("\"");
        for (char c : text.toCharArray()) {
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
        return out.append("\"").toString();
    }
}
