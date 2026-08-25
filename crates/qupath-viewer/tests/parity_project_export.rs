//! Checks this project's reader against a `project.qpproj` **QuPath wrote**.
//!
//! `parity_project_import.rs` runs the other way — QuPath's reader on a file
//! this project wrote — and that direction cannot see this one. A browser opens
//! projects QuPath wrote, and what arrives is Gson's spelling of
//! `DefaultProject`, not this project's spelling of the same fields.
//!
//! `parity/java/ProjectExport.java` loads the sample project, adds one entry
//! carrying the metadata block an opened image would have, and writes the whole
//! thing back with `syncChanges()`. The fixture is that file, with the two
//! absolute paths in it replaced by `/PROJECT` and `/SLIDES` and the
//! `randomizedName` UUIDs replaced by a fixed string — `record-project.sh`
//! makes those three substitutions and then checks that nothing of the
//! recording machine survived. Everything else is byte for byte what QuPath
//! wrote.
//!
//! # What the first recording said
//!
//! - **both URI spellings are in the same project.** The two entries QuPath
//!   loaded from this project's file kept `file:///SLIDES/...`, and the entry
//!   QuPath created from `File.toURI()` is `file:/SLIDES/...`. A reader that
//!   accepted one spelling would read two thirds of a project QuPath wrote and
//!   drop the rest;
//! - **`"imageName": "Image null"`.** `DefaultProjectImageEntry`'s constructor
//!   builds the fallback name from the `Long` *parameter* rather than the field
//!   it has just assigned, so an unnamed entry is named after a null three
//!   lines above its own `"entryID": 3`. Reading the source gives `Image 3`;
//! - **`"version": "0.8.0-SNAPSHOT"`.** QuPath stamps its own version over
//!   whatever the file claimed, so a reader must take the version as
//!   information and not as something to match against;
//! - and the **metadata block is now written down** — `levels`, `channels` as
//!   objects, `pixelCalibration` in microns, `preferredTileWidth`. That block
//!   is what `docs/VIEWER.md` §8.3 removed for not knowing the pyramid, and
//!   §8.8 lists as needing one. This is the specification for it, in QuPath's
//!   own hand.

use std::path::Path;

use qupath_viewer::project;

#[path = "../examples/shared/project.rs"]
mod sample;

const FIXTURE: &str = include_str!("../../../parity/fixtures/ProjectExport.json");

/// The placeholder `record-project.sh` puts where the slides' directory was.
const SLIDES: &str = "/SLIDES";

/// This project's reader reads a project QuPath wrote.
#[test]
fn a_project_qupath_wrote_reads_back() {
    let stored = project::read(FIXTURE).expect("QuPath's own project file");

    // QuPath stamps its own version over the one the file claimed, which was
    // `FORMAT_VERSION`. Asserted as "not what we wrote" rather than against a
    // literal, so that updating the vendored QuPath does not fail here for
    // having a different version number.
    let version = stored.version.expect("a version");
    assert_ne!(
        version,
        project::FORMAT_VERSION,
        "QuPath is expected to write its own version, not keep the one it read"
    );

    // The timestamps are the sample's, unchanged: `writeProject` writes back
    // what `loadProject` read, and never touches the clock. A reader that
    // required them is reading a file that has them.
    assert_eq!(stored.written, sample::WRITTEN);

    assert_eq!(stored.uri.as_deref(), Some("file:/PROJECT/project.qpproj"));
    assert_eq!(stored.images.len(), 3, "two loaded entries and one added");
}

/// **Both spellings of a `file:` URI appear in one project QuPath wrote.**
///
/// The entries QuPath loaded from this project's file kept `file:///`, the
/// three-slash form [`project::file_uri`] writes; the entry QuPath built itself
/// from `File.toURI()` is `file:/`, with no authority at all. Java reads both
/// as the same URI. A reader here that took only one would drop rows out of a
/// project with no sign that it had.
#[test]
fn the_two_uri_spellings_both_resolve() {
    let stored = project::read(FIXTURE).expect("QuPath's own project file");
    let uris: Vec<&str> = stored
        .images
        .iter()
        .map(|image| image.uri.as_deref().expect("a uri"))
        .collect();
    assert!(
        uris.iter().any(|uri| uri.starts_with("file:///")),
        "no three-slash URI in {uris:?}"
    );
    assert!(
        uris.iter()
            .any(|uri| uri.starts_with("file:/") && !uri.starts_with("file://")),
        "no one-slash URI in {uris:?}; the fixture no longer covers what it was recorded for"
    );

    for image in &stored.images {
        let slide = image.slide.as_deref().unwrap_or_else(|error| {
            panic!("entry {} did not resolve: {error}", image.id);
        });
        assert!(
            slide.starts_with(SLIDES),
            "entry {} resolved to {}, outside the recorded slide directory",
            image.id,
            slide.display(),
        );
    }
}

/// The entries QuPath loaded keep their ids, names and slides.
///
/// `entryID` names the directory under `data/`, so entries sharing one is a
/// project whose images overwrite each other — and it is invisible in a project
/// of one image, which is why the sample has two.
#[test]
fn the_entries_survive_the_round_trip_through_qupath() {
    let stored = project::read(FIXTURE).expect("QuPath's own project file");
    let ids: Vec<u64> = stored.images.iter().map(|image| image.id).collect();
    assert_eq!(ids, vec![1, 2, 3]);

    for (image, (name, file)) in stored.images.iter().zip([
        (sample::NAME, sample::SLIDE),
        (sample::SECOND_NAME, sample::SECOND_SLIDE),
    ]) {
        assert_eq!(image.name, name, "the browser would show a different name");
        assert_eq!(image.builder, project::BUILDER_TYPE);
        let file = Path::new(file).file_name().expect("a file name");
        assert_eq!(
            image.slide.as_deref().expect("a slide").file_name(),
            Some(file),
            "entry {} points at a different slide",
            image.id,
        );
    }
}

/// The name QuPath gives an entry that has none, which is not the one the
/// source reads like.
///
/// `this.imageName = "Image " + entryID` uses the `Long` parameter, null on
/// this path, rather than the field assigned from the counter one line above.
/// So an unnamed entry is `Image null` and not `Image 3`. Pinned because a
/// browser has to show *something* for such a row, and because this is the
/// second time reading `DefaultProject.java` has given a different answer from
/// running it.
#[test]
fn qupath_names_an_unnamed_entry_after_a_null() {
    let stored = project::read(FIXTURE).expect("QuPath's own project file");
    let added = stored.images.last().expect("the added entry");
    assert_eq!(added.id, 3);
    assert_eq!(added.name, "Image null");
}

/// The added entry carries a metadata block, and the entry's own `uri` is read
/// past it.
///
/// This is the shape `docs/VIEWER.md` §8.3 removed for not knowing the pyramid
/// and §8.8 lists as needing one. The fixture is the specification: a `levels`
/// array, `preferredTileWidth` and `preferredTileHeight`, `isRGB`, `channels`
/// as objects, and a `pixelCalibration` of `{value, unit}` quantities whose
/// unit is U+00B5 MICRO SIGN.
#[test]
fn the_metadata_block_is_what_a_stored_pyramid_looks_like() {
    // Read from the fixture text rather than through `project::read`, which
    // deliberately reports nothing from this block — a `Stored` image has a
    // name and a path and no dimensions, because a browser gets the pixels from
    // opening the slide. These assertions are about what a *writer* would have
    // to produce, and they are here because this file is where that arrived.
    for expected in [
        "\"levels\": [",
        "\"downsample\": 1.0",
        "\"downsample\": 16.0",
        "\"preferredTileWidth\": 256",
        "\"preferredTileHeight\": 256",
        "\"isRGB\": true",
        "\"pixelType\": \"UINT8\"",
        "\"unit\": \"\u{b5}m\"",
        "\"name\": \"Red\"",
    ] {
        assert!(
            FIXTURE.contains(expected),
            "{expected} is not in the block QuPath wrote"
        );
    }
    // The three fields that were silently wrong when this project last tried to
    // write one. `rgb` and `pixelWidthMicrons` are not field names, and a
    // channel is an object rather than a string.
    assert!(!FIXTURE.contains("\"rgb\":"), "the field is isRGB");
    assert!(
        !FIXTURE.contains("pixelWidthMicrons"),
        "the value is a quantity"
    );
    assert!(
        !FIXTURE.contains("\"channels\": [\""),
        "a channel is an object"
    );

    // And the entry's own uri is found past all of it.
    let stored = project::read(FIXTURE).expect("QuPath's own project file");
    let added = stored.images.last().expect("the added entry");
    assert_eq!(
        added.slide.as_deref(),
        Ok(Path::new("/SLIDES/CMU-1-JP2K-33005.svs")),
    );
}

/// **A project QuPath wrote, read and written back, is byte for byte the same
/// file.**
///
/// This is the check that says the reader loses nothing, and it is an equality
/// rather than an argument. `Stored` is a *summary* — an id, a name, a path per
/// image — and an index rebuilt from a summary would silently delete every
/// entry's `description`, its `metadata` map, its `tags` and its stored server
/// metadata block, in a file that still opened and still listed the same
/// images. Keeping the document and re-serialising it is what makes adding a
/// slide to someone else's project a safe thing to do.
///
/// It holds because `json`'s writer copies Gson's spelling exactly, which had
/// to be measured: two-space indent, `": "` separators, empty containers
/// inline, `<`, `>`, `&`, `=` and `'` as lowercase `\u00xx` escapes, U+2028 and
/// U+2029 escaped, everything else above ASCII raw, and no trailing newline.
/// Any one of those wrong and this fails — which is the point of asserting on
/// the bytes rather than on a parse.
#[test]
fn a_project_qupath_wrote_is_written_back_unchanged() {
    let stored = project::read(FIXTURE).expect("QuPath's own project file");
    let rewritten = stored.rewritten();
    if rewritten != FIXTURE {
        let at = rewritten
            .char_indices()
            .zip(FIXTURE.chars())
            .find(|((_, written), read)| written != read)
            .map_or(rewritten.len().min(FIXTURE.len()), |((at, _), _)| at);
        panic!(
            "the rewrite differs from byte {at}:\n  written: {:?}\n     read: {:?}",
            &rewritten[at.saturating_sub(40)..(at + 40).min(rewritten.len())],
            &FIXTURE[at.saturating_sub(40)..(at + 40).min(FIXTURE.len())],
        );
    }
}

/// Adding an entry to a project QuPath wrote changes that entry and nothing
/// else.
///
/// The entry that matters here is the third one, which carries the full
/// metadata block — the pyramid, the tile sizes, the channels, the pixel
/// calibration. That block is precisely what this crate cannot produce, so an
/// append that dropped it would be taking information out of a project in
/// exchange for adding a row to it.
#[test]
fn adding_to_a_project_leaves_every_other_entry_alone() {
    let stored = project::read(FIXTURE).expect("QuPath's own project file");
    let slide = Path::new("/SLIDES/CMU-1.svs");
    let entries = [project::Entry {
        path: slide,
        name: "CMU-1",
        width: 46_000,
        height: 32_914,
    }];
    let written = project::Timestamps {
        created: 1,
        modified: 1_800_000_000_000,
    };

    let files = project::added(&stored, &entries, written);
    let index = files
        .iter()
        .find(|file| file.path == "project.qpproj")
        .expect("the index");

    // Every entry that was there, byte for byte, still in QuPath's spelling.
    // Asserted as one span rather than field by field: the three entries and
    // everything nested inside them — the metadata block, its levels, its
    // channels, its pixel calibration — are one contiguous piece of the file,
    // and a rewrite that rebuilt any of it would not contain that piece.
    let opens = FIXTURE.find("\"images\": [").expect("the images array");
    let closes = FIXTURE.rfind("\n  ]").expect("the images array closes");
    assert!(
        index.contents.contains(&FIXTURE[opens..closes]),
        "the rewrite changed an entry it was not asked to touch"
    );

    // The header, apart from the two fields an append is entitled to change.
    for kept in [
        "\"version\": \"",
        "\"createTimestamp\": 1700000000000",
        "\"uri\": \"file:/PROJECT/project.qpproj\"",
    ] {
        assert!(
            index.contents.contains(kept),
            "{kept} did not survive the rewrite"
        );
    }
    assert!(
        index.contents.contains("\"lastID\": 4"),
        "lastID was not advanced"
    );
    assert!(
        !index
            .contents
            .contains("\"modifyTimestamp\": 1700000060000"),
        "modifyTimestamp was left saying the project had not been written"
    );

    // And the entry that was added is there, after the three that were.
    let reread = project::read(&index.contents).expect("the rewritten project");
    assert_eq!(reread.images.len(), 4);
    assert_eq!(reread.images[3].name, "CMU-1");
    assert_eq!(reread.images[3].slide.as_deref(), Ok(slide));

    // `lastID` was 3, so the new entry is 4 and its files go in `data/4`.
    assert_eq!(reread.images[3].id, 4);
    let written_paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    assert!(
        written_paths.contains(&"data/4/server.json"),
        "{written_paths:?}"
    );
    assert!(
        written_paths.contains(&"data/4/summary.json"),
        "{written_paths:?}"
    );

    // The timestamps: created untouched, modified advanced to the write.
    assert_eq!(reread.written.created, sample::WRITTEN.created);
    assert_eq!(reread.written.modified, written.modified);
}

/// The project's own `uri` comes before every entry's, so the first `"uri"` in
/// the file names the `.qpproj` rather than a slide.
///
/// This is the failure a reader that searched for a key would have, and it is
/// not a subtle one: every image in the project would open the project file.
#[test]
fn the_first_uri_in_the_file_is_not_a_slide() {
    let first = FIXTURE.find("\"uri\"").expect("a uri");
    let line = FIXTURE[first..].lines().next().expect("a line");
    assert!(
        line.contains(".qpproj"),
        "the fixture no longer starts with the project's own uri: {line}"
    );
    let stored = project::read(FIXTURE).expect("QuPath's own project file");
    for image in &stored.images {
        assert!(
            !image
                .slide
                .as_deref()
                .expect("a slide")
                .ends_with("project.qpproj"),
            "entry {} resolved to the project file",
            image.id,
        );
    }
}
