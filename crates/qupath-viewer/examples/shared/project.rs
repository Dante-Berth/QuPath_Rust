//! The entries the project parity sample is written from.
//!
//! Shared by `examples/project_sample.rs`, which writes the directory, and
//! `tests/parity_project_import.rs`, which asserts QuPath read those values
//! back. Unlike the GeoJSON sample the directory itself is **not** committed:
//! every path QuPath stores is an absolute `file:` URI, so a committed project
//! would name whichever machine recorded it. The entries are committed instead,
//! and the directory is written fresh by `parity/record-project.sh`.
//!
//! Not an example of its own: cargo only auto-discovers `examples/*.rs`.
#![allow(dead_code, reason = "the example and the test use different parts")]

use std::path::Path;

use qupath_viewer::Entry;
use qupath_viewer::project::Timestamps;

/// When the sample project claims to have been written.
///
/// Fixed, not `Timestamps::now`: QuPath requires both fields and reads nothing
/// else from them, so a clock here would make every regenerated project differ
/// from the last in two fields that mean nothing to this check. The two values
/// differ from each other because they are different fields — a sample with the
/// same number in both cannot see them swapped.
pub const WRITTEN: Timestamps = Timestamps {
    created: 1_700_000_000_000,
    modified: 1_700_000_060_000,
};

/// The slide the entries point at, relative to the repository root.
///
/// The smallest of the three in `data/`, because nothing here opens it — QuPath
/// resolves the URI and reads the metadata this project wrote, and the file has
/// to *exist* for the first of those to mean anything.
pub const SLIDE: &str = "data/CMU-1-Small-Region.svs";

/// A second slide, so the fixture covers a project with more than one image.
///
/// One entry cannot distinguish "the entry list was read" from "an entry was
/// read": `entryID` counts from 1, and a writer that emitted the same directory
/// name for every entry would put both images in `data/1` and be invisible in a
/// project of one.
pub const SECOND_SLIDE: &str = "data/CMU-1-JP2K-33005.svs";

/// The name the browser shows for the first entry.
pub const NAME: &str = "CMU-1 small region";

/// The name of the second, with a space and a comma in it.
///
/// Not decoration: names go through `escape` into JSON and paths go through
/// `file_uri`'s percent-encoding, and a name that is one word tests neither.
pub const SECOND_NAME: &str = "CMU-1, JP2K";

/// Level-0 dimensions of the first entry.
pub const WIDTH: u32 = 2220;
pub const HEIGHT: u32 = 2967;

/// The second entry's dimensions.
pub const SECOND_WIDTH: u32 = 46_000;
pub const SECOND_HEIGHT: u32 = 32_914;

/// The two entries, in the order they are written.
#[must_use]
pub fn entries<'a>(slide: &'a Path, second: &'a Path) -> Vec<Entry<'a>> {
    vec![
        Entry {
            path: slide,
            name: NAME,
            width: WIDTH,
            height: HEIGHT,
        },
        Entry {
            path: second,
            name: SECOND_NAME,
            width: SECOND_WIDTH,
            height: SECOND_HEIGHT,
        },
    ]
}
