//! Checks an image *opened from a project* against this project's own reader.
//!
//! `parity_project_import.rs` stops before the pixels, on purpose: it asks
//! whether QuPath can read a project this project wrote. This asks the harder
//! question — whether QuPath can **work in** one. Those fail differently. A
//! project whose entries all list correctly can still carry a `ServerBuilder`
//! that will not build, a URI that resolves to the wrong file, or a stored
//! metadata block that `setMetadata` applies over what OpenSlide read, so that
//! the image opens and every pixel after that is the wrong one.
//!
//! `parity/java/ProjectOpen.java` opens every entry through
//! `entry.readImageData()` — what double-clicking a row reaches, and the path
//! that goes through `getServerBuilder()` — and records what the server ended up
//! with, including a block of real pixels.
//!
//! # The blocker was not real
//!
//! §8.8 carried this as blocked on "a native library, and there is none on this
//! machine". That was checked with `ldconfig` and `apt`, and QuPath does not use
//! the system library: `qupath-extension-openslide` depends on a jar carrying
//! its own `libopenslide.so`, which JNA extracts at load time. Nothing needed
//! installing. See `parity_openslide_levels.rs`, which is the other half of the
//! same correction.
//!
//! # Where the pixels come from, which is the whole value of this file
//!
//! Metadata can agree with itself. Two readers that both report a 46000-pixel
//! width and both return the wrong tile agree on everything this file checks
//! except the pixels — so the pixels are the point, and *where* they are read
//! decides whether the check means anything.
//!
//! Not the centre of the slide, and not its origin: both are background here,
//! and two readers both returning white agree without either being right.
//! `docs/dev/VIEWER.md` §6 records that mistake being made three times with three
//! different instruments. The first version of this recorder made it a fourth
//! time — it clamped a large slide's tissue coordinates into the small region
//! and landed on its bottom-right corner, mean 243 against tissue's 99 — and the
//! uniformity check let it through because background is not perfectly uniform.
//! Both sides now name tissue per slide, and the recorder refuses a block
//! spanning fewer than 24 levels.
//!
//! # Why one entry is not pixel-compared
//!
//! `CMU-1-JP2K-33005.svs` is Aperio JPEG 2000, and this project decodes baseline
//! JPEG only — `read_tile_into` returns `UndecodableCodec`, and §3 has why that
//! is deliberate. Its metadata is still compared. The fixture says so per entry
//! in `pixelsComparable` rather than leaving this side to infer it from a file
//! name, and the test asserts that at least one entry *is* comparable, so a
//! future fixture cannot quietly skip every pixel check and still pass.

use std::path::{Path, PathBuf};

use qupath_slide::Slide;

const FIXTURE: &str = include_str!("../../../parity/fixtures/ProjectOpen.json");

/// One `{ ... }` block per entry QuPath opened, in file order.
fn entries(fixture: &str) -> Vec<&str> {
    fixture
        .split("\n    {\n")
        .skip(1)
        .map(|block| block.split("\n    }").next().expect("an entry block"))
        .collect()
}

/// The text of one field. A quoted value ends at its closing quote.
fn text<'a>(block: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\": ");
    let at = block
        .find(&needle)
        .unwrap_or_else(|| panic!("no {key} in {block}"))
        + needle.len();
    let rest = &block[at..];
    if let Some(body) = rest.strip_prefix('"') {
        let end = body.find('"').expect("an unterminated string");
        return &body[..end];
    }
    let end = rest.find([',', '\n']).unwrap_or(rest.len());
    rest[..end].trim()
}

fn number(block: &str, key: &str) -> u64 {
    text(block, key).parse().expect("an integer")
}

fn flag(block: &str, key: &str) -> bool {
    text(block, key).parse().expect("a boolean")
}

/// The `pixels` array, as channel values in the order QuPath wrote them.
fn pixels(block: &str) -> Vec<u8> {
    let at = block.find("\"pixels\": [").expect("a pixels array") + "\"pixels\": [".len();
    let rest = &block[at..];
    let end = rest.find(']').expect("an unterminated pixels array");
    rest[..end]
        .split(',')
        .map(|value| value.trim().parse().expect("a channel value"))
        .collect()
}

/// The slide beside the fixture, or `None` when it is not checked out.
fn slide(name: &str) -> Option<Slide> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name);
    if !path.is_file() {
        qupath_testing::skipped(&format!("data/{name} not present"));
        return None;
    }
    Some(Slide::open(&path).expect("the slide opens"))
}

#[test]
fn every_entry_opened() {
    for block in entries(FIXTURE) {
        assert!(
            flag(block, "opened"),
            "an entry QuPath could not open: {block}"
        );
        // The server that answered, because a project that opened through some
        // other reader would be testing that reader instead.
        assert_eq!(
            text(block, "serverClass"),
            "OpenslideImageServer",
            "an entry opened through an unexpected server"
        );
    }
}

#[test]
fn the_dimensions_match_our_reader() {
    for block in entries(FIXTURE) {
        let name = text(block, "slideFileName");
        let Some(slide) = slide(name) else {
            continue;
        };
        let level = &slide.levels()[0];
        assert_eq!(
            u64::from(level.width),
            number(block, "width"),
            "width {name}"
        );
        assert_eq!(
            u64::from(level.height),
            number(block, "height"),
            "height {name}"
        );
        assert_eq!(
            slide.levels().len() as u64,
            number(block, "levelCount"),
            "level count {name}"
        );
        assert_eq!(
            u64::from(level.tile_width),
            number(block, "preferredTileWidth"),
            "tile width {name}"
        );
    }
}

/// The pixels themselves, which is the check the rest of the file exists for.
#[test]
fn the_pixels_match_our_reader() {
    let mut compared = 0;
    for block in entries(FIXTURE) {
        if !flag(block, "pixelsComparable") {
            continue;
        }
        let name = text(block, "slideFileName");
        let Some(slide) = slide(name) else {
            continue;
        };

        let x = u32::try_from(number(block, "pixelX")).expect("an x");
        let y = u32::try_from(number(block, "pixelY")).expect("a y");
        let size = u32::try_from(number(block, "pixelBlock")).expect("a block size");
        let recorded = pixels(block);

        let ours = slide
            .read_region_rgb(0, x, y, size, size)
            .expect("the region decodes");

        assert_eq!(
            ours.len(),
            recorded.len(),
            "pixel count at {x},{y} of {name}"
        );
        assert_eq!(
            ours, recorded,
            "pixels at {x},{y} of {name}: QuPath's decode and ours disagree"
        );
        compared += 1;
    }

    // A fixture in which nothing is comparable would pass every assertion above
    // by running none of them.
    assert!(
        compared > 0
            || entries(FIXTURE)
                .iter()
                .all(|b| slide(text(b, "slideFileName")).is_none()),
        "no entry was pixel-compared, and the slides are present"
    );
}

/// At least one entry must be pixel-comparable, whatever the fixture says.
///
/// Guards the skip in the test above from growing to cover everything: a
/// recording in which every entry is JP2K would make that test vacuous, and
/// vacuous is how a parity check stops being one.
#[test]
fn some_entry_is_pixel_comparable() {
    assert!(
        entries(FIXTURE)
            .iter()
            .any(|block| flag(block, "pixelsComparable")),
        "no entry in the fixture is pixel-comparable"
    );
}

/// The recorded block must carry information, not background.
///
/// The recorder refuses to write a flat one, but the fixture is committed and
/// this is what keeps that promise checkable without rerunning a JVM.
#[test]
fn the_recorded_pixels_are_tissue() {
    for block in entries(FIXTURE) {
        let values = pixels(block);
        let low = values.iter().copied().min().expect("a pixel");
        let high = values.iter().copied().max().expect("a pixel");
        assert!(
            high - low >= 24,
            "the block for {} spans only {} levels, which is background",
            text(block, "slideFileName"),
            high - low
        );
    }
}
