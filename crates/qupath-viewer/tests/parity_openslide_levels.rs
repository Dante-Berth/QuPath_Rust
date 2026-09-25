//! Checks `Slide::levels` against the pyramid QuPath gets from OpenSlide.
//!
//! This is the comparison `docs/dev/VIEWER.md` §8.8 spent a long time asking for
//! and could not make. The entry there is not "write the stored metadata block"
//! but "record what OpenSlide reports", because the block *replaces* what QuPath
//! would otherwise have read: `AbstractServerBuilder.build` calls `setMetadata`
//! with it when the image is opened, so a level table that disagrees with
//! OpenSlide's is not a worse description of the slide — it is QuPath reading
//! the wrong pixels, silently, for as long as the project exists.
//!
//! `Slide::open` keeps the tiled directories and drops the label, macro and
//! thumbnail; OpenSlide's Aperio driver makes its own choice about what counts
//! as a level. Nothing had ever compared them. This does.
//!
//! # What it found
//!
//! They agree, on both slides, in every field: the level count, each level's
//! width and height, each downsample, and the preferred tile size. That is the
//! result that makes writing the block *safe* rather than the one that makes it
//! unnecessary — and it is worth having as a test rather than a note, because it
//! is the kind of agreement a change to either reader could end.
//!
//! # Why two slides
//!
//! CMU-1 has three levels; CMU-1-Small-Region has one. The one that matters
//! more is the small region, and for a reason that is easy to miss: its tile is
//! **240** pixels, and OpenSlide's fallback when a driver publishes no tile size
//! is 256. On CMU-1 alone, "the tile sizes agree" would be two defaults
//! coinciding — see the note in `MEMORY.md` about a factor of 1 hiding a bug.
//! The 240 is what makes it a measurement.
//!
//! The fixtures are recorded by `parity/record-openslide-levels.sh`. The slides
//! themselves are untracked (`docs/test-data.md`), so each test skips when its
//! slide is absent, the same as the reader's own tests.

use std::path::{Path, PathBuf};

use qupath_slide::Slide;

const CMU_1: &str = include_str!("../../../parity/fixtures/OpenSlideLevels-CMU-1.json");
const SMALL: &str =
    include_str!("../../../parity/fixtures/OpenSlideLevels-CMU-1-Small-Region.json");

/// One level's `{ ... }` block, in file order.
fn levels(fixture: &str) -> Vec<&str> {
    fixture
        .split("    {\"index\": ")
        .skip(1)
        .map(|block| block.split('}').next().expect("a level block"))
        .collect()
}

/// A numeric field of a block, as text.
///
/// Text rather than `f64` on purpose: `1.0` and `1` are the same number and a
/// different recording, and the fields compared here are checked as numbers
/// where they are numbers. Parsing happens at the call site, where the type is
/// known.
fn field<'a>(block: &'a str, key: &str) -> &'a str {
    let needle = format!("\"{key}\": ");
    let at = block
        .find(&needle)
        .unwrap_or_else(|| panic!("no {key} in {block}"))
        + needle.len();
    let rest = &block[at..];
    let end = rest.find([',', '\n']).unwrap_or(rest.len());
    rest[..end].trim()
}

/// A top-level integer field of the fixture.
fn number(fixture: &str, key: &str) -> u64 {
    field(fixture, key).parse().expect("an integer")
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

/// Every field of the level table, against one fixture.
fn check(name: &str, fixture: &str) {
    let Some(slide) = slide(name) else {
        return;
    };
    let recorded = levels(fixture);

    assert_eq!(
        slide.levels().len(),
        recorded.len(),
        "level count for {name}: OpenSlide keeps a different set of directories"
    );
    assert_eq!(
        usize::try_from(number(fixture, "levelCount")).expect("a level count"),
        recorded.len(),
        "the fixture's own levelCount disagrees with the levels it lists"
    );

    for (level, block) in slide.levels().iter().zip(&recorded) {
        let index: usize = block
            .split(',')
            .next()
            .expect("the index")
            .trim()
            .parse()
            .expect("an index");
        assert_eq!(level.index, index, "level order for {name}");
        assert_eq!(
            u64::from(level.width),
            field(block, "width").parse::<u64>().expect("a width"),
            "level {index} width for {name}"
        );
        assert_eq!(
            u64::from(level.height),
            field(block, "height").parse::<u64>().expect("a height"),
            "level {index} height for {name}"
        );

        // Compared as a number, not as text: QuPath prints `4.0` and this side
        // computes `4`, and those are the same downsample. The tolerance is
        // tight because both sides derive it from the widths — QuPath
        // *declines* OpenSlide's own averaged downsample for being inexact
        // (`OpenslideImageServer` says so in a comment), so a mismatch here
        // would mean one of them changed its mind about how to compute it, not
        // floating-point drift.
        let downsample: f64 = field(block, "downsample").parse().expect("a downsample");
        assert!(
            (level.downsample - downsample).abs() < 1e-9,
            "level {index} downsample for {name}: {} vs {downsample}",
            level.downsample
        );
    }
}

#[test]
fn the_three_level_pyramid_matches_openslide() {
    check("CMU-1.svs", CMU_1);
}

#[test]
fn the_single_level_slide_matches_openslide() {
    check("CMU-1-Small-Region.svs", SMALL);
}

/// The tile size QuPath would store, against the one this reader found.
///
/// Separate from the level table because it comes from a different place on
/// both sides — a property on QuPath's, the TIFF tags on ours — and because it
/// is the field with the trap in it. OpenSlide's default when a driver publishes
/// no tile size is 256, so this passing on CMU-1 alone would prove nothing.
/// CMU-1-Small-Region's 240 is what makes it evidence.
#[test]
fn the_preferred_tile_size_matches_openslide() {
    for (name, fixture) in [("CMU-1.svs", CMU_1), ("CMU-1-Small-Region.svs", SMALL)] {
        let Some(slide) = slide(name) else {
            continue;
        };
        let level = &slide.levels()[0];
        assert_eq!(
            u64::from(level.tile_width),
            number(fixture, "preferredTileWidth"),
            "preferred tile width for {name}"
        );
        assert_eq!(
            u64::from(level.tile_height),
            number(fixture, "preferredTileHeight"),
            "preferred tile height for {name}"
        );
    }
}

/// The small region's tile is not OpenSlide's default, which is the whole
/// reason the test above is worth running.
///
/// Asserted here so that a fixture re-recorded against a slide that *does* use
/// 256 everywhere cannot quietly turn the tile comparison back into two
/// defaults agreeing.
#[test]
fn one_of_the_slides_has_a_tile_size_that_is_not_the_default() {
    assert_eq!(
        number(SMALL, "preferredTileWidth"),
        240,
        "the small region's tile size is what makes the comparison a measurement"
    );
}

/// The pixel size, which the entry metadata already carries and which
/// `setMetadata` would also overwrite.
#[test]
fn the_pixel_size_matches_openslide() {
    for (name, fixture) in [("CMU-1.svs", CMU_1), ("CMU-1-Small-Region.svs", SMALL)] {
        let Some(slide) = slide(name) else {
            continue;
        };
        let recorded: f64 = field(fixture, "pixelWidthMicrons")
            .parse()
            .expect("a pixel size");
        let ours = slide.microns_per_pixel().expect("a pixel size");
        assert!(
            (ours - recorded).abs() < 1e-9,
            "pixel size for {name}: {ours} vs {recorded}"
        );
    }
}
