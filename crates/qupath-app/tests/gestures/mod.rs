//! Pointer gestures delivered to `chrome::draw`, with no window and no GPU.
//!
//! `docs/VIEWER.md` §8.8 drew its hole around the wrong thing: what [`App`]
//! cannot reach is not the click but the *pixel position*, and `chrome::draw`
//! returns the `Command` a press produced, so the whole chain from a coordinate
//! to what the application is asked to do closes without a window. This is that
//! machinery.
//!
//! Shared rather than copied, and that is the point of the file. The browser's
//! rows had it first, in the test file that drives them; the second thing to
//! reach a pointer at a coordinate wants the same six frames, the same "not on
//! the first" and the same repeat-every-frame — three decisions whose *reasons*
//! live in the comments below and nowhere else. A second copy is a second place
//! for those reasons to stop being true.
//!
//! A subdirectory module rather than `tests/gestures.rs`, because `cargo test`
//! makes a binary of every file directly in `tests/` and this one has no tests
//! in it.
#![allow(
    dead_code,
    reason = "compiled into each test binary that declares it, and no one of them uses all of it"
)]

use qupath_app::chrome;
use qupath_app::state::App;

/// The window these tests lay the interface out in, in points.
///
/// Large enough that every panel gets the width it asks for, so a coordinate
/// read off this layout is read off the layout a user has. [`lay_out_in`] is
/// for the tests that specifically want a *small* window — see the overview's,
/// where the pane being narrower than the picture is the case under test.
pub const WINDOW: egui::Vec2 = egui::vec2(1100.0, 760.0);

/// What the pointer does before the layout is read.
///
/// Two gestures rather than a position and a flag, for `examples/chrome`'s
/// reason: a hover is a `PointerMoved` and nothing else, and a click is that
/// move plus a press and a release. They are not one gesture with a modifier.
#[derive(Clone, Copy)]
pub enum Gesture {
    /// The pointer is moved here and left there.
    Hovering(egui::Pos2),
    /// The pointer is moved here, pressed and released.
    Clicking(egui::Pos2),
}

impl Gesture {
    /// Where the pointer ends up.
    pub fn at(self) -> egui::Pos2 {
        match self {
            Self::Hovering(at) | Self::Clicking(at) => at,
        }
    }
}

/// Lays the interface out in [`WINDOW`] with `gesture` delivered, and returns
/// what the frame drew and what it asked for.
pub fn lay_out(
    app: &mut App,
    gesture: Option<Gesture>,
) -> (Vec<egui::epaint::ClippedShape>, chrome::Command) {
    lay_out_in(app, WINDOW, gesture)
}

/// [`lay_out`] in a window of a given size.
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
pub fn lay_out_in(
    app: &mut App,
    window: egui::Vec2,
    gesture: Option<Gesture>,
) -> (Vec<egui::epaint::ClippedShape>, chrome::Command) {
    let context = egui::Context::default();
    chrome::style(&context);
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), window)),
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
            match gesture {
                Gesture::Hovering(at) => input.events.push(egui::Event::PointerMoved(at)),
                Gesture::Clicking(at) => {
                    input.events.push(egui::Event::PointerMoved(at));
                    input.events.push(button(at, true));
                    input.events.push(button(at, false));
                }
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

/// The primary button going down or coming up at a position.
fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}
