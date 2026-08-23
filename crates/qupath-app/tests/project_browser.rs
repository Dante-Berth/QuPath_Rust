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

use qupath_app::chrome;
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

/// A search does not outlive the project it was typed against.
///
/// The state this prevents is a list with rows missing and no visible reason:
/// the box holding the text is at the top of the pane, and a study of fifty
/// rows is scrolled. QuPath clears `tfFilter` on the same event.
#[test]
fn a_search_does_not_outlive_the_project_it_was_typed_against() {
    let first = project_in("searched", &[("liver", true), ("kidney", true)]);
    let second = project_in("searched-again", &[("spleen", true)]);

    let mut app = App::new(None);
    app.open_project(first);
    app.filter = String::from("liver");
    app.open_project(second);

    assert!(
        app.filter.is_empty(),
        "the search from the last study is still narrowing this one: {}",
        app.filter,
    );
}

/// Waits for the browser to finish making the pictures it is going to make.
///
/// The worker is a thread and the application takes delivery a frame at a time,
/// so a test that asserted straight after opening would assert on how fast a
/// slide opens. Bounded rather than a bare loop: a hang here should fail the
/// test, not stop it.
fn wait_for_pictures(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.take_pictures();
        let working = app
            .project
            .as_ref()
            .is_some_and(|project| project.pictures.working());
        if !working {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the thumbnails never arrived",
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A row's picture comes from the slide, and a row whose slide is not there
/// gets none.
///
/// The point of the first half is that no `thumbnail.jpg` is involved: this
/// project was written by `project::files`, which does not write one, and the
/// row has a picture anyway — from the pixels, exactly as QuPath's own browser
/// does when an entry has no stored thumbnail.
///
/// The second half is what stops "there is a picture" being a fact about the
/// row rather than about the slide: an entry naming a file that is not there
/// stays a row, and stays a row without a picture.
#[test]
fn a_row_gets_its_picture_from_the_slide() {
    let Some(slide) = a_real_slide() else {
        eprintln!("skipping: data/CMU-1-Small-Region.svs not present");
        return;
    };

    let directory = std::env::temp_dir().join("qupath-rust-test-pictures");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a directory");
    let missing = directory.join("gone.svs");
    let project = directory.join("project.qpproj");
    let entries = [
        Entry {
            path: &slide,
            name: "here",
            width: 2220,
            height: 2967,
        },
        Entry {
            path: &missing,
            name: "gone",
            width: 2220,
            height: 2967,
        },
    ];
    for file in project::files(&project, &entries, Timestamps::now()) {
        let path = directory.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("an entry directory");
        }
        std::fs::write(&path, &file.contents).expect("write a project file");
    }

    let mut app = App::new(None);
    app.open_project(project);
    wait_for_pictures(&mut app);

    let pictures = &app.project.as_ref().expect("the project opened").pictures;
    let picture = pictures
        .made(0)
        .expect("the slide that is there has a picture");
    let (width, height) = picture.overview.size();
    assert_eq!(
        picture.pixels.len(),
        width as usize * height as usize * 4,
        "the picture is not the size it says it is",
    );
    // The slide is 2220 x 2967, so the long edge is the one that gets
    // `MAX_LENGTH` and the picture is taller than it is wide. A square picture
    // would mean the aspect had been thrown away somewhere.
    assert!(
        height > width,
        "{width} x {height} is not the slide's shape"
    );
    assert!(
        pictures.made(1).is_none(),
        "a slide that is not on disk cannot have been read",
    );
    assert!(
        pictures.unread(1).is_some(),
        "the row with no picture does not say why it has none",
    );
}

/// A slide that will not read says so, rather than waiting for ever.
///
/// The state this is about is the one the running window showed: an entry whose
/// file is **there** and yields no picture — a JPEG 2000 pyramid opens and
/// decodes nothing, and `data/` has one. Its row held an empty frame, which is
/// also what a row whose slide is still being read holds, so the browser said
/// "wait" for something that was never coming.
///
/// The stand-in slides these projects are written with are the same case
/// arriving one step earlier: a file that exists and is not a TIFF.
#[test]
fn a_slide_that_will_not_read_says_why() {
    let path = project_in("unreadable", &[("stand-in", true)]);
    let mut app = App::new(None);
    app.open_project(path);
    wait_for_pictures(&mut app);

    let pictures = &app.project.as_ref().expect("the project opened").pictures;
    assert!(pictures.made(0).is_none(), "an empty file is not a slide");
    let reason = pictures
        .unread(0)
        .expect("a row with no picture and no reason is a row that looks like it is loading");
    // Names the file it is about, which is `qupath-slide`'s own rule for every
    // error it returns and the reason the browser can show one against a row
    // without wrapping it in a second sentence.
    assert!(
        reason.contains("stand-in.svs"),
        "the reason does not say which slide it is about: {reason}",
    );
}

/// Pictures belong to the project that asked for them, and to no other.
///
/// The failure this exists for is not hypothetical arithmetic: the worker
/// delivers by row index, and row 0 of one study is a different slide from row
/// 0 of the next. A browser that kept one queue would paint a picture from the
/// study that was closed onto the list of the study that replaced it, and both
/// rows would look perfectly ordinary.
///
/// What makes it impossible here is ownership rather than a check — the worker
/// belongs to the project, so opening another one drops the channel its
/// pictures would have arrived on. This asserts on where a *sequence* of opens
/// leaves the browser, because a single open is correct in every version of
/// this that gets it wrong.
#[test]
fn the_pictures_of_a_closed_project_do_not_arrive_in_the_next_one() {
    let Some(slide) = a_real_slide() else {
        eprintln!("skipping: data/CMU-1-Small-Region.svs not present");
        return;
    };

    let directory = std::env::temp_dir().join("qupath-rust-test-two-studies");
    let _ = std::fs::remove_dir_all(&directory);
    let first = directory.join("first");
    let second = directory.join("second");
    std::fs::create_dir_all(&first).expect("a directory");
    std::fs::create_dir_all(&second).expect("a directory");

    let write = |at: &Path, entries: &[Entry<'_>]| {
        let index = at.join("project.qpproj");
        for file in project::files(&index, entries, Timestamps::now()) {
            let path = at.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("an entry directory");
            }
            std::fs::write(&path, &file.contents).expect("write a project file");
        }
        index
    };
    let one = write(
        &first,
        &[Entry {
            path: &slide,
            name: "the slide",
            width: 2220,
            height: 2967,
        }],
    );
    let elsewhere = second.join("not-here.svs");
    let two = write(
        &second,
        &[Entry {
            path: &elsewhere,
            name: "a slide that is not there",
            width: 2220,
            height: 2967,
        }],
    );

    let mut app = App::new(None);
    app.open_project(one);
    // Waited for, so the first study's picture has certainly been made before
    // the second study replaces it. Without the wait this passes on a machine
    // where the worker had not got to it yet, which is the test agreeing with
    // itself rather than with the property.
    wait_for_pictures(&mut app);
    assert!(
        app.project
            .as_ref()
            .expect("the first project")
            .pictures
            .made(0)
            .is_some(),
        "the first study's own picture was never made, so this proves nothing",
    );

    app.open_project(two);
    wait_for_pictures(&mut app);

    let pictures = &app.project.as_ref().expect("the second project").pictures;
    assert!(
        pictures.made(0).is_none(),
        "row 0 of the second study is showing a picture of the first study's slide",
    );
}

/// Adding a slide does not blank the pictures the browser already has.
///
/// `add_to_project` writes the index and opens it again, which is a re-read of
/// a project whose rows are the same rows. A browser that started its worker
/// from nothing there would clear a fifty-slide study's pictures and read fifty
/// slides again, for one added row — visibly, as a pane that empties and fills
/// back in.
///
/// Asserted **without waiting**: the picture has to be there in the frame after
/// the add, which is the difference between carrying it over and making it
/// again quickly.
#[test]
fn adding_a_slide_keeps_the_pictures_already_made() {
    let Some(slide) = a_real_slide() else {
        eprintln!("skipping: data/CMU-1-Small-Region.svs not present");
        return;
    };

    let directory = std::env::temp_dir().join("qupath-rust-test-kept-pictures");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a directory");

    let mut app = App::new(None);
    app.new_project(&directory);
    app.open(slide.clone());
    app.add_to_project();
    wait_for_pictures(&mut app);
    assert!(
        app.project
            .as_ref()
            .expect("a project")
            .pictures
            .made(0)
            .is_some(),
        "the slide that was added has no picture: {}",
        app.message,
    );

    // A second entry for the same file, written straight into the index so the
    // application's own refusal to add a slide twice is not what is under test.
    let text = std::fs::read_to_string(directory.join(App::PROJECT_FILE)).expect("the index");
    let stored = project::read(&text).expect("a project");
    let files = project::added(
        &stored,
        &[Entry {
            path: &slide,
            name: "the same slide again",
            width: 2220,
            height: 2967,
        }],
        Timestamps::now(),
    );
    for file in files {
        let path = directory.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("an entry directory");
        }
        std::fs::write(&path, &file.contents).expect("write a project file");
    }

    app.open_project(directory.join(App::PROJECT_FILE));
    let pictures = &app
        .project
        .as_ref()
        .expect("the project re-opened")
        .pictures;
    assert!(pictures.made(0).is_some(), "the first row lost its picture");
    assert!(
        pictures.made(1).is_some(),
        "the second row names the same slide and should have been given the \
         picture already made for it",
    );
}

/// A project with the three states a row can be in — a slide that is there, one
/// that is not, and an entry built by a transform this cannot open — and the
/// `.qpproj` that lists them, in that order.
///
/// The second entry's `builderType` is rewritten afterwards, for
/// `examples/chrome`'s reason: writing one is exactly what `project.rs`
/// refuses to do, so the only way to get the row is to edit the file. The
/// rewrite lands on the *last* occurrence, so the openable row must be written
/// first — an entry added after it would silently become the greyed one.
fn a_project_with_an_unopenable_row(name: &str) -> PathBuf {
    // Named by the caller, and that is not tidiness. `cargo test` runs these
    // on threads; a shared directory means one test's `remove_dir_all` runs
    // while another is reading the project out of it, and what that produced
    // was the fill test failing only when run alongside the others and passing
    // alone. A fixture that is torn down under a reader is a flake that looks
    // like a bug in the thing under test.
    let directory = std::env::temp_dir().join(format!("qupath-rust-test-rows-{name}"));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a temporary project directory");

    let openable = directory.join("openable.svs");
    std::fs::write(&openable, b"").expect("a stand-in slide");
    // Named and not created, which is the ordinary state of a study directory
    // copied without its images.
    let gone = directory.join("gone.svs");
    let rotated = directory.join("rotated.svs");
    std::fs::write(&rotated, b"").expect("a stand-in slide");

    let entries = [
        Entry {
            path: &openable,
            name: "openable",
            width: 2220,
            height: 2967,
        },
        Entry {
            path: &gone,
            name: "gone",
            width: 2220,
            height: 2967,
        },
        // Last, and that is load-bearing: the rewrite below finds the *last*
        // `"builderType": "uri"` in the file, so an entry added after this one
        // would silently become the greyed row instead. `examples/chrome` has
        // the same comment, for the same reason and after the same mistake.
        Entry {
            path: &rotated,
            name: "rotated",
            width: 2967,
            height: 2220,
        },
    ];

    let project = directory.join("project.qpproj");
    for file in project::files(&project, &entries, Timestamps::now()) {
        let path = directory.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("an entry directory");
        }
        let contents = if file.path == "project.qpproj" {
            match file.contents.rsplit_once("\"builderType\": \"uri\"") {
                Some((before, after)) => format!("{before}\"builderType\": \"rotated\"{after}"),
                None => file.contents.clone(),
            }
        } else {
            file.contents.clone()
        };
        std::fs::write(&path, contents).expect("write a project file");
    }
    project
}

/// The y each row is laid out at, in points, in the 1100×760 pane these tests
/// use.
///
/// Read off the layout rather than computed: a row's height depends on the
/// picture beside it and on whether its name wraps, so a formula here would be
/// a second implementation of `chrome.rs`'s layout that could agree with it
/// while both were wrong. `a_row_is_where_these_tests_think_it_is` asserts
/// these still land on rows, so a layout change fails there rather than
/// quietly turning every gesture below into a press on nothing.
const ROWS: [f32; 3] = [281.0, 335.0, 389.0];

/// What the pointer does before the layout is read.
///
/// Two gestures rather than a position and a flag, for `examples/chrome`'s
/// reason: a hover is a `PointerMoved` and nothing else, and a click is that
/// move plus a press and a release. They are not one gesture with a modifier.
#[derive(Clone, Copy)]
enum Gesture {
    /// The pointer is moved here and left there.
    Hovering(egui::Pos2),
    /// The pointer is moved here, pressed and released.
    Clicking(egui::Pos2),
}

impl Gesture {
    /// Where the pointer ends up.
    fn at(self) -> egui::Pos2 {
        match self {
            Self::Hovering(at) | Self::Clicking(at) => at,
        }
    }
}

/// Lays the browser out with `gesture` delivered, and returns what the frame
/// drew and what it asked for.
///
/// Six frames and time between them, for `examples/chrome`'s reason: `egui`
/// interpolates a widget towards its hovered colour rather than switching to
/// it, so the frame the pointer arrives on has the fill part of the way in and
/// a caller reading that frame would be reading an animation a fifth of the way
/// through rather than a colour.
///
/// The gesture is delivered on **every** frame after the first rather than on
/// one of them. For a hover that is what holding the pointer still means; for a
/// click it means the press is repeated, which is deliberate — `Response
/// ::clicked()` is true on the frame the release lands, and a click delivered
/// once on frame 1 would have its command overwritten by the five frames after
/// it. Repeating means the returned command is the one the *settled* layout
/// produces, which is the layout everything else here is read off.
///
/// Returns the last frame's shapes and the last frame's command, so a caller
/// can assert on what was drawn and on what pressing it did without laying the
/// pane out twice and hoping the two agree.
fn lay_out(
    app: &mut App,
    gesture: Option<Gesture>,
) -> (Vec<egui::epaint::ClippedShape>, chrome::Command) {
    let context = egui::Context::default();
    chrome::style(&context);
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1100.0, 760.0),
        )),
        predicted_dt: 1.0 / 20.0,
        ..Default::default()
    };

    let mut last = None;
    for frame in 0..6 {
        input.events.clear();
        // Not on the first, which is `egui` still deciding where things are: a
        // press or a hover has to land on a widget that already has a
        // rectangle. The same reason `examples/chrome` clicks on the second.
        if let Some(gesture) = gesture.filter(|_| frame > 0) {
            input.events.push(egui::Event::PointerMoved(gesture.at()));
            if let Gesture::Clicking(at) = gesture {
                input.events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                });
                input.events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        let mut command = chrome::Command::None;
        let output = context.run(input.clone(), |context| {
            command = chrome::draw(context, app).0;
        });
        last = Some((output.shapes, command));
    }
    last.expect("a settled frame")
}

/// The fill colours of the full-width rounded rectangles the rows draw.
///
/// Reads the shapes rather than the tessellated meshes. A rounded rectangle
/// becomes a fan of triangles carrying the same colour repeated, so a fill read
/// back out of one is a fill read back out of a rasteriser; the `Shape::Rect`
/// is where the decision actually is.
///
/// Filtered by width, which is what separates a row's own rectangle from the
/// widget frames sharing the pane: the row is deliberately allocated the whole
/// width of the browser (see `chrome.rs`), and the search box and the sort
/// combo are not.
fn row_fills(app: &mut App, at: egui::Pos2) -> Vec<egui::Color32> {
    fn walk(shape: &egui::Shape, into: &mut Vec<egui::Color32>) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(shape, into);
                }
            }
            egui::Shape::Rect(rect) => {
                let size = rect.rect.size();
                if size.x > 200.0 && (40.0..80.0).contains(&size.y) {
                    into.push(rect.fill);
                }
            }
            _ => {}
        }
    }

    let (shapes, _) = lay_out(app, Some(Gesture::Hovering(at)));
    let mut fills = Vec::new();
    for clipped in &shapes {
        walk(&clipped.shape, &mut fills);
    }
    fills
}

/// A row the pointer is over fills only when pressing it would do something.
///
/// This is the assertion behind `chrome-project-hover.png` and
/// `chrome-project-hover-unopenable.png`, and the pictures are what found it.
/// The fill was on every hovered row, so an entry built by a transform this
/// cannot open took byte-identically the same colour as a slide that opens —
/// the two pictures were indistinguishable at the fill, and the greyed text was
/// carrying the whole difference. A row that lights up under the pointer and
/// then ignores the press is the interface promising something it will not do.
///
/// Written as three layouts of the same project rather than as one, because
/// the claim is a difference between them: the un-openable row not filling
/// means nothing unless the openable one does, on the same list, drawn the same
/// way. The neutral layout is what says a fill seen at all is the hover's and
/// not something the row draws regardless.
#[test]
fn only_a_row_that_opens_fills_under_the_pointer() {
    let mut app = App::new(None);
    app.open_project(a_project_with_an_unopenable_row("hover-fill"));
    wait_for_pictures(&mut app);

    // Well away from the list, so nothing is hovered.
    let neutral = row_fills(&mut app, egui::pos2(800.0, 600.0));
    // The rows, at the y each one is laid out at. The first is a slide that is
    // there; the third is the entry built by a transform this cannot open.
    let first = row_fills(&mut app, egui::pos2(150.0, ROWS[0]));
    let unopenable = row_fills(&mut app, egui::pos2(150.0, ROWS[2]));

    let filled = |fills: &[egui::Color32]| -> usize {
        fills
            .iter()
            .filter(|fill| **fill != egui::Color32::TRANSPARENT)
            .count()
    };

    assert_eq!(
        filled(&neutral),
        0,
        "no row should be filled with the pointer off the list, and one that is          means the fill below is not the hover's: {neutral:?}",
    );
    assert_eq!(
        filled(&first),
        1,
        "the openable row should fill under the pointer: {first:?}",
    );
    assert_eq!(
        filled(&unopenable),
        0,
        "the row built by a transform this cannot open should not fill under          the pointer — it takes no click, and a fill says it does: \
         {unopenable:?}",
    );
}

/// The positions in [`ROWS`] land on rows, one each and in order.
///
/// Without this every gesture below could be a press on nothing. A click that
/// misses produces no command and a hover that misses produces no fill, so both
/// of the assertions those tests make are *satisfied* by a coordinate that
/// stopped being right — which is the shape of a test that passes for the wrong
/// reason, and the one `examples/chrome` records twice already (a click landing
/// on no popup, and a builder rewrite landing on the wrong entry).
///
/// Asserts they are distinct rows rather than that they are at any particular
/// y. The numbers are a fact about `egui`'s spacing and this pane's contents;
/// what the tests below need is only that each one is a different row and that
/// they run in the order the project lists.
#[test]
fn a_row_is_where_these_tests_think_it_is() {
    let mut app = App::new(None);
    app.open_project(a_project_with_an_unopenable_row("positions"));
    wait_for_pictures(&mut app);

    let mut hit = Vec::new();
    for y in ROWS {
        let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(egui::pos2(150.0, y))));
        hit.push(command);
    }

    // The first two rows name slides and open; the third is the transform,
    // which is not clickable at all. So the first two must have produced a
    // command and they must not be the same one.
    let (first, second) = (&hit[0], &hit[1]);
    let named = |command: &chrome::Command| match command {
        chrome::Command::OpenPath(path) => Some(path.clone()),
        _ => None,
    };
    let first = named(first).unwrap_or_else(|| panic!("ROWS[0] pressed nothing: {first:?}"));
    let second = named(second).unwrap_or_else(|| panic!("ROWS[1] pressed nothing: {second:?}"));
    assert_ne!(
        first, second,
        "ROWS[0] and ROWS[1] are the same row, so one of them has moved",
    );
    assert!(
        first.ends_with("openable.svs"),
        "ROWS[0] is not the first row the project lists: {}",
        first.display(),
    );
    assert!(
        second.ends_with("gone.svs"),
        "ROWS[1] is not the second row the project lists: {}",
        second.display(),
    );
}

/// Pressing a row asks for that row's slide, and pressing one that cannot be
/// opened asks for nothing.
///
/// `docs/VIEWER.md` §8.8 recorded this as needing a hand on the machine —
/// "nothing has *clicked* a row" — alongside the hover states, and it needs one
/// no more than they did. A click is a `PointerMoved` and two `PointerButton`
/// events, and `chrome::draw` returns the `Command` the press produced, so the
/// whole chain from a pixel position to what the application is asked to do is
/// reachable without a window.
///
/// **The row that is missing its slide is the interesting one, and it must
/// still ask.** The browser's mark comes from `Path::is_file` at the moment the
/// project was opened, and a slide plugged in since is one the browser should
/// not be second-guessing — the row's own hover text says "Clicking still tries
/// it". A refusal here would make that sentence false, and `App::open`'s
/// message is where a slide that really is gone gets reported.
#[test]
fn pressing_a_row_asks_for_that_row_and_only_that_row() {
    let mut app = App::new(None);
    app.open_project(a_project_with_an_unopenable_row("press"));
    wait_for_pictures(&mut app);

    let press =
        |app: &mut App, y: f32| lay_out(app, Some(Gesture::Clicking(egui::pos2(150.0, y)))).1;

    match press(&mut app, ROWS[0]) {
        chrome::Command::OpenPath(path) => assert!(
            path.ends_with("openable.svs"),
            "pressing the first row asked for the wrong slide: {}",
            path.display(),
        ),
        other => panic!("pressing a row that opens asked for nothing: {other:?}"),
    }

    match press(&mut app, ROWS[1]) {
        chrome::Command::OpenPath(path) => assert!(
            path.ends_with("gone.svs"),
            "pressing the missing row asked for the wrong slide: {}",
            path.display(),
        ),
        other => panic!(
            "pressing a row whose slide is not there asked for nothing: {other:?} — the row's \
             own hover text says clicking still tries it",
        ),
    }

    // **A weaker assertion than it looks, and worth saying so.** The command is
    // emitted inside `if let Ok(path) = &image.slide`, and `openable` *is*
    // `slide.is_ok()` — so a row with no path cannot ask for anything whatever
    // its `Sense` is. Giving that row `Sense::click()` leaves this passing,
    // which was checked rather than assumed. What the sense actually controls
    // is the fill and the pointer cursor, and
    // `only_a_row_that_opens_fills_under_the_pointer` is what guards that half.
    // This line is here so that a future change routing a command out of the
    // greyed row has something to fail.
    assert!(
        matches!(press(&mut app, ROWS[2]), chrome::Command::None),
        "pressing a row built by a transform this cannot open asked for something",
    );
}

/// Pressing the browser's background asks for nothing.
///
/// The row is deliberately allocated the whole width of the pane so that the
/// space to the right of a short name takes the fill *and* the click — the two
/// have to agree, and the half that reads as broken is an obviously highlighted
/// row ignoring a press. This is the other side of that: the agreement must
/// stop at the row's edge, and a pane that opened a slide when someone pressed
/// the empty space under the list would be the same bug pointing outward.
#[test]
fn pressing_below_the_rows_asks_for_nothing() {
    let mut app = App::new(None);
    app.open_project(a_project_with_an_unopenable_row("below"));
    wait_for_pictures(&mut app);

    // Inside the browser's panel and below the last row, which ends at 412.
    let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(egui::pos2(150.0, 600.0))));
    assert!(
        matches!(command, chrome::Command::None),
        "pressing the empty space under the list asked for something: {command:?}",
    );
}

/// Where the pane's *Add the slide showing* button is laid out.
///
/// Read off the layout, not computed, for `ROWS`' reason: the button sits under
/// whatever the pane draws above it, so a formula here would be a second copy
/// of `chrome.rs`'s layout free to agree with it while both were wrong.
/// `the_add_button_is_where_these_tests_think_it_is` is what fails if the pane
/// grows a row above it and this coordinate lands on nothing.
const ADD_BUTTON: egui::Pos2 = egui::pos2(100.0, 140.0);

/// The coordinate above is on the button, and not merely on nothing.
///
/// The same guard `a_row_is_where_these_tests_think_it_is` exists for, and it
/// is needed more sharply here: two of the three assertions below are that
/// pressing this point produces **no** command, which is exactly what pressing
/// empty space produces. Without this test a coordinate that drifted off the
/// button would leave those two passing while checking nothing at all, and only
/// the third would notice.
#[test]
fn the_add_button_is_where_these_tests_think_it_is() {
    let Some(slide) = a_real_slide() else {
        eprintln!("skipping: data/CMU-1-Small-Region.svs not present");
        return;
    };
    let mut app = App::new(None);
    app.open_project(project_in("add-button-position", &[("first", true)]));
    app.open(slide);
    wait_for_pictures(&mut app);
    app.addable().expect("a project and a slide not in it");

    let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(ADD_BUTTON)));
    assert!(
        matches!(command, chrome::Command::AddToProject),
        "{ADD_BUTTON:?} is not on the add button any more: {command:?}",
    );
}

/// The pane's add button asks for the add, and only when there is one to make.
///
/// The button is `add_enabled(app.addable().is_ok(), …)`, so what it refuses is
/// refused by `egui` rather than by a branch here — but the mutation that
/// removes the guard is a real one and this catches it. Replacing the predicate
/// with `true` leaves both silent states below asking for `AddToProject`, which
/// is `add_to_project` rewriting a project with nothing to add in the first
/// case and adding a second entry for one image in the second. That is the
/// difference from `pressing_a_row_asks_for_that_row_and_only_that_row`'s
/// deliberately weak assertion: there the command was unreachable whatever the
/// widget did, and here it is one argument away.
///
/// Three states of the same button rather than one, because the claim is a
/// difference between them. That it stays quiet with nothing to add means
/// nothing unless it speaks when there is something.
#[test]
fn the_add_button_asks_only_when_there_is_something_to_add() {
    let Some(slide) = a_real_slide() else {
        eprintln!("skipping: data/CMU-1-Small-Region.svs not present");
        return;
    };

    // A project open and no slide showing: `addable` says so, and the button
    // carries that sentence as its disabled hover text.
    let mut app = App::new(None);
    app.open_project(project_in("add-button-no-slide", &[("first", true)]));
    wait_for_pictures(&mut app);
    let reason = app.addable().expect_err("a project but no slide");
    let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(ADD_BUTTON)));
    assert!(
        matches!(command, chrome::Command::None),
        "the button asked for the add with nothing to add ({reason}): {command:?}",
    );

    // A project and a slide it does not hold: the one state that asks.
    let directory = std::env::temp_dir().join("qupath-rust-test-add-button-live");
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a directory");
    let mut app = App::new(None);
    app.new_project(&directory);
    app.open(slide);
    assert!(
        app.open.is_some(),
        "the slide did not open: {}",
        app.message
    );
    wait_for_pictures(&mut app);
    app.addable().expect("a project and a slide not in it");
    let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(ADD_BUTTON)));
    assert!(
        matches!(command, chrome::Command::AddToProject),
        "the button did not ask for the add: {command:?}",
    );

    // The same slide, now in the project. Adding it twice is two entries for
    // one image, which QuPath shows as two rows; the button goes quiet again
    // without the slide having been closed.
    app.add_to_project();
    wait_for_pictures(&mut app);
    let reason = app
        .addable()
        .expect_err("the slide is already in the project");
    assert!(reason.contains("already in this project"), "{reason}");
    let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(ADD_BUTTON)));
    assert!(
        matches!(command, chrome::Command::None),
        "the button offered to add a slide the project already holds: {command:?}",
    );
}
