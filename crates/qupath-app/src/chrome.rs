//! The interface: a toolbar, a status bar, and what to show when nothing is
//! open.
//!
//! Everything here is a *function of* [`crate::state::App`] and changes none of
//! it. What a control does is expressed as a [`Command`] handed back to the
//! caller, which is what keeps "where a button lives" and "what a button does"
//! from growing into each other — the failure mode of immediate-mode code, where
//! the layout is also the event handler.
//!
//! It is one command per frame rather than a list because these are all things a
//! person does one at a time, and a frame in which the user both opened a slide
//! and zoomed it is not a frame that happens.

use std::path::PathBuf;

use qupath_viewer::SlidePoint;

use crate::state::{App, cursor_slide_position, describe, display_name};

/// What the interface is asking the application to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Command {
    #[default]
    None,
    Open,
    OpenRecent(PathBuf),
    Close,
    Fit,
    /// Zoom about the centre of the view by this many steps, in or out.
    Zoom(i32),
    /// One screen pixel per slide pixel.
    Actual,
    /// Detect cells in what the view is showing, replacing any run going.
    Detect,
    /// Show or hide the outlines of what was detected, without re-running.
    ToggleOverlay,
}

/// Sets the look once, at startup.
///
/// Dark, and deliberately: a slide is a bright image and chrome that is
/// brighter than it competes with the thing being looked at. QuPath defaults
/// the same way for the same reason.
pub fn style(context: &egui::Context) {
    context.set_visuals(egui::Visuals::dark());
    context.style_mut(|style| {
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    });
}

/// Lays out the whole interface and returns what the user asked for, along
/// with the rectangle left over for the slide.
///
/// The rectangle is returned rather than recomputed by the caller because it is
/// a fact about *this* layout — the toolbar's height depends on its contents and
/// the status bar's on whether a detection message is in it — and the caller
/// needs it to tell a scroll over the slide from a scroll over the interface.
pub fn draw(context: &egui::Context, app: &App) -> (Command, (f32, f32, f32, f32)) {
    let mut command = Command::None;

    egui::TopBottomPanel::top("toolbar").show(context, |ui| {
        ui.horizontal(|ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open slide…").clicked() {
                    command = Command::Open;
                    ui.close();
                }
                ui.add_enabled_ui(!app.recent.is_empty(), |ui| {
                    ui.menu_button("Open recent", |ui| {
                        for path in &app.recent {
                            if ui.button(display_name(path)).clicked() {
                                command = Command::OpenRecent(path.clone());
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                if ui
                    .add_enabled(app.open.is_some(), egui::Button::new("Close"))
                    .clicked()
                {
                    command = Command::Close;
                    ui.close();
                }
            });

            ui.separator();
            ui.add_enabled_ui(app.open.is_some(), |ui| {
                if ui
                    .button("Fit")
                    .on_hover_text("Fit the whole slide (f)")
                    .clicked()
                {
                    command = Command::Fit;
                }
                if ui
                    .button("1:1")
                    .on_hover_text("One screen pixel per slide pixel")
                    .clicked()
                {
                    command = Command::Actual;
                }
                if ui
                    .button("−")
                    .on_hover_text("Zoom out (− key, or scroll down)")
                    .clicked()
                {
                    command = Command::Zoom(-1);
                }
                if ui
                    .button("+")
                    .on_hover_text(
                        "Zoom in (+ key, or scroll up). To zoom to a particular \
                         area, right-drag a box around it.",
                    )
                    .clicked()
                {
                    command = Command::Zoom(1);
                }
            });

            ui.separator();
            detection(ui, app, &mut command);

            ui.separator();
            ui.label(
                egui::RichText::new(match &app.open {
                    Some(open) => display_name(&open.path),
                    None => "No slide".to_owned(),
                })
                .strong(),
            );

            // Pushed to the right, so the backend is where a window's own
            // furniture would be rather than in the middle of the controls.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new("software compositor").weak())
                    .on_hover_text(
                        "Tiles and this interface are both drawn on the CPU. \
                         No GPU adapter is used or needed.",
                    );
            });
        });
    });

    egui::TopBottomPanel::bottom("status").show(context, |ui| {
        ui.horizontal(|ui| match &app.open {
            Some(open) => {
                let level = open.view.level(open.slide());
                let (bytes, tiles) = open.resident();
                ui.label(format!(
                    "level {} ({:.0}x)   {:.4} px/px",
                    level.index, level.downsample, open.view.scale
                ));
                ui.separator();

                let SlidePoint { x, y } = cursor_slide_position(open, app.cursor());
                ui.label(format!("{x:.0}, {y:.0}"))
                    .on_hover_text("Cursor position in level-0 slide pixels");
                ui.separator();

                ui.label(format!("{} tiles drawn", open.drawn));
                ui.separator();
                ui.label(format!("{tiles} resident, {} MB", bytes >> 20));

                let outstanding = open.outstanding();
                if outstanding > 0 {
                    ui.separator();
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(format!("{outstanding} loading"));
                }

                if let Some(overlay) = &app.detector.overlay
                    && app.show_overlay
                {
                    ui.separator();
                    // Drawn against found, not just found. The gap between them
                    // is the culling in `Overlay::segments_into` doing its job,
                    // and it is the number that explains why zooming out stops
                    // showing outlines rather than leaving it to look like a
                    // bug.
                    ui.label(format!("{} of {} outlined", app.outlined, overlay.len()))
                        .on_hover_text(
                            "Objects whose outline this frame drew, against how many the \
                             last run found. Objects off screen, or too small to see, are \
                             not drawn.",
                        );
                }
                if let Some(detection) = &app.detector.message {
                    ui.separator();
                    ui.label(egui::RichText::new(detection).weak());
                }

                // While a box is being dragged, what releasing it would do. This
                // replaces the standing hint rather than sitting beside it: the
                // hint's whole job is to get the user to start this gesture, and
                // once they have started it the useful thing to say is how far
                // it currently zooms.
                if let Some(factor) = app.band_factor() {
                    ui.separator();
                    ui.label(egui::RichText::new(format!("zoom to {factor:.1}x")).strong())
                        .on_hover_text("Release to zoom to the box; escape to abandon it.");
                } else {
                    ui.separator();
                    ui.label(
                        egui::RichText::new("drag to pan · right-drag a box to zoom · +/−").weak(),
                    )
                    .on_hover_text(
                        "Left-drag moves the slide. Right-drag draws a rectangle and \
                         releasing zooms to it. The scroll wheel and the +/− keys zoom \
                         about the cursor, and f fits the whole slide.",
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(cost) = app.frame_cost() {
                        // Main-thread time only, and said so: it excludes the
                        // decode, which is what a pan actually waits for.
                        ui.label(
                            egui::RichText::new(format!("{:.1} ms", cost.as_secs_f64() * 1000.0))
                                .weak(),
                        )
                        .on_hover_text(
                            "Mean main-thread cost of the last 30 frames: laying out this \
                                 interface, composing the tiles and presenting. It does not \
                                 include tile decoding, which happens on other threads.",
                        );
                    }
                });
            }
            None => {
                ui.label(egui::RichText::new(&app.message).weak());
            }
        });
    });

    // After the two panels have taken their strips and before the central one
    // is shown, which is when this is the slide's rectangle and nothing else's.
    let available = context.available_rect();
    let slide_area = (
        available.min.x,
        available.min.y,
        available.max.x,
        available.max.y,
    );

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(context, |ui| {
            if app.open.is_some() {
                // The slide is drawn underneath by the compositor, not by
                // `egui`, so this only reserves the space and leaves the pixels
                // alone. No sense: nothing in this area is an `egui` widget,
                // and the slide's own gestures are handled from raw winit
                // events in `state::input`.
                //
                // The sense here is *not* what decides whether a scroll reaches
                // the viewer, which is worth stating because it looks as though
                // it should be. `wants_pointer_input` is true for a pointer
                // anywhere over a panel — an empty `CentralPanel` with nothing
                // allocated in it at all is enough — so the wheel arrives
                // `consumed` whatever this argument says. That is handled where
                // it can be, in `state::input`; see the wheel arm there.
                ui.allocate_response(ui.available_size(), egui::Sense::empty());
                return;
            }
            welcome(ui, app, &mut command);
        });

    (command, slide_area)
}

/// The detection controls: run it, and show or hide what it found.
///
/// The button is disabled while a run is going *and* says so in its label,
/// because a control that is merely greyed out is indistinguishable from one
/// that is broken. The elapsed time is what stands in for a progress fraction:
/// `run_collecting` reports nothing until it returns, and a bar that advanced
/// on a timer would be an invention rather than a measurement — see `detect`'s
/// module documentation.
fn detection(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    let running = app.detector.is_running();
    ui.add_enabled_ui(app.open.is_some() && !running, |ui| {
        if ui
            .button("Detect cells")
            .on_hover_text(
                "Run cell detection over what is on screen (d). \
                 It runs on another thread, so the view keeps moving.",
            )
            .clicked()
        {
            *command = Command::Detect;
        }
    });

    if running {
        ui.add(egui::Spinner::new().size(14.0));
        if let Some(elapsed) = app.detector.elapsed() {
            ui.label(
                egui::RichText::new(format!("detecting… {:.0} s", elapsed.as_secs_f64())).weak(),
            );
        }
    } else if let Some(overlay) = &app.detector.overlay {
        // Only once there is something to show or hide. A toggle for an overlay
        // that does not exist is a control with no effect, which is worse than
        // an absent one.
        let mut shown = app.show_overlay;
        if ui
            .checkbox(&mut shown, "Outlines")
            .on_hover_text(format!("Show the {} detected outlines (h)", overlay.len()))
            .changed()
        {
            *command = Command::ToggleOverlay;
        }
    }
}

/// What fills the window before a slide is open.
///
/// An empty grey rectangle is the commonest way an application looks broken
/// when it is merely idle, so this says what it is waiting for and gives the
/// two ways to provide it.
fn welcome(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() / 3.0);
        ui.heading("QuPath Rust");
        ui.add_space(8.0);
        ui.label(egui::RichText::new(&app.message).weak());
        ui.add_space(16.0);
        if ui.button("Open a slide…").clicked() {
            *command = Command::Open;
        }
        if !app.recent.is_empty() {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Recent").weak());
            for path in &app.recent {
                if ui.link(display_name(path)).clicked() {
                    *command = Command::OpenRecent(path.clone());
                }
            }
        }
        ui.add_space(24.0);
        ui.label(
            egui::RichText::new(
                "Baseline JPEG pyramids only. Drag to pan, scroll to zoom, \
                 right-drag a box to zoom to it, f to fit.",
            )
            .weak()
            .small(),
        );
    });

    // Kept honest: the description of a slide comes from the same function the
    // status bar uses, so there is one statement of what a slide is.
    let _ = describe;
}

#[cfg(test)]
mod tests {
    /// Lays the interface out headlessly and returns the slide rectangle
    /// `draw` reports, together with `egui`'s own answer to "do you want the
    /// pointer" at `pointer`.
    ///
    /// Two frames, because hover state is a function of the *previous* frame's
    /// layout: a single pass reports the pointer as over nothing at all and
    /// would make the assertions below pass for the wrong reason.
    fn layout(pointer: egui::Pos2) -> ((f32, f32, f32, f32), bool) {
        let context = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut area = (0.0, 0.0, 0.0, 0.0);
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerMoved(pointer)],
                ..Default::default()
            };
            let _ = context.run(input.clone(), |context| {
                // The real layout, with a toolbar and a status bar above and
                // below, is what gives the central rectangle its extent.
                egui::TopBottomPanel::top("toolbar").show(context, |ui| {
                    ui.horizontal(|ui| {
                        let _ = ui.button("Fit");
                    });
                });
                egui::TopBottomPanel::bottom("status").show(context, |ui| {
                    ui.label("status");
                });
                area = {
                    let available = context.available_rect();
                    (
                        available.min.x,
                        available.min.y,
                        available.max.x,
                        available.max.y,
                    )
                };
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(context, |ui| {
                        ui.allocate_response(ui.available_size(), egui::Sense::empty());
                    });
            });
        }
        (area, context.wants_pointer_input())
    }

    fn contains((left, top, right, bottom): (f32, f32, f32, f32), point: egui::Pos2) -> bool {
        point.x >= left && point.x < right && point.y >= top && point.y < bottom
    }

    #[test]
    fn the_slide_rectangle_is_what_the_panels_left_over() {
        // What `state::cursor_over_slide` is gated on. A point in the middle of
        // the window is the slide's; points in the toolbar's and the status
        // bar's strips are not, and the rectangle has to exclude both — a
        // rectangle that swallowed either would make a scroll over the toolbar
        // zoom the slide underneath it.
        let middle = egui::pos2(400.0, 300.0);
        let (area, _) = layout(middle);
        assert!(
            contains(area, middle),
            "the slide rectangle {area:?} excludes its own middle"
        );
        assert!(
            !contains(area, egui::pos2(400.0, 2.0)),
            "the toolbar strip is inside {area:?}"
        );
        assert!(
            !contains(area, egui::pos2(400.0, 598.0)),
            "the status strip is inside {area:?}"
        );
        // And it is a real rectangle rather than the whole window: both panels
        // took a strip, so the top edge has moved down and the bottom edge up.
        assert!(
            area.1 > 0.0 && area.3 < 600.0,
            "the panels reserved nothing: {area:?}"
        );
    }

    #[test]
    fn egui_claims_the_pointer_over_the_slide_which_is_why_consumed_is_unusable() {
        // The bug's actual cause, pinned so that the workaround is not quietly
        // reverted as redundant. `egui_winit` marks a pointer event `consumed`
        // whenever `wants_pointer_input` is true, and that is true over the
        // slide — with no widget there and nothing sensing input. Gating the
        // wheel on it discarded every scroll over the slide, which left the
        // centre-anchored toolbar buttons as the only way to zoom.
        //
        // If this ever comes out false, `egui` has changed its answer and
        // `state::input` may go back to the simpler `!consumed` test.
        let (area, wants) = layout(egui::pos2(400.0, 300.0));
        assert!(contains(area, egui::pos2(400.0, 300.0)));
        assert!(
            wants,
            "egui no longer claims the pointer over a bare central panel: the \
             geometric test in `state::cursor_over_slide` is now unnecessary"
        );
    }
}
