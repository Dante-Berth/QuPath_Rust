//! Writing a QuPath project directory — the JSON parts of it.
//!
//! `docs/VIEWER.md` §8.3, and the **second and larger revision of §0's stated
//! non-goals**: §0 says no project format, and §8.3 says the item is "not to be
//! started without settling that". It was settled deliberately, and the terms
//! are worth keeping written down, because §8.3 argues both sides. For: a study
//! is fifty slides rather than one, and a project is how that work is actually
//! organised. Against: a project format is the point at which "a viewer"
//! starts becoming "a QuPath replacement", which §0 rules out.
//!
//! What settles it in favour without settling the larger question is that the
//! format is a **specification we can follow** rather than a design we would be
//! inventing. A project written here that QuPath opens is worth far more than
//! one that is merely similar, and following `DefaultProject.java` exactly is
//! what makes this an interoperability feature rather than a second application.
//!
//! # What this writes, and the one part it refuses to
//!
//! ```text
//! project.qpproj          {version, uri, images: [...]}
//! data/<entry-id>/
//!     server.json         how to reopen the image
//!     thumbnail.jpg       the caller's bytes, written as given
//!     summary.json        what is in the entry
//!     data.qpdata         NOT WRITTEN — see below
//! ```
//!
//! **`data.qpdata` is QuPath's Java serialisation and this does not attempt
//! it.** It is a binary format tied to Java's object graph, and a guessed
//! version is how a project becomes corrupt in a way that is discovered much
//! later, by someone who trusted it. Its absence is handled: QuPath opens an
//! entry with no data file as an entry with no objects, which is true. The
//! detections belong beside it as GeoJSON — [`crate::geojson`] — which is a
//! format both sides can actually read.
//!
//! That asymmetry is the honest description of this module: it writes what can
//! be written correctly and says plainly what it leaves out, rather than
//! producing four files of which one is a plausible fake.
//!
//! # Paths are URIs, and that is the part that silently breaks
//!
//! Every path QuPath stores is a `file:` URI, and a project whose `uri` fields
//! are bare paths opens with every image missing — the browser shows the entry
//! and fails to load it. [`file_uri`] is the conversion, percent-encoding
//! included, because a slide in a directory with a space in its name is not an
//! edge case.

use std::fmt::Write as _;
use std::path::Path;

use crate::geojson::escape;

/// The `builderType` QuPath keys a plain file-backed image on.
///
/// From `ImageServers.serverBuilderFactory`, which registers
/// `DefaultImageServerBuilder` under `"uri"`. A `server.json` with any other
/// discriminator — or none — is one QuPath's reader cannot dispatch, and the
/// entry fails to open with a message about the builder rather than about the
/// slide.
pub const BUILDER_TYPE: &str = "uri";

/// The provider class QuPath uses to open a whole-slide image.
///
/// A Java class name, written because QuPath's reader looks for a provider
/// whose class name matches. It is not a name this project can honour itself —
/// nothing here loads Java classes — and it is written anyway because the file
/// is for QuPath to read, not for this to read back.
pub const OPENSLIDE_PROVIDER: &str = "qupath.lib.images.servers.openslide.OpenslideServerBuilder";

/// When a project was created and when it was last written, in milliseconds
/// since the Unix epoch.
///
/// **QuPath requires both, and this is not a soft requirement.**
/// `DefaultProject.loadProject` reads them with no `has` guard at all:
///
/// ```java
/// creationTimestamp = element.get("createTimestamp").getAsLong();
/// modificationTimestamp = element.get("modifyTimestamp").getAsLong();
/// ```
///
/// Every other field it reads — `uri`, `version`, `images`, `lastID` — is
/// guarded. These two are the first thing it touches, so a project without them
/// throws a `NullPointerException` wrapped in an `IOException` before a single
/// entry is looked at, and the message names Gson rather than the field. A
/// project this module wrote could not be opened at all until they were here.
///
/// Taken as a parameter rather than read from the clock inside [`files`],
/// because this module's whole design is that the format is testable with no
/// directory on disk — and a function that reads the time is one that cannot be
/// compared against itself. It is also the honest shape: the two are genuinely
/// different values once a project is rewritten, and only the caller knows when
/// the project it is rewriting was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamps {
    /// When the project was created.
    pub created: u64,
    /// When it was last written.
    pub modified: u64,
}

impl Timestamps {
    /// Both fields set to now, for a project being created.
    ///
    /// Falls back to the epoch if the system clock is before it, which is a
    /// machine with its clock wrong rather than a case worth an error: QuPath
    /// shows these and reads nothing else from them, so a project that opens
    /// with a wrong date beats one that is not written.
    #[must_use]
    pub fn now() -> Self {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX));
        Self {
            created: millis,
            modified: millis,
        }
    }
}

/// One image in a project.
///
/// **There is no pixel size here.** There was, and it had nowhere to go: the
/// only field QuPath reads one from is the server metadata block, which
/// [`server_builder`] does not write and explains why, and `summary.json` has
/// no such field in `ImageDataSummary`. A pixel size on this struct would have
/// been written into a file, discarded by Gson, and reported by nothing —
/// which is the shape of every claim this project has had to go back and
/// check. QuPath reads the slide's own pixel size when it opens the image,
/// which is where this one came from.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    /// The slide's path on disk.
    pub path: &'a Path,
    /// The name the browser shows.
    pub name: &'a str,
    /// Level-0 width in pixels.
    pub width: u32,
    /// Level-0 height in pixels.
    pub height: u32,
}

impl Entry<'_> {
    /// The directory name for this entry's data, given its position.
    ///
    /// QuPath names these by a counter — `entryID`, from `counter
    /// .incrementAndGet()` — starting at 1. Copied rather than using a UUID,
    /// which QuPath keeps for a different purpose (`randomizedName`, for
    /// masking names in blinded studies).
    #[must_use]
    pub fn directory(index: usize) -> String {
        (index + 1).to_string()
    }
}

/// The contents of one file to write, as a path relative to the project
/// directory and its bytes.
#[derive(Debug, Clone)]
pub struct Output {
    /// Where the file goes, relative to the project directory.
    pub path: String,
    /// What to write there.
    pub contents: String,
}

/// Every JSON file a project directory needs, for `entries` in order.
///
/// Returns the relative paths and contents rather than writing them, so the
/// whole format is testable with no directory on disk — the rule
/// `docs/VIEWER.md` §6 applies to the viewport math, applied to a file format
/// for the same reason. The caller creates the directories and writes the
/// bytes, and writes `thumbnail.jpg` itself, since this crate encodes no JPEG.
///
/// `project_path` is where `project.qpproj` will live; it becomes the `uri`
/// field, which QuPath uses to tell a moved project from one in place.
/// `written` is [`Timestamps`], which QuPath requires and does not guard.
#[must_use]
pub fn files(project_path: &Path, entries: &[Entry<'_>], written: Timestamps) -> Vec<Output> {
    let mut out = vec![Output {
        path: "project.qpproj".to_owned(),
        contents: project_file(project_path, entries, written),
    }];
    for (index, entry) in entries.iter().enumerate() {
        let directory = Entry::directory(index);
        out.push(Output {
            path: format!("data/{directory}/server.json"),
            contents: server_file(entry),
        });
        out.push(Output {
            path: format!("data/{directory}/summary.json"),
            contents: summary_file(entry, written),
        });
    }
    out
}

/// `project.qpproj` — the index QuPath reads first.
///
/// `version` is the QuPath version the format matches rather than this
/// project's own: it says which reader the file was written for, and claiming a
/// version this format does not match is how a file gets read by the wrong
/// code path.
///
/// The two timestamps are not optional; [`Timestamps`] says why.
#[must_use]
pub fn project_file(project_path: &Path, entries: &[Entry<'_>], written: Timestamps) -> String {
    let mut out = String::new();
    out.push_str("{\n  \"version\": \"");
    escape(&mut out, FORMAT_VERSION);
    // The order is `DefaultProject.writeProject`'s: version, the two
    // timestamps, uri, lastID, images. Gson does not care about field order and
    // this follows it anyway, so that a project written here and one written by
    // QuPath can be read side by side.
    out.push_str("\",\n  \"createTimestamp\": ");
    let _ = write!(out, "{}", written.created);
    out.push_str(",\n  \"modifyTimestamp\": ");
    let _ = write!(out, "{}", written.modified);
    out.push_str(",\n  \"uri\": \"");
    escape(&mut out, &file_uri(project_path));
    out.push_str("\",\n  \"lastID\": ");
    let _ = write!(out, "{}", entries.len());
    out.push_str(",\n  \"images\": [");
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    {\n      \"entryID\": ");
        let _ = write!(out, "{}", index + 1);
        out.push_str(",\n      \"randomizedName\": \"");
        escape(&mut out, &placeholder_name(index));
        out.push_str("\",\n      \"imageName\": \"");
        escape(&mut out, entry.name);
        out.push_str("\",\n      \"serverBuilder\": ");
        out.push_str(&server_builder(entry));
        out.push_str("\n    }");
    }
    // An empty project closes the array on the same line, so it reads
    // `"images": []` rather than a bracket pair with a blank line between
    // them. Both parse; only one says "no images" at a glance, and an empty
    // project is exactly the state someone is looking at when they are
    // wondering why nothing is in it.
    if entries.is_empty() {
        out.push_str("]\n}\n");
    } else {
        out.push_str("\n  ]\n}\n");
    }
    out
}

/// The QuPath version this format is written for.
///
/// The vendored source in `qupath/` is 0.8.0-SNAPSHOT, and the format read here
/// is what that tree implements. Written as a released version rather than a
/// snapshot, because a snapshot string in a file that outlives the snapshot
/// says less than the release it corresponds to.
pub const FORMAT_VERSION: &str = "0.6.0";

/// `server.json` — how to reopen one image.
#[must_use]
pub fn server_file(entry: &Entry<'_>) -> String {
    let mut out = server_builder(entry);
    out.push('\n');
    out
}

/// The `serverBuilder` object, which appears both in `project.qpproj` and in
/// `server.json`.
///
/// One function for both because QuPath writes the same structure to each, and
/// two copies of it are two things to keep in step.
///
/// # Why there is no `metadata` block, and what was in it
///
/// QuPath writes one — `OpenslideImageServer.createServerBuilder` passes
/// `getMetadata()` — and this deliberately does not, because a stored metadata
/// block is **applied to the server when the image is opened**:
/// `AbstractServerBuilder.build` calls `server.setMetadata(metadata)` when one
/// is present, and `AbstractImageServer.setMetadata` reaches
/// `metadata.getLevels()`, which is `Arrays.asList(levels)` over a field Gson
/// leaves null when the JSON has no `levels`. A block without them is a
/// `NullPointerException` at the moment someone double-clicks the image.
///
/// The pyramid is the thing this crate cannot know. [`Entry`] carries a
/// width, a height and a pixel size; a level table is what `openslide` reports
/// from the file, and so are the preferred tile sizes, which arrive as `0` when
/// absent. Writing one level would be worse than writing none — it says the
/// image has no pyramid, and QuPath would then read full resolution for every
/// view of a 46,000-pixel slide.
///
/// QuPath's own API has the shape for this:
/// `DefaultImageServerBuilder.createInstance(providerClass, uri, args)` passes
/// a null metadata, and `build` then leaves the server with what OpenSlide read
/// from the slide. That is strictly better information than anything written
/// here, since it comes from the file.
///
/// **Three separate fields in that block were silently wrong before it was
/// removed**, and they are recorded because they are what a reader of
/// `ImageServerMetadata` should expect rather than assume:
///
/// - `"rgb": true` is not a field. Gson uses the field name, which is `isRGB`,
///   and QuPath read `false` off a project that said `true`;
/// - `"pixelWidthMicrons"` is not a field either. The pixel size lives in a
///   `pixelCalibration` object holding `{value, unit}` quantities, and the unit
///   is compared by string equality against `"\u{b5}m"` — U+00B5 MICRO SIGN,
///   not U+03BC GREEK SMALL LETTER MU, which looks identical in most fonts;
/// - `"channels": ["Red", "Green", "Blue"]` is the one that was not silent. An
///   `ImageChannel` is an object, so Gson threw
///   `Expected BEGIN_OBJECT but was STRING` and the **whole project** failed to
///   load, not just the entry.
fn server_builder(entry: &Entry<'_>) -> String {
    let mut out = String::new();
    out.push_str("{\n        \"builderType\": \"");
    out.push_str(BUILDER_TYPE);
    out.push_str("\",\n        \"providerClassName\": \"");
    out.push_str(OPENSLIDE_PROVIDER);
    out.push_str("\",\n        \"uri\": \"");
    escape(&mut out, &file_uri(entry.path));
    // No `metadata` block. The slide's own metadata is what QuPath uses, and
    // it is better than anything that could be written here; the doc comment
    // above says what happens to a project that claims otherwise.
    out.push_str("\",\n        \"args\": []\n      }");
    out
}

/// `summary.json` — what the entry holds.
///
/// QuPath's `ImageDataSummary` also carries a hierarchy summary of how many
/// objects of each kind an entry has. This writes zero, and that is accurate
/// rather than a placeholder: no `data.qpdata` is written, so the entry
/// genuinely contains no objects as far as QuPath is concerned.
///
/// **The keys are `server` and `hierarchy`**, which are the field names
/// `ImageDataSummary` declares — not `serverSummary` and `hierarchySummary`,
/// which is what this wrote until a real QuPath was asked. Nothing in QuPath
/// reads this file, so the mistake could not have surfaced as a failure; it is
/// corrected because a file that claims to be in QuPath's format and is not is
/// exactly what this module exists not to produce.
#[must_use]
pub fn summary_file(entry: &Entry<'_>, written: Timestamps) -> String {
    let mut out = String::new();
    out.push_str("{\n  \"timestamp\": ");
    let _ = write!(out, "{}", written.modified);
    out.push_str(",\n  \"imageType\": \"");
    out.push_str(IMAGE_TYPE);
    out.push_str("\",\n  \"server\": {\n    \"width\": ");
    let _ = write!(out, "{}", entry.width);
    out.push_str(",\n    \"height\": ");
    let _ = write!(out, "{}", entry.height);
    out.push_str(
        ",\n    \"sizeC\": 3,\n    \"sizeZ\": 1,\n    \"sizeT\": 1\n  },\n  \
         \"hierarchy\": {\n    \"nObjects\": 0\n  }\n}\n",
    );
    out
}

/// The image type an entry claims, which is that it does not know.
///
/// `ImageData.ImageType` has `BRIGHTFIELD_H_E` and this writes `UNSET`, because
/// nothing here has looked at the pixels. Naming the stain would be right for
/// every slide this project has been run on and wrong the first time someone
/// opens an immunohistochemistry slide, and QuPath asks when the type is unset.
pub const IMAGE_TYPE: &str = "UNSET";

/// The name QuPath would randomise for a blinded study.
///
/// QuPath uses `UUID.randomUUID()`. This writes a stable name derived from the
/// index instead, because a random one makes the same project written twice
/// differ byte for byte, which is untestable and produces a spurious diff every
/// time a project is rewritten. It is **not** a substitute for real
/// randomisation: masking names for a blinded study needs unpredictability, and
/// a caller doing that should supply its own. Nothing here claims to.
fn placeholder_name(index: usize) -> String {
    format!("entry-{}", index + 1)
}

/// A `file:` URI for a path, percent-encoding what has to be encoded.
///
/// QuPath stores `File.toURI().toString()` everywhere and matches on it, so a
/// bare path is not a lenient alternative — it is a project whose images all
/// fail to load. Directories are not marked with a trailing slash here because
/// every path this writes is a file.
///
/// Only the characters that must be encoded are: a URI with every byte escaped
/// is legal and unreadable, and the fields are read by people when a project
/// goes wrong.
///
/// # A relative path is resolved first, because `File.toURI()` resolves one
///
/// `file://` followed by a relative path is not a lenient spelling of the same
/// thing: `file://data/CMU-1.svs` names the **host** `data` and the path
/// `/CMU-1.svs`, so QuPath looks for the slide on a machine called `data` and
/// the entry fails to open — the exact failure this module's header warns
/// about, arrived at from the other side.
///
/// It is not a hypothetical path either. `docs/VIEWER.md` documents starting
/// the application as `cargo run -p qupath-app -- data/CMU-1.svs`, so the path
/// an entry would be written from is relative in the commonest invocation
/// there is.
///
/// Java's `File.toURI()` resolves against the working directory, and following
/// it exactly is this module's whole thesis, so that is what happens here.
/// [`std::path::absolute`] is the lexical form of it — it reads the working
/// directory and joins, without resolving symlinks or touching the file — which
/// is what `File.toURI()` does too. A path that cannot be made absolute is
/// written as it stands, since a wrong URI beats no project at all and the
/// caller has no better information than this does.
#[must_use]
pub fn file_uri(path: &Path) -> String {
    let absolute = std::path::absolute(path);
    let path = absolute.as_deref().unwrap_or(path);
    let mut out = String::from("file://");
    let text = path.to_string_lossy();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed pair, so two writes of the same project are comparable.
    ///
    /// Distinct values rather than one repeated, because `createTimestamp` and
    /// `modifyTimestamp` are different fields and a test written with the same
    /// number in both cannot see them swapped.
    fn written() -> Timestamps {
        Timestamps {
            created: 1_700_000_000_000,
            modified: 1_700_000_060_000,
        }
    }

    fn entry() -> Entry<'static> {
        Entry {
            path: Path::new("/data/CMU-1.svs"),
            name: "CMU-1.svs",
            width: 46000,
            height: 32914,
        }
    }

    #[test]
    fn a_project_names_every_file_qupath_looks_for() {
        let files = files(Path::new("/study/project.qpproj"), &[entry()], written());
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "project.qpproj",
                "data/1/server.json",
                "data/1/summary.json",
            ],
        );
    }

    /// The one file this module refuses to write, pinned so that adding it
    /// later is a deliberate act rather than a quiet one.
    #[test]
    fn no_qpdata_is_written() {
        let files = files(Path::new("/study/project.qpproj"), &[entry(), entry()], written());
        assert!(
            !files.iter().any(|file| file.path.contains("qpdata")),
            "data.qpdata is Java serialisation and is deliberately not attempted",
        );
    }

    /// The discriminator QuPath dispatches on. Getting this wrong produces a
    /// project that opens and then fails on every image.
    #[test]
    fn the_server_builder_carries_qupaths_discriminator() {
        let json = server_file(&entry());
        assert!(json.contains("\"builderType\": \"uri\""), "{json}");
        assert!(json.contains("OpenslideServerBuilder"), "{json}");
    }

    #[test]
    fn entries_are_numbered_from_one() {
        assert_eq!(Entry::directory(0), "1");
        assert_eq!(Entry::directory(4), "5");
        let files = files(Path::new("/p.qpproj"), &[entry(), entry()], written());
        assert!(files.iter().any(|f| f.path == "data/2/server.json"));
    }

    /// Paths are URIs, and a bare path is a project whose images all fail to
    /// load rather than a lenient alternative.
    #[test]
    fn paths_are_written_as_file_uris() {
        let json = server_file(&entry());
        assert!(json.contains("file:///data/CMU-1.svs"), "{json}");
    }

    /// A directory with a space in its name is not an edge case, and an
    /// unencoded space is what breaks the URI.
    /// A relative path becomes an absolute URI, because `file://` plus a
    /// relative path names a *host*.
    ///
    /// `file://data/CMU-1.svs` is host `data`, path `/CMU-1.svs`: QuPath looks
    /// for the slide on another machine and the entry fails to open, which is
    /// the failure this module's header is about. It is the commonest case
    /// rather than an odd one — `docs/VIEWER.md` starts the application as
    /// `cargo run -p qupath-app -- data/CMU-1.svs`.
    #[test]
    fn a_relative_path_is_resolved_rather_than_naming_a_host() {
        let uri = file_uri(Path::new("data/CMU-1.svs"));
        assert!(
            uri.starts_with("file:///"),
            "a relative path must not leave a host in the URI: {uri}"
        );
        assert!(uri.ends_with("/data/CMU-1.svs"), "{uri}");

        // An absolute path is untouched, which is the case the rest of these
        // tests are written against.
        assert_eq!(
            file_uri(Path::new("/data/CMU-1.svs")),
            "file:///data/CMU-1.svs"
        );
    }

    #[test]
    fn a_space_in_a_path_is_percent_encoded() {
        assert_eq!(
            file_uri(Path::new("/My Slides/a b.svs")),
            "file:///My%20Slides/a%20b.svs",
        );
    }

    /// Encoding every byte is legal and unreadable; the ordinary characters
    /// must survive so the field can be read when a project goes wrong.
    #[test]
    fn ordinary_characters_are_not_encoded() {
        assert_eq!(
            file_uri(Path::new("/data/CMU-1_test.v2.svs")),
            "file:///data/CMU-1_test.v2.svs",
        );
    }

    /// No server metadata, and no pixel size anywhere.
    ///
    /// Both were written until a real QuPath was asked to open the result. The
    /// metadata block is applied to the image server when the image is opened —
    /// `AbstractImageServer.setMetadata` reaches `metadata.getLevels()` — and a
    /// block with no `levels` is a `NullPointerException` at that moment. The
    /// pyramid is the one thing this crate cannot know, so the block goes, and
    /// the pixel size with it: `pixelWidthMicrons` was never a field QuPath has
    /// and Gson had been silently dropping it.
    #[test]
    fn no_metadata_is_claimed_about_the_image() {
        let json = server_file(&entry());
        for absent in [
            "metadata",
            "pixelWidthMicrons",
            "pixelCalibration",
            "channels",
            "rgb",
            "width",
        ] {
            assert!(!json.contains(absent), "{absent} is still written: {json}");
        }
        // What is left is what QuPath dispatches on and opens with.
        assert!(json.contains("\"builderType\": \"uri\""), "{json}");
        assert!(json.contains("\"uri\": \"file:///data/CMU-1.svs\""), "{json}");
        assert!(json.contains("\"args\": []"), "{json}");
    }

    /// `summary.json`'s keys are the field names `ImageDataSummary` declares.
    ///
    /// `serverSummary` and `hierarchySummary` were plausible and wrong. Nothing
    /// in QuPath reads this file, so no failure could have reported it.
    #[test]
    fn the_summary_uses_qupaths_field_names() {
        let json = summary_file(&entry(), written());
        assert!(json.contains("\"server\": {"), "{json}");
        assert!(json.contains("\"hierarchy\": {"), "{json}");
        assert!(!json.contains("serverSummary"), "{json}");
        assert!(!json.contains("hierarchySummary"), "{json}");
        assert!(json.contains("\"nObjects\": 0"), "{json}");
        assert!(json.contains("\"timestamp\": 1700000060000"), "{json}");
        // Unset, because nothing here has looked at the pixels.
        assert!(json.contains("\"imageType\": \"UNSET\""), "{json}");
    }

    /// Writing the same project twice must produce the same bytes, or every
    /// rewrite is a spurious diff.
    #[test]
    fn the_output_is_stable_across_runs() {
        let first = files(Path::new("/study/project.qpproj"), &[entry()], written());
        let second = files(Path::new("/study/project.qpproj"), &[entry()], written());
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.contents, b.contents);
        }
    }

    #[test]
    fn every_file_is_balanced_json() {
        for file in files(Path::new("/study/p.qpproj"), &[entry(), entry()], written()) {
            assert_eq!(
                file.contents.matches('{').count(),
                file.contents.matches('}').count(),
                "unbalanced braces in {}",
                file.path,
            );
            assert_eq!(
                file.contents.matches('[').count(),
                file.contents.matches(']').count(),
                "unbalanced brackets in {}",
                file.path,
            );
        }
    }

    /// The two fields QuPath reads before it reads anything else.
    ///
    /// `DefaultProject.loadProject` calls `getAsLong()` on both with no `has`
    /// guard, so their absence is not a missing nicety — it is a
    /// `NullPointerException` wrapped in an `IOException`, thrown before a
    /// single entry is looked at, with a message that names Gson rather than
    /// the field. Every project this module wrote was unopenable until they
    /// were written, and nothing here could see it: the file parses, the field
    /// names that *are* present are right, and `DefaultProject.java` was read
    /// for what it writes rather than for what it demands.
    #[test]
    fn a_project_carries_the_timestamps_qupath_reads_unguarded() {
        let json = project_file(Path::new("/study/project.qpproj"), &[entry()], written());
        assert!(
            json.contains("\"createTimestamp\": 1700000000000"),
            "no createTimestamp: {json}"
        );
        assert!(
            json.contains("\"modifyTimestamp\": 1700000060000"),
            "no modifyTimestamp: {json}"
        );

        // Written as JSON numbers. `getAsLong` on a quoted string happens to
        // work in Gson, which is exactly why writing one would survive here and
        // be wrong beside a project QuPath wrote.
        assert!(!json.contains("\"createTimestamp\": \""), "{json}");

        // A clock that has not been asked twice still gives two fields.
        let now = Timestamps::now();
        assert!(now.created > 1_600_000_000_000, "the epoch is in milliseconds");
        assert_eq!(now.created, now.modified, "a new project is written once");
    }

    #[test]
    fn a_project_with_no_images_is_still_a_project() {
        let files = files(Path::new("/study/project.qpproj"), &[], written());
        assert_eq!(files.len(), 1);
        assert!(files[0].contents.contains("\"images\": []"));
    }
}
