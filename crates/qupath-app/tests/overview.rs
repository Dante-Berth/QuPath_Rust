//! The overview map's click-to-jump, driven by a pointer rather than by
//! arithmetic.
//!
//! `docs/dev/VIEWER.md` §8.4 states the property this pane has to have — the two
//! mappings must agree, so clicking the centre of the drawn rectangle does not
//! move the view — and `overview.rs` has a unit test for it. That test checks
//! `Overview`'s arithmetic. It cannot see the pane, and the pane is where two
//! things live that the arithmetic does not have: the *offset* of the picture
//! inside a panel, and a `scale` the picture is fitted to the panel by. A click
//! goes through both before it reaches `to_slide`.
//!
//! The second of those is why this file exists rather than being a paragraph in
//! §8.4. `scale` is `(available / width).min(1.0)`, and in every window anyone
//! runs the application in it is exactly **1.0** — the panel asks for 240 points
//! and the picture is 112 wide. A factor of one hides a bug the way a
//! downsample of one does: dividing by it, multiplying by it and forgetting it
//! are three programs that agree. `a_pane_narrower_than_the_picture_still_lands
//! _where_it_was_aimed` is the one test here that runs with a factor that is not
//! one, and it is the reason the others can be trusted.
//!
//! §8.8's hole is what this closes: click-to-jump was reachable "only with a
//! hand on the machine". It was not — `chrome::draw` returns the `Command` a
//! press produced, which is the same route the browser's rows came out of the
//! hole by.

use std::path::{Path, PathBuf};

use qupath_app::chrome::{self, Command};
use qupath_app::state::App;
use qupath_viewer::viewport::Viewport;
use qupath_viewer::{ScreenPoint, SlidePoint};

mod gestures;

use gestures::{Gesture, lay_out, lay_out_in};

/// The slide these tests open, or `None` when it is not fetched.
///
/// A real one, and it has to be: the picture is read from the coarsest level of
/// an actual pyramid, and the mapping under test is between that picture's size
/// and the slide's. A stub would need both of those invented, which is to
/// invent the answer.
fn a_real_slide() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/CMU-1-Small-Region.svs");
    path.is_file().then_some(path)
}

/// An application with that slide open and the pane holding the overview
/// showing.
///
/// `show_metadata` is off by default and the overview lives inside that panel,
/// so a test that forgot this line would lay out an interface with no picture
/// in it — and every assertion below is about a picture that was found, which
/// is why they `expect` it rather than skipping when it is absent.
fn showing_the_overview() -> Option<App> {
    let slide = a_real_slide()?;
    let mut app = App::new(None);
    app.open(slide);
    assert!(
        app.open.is_some(),
        "the slide did not open: {}",
        app.message
    );
    app.show_metadata = true;
    app.show_overview = true;
    Some(app)
}

/// The rectangle the overview's picture was drawn into, in points.
///
/// Read off the frame rather than written down as a constant, which is what
/// makes `ROWS`' guard test unnecessary here: a layout change moves the
/// rectangle and moves every click in this file with it. What a layout change
/// *cannot* do is leave the picture undrawn — [`expect`] on the `Option` is
/// what catches that, and it is not a formality, since a pane that quietly
/// stopped drawing the map would otherwise turn every "clicking here asks for
/// nothing" assertion into a test of an empty panel.
///
/// The quad carries [`chrome::OVERVIEW_TEXTURE`], which is how it is told from
/// the browser's row pictures: those are meshes too, with a texture id each.
fn picture(shapes: &[egui::epaint::ClippedShape]) -> Option<egui::Rect> {
    fn walk(shape: &egui::Shape, into: &mut Vec<egui::Rect>) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(shape, into);
                }
            }
            egui::Shape::Mesh(mesh)
                if mesh.texture_id == egui::TextureId::User(chrome::OVERVIEW_TEXTURE) =>
            {
                into.push(mesh.calc_bounds());
            }
            _ => {}
        }
    }

    let mut found = Vec::new();
    for clipped in shapes {
        walk(&clipped.shape, &mut found);
    }
    assert!(
        found.len() <= 1,
        "the overview texture was drawn {} times in one frame, so the tests below \
         cannot say which quad they clicked: {found:?}",
        found.len(),
    );
    found.pop()
}

/// The red rectangle showing what is on screen, in points, if it was drawn.
///
/// Filtered by the stroke colour rather than by position, because position is
/// the thing under test. QuPath's own `rgba(200,0,0,0.8)` — `overview
/// ::RECT_COLOUR` — is what the pane strokes it with, and the only other
/// rectangle it strokes is the grey border at `from_gray(64)`.
fn visible_rectangle(shapes: &[egui::epaint::ClippedShape]) -> Option<egui::Rect> {
    fn walk(shape: &egui::Shape, into: &mut Vec<egui::Rect>) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(shape, into);
                }
            }
            egui::Shape::Rect(rect)
                if rect.stroke.color == egui::Color32::from_rgb(200, 0, 0)
                    && rect.stroke.width > 0.0 =>
            {
                into.push(rect.rect);
            }
            _ => {}
        }
    }

    let mut found = Vec::new();
    for clipped in shapes {
        walk(&clipped.shape, &mut found);
    }
    found.pop()
}

/// The picture's size in its own pixels, which is what a point in it is a
/// fraction of.
fn picture_size(app: &App) -> (f32, f32) {
    let thumbnail = app
        .open
        .as_ref()
        .and_then(|open| open.overview())
        .expect("the slide has an overview");
    let (width, height) = thumbnail.overview.size();
    (width as f32, height as f32)
}

/// The slide's level-0 extent, which is what a jump is asserted against.
fn slide_size(app: &App) -> (f64, f64) {
    let level = &app.open.as_ref().expect("a slide is open").slide().levels()[0];
    (f64::from(level.width), f64::from(level.height))
}

/// Where a `JumpTo` asked to go, or a failure naming what came instead.
fn jumped_to(command: &Command) -> SlidePoint {
    match command {
        Command::JumpTo(centre) => *centre,
        other => panic!("the press did not ask to jump: {other:?}"),
    }
}

/// The tolerance a jump is checked to, in level-0 pixels: one pixel of the
/// picture, which is the finest thing a click on it can mean.
fn within_one_picture_pixel(app: &App) -> (f64, f64) {
    let (columns, rows) = picture_size(app);
    let (width, height) = slide_size(app);
    (width / f64::from(columns), height / f64::from(rows))
}

/// The picture is drawn once, at its own size, in a window with room for it.
///
/// The guard the rest of the file rests on, and it is two claims rather than
/// one. *Drawn once*: a second quad with the same texture would make every
/// click below ambiguous. *At its own size*: the ordinary case has `scale`
/// exactly 1.0, which is the case that hides a mistake in the division — so
/// this pins that the ordinary case really is the one with no scaling, and
/// `a_pane_narrower_than_the_picture_still_lands_where_it_was_aimed` is what
/// then says the other case is reached at all.
#[test]
fn the_overview_is_drawn_once_and_at_its_own_size() {
    let Some(mut app) = showing_the_overview() else {
        qupath_testing::skipped("data/CMU-1-Small-Region.svs not present");
        return;
    };

    let (shapes, _) = lay_out(&mut app, None);
    let quad = picture(&shapes).expect("the overview picture was drawn");
    let (width, height) = picture_size(&app);

    assert!(
        (quad.width() - width).abs() < 0.5 && (quad.height() - height).abs() < 0.5,
        "a panel with room for the picture should draw it at its own size — \
         {width}x{height} against a quad of {}x{}",
        quad.width(),
        quad.height(),
    );
}

/// A click on the picture asks to centre on the point that was clicked.
///
/// Checked against a fraction of the *slide*, not against `to_slide` — running
/// the mapping the pane runs and comparing the two answers is a test of nothing
/// at all. Three points rather than one, and none of them the middle: the middle
/// of a picture is where a transposed mapping, a mirrored one and a correct one
/// all agree, which is `zooming_in_never_grows_the_rectangle`'s reason in
/// §8.4 met from the other side.
#[test]
fn clicking_the_overview_asks_to_centre_on_what_was_clicked() {
    let Some(mut app) = showing_the_overview() else {
        qupath_testing::skipped("data/CMU-1-Small-Region.svs not present");
        return;
    };

    let (shapes, _) = lay_out(&mut app, None);
    let quad = picture(&shapes).expect("the overview picture was drawn");
    let (width, height) = slide_size(&app);
    let (tolerance_x, tolerance_y) = within_one_picture_pixel(&app);

    // Off-centre and asymmetric between the axes, so a swap of x and y is a
    // failure rather than a coincidence.
    for (across, down) in [(0.25, 0.75), (0.75, 0.25), (0.5, 0.9)] {
        let at = quad.min + egui::vec2(quad.width() * across, quad.height() * down);
        let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(at)));
        let centre = jumped_to(&command);

        let (want_x, want_y) = (width * f64::from(across), height * f64::from(down));
        assert!(
            (centre.x - want_x).abs() < tolerance_x && (centre.y - want_y).abs() < tolerance_y,
            "a click {across} across and {down} down the picture should ask for \
             ({want_x:.0}, {want_y:.0}) on the slide, and asked for \
             ({:.0}, {:.0})",
            centre.x,
            centre.y,
        );
    }
}

/// Clicking the centre of the drawn rectangle does not move the view.
///
/// §8.4's invariant, through the pane this time. The two mappings it is about
/// are `visible_rect`, which puts the rectangle on the picture, and `to_slide`,
/// which turns a click back into a centre — and *neither is individually
/// defensible*: each is a plausible program at any single scale, and an
/// inverted or transposed one is only visible where the two meet. Where they
/// meet, for a user, is this gesture.
///
/// What the pane adds to the unit test of the same property is the offset and
/// the fit: the rectangle is drawn at `quad.min + visible * scale` and the click
/// is read back at `(position - quad.min) / scale`. Those two have to be
/// inverses. Dropping either factor from one side alone leaves a program that
/// still draws a rectangle in a plausible place and still jumps somewhere
/// plausible when clicked.
///
/// The view is set here rather than fitted, because at `Viewport::fit` QuPath
/// draws no rectangle at all — "in which case everything is visible" — so the
/// gesture under test does not exist until something is zoomed in.
#[test]
fn clicking_the_visible_rectangles_centre_does_not_move_the_view() {
    let Some(mut app) = showing_the_overview() else {
        qupath_testing::skipped("data/CMU-1-Small-Region.svs not present");
        return;
    };

    // Well inside the slide on both axes, so the rectangle is not clipped to
    // the picture's edge — a clipped rectangle has a centre that is honestly
    // not the view's, and this test would then be asserting the wrong thing
    // about correct code.
    let centre = SlidePoint::new(1000.0, 1500.0);
    let view = Viewport {
        centre,
        scale: 2.0,
        width: 1280,
        height: 860,
    };
    app.open.as_mut().expect("a slide is open").view = view;

    let (shapes, _) = lay_out(&mut app, None);
    let quad = picture(&shapes).expect("the overview picture was drawn");
    let rectangle = visible_rectangle(&shapes)
        .expect("a view this far in shows part of the slide, so the rectangle is drawn");
    assert!(
        quad.contains_rect(rectangle),
        "the rectangle should be inside the picture: {rectangle:?} in {quad:?}",
    );

    let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(rectangle.center())));
    let asked = jumped_to(&command);
    let (tolerance_x, tolerance_y) = within_one_picture_pixel(&app);

    assert!(
        (asked.x - centre.x).abs() < tolerance_x && (asked.y - centre.y).abs() < tolerance_y,
        "clicking the centre of the drawn rectangle must not move the view: it was at \
         ({:.0}, {:.0}) and the click asked for ({:.0}, {:.0})",
        centre.x,
        centre.y,
        asked.x,
        asked.y,
    );
}

/// A pane narrower than the picture still lands where it was aimed.
///
/// The one test here with a `scale` that is not 1.0, and the reason the file's
/// header says the others cannot stand alone. `overview_pane` fits the picture
/// to the panel — deliberately, because a picture that overflows a resized
/// panel is *clipped* rather than scaled, which loses the right-hand edge of
/// the slide without saying so — and then divides the click position back by
/// the same factor. In every window the application is run in that factor is
/// one, so both the division and its absence are correct programs there.
///
/// The narrow window is what separates them: at these dimensions the panel is
/// capped to half the window between the panels that are open, the picture is
/// fitted down to fit it, and a missing division misses by 1/scale — about a
/// third of the slide here, which is a click on tissue landing on background.
///
/// The first assertion is the witness. Without it a change that stopped fitting
/// the picture would leave the rest of the test passing at scale 1.0, testing
/// the case the other tests already cover and reporting it as this one.
#[test]
fn a_pane_narrower_than_the_picture_still_lands_where_it_was_aimed() {
    let Some(mut app) = showing_the_overview() else {
        qupath_testing::skipped("data/CMU-1-Small-Region.svs not present");
        return;
    };

    let window = egui::vec2(200.0, 700.0);
    let (shapes, _) = lay_out_in(&mut app, window, None);
    let quad = picture(&shapes).expect("the overview picture was drawn");
    let (natural, _) = picture_size(&app);
    assert!(
        quad.width() < natural - 1.0,
        "this test is only worth running where the picture had to be fitted, and it \
         was drawn at its natural {natural} points wide in a {} point window",
        window.x,
    );

    let (width, height) = slide_size(&app);
    let (tolerance_x, tolerance_y) = within_one_picture_pixel(&app);
    for (across, down) in [(0.25, 0.75), (0.75, 0.25)] {
        let at = quad.min + egui::vec2(quad.width() * across, quad.height() * down);
        let (_, command) = lay_out_in(&mut app, window, Some(Gesture::Clicking(at)));
        let centre = jumped_to(&command);

        let (want_x, want_y) = (width * f64::from(across), height * f64::from(down));
        assert!(
            (centre.x - want_x).abs() < tolerance_x && (centre.y - want_y).abs() < tolerance_y,
            "in a fitted picture a click {across} across and {down} down should still \
             ask for ({want_x:.0}, {want_y:.0}), and asked for ({:.0}, {:.0}) — a jump \
             scaled by the fit is a click on tissue landing on background",
            centre.x,
            centre.y,
        );
    }
}

/// Holding the button and moving scrubs the view to where the pointer is.
///
/// `ImageOverview` binds `mouseViewerToLocation` to both the press and the
/// drag, so holding the button moves the view across the slide rather than
/// needing one click per jump, and `overview_pane` copies that with
/// `response.clicked() || response.dragged()`. The half of it worth pinning is
/// which position the drag reports: `interact_pointer_pos` during a drag is
/// where the pointer *is*, and a program that used the press position instead
/// would jump once and then stick — a scrub that ignores the hand moving, which
/// looks like a dropped frame rather than like a bug.
#[test]
fn dragging_across_the_overview_scrubs_to_where_the_pointer_is() {
    let Some(mut app) = showing_the_overview() else {
        qupath_testing::skipped("data/CMU-1-Small-Region.svs not present");
        return;
    };

    let (shapes, _) = lay_out(&mut app, None);
    let quad = picture(&shapes).expect("the overview picture was drawn");
    let point =
        |across: f32, down: f32| quad.min + egui::vec2(quad.width() * across, quad.height() * down);

    // Asymmetric in the picture's *pixels* and not merely in its fractions,
    // which is a distinction this test learned the hard way: at (0.8, 0.6) of a
    // 112x150 picture the two axes are 89.6 and 90.0 points, so a mutation that
    // swapped x for y moved the jump by eight slide pixels and the test passed
    // through it. The other three tests here caught that swap; this one has to
    // catch it on its own, because it is the only one that drags.
    let (_, command) = lay_out(
        &mut app,
        Some(Gesture::Dragging {
            from: point(0.2, 0.2),
            to: point(0.85, 0.25),
        }),
    );
    let centre = jumped_to(&command);

    let (width, height) = slide_size(&app);
    let (tolerance_x, tolerance_y) = within_one_picture_pixel(&app);
    let (want_x, want_y) = (width * 0.85, height * 0.25);
    assert!(
        (centre.x - want_x).abs() < tolerance_x && (centre.y - want_y).abs() < tolerance_y,
        "a drag should follow the pointer to ({want_x:.0}, {want_y:.0}) and asked for \
         ({:.0}, {:.0}) — the press position is ({:.0}, {:.0})",
        centre.x,
        centre.y,
        width * 0.2,
        height * 0.2,
    );
}

/// A press beside the picture asks for nothing.
///
/// The outward-facing half of the tests above, and the same one
/// `pressing_below_the_rows_asks_for_nothing` makes about the browser: a pane
/// that answered a press anywhere in it would put the view somewhere the user
/// did not point at, and a `JumpTo` is silent — the picture simply shows
/// somewhere else.
///
/// Below the picture rather than beside it, because the panel is barely wider
/// than the picture and the label under it — "Click to jump" — is exactly what
/// a person aims at when they have misread the instruction.
#[test]
fn pressing_beside_the_picture_asks_for_nothing() {
    let Some(mut app) = showing_the_overview() else {
        qupath_testing::skipped("data/CMU-1-Small-Region.svs not present");
        return;
    };

    let (shapes, _) = lay_out(&mut app, None);
    let quad = picture(&shapes).expect("the overview picture was drawn");

    let at = egui::pos2(quad.center().x, quad.max.y + 6.0);
    let (_, command) = lay_out(&mut app, Some(Gesture::Clicking(at)));
    assert_eq!(
        command,
        Command::None,
        "a press under the picture asked for something",
    );
}

/// The two mappings the pane composes are inverses of each other.
///
/// The property in the form §8.4 states it, rather than through one gesture:
/// take a point on the slide, put it on the picture with `to_overview`, and
/// read it back with `to_slide`. The tests above are what say the *pane* uses
/// these two; this is what says the two agree, at points spread across the
/// slide rather than at its middle, where a transposed mapping agrees with a
/// correct one.
///
/// Cheap enough to be worth having beside the gesture tests even though it
/// needs no layout: when one of them fails, this is what says whether the
/// arithmetic or the pane moved.
#[test]
fn the_two_mappings_are_inverses_across_the_picture() {
    let Some(app) = showing_the_overview() else {
        qupath_testing::skipped("data/CMU-1-Small-Region.svs not present");
        return;
    };

    let thumbnail = app
        .open
        .as_ref()
        .and_then(|open| open.overview())
        .expect("the slide has an overview");
    let (width, height) = slide_size(&app);
    let (tolerance_x, tolerance_y) = within_one_picture_pixel(&app);

    for (across, down) in [(0.1, 0.9), (0.9, 0.1), (0.5, 0.25), (0.25, 0.5)] {
        let point = SlidePoint::new(width * across, height * down);
        let on_picture: ScreenPoint = thumbnail.overview.to_overview(point);
        let back = thumbnail.overview.to_slide(on_picture);
        assert!(
            (back.x - point.x).abs() < tolerance_x && (back.y - point.y).abs() < tolerance_y,
            "({:.0}, {:.0}) went to the picture and came back as ({:.0}, {:.0})",
            point.x,
            point.y,
            back.x,
            back.y,
        );
    }
}
