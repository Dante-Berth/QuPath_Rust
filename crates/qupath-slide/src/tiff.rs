//! The slice of TIFF needed to locate tiles in a pyramidal slide.
//!
//! §9 of the project guidelines warns against writing a TIFF parser on the
//! assumption that pure Rust will cover whole-slide formats. That warning is
//! about *pixel* decoding — Aperio's JPEG 2000 variants have no mature pure
//! Rust decoder, and this crate refuses them by name rather than pretending
//! (see [`Compression`]). Locating tiles is a different problem: an IFD is a
//! flat table of 12-byte entries, this module reads eight tags out of it, and
//! everything it cannot account for becomes an error rather than a guess.
//!
//! What it deliberately does not do: strips (only tiles), BigTIFF, planar
//! configuration 2, or bit depths other than 8. Each is rejected explicitly.

use std::fs::File;
use std::path::Path;

use crate::error::{Result, SlideError};
use crate::io::read_exact_at;

/// TIFF tags this reader understands. Everything else is skipped.
mod tag {
    pub const IMAGE_WIDTH: u16 = 256;
    pub const IMAGE_LENGTH: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC: u16 = 262;
    pub const IMAGE_DESCRIPTION: u16 = 270;
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const PLANAR_CONFIG: u16 = 284;
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_LENGTH: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_BYTE_COUNTS: u16 = 325;
    pub const JPEG_TABLES: u16 = 347;
}

/// How a directory's tiles are compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Baseline JPEG, TIFF compression 7. Aperio writes abbreviated streams
    /// that share one table set through `tag::JPEG_TABLES`.
    Jpeg,
    /// TIFF compression 5, used by Aperio for the label image only.
    Lzw,
    /// Aperio JPEG 2000, YCbCr (33003) or RGB (33005). Not decodable here.
    Jpeg2000,
    /// Anything else, kept so the error can name the number.
    Other(u16),
}

impl Compression {
    fn from_tag(value: u16) -> Self {
        match value {
            5 => Self::Lzw,
            7 => Self::Jpeg,
            33003 | 33005 => Self::Jpeg2000,
            other => Self::Other(other),
        }
    }
}

impl std::fmt::Display for Compression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Jpeg => write!(f, "JPEG"),
            Self::Lzw => write!(f, "LZW"),
            Self::Jpeg2000 => write!(f, "Aperio JPEG 2000"),
            Self::Other(code) => write!(f, "TIFF compression {code}"),
        }
    }
}

/// One image file directory: a pyramid level, or an associated image.
#[derive(Debug, Clone)]
pub struct Directory {
    /// Full width in pixels.
    pub width: u32,
    /// Full height in pixels.
    pub height: u32,
    /// Tile width, or `None` for a stripped image such as the label or macro.
    pub tile_width: Option<u32>,
    /// Tile height, or `None` for a stripped image.
    pub tile_height: Option<u32>,
    /// How the tiles are compressed.
    pub compression: Compression,
    /// Samples per pixel; 3 for the RGB levels this crate reads.
    pub samples: u16,
    /// TIFF PhotometricInterpretation: 2 is RGB, 6 is YCbCr.
    ///
    /// Aperio writes 2 and stores JPEG components that are *already* RGB, so a
    /// decoder that assumes JFIF and applies the YCbCr conversion produces
    /// plausible-looking, badly wrong colour. This tag is what decides.
    pub photometric: u16,
    /// File offset of each tile, in row-major tile order.
    pub tile_offsets: Vec<u64>,
    /// Compressed byte count of each tile, parallel to `tile_offsets`.
    pub tile_byte_counts: Vec<u64>,
    /// The shared abbreviated-JPEG table stream, when present.
    pub jpeg_tables: Option<Vec<u8>>,
    /// Aperio packs the level geometry and magnification in here.
    pub description: String,
}

impl Directory {
    /// Whether this directory is a tiled pyramid level rather than an
    /// associated image (label, macro, thumbnail).
    pub fn is_tiled_level(&self) -> bool {
        self.tile_width.is_some() && self.tile_height.is_some() && !self.tile_offsets.is_empty()
    }

    /// Number of tile columns.
    pub fn tile_columns(&self) -> u32 {
        self.tile_width.map_or(0, |tw| self.width.div_ceil(tw))
    }

    /// Number of tile rows.
    pub fn tile_rows(&self) -> u32 {
        self.tile_height.map_or(0, |th| self.height.div_ceil(th))
    }
}

/// Byte order of the file, applied to every multi-byte read.
#[derive(Debug, Clone, Copy)]
struct ByteOrder(bool);

impl ByteOrder {
    fn u16(self, bytes: [u8; 2]) -> u16 {
        if self.0 {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    }

    fn u32(self, bytes: [u8; 4]) -> u32 {
        if self.0 {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    }
}

/// Reads every directory in the file, in order.
pub fn read_directories(file: &File, path: &Path) -> Result<Vec<Directory>> {
    let mut header = [0u8; 8];
    read_exact_at(file, path, &mut header, 0)?;

    let order = match &header[..2] {
        b"II" => ByteOrder(true),
        b"MM" => ByteOrder(false),
        _ => {
            return Err(SlideError::NotTiff {
                path: path.to_path_buf(),
                detail: format!("byte-order mark {:?} is neither II nor MM", &header[..2]),
            });
        }
    };

    let magic = order.u16([header[2], header[3]]);
    if magic == 43 {
        return Err(SlideError::Unsupported {
            path: path.to_path_buf(),
            detail: "BigTIFF; only classic TIFF is implemented".to_owned(),
        });
    }
    if magic != 42 {
        return Err(SlideError::NotTiff {
            path: path.to_path_buf(),
            detail: format!("magic {magic} is not 42"),
        });
    }

    let mut next = u64::from(order.u32([header[4], header[5], header[6], header[7]]));
    let mut directories = Vec::new();
    // A malformed chain could loop; the count is bounded well above the ~8
    // directories a real slide carries.
    while next != 0 && directories.len() < 64 {
        let (directory, following) = read_directory(file, path, order, next)?;
        directories.push(directory);
        next = following;
    }

    if directories.is_empty() {
        return Err(SlideError::NotTiff {
            path: path.to_path_buf(),
            detail: "no image file directories".to_owned(),
        });
    }
    Ok(directories)
}

/// Reads one directory, returning it and the offset of the next.
fn read_directory(
    file: &File,
    path: &Path,
    order: ByteOrder,
    offset: u64,
) -> Result<(Directory, u64)> {
    let mut count_bytes = [0u8; 2];
    read_exact_at(file, path, &mut count_bytes, offset)?;
    let count = usize::from(order.u16(count_bytes));

    let mut entries = vec![0u8; count * 12 + 4];
    read_exact_at(file, path, &mut entries, offset + 2)?;

    let mut directory = Directory {
        width: 0,
        height: 0,
        tile_width: None,
        tile_height: None,
        compression: Compression::Other(1),
        samples: 1,
        photometric: 2,
        tile_offsets: Vec::new(),
        tile_byte_counts: Vec::new(),
        jpeg_tables: None,
        description: String::new(),
    };

    for entry in entries[..count * 12].chunks_exact(12) {
        let tag = order.u16([entry[0], entry[1]]);
        let field_type = order.u16([entry[2], entry[3]]);
        let values = u64::from(order.u32([entry[4], entry[5], entry[6], entry[7]]));
        let inline: [u8; 4] = [entry[8], entry[9], entry[10], entry[11]];

        match tag {
            tag::IMAGE_WIDTH => directory.width = scalar(order, field_type, inline)?,
            tag::IMAGE_LENGTH => directory.height = scalar(order, field_type, inline)?,
            tag::TILE_WIDTH => directory.tile_width = Some(scalar(order, field_type, inline)?),
            tag::TILE_LENGTH => directory.tile_height = Some(scalar(order, field_type, inline)?),
            tag::COMPRESSION => {
                let raw = scalar(order, field_type, inline)?;
                directory.compression =
                    Compression::from_tag(u16::try_from(raw).unwrap_or(u16::MAX));
            }
            tag::PHOTOMETRIC => {
                directory.photometric =
                    u16::try_from(scalar(order, field_type, inline)?).unwrap_or(u16::MAX);
            }
            tag::SAMPLES_PER_PIXEL => {
                directory.samples =
                    u16::try_from(scalar(order, field_type, inline)?).unwrap_or(u16::MAX);
            }
            tag::PLANAR_CONFIG => {
                let planar = scalar(order, field_type, inline)?;
                if planar != 1 {
                    return Err(SlideError::Unsupported {
                        path: path.to_path_buf(),
                        detail: format!("planar configuration {planar}; only chunky is read"),
                    });
                }
            }
            tag::BITS_PER_SAMPLE => {
                let bits = read_integers(file, path, order, field_type, values, inline)?;
                if bits.iter().any(|&b| b != 8) {
                    return Err(SlideError::Unsupported {
                        path: path.to_path_buf(),
                        detail: format!("bits per sample {bits:?}; only 8 is read"),
                    });
                }
            }
            tag::TILE_OFFSETS => {
                directory.tile_offsets =
                    read_integers(file, path, order, field_type, values, inline)?;
            }
            tag::TILE_BYTE_COUNTS => {
                directory.tile_byte_counts =
                    read_integers(file, path, order, field_type, values, inline)?;
            }
            tag::JPEG_TABLES => {
                directory.jpeg_tables = Some(read_bytes(file, path, order, values, inline)?);
            }
            tag::IMAGE_DESCRIPTION => {
                let raw = read_bytes(file, path, order, values, inline)?;
                let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                directory.description = String::from_utf8_lossy(&raw[..end]).into_owned();
            }
            _ => {}
        }
    }

    let tail = &entries[count * 12..];
    let next = u64::from(order.u32([tail[0], tail[1], tail[2], tail[3]]));

    if directory.tile_offsets.len() != directory.tile_byte_counts.len() {
        return Err(SlideError::Malformed {
            path: path.to_path_buf(),
            detail: format!(
                "{} tile offsets but {} byte counts",
                directory.tile_offsets.len(),
                directory.tile_byte_counts.len()
            ),
        });
    }

    Ok((directory, next))
}

/// Reads a tag whose value is a single integer stored inline.
fn scalar(order: ByteOrder, field_type: u16, inline: [u8; 4]) -> Result<u32> {
    Ok(match field_type {
        3 => u32::from(order.u16([inline[0], inline[1]])),
        4 => order.u32(inline),
        1 => u32::from(inline[0]),
        _ => order.u32(inline),
    })
}

/// Reads an integer array tag, inline when it fits in four bytes and from the
/// pointed-at offset otherwise.
fn read_integers(
    file: &File,
    path: &Path,
    order: ByteOrder,
    field_type: u16,
    count: u64,
    inline: [u8; 4],
) -> Result<Vec<u64>> {
    let width = match field_type {
        1 => 1usize,
        3 => 2,
        4 => 4,
        other => {
            return Err(SlideError::Malformed {
                path: path.to_path_buf(),
                detail: format!("unexpected field type {other} for an integer array"),
            });
        }
    };

    let count = usize::try_from(count).unwrap_or(usize::MAX);
    let total = width * count;
    let raw = if total <= 4 {
        inline[..total].to_vec()
    } else {
        let mut buffer = vec![0u8; total];
        read_exact_at(file, path, &mut buffer, u64::from(order.u32(inline)))?;
        buffer
    };

    Ok(raw
        .chunks_exact(width)
        .map(|chunk| match width {
            1 => u64::from(chunk[0]),
            2 => u64::from(order.u16([chunk[0], chunk[1]])),
            _ => u64::from(order.u32([chunk[0], chunk[1], chunk[2], chunk[3]])),
        })
        .collect())
}

/// Reads a byte-valued tag (ASCII or UNDEFINED).
fn read_bytes(
    file: &File,
    path: &Path,
    order: ByteOrder,
    count: u64,
    inline: [u8; 4],
) -> Result<Vec<u8>> {
    let count = usize::try_from(count).unwrap_or(usize::MAX);
    if count <= 4 {
        return Ok(inline[..count].to_vec());
    }
    let mut buffer = vec![0u8; count];
    read_exact_at(file, path, &mut buffer, u64::from(order.u32(inline)))?;
    Ok(buffer)
}
