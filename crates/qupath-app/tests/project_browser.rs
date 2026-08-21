//! The project browser, driven through [`App`] rather than through a window.
//!
//! `docs/VIEWER.md` §6 says the picture is checked by looking and the state is
//! checked by asserting, and this is the second half. Nothing here needs a
//! window, a GPU or a slide: opening a project is reading a file and building a
//! list, and every failure of it is a state the application is left in.
//!
//! Written against states rather than against return values on purpose. A
//! project that will not open must leave the one already open alone, and a
//! function that returns nothing is one whose only observable effect is where
//! it leaves the application — see `docs/VIEWER.md` §3.4's three bugs, all of
//! which were about *when* state becomes visible.

use std::path::{Path, PathBuf};

use qupath_app::state::App;
use qupath_viewer::project::{self, Entry, Timestamps};

/// Writes a project into a fresh directory under the temporary one and returns
/// its `.qpproj`.
///
/// The slides are created as empty files, because nothing here opens one: what
/// is under test is the list, and `Path::is_file` is what the browser's mark
/// asks. An image whose flag is false names a slide that is *not* created,
/// which is the ordinary state of a study directory copied without its images.
fn project_in(name: &str, images: &[(&str, bool)]) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("qupath-rust-test-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a temporary project directory");

    let slides: Vec<PathBuf> = images
        .iter()
        .map(|(name, present)| {
            let path = directory.join(format!("{name}.svs"));
            if *present {
                std::fs::write(&path, b"").expect("a stand-in slide");
            }
            path
        })
        .collect();
    let entries: Vec<Entry<'_>> = images
        .iter()
        .zip(&slides)
        .map(|((name, _), path)| Entry {
            path,
            name,
            width: 2220,
            height: 2967,
        })
        .collect();

    let project = directory.join("project.qpproj");
    for file in project::files(&project, &entries, Timestamps::now()) {
        let path = directory.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("an entry directory");
        }
        std::fs::write(&path, &file.contents).expect("write a project file");
    }
    project
}

/// A project this application wrote is one it can open.
///
/// The writer and the reader are the same module, so this is not parity — that
/// is `parity_project_export.rs`, against a file QuPath wrote. What this checks
/// is the application's own loop: the pane's rows are what the project said.
#[test]
fn a_project_written_here_opens_in_the_browser() {
    let path = project_in("written", &[("first", true), ("second", true)]);
    let mut app = App::new(None);
    assert!(app.project.is_none(), "nothing is open before anything is opened");

    app.open_project(path.clone());

    let project = app.project.as_ref().expect("the project opened");
    assert_eq!(project.path, path);
    assert_eq!(project.name, "qupath-rust-test-written/project.qpproj");
    assert_eq!(project.stored.images.len(), 2);
    assert_eq!(project.stored.images[0].name, "first");
    assert_eq!(project.stored.images[1].name, "second");
    assert!(
        project.stored.images[1]
            .slide
            .as_deref()
            .expect("a path")
            .ends_with("second.svs")
    );

    // The pane is opened by opening a project. A list that arrives hidden is a
    // command that reports having done nothing.
    assert!(app.show_project, "opening a project did not show the browser");
    assert!(app.message.contains('2'), "the status bar says nothing: {}", app.message);
}

/// A slide that is not there is **listed and marked**, not dropped.
///
/// A study directory copied without its images is the ordinary way this
/// happens, and the whole point of a browser is that it is visible before fifty
/// clicks discover it one at a time.
#[test]
fn a_missing_slide_is_listed_and_marked() {
    let path = project_in("missing", &[("here", true), ("gone", false)]);
    let mut app = App::new(None);
    app.open_project(path);

    let project = app.project.as_ref().expect("the project opened");
    assert_eq!(project.stored.images.len(), 2, "the missing entry was dropped");
    assert!(project.present(0), "the slide that is there is marked absent");
    assert!(!project.present(1), "the slide that is not there is marked present");

    // Out of range is present, so a caller iterating something else does not
    // paint warnings on rows it invented.
    assert!(project.present(2));
}

/// A file that will not read leaves the project already open alone.
///
/// The same rule as a slide that will not open: failing to open a second one
/// must not close the first, which is the behaviour that makes an application
/// annoying rather than merely limited.
#[test]
fn a_project_that_will_not_read_keeps_the_one_that_did() {
    let good = project_in("kept", &[("first", true)]);
    let mut app = App::new(None);
    app.open_project(good.clone());
    assert_eq!(app.project.as_ref().expect("open").stored.images.len(), 1);

    let broken = good.with_file_name("broken.qpproj");
    std::fs::write(&broken, "{ this is not a project").expect("write a broken file");
    app.open_project(broken);

    let project = app.project.as_ref().expect("the first project is still open");
    assert_eq!(project.path, good, "a file that would not read replaced the project");
    assert!(
        app.message.contains("broken.qpproj"),
        "the failure does not name the file: {}",
        app.message
    );

    // And a path that is not there at all takes the same route, rather than
    // panicking on the read.
    app.open_project(good.with_file_name("absent.qpproj"));
    assert_eq!(app.project.as_ref().expect("still open").path, good);
    assert!(app.message.contains("absent.qpproj"), "{}", app.message);
}

/// A project with nothing in it opens, and says so.
///
/// An empty pane and a pane that failed to fill look identical, and the one
/// thing that separates them is a sentence.
#[test]
fn an_empty_project_opens_and_says_it_is_empty() {
    let path = project_in("empty", &[]);
    let mut app = App::new(None);
    app.open_project(path);

    let project = app.project.as_ref().expect("the project opened");
    assert!(project.stored.images.is_empty());
    assert!(
        app.message.to_lowercase().contains("no images"),
        "an empty project says nothing: {}",
        app.message
    );
}

/// A project that has been moved is one whose images will not open, and the
/// browser says so once rather than fifty times.
#[test]
fn a_project_that_moved_knows_it_moved() {
    let path = project_in("moved", &[("first", true)]);
    let mut app = App::new(None);
    app.open_project(path.clone());
    assert!(
        !app.project.as_ref().expect("open").moved(),
        "a project read from where it says it is has not moved"
    );

    // Copy the file somewhere else, which is what copying a study directory
    // does to every project in it. The `uri` field still names the old place.
    let elsewhere = std::env::temp_dir().join("qupath-rust-test-moved-elsewhere");
    let _ = std::fs::remove_dir_all(&elsewhere);
    std::fs::create_dir_all(&elsewhere).expect("a second directory");
    let moved = elsewhere.join("project.qpproj");
    std::fs::copy(&path, &moved).expect("copy the project");

    app.open_project(moved);
    assert!(
        app.project.as_ref().expect("open").moved(),
        "a project opened from somewhere other than its uri does not know it moved"
    );
}

/// The command line tells a project from a slide by its extension, and the
/// application has to agree with the file dialog's filter about which is which.
#[test]
fn a_project_is_told_from_a_slide_by_its_extension() {
    assert!(App::is_project(Path::new("study/project.qpproj")));
    assert!(App::is_project(Path::new("study/PROJECT.QPPROJ")));
    assert!(!App::is_project(Path::new("data/CMU-1.svs")));
    assert!(!App::is_project(Path::new("data/CMU-1")));
    // A slide named on the command line is still a slide, and the branch that
    // decides is the one above.
    let app = App::new(Some("data/CMU-1.svs".to_owned()));
    assert!(app.project.is_none());
}
