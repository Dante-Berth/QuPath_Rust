//! Errors from reading a slide.
//!
//! §8 of the project guidelines: image I/O fails in ordinary, expected ways —
//! truncated files, unsupported compression, missing pyramid levels — so these
//! are `Result`, and they carry the path and, where it applies, the level.

use std::path::PathBuf;

/// The result type every fallible operation in this crate returns.
pub type Result<T> = std::result::Result<T, SlideError>;

/// Why a slide could not be read.
#[derive(Debug, thiserror::Error)]
pub enum SlideError {
    /// The file could not be read at all.
    #[error("{path}: {source}")]
    Io {
        /// File being read.
        path: PathBuf,
        /// Underlying cause.
        source: std::io::Error,
    },

    /// The file is not a TIFF, so it is not a slide this crate can open.
    #[error("{path}: not a TIFF ({detail})")]
    NotTiff {
        /// File being read.
        path: PathBuf,
        /// What specifically was wrong.
        detail: String,
    },

    /// The file is a TIFF, but uses a feature this reader does not implement.
    #[error("{path}: unsupported ({detail})")]
    Unsupported {
        /// File being read.
        path: PathBuf,
        /// The feature that is not implemented.
        detail: String,
    },

    /// Pixels are in a codec with no pure-Rust decoder.
    ///
    /// This is the case §9 of the guidelines calls out: Aperio JPEG 2000 needs
    /// OpenSlide or OpenJPEG through FFI, and pretending otherwise would mean
    /// silently returning wrong pixels.
    #[error(
        "{path}: level {level} is {compression}, which has no pure-Rust decoder; \
         reading it needs OpenSlide or OpenJPEG through FFI (see §9)"
    )]
    UndecodableCodec {
        /// File being read.
        path: PathBuf,
        /// Pyramid level index.
        level: usize,
        /// The codec found.
        compression: String,
    },

    /// The file's structure contradicts itself.
    #[error("{path}: malformed ({detail})")]
    Malformed {
        /// File being read.
        path: PathBuf,
        /// The inconsistency found.
        detail: String,
    },

    /// A pyramid level was asked for that the slide does not have.
    #[error("{path}: level {level} requested, slide has {available}")]
    NoSuchLevel {
        /// File being read.
        path: PathBuf,
        /// Level index requested.
        level: usize,
        /// Number of levels the slide actually has.
        available: usize,
    },

    /// A region was asked for that falls outside the level.
    #[error(
        "{path}: region {x},{y} {width}x{height} is outside level {level} ({level_width}x{level_height})"
    )]
    OutOfBounds {
        /// File being read.
        path: PathBuf,
        /// Pyramid level index.
        level: usize,
        /// Requested origin.
        x: u32,
        /// Requested origin.
        y: u32,
        /// Requested size.
        width: u32,
        /// Requested size.
        height: u32,
        /// The level's width.
        level_width: u32,
        /// The level's height.
        level_height: u32,
    },

    /// A tile's compressed bytes could not be decoded.
    #[error("{path}: level {level} tile ({column},{row}): {detail}")]
    TileDecode {
        /// File being read.
        path: PathBuf,
        /// Pyramid level index.
        level: usize,
        /// Tile column.
        column: u32,
        /// Tile row.
        row: u32,
        /// What the decoder reported.
        detail: String,
    },
}
