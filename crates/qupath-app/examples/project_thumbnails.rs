//! Writes `thumbnail.jpg` into an already-written project directory.
//!
//! ```text
//! cargo run -p qupath-app --example project_thumbnails -- /tmp/study slide.svs ...
//! ```
//!
//! The companion of `qupath-viewer`'s `project_sample`, and separate from it for
//! one reason: the encoder lives here. `qupath-viewer` depends on `qupath-slide`
//! alone — `docs/VIEWER.md` §7 settled that, and a JPEG encoder is not a reason
//! to reopen it — so the crate that writes the format cannot encode the picture,
//! and the crate that runs the application can. `project::files` says as much in
//! its own documentation: it returns the JSON and leaves this file to its
//! caller.
//!
//! Driven by `parity/record-project.sh`, so that `ProjectImport`'s
//! `thumbnailReadable` is recorded against a project that *has* thumbnails.
//! That field has read `false` for as long as it has existed, which measured
//! only that QuPath tolerates the file's absence. What it could never say is
//! whether a thumbnail written here is one `ImageIO.read` accepts — a JPEG
//! checked only against the encoder that wrote it is untested, and this is the
//! reader that matters.
//!
//! The entries are numbered from 1 in the order they are given, which is
//! `Entry::directory`'s rule and the order `project_sample` writes them in.
//!
//! A slide whose pixels this project cannot decode — `CMU-1-JP2K-33005.svs` is
//! Aperio JPEG 2000, and §3 has why that is refused rather than guessed at —
//! gets **no** thumbnail, and that is not an error. QuPath regenerates the file
//! from the pixels when it is absent, so a project with one thumbnail and one
//! missing is a valid project, and it is also the more interesting fixture:
//! `thumbnailReadable` then records both answers from a single recording.
//!
//! Scratch tooling, not part of the library.
#![allow(missing_docs, reason = "example binary")]

use std::path::PathBuf;

use qupath_app::thumbnails;
use qupath_slide::Slide;

fn main() {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(args.next().expect("a project directory"));
    let slides: Vec<PathBuf> = args.map(PathBuf::from).collect();
    assert!(!slides.is_empty(), "at least one slide is needed");

    for (index, slide_path) in slides.iter().enumerate() {
        let slide = Slide::open(slide_path)
            .unwrap_or_else(|error| panic!("{} does not open: {error}", slide_path.display()));
        // A slide this project cannot decode is skipped, not fatal. See above:
        // the absent file is a state QuPath handles, and recording it beside a
        // present one is worth more than refusing to record at all.
        let bytes = match thumbnails::make(&slide).and_then(|t| thumbnails::encode_jpeg(&t)) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("no thumbnail for {}: {error}", slide_path.display());
                continue;
            }
        };

        // `Entry::directory`'s rule: entries are numbered from 1.
        let at = directory.join(format!("data/{}/thumbnail.jpg", index + 1));
        std::fs::create_dir_all(at.parent().expect("an entry directory")).expect("a directory");
        std::fs::write(&at, &bytes).expect("a thumbnail");
        eprintln!("wrote {} ({} bytes)", at.display(), bytes.len());
    }
}
