//! Reads pyramidal whole-slide images as tiles.
//!
//! This is the first crate in the workspace that touches a real slide. Until
//! now every ported component ran on buffers handed to it by a test or a
//! benchmark, so the end-to-end figure in `docs/profiles/README.md` could not
//! move no matter how fast those components became.
//!
//! # Scope, and what it refuses
//!
//! Aperio SVS is a tiled TIFF. The tiles of `CMU-1.svs` are baseline JPEG, so
//! this crate reads them with a pure-Rust decoder and no native dependency.
//! That is *not* true of SVS in general: the JPEG 2000 variants have no mature
//! pure-Rust decoder, which is exactly what §9 of the project guidelines warns
//! about. Those are rejected by name — [`SlideError::UndecodableCodec`] says
//! that reading them needs OpenSlide or OpenJPEG through FFI — rather than
//! being approximated or silently mis-decoded.
//!
//! So this crate is the pure-Rust fast path, not a replacement for the FFI
//! work §9 describes. `docs/ROADMAP.md` tracks that.
//!
//! # Memory
//!
//! A slide does not fit in RAM: `CMU-1.svs` is 46000x32914 at full resolution,
//! or 4.5 GB as RGB. Nothing here materialises a level. Tiles are read from the
//! file on demand through positioned reads, so [`Slide`] is `Sync` and a tile
//! loop needs no lock — see [`Slide::read_tile_into`], which fills a caller-owned
//! buffer and so allocates nothing per tile.

mod error;
mod io;
mod jpeg;
mod tiff;

use std::fs::File;
use std::path::{Path, PathBuf};

pub use error::{Result, SlideError};
pub use tiff::Compression;

use tiff::Directory;

/// One pyramid level: a tiled image at some downsample of level 0.
#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    /// Index into [`Slide::levels`].
    pub index: usize,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tile width in pixels.
    pub tile_width: u32,
    /// Tile height in pixels.
    pub tile_height: u32,
    /// Tiles across.
    pub columns: u32,
    /// Tiles down.
    pub rows: u32,
    /// Level 0 width divided by this level's width. Aperio levels are powers
    /// of two in practice, but this is computed, not assumed.
    pub downsample: f64,
}

impl Level {
    /// Size in bytes of one decoded RGB tile.
    #[must_use]
    pub fn tile_bytes(&self) -> usize {
        self.tile_width as usize * self.tile_height as usize * 3
    }
}

/// An open slide.
///
/// Holds a file handle and the tile index for each level; the pixel data stays
/// on disk until a tile is asked for.
#[derive(Debug)]
pub struct Slide {
    file: File,
    path: PathBuf,
    levels: Vec<Level>,
    directories: Vec<Directory>,
    /// Index into `directories` for each entry of `levels`.
    level_directories: Vec<usize>,
}

impl Slide {
    /// Opens a slide and reads its tile index.
    ///
    /// Reads only the TIFF directories, not pixels, so this is fast and
    /// bounded regardless of slide size.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read, is not a classic TIFF, or contains no
    /// tiled pyramid level.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|source| SlideError::Io {
            path: path.clone(),
            source,
        })?;

        let directories = tiff::read_directories(&file, &path)?;

        // Associated images — label, macro, thumbnail — sit in the same
        // directory chain as the pyramid. Only tiled directories are levels.
        let level_directories: Vec<usize> = directories
            .iter()
            .enumerate()
            .filter(|(_, directory)| directory.is_tiled_level())
            .map(|(index, _)| index)
            .collect();

        if level_directories.is_empty() {
            return Err(SlideError::Unsupported {
                path,
                detail: "no tiled pyramid level; stripped images are not read".to_owned(),
            });
        }

        let base_width = f64::from(directories[level_directories[0]].width);
        let levels = level_directories
            .iter()
            .enumerate()
            .map(|(index, &directory_index)| {
                let directory = &directories[directory_index];
                Level {
                    index,
                    width: directory.width,
                    height: directory.height,
                    tile_width: directory.tile_width.unwrap_or(0),
                    tile_height: directory.tile_height.unwrap_or(0),
                    columns: directory.tile_columns(),
                    rows: directory.tile_rows(),
                    downsample: base_width / f64::from(directory.width),
                }
            })
            .collect();

        Ok(Self {
            file,
            path,
            levels,
            directories,
            level_directories,
        })
    }

    /// The path this slide was opened from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The pyramid levels, coarsest last.
    #[must_use]
    pub fn levels(&self) -> &[Level] {
        &self.levels
    }

    /// Aperio's `ImageDescription` for a level, which carries magnification and
    /// scanner metadata.
    ///
    /// # Errors
    ///
    /// Fails if `level` is out of range.
    pub fn description(&self, level: usize) -> Result<&str> {
        Ok(&self.directory(level)?.description)
    }

    /// Microns per pixel at level 0, from Aperio's `MPP` field.
    ///
    /// `None` when the description carries no `MPP`, which is every slide that
    /// is not Aperio and some that are. A caller that needs a number must decide
    /// what to do without one rather than being handed a guess — see
    /// [`Slide::magnification`] for the same contract.
    ///
    /// # Why this is not a convenience
    ///
    /// Every micron-derived detection default is a length in *pixels*, and the
    /// conversion is this number. `qupath_pipeline::Params::default` hardcoded
    /// CMU-1's 0.4990, so on a slide scanned at 0.25 µm/px every default was out
    /// by a factor of two — a background radius half the size QuPath would use,
    /// silently, with detection still returning plausible objects. That is the
    /// failure mode `docs/VIEWER.md` §6 is written against, and it is why this
    /// reads the value rather than the pipeline assuming it.
    ///
    /// Aperio writes `MPP = 0.4990` as one of the `|`-separated fields of the
    /// level-0 description. Only level 0 is consulted: the field is absent from
    /// the reduced levels, and their pixel size is this one times
    /// [`Level::downsample`].
    #[must_use]
    pub fn microns_per_pixel(&self) -> Option<f64> {
        Self::aperio_field(self.description(0).ok()?, "MPP")?
            .parse::<f64>()
            .ok()
            // A zero or negative pixel size is a corrupt field, and every caller
            // divides by this. Refusing it here means no call site has to.
            .filter(|mpp| *mpp > 0.0)
    }

    /// Scanner magnification at level 0, from Aperio's `AppMag` field.
    ///
    /// `None` when the description does not carry one; see
    /// [`Slide::microns_per_pixel`] for why that is not defaulted.
    #[must_use]
    pub fn magnification(&self) -> Option<f64> {
        Self::aperio_field(self.description(0).ok()?, "AppMag")?
            .parse::<f64>()
            .ok()
            .filter(|magnification| *magnification > 0.0)
    }

    /// One `Key = Value` field of an Aperio description.
    ///
    /// The format is a free-text first line followed by `|`-separated
    /// `Key = Value` pairs. Both the key and the value are trimmed, because
    /// Aperio writes the spaces around `=` and a parser that does not trim finds
    /// nothing while looking like it is working.
    fn aperio_field<'a>(description: &'a str, key: &str) -> Option<&'a str> {
        description.split('|').find_map(|field| {
            let (name, value) = field.split_once('=')?;
            (name.trim() == key).then(|| value.trim())
        })
    }

    /// The coarsest level at least `min_width` pixels wide.
    ///
    /// Choosing the right level is the first item in §0 — doing less work beats
    /// doing the same work faster — so this exists to make the cheap choice the
    /// easy one.
    ///
    /// # This is an *extent* criterion, not a resolution one
    ///
    /// It answers "which level gives me at least this many pixels across". That
    /// is the right question for reading a region at a known size, and the
    /// wrong one for "what does a screen need", because it fails on the
    /// pyramid's own rounding: a level 1.4% narrower than asked is rejected
    /// outright even though nothing about it is visibly softer. `qupath-viewer`
    /// hit exactly that and jumped two levels, decoding 221 tiles where 20 would
    /// do; see `Viewport::level`, which compares [`Level::downsample`] against
    /// the resolution it wants and keeps a tolerance. Pick the criterion that
    /// matches the question rather than assuming this one covers both.
    #[must_use]
    pub fn level_for_width(&self, min_width: u32) -> &Level {
        self.levels
            .iter()
            .rfind(|level| level.width >= min_width)
            .unwrap_or(&self.levels[0])
    }

    fn directory(&self, level: usize) -> Result<&Directory> {
        let index = self
            .level_directories
            .get(level)
            .ok_or_else(|| SlideError::NoSuchLevel {
                path: self.path.clone(),
                level,
                available: self.levels.len(),
            })?;
        Ok(&self.directories[*index])
    }

    /// Decodes one tile into `out` as RGB8, resizing it if needed.
    ///
    /// `out` is reused across calls, so a tile loop allocates nothing. Tiles at
    /// the right and bottom edges are full-sized with padding, as TIFF stores
    /// them; [`Slide::read_region_rgb`] is what crops.
    ///
    /// # Errors
    ///
    /// Fails if the level or tile is out of range, the codec has no pure-Rust
    /// decoder, or the tile's bytes do not decode.
    pub fn read_tile_into(
        &self,
        level: usize,
        column: u32,
        row: u32,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let directory = self.directory(level)?;
        let info = &self.levels[level];

        if !matches!(directory.compression, Compression::Jpeg) {
            return Err(SlideError::UndecodableCodec {
                path: self.path.clone(),
                level,
                compression: directory.compression.to_string(),
            });
        }

        if column >= info.columns || row >= info.rows {
            return Err(SlideError::OutOfBounds {
                path: self.path.clone(),
                level,
                x: column,
                y: row,
                width: 1,
                height: 1,
                level_width: info.columns,
                level_height: info.rows,
            });
        }

        let tile = (row * info.columns + column) as usize;
        let offset = directory.tile_offsets[tile];
        let length = usize::try_from(directory.tile_byte_counts[tile]).unwrap_or(usize::MAX);

        let mut compressed = vec![0u8; length];
        io::read_exact_at(&self.file, &self.path, &mut compressed, offset)?;

        jpeg::decode_into(
            &compressed,
            directory.jpeg_tables.as_deref(),
            // PhotometricInterpretation 2 is RGB: the components need no
            // colour conversion, whatever the JPEG's own markers imply.
            directory.photometric == 2,
            info.tile_width,
            info.tile_height,
            out,
        )
        .map_err(|detail| SlideError::TileDecode {
            path: self.path.clone(),
            level,
            column,
            row,
            detail,
        })
    }

    /// Reads an arbitrary region of a level as RGB8, row-major.
    ///
    /// Decodes only the tiles the region touches. Allocates the output; a tile
    /// loop should call [`Slide::read_region_into`] instead, which reuses both
    /// the output and the decode scratch.
    ///
    /// # Errors
    ///
    /// Fails if the level is out of range, the region leaves the level, or a
    /// tile does not decode.
    pub fn read_region_rgb(
        &self,
        level: usize,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        let mut region = Vec::new();
        let mut tile = Vec::new();
        self.read_region_into(level, x, y, width, height, &mut region, &mut tile)?;
        Ok(region)
    }

    /// Reads an arbitrary region of a level as RGB8 into `out`, row-major.
    ///
    /// `out` and `tile` are both reused across calls, so a tile loop over
    /// regions allocates nothing — which is what §6 requires of the detection
    /// pipeline, whose processing tiles do not line up with the TIFF's own tile
    /// grid once they carry a halo.
    ///
    /// # Errors
    ///
    /// Fails if the level is out of range, the region leaves the level, or a
    /// tile does not decode.
    #[expect(
        clippy::too_many_arguments,
        reason = "a level, a rectangle and two reusable buffers; grouping them \
                  would need a region type this crate does not otherwise have"
    )]
    pub fn read_region_into(
        &self,
        level: usize,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        out: &mut Vec<u8>,
        tile: &mut Vec<u8>,
    ) -> Result<()> {
        let info = self
            .levels
            .get(level)
            .ok_or_else(|| SlideError::NoSuchLevel {
                path: self.path.clone(),
                level,
                available: self.levels.len(),
            })?
            .clone();

        if width == 0
            || height == 0
            || x.saturating_add(width) > info.width
            || y.saturating_add(height) > info.height
        {
            return Err(SlideError::OutOfBounds {
                path: self.path.clone(),
                level,
                x,
                y,
                width,
                height,
                level_width: info.width,
                level_height: info.height,
            });
        }

        out.clear();
        out.resize(width as usize * height as usize * 3, 0);
        let region = &mut out[..];

        let first_column = x / info.tile_width;
        let last_column = (x + width - 1) / info.tile_width;
        let first_row = y / info.tile_height;
        let last_row = (y + height - 1) / info.tile_height;

        for tile_row in first_row..=last_row {
            for tile_column in first_column..=last_column {
                self.read_tile_into(level, tile_column, tile_row, tile)?;

                let tile_x = tile_column * info.tile_width;
                let tile_y = tile_row * info.tile_height;

                // Overlap of this tile with the requested region, in level
                // coordinates.
                let from_x = x.max(tile_x);
                let to_x = (x + width).min(tile_x + info.tile_width);
                let from_y = y.max(tile_y);
                let to_y = (y + height).min(tile_y + info.tile_height);

                for line in from_y..to_y {
                    let source = ((line - tile_y) as usize * info.tile_width as usize
                        + (from_x - tile_x) as usize)
                        * 3;
                    let destination =
                        ((line - y) as usize * width as usize + (from_x - x) as usize) * 3;
                    let span = (to_x - from_x) as usize * 3;
                    region[destination..destination + span]
                        .copy_from_slice(&tile[source..source + span]);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The small Aperio sample, committed alongside the full slide.
    fn small_slide() -> Option<Slide> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/CMU-1-Small-Region.svs");
        path.exists().then(|| Slide::open(&path).expect("open"))
    }

    /// The field parser, against the shape Aperio actually writes — spaces
    /// around `=`, a free-text first line with no `=` in it at all, and the key
    /// appearing as a substring of other keys.
    #[test]
    fn an_aperio_field_is_read_around_its_spaces() {
        let description = "Aperio Image Library v10.0.51\r\n46920x33014 [0,100 46000x32914] \
             (256x256) JPEG/RGB Q=30|AppMag = 20|StripeWidth = 2040|MPP = 0.4990|Left = 25.69";
        assert_eq!(Slide::aperio_field(description, "MPP"), Some("0.4990"));
        assert_eq!(Slide::aperio_field(description, "AppMag"), Some("20"));
        assert_eq!(Slide::aperio_field(description, "Missing"), None);
    }

    /// `MPP` must not be found inside `StripeWidth` or any other key that
    /// contains it, which is what a `contains` test rather than an equality test
    /// would do.
    #[test]
    fn a_field_name_matches_whole_and_not_as_a_substring() {
        assert_eq!(Slide::aperio_field("XMPP = 3|MPP = 0.5", "MPP"), Some("0.5"));
        assert_eq!(Slide::aperio_field("XMPP = 3", "MPP"), None);
    }

    /// The first line carries no `=` at all on some scanners and a `Q=30` on
    /// others, and neither may be mistaken for a field.
    #[test]
    fn a_description_with_no_fields_yields_nothing() {
        assert_eq!(Slide::aperio_field("just some text", "MPP"), None);
    }

    #[test]
    fn the_sample_slide_reports_its_pixel_size() {
        let Some(slide) = small_slide() else {
            return;
        };
        // Aperio's CMU-1 family is scanned at 0.4990 µm/px — the number
        // `qupath_pipeline::Params` used to hardcode. Asserting the value rather
        // than merely `is_some` is what makes this catch a parser that reads the
        // wrong field.
        assert_eq!(slide.microns_per_pixel(), Some(0.4990));
        assert_eq!(slide.magnification(), Some(20.0));
    }

    /// A pixel size of zero would make every micron conversion infinite, and a
    /// corrupt field is likelier than a scanner that means it.
    #[test]
    fn a_nonpositive_pixel_size_is_refused() {
        // Exercised through the parser, since building a Slide needs a file.
        for text in ["MPP = 0", "MPP = -1", "MPP = wat"] {
            let parsed = Slide::aperio_field(text, "MPP")
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|mpp| *mpp > 0.0);
            assert_eq!(parsed, None, "{text} must not yield a pixel size");
        }
    }

    #[test]
    fn opening_a_missing_file_names_it() {
        let error = Slide::open("/nonexistent/slide.svs").expect_err("must fail");
        assert!(
            error.to_string().contains("/nonexistent/slide.svs"),
            "error must name the path, got: {error}"
        );
    }

    #[test]
    fn a_non_tiff_is_rejected() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let error = Slide::open(&path).expect_err("must fail");
        assert!(matches!(error, SlideError::NotTiff { .. }), "got {error}");
    }

    #[test]
    fn levels_descend_in_size() {
        let Some(slide) = small_slide() else {
            return;
        };
        let levels = slide.levels();
        assert!(!levels.is_empty());
        for pair in levels.windows(2) {
            assert!(
                pair[0].width > pair[1].width,
                "levels must get smaller: {:?}",
                levels
            );
        }
        assert!((levels[0].downsample - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_region_outside_the_level_is_refused() {
        let Some(slide) = small_slide() else {
            return;
        };
        let level = &slide.levels()[0];
        let error = slide
            .read_region_rgb(0, level.width - 4, 0, 64, 64)
            .expect_err("must fail");
        assert!(
            matches!(error, SlideError::OutOfBounds { .. }),
            "got {error}"
        );
    }

    #[test]
    fn overlapping_regions_agree_where_they_overlap() {
        // Region assembly stitches tiles and crops; a seam bug shows up as two
        // reads of the same pixels disagreeing. This crosses a tile boundary in
        // both axes by construction.
        let Some(slide) = small_slide() else {
            return;
        };
        let level = &slide.levels()[0];
        let tile = level.tile_width.min(level.tile_height);
        assert!(level.width > tile * 2 && level.height > tile * 2);

        let origin = tile - 8;
        let wide = slide
            .read_region_rgb(0, origin, origin, 32, 32)
            .expect("wide");
        let inner = slide
            .read_region_rgb(0, origin + 8, origin + 8, 16, 16)
            .expect("inner");

        for row in 0..16usize {
            let from_wide = ((row + 8) * 32 + 8) * 3;
            let from_inner = row * 16 * 3;
            assert_eq!(
                &wide[from_wide..from_wide + 16 * 3],
                &inner[from_inner..from_inner + 16 * 3],
                "row {row} of the overlap disagrees"
            );
        }
    }
}
