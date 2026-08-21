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

use std::path::{Path, PathBuf};

use qupath_pipeline::Micron;
use qupath_viewer::{ScreenPoint, SlidePoint};

use crate::state::{App, Appearance, Channel, cursor_slide_position, describe, display_name};
use crate::thumbnails::Pictures;

/// What the interface is asking the application to do.
///
/// `Eq` is deliberately absent: [`Command::SetTuning`] carries floats, and a
/// derived `Eq` on them is the one that does not mean what it says.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Command {
    #[default]
    None,
    Open,
    /// Open this slide, with no dialog.
    ///
    /// Named for what it does rather than for where it came from: the recent
    /// menu, the welcome screen and the project browser all mean the same act,
    /// and a variant called `OpenRecent` sent from a project entry would be
    /// naming the wrong one of them.
    OpenPath(PathBuf),
    Close,
    /// Open a project, through a dialog.
    OpenProject,
    /// Create an empty project in a directory chosen through a dialog.
    ///
    /// Empty, and the slide showing is not put in it. QuPath separates
    /// *Create project* from *Add images* and so does this: one command that
    /// both made a project and decided what went in it would be two decisions
    /// taken by one click.
    NewProject,
    /// Add the slide showing to the project that is open.
    AddToProject,
    /// Put the project list away. The slide showing stays open.
    CloseProject,
    /// Narrow the browser's list to the rows whose name matches this text.
    ///
    /// Carries the whole text rather than a keystroke, for
    /// [`Command::SetTuning`]'s reason: the field is edited, not incremented,
    /// and a command per character would make the list a function of how many
    /// frames the typing was spread across.
    Filter(String),
    /// Show or hide the project browser.
    ToggleProject,
    Fit,
    /// Zoom about the centre of the view by this many steps, in or out.
    Zoom(i32),
    /// One screen pixel per slide pixel.
    Actual,
    /// Detect cells in what the view is showing, replacing any run going.
    Detect,
    /// Stop the run in flight, leaving no successor.
    ///
    /// Distinct from `Detect`, which also ends the run going: that one replaces
    /// it, this one just stops. A whole-slide run started by mistake wants the
    /// core back.
    StopDetecting,
    /// Write what the last run found as GeoJSON, through a save dialog.
    ///
    /// The outlines only. QuPath reads the file and shows the objects; what it
    /// will not show is measurements, because the viewer keeps the geometry and
    /// discards the rest — see [`crate::detect`].
    Export,
    /// Show or hide the outlines of what was detected, without re-running.
    ToggleOverlay,
    /// Show or hide the analysis panel.
    ToggleAnalysis,
    /// Show or hide the slide-metadata panel.
    ToggleMetadata,
    /// Show or hide the gesture and key list.
    ToggleHelp,
    /// Show or hide the overview thumbnail.
    ToggleOverview,
    /// Centre the view on a slide point, from a click on the overview.
    ///
    /// Carries the point rather than the click position because the mapping
    /// belongs to `Overview` and the layout code is where the click's position
    /// within the picture is known — converting here keeps `state` from having
    /// to know where the pane was laid out.
    JumpTo(SlidePoint),
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
    /// Show or hide the brightness/contrast controls.
    ToggleAppearance,
    /// Replace the display settings.
    ///
    /// Carries the whole [`Appearance`] for [`Command::SetTuning`]'s reason,
    /// and it applies more strongly here: the channel buttons and the range
    /// sliders are edits to the same value, so sending them separately would
    /// make a click on a channel and a drag of a slider in the same frame two
    /// commands where only one can be reported.
    SetAppearance(Appearance),
    /// Put the display back to the slide as read.
    ResetAppearance,
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

/// The `egui` user-texture id the overview picture is registered under.
///
/// One constant shared by the pane that names it and the renderer that uploads
/// it, because two literals that must match are one literal that eventually
/// does not.
pub const OVERVIEW_TEXTURE: u64 = 1;

/// The first user texture id the project browser's row pictures take.
///
/// Row `index` is `PROJECT_TEXTURE + index`, so the ids are the project's own
/// order and are **reused** when another project is opened. That bounds the
/// renderer's texture table by the largest project ever opened rather than by
/// every slide ever looked at, and it is why a row may only draw once
/// `Pictures::ready` says this project's picture is the one at its id.
pub const PROJECT_TEXTURE: u64 = 2;

/// The fraction of the window the side panels may take between them.
///
/// The slide keeps the rest. Half is chosen because the slide is what the
/// application is for: a panel is being read while the tissue is being looked
/// at, so the tissue keeps the larger share whenever the two compete.
const PANEL_SHARE: f32 = 0.5;

/// How many side panels are open.
fn open_panels(app: &App) -> u32 {
    u32::from(app.show_analysis)
        + u32::from(app.show_metadata)
        + u32::from(app.show_help)
        // Counted only when there is a project to list. The pane is not drawn
        // otherwise, and a cap divided by a panel that is not there is the
        // arithmetic that makes every other panel narrower than it needs to be.
        + u32::from(app.show_project && app.project.is_some())
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
                                command = Command::OpenPath(path.clone());
                                ui.close();
                            }
                        }
                    });
                });
                ui.separator();
                if ui
                    .button("New project…")
                    .on_hover_text(
                        "Create an empty QuPath project in a directory. Slides \
                         go into it one at a time, as they do in QuPath.",
                    )
                    .clicked()
                {
                    command = Command::NewProject;
                    ui.close();
                }
                if ui
                    .button("Open project…")
                    .on_hover_text(
                        "Open a QuPath project and list its images (p). \
                         Opening one of them is a second click.",
                    )
                    .clicked()
                {
                    command = Command::OpenProject;
                    ui.close();
                }
                let addable = app.addable();
                if ui
                    .add_enabled(addable.is_ok(), egui::Button::new("Add slide to project"))
                    .on_hover_text(
                        "Put the slide showing into the project that is open. \
                         Everything already in the project is written back \
                         exactly as it was read.",
                    )
                    .on_disabled_hover_text(addable.err().unwrap_or_default())
                    .clicked()
                {
                    command = Command::AddToProject;
                    ui.close();
                }
                if ui
                    .add_enabled(app.project.is_some(), egui::Button::new("Close project"))
                    .on_hover_text("Put the list away. The slide showing stays open.")
                    .clicked()
                {
                    command = Command::CloseProject;
                    ui.close();
                }
                ui.separator();
                // Enabled on there being something to write, not on a slide
                // being open: an overlay outlives the run that made it, and a
                // menu item that is live with nothing behind it produces an
                // empty file rather than a refusal.
                let found = app.found().is_some();
                if ui
                    .add_enabled(found, egui::Button::new("Save detections…"))
                    .on_hover_text(
                        "Write the outlines as GeoJSON, which QuPath opens \
                         directly (s).",
                    )
                    .on_disabled_hover_text("Detect cells first (d).")
                    .clicked()
                {
                    command = Command::Export;
                    ui.close();
                }
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
                    .on_hover_text(
                        "Show the slide at full detail: one screen pixel per slide pixel.",
                    )
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
                if ui
                    .checkbox(&mut metadata, "Slide panel")
                    .on_hover_text(
                        "The slide's details, the overview map and the \
                         brightness controls.",
                    )
                    .clicked()
                {
                    command = Command::ToggleMetadata;
                    ui.close();
                }
                let mut project = app.show_project;
                if ui
                    .add_enabled(
                        app.project.is_some(),
                        egui::Checkbox::new(&mut project, "Project"),
                    )
                    .on_hover_text("The images in the project that is open.")
                    .on_disabled_hover_text("Open a project first (p).")
                    .clicked()
                {
                    command = Command::ToggleProject;
                    ui.close();
                }
                let mut help = app.show_help;
                if ui.checkbox(&mut help, "Mouse and keyboard help").clicked() {
                    command = Command::ToggleHelp;
                    ui.close();
                }
                let mut overview = app.show_overview;
                if ui
                    .checkbox(&mut overview, "Overview map")
                    .on_hover_text(
                        "A thumbnail of the whole slide showing where you are. \
                         Click it to jump. Opens in the slide panel.",
                    )
                    .clicked()
                {
                    command = Command::ToggleOverview;
                    ui.close();
                }
                let mut appearance = app.show_appearance;
                if ui
                    .checkbox(&mut appearance, "Brightness and contrast")
                    .on_hover_text(
                        "Brightness, contrast and the haematoxylin/eosin \
                         channels. Opens in the slide panel. Changes how the \
                         slide looks, never what gets counted.",
                    )
                    .clicked()
                {
                    command = Command::ToggleAppearance;
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

                // QuPath draws its scalebar over the bottom-left of the slide
                // itself (`QuPathViewerPlus`, an `AnchorPane` at the corner).
                // Here it goes in the status bar instead, and deliberately: a bar
                // floating over the image would sit inside the slide's rectangle,
                // and §8.1c is what happens when something lands there. The bar
                // is what gives the `px/px` beside it a physical meaning.
                if let Some(bar) = qupath_viewer::scalebar(
                    open.slide().microns_per_pixel(),
                    points_per_slide_pixel(open.view.scale, ui.ctx().pixels_per_point()),
                ) {
                    scalebar(ui, &bar);
                    ui.separator();
                }

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

                // Said, rather than left as a hole in the picture. A tile that
                // cannot be decoded is never asked for again — otherwise every
                // redraw would retry it — so the gap is permanent, and the
                // underlay drawn under it makes it look like a load that has
                // stalled rather than one that has failed. The spinner beside
                // this is gone by then, which is what makes the frame look
                // finished.
                let unreadable = open.failed();
                if unreadable > 0 {
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!("{unreadable} unreadable"))
                            .color(ui.visuals().warn_fg_color),
                    )
                    .on_hover_text(
                        "Tiles this slide's codec could not decode. They are \
                         not retried, so those squares stay as they are — what \
                         is drawn there is the coarser level underneath.",
                    );
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
            .show(context, |ui| {
                scrolled("analysis", ui, |ui| analysis(ui, app, &mut command))
            });
    }
    if app.show_metadata {
        egui::SidePanel::left("metadata")
            .default_width(240.0_f32.min(cap))
            .max_width(cap)
            .show(context, |ui| {
                scrolled("metadata", ui, |ui| metadata(ui, app, &mut command))
            });
    }
    // First on the left, because it is the outermost thing being worked
    // through: a study is opened, then an image in it, then that image's
    // details. The metadata pane beside it is about whichever image the list
    // led to.
    if app.show_project && app.project.is_some() {
        egui::SidePanel::left("project")
            .default_width(250.0_f32.min(cap))
            .max_width(cap)
            .show(context, |ui| {
                scrolled("project", ui, |ui| project_pane(ui, app, &mut command));
            });
    }
    if app.show_help {
        egui::SidePanel::left("help")
            .default_width(280.0_f32.min(cap))
            .max_width(cap)
            .show(context, |ui| scrolled("help", ui, help));
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

/// Runs a panel's contents inside a vertical scroll area.
///
/// # Why every side panel needs one
///
/// The cap above bounds a panel's *width* so the slide keeps its share, and
/// says in as many words that "the cap cannot bind a panel whose contents
/// refuse to be narrower". Nothing bounded its **height** at all, and a panel
/// is as tall as the window between the toolbar and the status bar — so on a
/// short window the bottom of a panel was simply not drawn.
///
/// That is worse than it sounds for the analysis pane, whose `Detect cells` and
/// `Reset` buttons live under six sliders: at 760x520 with three panels open
/// they are past the bottom edge, and there is no gesture that reaches them.
/// A control that is clipped is a control that does not exist, and unlike the
/// clipped *labels* beside it, nothing about the picture says so.
///
/// `auto_shrink` is off on the vertical axis so the area claims the panel's
/// full height rather than collapsing to its contents, which is what makes the
/// scrollbar appear only when there is something below the fold.
fn scrolled(id: &str, ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, contents);
}

/// The analysis panel: QuPath's cell-detection parameters, in QuPath's units.
///
/// # Why microns, and why the pixel value beside them
///
/// QuPath's dialog is in microns and so is this, so a number a user knows
/// transfers. But the pipeline computes in **pixels**, and the conversion needs
/// a pixel size — so the panel shows both: the micron value that is being
/// edited, and what the slide's pixel size makes of it. A parameter whose
/// effect depends on a number the interface does not show is one the user cannot
/// reason about, and the pixel column is what makes "8 µm" mean something
/// specific on this slide rather than in general.
///
/// The column is in **level-0** pixels, which is the frame everything else here
/// is quoted in. A run that reads a coarser level converts again with that
/// level's pixel size — `detect::params_for` — so the micron value is what
/// holds across levels and the pixel value is what it means at full
/// resolution.
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
        ui.label("Threshold");
        changed |= ui
            .add(egui::Slider::new(&mut tuning.intensity_threshold, 0.0..=1.0).fixed_decimals(3))
            .on_hover_text(
                "A candidate's mean haematoxylin optical density must exceed \
                 this to survive. Dimensionless, so it does not scale with the \
                 pixel size.",
            )
            .changed();
        let mut max_background = f64::from(tuning.max_background);
        ui.label("Max background");
        if ui
            .add(egui::Slider::new(&mut max_background, 0.0..=5.0).fixed_decimals(2))
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
                .on_hover_text(
                    "Run with these parameters over what is on screen (d). \
                     Zoomed out, it reads finer pixels than the screen shows so \
                     that nuclei are resolved, and may cover only the centre of \
                     the view.",
                )
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
    // The name goes *above* the slider rather than after it. `Slider::text`
    // lays the label out inline, so the widget's width plus the text is what
    // the panel has to fit — and the panel is capped (see `panel_cap`), so the
    // text is what gets clipped. At a 1100px window with every panel open this
    // showed "Background radius" as "Bac". §8.1c is the same shape: a panel
    // whose contents refuse to be narrower than the cap that binds them.
    //
    // Found by looking, which is the only way it could have been: every
    // layout test here asserts on rectangles, and the rectangles were right.
    ui.label(format!("{label} ({unit})"));
    let changed = ui
        .add(egui::Slider::new(value, range).fixed_decimals(1))
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
        .on_hover_text(
            "What this slide's pixel size makes of the value above, in level-0 \
             pixels. A run that reads a coarser level converts again for that \
             level, so the micron value is the one that holds.",
        );
    }
    changed
}

/// The slide-metadata panel: what the file says about itself.
///
/// Everything here is read from the slide rather than computed, and it exists
/// because `VIEWER.md` §8.0's pixel-size fix is otherwise invisible — the number
/// every micron parameter divides by now has somewhere to be seen.
/// The overview thumbnail: where the view is on the slide, and a click to jump.
///
/// `VIEWER.md` §8.2, and the pane the section put at the top of its list —
/// zoomed in past a few hundred microns nothing else on screen says where on
/// the slide you are, because the status bar's level-0 coordinates are a number
/// rather than a position.
///
/// The picture is painted as a mesh of one textured quad rather than through an
/// `egui` texture handle, because `draw` takes `&App` by design (see the module
/// documentation) and registering a texture needs somewhere to keep the handle.
/// The pixels are already in memory and the quad is what `compose` rasterises
/// anyway, so this costs a copy of a 150x107 image per frame — about 64 KB,
/// against the megabytes of tile the same frame blits.
fn overview_pane(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    let Some(thumbnail) = app.open.as_ref().and_then(|open| open.overview()) else {
        return;
    };
    let view = app.open.as_ref().map(|open| open.view);
    let (width, height) = thumbnail.overview.size();

    // Fitted to the pane rather than drawn at its stored size: the pane is
    // resizable and a picture that overflows it is clipped rather than scaled,
    // which loses the right-hand edge of the slide without saying so.
    let available = ui.available_width().max(1.0);
    let scale = (available / width as f32).min(1.0);
    let size = egui::vec2(width as f32 * scale, height as f32 * scale);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());

    let painter = ui.painter_at(rect);
    let texture = egui::TextureId::User(OVERVIEW_TEXTURE);
    let mut mesh = egui::Mesh::with_texture(texture);
    mesh.add_rect_with_uv(
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    painter.add(egui::Shape::mesh(mesh));

    // The visible rectangle, unless everything is visible — QuPath skips it at
    // zoom-to-fit, "in which case everything is visible".
    if let Some(view) = view
        && !thumbnail.overview.covers_everything(view)
    {
        let visible = thumbnail.overview.visible_rect(view);
        let corner = rect.min + egui::vec2(visible[0] * scale, visible[1] * scale);
        let box_ =
            egui::Rect::from_min_size(corner, egui::vec2(visible[2] * scale, visible[3] * scale));
        // Clipped to the picture, because panning past an edge puts part of the
        // rectangle outside it — `visible_rect` returns that honestly and the
        // drawing is where it gets bounded.
        painter.rect_stroke(
            box_.intersect(rect),
            0.0,
            egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 0, 0)),
            egui::StrokeKind::Inside,
        );
    }
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(64)),
        egui::StrokeKind::Inside,
    );

    // Click *and* drag, as `ImageOverview` does: it binds both to
    // `mouseViewerToLocation`, so holding the button scrubs the view across the
    // slide rather than needing one click per jump.
    if (response.clicked() || response.dragged())
        && let Some(position) = response.interact_pointer_pos()
    {
        let local = position - rect.min;
        *command = Command::JumpTo(
            thumbnail
                .overview
                .to_slide(ScreenPoint::new(local.x / scale, local.y / scale)),
        );
    }
    ui.label(egui::RichText::new("Click to jump").weak().small());
}

/// The brightness/contrast controls: `docs/VIEWER.md` §8.7, which was §8.8's
/// first missing item until it was done.
///
/// QuPath puts these in a floating dialog. They go in this pane instead, for
/// §8.1c's reason and the overview's: a window over the image sits inside the
/// slide's rectangle, and what lands there is a hit-test that stops agreeing
/// with what is drawn.
///
/// The controls edit an [`Appearance`] and hand it back through
/// [`Command::SetAppearance`], so this function keeps taking `&App` — see
/// [`Command::SetTuning`]. Nothing here can reach the detector, which is the
/// half of §8.5's display-only claim that the interface is responsible for:
/// the crate boundary stops a `Display` reaching `qupath-pipeline`, and this
/// stops a control writing anywhere but [`App::appearance`].
fn appearance_pane(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    ui.label(egui::RichText::new("Brightness and contrast").strong());
    ui.label(
        egui::RichText::new("Changes how the slide looks, not what gets counted.")
            .weak()
            .small(),
    );
    let mut appearance = app.appearance;
    let mut changed = false;

    ui.horizontal(|ui| {
        for channel in Channel::ALL {
            if ui
                .selectable_label(appearance.channel == channel, channel.label())
                .clicked()
                && appearance.channel != channel
            {
                appearance.channel = channel;
                changed = true;
            }
        }
    });

    if appearance.channel == Channel::Original {
        // The two ends are kept apart rather than clamped together, so the
        // sliders cannot produce the empty or inverted range `Display::range`
        // refuses. Dragging `min` past `max` pushes `max` along, which is what
        // QuPath's dialog does and what a user dragging hard expects.
        let mut min = appearance.min;
        let mut max = appearance.max;
        ui.label("Darkest");
        if ui
            .add(egui::Slider::new(&mut min, 0.0..=254.0))
            .on_hover_text(
                "Pixels this dark and darker show as black. Raise it to deepen \
                 the shadows. (QuPath calls this minDisplay.)",
            )
            .changed()
        {
            appearance.min = min;
            appearance.max = appearance.max.max(min + 1.0);
            changed = true;
        }
        ui.label("Brightest");
        if ui
            .add(egui::Slider::new(&mut max, 1.0..=255.0))
            .on_hover_text(
                "Pixels this bright and brighter show as white. Lower it to \
                 brighten the image. (QuPath calls this maxDisplay.)",
            )
            .changed()
        {
            appearance.max = max;
            appearance.min = appearance.min.min(max - 1.0);
            changed = true;
        }
    } else {
        // An optical density, not a byte: a stain amount runs 0 to about 2, so
        // the byte range would put every real value in the bottom one percent
        // of the scale. `Stain::full` records the same thing.
        ui.label("Strength");
        if ui
            .add(egui::Slider::new(&mut appearance.full, 0.1..=3.0).fixed_decimals(2))
            .on_hover_text(
                "How much stain counts as fully dark. Lower it for more \
                 contrast, raise it for a paler picture. This is an optical \
                 density rather than a brightness, so it has its own scale.",
            )
            .changed()
        {
            changed = true;
        }
    }

    // Set apart from the sliders. Butted straight up against them it reads as
    // another row of the same stack rather than as the thing that undoes them,
    // which is what looking at `chrome-slide.png` showed.
    ui.add_space(6.0);
    if ui
        .button("Reset")
        .on_hover_text("Back to the slide's original colours.")
        .clicked()
    {
        *command = Command::ResetAppearance;
    } else if changed && *command == Command::None {
        // After the button, for `tuning`'s reason: one command per frame, and
        // a dropped slider frame is invisible where a dropped press is not.
        *command = Command::SetAppearance(appearance);
    }

    ui.add_space(4.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(
                "Cell detection always reads the original pixels, so nothing \
                 here can change a count.",
            )
            .weak()
            .small(),
        )
        .wrap(),
    );
}

/// The first letter upper-cased, for a sentence assembled from fragments.
fn capitalise(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn metadata(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    ui.heading("Slide");
    ui.separator();
    let Some(open) = &app.open else {
        ui.label(egui::RichText::new("No slide open. Use the File menu, or press o.").weak());
        // Say what the panel *will* hold rather than only that it is empty.
        // Turning "Brightness and contrast" on with no slide open otherwise
        // opens a panel that shows nothing and does not say why, which reads
        // as the toggle having failed — the same class of confusion as a
        // control whose panel is closed. Found by looking, per §6.
        if app.show_overview || app.show_appearance {
            ui.add_space(6.0);
            let mut waiting = Vec::new();
            if app.show_overview {
                waiting.push("the overview map");
            }
            if app.show_appearance {
                waiting.push("the brightness and contrast controls");
            }
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{} will appear here once a slide is open.",
                        capitalise(&waiting.join(" and ")),
                    ))
                    .weak()
                    .small(),
                )
                .wrap(),
            );
        }
        return;
    };
    let slide = open.slide();

    // The overview goes at the top of this pane rather than floating over the
    // image where QuPath anchors it (`QuPathViewerPlus`, an `AnchorPane`). A
    // picture over the slide would sit inside the slide's rectangle, and
    // §8.1c is the record of what lands there — the same decision the scalebar
    // took in §8.1d, and taken the same way.
    if app.show_overview {
        overview_pane(ui, app, command);
        ui.add_space(8.0);
    }

    // Below the overview and above the metadata, because it is a control where
    // both of those are readouts, and QuPath's dialog opens over the image for
    // the same reason — it is the thing a hand is on.
    if app.show_appearance {
        appearance_pane(ui, app, command);
        ui.add_space(8.0);
        ui.separator();
    }

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

/// The project browser: the images in the project that is open.
///
/// # A row is a picture and a name
///
/// QuPath's `ProjectBrowser` puts a thumbnail beside each name, and so does
/// this. The picture is **made from the slide** rather than read from the
/// entry's `thumbnail.jpg`, which is the same thing QuPath's browser does when
/// an entry has no stored thumbnail — see [`crate::thumbnails`]. So the row
/// looks the same whether the project was written here or there, which is what
/// the file-based version could not have managed.
///
/// # Every row is shown, including the ones that will not open
///
/// An entry whose slide has moved, or whose image is built by one of the
/// sixteen transforms QuPath supports and this does not, is listed with the
/// reason under it rather than left out. A browser that dropped those rows
/// would show a project with fewer images than it has, and nothing about the
/// picture would say so — the same argument `project::read` makes for keeping
/// them, arriving here as the reason it matters.
fn project_pane(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    ui.heading("Project");
    ui.separator();
    let Some(project) = &app.project else {
        return;
    };

    ui.add(egui::Label::new(egui::RichText::new(&project.name).strong()).wrap());

    let images = project.stored.images.len();
    let shown = project
        .stored
        .images
        .iter()
        .filter(|image| matches(&image.name, &app.filter))
        .count();
    ui.label(
        egui::RichText::new(match (images, shown, images == shown) {
            (0, ..) => "No images".to_owned(),
            (_, _, false) => format!("{shown} of {images} images shown"),
            (1, ..) => "1 image, click it to open".to_owned(),
            (many, ..) => format!("{many} images, click one to open"),
        })
        .weak()
        .small(),
    );

    // A moved project is the explanation for a list of rows that will not
    // open, and it is worth saying once at the top rather than fifty times
    // below. QuPath makes the same comparison for the same reason.
    if project.moved() {
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "This project has been moved. Its images are stored as \
                     absolute paths, so any that were not moved with it will \
                     not open.",
                )
                .color(ui.visuals().warn_fg_color)
                .small(),
            )
            .wrap(),
        );
    }

    ui.add_space(8.0);
    add_slide(ui, app, command);
    search(ui, app, command);

    if project.stored.images.is_empty() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "The project file has no images in it. Open a slide and \
                     add it, or add images to the project in QuPath.",
                )
                .weak()
                .small(),
            )
            .wrap(),
        );
        return;
    }

    if shown == 0 {
        ui.add(
            egui::Label::new(
                egui::RichText::new("No image in this project matches the search.")
                    .weak()
                    .small(),
            )
            .wrap(),
        );
        return;
    }

    let showing = app.open.as_ref().map(|open| open.path.as_path());
    for (index, image) in project.stored.images.iter().enumerate() {
        // The row is skipped and the *index* is not: a picture belongs to the
        // entry it was read for, and renumbering the rows a search leaves
        // showing would hand row 3's picture to row 1.
        if !matches(&image.name, &app.filter) {
            continue;
        }
        let current = is_showing(image, showing);
        let present = project.present(index);
        let openable = image.slide.is_ok();
        // The whole row is the click target — the picture as much as the name.
        // A 50 px picture beside a name is most of the row's area, and a row
        // that only reacts to its text is one where the obvious place to press
        // does nothing.
        let sense = if openable {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        };
        let laid = ui.scope_builder(egui::UiBuilder::new().sense(sense), |ui| {
            // Reserved before the contents so the fill lands *under* them.
            // `egui` paints in the order shapes are added, and a background
            // added afterwards is a rectangle over the picture.
            let background = ui.painter().add(egui::Shape::Noop);
            ui.horizontal(|ui| {
                row_picture(ui, &project.pictures, index, present && openable);
                let text = row_text(ui, image, current, present);
                // `wrap()` and not the default, which inside a horizontal
                // layout is `Extend`: a label that refuses to wrap is a label
                // whose desired width is its whole text, and a side panel sized
                // from its contents then grows to the cap that binds it and
                // clips there. §8.1c's shape exactly, met a second time.
                ui.add(egui::Label::new(text).wrap().selectable(false));
            });
            // Nothing, of the width that is left and no height: it makes the
            // row as wide as the pane without making it taller. The row is a
            // list row, so the part of it to the right of a short name is part
            // of the row — it takes the fill, and it takes the click. Without
            // this the two disagree, and the half that reads as broken is the
            // one where an obviously highlighted row ignores a press.
            ui.allocate_space(egui::vec2(ui.available_width(), 0.0));
            background
        });

        // The slide on screen keeps a filled row for as long as it is on
        // screen; hovering fills one while the pointer is over it. Both say the
        // same thing in the same way, which is what makes the first read as a
        // state rather than as a stuck highlight.
        let fill = if current {
            Some(ui.visuals().selection.bg_fill.gamma_multiply(0.35))
        } else if laid.response.hovered() {
            Some(ui.visuals().widgets.hovered.weak_bg_fill)
        } else {
            None
        };
        if let Some(fill) = fill {
            ui.painter().set(
                laid.inner,
                // Taller than the row and no wider: the row already runs the
                // width of the pane, and expanding that puts two pixels of fill
                // outside the panel's own margin.
                egui::Shape::rect_filled(
                    laid.response.rect.expand2(egui::vec2(0.0, 2.0)),
                    3.0,
                    fill,
                ),
            );
        }

        // An entry that names no file has nothing to hover and nothing to open,
        // and is greyed rather than absent for the reason `project::read` keeps
        // it at all: a browser that dropped those rows would show a project with
        // fewer images than it has.
        if let Ok(path) = &image.slide {
            let mut hover = path.display().to_string();
            if !present {
                hover.push_str(
                    "\n\nThis file was not there when the project was opened. \
                     Clicking still tries it.",
                );
            } else if let Some(reason) = project.pictures.unread(index) {
                // Only when the file *is* there. A slide that is missing has
                // already been told about above, and "the picture could not be
                // read" as a second sentence about it is the same fact twice.
                hover.push_str("\n\nNo picture: ");
                hover.push_str(reason);
            }
            let row = laid.response.on_hover_text(hover);
            if row.clicked() {
                // The same command the recent menu and the welcome screen send.
                // A row that will not open reports through `App::open`'s own
                // message rather than being refused here: the check above is
                // from when the project was opened, and a slide plugged in
                // since is one this should not be second-guessing.
                *command = Command::OpenPath(path.clone());
            }
        }
        ui.add_space(2.0);
    }
}

/// The search box that narrows the list.
///
/// QuPath's `tfFilter`, at the top of its browser above the tree. Here it is
/// under the add button, because the button is about the slide showing and the
/// search is about the list — putting the search directly over the rows it
/// changes keeps a control beside the thing it acts on.
///
/// Emits the whole text through [`Command::Filter`] rather than writing
/// [`App::filter`], for `appearance_pane`'s reason: this function takes `&App`
/// and every change to the application is a command.
fn search(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    let mut text = app.filter.clone();
    let field = egui::TextEdit::singleline(&mut text)
        .hint_text("Search names")
        .desired_width(f32::INFINITY);
    let response = ui.add(field).on_hover_text(
        "Shows the images whose name contains this. Separate several with `|` \
         and every one of them has to match.",
    );
    ui.add_space(6.0);
    if response.changed() && *command == Command::None {
        *command = Command::Filter(text);
    }
}

/// Whether `name` passes the browser's search box.
///
/// `ProjectEntryPredicate.createIgnoreCase`, which is what QuPath's browser
/// builds unless the regular-expression toggle is on:
///
/// - the text is trimmed, and an empty one — or one that is nothing but `|`
///   separators — accepts everything;
/// - the comparison is case-insensitive, both sides lowercased;
/// - the whole text is looked for in the name first;
/// - and only if the text contains `|` is it split into tokens, of which
///   **every one** must be found.
///
/// **`|` reads as "or" and means "and"**, which is worth stating because it is
/// the one thing about this control a user can get wrong: `tif|block` shows the
/// images whose name has both, not either. QuPath's own documentation for the
/// field is the class comment quoted above, and it says the same thing in the
/// same surprising way.
///
/// What is **not** ported is the other half of a token: QuPath also tests each
/// one against the entry's metadata as `key=value`, plus a synthesised
/// `has_data=<bool>`. `project::read` summarises an entry to an id, a name, a
/// builder and a path — the metadata map stays in the document, unread — so a
/// token like `has_data=true` matches nothing here where QuPath would match a
/// row. Narrowing that would mean reading the map, which is a different change
/// from a search box.
fn matches(name: &str, filter: &str) -> bool {
    let filter = filter.trim();
    if filter.replace('|', "").is_empty() {
        return true;
    }
    let filter = filter.to_lowercase();
    let name = name.to_lowercase();
    if name.contains(&filter) {
        return true;
    }
    if !filter.contains('|') {
        return false;
    }
    // Not trimmed, which is QuPath's behaviour rather than an oversight: it
    // drops a *blank* token and matches the rest as they are, so `a | b` looks
    // for `a ` and ` b` with their spaces.
    filter
        .split('|')
        .filter(|token| !token.trim().is_empty())
        .all(|token| name.contains(token))
}

/// Whether this row's image is the slide on screen.
///
/// A function rather than the line it was, because the line it was is wrong in
/// a way that reads correctly: `image.slide.as_deref().ok() == showing` puts
/// `None` on the left for an entry that names no file — one of the sixteen
/// builder types — and `None` on the right for an application with no slide
/// open, so **every unopenable row is marked as the one being looked at the
/// moment nothing is**. Found by looking at the pane, which is where a row
/// wearing the current-image fill with no image on screen is obvious and where
/// nothing else was ever going to see it.
fn is_showing(image: &qupath_viewer::project::Image, showing: Option<&Path>) -> bool {
    image
        .slide
        .as_deref()
        .is_ok_and(|path| showing == Some(path))
}

/// A row's name, and the reason under it when there is one.
///
/// **One widget and not two**, which is a layout decision rather than a tidying
/// one: `egui` places a child of unknown height at the top of the line it is on,
/// so a name in a nested `vertical` beside a 40-point picture sits against the
/// picture's top edge rather than beside its middle. A `LayoutJob` is a single
/// piece of text whose size is known when it is placed, so the row's own
/// `Align::Center` puts it where a row of a list belongs.
///
/// It also puts the two lines in one wrap: the reason wraps against the same
/// width the name does, and neither can be laid out against a width the other
/// has already spent. The width itself is the `Label`'s to decide — a job that
/// set its own would be one the widget's wrap mode overrides.
fn row_text(
    ui: &egui::Ui,
    image: &qupath_viewer::project::Image,
    current: bool,
    present: bool,
) -> egui::text::LayoutJob {
    let visuals = ui.visuals();
    // The row being looked at is marked, because a list of fifty names says
    // nothing about which of them is on screen — and the title bar naming the
    // file is not an answer when two entries name the same slide.
    let colour = match (&image.slide, current, present) {
        (Ok(_), true, _) => visuals.strong_text_color(),
        (Ok(_), false, true) => visuals.text_color(),
        (Ok(_), false, false) => visuals.warn_fg_color,
        (Err(_), ..) => visuals.weak_text_color(),
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &image.name,
        0.0,
        egui::TextFormat {
            font_id: egui::TextStyle::Body.resolve(ui.style()),
            color: colour,
            ..Default::default()
        },
    );
    if let Err(reason) = &image.slide {
        // The reason is shown because "cannot open" and "cannot open *because
        // the image is a rotated view of another one*" are different amounts of
        // help.
        job.append(
            &format!("\n{reason}"),
            0.0,
            egui::TextFormat {
                font_id: egui::TextStyle::Small.resolve(ui.style()),
                color: visuals.weak_text_color(),
                ..Default::default()
            },
        );
    }
    job
}

/// How large a row's picture is, in points: QuPath's `ProjectThumbnailSize`.
///
/// `SMALL`, which is its default — 50 by 40, and the two are separate constants
/// there rather than one aspect. A fixed box and not a fitted one, because rows
/// of different-shaped slides have to line their names up: the picture is
/// centred in the box and keeps its own aspect, so a long thin slide is a short
/// wide picture in the same rectangle rather than a row of a different height.
const ROW_PICTURE: egui::Vec2 = egui::vec2(50.0, 40.0);

/// One row's picture, or the space where it will be.
///
/// The space is allocated whether or not there is anything to put in it, which
/// is what stops the list jumping as pictures land: a browser that grew each
/// row when its thumbnail arrived would move the row under the pointer between
/// the press and the release.
///
/// `lit` is false for a slide that was not on disk when the project was opened.
/// QuPath draws that thumbnail at `opacity 0.2` and puts a warning icon over
/// it; this fades the picture the same way and leaves the warning to the name
/// beside it, which is already in the warning colour and already says so on
/// hover.
fn row_picture(ui: &mut egui::Ui, pictures: &Pictures, index: usize, lit: bool) {
    let (box_, _) = ui.allocate_exact_size(ROW_PICTURE, egui::Sense::hover());
    let Some(picture) = pictures.ready(index) else {
        // An empty frame, drawn rather than left blank: the box is allocated
        // either way, so a blank one keeps the names lined up and looks like a
        // ragged indent doing it. The outline is what says the space is a
        // picture's.
        ui.painter()
            .rect_filled(box_, 0.0, ui.visuals().faint_bg_color);
        ui.painter().rect_stroke(
            box_,
            0.0,
            egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Inside,
        );
        // Crossed out once it is settled that no picture is coming. Without
        // this a slide that opens and decodes nothing — every JPEG 2000
        // pyramid, which is half of `data/` — holds the same empty frame as a
        // slide still being read, for as long as the project stays open. The
        // cross is two lines rather than a glyph because the last thing this
        // interface reached into the font for came out as `□`.
        if pictures.unread(index).is_some() {
            let cross = box_.shrink(box_.height() * 0.3);
            let stroke = egui::Stroke::new(1.0, ui.visuals().weak_text_color());
            ui.painter()
                .line_segment([cross.left_top(), cross.right_bottom()], stroke);
            ui.painter()
                .line_segment([cross.left_bottom(), cross.right_top()], stroke);
        }
        return;
    };

    let (width, height) = picture.overview.size();
    #[expect(clippy::cast_precision_loss, reason = "a thumbnail, at most 150 px")]
    let (width, height) = (width as f32, height as f32);
    let scale = (box_.width() / width).min(box_.height() / height);
    let size = egui::vec2(width * scale, height * scale);
    let rect = egui::Rect::from_center_size(box_.center(), size);

    let mut mesh = egui::Mesh::with_texture(egui::TextureId::User(PROJECT_TEXTURE + index as u64));
    mesh.add_rect_with_uv(
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        // Premultiplied white, so the vertex colour multiplies the texel and
        // leaves it alone — and at less than full alpha fades it instead.
        if lit {
            egui::Color32::WHITE
        } else {
            egui::Color32::from_white_alpha(64)
        },
    );
    ui.painter().add(egui::Shape::mesh(mesh));
    ui.painter().rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
}

/// The button that puts the slide showing into the project.
///
/// In the pane and not only in the `File` menu, because this is the one command
/// whose subject is *both* of the things the pane and the viewer are showing:
/// putting it where the list is means the list is what the user is looking at
/// when they decide.
///
/// Disabled rather than hidden when there is nothing to add, with the reason as
/// its hover text — a control that disappears takes its explanation with it, and
/// "why can I not add this slide" is a question that needs an answer where the
/// question is asked. The reason comes from [`App::addable`], which is the same
/// predicate the command itself checks, so the button cannot offer something
/// the action then refuses.
fn add_slide(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    let addable = app.addable();
    let button = egui::Button::new("Add the slide showing");
    let response = ui
        .add_enabled(addable.is_ok(), button)
        .on_hover_text(
            "Write the slide on screen into this project. Everything already \
             in the project is written back exactly as it was read.",
        )
        .on_disabled_hover_text(addable.err().unwrap_or_default());
    if response.clicked() {
        *command = Command::AddToProject;
    }
    ui.add_space(6.0);
}

/// The viewport's scale in the unit `egui` draws in.
///
/// `Viewport::scale` is **physical** screen pixels per level-0 pixel — the
/// framebuffer's own unit, and what the tiles are drawn in. `egui` lays out and
/// paints in **logical points**. A length taken from the viewport and handed to
/// a painter is therefore longer than it claims by exactly the scale factor.
///
/// In the scalebar that is not a cosmetic error but a wrong measurement: at a
/// scale factor of 2.0 the bar labelled "50 µm" was drawn twice as long as
/// 50 µm, so anything sized against it read half its real size — and the label
/// being a round number is what makes a scalebar look trustworthy while it is
/// wrong. Same family as `state::draw_interface` and `state::scale_area`, and
/// the same fix: convert at the boundary, once.
///
/// Asking for the scale in points also puts QuPath's preferred bar length of
/// 100 back in the unit QuPath means it in — JavaFX lays out in logical units,
/// so a bar that prefers 100 *physical* pixels would snap to a different round
/// number on a scaled display than QuPath picks for the same view.
fn points_per_slide_pixel(scale: f64, pixels_per_point: f32) -> f64 {
    scale / f64::from(pixels_per_point)
}

/// Draws a scalebar: a bar with end ticks, and its length written above it.
///
/// The length comes from [`qupath_viewer::scalebar`], which is QuPath's rule; all
/// this does is put it on screen. The bar is clamped to something that fits in a
/// status bar, because past the end of QuPath's table it can grow without bound
/// and the layout is not the place to discover that.
fn scalebar(ui: &mut egui::Ui, bar: &qupath_viewer::Scalebar) {
    // Cast once, and clamp: `pixels` is what the rule asked for, and a status bar
    // has an opinion about how much of itself it will give up. The clamp is also
    // what makes the truncation harmless — the range it admits is a few hundred,
    // where `f32` is exact in the integers.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "screen space is f32, and the clamp bounds the value to a few hundred"
    )]
    let width = (bar.pixels as f32).clamp(8.0, 220.0);
    let height = ui.text_style_height(&egui::TextStyle::Small);
    let (response, painter) =
        ui.allocate_painter(egui::vec2(width, height + 6.0), egui::Sense::hover());
    let rect = response.rect;
    let colour = ui.visuals().text_color();
    let stroke = egui::Stroke::new(1.0, colour);

    // The bar sits at the bottom of what was allocated with the label above it,
    // which is the arrangement QuPath uses and reads as one object rather than a
    // line and some text that happen to be adjacent.
    let y = rect.bottom() - 1.0;
    painter.line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        stroke,
    );
    // End ticks, so the bar has ends rather than fading into the separator next
    // to it.
    for x in [rect.left(), rect.right()] {
        painter.line_segment([egui::pos2(x, y - 4.0), egui::pos2(x, y)], stroke);
    }
    painter.text(
        egui::pos2(rect.center().x, y - 5.0),
        egui::Align2::CENTER_BOTTOM,
        &bar.label,
        egui::FontId::proportional(height),
        colour,
    );
    response.on_hover_text(
        "Scalebar. The length is snapped to a round number, so the bar is \
         whatever that length happens to measure on screen.",
    );
}

/// Every gesture and key, in one place.
///
/// This is the pane QuPath has no equivalent of, and it exists because the
/// complaint that produced `VIEWER.md` §3.7 was discoverability: the zoom
/// gesture was invisible, which is how a dead scroll wheel survived unnoticed.
/// The status bar fixed one gesture's worth of that; this is the rest.
fn help(ui: &mut egui::Ui) {
    ui.heading("Mouse and keyboard");
    ui.separator();
    egui::Grid::new("help-grid")
        .num_columns(2)
        .spacing([14.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            for (gesture, effect) in [
                ("Left-drag", "Pan. The view stays on the slide."),
                ("Right-drag", "Draw a box; releasing zooms to it."),
                ("Escape", "Abandon a box mid-drag, or stop a detection run."),
                (
                    "Scroll",
                    "Zoom about the cursor. Two fingers on a trackpad.",
                ),
                ("Ctrl-scroll", "Zoom faster; this is what a pinch sends."),
                ("+ / −", "Zoom about the cursor."),
                ("f", "Fit the whole slide."),
                ("d", "Detect cells in what is on screen."),
                ("h", "Show or hide the outlines."),
                ("s", "Save what was found as GeoJSON."),
                ("o", "Open a slide."),
                ("p", "Open a QuPath project and list its images."),
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
            "The scalebar in the status bar is snapped to a round number, so it \
             changes length as you zoom rather than changing what it says.",
        )
        .weak()
        .small(),
    );
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "Zoom buttons in the toolbar work about the middle of the slide \
             area; the wheel and the +/− keys work about the cursor.",
        )
        .weak()
        .small(),
    );
}

/// The detection controls: run it, and show or hide what it found.
///
/// The button is disabled while a run is going *and* says so in its label,
/// because a control that is merely greyed out is indistinguishable from one
/// that is broken.
///
/// While a run is going this shows a real fraction and a way to stop. Both were
/// absent for as long as `run_collecting` was one opaque call — the elapsed
/// time stood in for the fraction, because a bar advancing on a timer would
/// have been an invention rather than a measurement. `run_collecting_watched`
/// changed what is knowable: the denominator is the run's own tile count, fixed
/// before the first tile starts, so the bar measures something. It is still
/// tiles and not time, which is why no finish estimate is offered beside it —
/// tiles over glass are far cheaper than tiles over tissue.
fn detection(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    let running = app.detector.is_running();
    ui.add_enabled_ui(app.open.is_some() && !running, |ui| {
        if ui
            .button("Detect cells")
            .on_hover_text(
                "Run cell detection over what is on screen (d). \
                 It runs on another thread, so the view keeps moving. \
                 Zoomed out, it reads finer pixels than the screen shows so that \
                 nuclei are resolved, and may cover only the centre of the view.",
            )
            .clicked()
        {
            *command = Command::Detect;
        }
    });

    if running {
        let elapsed = app
            .detector
            .elapsed()
            .map(|elapsed| format!("{:.0} s", elapsed.as_secs_f64()))
            .unwrap_or_default();
        match app.detector.fraction() {
            // A real fraction, so a real bar. The percentage is written into it
            // rather than left to the fill, because a bar at 3% and a bar that
            // is stuck look identical.
            Some(fraction) => {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "a fraction in 0.0..=1.0; f32 carries it to seven digits"
                )]
                let fraction = fraction as f32;
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(90.0)
                        .text(egui::RichText::new(format!("{:.0}%", fraction * 100.0)).small()),
                );
                ui.label(egui::RichText::new(elapsed).weak());
            }
            // Between the worker starting and the tile grid being built there
            // is no denominator yet. A spinner says "going" without claiming a
            // position, which is the honest thing for that gap.
            None => {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(egui::RichText::new(format!("detecting… {elapsed}")).weak());
            }
        }
        if ui
            .button("Stop")
            .on_hover_text(
                "Stop the run. It ends within a second or two \u{2014} one tile\u{2019}s \
                 work \u{2014} and the outlines from the last finished run are kept.",
            )
            .clicked()
        {
            *command = Command::StopDetecting;
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
        ui.add_space(4.0);
        if ui
            .button("Open a project…")
            .on_hover_text("A QuPath project directory's .qpproj file.")
            .clicked()
        {
            *command = Command::OpenProject;
        }
        if !app.recent.is_empty() {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("Recent").weak());
            for path in &app.recent {
                if ui.link(display_name(path)).clicked() {
                    *command = Command::OpenPath(path.clone());
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

    /// What a label like `50 µm` or `2 mm` claims, in microns.
    fn claimed_microns(label: &str) -> f64 {
        let (value, unit) = label.split_once(' ').expect("a value and a unit");
        let value: f64 = value.parse().expect("a number");
        if unit == "mm" { value * 1000.0 } else { value }
    }

    /// The search box narrows by name, the way QuPath's does.
    ///
    /// The case worth having a test for is the last group: `|` looks like "or"
    /// and QuPath means "and". A port that read it the obvious way would show
    /// *more* rows than QuPath for the same text, and every one of them would
    /// look like a match.
    #[test]
    fn the_search_box_follows_qupaths_predicate() {
        // Nothing typed, and nothing but separators, accept everything.
        assert!(super::matches("CMU-1", ""));
        assert!(super::matches("CMU-1", "   "));
        assert!(super::matches("CMU-1", "|"));
        assert!(super::matches("CMU-1", " || "));

        // A substring, either case, anywhere in the name.
        assert!(super::matches("CMU-1, JP2K", "jp2k"));
        assert!(super::matches("CMU-1, JP2K", "CMU"));
        assert!(super::matches("CMU-1, JP2K", "1, jp"));
        assert!(!super::matches("CMU-1, JP2K", "block"));

        // Every token has to be found, not any of them.
        assert!(super::matches("CMU-1, JP2K", "cmu|jp2k"));
        assert!(!super::matches("CMU-1, JP2K", "cmu|block"));
        assert!(!super::matches("CMU-1, JP2K", "block|section"));

        // And the whole text is tried against the name before it is split, so
        // a name that really does contain a `|` matches it literally.
        assert!(super::matches("scan | second pass", "n | s"));
    }

    /// A row is marked only when its own slide is the one on screen.
    ///
    /// The case that fails without the `is_ok_and` is the last pair: an entry
    /// that names no file, in an application with no slide open. Both sides are
    /// then `None`, the row is marked as the current image, and the browser
    /// shows a study whose "open" slide is the one row that cannot be opened.
    #[test]
    fn a_row_is_marked_only_for_the_slide_on_screen() {
        use qupath_viewer::project::Image;

        let entry = |slide: Result<&str, &str>| Image {
            id: 1,
            name: String::from("an image"),
            builder: String::from("uri"),
            slide: slide
                .map(std::path::PathBuf::from)
                .map_err(std::string::ToString::to_string),
            uri: None,
        };
        let here = std::path::Path::new("/slides/one.svs");
        let elsewhere = std::path::Path::new("/slides/two.svs");

        assert!(super::is_showing(&entry(Ok("/slides/one.svs")), Some(here)));
        assert!(!super::is_showing(
            &entry(Ok("/slides/one.svs")),
            Some(elsewhere)
        ));
        assert!(!super::is_showing(&entry(Ok("/slides/one.svs")), None));
        assert!(!super::is_showing(
            &entry(Err("a `rotated` image")),
            Some(here)
        ));
        assert!(!super::is_showing(&entry(Err("a `rotated` image")), None));
    }

    /// The scalebar measures what its label says, on a scaled display too.
    ///
    /// The bar's length arrives from the viewport in *physical* pixels and is
    /// drawn by `egui` in *points*. Unconverted, the bar labelled "50 µm" was
    /// drawn `pixels_per_point` times too long — so on a 2.0 display everything
    /// measured against it read half its real size, and nothing about the
    /// picture said so. A wrong scalebar is the one kind of wrong here that a
    /// user would go on to quote in a figure caption.
    ///
    /// Asserted as a round trip rather than against a remembered length: what
    /// matters is that the drawn bar and the printed label agree, whatever the
    /// snapping picked.
    #[test]
    fn the_scalebar_measures_what_it_says_on_a_scaled_display() {
        // CMU-1's pixel size, across the zooms a viewer actually sits at.
        const MICRONS_PER_PIXEL: f64 = 0.499;

        for scale in [0.02_f64, 0.05, 0.25, 1.0] {
            for pixels_per_point in [1.0_f32, 1.5, 2.0] {
                let bar = qupath_viewer::scalebar(
                    Some(MICRONS_PER_PIXEL),
                    super::points_per_slide_pixel(scale, pixels_per_point),
                )
                .expect("a slide with a pixel size");

                // The bar is drawn `pixels` points long, which is that many
                // physical pixels times the scale factor, which is that many
                // level-0 pixels over the viewport's scale.
                let measured = bar.pixels * f64::from(pixels_per_point) / scale * MICRONS_PER_PIXEL;
                let claimed = claimed_microns(&bar.label);
                assert!(
                    (measured - claimed).abs() < claimed * 1e-9,
                    "at {pixels_per_point}x and scale {scale}, a bar labelled {} \
                     measures {measured} µm",
                    bar.label,
                );
            }
        }

        // The value that would have made this fail is reachable: handing the
        // viewport's physical scale straight to a painter that draws in points
        // overstates the bar by exactly the scale factor.
        let unconverted = qupath_viewer::scalebar(Some(MICRONS_PER_PIXEL), 0.25)
            .expect("a slide with a pixel size");
        let measured = unconverted.pixels * 2.0 / 0.25 * MICRONS_PER_PIXEL;
        let claimed = claimed_microns(&unconverted.label);
        assert!(
            (measured - claimed * 2.0).abs() < claimed * 1e-9,
            "unconverted, a bar labelled {} measures {measured} µm -- \
             this is what regressed",
            unconverted.label,
        );
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
    /// The slack is a pixel per panel, not a fudge factor: the panels take
    /// exactly [`PANEL_SHARE`] and every container between the cap and the
    /// slide's rectangle rounds once — the separator lines, and the scroll area
    /// each panel's contents sit in. The measured 638.9 of a 1280px window is
    /// the bound being *met* at 49.9%; asserting strictly more than half would
    /// fail on the correct layout.
    #[test]
    fn three_panels_leave_the_slide_its_share_of_a_normal_window() {
        /// One pixel of rounding per panel, per the note above.
        const SLACK: f32 = 3.0;

        let size = egui::vec2(1280.0, 800.0);
        let bare = width(draw_headless(size, false, false, false));
        let all = width(draw_headless(size, true, true, true));
        assert!(
            all >= bare * super::PANEL_SHARE - SLACK,
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
