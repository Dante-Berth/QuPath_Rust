//! Writes a QuPath project directory, for QuPath to open.
//!
//! `docs/VIEWER.md` §8.8's remaining half. The GeoJSON export has been through
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
//! `thumbnail.jpg` is not written here and `data.qpdata` is not written at all;
//! §8.3 and `project.rs` say why. Whether QuPath minds is one of the things
//! this exists to find out.
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
    let second = PathBuf::from(args.next().unwrap_or_else(|| sample::SECOND_SLIDE.to_owned()));

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
    // A fixed pair rather than `Timestamps::now`, so re-recording a fixture
    // does not produce a project that differs from the last one in two fields
    // that mean nothing to this check.
    for file in qupath_viewer::project::files(&project, &entries, sample::WRITTEN) {
        let path = directory.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("project directory");
        }
        std::fs::write(&path, &file.contents).expect("write a project file");
        println!("{}", path.display());
    }
    println!(
        "{} entries in {}",
        entries.len(),
        Path::new(&directory).display()
    );
}
