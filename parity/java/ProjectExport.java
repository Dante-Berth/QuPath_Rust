import qupath.lib.images.servers.ImageServerBuilder.DefaultImageServerBuilder;
import qupath.lib.images.servers.ImageServerBuilder.ServerBuilder;
import qupath.lib.images.servers.ImageServerMetadata;
import qupath.lib.images.servers.JsonImageServerBuilder;
import qupath.lib.images.servers.PixelType;
import qupath.lib.projects.Project;
import qupath.lib.projects.ProjectIO;

import java.awt.image.BufferedImage;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

/**
 * Records a {@code project.qpproj} <em>QuPath wrote</em>, for this project's
 * reader to be checked against.
 *
 * <p>{@code ProjectImport} runs one way — QuPath reading a file this project
 * wrote — and that direction cannot see the other one. A browser opens projects
 * QuPath wrote, and the format they arrive in is Gson's spelling of
 * {@code DefaultProject}, not this project's spelling of the same fields. The
 * two differ, and only a file from QuPath shows how.
 *
 * <p>The project is built in two halves, because a browser meets both:
 *
 * <ol>
 *   <li>the entries of a project <b>this</b> project wrote, loaded with
 *       {@link ProjectIO#loadProject(File, Class)} and written back out by
 *       {@code syncChanges()} — QuPath's hand, this project's values;</li>
 *   <li>one entry added here with a <b>full metadata block</b>, which is what
 *       an entry looks like once QuPath has opened the image. That block is
 *       the shape {@code project.rs} refuses to write and cannot know — a
 *       level table, preferred tile sizes, {@code isRGB}, channels as objects
 *       and a {@code pixelCalibration} whose unit is the micron sign — and
 *       recording it is what turns "this crate cannot know the pyramid" from a
 *       missing specification into a written one.</li>
 * </ol>
 *
 * <p>The block is also why a reader here has to parse rather than search for a
 * key: it carries a {@code width} and a {@code height} of its own, and a
 * {@code levels} array of more of them, and {@code channels} with a
 * {@code name} each. The project's own {@code uri} — the {@code .qpproj} file,
 * not a slide — precedes every entry's in the file, so the <em>first</em>
 * {@code "uri"} in a project is never one that names an image.
 *
 * <p>What this does <b>not</b> record is an entry built by one of the sixteen
 * builders that wrap another builder — {@code rotated}, {@code cropped},
 * {@code sparse} and the rest. They are package-private in
 * {@code ImageServers}, so constructing one needs a helper inside that package,
 * and what a reader owes such an entry — a row that is listed and says it
 * cannot be opened — is covered by unit tests rather than by a file QuPath
 * wrote.
 *
 * <p>The provider class for the added entry is {@link JsonImageServerBuilder}
 * rather than OpenSlide's, because it is the one concrete
 * {@code ImageServerBuilder} on {@code qupath-core}'s classpath and
 * {@code DefaultImageServerBuilder.createInstance} takes a class rather than a
 * name. Nothing here opens an image, so which provider is named does not change
 * what is written; the field is recorded and the reader ignores it, exactly as
 * it ignores the one this project writes.
 *
 * <p><b>Its name is left unset on purpose</b>, and the answer was not the one
 * the source reads like. {@code DefaultProjectImageEntry}'s constructor does
 * {@code this.imageName = "Image " + entryID} — but {@code entryID} there is
 * the {@code Long} <em>parameter</em>, which is null on this path, not the
 * field it has just assigned from the counter. So an unnamed entry is written
 * as {@code "Image null"}, three lines below the {@code "entryID": 3} that was
 * meant to be in it. Reading the source gives {@code "Image 3"}; running it
 * gives what is in the fixture.
 *
 * <p>The reader on the other side applies no fallback at all —
 * {@code getImageName()} returns the stored field — so an entry whose JSON has
 * no {@code imageName} is a blank row in QuPath's own browser. Both facts are
 * worth pinning, because a reader elsewhere has to choose what to show and
 * neither choice is obvious from the code.
 *
 * <p>What is printed is the file itself, so the fixture is the artefact rather
 * than a description of it. {@code record-project.sh} replaces the two absolute
 * paths in it with placeholders and checks that nothing of the recording
 * machine survives; every project path is an absolute {@code file:} URI, which
 * is the same reason the input to {@code ProjectImport} cannot be committed.
 */
public class ProjectExport {

    /**
     * The pixel size the added entry claims, in microns.
     *
     * <p>CMU-1's own, so the number in the fixture is one this repository
     * already recognises. The unit QuPath compares against is U+00B5 MICRO
     * SIGN, not U+03BC GREEK SMALL LETTER MU — identical in most fonts, and
     * one of the three fields that were silently wrong when this project tried
     * to write a metadata block of its own.
     */
    private static final double PIXEL_SIZE_MICRONS = 0.2498;

    public static void main(String[] args) throws Exception {
        File file = new File(args.length > 0 ? args[0] : "study/project.qpproj");
        File slide = new File(args.length > 1 ? args[1] : "data/CMU-1.svs");

        // See ProjectImport: the subtype factory mapping `"builderType": "uri"`
        // onto DefaultImageServerBuilder is registered in a static initializer
        // of ImageServers, and a JVM that has never touched that class cannot
        // instantiate the interface.
        Class.forName("qupath.lib.images.servers.ImageServers");

        Project<BufferedImage> project = ProjectIO.loadProject(file, BufferedImage.class);
        project.addImage(withMetadata(slide));
        project.syncChanges();

        System.out.print(Files.readString(file.toPath(), StandardCharsets.UTF_8));
    }

    /**
     * A builder carrying the metadata block an opened image would have.
     *
     * <p>Three levels rather than one, because one level says the image has no
     * pyramid and that is the claim {@code project.rs} refused to make; the
     * downsamples are CMU-1's. The tile size is the second thing a stored block
     * needs and the second thing that crate does not know.
     */
    private static ServerBuilder<BufferedImage> withMetadata(File slide) {
        ImageServerMetadata metadata = new ImageServerMetadata.Builder()
                .width(46000)
                .height(32914)
                .levelsFromDownsamples(1.0, 4.0, 16.0)
                .preferredTileSize(256, 256)
                .rgb(true)
                .pixelType(PixelType.UINT8)
                .pixelSizeMicrons(PIXEL_SIZE_MICRONS, PIXEL_SIZE_MICRONS)
                .build();
        return DefaultImageServerBuilder.createInstance(
                JsonImageServerBuilder.class, metadata, slide.toURI());
    }
}
