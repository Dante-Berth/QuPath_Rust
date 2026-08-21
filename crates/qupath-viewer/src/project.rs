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
    /// QuPath substitutes `Image <entryID>` when an entry has no `imageName`,
    /// in `DefaultProjectImageEntry`'s constructor, and so does this: a browser
    /// that showed a blank row where QuPath shows a name is showing a different
    /// project.
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
    })
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

    Image {
        id,
        name,
        builder: kind,
        slide,
        uri,
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

    #[test]
    fn a_project_with_no_images_is_still_a_project() {
        let files = files(Path::new("/study/project.qpproj"), &[], written());
        assert_eq!(files.len(), 1);
        assert!(files[0].contents.contains("\"images\": []"));
    }
}
