import qupath.lib.images.ImageData;
import qupath.lib.images.servers.ImageServer;
import qupath.lib.projects.Project;
import qupath.lib.projects.ProjectIO;
import qupath.lib.projects.ProjectImageEntry;

import java.awt.image.BufferedImage;
import java.io.File;
import java.nio.file.Path;

/**
 * Opens the <em>pixels</em> of every entry in a project this project wrote.
 *
 * <p>{@link ProjectImport} deliberately stops before this: it records everything
 * QuPath can tell about an entry before the image is opened, and opening one
 * needs {@code qupath-extension-openslide} on the classpath. That was recorded
 * in {@code docs/VIEWER.md} §8.8 as blocked on "a native library, and there is
 * none on this machine" — which was true of {@code ldconfig} and false of the
 * thing QuPath loads. The extension's own dependency carries a
 * {@code libopenslide.so} inside a jar and JNA extracts it, so nothing needs
 * installing. This recorder is that entry, unblocked.
 *
 * <p><b>What it is for.</b> The claim being tested is that a project written
 * here is one QuPath can actually <i>work in</i>, not merely one it can list.
 * That is a different claim, and it fails differently: a project whose entries
 * all read correctly can still have a {@code ServerBuilder} that cannot build,
 * a URI that resolves to the wrong file, or — the case this whole area circles
 * — a stored metadata block that {@code setMetadata} applies over what OpenSlide
 * read, so that the image opens and every pixel after that is the wrong one.
 *
 * <p>So the recorded fields are the ones that would expose exactly that: the
 * server's class, the dimensions and level count it ended up with, its tile
 * size and pixel size, and a block of pixels actually read from level 0. The
 * pixels are the part that cannot be faked by metadata agreeing with itself —
 * {@code parity_project_open.rs} checks them against this project's own reader
 * on the same file, which is the one comparison that fails when the level table
 * is plausible and wrong.
 *
 * <p><b>Where the pixels are read matters, twice.</b> Not the centre of the
 * slide, and not its origin: both are background on these files, and two readers
 * both returning white would agree without either of them being right —
 * {@code docs/VIEWER.md} §6 records that mistake being made three times with
 * three different instruments. The region is the dense-tissue one
 * {@code SlideRegion} already profiles against, and the recorder asserts it did
 * not come back uniform, so a fixture recorded over background fails here rather
 * than passing everywhere.
 *
 * <p>The second is the codec. {@code CMU-1-JP2K-33005.svs} is Aperio JPEG 2000,
 * and this project decodes baseline JPEG only ({@code read_tile_into} returns
 * {@code UndecodableCodec}); §3 has why that is deliberate. Its metadata is
 * still worth recording and still compared, so what is recorded per entry is
 * {@code pixelsComparable} — false for JP2K — and the Rust side skips the pixel
 * assertion for those entries rather than pretending the comparison happened.
 *
 * <p>{@code readImageData} is the entry point rather than
 * {@code ImageServerProvider.buildServer}, because it is what <i>double-clicking
 * a row</i> reaches: it goes through {@code getServerBuilder()}, which is where a
 * stored block would be applied.
 */
public class ProjectOpen {

    /**
     * Where the tissue is, per slide, and the size of the block read there.
     *
     * <p>Per slide because clamping one coordinate into a smaller image does
     * not find tissue — it finds the corner. The first version of this recorder
     * clamped {@code SlideRegion}'s profiled region into the 2220x2967 small
     * region and landed on (2212, 2959), which is its bottom-right corner and
     * is background: mean 243 against tissue's 99. It passed the uniformity
     * check because background is not perfectly uniform, which is why that check
     * is now a spread rather than an equality.
     *
     * <p>CMU-1-JP2K's is the dense region {@code SlideRegion} already profiles
     * against; the small region's was found by scanning its tiles for the
     * darkest.
     */
    private static final int BLOCK = 8;

    private static int[] tissue(String slideFileName) {
        if (slideFileName != null && slideFileName.startsWith("CMU-1-Small-Region")) {
            return new int[] {1320, 1080};
        }
        return new int[] {27776, 5952};
    }

    /**
     * The least spread a block must show to be worth comparing.
     *
     * <p>Background is not flat — JPEG leaves it varying by a few levels — so
     * "not uniform" is satisfied by a block that carries no information. Tissue
     * on these slides spans well over a hundred levels; background spans single
     * digits.
     */
    private static final int MIN_SPREAD = 24;

    public static void main(String[] args) throws Exception {
        File file = new File(args.length > 0 ? args[0] : "study/project.qpproj");

        // See ProjectImport for why this is named: the subtype factory that maps
        // "builderType" onto a builder is registered in a static initializer, so
        // in a JVM that has never touched the class Gson cannot instantiate the
        // interface and the project fails to load.
        Class.forName("qupath.lib.images.servers.ImageServers");

        Project<BufferedImage> project = ProjectIO.loadProject(file, BufferedImage.class);

        var out = new StringBuilder();
        out.append("{\n");
        out.append("  \"entries\": [\n");

        var images = project.getImageList();
        for (int i = 0; i < images.size(); i++) {
            ProjectImageEntry<BufferedImage> entry = images.get(i);
            out.append("    {\n");
            out.append("      \"imageName\": ").append(quote(entry.getImageName())).append(",\n");
            out.append("      \"slideFileName\": ").append(quote(slideFileName(entry))).append(",\n");

            // Every failure mode here is a real answer about the format, so
            // none of them may take the recorder down: an entry that cannot be
            // opened is recorded as the exception it threw, and the fixture
            // still lists the entries after it.
            try (ImageData<BufferedImage> data = entry.readImageData()) {
                ImageServer<BufferedImage> server = data.getServer();
                out.append("      \"opened\": true,\n");
                out.append("      \"serverClass\": ")
                        .append(quote(server.getClass().getSimpleName())).append(",\n");
                out.append("      \"width\": ").append(server.getWidth()).append(",\n");
                out.append("      \"height\": ").append(server.getHeight()).append(",\n");
                out.append("      \"levelCount\": ").append(server.nResolutions()).append(",\n");
                out.append("      \"rgb\": ").append(server.isRGB()).append(",\n");

                var m = server.getMetadata();
                out.append("      \"preferredTileWidth\": ").append(m.getPreferredTileWidth()).append(",\n");
                out.append("      \"preferredTileHeight\": ").append(m.getPreferredTileHeight()).append(",\n");

                var pixels = m.getPixelCalibration();
                boolean known = pixels != null && pixels.hasPixelSizeMicrons();
                out.append("      \"pixelSizeKnown\": ").append(known).append(",\n");
                out.append("      \"pixelWidthMicrons\": ").append(
                        known ? Double.toString(pixels.getPixelWidthMicrons()) : "null").append(",\n");

                // Whether this project's reader can decode the tiles at all.
                // Aperio JPEG 2000 it cannot, on purpose (§3), so the pixel
                // block below is recorded but not compared -- said in the
                // fixture rather than left for the Rust side to infer from a
                // file name.
                boolean comparable = !"CMU-1-JP2K-33005.svs".equals(slideFileName(entry));
                out.append("      \"pixelsComparable\": ").append(comparable).append(",\n");

                // A block of pixels from tissue, not from the middle of the
                // slide and not from its origin: both are background here, and
                // two readers both returning white agree without either being
                // right. See the class comment.
                int[] at = tissue(slideFileName(entry));
                int x = at[0];
                int y = at[1];
                out.append("      \"pixelX\": ").append(x).append(",\n");
                out.append("      \"pixelY\": ").append(y).append(",\n");
                out.append("      \"pixelBlock\": ").append(BLOCK).append(",\n");

                BufferedImage tile = server.readRegion(1.0, x, y, BLOCK, BLOCK);
                var block = new StringBuilder();
                int lowest = 255;
                int highest = 0;
                for (int py = 0; py < BLOCK; py++) {
                    for (int px = 0; px < BLOCK; px++) {
                        int rgb = tile.getRGB(px, py) & 0xffffff;
                        for (int shift : new int[] {16, 8, 0}) {
                            int value = (rgb >> shift) & 0xff;
                            lowest = Math.min(lowest, value);
                            highest = Math.max(highest, value);
                        }
                        if (block.length() > 0) {
                            block.append(", ");
                        }
                        block.append((rgb >> 16) & 0xff).append(", ")
                                .append((rgb >> 8) & 0xff).append(", ")
                                .append(rgb & 0xff);
                    }
                }

                // A flat block is the failure this placement exists to avoid,
                // and it must not be committed as a fixture that passes.
                if (highest - lowest < MIN_SPREAD) {
                    throw new IllegalStateException(
                            "the recorded block spans only " + (highest - lowest)
                                    + " levels at " + x + "," + y
                                    + " -- it is background, and would compare equal to anything");
                }

                out.append("      \"pixels\": [").append(block).append("]\n");
            } catch (Exception | Error e) {
                out.append("      \"opened\": false,\n");
                out.append("      \"error\": ").append(quote(e.getClass().getSimpleName())).append(",\n");
                out.append("      \"errorMessage\": ").append(quote(oneLine(e.getMessage()))).append("\n");
            }

            out.append("    }");
            if (i + 1 < images.size()) {
                out.append(",");
            }
            out.append("\n");
        }
        out.append("  ]\n}\n");
        System.out.print(out);
    }

    /** The slide's file name, or null — paths are never recorded, see ProjectImport. */
    private static String slideFileName(ProjectImageEntry<BufferedImage> entry)
            throws java.io.IOException {
        var uris = entry.getURIs();
        if (uris.isEmpty()) {
            return null;
        }
        return Path.of(uris.iterator().next()).getFileName().toString();
    }

    /** A message, flattened — an exception's can carry newlines and a path. */
    private static String oneLine(String text) {
        return text == null ? null : text.replace("\n", " | ").replace("\r", "").trim();
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
