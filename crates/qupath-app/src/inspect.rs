//! Selecting a detection, reading its values, and colouring every outline by
//! a measurement.
//!
//! `docs/dev/VIEWER.md` §3.5b. Everything this adds to the application lives
//! here: the selection and the colour choice as state, the per-frame drawing
//! of both, and the **Objects** pane. `state` and `chrome` call in at the few
//! places they have to — a click, a frame, a menu entry — so the rest of the
//! application is not rearranged around it.
//!
//! # Row indices that outlive their overlay
//!
//! A selection is a row number, and a colour table is one colour per row. Both
//! are only meaningful against the overlay they were taken from: after a new
//! run, row 312 is some other nucleus, and reading its values beside the old
//! highlight — or colouring the new outlines with the old table — shows
//! numbers against objects they do not belong to, which looks exactly like a
//! result. So both remember [`Detector::revision`] and are dropped the moment
//! it moves, and a selection also remembers its row's bounding box and is
//! dropped if the row under it no longer has that box, which also catches a
//! caller that replaced the overlay without going through
//! [`Detector::show`].
//!
//! # Which values, and whose
//!
//! The per-cell measurement tail of QuPath's pipeline is **not ported**. What
//! a run carries is what the pipeline computes on the way — the columns of
//! [`Detector::measurements`] — and this pane lists whatever columns are
//! there, so a column added to a run later appears without this changing.
//! The two it has today are this project's numbers, described in [`about`];
//! neither has been compared with a QuPath measurement, and the pane says so
//! beside each value rather than once at the top where it can be missed.

use egui::RichText;
use qupath_pipeline as pipeline;
use qupath_viewer::colour_map::{self, HIGH_PERCENTILE, INFERNO, LOW_PERCENTILE, Range};
use qupath_viewer::compose::{Framebuffer, pack};
use qupath_viewer::{Overlay, ScreenPoint, SlidePoint, Viewport, pick};

use crate::chrome::Command;
use crate::detect::Detector;
use crate::state::App;

/// The colour a selected outline is drawn in.
///
/// Cyan, and not QuPath's selection yellow (`PathPrefs.colorSelectedObject`):
/// the plain outlines here are yellow-green and the top of [`INFERNO`] is pale
/// yellow, so a yellow selection would vanish into either. Cyan is on neither
/// and reads against haematoxylin and eosin alike. It is also drawn three
/// pixels wide ([`pick::HIGHLIGHT_OFFSETS`]), so it differs from every other
/// outline in two ways rather than one.
pub const SELECTED_COLOUR: u32 = pack(0, 255, 255);

/// How far, in window pixels, the pointer may move between press and release
/// and still be a click rather than a drag.
///
/// A drag pans from its first pixel, as it always has — this does not delay
/// panning — so a click with a shaky hand moves the view by a pixel or two and
/// still selects. Physical pixels, like every coordinate the event handler
/// sees; at a scale factor of two it is two points.
pub const CLICK_SLOP_PX: f32 = 4.0;

/// A selected object, and what is needed to know it is still that object.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Selected {
    revision: u64,
    row: usize,
    bounds: [f32; 4],
}

/// A column's colours, and the range and counts the legend shows.
#[derive(Debug, Clone)]
pub struct Map {
    revision: u64,
    /// The column this was built from.
    pub column: String,
    /// The value range the two ends of the colour map stand for.
    pub range: Range,
    /// One colour per row.
    colours: Vec<u32>,
    /// Rows with a finite value.
    pub valued: usize,
    /// Rows with no value, drawn in [`colour_map::NO_VALUE`].
    pub missing: usize,
    /// Rows below and above the range, drawn in the end colours.
    pub outside: (usize, usize),
}

/// The selection, the colour choice, and the buffers that draw them.
#[derive(Debug, Default)]
pub struct Inspect {
    selected: Option<Selected>,
    /// The column chosen, by name, so a new run with the same columns keeps
    /// the choice and one without it quietly falls back to plain.
    colour_by: Option<String>,
    map: Option<Map>,
    /// Per-frame: which object each segment belongs to.
    owners: Vec<u32>,
    /// Per-frame: the selected outline's segments.
    highlight: Vec<ScreenPoint>,
}

impl Inspect {
    /// Nothing selected and outlines plain.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The selected row, if the selection still names an object of the
    /// overlay `detector` holds.
    #[must_use]
    pub fn selected(&self, detector: &Detector) -> Option<usize> {
        let selected = self.selected?;
        let overlay = detector.overlay.as_ref()?;
        (selected.revision == detector.revision()
            && overlay.bounds().get(selected.row) == Some(&selected.bounds))
        .then_some(selected.row)
    }

    /// Selects the object under `point`, or clears the selection over empty
    /// slide. Says whether anything changed.
    pub fn click(&mut self, detector: &Detector, point: SlidePoint) -> bool {
        let before = self.selected(detector);
        let after = detector
            .overlay
            .as_ref()
            .and_then(|overlay| pick::object_at(overlay, point).map(|row| (overlay, row)));
        self.selected = after.map(|(overlay, row)| Selected {
            revision: detector.revision(),
            row,
            bounds: overlay.bounds()[row],
        });
        before != after.map(|(_, row)| row)
    }

    /// The column outlines are coloured by, or `None` for plain.
    #[must_use]
    pub fn colour_by(&self) -> Option<&str> {
        self.colour_by.as_deref()
    }

    /// Chooses the column to colour by, and builds its colours.
    pub fn set_colour_by(&mut self, column: Option<String>, detector: &Detector) {
        self.colour_by = column;
        self.map = None;
        self.refresh(detector);
    }

    /// Brings the selection and the colour table up to date with `detector`.
    ///
    /// Called once a frame, after the detector has been polled and before the
    /// interface is laid out. Costs a comparison unless the run or the column
    /// changed, when it rebuilds the table — about a millisecond at 150,000
    /// objects (`examples/pick_cost.rs`).
    pub fn refresh(&mut self, detector: &Detector) {
        if self.selected(detector).is_none() {
            self.selected = None;
        }
        let current = self.map.as_ref().is_some_and(|map| {
            map.revision == detector.revision()
                && self.colour_by.as_deref() == Some(map.column.as_str())
        });
        if current {
            return;
        }
        self.map = self
            .colour_by
            .as_deref()
            .and_then(|column| build(detector, column));
    }

    /// The colour table in use, if outlines are being coloured and the table
    /// is for the overlay `detector` holds.
    #[must_use]
    pub fn map(&self, detector: &Detector) -> Option<&Map> {
        let map = self.map.as_ref()?;
        let rows = detector.overlay.as_ref().map_or(0, Overlay::len);
        (map.revision == detector.revision() && map.colours.len() == rows).then_some(map)
    }

    /// Projects this frame's outlines into `segments`, and the selection into
    /// a buffer of its own. Returns how many objects were outlined.
    ///
    /// The plain path is `Overlay::segments_into` exactly as before; a colour
    /// table adds the owner of each segment and changes nothing about which
    /// segments there are.
    pub fn project(
        &mut self,
        detector: &Detector,
        view: Viewport,
        min_screen_px: f32,
        segments: &mut Vec<ScreenPoint>,
    ) -> usize {
        self.owners.clear();
        self.highlight.clear();
        let Some(overlay) = detector.overlay.as_ref() else {
            segments.clear();
            return 0;
        };
        let drawn = if self.map(detector).is_some() {
            overlay.owned_segments_into(view, min_screen_px, segments, &mut self.owners)
        } else {
            overlay.segments_into(view, min_screen_px, segments)
        };
        if let Some(row) = self.selected(detector) {
            pick::highlight_into(overlay, row, view, &mut self.highlight);
        }
        drawn
    }

    /// Forgets the last frame's outlines, for a frame that draws none.
    pub fn forget_frame(&mut self) {
        self.owners.clear();
        self.highlight.clear();
    }

    /// Draws what [`Inspect::project`] projected: the outlines, in their
    /// colours or in `plain`, and the selection over them.
    pub fn paint(
        &self,
        detector: &Detector,
        frame: &mut Framebuffer,
        segments: &[ScreenPoint],
        plain: u32,
    ) {
        match self.map(detector) {
            Some(map) if !self.owners.is_empty() => {
                frame.draw_owned_segments(segments, &self.owners, &map.colours);
            }
            _ => frame.draw_segments(segments, plain),
        }
        frame.draw_segments(&self.highlight, SELECTED_COLOUR);
    }
}

/// The colour table for `column`, or `None` if the run has no such column,
/// its length disagrees with the overlay, or it holds no finite value.
fn build(detector: &Detector, column: &str) -> Option<Map> {
    let overlay = detector.overlay.as_ref()?;
    let (_, values) = detector
        .measurements
        .iter()
        .find(|(name, _)| *name == column)?;
    if values.len() != overlay.len() {
        return None;
    }
    let range = Range::robust(values)?;
    let mut colours = Vec::new();
    range.colours_into(values, &mut colours);
    let missing = values.iter().filter(|v| v.is_nan()).count();
    let below = values.iter().filter(|&&v| v < range.low).count();
    let above = values.iter().filter(|&&v| v > range.high).count();
    Some(Map {
        revision: detector.revision(),
        column: column.to_owned(),
        range,
        colours,
        valued: values.len() - missing,
        missing,
        outside: (below, above),
    })
}

/// What the pane says about a column: its unit and where the number comes
/// from.
pub struct About {
    /// Printed after a value. Empty when there is none worth printing.
    pub unit: &'static str,
    /// One or two sentences on what the number is and what it has been checked
    /// against, shown on hover.
    pub note: &'static str,
}

/// The unit and provenance of a column, by its name.
///
/// Keyed on `qupath_pipeline`'s own constants, so a renamed column falls
/// through to the unknown case rather than keeping a description of something
/// else. A column this does not know — one a later change adds — is listed
/// with no unit and a note saying so, rather than not listed.
#[must_use]
pub fn about(column: &str) -> About {
    if column == pipeline::AREA {
        About {
            unit: "px",
            note: "Pixels in the nucleus mask, in level-0 pixels (scaled by the \
                   square of the downsample when a run read a coarser level). \
                   This project's number. QuPath's own nucleus area is \
                   calibrated and is not ported, and this has not been compared \
                   with it; the masks themselves are compared with QuPath's in \
                   DIVERGENCES.md D-009.",
        }
    } else if column == pipeline::MEAN {
        About {
            unit: "OD",
            note: "Mean background-subtracted haematoxylin optical density over \
                   the nucleus — the value the detection's intensity threshold \
                   tests. This project's number. QuPath's per-cell measurements \
                   are not ported, and this has not been compared with any of \
                   them.",
        }
    } else {
        About {
            unit: "",
            note: "Not described here, and not checked against QuPath.",
        }
    }
}

/// A value for the pane: enough digits to tell two nuclei apart, and a dash
/// for none.
#[must_use]
pub fn format_value(value: f32) -> String {
    if !value.is_finite() {
        return String::from("—");
    }
    let magnitude = value.abs();
    if magnitude >= 1000.0 {
        format!("{value:.0}")
    } else if magnitude >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.3}")
    }
}

/// The **Objects** pane: the colour choice with its legend, and the selected
/// object's values.
pub fn pane(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    ui.heading("Objects");
    let detector = &app.detector;
    let Some(overlay) = detector.overlay.as_ref().filter(|o| !o.is_empty()) else {
        ui.label(
            RichText::new("Detect cells first (d); then click an outline to see its values.")
                .weak(),
        );
        return;
    };
    ui.label(
        RichText::new(format!("{} detected", overlay.len()))
            .weak()
            .small(),
    );
    ui.separator();

    colour_choice(ui, app, command);
    if let Some(map) = app.inspect.map(detector) {
        legend(ui, map);
    }
    ui.separator();

    let Some(row) = app.inspect.selected(detector) else {
        ui.label(RichText::new("Click an outline to select it.").weak())
            .on_hover_text("Clicking empty slide clears the selection. Dragging pans, as always.");
        return;
    };
    ui.label(RichText::new(format!("Object {} of {}", row + 1, overlay.len())).strong())
        .on_hover_text(format!("Row {row} of the run's table, counting from 0."));

    let mpp = app
        .open
        .as_ref()
        .and_then(|open| open.slide().microns_per_pixel());
    if let Some(centre) = pick::centroid(overlay.outline(row)) {
        ui.label(RichText::new("Centroid").strong());
        let microns = mpp.map_or(String::new(), |mpp| {
            format!("\n{:.1}, {:.1} µm", centre.x * mpp, centre.y * mpp)
        });
        ui.label(format!("{:.1}, {:.1} px{microns}", centre.x, centre.y))
            .on_hover_text(
                "The centre of area of the traced outline, in level-0 pixels. \
                 This project's number, from the unsmoothed outline; QuPath \
                 reports its own ROI's centroid and the two have not been \
                 compared.",
            );
        unchecked(ui);
    }

    for (name, values) in &detector.measurements {
        let Some(&value) = values.get(row) else {
            continue;
        };
        let about = about(name);
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(*name).strong()).wrap());
        let unit = if about.unit.is_empty() || !value.is_finite() {
            String::new()
        } else {
            format!(" {}", about.unit)
        };
        ui.label(format!("{}{unit}", format_value(value)))
            .on_hover_text(about.note);
        unchecked(ui);
    }
}

/// The tag under every value: whose number it is.
fn unchecked(ui: &mut egui::Ui) {
    ui.label(
        RichText::new("ours · not checked against QuPath")
            .weak()
            .small(),
    )
    .on_hover_text(
        "Computed by this project. QuPath's per-cell measurements are not \
             ported, so there is nothing of QuPath's this has been compared with.",
    );
}

/// The colour-by control: plain, or one of whatever columns the run carries.
fn colour_choice(ui: &mut egui::Ui, app: &App, command: &mut Command) {
    let chosen = app.inspect.colour_by().map(str::to_owned);
    let mut picked = chosen.clone();
    ui.label(RichText::new("Colour outlines by").small());
    // The width is the pane's and not the widest label's, for `sort_by`'s
    // reason in `chrome`: a combo sizes itself from its choices, and a column
    // name is as long as whoever named it made it.
    let width = (ui.available_width() - 4.0).max(72.0);
    egui::ComboBox::from_id_salt("colour by")
        .width(width)
        .selected_text(RichText::new(chosen.as_deref().unwrap_or("Nothing (plain)")).small())
        .show_ui(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.selectable_value(&mut picked, None, "Nothing (plain)");
            for (name, _) in &app.detector.measurements {
                ui.selectable_value(&mut picked, Some((*name).to_owned()), *name);
            }
        })
        .response
        .on_hover_text(
            "Colour every outline by one measurement, with QuPath's default \
             colour map (Inferno). The range is the 2nd to 98th percentile of \
             the values, so one outlier does not flatten the rest.",
        );
    if picked != chosen && *command == Command::None {
        *command = Command::ColourBy(picked);
    }
}

/// Slices the legend's colour bar is painted in.
const LEGEND_STEPS: usize = 64;

/// The colour bar, the range it stands for, and what falls outside it.
fn legend(ui: &mut egui::Ui, map: &Map) {
    let width = ui.available_width().clamp(60.0, 240.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let step = rect.width() / LEGEND_STEPS as f32;
    for slice in 0..LEGEND_STEPS {
        // The entry at the centre of the slice, through the same index rule
        // an outline's colour goes through, over a range of 0..1.
        let centre = (slice as f64 + 0.5) / LEGEND_STEPS as f64;
        let colour = INFERNO[colour_map::index(centre, 0.0, 1.0)];
        let left = rect.left() + step * slice as f32;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(left, rect.top()),
                egui::pos2(left + step + 0.5, rect.bottom()),
            ),
            0.0,
            to_egui(colour),
        );
    }
    let unit = about(&map.column).unit;
    ui.horizontal(|ui| {
        ui.label(RichText::new(format_value(map.range.low)).small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{} {unit}", format_value(map.range.high))).small());
        });
    });
    let mut notes = vec![format!(
        "Range: {LOW_PERCENTILE:.0}th–{HIGH_PERCENTILE:.0}th percentile of {} values.",
        map.valued
    )];
    if map.outside != (0, 0) {
        notes.push(format!(
            "{} below and {} above are drawn in the end colours.",
            map.outside.0, map.outside.1
        ));
    }
    if map.missing > 0 {
        notes.push(format!("{} with no value are grey.", map.missing));
    }
    ui.add(egui::Label::new(RichText::new(notes.join(" ")).weak().small()).wrap());
}

/// A packed frame colour as `egui`'s.
fn to_egui(colour: u32) -> egui::Color32 {
    let [_, red, green, blue] = colour.to_be_bytes();
    egui::Color32::from_rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qupath_viewer::compose::CLEAR;

    const PLAIN: u32 = pack(200, 255, 0);

    /// Two squares far apart on both axes, with an area and a mean column,
    /// and a view that shows both at 1:1.
    fn two_objects() -> (Detector, Viewport) {
        let mut overlay = Overlay::new();
        overlay.push_level_ring(&[[10, 10], [30, 10], [30, 30], [10, 30]], 1.0);
        overlay.push_level_ring(&[[60, 45], [80, 45], [80, 65], [60, 65]], 1.0);
        let mut detector = Detector::new();
        detector.show(
            overlay,
            vec![
                (pipeline::AREA, vec![400.0, 400.0]),
                (pipeline::MEAN, vec![0.1, 0.9]),
            ],
        );
        let view = Viewport {
            centre: SlidePoint::new(48.0, 40.0),
            scale: 1.0,
            width: 96,
            height: 80,
        };
        (detector, view)
    }

    /// Projects and paints one frame of `inspect` over `detector`.
    fn frame(inspect: &mut Inspect, detector: &Detector, view: Viewport) -> Framebuffer {
        let mut segments = Vec::new();
        let mut frame = Framebuffer::new(view.width, view.height);
        inspect.project(detector, view, 0.0, &mut segments);
        inspect.paint(detector, &mut frame, &segments, PLAIN);
        frame
    }

    /// Plain, coloured and selected, side by side.
    ///
    /// The claim is three differences and one sameness: colouring changes the
    /// colour of every outline pixel to its own object's and no pixel's
    /// coverage; selecting adds cyan around the one object and touches nothing
    /// at the other. Any one of the three pictures alone looks like outlines.
    #[test]
    fn colouring_changes_colours_and_selecting_changes_one_object() {
        let (detector, view) = two_objects();
        let mut inspect = Inspect::new();
        let plain = frame(&mut inspect, &detector, view);

        inspect.set_colour_by(Some(pipeline::MEAN.to_owned()), &detector);
        let map = inspect.map(&detector).expect("a table").clone();
        let coloured = frame(&mut inspect, &detector, view);

        let mut lit = 0;
        for (index, (&p, &c)) in plain.pixels().iter().zip(coloured.pixels()).enumerate() {
            assert_eq!(p != CLEAR, c != CLEAR, "coverage moved at {index}");
            if c == CLEAR {
                continue;
            }
            lit += 1;
            // Left square is object 0 (low mean), right is object 1 (high).
            let row = usize::from(index % 96 >= 48);
            assert_eq!(
                c, map.colours[row],
                "pixel {index} is not its object's colour"
            );
        }
        assert!(lit > 100, "only {lit} outline pixels");
        assert_ne!(
            map.colours[0], map.colours[1],
            "the two means share a colour"
        );
        assert_ne!(map.colours[0], PLAIN);

        // Select the right-hand object by clicking inside it.
        assert!(inspect.click(&detector, SlidePoint::new(70.0, 55.0)));
        let selected = frame(&mut inspect, &detector, view);
        let mut cyan = 0;
        for (index, (&c, &s)) in coloured.pixels().iter().zip(selected.pixels()).enumerate() {
            if index % 96 < 48 {
                assert_eq!(
                    c, s,
                    "selecting the right object changed the left at {index}"
                );
            } else if s == SELECTED_COLOUR {
                cyan += 1;
            }
        }
        // Three pixels wide around a 20-pixel square.
        assert!(cyan > 200, "only {cyan} highlight pixels");

        // Clicking empty slide clears, and the frame goes back to coloured.
        assert!(inspect.click(&detector, SlidePoint::new(45.0, 70.0)));
        assert_eq!(
            frame(&mut inspect, &detector, view).pixels(),
            coloured.pixels()
        );
        // And back to plain.
        inspect.set_colour_by(None, &detector);
        assert_eq!(
            frame(&mut inspect, &detector, view).pixels(),
            plain.pixels()
        );
    }

    #[test]
    fn a_column_the_run_does_not_have_draws_plain() {
        let (detector, view) = two_objects();
        let mut inspect = Inspect::new();
        let plain = frame(&mut inspect, &detector, view);
        inspect.set_colour_by(Some(String::from("Cell: Circularity")), &detector);
        assert!(inspect.map(&detector).is_none());
        assert_eq!(
            frame(&mut inspect, &detector, view).pixels(),
            plain.pixels()
        );
        // The choice is kept, so a later run carrying the column uses it.
        assert_eq!(inspect.colour_by(), Some("Cell: Circularity"));
    }

    #[test]
    fn the_legend_counts_what_the_range_leaves_out() {
        let mut overlay = Overlay::new();
        let mut values = Vec::new();
        for step in 0..102u32 {
            overlay.push_level_ring(&[[step * 4, 0], [step * 4 + 2, 0], [step * 4 + 2, 2]], 1.0);
            values.push(step as f32);
        }
        values[101] = f32::NAN;
        let mut detector = Detector::new();
        detector.show(overlay, vec![(pipeline::MEAN, values)]);
        let mut inspect = Inspect::new();
        inspect.set_colour_by(Some(pipeline::MEAN.to_owned()), &detector);
        let map = inspect.map(&detector).expect("a table");
        // 101 finite values 0..=100: ranks 2 and 98.
        assert_eq!((map.range.low, map.range.high), (2.0, 98.0));
        assert_eq!(map.outside, (2, 2));
        assert_eq!((map.valued, map.missing), (101, 1));
        assert_eq!(map.colours[101], colour_map::NO_VALUE);
    }

    #[test]
    fn a_value_prints_with_digits_that_fit_it() {
        assert_eq!(format_value(0.123_456), "0.123");
        assert_eq!(format_value(36.25), "36.2");
        assert_eq!(format_value(9216.0), "9216");
        assert_eq!(format_value(f32::NAN), "—");
    }
}
