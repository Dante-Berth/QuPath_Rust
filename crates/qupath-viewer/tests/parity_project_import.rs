//! Checks a project directory *this project wrote* against QuPath's own reader.
//!
//! The companion of `parity_geojson_import.rs`, for the other half of
//! `docs/dev/VIEWER.md` §8.8. `parity/java/ProjectImport.java` opens the directory
//! with `ProjectIO.loadProject` — what *Open project* reaches — and records
//! what it found; this asserts that reading is the entries the project was
//! written from.
//!
//! **The input is not committed, and cannot be.** Every path QuPath stores is
//! an absolute `file:` URI, so a committed project would name the machine that
//! recorded it. `parity/record-project.sh` writes one, records it and deletes
//! it, and what ties the fixture to this code is `examples/shared/project.rs`,
//! which both the writer and this test read the entries from.
//!
//! # What the first run found
//!
//! The project did not open at all, and then opened three more times without
//! the image being openable. In order:
//!
//! - **no `createTimestamp` or `modifyTimestamp`.** `DefaultProject.loadProject`
//!   reads both with `getAsLong()` and no `has` guard, before anything else, so
//!   their absence is a `NullPointerException` wrapped in an `IOException` whose
//!   message names Gson;
//! - **`"channels": ["Red", "Green", "Blue"]`.** An `ImageChannel` is an object,
//!   so Gson threw `Expected BEGIN_OBJECT but was STRING` and the whole project
//!   failed, not the one entry;
//! - **`"rgb"` and `"pixelWidthMicrons"` are not fields.** The names are `isRGB`
//!   and a `pixelCalibration` object, and Gson drops what it does not
//!   recognise — QuPath read `rgb: false` off a project that said `true`, and
//!   no pixel size at all;
//! - and then, with all of those corrected, `metadata.getLevels()` threw
//!   `NullPointerException` — which is what `AbstractImageServer.setMetadata`
//!   calls when the image is opened. The pyramid is the one thing this crate
//!   cannot know, so the metadata block is not written at all; QuPath uses what
//!   OpenSlide reads from the slide, which is better information anyway.
//!
//! `no_metadata_is_claimed_about_the_image` in `project.rs` is the unit half of
//! that last one. This file is the half that needed a JVM to find.

#[path = "../examples/shared/project.rs"]
mod sample;

const FIXTURE: &str = include_str!("../../../parity/fixtures/ProjectImport.json");

/// One `{ ... }` block per entry QuPath read, in file order.
fn entries(fixture: &str) -> Vec<&str> {
    fixture
        .split("\n    {\n")
        .skip(1)
        .map(|block| block.split("\n    }").next().expect("an entry block"))
        .collect()
}

/// The text of one field.
///
/// A quoted value ends at its closing quote and **not** at the first comma:
/// `"CMU-1, JP2K"` is one of the sample's names precisely because a name with a
/// comma in it is ordinary, and the first version of this helper split it in
/// half and reported a mismatch that was its own.
fn text<'a>(block: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\": ");
    let at = block
        .find(&needle)
        .unwrap_or_else(|| panic!("no {key} in {block}"))
        + needle.len();
    let rest = &block[at..];
    if let Some(body) = rest.strip_prefix('"') {
        let end = body
            .find('"')
            .expect("an unterminated string in the fixture");
        return &body[..end];
    }
    let end = rest.find([',', '\n']).unwrap_or(rest.len());
    rest[..end].trim()
}

/// The strings of one JSON array field, in order.
///
/// A second helper rather than a widening of [`text`], which stops at the first
/// closing quote — correct for a scalar and wrong for a list, where it returns
/// the first element and looks like a mismatch in the fixture rather than in
/// the reader. That is what it did here first.
fn list<'a>(block: &'a str, key: &str) -> Vec<&'a str> {
    let needle = format!("\"{key}\": [");
    let at = block
        .find(&needle)
        .unwrap_or_else(|| panic!("no {key} in {block}"))
        + needle.len();
    let body = &block[at..];
    let end = body
        .find(']')
        .expect("an unterminated array in the fixture");
    let body = &body[..end];
    if body.trim().is_empty() {
        return Vec::new();
    }
    body.split(", ")
        .map(|item| item.trim().trim_matches('"'))
        .collect()
}

/// QuPath opened the project, and found the entries that were written.
#[test]
fn qupath_reads_back_the_entries_it_was_given() {
    // A project that failed to load leaves no entries, and the record script
    // refuses to overwrite the fixture with an empty file — so an empty list
    // here means the recorder read a project with nothing in it.
    let recorded = entries(FIXTURE);
    assert_eq!(
        recorded.len(),
        2,
        "QuPath read {} entries from a project holding 2",
        recorded.len()
    );

    // The version this format claims. QuPath refuses two old ones outright and
    // dispatches on it, so a project claiming a version it does not match is
    // read by the wrong code path.
    assert!(
        FIXTURE.contains(&format!(
            "\"version\": \"{}\"",
            qupath_viewer::project::FORMAT_VERSION
        )),
        "the recorded project claims a different format version"
    );
    // `uri` is the field QuPath compares to tell a moved project from one in
    // place. The recorder writes the project to a temporary directory and reads
    // it there, so a `uri` that did not match would show up as a move.
    assert_eq!(
        text(FIXTURE, "movedFromPreviousUri"),
        "false",
        "QuPath thinks the project moved, which means `uri` is not where it is"
    );

    let names = [sample::NAME, sample::SECOND_NAME];
    let files = [sample::SLIDE, sample::SECOND_SLIDE];
    for (index, block) in recorded.iter().enumerate() {
        let position = index + 1;

        // `entryID` counts from 1 and names the directory. Two entries sharing
        // a directory is invisible in a project of one image.
        assert_eq!(text(block, "id"), position.to_string());
        assert_eq!(text(block, "entryDirectory"), position.to_string());

        assert_eq!(
            text(block, "imageName"),
            names[index],
            "the browser would show a different name"
        );

        // The URI resolved to the slide, and to a file that is there. This is
        // the failure `file_uri` exists to prevent: a bare or relative path
        // produces a well-formed URI naming another host, and the entry fails
        // to open with no hint as to why.
        assert_eq!(text(block, "uris"), "1");
        assert_eq!(
            text(block, "slideFileName"),
            std::path::Path::new(files[index])
                .file_name()
                .and_then(|name| name.to_str())
                .expect("a slide file name")
        );
        assert_eq!(
            text(block, "slideExists"),
            "true",
            "QuPath resolved the URI to a file that is not there"
        );

        // The discriminator dispatched. A `builderType` QuPath does not know
        // fails with a message about the builder rather than about the slide.
        assert_eq!(text(block, "builderClass"), "DefaultImageServerBuilder");

        // No stored metadata, which is deliberate: `project.rs` explains why a
        // block without `levels` is a NullPointerException when the image is
        // opened, and the pyramid is not something this crate knows.
        assert_eq!(
            text(block, "metadata"),
            "null",
            "a metadata block is being written again; see project::server_builder"
        );

        // The two files this format deliberately leaves out, measured rather
        // than assumed. QuPath opens the entry regardless, which is the claim.
        assert_eq!(
            text(block, "hasImageData"),
            "false",
            "data.qpdata is Java serialisation and is not attempted"
        );
        // The thumbnail is now written, and QuPath reads it — but only for
        // slides whose pixels this project can decode. `CMU-1-JP2K-33005.svs`
        // is Aperio JPEG 2000 (§3), so its entry has no thumbnail at all, and
        // that is a state QuPath handles by making the picture itself.
        //
        // Asserted per entry rather than as one value, because a single
        // recording now carries both answers and collapsing them would lose the
        // half that is evidence: `true` is `ImageIO.read` accepting a JPEG this
        // workspace encoded, which is the only thing that could confirm it.
        let expected = if text(block, "slideFileName").starts_with("CMU-1-JP2K") {
            "false"
        } else {
            "true"
        };
        assert_eq!(
            text(block, "thumbnailReadable"),
            expected,
            "thumbnail for {}",
            text(block, "slideFileName")
        );
        assert!(
            text(block, "summary").contains("No data file"),
            "QuPath's own summary should say the entry has no data"
        );
    }
}

/// **QuPath read back the metadata this project's editor wrote.**
///
/// The check `project.rs`'s own unit tests cannot make. Those go through
/// `project::edited` and back through `project::read`, which is one reader
/// agreeing with one writer — §8.6's "a format checked against the code that
/// writes it is untested", applied to the one field a user types into.
///
/// What is asserted is `DefaultProjectImageEntry.getMetadata()`, the map the
/// browser's right-click edits, and **not** the server metadata block: two
/// different fields at two depths with the same name, which §8.3b records
/// mistaking for each other.
///
/// The keys are awkward on purpose. `H&E` and `A=3` carry the characters the
/// browser's own `key=value` search splits on, and `noté`/`réséqué` are
/// non-ASCII — what QuPath has to agree about is the bytes, and a fixture of
/// `a=1` would agree about nothing.
#[test]
fn qupath_reads_back_the_metadata_that_was_written() {
    let entries = entries(FIXTURE);
    let first = entries.first().expect("an entry");

    assert_eq!(
        list(first, "metadataKeys"),
        ["stain", "block", "noté"],
        "QuPath read different keys, or read them in a different order — the \
         map is a LinkedHashMap and its order is the order they were written"
    );
    assert_eq!(
        list(first, "metadataValues"),
        ["H&E", "A=3", "réséqué"],
        "QuPath read different values"
    );

    // And QuPath's *own* summary line, which is what its browser puts under
    // an entry's name. Asserted as well as the map, because the map is what
    // was read and this is what a QuPath user would see — the same distinction
    // §8.6 draws between a file that parses and a file that shows something.
    let summary = text(first, "summary");
    for shown in ["stain:", "H&E", "block:", "A=3", "noté:", "réséqué"] {
        assert!(
            summary.contains(shown),
            "QuPath's own entry summary does not show {shown}: {summary}"
        );
    }

    // And the entry that was not edited has none, which is what says the
    // rewrite touched one entry rather than every one of them.
    let second = entries.get(1).expect("a second entry");
    assert!(list(second, "metadataKeys").is_empty());
    assert!(list(second, "metadataValues").is_empty());
    assert!(
        !text(second, "summary").contains("stain"),
        "an entry nobody edited is showing another entry's metadata"
    );
}
