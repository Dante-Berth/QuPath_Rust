//! A QuPath project directory: writing the JSON parts of it, and reading one
//! back.
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
//! # Reading, which is not the inverse of writing
//!
//! [`read`] parses `project.qpproj` back into [`Stored`], because a browser
//! opens projects it did not write. It is **not** a round trip: a [`Stored`]
//! image has a name and a path and no dimensions, since the only place a
//! project keeps those is the metadata block this refuses to write. A browser
//! learns where the slide is from the project and everything about the pixels
//! from opening it, which is the division §8.3 settled on when the block went.
//!
//! Reading a real project is also what showed that **QuPath's spelling of a
//! `file:` URI is not this one's**: `File.toURI()` writes `file:/data/x.svs`
//! with no authority, and [`file_uri`] writes `file:///data/x.svs` with an
//! empty one. Java parses both to the same URI, which is why writing the second
//! has always opened — but a reader that took only the second would fail on
//! every project QuPath wrote. [`uri_path`] takes both.
//!
//! # Paths are URIs, and that is the part that silently breaks
//!
//! Every path QuPath stores is a `file:` URI, and a project whose `uri` fields
//! are bare paths opens with every image missing — the browser shows the entry
//! and fails to load it. [`file_uri`] is the conversion, percent-encoding
//! included, because a slide in a directory with a space in its name is not an
//! edge case.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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
        out.push('\n');
        out.push_str(&entry_object(index as u64 + 1, entry));
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

/// One entry of the `images` array, indented as it sits inside the file.
///
/// One definition, because there are two callers: [`project_file`], writing a
/// project that does not exist yet, and [`added`], appending to one that does.
/// Two spellings of an entry would be two things to keep in step, and the way
/// they would eventually differ is a project whose first entries QuPath reads
/// and whose last one it does not.
///
/// `id` rather than a position: an appended entry's id continues the project's
/// counter, and the two are the same number only in a project written from
/// scratch.
fn entry_object(id: u64, entry: &Entry<'_>) -> String {
    let mut out = String::new();
    out.push_str("    {\n      \"entryID\": ");
    let _ = write!(out, "{id}");
    out.push_str(",\n      \"randomizedName\": \"");
    escape(&mut out, &placeholder_name(id));
    out.push_str("\",\n      \"imageName\": \"");
    escape(&mut out, entry.name);
    out.push_str("\",\n      \"serverBuilder\": ");
    out.push_str(&server_builder(entry));
    out.push_str("\n    }");
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
/// entry's id instead, because a random one makes the same project written
/// twice differ byte for byte, which is untestable and produces a spurious diff
/// every time a project is rewritten. It is **not** a substitute for real
/// randomisation: masking names for a blinded study needs unpredictability, and
/// a caller doing that should supply its own. Nothing here claims to.
///
/// QuPath's own is not stable across a *read* either: its copy constructor for
/// an entry does not carry the field over, so `UUID.randomUUID()` runs again
/// every time a project is loaded. `parity/record-project.sh` has to substitute
/// them out of its fixture for that reason.
fn placeholder_name(id: u64) -> String {
    format!("entry-{id}")
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

/// One image in a project, as the project stores it.
///
/// The counterpart of [`Entry`], and **not its inverse**: an [`Entry`] carries
/// a width and a height, and this does not, because the only place a project
/// keeps them is the server metadata block that [`server_builder`] deliberately
/// does not write. A reader that reported dimensions here would be reporting
/// something no project this module writes contains, and none QuPath writes
/// contains either once the image has never been opened.
///
/// That asymmetry is the useful fact rather than a gap: a browser learns a
/// name and a path from the project, and everything about the pixels from
/// opening the slide — which is exactly the division `docs/VIEWER.md` §8.3
/// settled on when the metadata block was removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// `entryID`, which is also the name of the entry's directory under
    /// `data/`.
    pub id: u64,
    /// The name the browser shows.
    ///
    /// `Image <id>` when the entry has no `imageName`, and **that is this
    /// module's choice rather than QuPath's behaviour** — the distinction took
    /// a recording to see. `DefaultProjectImageEntry`'s constructor reads
    /// `this.imageName = "Image " + entryID`, but `entryID` there is the `Long`
    /// parameter, null on the path that creates an entry from a builder, not
    /// the field assigned from the counter one line above. QuPath writes
    /// `"Image null"`, which `ProjectExport.json` pins. And its *reader*
    /// applies no fallback at all — `getImageName()` returns the stored field —
    /// so an entry whose JSON has no `imageName` is a blank row in QuPath's own
    /// browser.
    ///
    /// Neither is worth copying. A blank row is a row nobody can point at, and
    /// `Image null` names the bug rather than the entry, so this uses the name
    /// that constructor was written to produce.
    pub name: String,
    /// The `builderType` discriminator, which says what kind of image this is.
    ///
    /// [`BUILDER_TYPE`] — `"uri"` — is a plain file. QuPath registers sixteen
    /// others in `ImageServers`: `rotated`, `cropped`, `sparse`, `affine`,
    /// `pyramidize` and the rest, each wrapping another builder rather than
    /// naming a file. Kept as text because this is not a list to reimplement;
    /// it is what lets a browser say *which* kind of entry it cannot open.
    pub builder: String,
    /// Where the slide is, or why this entry does not name one here.
    ///
    /// A `Result` and not an `Option` because every way of not having a path is
    /// a thing the user needs told: an entry built by a transform QuPath
    /// supports and this does not, a URI naming another machine, a URI that is
    /// not a `file:` URI at all. A browser that silently omitted those rows
    /// would be showing a project with fewer images than it has.
    pub slide: Result<PathBuf, String>,
    /// The URI exactly as the project stores it, for the builders that have
    /// one.
    ///
    /// Kept verbatim rather than reconstructed from [`Image::slide`], because
    /// the two spellings are not the same text. `File.toURI()` writes
    /// `file:/data/x.svs` and [`file_uri`] writes `file:///data/x.svs`; Java
    /// reads both as the same URI, so a project rewritten from the decoded
    /// paths would change every line without changing anything.
    pub uri: Option<String>,
    /// The entry's free-text `description`, which QuPath's browser shows under
    /// the name.
    ///
    /// `Option` rather than an empty string, because `DefaultProjectImageEntry`
    /// leaves the field null until someone sets one and Gson omits it — so
    /// "nobody wrote a description" and "somebody cleared it to empty" are
    /// different states of the file, and only the reader can still tell them
    /// apart.
    pub description: Option<String>,
    /// The entry's `metadata` map, in the file's order.
    ///
    /// **Not the server metadata block, which is a different `metadata` at a
    /// different depth.** The one here is `DefaultProjectImageEntry.metadata`,
    /// a `Map<String, String>` a user fills in from the browser; the one inside
    /// `serverBuilder` describes the pixels and is what [`server_builder`]
    /// deliberately does not write. The two are told apart by where they are
    /// and nothing else — the same trap [`Image::uri`] met, where the first
    /// `"uri"` in an entry's text is not the entry's.
    ///
    /// A `Vec` of pairs and not a map, because Gson serialises a
    /// `LinkedHashMap` and the order in the file is the order somebody entered
    /// the keys in. `getMetadata` hands the browser that map directly, so its
    /// iteration order is what QuPath's own summary lists — and a `BTreeMap`
    /// here would silently re-alphabetise it.
    ///
    /// Values that are not strings are dropped rather than stringified: the
    /// Java field is `Map<String, String>`, so a number there is a file QuPath
    /// would itself fail to deserialise, and inventing a spelling for it would
    /// be this reader claiming to have read something QuPath cannot.
    pub metadata: Vec<(String, String)>,
    /// The entry's `tags`, in the file's order.
    ///
    /// A `LinkedHashSet` on the Java side, so the same ordering argument as
    /// [`Image::metadata`] applies. Read but not yet shown by anything: it is
    /// here because [`added`] preserves the field blind, and a browser that
    /// wants to filter on tags should not have to re-read the document to find
    /// them.
    pub tags: Vec<String>,
    /// The `randomizedName`, which is the name shown when names are masked.
    ///
    /// `getImageName()` returns this instead of `imageName` when a project has
    /// `setMaskImageNames(true)`, for blinded analysis. `Option` because it is
    /// a field a project may not carry, and a browser that masked names by
    /// showing an empty string would blind the reader rather than the study.
    ///
    /// **What is in it depends on who wrote the project, and the difference
    /// matters.** QuPath writes `UUID.randomUUID()`. This module writes
    /// `entry-<id>` — see `placeholder_name`, which says why: a random name
    /// makes the same project written twice differ byte for byte. That
    /// placeholder is *derived from the id*, so masking a project this module
    /// wrote reveals the entry order, which is the one thing masking exists to
    /// hide. A caller that offers masking has to say so; see
    /// `chrome::Order::Masked`.
    pub randomized_name: Option<String>,
}

/// A project, as read back from `project.qpproj`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// The format version the file claims, if it says.
    ///
    /// `Option` because QuPath's own reader guards it — `element.has("version")`
    /// — and treats its absence as an old project rather than an error.
    pub version: Option<String>,
    /// The two timestamps, which QuPath does *not* guard; see [`Timestamps`].
    pub written: Timestamps,
    /// The `uri` field: where the project says it lives.
    ///
    /// QuPath compares this against the file it actually opened to tell a
    /// project that has been moved from one in place, and so can a browser.
    pub uri: Option<String>,
    /// The images, in file order.
    pub images: Vec<Image>,
    /// The file as it was read, whole.
    ///
    /// Everything above is a *summary*: the fields a browser needs, in the
    /// types it wants them in. This is the rest — an entry's `description`, its
    /// metadata map, its tags, a stored server metadata block, and every field
    /// a later QuPath adds that this has never heard of.
    ///
    /// It is kept because a project that is rewritten must not be a project
    /// that has been quietly emptied. Building a new index out of the summary
    /// would drop all of the above, and it would drop it *silently* — the file
    /// would still open, still list the same images, and have lost whatever
    /// someone had recorded against them. [`added`] rewrites this instead.
    ///
    /// Private, and not merely because [`crate::json`] is: what a caller can do
    /// with it is [`added`], and a `Value` on the public surface would be an
    /// invitation to edit a project field by field with nothing checking the
    /// result is still a project.
    document: crate::json::Value,
}

/// Reads a `project.qpproj`.
///
/// # Why this exists at all, given the rest of the module writes
///
/// A project browser opens projects it did not write. `DefaultProject
/// .writeProject` puts the index through Gson with pretty-printing on, so what
/// arrives is QuPath's spelling of the format rather than this module's, and
/// the two differ in ways that only reading a real one shows — see
/// [`Image::uri`] for the first of them.
///
/// # What is *not* an error here
///
/// An entry this cannot open is kept, with [`Image::slide`] saying why. QuPath
/// lists an entry whose slide has been moved and fails when it is opened, and a
/// browser that dropped the row instead would be showing a project with fewer
/// images than it has — which is the worse failure, because nothing about the
/// picture says a row is missing.
///
/// # Errors
///
/// The message names a line and a column when the file is not JSON, and says
/// which field is wrong when it is JSON that is not a project. The two
/// timestamps are required, because [`Timestamps`] is what QuPath demands
/// before it looks at anything else — a file without them is one QuPath will
/// not open, and reporting that here is more use than reading it anyway and
/// discovering later.
pub fn read(text: &str) -> Result<Stored, String> {
    let value = crate::json::parse(text)?;
    let created = value
        .get("createTimestamp")
        .and_then(crate::json::Value::as_u64)
        .ok_or("no createTimestamp: QuPath reads this one unguarded and would not open the file")?;
    let modified = value
        .get("modifyTimestamp")
        .and_then(crate::json::Value::as_u64)
        .ok_or("no modifyTimestamp: QuPath reads this one unguarded and would not open the file")?;

    // `images` absent is an empty project, which is what `loadProject` does
    // with it — `element.has("images") ? ... : Collections.emptyList()`.
    let images = match value.get("images") {
        None => Vec::new(),
        Some(listed) => {
            let listed = listed
                .as_array()
                .ok_or("`images` is not a list, so this is not a project index")?;
            listed.iter().enumerate().map(image).collect()
        }
    };

    Ok(Stored {
        version: value
            .get("version")
            .and_then(crate::json::Value::as_str)
            .map(str::to_owned),
        written: Timestamps { created, modified },
        uri: value
            .get("uri")
            .and_then(crate::json::Value::as_str)
            .map(str::to_owned),
        images,
        document: value,
    })
}

impl Stored {
    /// The index written back out, unchanged.
    ///
    /// For a file Gson wrote this is **byte for byte the file that was read**,
    /// which is the check that says nothing was lost — see
    /// `parity_project_export.rs`. It is not a formatting coincidence:
    /// [`crate::json`]'s writer copies Gson's spelling precisely so that this
    /// claim can be made as an equality rather than as an argument.
    #[must_use]
    pub fn rewritten(&self) -> String {
        crate::json::write(&self.document)
    }

    /// The id the next entry added to this project takes.
    ///
    /// The larger of `lastID` and the biggest `entryID` present, plus one,
    /// which is `DefaultProject.loadProject`'s own arithmetic — it sets the
    /// counter to `max(lastID, max(entryID))` and `incrementAndGet`s from
    /// there. Taking `lastID` alone would reuse an id in a project whose
    /// `lastID` is stale, and reusing one means two entries sharing a directory
    /// under `data/`.
    #[must_use]
    pub fn next_id(&self) -> u64 {
        let last = self
            .document
            .get("lastID")
            .and_then(crate::json::Value::as_u64)
            .unwrap_or(0);
        let highest = self.images.iter().map(|image| image.id).max().unwrap_or(0);
        last.max(highest) + 1
    }
}

/// Every file to write when `entries` are added to a project that already
/// exists.
///
/// The counterpart of [`files`], and the difference between them is the whole
/// point of this one. [`files`] writes a project from nothing, so every byte in
/// it is a byte this module chose. This writes a project **someone else may
/// have written**, so the index is [`Stored::rewritten`] with the new entries
/// appended — not rebuilt from what was read.
///
/// # What that preserves, and why rebuilding would not
///
/// An entry carries more than a name and a URI. `DefaultProjectImageEntry` has
/// a `description`, a `metadata` map that QuPath's browser lets a user edit, a
/// set of `tags`, and a `serverBuilder` that may hold a full metadata block
/// with the image's pyramid in it. None of those is in [`Image`], because a
/// browser does not need them — and a rewrite built from [`Image`] would delete
/// every one of them from every entry it did not touch. The file would still
/// open, still list the same images, and have lost what someone recorded
/// against them, which is the worst shape a bug of this kind can take.
///
/// # What it changes
///
/// `images` gains the new entries, `lastID` becomes the last id issued, and
/// `modifyTimestamp` becomes `written.modified`. `createTimestamp` is left
/// alone: the project was created when it was created. Everything else in the
/// file, at every depth, is exactly as it was read.
///
/// Updating `modifyTimestamp` is a deliberate departure from
/// `DefaultProject.writeProject`, which writes back whatever it loaded and so
/// never advances the field at all. The field means when the project was last
/// written; a rewrite that leaves it stale is reporting something untrue about
/// a file it has just changed.
#[must_use]
pub fn added(stored: &Stored, entries: &[Entry<'_>], written: Timestamps) -> Vec<Output> {
    let mut document = stored.document.clone();
    let first = stored.next_id();

    let images: Vec<crate::json::Value> = entries
        .iter()
        .enumerate()
        .map(|(offset, entry)| {
            let id = first + offset as u64;
            // Through the one definition of an entry, and back through the
            // parser. The alternative is a second way of building the same
            // object, which is the thing `entry_object` exists to prevent — and
            // this way a change to the format cannot make `files` and `added`
            // disagree, because the text is the same text.
            crate::json::parse(&entry_object(id, entry)).expect("an entry this module just wrote")
        })
        .collect();

    if let crate::json::Value::Object(fields) = &mut document {
        match fields.iter_mut().find(|(key, _)| key == "images") {
            Some((_, crate::json::Value::Array(listed))) => listed.extend(images),
            // A project with no `images` key at all is one QuPath reads as
            // empty — `element.has("images") ? ... : emptyList()` — so adding
            // the key is adding what was meant rather than inventing a field.
            _ => fields.push((
                "images".to_owned(),
                crate::json::Value::Array(images),
            )),
        }
        // Only when an id was actually issued. `first` is the id the *next*
        // entry would take, so writing it back with nothing added advances the
        // counter past an entry that does not exist — and the next append then
        // skips a number, leaving a `data/` directory nothing points at.
        if let Some(last) = entries.len().checked_sub(1) {
            set(fields, "lastID", first + last as u64);
        }
        set(fields, "modifyTimestamp", written.modified);
    }

    let mut out = vec![Output {
        path: "project.qpproj".to_owned(),
        contents: crate::json::write(&document),
    }];
    for (offset, entry) in entries.iter().enumerate() {
        let directory = (first + offset as u64).to_string();
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

/// The index rewritten with entry `id`'s metadata map replaced by `metadata`.
///
/// The third writer, and the first whose input is something a user *typed*.
/// [`files`] writes a project from nothing and [`added`] appends to one; this
/// reaches into an entry somebody else's program wrote and changes a field in
/// it. So it is the same rewrite-what-was-read shape as [`added`] — the
/// document is [`Stored`]'s own, cloned and edited at one key — rather than
/// anything built out of [`Image`]. What that preserves is what [`added`]'s
/// documentation lists, and the argument is stronger here: the entry being
/// edited is one that certainly has fields nothing in [`Image`] carries, since
/// its `serverBuilder` is how the image opens at all.
///
/// # Only the one entry, and only its `metadata`
///
/// Every other entry is untouched, including its metadata, and every other
/// field of *this* entry is untouched too. `modifyTimestamp` advances, for the
/// reason [`added`] advances it: the file has just been written, and a stale
/// field there reports something untrue.
///
/// `lastID` does not, and that is the difference from [`added`] worth naming —
/// no id is issued, because no entry is created. Advancing the counter here
/// would leave a `data/` directory nothing points at.
///
/// # An empty map removes the field
///
/// Rather than writing `"metadata": {}`. `DefaultProjectImageEntry` starts with
/// an empty `LinkedHashMap` and Gson writes it as `{}` when it is empty, so
/// both spellings read back identically — but a project whose entries never had
/// the field should not acquire one because somebody opened a metadata editor
/// and closed it again. The rule is that clearing the last key returns the
/// entry to the shape it had.
///
/// # Errors
///
/// When no entry has this `id`. A caller holding an [`Image`] cannot hit it,
/// but a caller holding an id across a re-read can — the project may have been
/// rewritten by something else in between, which is exactly the case the
/// application checks for before calling this.
pub fn edited(
    stored: &Stored,
    id: u64,
    metadata: &[(String, String)],
    written: Timestamps,
) -> Result<Vec<Output>, String> {
    let mut document = stored.document.clone();

    let crate::json::Value::Object(fields) = &mut document else {
        return Err("the project index is not an object".to_owned());
    };
    let Some((_, crate::json::Value::Array(images))) =
        fields.iter_mut().find(|(key, _)| key == "images")
    else {
        return Err(format!("no entry {id}: the project has no images"));
    };

    // By `entryID` and not by position, because the caller's list is the
    // *browser's* order — sorted, and filtered — and an index into that is an
    // index into a different list than this one. The same reasoning
    // `App::picture_for` uses for keying pictures by id.
    let found = images.iter_mut().enumerate().find(|(position, image)| {
        image
            .get("entryID")
            .and_then(crate::json::Value::as_u64)
            .unwrap_or(*position as u64 + 1)
            == id
    });
    let Some((_, crate::json::Value::Object(entry))) = found else {
        return Err(format!("no entry {id} in this project"));
    };

    let map = crate::json::Value::Object(
        metadata
            .iter()
            .map(|(key, value)| (key.clone(), crate::json::Value::String(value.clone())))
            .collect(),
    );
    // In place where the field already is, appended only when it is new, which
    // is `set`'s argument applied one level down: an entry that carried
    // `metadata` between `imageName` and `serverBuilder` keeps it there, so a
    // diff of the two files is the map that changed rather than the entry
    // reordered around it.
    match entry.iter_mut().find(|(key, _)| key == "metadata") {
        Some(_) if metadata.is_empty() => entry.retain(|(key, _)| key != "metadata"),
        Some((_, existing)) => *existing = map,
        None if metadata.is_empty() => {}
        None => entry.push(("metadata".to_owned(), map)),
    }

    if let crate::json::Value::Object(fields) = &mut document {
        set(fields, "modifyTimestamp", written.modified);
    }

    Ok(vec![Output {
        path: "project.qpproj".to_owned(),
        contents: crate::json::write(&document),
    }])
}

/// Replaces a numeric field in place, or appends it if the file has none.
///
/// In place, so a project's field order survives a rewrite. Appending a
/// `lastID` that was already there — at the end, where QuPath writes it near
/// the top — would make a diff of the two files look like a rewrite of the
/// whole thing.
fn set(fields: &mut Vec<(String, crate::json::Value)>, key: &str, value: u64) {
    let number = crate::json::Value::Number(value.to_string());
    match fields.iter_mut().find(|(name, _)| name == key) {
        Some((_, existing)) => *existing = number,
        None => fields.push((key.to_owned(), number)),
    }
}

/// One entry of the `images` array.
///
/// `position` stands in for a missing `entryID`, counting from 1 the way
/// QuPath's counter does. An entry without one is not something QuPath writes,
/// and giving every such entry id 0 would put them all in `data/0`.
fn image((position, value): (usize, &crate::json::Value)) -> Image {
    let id = value
        .get("entryID")
        .and_then(crate::json::Value::as_u64)
        .unwrap_or(position as u64 + 1);
    let name = value
        .get("imageName")
        .and_then(crate::json::Value::as_str)
        .map_or_else(|| format!("Image {id}"), str::to_owned);

    let builder = value.get("serverBuilder");
    let kind = builder
        .and_then(|builder| builder.get("builderType"))
        .and_then(crate::json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let uri = builder
        .and_then(|builder| builder.get("uri"))
        .and_then(crate::json::Value::as_str)
        .map(str::to_owned);

    let slide = if kind == BUILDER_TYPE {
        match &uri {
            Some(uri) => uri_path(uri),
            None => Err("the entry has no uri".to_owned()),
        }
    } else if kind.is_empty() {
        Err("the entry has no serverBuilder".to_owned())
    } else {
        Err(format!(
            "the image is built by a `{kind}` transform, which this reads but cannot open"
        ))
    };

    // `value.get`, and pointedly not a search: the entry's map is the one at
    // the top level of the entry. `serverBuilder` carries a `metadata` of its
    // own describing the pixels, and reading that one here would hand a browser
    // `width` and `height` as though a user had typed them.
    let metadata = value
        .get("metadata")
        .and_then(crate::json::Value::as_object)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();

    let tags = value
        .get("tags")
        .and_then(crate::json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(crate::json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    Image {
        id,
        name,
        builder: kind,
        slide,
        uri,
        description: value
            .get("description")
            .and_then(crate::json::Value::as_str)
            .map(str::to_owned),
        metadata,
        tags,
        randomized_name: value
            .get("randomizedName")
            .and_then(crate::json::Value::as_str)
            .map(str::to_owned),
    }
}

/// The path a `file:` URI names — the inverse of [`file_uri`], and QuPath's
/// `new File(uri)`.
///
/// # Both spellings, because QuPath writes the one this does not
///
/// `File.toURI()` produces `file:/data/x.svs`: a `file:` URI with **no**
/// authority at all, which is what Java's `URI` constructor makes of a null
/// authority. [`file_uri`] writes `file:///data/x.svs`, an *empty* authority,
/// which is what RFC 8089 recommends and what `ProjectImport` confirms QuPath
/// opens. Java parses both to `authority == null`, so they are the same URI to
/// the reader on the other side — and a reader here that accepted only the
/// spelling this module writes would fail on **every project QuPath wrote**,
/// which is the entire population a browser exists to open.
///
/// # A host is refused, and that includes `localhost`
///
/// `file://data/x.svs` names the host `data`, and `new File(URI)` throws
/// `IllegalArgumentException: URI has an authority component` for it — and for
/// `file://localhost/data/x.svs` too, which RFC 8089 does treat as equivalent
/// to no host. This refuses exactly what QuPath refuses, so an entry that would
/// fail to open there fails to open here, with a message that says why rather
/// than a path pointing at the wrong place.
///
/// # Errors
///
/// A message naming what is wrong with the URI, written to be shown next to
/// the entry it belongs to.
pub fn uri_path(uri: &str) -> Result<PathBuf, String> {
    let rest = uri
        .strip_prefix("file:")
        .ok_or_else(|| format!("`{uri}` is not a file: URI"))?;
    // A query or a fragment is not something `File.toURI()` writes, and
    // `new File(URI)` refuses both. Checked before the authority so that a URI
    // with several problems reports the first one a reader would meet.
    if let Some(at) = rest.find(['?', '#']) {
        let what = if rest.as_bytes()[at] == b'?' { "query" } else { "fragment" };
        return Err(format!("`{uri}` has a {what} component, so it does not name a file"));
    }
    let path = match rest.strip_prefix("//") {
        // `file://...`: an authority runs to the next `/`. Empty is the
        // `file:///path` spelling and names no host.
        Some(authority) => {
            let end = authority.find('/').unwrap_or(authority.len());
            let (host, path) = authority.split_at(end);
            if !host.is_empty() {
                return Err(format!(
                    "`{uri}` names the host `{host}`, so the slide is on another machine"
                ));
            }
            path
        }
        // `file:/path`, which is what `File.toURI()` writes.
        None => rest,
    };
    if !path.starts_with('/') {
        return Err(format!("`{uri}` is a relative URI and names no file"));
    }
    let decoded = percent_decode(path).ok_or_else(|| format!("`{uri}` is not valid UTF-8"))?;
    if decoded.contains('\0') {
        return Err(format!("`{uri}` decodes to a path with a NUL in it"));
    }
    Ok(PathBuf::from(decoded))
}

/// Resolves `%XX` escapes, or `None` if what they spell is not UTF-8.
///
/// The escapes are decoded to *bytes* and the whole run is validated at the
/// end, which is the only order that works: a character outside ASCII is
/// several escapes, and each byte of it is invalid UTF-8 on its own.
fn percent_decode(text: &str) -> Option<String> {
    if !text.contains('%') {
        return Some(text.to_owned());
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let digits = bytes.get(at + 1..at + 3)?;
            let text = std::str::from_utf8(digits).ok()?;
            out.push(u8::from_str_radix(text, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// One run of a name, as `GeneralTools.StringPartsSorter` splits it.
///
/// The split is on `(\d+)`, so a name is an alternating-ish sequence of digit
/// runs and everything else. What matters for the ordering is that the two
/// kinds compare differently, and that a digit run compares by *value*.
#[derive(Debug, PartialEq, Eq)]
enum Part<'a> {
    /// A run of digits, with its leading zeros stripped.
    ///
    /// Stripped because QuPath parses the run into a `BigDecimal`, and
    /// `BigDecimal.compareTo` ignores scale: `007` and `7` compare **equal**,
    /// and the tie is then broken by the number of parts rather than by the
    /// text. Keeping the run as it was written would order them, which is the
    /// opposite of what QuPath does. `""` is the value zero, from a run that
    /// was all zeros — `0` and `000` are the same number and `0` prints as one
    /// digit.
    Number(&'a str),
    /// Everything between two digit runs, exactly as written.
    Text(&'a str),
}

impl Part<'_> {
    /// What Java's `toString` gives, which is what a mixed pair compares on.
    ///
    /// `BigDecimal("007").toString()` is `"7"` — the leading zeros are gone by
    /// the time a number is compared against text, so this returns the
    /// stripped run and not the original one. A number whose run was all zeros
    /// prints as `0`.
    fn as_text(&self) -> &str {
        match self {
            Part::Number("") => "0",
            Part::Number(digits) | Part::Text(digits) => digits,
        }
    }
}

/// Splits a name into digit runs and the text between them.
fn parts(name: &str) -> Vec<Part<'_>> {
    let mut parts = Vec::new();
    let bytes = name.as_bytes();
    let mut next = 0;
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        if start > next {
            parts.push(Part::Text(&name[next..start]));
        }
        // Leading zeros go here rather than at the comparison, so that
        // `as_text` and the numeric compare agree about what the run is.
        parts.push(Part::Number(name[start..at].trim_start_matches('0')));
        next = at;
    }
    if next < name.len() {
        parts.push(Part::Text(&name[next..]));
    }
    parts
}

/// QuPath's `GeneralTools.smartStringComparator`, over two names.
///
/// A plain string sort puts `a10` before `a2`, because `1` is less than `2`.
/// This one splits each name into digit runs and the text between them and
/// compares the runs by value, so `["a1", "a2", "a10"]` stays as it is. QuPath
/// sorts the project browser's groups with it, and this is the browser's row
/// order.
///
/// Three things about it are worth stating, because all three are visible in a
/// list of slide names and none of them is what a reader would assume:
///
/// - **It is case-sensitive, and it compares UTF-16.** The comparison is
///   `String.compareTo`, not the case-insensitive one the search box uses, so
///   every capital sorts before every lower-case letter: `Block` before `abc`.
///   The search box in the same pane lowercases both sides, and it is the same
///   pane and the same names — the two controls really do disagree, in QuPath
///   as here. The UTF-16 half is [`compare_utf16`], and it is not a detail: a
///   byte comparison puts `\u{1f600}` on the wrong side of `\u{e000}`.
/// - **`007` and `7` compare equal**, because the run becomes a `BigDecimal`
///   and scale is ignored. The tie falls through to the part count.
/// - **The last tiebreak is the number of parts, not the length.** `a1` has two
///   parts and `a1b1` has four, so the shorter name wins — but so does `a1`
///   against `a1bbbbbbbb`, which also has two parts and is longer. The parts
///   before it decided nothing, so the *count* decides.
///
/// A digit run compares against a text part by what Java's `toString` gives for
/// it, which is the run with its leading zeros already gone.
pub fn smart_string_compare(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (left, right) = (parts(left), parts(right));
    for (one, two) in left.iter().zip(right.iter()) {
        let ordering = match (one, two) {
            // Both numbers: by value. Non-negative integers with no leading
            // zeros order by length first and then lexicographically, which is
            // `BigDecimal.compareTo` without the parse — and without a cap on
            // how many digits a name is allowed to have.
            (Part::Number(one), Part::Number(two)) => {
                one.len().cmp(&two.len()).then_with(|| one.cmp(two))
            }
            // Anything else: Java compares the two `toString`s, so a number
            // meeting text is compared as the digits it prints as.
            _ => compare_utf16(one.as_text(), two.as_text()),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

/// Two strings in the order `String.compareTo` puts them.
///
/// **Not `str::cmp`, and the difference is not theoretical.** Java compares
/// UTF-16 code units; Rust compares UTF-8 bytes. The two orders agree for
/// every character in the basic multilingual plane and disagree above it: a
/// character from `U+10000` up is a *surrogate pair* in Java, and both halves
/// live in `U+D800..U+E000` — below every character from `U+E000` to `U+FFFF`.
/// So Java puts `😀` before `\u{e000}` and a byte comparison puts it after.
/// `parity/fixtures/SmartStringSort.json` records QuPath doing exactly that,
/// and it is the only line of the fixture the byte order got wrong.
///
/// A slide named with an emoji is not the reason to carry this. The reason is
/// that the order is *QuPath's*, and a browser whose rows are in a different
/// order from QuPath's for any name is a browser that has to be checked name by
/// name before it is believed.
fn compare_utf16(left: &str, right: &str) -> std::cmp::Ordering {
    // No allocation: `encode_utf16` yields the code units one at a time, and
    // comparing two iterators lexicographically is what `String.compareTo`
    // does — first difference wins, and a prefix loses to what extends it.
    left.encode_utf16().cmp(right.encode_utf16())
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

    /// The project the metadata tests edit: QuPath's own, with three entries
    /// whose `metadata` is present and empty. Present matters — it is the case
    /// where the field has a *place* in the entry already.
    const QUPATH_PROJECT: &str = include_str!("../../../parity/fixtures/ProjectExport.json");

    fn pairs(from: &[(&str, &str)]) -> Vec<(String, String)> {
        from.iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn metadata_of(text: &str, id: u64) -> Vec<(String, String)> {
        let stored = read(text).expect("a project");
        stored
            .images
            .iter()
            .find(|image| image.id == id)
            .expect("the entry")
            .metadata
            .clone()
    }

    #[test]
    fn an_edit_reads_back_as_what_was_typed() {
        let stored = read(QUPATH_PROJECT).expect("QuPath's own project file");
        let out = edited(
            &stored,
            2,
            &pairs(&[("stain", "H&E"), ("case", "17")]),
            written(),
        )
        .expect("entry 2 is in this project");

        assert_eq!(out.len(), 1, "an edit writes the index and nothing else");
        assert_eq!(out[0].path, "project.qpproj");
        assert_eq!(
            metadata_of(&out[0].contents, 2),
            pairs(&[("stain", "H&E"), ("case", "17")]),
            "the keys read back in the order they were given"
        );
    }

    /// The point of rewriting the document rather than rebuilding it. An edit
    /// to one entry must be invisible in every other entry, and in every other
    /// field of the one edited.
    #[test]
    fn an_edit_changes_one_entry_and_one_field() {
        let stored = read(QUPATH_PROJECT).expect("QuPath's own project file");
        let out = edited(&stored, 2, &pairs(&[("stain", "H&E")]), written()).expect("entry 2");
        let after = read(&out[0].contents).expect("the edited project reads back");

        assert_eq!(after.images.len(), stored.images.len());
        for (before, after) in stored.images.iter().zip(&after.images) {
            assert_eq!(before.id, after.id);
            assert_eq!(before.name, after.name);
            assert_eq!(before.uri, after.uri);
            assert_eq!(before.slide, after.slide);
            assert_eq!(before.tags, after.tags, "tags survive an edit to metadata");
            assert_eq!(before.randomized_name, after.randomized_name);
            if after.id != 2 {
                assert_eq!(
                    before.metadata, after.metadata,
                    "entry {} was not the one edited",
                    after.id
                );
            }
        }
        // The counter does not move: no entry was created, so no id was issued.
        assert_eq!(after.next_id(), stored.next_id());
        assert_eq!(after.written.created, stored.written.created);
        assert_eq!(after.written.modified, written().modified);
    }

    /// `serverBuilder` is how the image opens at all, and it is the field a
    /// rewrite built from [`Image`] would drop. Checked as text, because that
    /// is the level at which it would go missing.
    #[test]
    fn an_edit_keeps_what_the_reader_does_not_summarise() {
        let stored = read(QUPATH_PROJECT).expect("QuPath's own project file");
        let out = edited(&stored, 1, &pairs(&[("k", "v")]), written()).expect("entry 1");

        for keep in [
            "providerClassName",
            "qupath.lib.images.servers.openslide.OpenslideServerBuilder",
            "\"args\"",
            "randomized-name",
        ] {
            assert!(
                out[0].contents.contains(keep),
                "an edit dropped {keep} from the document it rewrote"
            );
        }
    }

    /// Clearing the last key returns the entry to the shape it had, rather than
    /// leaving `"metadata": {}` behind — and for this fixture, whose entries
    /// carry an empty map already, "the shape it had" is that empty map.
    #[test]
    fn clearing_every_key_returns_the_entry_to_its_own_shape() {
        let stored = read(QUPATH_PROJECT).expect("QuPath's own project file");
        let filled = edited(&stored, 3, &pairs(&[("a", "1")]), written()).expect("entry 3");
        let reread = read(&filled[0].contents).expect("reads back");
        let cleared = edited(&reread, 3, &[], written()).expect("entry 3 again");

        assert!(metadata_of(&cleared[0].contents, 3).is_empty());
        // Down to the byte, against the file that was never edited at all —
        // which is the claim that "cleared" and "never written" are the same
        // document rather than merely the same reading. Only the timestamp is
        // allowed to differ, so the comparison is against a no-op edit of the
        // original, which moves that field and nothing else.
        let untouched = edited(&stored, 3, &[], written()).expect("a no-op edit");
        assert_eq!(
            cleared[0].contents, untouched[0].contents,
            "clearing the keys left the document different from never having set them"
        );
    }

    /// Editing the same entry twice is the gesture a user actually makes — type
    /// a key, come back and change it. The second edit must replace the map
    /// rather than accumulate against it, and must not move the field.
    #[test]
    fn editing_twice_replaces_rather_than_accumulates() {
        let stored = read(QUPATH_PROJECT).expect("QuPath's own project file");
        let once = edited(&stored, 2, &pairs(&[("stain", "H&E")]), written()).expect("entry 2");
        let reread = read(&once[0].contents).expect("reads back");
        let twice = edited(&reread, 2, &pairs(&[("stain", "PAS")]), written()).expect("entry 2");

        assert_eq!(
            metadata_of(&twice[0].contents, 2),
            pairs(&[("stain", "PAS")])
        );
        assert_eq!(
            read(&twice[0].contents).expect("reads back").images.len(),
            stored.images.len(),
            "a second edit did not add an entry"
        );
    }

    /// The field keeps its place in the entry. QuPath writes `metadata` between
    /// `imageName` and `tags`; an edit that appended it instead would leave a
    /// file that reads the same and diffs as the entry rewritten.
    #[test]
    fn an_edit_leaves_the_field_where_qupath_put_it() {
        let stored = read(QUPATH_PROJECT).expect("QuPath's own project file");
        let out = edited(&stored, 1, &pairs(&[("k", "v")]), written()).expect("entry 1");
        let text = &out[0].contents;

        let name = text.find("\"imageName\"").expect("an imageName");
        let metadata = text.find("\"metadata\"").expect("a metadata map");
        let tags = text.find("\"tags\"").expect("tags");
        assert!(
            name < metadata && metadata < tags,
            "metadata moved out from between imageName and tags"
        );
    }

    /// An id the project does not have is the case a caller holding one across
    /// a re-read can reach, and it is refused rather than written somewhere.
    #[test]
    fn an_edit_to_an_absent_entry_is_refused() {
        let stored = read(QUPATH_PROJECT).expect("QuPath's own project file");
        let error = edited(&stored, 99, &pairs(&[("k", "v")]), written())
            .expect_err("entry 99 is not in this project");
        assert!(error.contains("99"), "the message names the entry: {error}");
    }

    /// A value with the characters that end a JSON string in it. The writer
    /// escapes through the same path every other string here does, and the
    /// check is that it survives the round trip rather than that it is spelled
    /// any particular way.
    #[test]
    fn a_value_that_needs_escaping_survives() {
        let stored = read(QUPATH_PROJECT).expect("QuPath's own project file");
        let awkward = "a \"quoted\" \\ value\twith\nlines";
        let out = edited(&stored, 1, &pairs(&[("note", awkward)]), written()).expect("entry 1");

        assert_eq!(
            metadata_of(&out[0].contents, 1),
            pairs(&[("note", awkward)])
        );
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

    /// What is written comes back — and what does not is the point.
    ///
    /// The dimensions are gone, because they were only ever in the metadata
    /// block that is not written. A browser learns a name and a path from the
    /// project and everything about the pixels from opening the slide, which is
    /// the division `docs/VIEWER.md` §8.3 settled on.
    #[test]
    fn a_project_this_module_wrote_reads_back() {
        let text = project_file(Path::new("/study/project.qpproj"), &[entry()], written());
        let stored = read(&text).expect("a project this module wrote");

        assert_eq!(stored.version.as_deref(), Some(FORMAT_VERSION));
        assert_eq!(stored.written, written());
        assert_eq!(stored.uri.as_deref(), Some("file:///study/project.qpproj"));

        assert_eq!(stored.images.len(), 1);
        let image = &stored.images[0];
        assert_eq!(image.id, 1);
        assert_eq!(image.name, "CMU-1.svs");
        assert_eq!(image.builder, BUILDER_TYPE);
        assert_eq!(image.slide.as_deref(), Ok(Path::new("/data/CMU-1.svs")));
        assert_eq!(image.uri.as_deref(), Some("file:///data/CMU-1.svs"));
    }

    /// `entryID` names the directory, so two entries must not read as one.
    #[test]
    fn every_entry_keeps_its_own_id_and_path() {
        let second = Entry {
            path: Path::new("/data/other.svs"),
            name: "Other",
            width: 10,
            height: 20,
        };
        let text = project_file(Path::new("/p.qpproj"), &[entry(), second], written());
        let stored = read(&text).expect("two entries");
        let ids: Vec<u64> = stored.images.iter().map(|image| image.id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(stored.images[1].name, "Other");
        assert_eq!(stored.images[1].slide.as_deref(), Ok(Path::new("/data/other.svs")));
    }

    /// **The spelling QuPath writes is not the spelling this writes.**
    ///
    /// `File.toURI()` produces `file:/data/x.svs` — no authority at all — and
    /// [`file_uri`] produces `file:///data/x.svs`, an empty one. Java reads
    /// both as the same URI, which is why writing the second has always worked;
    /// a reader that accepted only the second would fail on every project
    /// QuPath itself wrote, which is the whole population a browser opens.
    #[test]
    fn both_spellings_of_a_file_uri_name_the_same_path() {
        assert_eq!(uri_path("file:/data/x.svs"), Ok(PathBuf::from("/data/x.svs")));
        assert_eq!(uri_path("file:///data/x.svs"), Ok(PathBuf::from("/data/x.svs")));
        // And what this module writes survives its own round trip.
        let uri = file_uri(Path::new("/data/CMU-1.svs"));
        assert_eq!(uri_path(&uri), Ok(PathBuf::from("/data/CMU-1.svs")));
    }

    /// A host is a slide on another machine, and QuPath refuses it — including
    /// `localhost`, which RFC 8089 says means no host.
    ///
    /// `new File(URI)` throws `IllegalArgumentException: URI has an authority
    /// component` for both, so refusing exactly these is refusing what QuPath
    /// refuses. Reading it as a local path instead would open the wrong file
    /// with no complaint: `file://data/CMU-1.svs` would become `/CMU-1.svs`.
    #[test]
    fn a_uri_naming_a_host_is_refused_rather_than_read_as_local() {
        let error = uri_path("file://data/CMU-1.svs").expect_err("a host");
        assert!(error.contains("host `data`"), "{error}");
        let error = uri_path("file://localhost/data/x.svs").expect_err("a host");
        assert!(error.contains("host `localhost`"), "{error}");
    }

    #[test]
    fn what_is_not_a_path_is_said_rather_than_guessed() {
        for (uri, expected) in [
            ("https://example.org/x.svs", "not a file: URI"),
            ("/data/x.svs", "not a file: URI"),
            ("file:data/x.svs", "relative URI"),
            ("file:///data/x.svs?v=2", "query"),
            ("file:///data/x.svs#page", "fragment"),
            ("file:///data/%FF.svs", "not valid UTF-8"),
            ("file:///data/%00.svs", "NUL"),
        ] {
            let error = uri_path(uri).expect_err(uri);
            assert!(error.contains(expected), "{uri}: {error}");
        }
    }

    /// Percent-escapes are decoded as bytes and validated as a whole, because
    /// a character outside ASCII is several escapes and no one of them is
    /// UTF-8 by itself.
    #[test]
    fn percent_escapes_come_back_as_the_path_that_was_encoded() {
        assert_eq!(
            uri_path("file:///My%20Slides/a%20b.svs"),
            Ok(PathBuf::from("/My Slides/a b.svs"))
        );
        // Three bytes, three escapes, one character.
        assert_eq!(
            uri_path("file:///data/%E2%80%94.svs"),
            Ok(PathBuf::from("/data/—.svs"))
        );
        let round = file_uri(Path::new("/études/CMU 1.svs"));
        assert_eq!(uri_path(&round), Ok(PathBuf::from("/études/CMU 1.svs")));
    }

    /// The two fields QuPath reads before anything else. A file without them
    /// is one QuPath will not open, so reading it here anyway would be
    /// reporting a project that does not exist.
    #[test]
    fn a_project_without_the_timestamps_is_refused_here_too() {
        let error = read(r#"{"version": "0.6.0", "images": []}"#).expect_err("no timestamps");
        assert!(error.contains("createTimestamp"), "{error}");
        let error = read(r#"{"createTimestamp": 1, "images": []}"#).expect_err("one timestamp");
        assert!(error.contains("modifyTimestamp"), "{error}");
    }

    /// An entry QuPath can open and this cannot is **listed**, not dropped.
    ///
    /// QuPath registers sixteen builders that wrap another builder rather than
    /// naming a file — `rotated`, `cropped`, `sparse` and the rest. A browser
    /// that omitted those rows would show a project with fewer images than it
    /// has, and nothing about the picture would say so.
    #[test]
    fn an_image_this_cannot_open_is_still_an_image() {
        let stored = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2, "images": [
                 {"entryID": 7, "imageName": "Rotated",
                  "serverBuilder": {"builderType": "rotated", "rotation": "ROTATE_90"}}]}"#,
        )
        .expect("a project");
        assert_eq!(stored.images.len(), 1);
        let image = &stored.images[0];
        assert_eq!(image.id, 7);
        assert_eq!(image.name, "Rotated");
        assert_eq!(image.builder, "rotated");
        let error = image.slide.as_ref().expect_err("no path");
        assert!(error.contains("`rotated`"), "{error}");
        assert_eq!(image.uri, None);
    }

    /// QuPath names an entry `Image <entryID>` when it has none, in
    /// `DefaultProjectImageEntry`'s constructor. A blank row here would be a
    /// different project from the one QuPath shows.
    #[test]
    fn an_entry_with_no_name_gets_the_name_qupath_gives_it() {
        let stored = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2,
                "images": [{"entryID": 4, "serverBuilder": {"builderType": "uri",
                            "uri": "file:/data/x.svs"}}]}"#,
        )
        .expect("a project");
        assert_eq!(stored.images[0].name, "Image 4");
        assert_eq!(stored.images[0].slide.as_deref(), Ok(Path::new("/data/x.svs")));
    }

    /// The reason [`crate::json`] is a parser: a `serverBuilder` QuPath wrote
    /// carries a metadata block, and the first `"uri"` in the text is not the
    /// entry's.
    ///
    /// Both are strings and both look like paths, so a reader that took the
    /// first would open the wrong slide and say nothing.
    #[test]
    fn a_uri_inside_a_metadata_block_is_not_the_entrys_uri() {
        let stored = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2, "images": [
                 {"entryID": 1, "imageName": "A", "serverBuilder": {
                    "builderType": "uri",
                    "metadata": {"uri": "file:/data/wrong.svs", "width": 46000,
                                 "levels": [{"downsample": 1.0}]},
                    "uri": "file:/data/right.svs"}}]}"#,
        )
        .expect("a project");
        assert_eq!(
            stored.images[0].slide.as_deref(),
            Ok(Path::new("/data/right.svs"))
        );
    }

    /// The same trap one level along: an entry has a `metadata` map and its
    /// `serverBuilder` has a `metadata` block, and they are different things.
    ///
    /// The entry's is `Map<String, String>` a user typed into the browser. The
    /// builder's describes the pixels — `width`, `levels`, `isRGB` — and is the
    /// block [`server_builder`] refuses to write. A reader that found the wrong
    /// one would offer `width=46000` as a metadata key somebody could sort a
    /// study by, and it would look entirely plausible.
    #[test]
    fn a_builders_metadata_block_is_not_the_entrys_metadata() {
        let stored = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2, "images": [
                 {"entryID": 1, "imageName": "A",
                  "metadata": {"case": "17B"},
                  "serverBuilder": {
                    "builderType": "uri",
                    "uri": "file:/data/right.svs",
                    "metadata": {"width": 46000, "isRGB": true}}}]}"#,
        )
        .expect("a project");
        assert_eq!(
            stored.images[0].metadata,
            vec![(String::from("case"), String::from("17B"))],
            "the builder's block was read as the entry's map"
        );
    }

    /// The map keeps the file's order, because Gson wrote a `LinkedHashMap`
    /// and that order is the one somebody entered the keys in.
    ///
    /// Written with keys that are *not* in alphabetical order, so that a reader
    /// which collected them into a `BTreeMap` — the obvious choice, and the
    /// wrong one — fails here rather than passing and re-alphabetising every
    /// study quietly.
    #[test]
    fn the_metadata_map_is_in_the_files_order() {
        let stored = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2, "images": [
                 {"entryID": 1, "imageName": "A",
                  "metadata": {"stain": "H&E", "case": "17B", "block": "2"},
                  "serverBuilder": {"builderType": "uri", "uri": "file:/x.svs"}}]}"#,
        )
        .expect("a project");
        let keys: Vec<&str> = stored.images[0]
            .metadata
            .iter()
            .map(|(key, _)| key.as_str())
            .collect();
        assert_eq!(keys, ["stain", "case", "block"]);
    }

    /// A value that is not a string is dropped, and the rest of the map
    /// survives.
    ///
    /// The Java field is `Map<String, String>`, so a number there is a file
    /// QuPath's own Gson would refuse. Dropping the one pair rather than
    /// failing the whole project is the same judgement [`read`] makes about an
    /// entry it cannot open: the row is still worth showing, and a study is not
    /// worth refusing over one key.
    #[test]
    fn a_metadata_value_that_is_not_a_string_is_dropped() {
        let stored = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2, "images": [
                 {"entryID": 1, "imageName": "A",
                  "metadata": {"case": "17B", "count": 12, "stain": "H&E"},
                  "serverBuilder": {"builderType": "uri", "uri": "file:/x.svs"}}]}"#,
        )
        .expect("a project");
        assert_eq!(
            stored.images[0].metadata,
            vec![
                (String::from("case"), String::from("17B")),
                (String::from("stain"), String::from("H&E")),
            ]
        );
    }

    /// The `randomizedName` is read, and what this module writes into it is
    /// **not** a mask.
    ///
    /// The second half is the point of the test. `placeholder_name` is
    /// `entry-<id>` so that the same project written twice is the same bytes,
    /// and that is the right trade for a field nothing was reading — but it
    /// makes the masked name a spelling of the entry id. A study masked with
    /// these is one where the rows can be put back in order by reading them,
    /// which is what masking exists to prevent. Asserted here so that anything
    /// offering to mask names has to meet this first.
    #[test]
    fn the_randomized_name_is_read_and_this_modules_own_is_not_random() {
        let written = project_file(
            Path::new("/study/project.qpproj"),
            &[
                Entry {
                    path: Path::new("/slides/a.svs"),
                    name: "A",
                    width: 1,
                    height: 1,
                },
                Entry {
                    path: Path::new("/slides/b.svs"),
                    name: "B",
                    width: 1,
                    height: 1,
                },
            ],
            Timestamps { created: 1, modified: 2 },
        );
        let stored = read(&written).expect("a project");
        let names: Vec<&str> = stored
            .images
            .iter()
            .map(|image| image.randomized_name.as_deref().expect("a randomized name"))
            .collect();
        assert_eq!(names, ["entry-1", "entry-2"]);
        for (image, name) in stored.images.iter().zip(&names) {
            assert!(
                name.contains(&image.id.to_string()),
                "the placeholder no longer names the id, and the warning on \
                 `Image::randomized_name` needs rewriting rather than deleting"
            );
        }
    }

    /// The description and the tags, which QuPath writes beside the map.
    ///
    /// An entry with neither reads as `None` and an empty list rather than as
    /// an error: `DefaultProjectImageEntry` leaves `description` null until
    /// somebody sets one, and Gson omits a null.
    #[test]
    fn the_description_and_tags_are_read_and_may_be_absent() {
        let stored = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2, "images": [
                 {"entryID": 1, "imageName": "A", "description": "control arm",
                  "tags": ["control", "batch 3"],
                  "serverBuilder": {"builderType": "uri", "uri": "file:/a.svs"}},
                 {"entryID": 2, "imageName": "B",
                  "serverBuilder": {"builderType": "uri", "uri": "file:/b.svs"}}]}"#,
        )
        .expect("a project");
        assert_eq!(stored.images[0].description.as_deref(), Some("control arm"));
        assert_eq!(stored.images[0].tags, ["control", "batch 3"]);
        assert_eq!(stored.images[1].description, None);
        assert!(stored.images[1].tags.is_empty());
    }

    /// An empty project reads as an empty project, and so does one whose
    /// `images` key is missing — which is what `loadProject` does with it.
    #[test]
    fn an_empty_project_reads_as_empty_rather_than_failing() {
        let text = project_file(Path::new("/study/project.qpproj"), &[], written());
        assert!(read(&text).expect("a project").images.is_empty());
        let bare = read(r#"{"createTimestamp": 1, "modifyTimestamp": 2}"#).expect("a project");
        assert!(bare.images.is_empty());
        assert_eq!(bare.version, None);
        assert_eq!(bare.uri, None);
    }

    /// A project written here, read, added to and read again.
    #[test]
    fn adding_an_entry_continues_the_project() {
        let text = project_file(Path::new("/study/project.qpproj"), &[entry()], written());
        let stored = read(&text).expect("a project");
        assert_eq!(stored.next_id(), 2);

        let extra = Entry {
            path: Path::new("/data/second.svs"),
            name: "Second",
            width: 100,
            height: 200,
        };
        let later = Timestamps {
            created: written().created,
            modified: 1_800_000_000_000,
        };
        let files = added(&stored, &[extra], later);

        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "project.qpproj",
                "data/2/server.json",
                "data/2/summary.json",
            ],
            "an appended entry's files must go in its own directory"
        );

        let reread = read(&files[0].contents).expect("the rewritten project");
        assert_eq!(reread.images.len(), 2);
        assert_eq!(reread.images[0].name, entry().name, "the first entry changed");
        assert_eq!(reread.images[1].id, 2);
        assert_eq!(reread.images[1].name, "Second");
        assert_eq!(
            reread.images[1].slide.as_deref(),
            Ok(Path::new("/data/second.svs"))
        );
        assert_eq!(reread.written.created, written().created);
        assert_eq!(reread.written.modified, later.modified);
    }

    /// A rewrite with nothing to add changes the clock and nothing else.
    ///
    /// **`lastID` in particular.** It is the id the *next* entry takes, so
    /// advancing it with nothing added skips a number, and the entry after that
    /// lands in a `data/` directory one further along than the project says.
    /// The first version of `added` did exactly that, and no test that added
    /// something could have seen it.
    #[test]
    fn a_rewrite_that_adds_nothing_changes_nothing_but_the_clock() {
        let text = project_file(Path::new("/study/project.qpproj"), &[entry()], written());
        let stored = read(&text).expect("a project");
        assert_eq!(stored.rewritten(), stored.rewritten(), "the writer is stable");

        let later = Timestamps {
            created: written().created,
            modified: 1_800_000_000_000,
        };
        let files = added(&stored, &[], later);
        assert_eq!(files.len(), 1, "no entries means no entry directories");
        assert_eq!(
            files[0].contents,
            stored
                .rewritten()
                .replace("\"modifyTimestamp\": 1700000060000", "\"modifyTimestamp\": 1800000000000"),
            "a rewrite that added nothing changed something other than the clock",
        );
        // And the counter is still where it was, so the next entry is still 2.
        assert_eq!(read(&files[0].contents).expect("a project").next_id(), 2);
    }

    /// `lastID` is what QuPath counts from, and a stale one must not reissue an
    /// id — two entries sharing a directory under `data/` is one overwriting
    /// the other.
    #[test]
    fn a_new_id_clears_both_the_counter_and_the_entries() {
        // `lastID` behind the entries: QuPath takes the larger, and so does
        // this. Reading `lastID` alone would hand out 2, which entry 7 has
        // already used.
        let behind = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2, "lastID": 1,
                "images": [{"entryID": 7, "serverBuilder": {"builderType": "uri",
                            "uri": "file:/data/x.svs"}}]}"#,
        )
        .expect("a project");
        assert_eq!(behind.next_id(), 8);

        // `lastID` ahead of them, which is a project an entry has been removed
        // from. The counter is what QuPath keeps, so it wins.
        let ahead = read(r#"{"createTimestamp": 1, "modifyTimestamp": 2, "lastID": 9}"#)
            .expect("a project");
        assert_eq!(ahead.next_id(), 10);

        // Neither: the first entry of an empty project is 1.
        let bare = read(r#"{"createTimestamp": 1, "modifyTimestamp": 2}"#).expect("a project");
        assert_eq!(bare.next_id(), 1);
    }

    /// A project whose file has no `images` key is one QuPath reads as empty,
    /// so adding the key is writing down what was meant.
    #[test]
    fn adding_to_a_project_with_no_images_key_creates_one() {
        let stored = read(r#"{"createTimestamp": 1, "modifyTimestamp": 2}"#).expect("a project");
        let files = added(&stored, &[entry()], written());
        let reread = read(&files[0].contents).expect("the rewritten project");
        assert_eq!(reread.images.len(), 1);
        assert_eq!(reread.images[0].id, 1);
        assert!(files.iter().any(|file| file.path == "data/1/server.json"));
    }

    /// Fields QuPath has and this does not interpret survive a rewrite.
    ///
    /// The unit half of `parity_project_export.rs`'s claim, on the fields that
    /// fixture does not happen to carry: an entry's `description`, its
    /// `metadata` map and its `tags` are all things QuPath's browser lets a
    /// user set, and all things `Image` does not report.
    #[test]
    fn what_the_reader_does_not_interpret_is_not_deleted() {
        let stored = read(
            r#"{"createTimestamp": 1, "modifyTimestamp": 2, "lastID": 1,
                "somethingNewer": {"deep": [1, 2]},
                "images": [{"entryID": 1, "imageName": "A",
                            "description": "cut on Tuesday",
                            "metadata": {"case": "17B"}, "tags": ["control"],
                            "serverBuilder": {"builderType": "uri",
                                              "uri": "file:/data/x.svs"}}]}"#,
        )
        .expect("a project");
        let rewritten = added(&stored, &[entry()], written())
            .into_iter()
            .next()
            .expect("the index")
            .contents;
        for kept in [
            "\"description\": \"cut on Tuesday\"",
            "\"case\": \"17B\"",
            "\"control\"",
            "\"somethingNewer\"",
        ] {
            assert!(rewritten.contains(kept), "{kept} was deleted by the rewrite");
        }
    }


    /// The tiebreak is a part count, and that is not a length.
    ///
    /// `parity/fixtures/SmartStringSort.json` pins the order of the whole list
    /// and so covers this by accident. Stated here because it is the rule the
    /// last line of the comparator implements, and a port that reached for
    /// `len()` — the obvious thing, and what a string sort would do — passes
    /// the first of these and fails the second.
    #[test]
    fn a_shorter_name_wins_on_parts_and_not_on_length() {
        use std::cmp::Ordering::Less;
        // Two parts against four: `a1` runs out first and wins.
        assert_eq!(smart_string_compare("a1", "a1b1"), Less);
        // Two parts against two, and the longer name wins on its own text —
        // nothing has run out, so the count never decides.
        assert_eq!(smart_string_compare("a1", "a1bbbbbbbb"), Less);
        // The same pair with the tail made to lose on text instead, so that
        // the two rules disagree and the text one is seen to be the one used.
        assert_eq!(smart_string_compare("ab1", "aa1bbbbbbbb"), std::cmp::Ordering::Greater);
    }

    /// A digit run meets a text part as the digits it *prints* as.
    ///
    /// Java compares the two `toString`s, and `new BigDecimal("007")` prints
    /// as `7`. So `007` against `-` compares `7` against `-`, not `007`
    /// against `-`, and a port that kept the run as written orders the pair
    /// the other way — `0` is below `-` in both encodings and `7` is above it.
    #[test]
    fn a_number_meeting_text_compares_as_the_digits_it_prints_as() {
        use std::cmp::Ordering::Greater;
        assert_eq!(smart_string_compare("007", "-"), Greater);
        assert_eq!(smart_string_compare("7", "-"), Greater);
        // And an all-zero run prints as one `0`, not as nothing: a part that
        // stripped to `""` would compare below every text part there is.
        assert_eq!(smart_string_compare("000", "-"), Greater);
    }

    /// A digit run longer than any integer type is compared, not parsed.
    ///
    /// `BigDecimal` has no width, so a name with forty digits in it sorts by
    /// value like any other. The fixture carries a 23-digit pair for this; the
    /// case here is the one where a `u64` parse would not merely be wrong but
    /// would fail outright.
    #[test]
    fn a_digit_run_is_not_bounded_by_an_integer_type() {
        use std::cmp::Ordering::Less;
        let small = format!("scan{}", "9".repeat(40));
        let large = format!("scan1{}", "0".repeat(40));
        assert_eq!(smart_string_compare(&small, &large), Less);
    }
}
