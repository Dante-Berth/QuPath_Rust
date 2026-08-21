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

/// Creating a project makes a directory QuPath's own reader would open, and
/// leaves it open here.
#[test]
fn a_new_project_is_created_empty_and_opened() {
    let directory = std::env::temp_dir().join("qupath-rust-test-new");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a directory");

    let mut app = App::new(None);
    app.new_project(&directory);

    let project = app.project.as_ref().expect("the project was created and opened");
    assert_eq!(project.path, directory.join(App::PROJECT_FILE));
    assert!(project.stored.images.is_empty(), "a new project starts empty");
    assert!(project.path.is_file(), "the index was not written");

    // Empty on purpose: QuPath separates *Create project* from *Add images*,
    // and one click that did both would be two decisions taken by one.
    assert!(app.show_project);
}

/// A directory that already holds a project is **not** overwritten.
///
/// Someone pointing a create dialog at a study directory is far more likely to
/// have meant to open it, and the cost of the two mistakes is not symmetrical.
#[test]
fn creating_a_project_does_not_overwrite_one() {
    let path = project_in("not-overwritten", &[("first", true)]);
    let directory = path.parent().expect("a directory").to_path_buf();
    let before = std::fs::read_to_string(&path).expect("the project");

    let mut app = App::new(None);
    app.new_project(&directory);

    assert_eq!(
        std::fs::read_to_string(&path).expect("the project"),
        before,
        "an existing project was overwritten",
    );
    assert!(app.project.is_none(), "the refusal opened something anyway");
    assert!(
        app.message.contains("already has a project"),
        "the refusal does not say why: {}",
        app.message
    );
}

/// Adding is refused, with a reason, when there is nothing to add or nowhere to
/// add it.
///
/// The reason is the button's own hover text, so this is asserting the sentence
/// the user reads rather than a boolean beside it.
#[test]
fn adding_says_why_it_cannot() {
    let mut app = App::new(None);
    let reason = app.addable().expect_err("no project and no slide");
    assert!(reason.contains("project"), "{reason}");

    let path = project_in("nothing-to-add", &[("first", true)]);
    app.open_project(path);
    let reason = app.addable().expect_err("a project but no slide");
    assert!(reason.contains("slide"), "{reason}");

    // And the command itself refuses rather than writing an entry for a slide
    // that is not open — the button and the action ask the same predicate.
    let before = std::fs::read_to_string(app.project.as_ref().expect("open").path.clone())
        .expect("the project");
    app.add_to_project();
    assert_eq!(
        std::fs::read_to_string(app.project.as_ref().expect("open").path.clone())
            .expect("the project"),
        before,
        "the project was rewritten with nothing to add",
    );
}

/// The slide the round-trip test opens, or `None` when it is not fetched.
///
/// Adding a slide to a project is the one thing here that needs a *real* one:
/// `summary.json` records the level-0 size, and the only place that comes from
/// is a slide that has been opened. Skipped rather than faked, because a
/// fabricated size is exactly the kind of plausible wrong number this format
/// work keeps turning up.
fn a_real_slide() -> Option<PathBuf> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/CMU-1-Small-Region.svs");
    path.is_file().then_some(path)
}

/// A slide, added to a project, read back out of the file.
///
/// The whole loop: open a project, open a slide, add it, and find it in the
/// project on disk with the size the slide actually has. What makes it worth
/// running against a real slide rather than a stub is the size — nothing but an
/// opened slide knows it.
#[test]
fn a_slide_added_to_a_project_is_in_the_file_afterwards() {
    let Some(slide) = a_real_slide() else {
        eprintln!("skipping: data/CMU-1-Small-Region.svs not present");
        return;
    };

    let directory = std::env::temp_dir().join("qupath-rust-test-added");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a directory");

    let mut app = App::new(None);
    app.new_project(&directory);
    app.open(slide.clone());
    assert!(app.open.is_some(), "the slide did not open: {}", app.message);

    app.addable().expect("a project and a slide, and the slide not in it");
    app.add_to_project();

    // Read from disk rather than from the application, because what the pane
    // lists has to be what another reader would find.
    let text = std::fs::read_to_string(directory.join(App::PROJECT_FILE)).expect("the index");
    let stored = qupath_viewer::project::read(&text).expect("a project");
    assert_eq!(stored.images.len(), 1);
    assert_eq!(stored.images[0].id, 1);
    assert_eq!(stored.images[0].name, "CMU-1-Small-Region.svs");
    assert_eq!(
        stored.images[0].slide.as_deref(),
        Ok(std::path::absolute(&slide).expect("an absolute path").as_path()),
        "the entry does not point at the slide that was added",
    );

    // The entry's own files, in the directory its id names. `summary.json`
    // carries the level-0 size, which is the one field that needed the slide.
    let summary = std::fs::read_to_string(directory.join("data/1/summary.json"))
        .expect("the entry summary");
    assert!(summary.contains("\"width\": 2220"), "{summary}");
    assert!(summary.contains("\"height\": 2967"), "{summary}");
    assert!(directory.join("data/1/server.json").is_file());

    // Adding it twice is refused, because two entries for one image is two
    // directories under `data/` that QuPath will show as two rows.
    let reason = app.addable().expect_err("the slide is already in the project");
    assert!(reason.contains("already in this project"), "{reason}");
}

/// A slide opened by a **relative** path is the same slide as the project's
/// absolute one.
///
/// Found by looking at the running application: the row for the slide on screen
/// was not marked, because the application had been started as
/// `-- data/CMU-1-Small-Region.svs` and every path a project stores is
/// absolute. The mark was the visible half. The half that mattered is
/// [`App::addable`], which offered to add a slide the project already
/// contained — two entries and two directories under `data/` for one image.
///
/// The same trap `project::file_uri` was written for, met from the other side,
/// and fixed the same way: resolve against the working directory, lexically,
/// before anything keeps the path.
#[test]
fn a_slide_opened_relatively_is_the_slide_the_project_names() {
    let Some(slide) = a_real_slide() else {
        eprintln!("skipping: data/CMU-1-Small-Region.svs not present");
        return;
    };

    let directory = std::env::temp_dir().join("qupath-rust-test-relative");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a directory");

    let mut app = App::new(None);
    app.new_project(&directory);
    app.open(slide.clone());
    app.add_to_project();
    assert_eq!(
        app.project.as_ref().expect("open").stored.images.len(),
        1,
        "the slide was not added: {}",
        app.message
    );

    // The same file, named the way a command line names it. `a_real_slide`
    // builds its path from `CARGO_MANIFEST_DIR`, so this is the same file
    // through a relative spelling with `..` in it.
    let relative = Path::new("../../data/CMU-1-Small-Region.svs");
    assert!(relative.is_file(), "the test's own relative path is wrong");
    app.open(relative.to_path_buf());
    assert!(app.open.is_some(), "the slide did not open: {}", app.message);

    let reason = app
        .addable()
        .expect_err("the project already holds this slide, spelled differently");
    assert!(reason.contains("already in this project"), "{reason}");

    // And the row for it is the one the browser marks, which is the same
    // comparison the pane makes.
    let showing = app.open.as_ref().expect("a slide").path.clone();
    let project = app.project.as_ref().expect("open");
    assert_eq!(
        project.stored.images[0].slide.as_deref(),
        Ok(showing.as_path()),
        "the browser would not mark the row for the slide on screen",
    );
    assert_eq!(showing, std::path::absolute(&slide).expect("an absolute path"));
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

/// A project and a slide on the same command line are both taken, in either
/// order.
///
/// The project is a list and the slide is what is being looked at; they are not
/// alternatives, and a single slot for both would silently drop one.
#[test]
fn a_project_and_a_slide_can_be_asked_for_together() {
    let project = PathBuf::from("study/project.qpproj");
    let slide = PathBuf::from("data/CMU-1.svs");
    for order in [
        ["study/project.qpproj", "data/CMU-1.svs"],
        ["data/CMU-1.svs", "study/project.qpproj"],
    ] {
        let asked = qupath_app::state::requested(order.map(str::to_owned));
        assert_eq!(
            asked,
            (Some(project.clone()), Some(slide.clone())),
            "one of {order:?} was dropped",
        );
    }

    // One of each kind, and neither.
    assert_eq!(
        qupath_app::state::requested(["data/CMU-1.svs".to_owned()]),
        (None, Some(slide.clone())),
    );
    assert_eq!(
        qupath_app::state::requested(["study/project.qpproj".to_owned()]),
        (Some(project), None),
    );
    assert_eq!(qupath_app::state::requested([]), (None, None));

    // Said twice, the second one is meant — a shell history full of appended
    // arguments is how that happens.
    assert_eq!(
        qupath_app::state::requested(
            ["a.svs", "b.svs"].map(str::to_owned)
        ),
        (None, Some(PathBuf::from("b.svs"))),
    );
}
