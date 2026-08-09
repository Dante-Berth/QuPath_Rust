import java.awt.image.BufferedImage;
import java.nio.file.Path;
import qupath.lib.images.servers.ImageServer;
import qupath.lib.images.servers.ImageServerProvider;
import qupath.lib.regions.RegionRequest;

/**
 * Records pixels QuPath itself reads from a slide, so that the pure-Rust SVS
 * reader is checked against QuPath's decode rather than against our reading of
 * the TIFF specification.
 *
 * This matters more than the other recorders. A wrong kernel shape produces
 * visibly wrong output; a wrong JPEG colour conversion produces output that
 * looks entirely plausible and is quietly biased, which would then propagate
 * into every measurement downstream.
 *
 * Regions are small and deliberately placed: one crossing a tile boundary in
 * both axes, one interior to a single tile, and one at the slide's far edge
 * where tiles are padded.
 */
public class SlideRegion {

    public static void main(String[] args) throws Exception {
        var path = Path.of(args.length > 0 ? args[0] : "data/CMU-1.svs");

        var out = new StringBuilder();
        out.append("{\n  \"slide\": \"").append(path.getFileName()).append("\",\n");
        out.append("  \"cases\": [\n");

        try (ImageServer<BufferedImage> server =
                ImageServerProvider.buildServer(path.toUri().toString(), BufferedImage.class)) {

            out.append("    {\"name\": \"metadata\"")
                    .append(", \"width\": ").append(server.getWidth())
                    .append(", \"height\": ").append(server.getHeight())
                    .append(", \"levels\": ").append(server.nResolutions())
                    .append("},\n");

            // Interior of a single 256x256 tile.
            emit(out, server, "tile_interior", 0, 512, 512, 32, 32, false);
            // Straddles a tile boundary in x and y, which is where a stitching
            // bug would show.
            emit(out, server, "tile_seam", 0, 240, 240, 32, 32, false);
            // Inside the dense-tissue region the JFR profile used.
            emit(out, server, "tissue", 0, 27776, 5952, 32, 32, false);
            // Downsampled level, where QuPath reads a different IFD.
            emit(out, server, "level_1", 1, 1000, 1000, 32, 32, false);
            // Last case closes the array.
            emit(out, server, "level_2", 2, 500, 500, 16, 16, true);
        }

        out.append("  ]\n}\n");
        System.out.print(out);
        System.out.flush();
        System.exit(0);
    }

    private static void emit(StringBuilder out, ImageServer<BufferedImage> server, String name,
            int level, int x, int y, int width, int height, boolean last) throws Exception {

        double downsample = server.getDownsampleForResolution(level);
        // RegionRequest coordinates are always in level-0 space; the downsample
        // selects the level. Multiplying here means x,y above read as level
        // coordinates, which is what the Rust side is given.
        var request = RegionRequest.createInstance(server.getPath(), downsample,
                (int) Math.round(x * downsample), (int) Math.round(y * downsample),
                (int) Math.round(width * downsample), (int) Math.round(height * downsample));

        BufferedImage image = server.readRegion(request);

        out.append("    {\"name\": \"").append(name).append("\"")
                .append(", \"level\": ").append(level)
                .append(", \"x\": ").append(x)
                .append(", \"y\": ").append(y)
                .append(", \"width\": ").append(image.getWidth())
                .append(", \"height\": ").append(image.getHeight())
                .append(", \"rgb\": [");

        for (int row = 0; row < image.getHeight(); row++) {
            for (int column = 0; column < image.getWidth(); column++) {
                int packed = image.getRGB(column, row);
                if (row > 0 || column > 0) {
                    out.append(",");
                }
                out.append((packed >> 16) & 0xFF).append(",")
                        .append((packed >> 8) & 0xFF).append(",")
                        .append(packed & 0xFF);
            }
        }

        out.append("]}").append(last ? "\n" : ",\n");
    }
}
