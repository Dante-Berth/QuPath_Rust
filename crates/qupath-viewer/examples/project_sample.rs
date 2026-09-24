//! Writes a QuPath project directory, for QuPath to open.
//!
//! `docs/dev/VIEWER.md` §8.8's remaining half. The GeoJSON export has been through
//! `PathIO.readObjects`; the project format had been checked against a JSON
//! parser and against `DefaultProject.java` and never opened by
//! `ProjectIO.loadProject`.
//!
//! ```text
//! cargo run -p qupath-viewer --example project_sample -- /tmp/study
//! ```
//!
//! Driven by `parity/record-project.sh`, which writes the directory, has
//! `parity/java/ProjectImport.java` read it back, and records what QuPath saw.
//!
//! **The directory is written fresh rather than committed**, which is the one
//! way this differs from the GeoJSON sample: every path QuPath stores is an
//! absolute `file:` URI — `file_uri` resolves a relative one for exactly that
//! reason — so a committed project would name the machine that recorded it. The
//! *entries* are committed, in `shared/project.rs`, and they are what the test
//! compares QuPath's reading against.
//!
//! `thumbnail.jpg` is not written *here* and `data.qpdata` is not written at
//! all; §8.3 and `project.rs` say why. The thumbnail is written straight after
//! this, by `qupath-app`'s `project_thumbnails` example — the encoder lives in
//! that crate — and `ProjectImport` then records that QuPath's `ImageIO.read`
//! accepts it.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::path::{Path, PathBuf};

#[path = "shared/project.rs"]
mod sample;

fn main() {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(args.next().unwrap_or_else(|| "study".to_owned()));
    let slide = PathBuf::from(args.next().unwrap_or_else(|| sample::SLIDE.to_owned()));
    let second = PathBuf::from(
        args.next()
            .unwrap_or_else(|| sample::SECOND_SLIDE.to_owned()),
    );

    for path in [&slide, &second] {
        assert!(
            path.exists(),
            "{} is missing; see docs/test-data.md. QuPath resolves the URI in \
             the project, so a slide that is not there makes the entry fail to \
             open for a reason that has nothing to do with the format.",
            path.display()
        );
    }

    let entries = sample::entries(&slide, &second);
    let project = directory.join("project.qpproj");

    // Built the way the **application** builds one: an empty project, then the
    // entries appended to it. That is `qupath-app`'s New project followed by
    // Add slide, and it is a different writer from `files` with two entries in
    // it — `added` re-serialises the whole index through `json`, so these are
    // the bytes a project someone creates in the application actually has.
    // Writing it the other way here would leave that path unopened by QuPath,
    // which is the hole this sample exists to close.
    //
    // A fixed pair rather than `Timestamps::now`, so re-recording a fixture
    // does not produce a project that differs from the last one in two fields
    // that mean nothing to this check.
    let empty = qupath_viewer::project::project_file(&project, &[], sample::WRITTEN);
    let stored = qupath_viewer::project::read(&empty).expect("an empty project reads back");
    for file in qupath_viewer::project::added(&stored, &entries, sample::WRITTEN) {
        let path = directory.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("project directory");
        }
        std::fs::write(&path, &file.contents).expect("write a project file");
        println!("{}", path.display());
    }
    // And then an entry's metadata edited, which is the third writer and the
    // one whose output nothing had ever put in front of QuPath. A project
    // recorded without this exercises `files` and `added` only, so the field a
    // user actually types into would be checked against nothing but this
    // project's own reader — which is the shape `docs/dev/VIEWER.md` §8.6 calls a
    // format tested against the code that writes it.
    //
    // The keys are chosen to be awkward on purpose: one holds the `=` and `&`
    // that the browser's own `key=value` search splits on, and one holds a
    // non-ASCII character, because what QuPath has to agree about is the bytes
    // and not the easy cases.
    let text = std::fs::read_to_string(&project).expect("the project just written");
    let stored = qupath_viewer::project::read(&text).expect("it reads back");
    let first = stored.images.first().expect("an entry").id;
    let metadata = [
        (String::from("stain"), String::from("H&E")),
        (String::from("block"), String::from("A=3")),
        (String::from("noté"), String::from("réséqué")),
    ];
    for file in qupath_viewer::project::edited(&stored, first, &metadata, sample::WRITTEN)
        .expect("the entry is in the project it was read from")
    {
        let path = directory.join(&file.path);
        std::fs::write(&path, &file.contents).expect("write a project file");
        println!("{}", path.display());
    }

    println!(
        "{} entries in {}, {} metadata keys on entry {first}",
        entries.len(),
        Path::new(&directory).display(),
        metadata.len(),
    );
}
