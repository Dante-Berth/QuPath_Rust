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

use qupath_pipeline::Micron;
use qupath_viewer::SlidePoint;

use crate::state::{App, cursor_slide_position, describe, display_name};

/// What the interface is asking the application to do.
///
/// `Eq` is deliberately absent: [`Command::SetTuning`] carries floats, and a
/// derived `Eq` on them is the one that does not mean what it says.
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// Show or hide the analysis panel.
    ToggleAnalysis,
    /// Show or hide the slide-metadata panel.
    ToggleMetadata,
    /// Show or hide the gesture and key list.
    ToggleHelp,
    /// Replace the detection parameters.
    ///
    /// The whole struct rather than one field, because it keeps [`Command`] one
    /// value per frame — see the module documentation — and because a slider
    /// being dragged produces a new value every frame anyway, so there is
    /// nothing to accumulate.
    ///
    /// This is what lets `draw` keep taking `&App`: the panel reads the current
    /// parameters, shows the edit the user just made, and hands the result back
    /// rather than writing through a `&mut`. The alternative is
    /// `draw(&mut App)`, which would give the layout code the ability to change
    /// anything and lose the property this module is built on.
    SetTuning(Micron),
    /// Put the detection parameters back to QuPath's defaults.
    ResetTuning,
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

/// The fraction of the window the side panels may take between them.
///
/// The slide keeps the rest. Half is chosen because the slide is what the
/// application is for: a panel is being read while the tissue is being looked
/// at, so the tissue keeps the larger share whenever the two compete.
const PANEL_SHARE: f32 = 0.5;

/// How many side panels are open.
fn open_panels(app: &App) -> u32 {
    u32::from(app.show_analysis) + u32::from(app.show_metadata) + u32::from(app.show_help)
}

/// The widest a single side panel may be, given the window and how many panels
/// share it.
///
/// The cap binds each panel, so it is the *total* share divided between however
/// many are open: three panels at a 1280px window get 213 each rather than 640
/// each, which is what makes the sum a bound rather than a bound on the widest
/// one. Capping each panel at the whole share instead is the mistake that looks
/// right — with three panels of 260, 240 and 280 it binds only the last.
///
/// Pure, and separate from [`draw`], so the arithmetic that stopped the slide
/// rectangle going negative can be asserted without laying anything out.
fn panel_cap(window: f32, panels: u32) -> f32 {
    if panels == 0 {
        return 0.0;
    }
    window * PANEL_SHARE / panels as f32
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

            ui.menu_button("View", |ui| {
                // Checkboxes rather than buttons: these are states, and a
                // menu entry that does not say whether the thing is currently
                // showing makes the user open the panel to find out.
                let mut analysis = app.show_analysis;
                if ui.checkbox(&mut analysis, "Analysis").clicked() {
                    command = Command::ToggleAnalysis;
                    ui.close();
                }
                let mut metadata = app.show_metadata;
                if ui.checkbox(&mut metadata, "Slide metadata").clicked() {
                    command = Command::ToggleMetadata;
                    ui.close();
                }
                let mut help = app.show_help;
                if ui.checkbox(&mut help, "Gestures and keys").clicked() {
                    command = Command::ToggleHelp;
                    ui.close();
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

    // The side panels go in *before* `available_rect` is read below, which is
    // the whole contract that keeps `cursor_over_slide` honest: a panel added
    // after that line would sit over the slide's rectangle, and every gesture
    // would treat a click on it as a click on the tissue. `docs/VIEWER.md` §3.7
    // records what that class of bug cost the last time.
    // Each panel is also capped, because a `default_width` is a request made
    // without knowledge of the other panels: 260 + 240 + 280 is 780 of an 800px
    // window, and what came out was a slide rectangle *130 pixels wide in the
    // negative* — an inverted rectangle that `cursor_over_slide` rejects every
    // point of, so every gesture over the slide silently stopped working. The
    // cap is a share of the window rather than a constant, so the panels shrink
    // together on a small window and are untouched on a large one.
    //
    // The cap is applied by clamping the width *before* handing it over, rather
    // than through `SidePanel::max_width`. `default_width` widens the range to
    // include whatever it is given (`egui` 0.33 `panel.rs:178`), and a panel's
    // width is remembered across frames besides, so the two combined do not
    // reliably bind. Doing the arithmetic here means the number that runs is the
    // number `panel_cap` returned.
    let cap = panel_cap(context.content_rect().width(), open_panels(app));
    if app.show_analysis {
        egui::SidePanel::right("analysis")
            .default_width(260.0_f32.min(cap))
            .max_width(cap)
            .show(context, |ui| analysis(ui, app, &mut command));
    }
    if app.show_metadata {
        egui::SidePanel::left("metadata")
            .default_width(240.0_f32.min(cap))
            .max_width(cap)
            .show(context, |ui| metadata(ui, app));
    }
    if app.show_help {
        egui::SidePanel::left("help")
            .default_width(280.0_f32.min(cap))
            .max_width(cap)
            .show(context, help);
    }

    // After the panels have taken their strips and before the central one
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

/// The analysis panel: QuPath's cell-detection parameters, in QuPath's units.
///
/// # Why microns, and why the pixel value beside them
///
/// QuPath's dialog is in microns and so is this, so a number a user knows
/// transfers. But the pipeline computes in **pixels**, and the conversion needs
/// the slide's pixel size — so the panel shows both: the micron value that is
/// being edited, and the pixel value that will actually run. A parameter whose
/// effect depends on a number the interface does not show is one the user cannot
/// reason about, and the pixel column is what makes "8 µm" mean something
/// specific on this slide rather than in general.
///
/// **Only the six parameters this pipeline implements appear.** QuPath's median
/// filter radius, cell expansion and boundary smoothing are not implemented, and
/// a slider for one of them would be a control that silently does nothing — the
/// user's reasonable conclusion being that detection is broken. `VIEWER.md` §8.1
/// has the full table.
fn analysis(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    ui.heading("Analysis");
    ui.label(
        egui::RichText::new("Cell detection parameters, as QuPath names them.")
            .weak()
            .small(),
    );
    ui.separator();

    // The slide's own pixel size, which every micron value converts through.
    // Stated rather than assumed: a slide with no `MPP` runs on the built-in
    // defaults, and the user has to be able to tell which of the two is
    // happening — otherwise the µm labels are a claim the run does not honour.
    let mpp = app
        .open
        .as_ref()
        .and_then(|open| open.slide().microns_per_pixel());
    match mpp {
        Some(mpp) => {
            ui.label(format!("{mpp:.4} µm/pixel"))
                .on_hover_text("Read from the slide's Aperio description (MPP).");
        }
        None => {
            ui.label(egui::RichText::new("Pixel size unknown").weak())
                .on_hover_text(
                    "This slide records no MPP, so microns cannot be converted \
                     to pixels. Detection runs with the built-in defaults and \
                     the values below are not applied.",
                );
        }
    }
    ui.separator();

    let mut tuning = app.tuning;
    let mut changed = false;
    // Every control is disabled without a pixel size, because without one the
    // values below are not what runs. Greyed out and explained above beats a
    // slider that moves and changes nothing.
    ui.add_enabled_ui(mpp.is_some(), |ui| {
        ui.label(egui::RichText::new("Nucleus parameters").strong());
        changed |= slider(
            ui,
            &mut tuning.background_radius,
            0.0..=30.0,
            "Background radius",
            "µm",
            mpp,
            Unit::Length,
            "Radius for background estimation; should exceed the largest nucleus \
             radius. Zero turns background subtraction off.",
        );
        changed |= slider(
            ui,
            &mut tuning.sigma,
            0.0..=10.0,
            "Sigma",
            "µm",
            mpp,
            Unit::Length,
            "Gaussian smoothing before the Laplacian. Larger values stop nuclei \
             fragmenting but blur their boundaries.",
        );
        changed |= slider(
            ui,
            &mut tuning.min_area,
            0.0..=200.0,
            "Minimum area",
            "µm²",
            mpp,
            Unit::Area,
            "Detected nuclei smaller than this are discarded.",
        );
        changed |= slider(
            ui,
            &mut tuning.max_area,
            0.0..=2000.0,
            "Maximum area",
            "µm²",
            mpp,
            Unit::Area,
            "Detected nuclei larger than this are discarded.",
        );

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Intensity parameters").strong());
        // Dimensionless, so no pixel column: these are optical densities and
        // mean the same thing at any pixel size.
        changed |= ui
            .add(
                egui::Slider::new(&mut tuning.intensity_threshold, 0.0..=1.0)
                    .text("Threshold")
                    .fixed_decimals(3),
            )
            .on_hover_text(
                "A candidate's mean haematoxylin optical density must exceed \
                 this to survive. Dimensionless, so it does not scale with the \
                 pixel size.",
            )
            .changed();
        let mut max_background = f64::from(tuning.max_background);
        if ui
            .add(
                egui::Slider::new(&mut max_background, 0.0..=5.0)
                    .text("Max background")
                    .fixed_decimals(2),
            )
            .on_hover_text(
                "Nuclei sitting on a background above this are discarded. \
                 Only has an effect when the background radius is above zero.",
            )
            .changed()
        {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a slider bounded to 0..=5, which f32 holds exactly enough of"
            )]
            {
                tuning.max_background = max_background as f32;
            }
            changed = true;
        }
    });

    ui.separator();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(app.open.is_some() && !app.detector.is_running(), |ui| {
            if ui
                .button("Detect cells")
                .on_hover_text("Run with these parameters over what is on screen (d).")
                .clicked()
            {
                *command = Command::Detect;
            }
        });
        if ui
            .button("Reset")
            .on_hover_text("Back to QuPath's defaults.")
            .clicked()
        {
            *command = Command::ResetTuning;
        }
    });

    // After the buttons, so a click on one of them wins the frame. Both cannot
    // be reported at once — one command per frame, per this module's
    // documentation — and a dropped slider frame is invisible where a dropped
    // button press is not.
    if changed && *command == Command::None {
        *command = Command::SetTuning(tuning);
    }

    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "QuPath's median filter, cell expansion and boundary smoothing are \
             not implemented here, so they are not shown.",
        )
        .weak()
        .small(),
    );
}

/// Whether a parameter is a length or an area, which is how it converts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    /// Divides by the pixel size once.
    Length,
    /// Divides by the pixel size **twice**.
    Area,
}

/// One micron-valued slider, with the pixel value it converts to beside it.
///
/// The pixel column is the point: an area divides by the *square* of the pixel
/// size where a length divides by it once, and showing the result is what makes
/// that visible rather than a thing to be trusted.
#[expect(
    clippy::too_many_arguments,
    reason = "a slider's label, unit, range, tooltip and conversion; splitting \
              them into a struct would be more ceremony than the four calls save"
)]
fn slider(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    label: &str,
    unit: &str,
    mpp: Option<f64>,
    kind: Unit,
    hover: &str,
) -> bool {
    let changed = ui
        .add(
            egui::Slider::new(value, range)
                .text(format!("{label} ({unit})"))
                .fixed_decimals(1),
        )
        .on_hover_text(hover)
        .changed();
    if let Some(mpp) = mpp {
        let pixels = match kind {
            Unit::Length => *value / mpp,
            Unit::Area => *value / (mpp * mpp),
        };
        let suffix = if kind == Unit::Area { "px²" } else { "px" };
        ui.label(
            egui::RichText::new(format!("    = {pixels:.1} {suffix}"))
                .weak()
                .small(),
        )
        .on_hover_text("What this slide's pixel size makes of the value above.");
    }
    changed
}

/// The slide-metadata panel: what the file says about itself.
///
/// Everything here is read from the slide rather than computed, and it exists
/// because `VIEWER.md` §8.0's pixel-size fix is otherwise invisible — the number
/// every micron parameter divides by now has somewhere to be seen.
fn metadata(ui: &mut egui::Ui, app: &App) {
    ui.heading("Slide");
    ui.separator();
    let Some(open) = &app.open else {
        ui.label(egui::RichText::new("No slide open.").weak());
        return;
    };
    let slide = open.slide();

    ui.label(egui::RichText::new(display_name(&open.path)).strong());
    ui.label(egui::RichText::new(describe(slide)).weak().small());
    ui.add_space(6.0);

    egui::Grid::new("metadata-grid")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            // `None` is shown as a dash rather than a plausible default, for
            // `Slide::microns_per_pixel`'s reason: a guessed pixel size is
            // wrong in the way that looks right.
            ui.label("Pixel size");
            match slide.microns_per_pixel() {
                Some(mpp) => ui.label(format!("{mpp:.4} µm")),
                None => ui.label(egui::RichText::new("—").weak()),
            };
            ui.end_row();

            ui.label("Magnification");
            match slide.magnification() {
                Some(magnification) => ui.label(format!("{magnification:.0}x")),
                None => ui.label(egui::RichText::new("—").weak()),
            };
            ui.end_row();

            ui.label("Levels");
            ui.label(format!("{}", slide.levels().len()));
            ui.end_row();
        });

    ui.add_space(8.0);
    ui.label(egui::RichText::new("Pyramid").strong());
    egui::Grid::new("level-grid")
        .num_columns(3)
        .spacing([10.0, 2.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Level").weak().small());
            ui.label(egui::RichText::new("Size").weak().small());
            ui.label(egui::RichText::new("Downsample").weak().small());
            ui.end_row();
            let current = open.view.level(slide).index;
            for level in slide.levels() {
                // The level being shown is marked, because the status bar
                // reports a level number and this table is where that number
                // gets its meaning.
                let mark = |text: String| {
                    if level.index == current {
                        egui::RichText::new(text).strong()
                    } else {
                        egui::RichText::new(text)
                    }
                };
                ui.label(mark(level.index.to_string()));
                ui.label(mark(format!("{} x {}", level.width, level.height)));
                ui.label(mark(format!("{:.1}x", level.downsample)));
                ui.end_row();
            }
        });
}

/// Every gesture and key, in one place.
///
/// This is the pane QuPath has no equivalent of, and it exists because the
/// complaint that produced `VIEWER.md` §3.7 was discoverability: the zoom
/// gesture was invisible, which is how a dead scroll wheel survived unnoticed.
/// The status bar fixed one gesture's worth of that; this is the rest.
fn help(ui: &mut egui::Ui) {
    ui.heading("Gestures and keys");
    ui.separator();
    egui::Grid::new("help-grid")
        .num_columns(2)
        .spacing([14.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for (gesture, effect) in [
                ("Left-drag", "Pan. The view stays on the slide."),
                ("Right-drag", "Draw a box; releasing zooms to it."),
                ("Escape", "Abandon a box mid-drag."),
                ("Scroll", "Zoom about the cursor. Two fingers on a trackpad."),
                ("Ctrl-scroll", "Zoom faster; this is what a pinch sends."),
                ("+ / −", "Zoom about the cursor."),
                ("f", "Fit the whole slide."),
                ("d", "Detect cells in what is on screen."),
                ("h", "Show or hide the outlines."),
                ("o", "Open a slide."),
            ] {
                ui.label(egui::RichText::new(gesture).strong());
                // Wrapped, not merely long: a `Grid` sizes itself to its widest
                // unwrapped cell, and that intrinsic width beats the panel's
                // `max_width`. With these left unwrapped the three panels
                // together took a 1280px window down to 471px of slide, and an
                // 800px one to a *negative* width — the whole reason the cap
                // above exists. The cap cannot bind a panel whose contents
                // refuse to be narrower.
                ui.add(egui::Label::new(effect).wrap());
                ui.end_row();
            }
        });
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "Zoom buttons in the toolbar work about the centre of the view; \
             the wheel and the +/− keys work about the cursor.",
        )
        .weak()
        .small(),
    );
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

    /// The same layout with a side panel open, which is what the analysis,
    /// metadata and help panes add.
    fn layout_with_side_panel() -> (f32, f32, f32, f32) {
        let context = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut area = (0.0, 0.0, 0.0, 0.0);
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            };
            let _ = context.run(input, |context| {
                egui::TopBottomPanel::top("toolbar").show(context, |ui| {
                    ui.horizontal(|ui| {
                        let _ = ui.button("Fit");
                    });
                });
                egui::TopBottomPanel::bottom("status").show(context, |ui| {
                    ui.label("status");
                });
                // The ordering `draw` uses: side panels before the rectangle is
                // read. This test exists because putting them after is the
                // mistake that would compile and look right.
                egui::SidePanel::right("analysis")
                    .default_width(260.0)
                    .show(context, |ui| {
                        ui.label("parameters");
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
        area
    }

    /// Runs the *real* [`draw`] headlessly at `size`, with the panels the flags
    /// ask for, and returns the slide rectangle it reports.
    ///
    /// Every other helper in this module rebuilds a replica of the layout, which
    /// cannot catch a panel being too wide for the window because the replica
    /// does not contain the panel's contents. This one calls the function the
    /// application calls, so what it measures is what a user would see.
    ///
    /// Two frames, for `layout`'s reason.
    fn draw_headless(
        size: egui::Vec2,
        analysis: bool,
        metadata: bool,
        help: bool,
    ) -> (f32, f32, f32, f32) {
        // Imported here rather than at module scope: the helpers above are pure
        // `egui` and stay clearer without the crate's names in view.
        use super::{App, draw, style};

        let context = egui::Context::default();
        style(&context);
        let mut app = App::new(None);
        app.show_analysis = analysis;
        app.show_metadata = metadata;
        app.show_help = help;
        let mut area = (0.0, 0.0, 0.0, 0.0);
        for _ in 0..2 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                ..Default::default()
            };
            let _ = context.run(input, |context| {
                let (_, reported) = draw(context, &app);
                area = reported;
            });
        }
        area
    }

    /// The width a rectangle from [`draw_headless`] leaves for the slide.
    fn width(area: (f32, f32, f32, f32)) -> f32 {
        area.2 - area.0
    }

    /// With every panel open, the slide must still keep its share of the window.
    ///
    /// This is the rough edge `docs/VIEWER.md` §6 predicted and could not check:
    /// three side panels are opened independently, none of them knows about the
    /// others, and on a small window they can add up to more than there is.
    /// Whether they *look* right is still not asserted — but "the slide is left
    /// with nothing" is a number, and it is the failure that matters.
    ///
    /// The slack is a pixel, not a fudge factor: the panels take exactly
    /// [`PANEL_SHARE`] and the separator lines between them round, so the
    /// measured 639.875 of a 1280px window is the bound being *met*. Asserting
    /// strictly more than half would fail on the correct layout.
    #[test]
    fn three_panels_leave_the_slide_its_share_of_a_normal_window() {
        let size = egui::vec2(1280.0, 800.0);
        let bare = width(draw_headless(size, false, false, false));
        let all = width(draw_headless(size, true, true, true));
        assert!(
            all >= bare * super::PANEL_SHARE - 1.0,
            "three panels took more than their share of a 1280px window: \
             {all} left of {bare}"
        );
    }

    /// And on a small window they must not take *all* of it.
    ///
    /// The weaker claim, deliberately: at 800x600 three panels genuinely are a
    /// lot of the window, and asserting a comfortable share there would be
    /// asserting a preference. What must hold is that the slide does not vanish,
    /// because a viewer showing no slide is broken rather than cramped.
    #[test]
    fn the_slide_does_not_vanish_on_a_small_window() {
        let all = width(draw_headless(egui::vec2(800.0, 600.0), true, true, true));
        assert!(
            all > 100.0,
            "three panels left the slide {all}px wide on an 800px window"
        );
    }

    /// Each panel must cost something, or one of them is not being laid out.
    ///
    /// The test above passes trivially if a flag is ignored — a panel that never
    /// shows takes no width. This pins that all three actually reserve space,
    /// which is what makes the sum above a real measurement.
    #[test]
    fn every_panel_reserves_width_of_its_own() {
        let size = egui::vec2(1280.0, 800.0);
        let bare = width(draw_headless(size, false, false, false));
        for (name, analysis, metadata, help) in [
            ("analysis", true, false, false),
            ("metadata", false, true, false),
            ("help", false, false, true),
        ] {
            let one = width(draw_headless(size, analysis, metadata, help));
            assert!(
                one < bare,
                "the {name} panel reserved no width: {one} against {bare} with none open"
            );
        }
    }

    /// A side panel must come out of the slide's rectangle.
    ///
    /// If it does not, every gesture treats a click on a slider as a click on
    /// the tissue: dragging a parameter would pan the slide underneath it, and
    /// a scroll to reach a control further down the panel would zoom. That is
    /// the same class of bug `docs/VIEWER.md` §3.7 records, which is why the
    /// panels are laid out before `available_rect` is read and why this asserts
    /// on it rather than trusting the ordering.
    #[test]
    fn a_side_panel_is_not_part_of_the_slide_rectangle() {
        let area = layout_with_side_panel();
        // The panel is on the right, so the rectangle's right edge must have
        // moved in from the window's.
        assert!(
            area.2 < 800.0,
            "the side panel reserved nothing: {area:?} still reaches the window edge"
        );
        // A point inside the panel is not the slide's.
        let in_panel = egui::pos2(780.0, 300.0);
        assert!(
            !contains(area, in_panel),
            "a point in the side panel is inside the slide rectangle {area:?}"
        );
        // And the middle of what is left still is, so the panel has not taken
        // the whole window.
        assert!(
            contains(area, egui::pos2(200.0, 300.0)),
            "the slide rectangle {area:?} excludes the tissue left of the panel"
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

