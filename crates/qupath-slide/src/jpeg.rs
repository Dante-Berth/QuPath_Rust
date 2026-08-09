//! Decoding Aperio's abbreviated JPEG tiles.
//!
//! A tiled TIFF with JPEG compression does not repeat the Huffman and
//! quantisation tables in every tile. They live once in the directory, under
//! tag 347, and each tile carries only the frame and scan. Neither half is a
//! valid JPEG on its own: the tables end with EOI and the tile starts with SOI.
//! Splicing them — tables without their EOI, then the tile without its SOI —
//! gives the complete stream a decoder expects.

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

/// Start of image, and end of image.
const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: [u8; 2] = [0xFF, 0xD9];

/// Decodes one tile into `out` as RGB8, reusing its allocation.
///
/// `width` and `height` are the tile's declared size; a stream that decodes to
/// anything else is an error rather than something to pad or crop, because it
/// means the tile index and the pixel data disagree.
pub fn decode_into(
    compressed: &[u8],
    tables: Option<&[u8]>,
    already_rgb: bool,
    width: u32,
    height: u32,
    out: &mut Vec<u8>,
) -> std::result::Result<(), String> {
    let mut stream;
    let complete = match tables {
        // Already a full stream: some writers repeat the tables per tile.
        Some(tables) if tables.len() > 4 && compressed.starts_with(&SOI) => {
            let tables_body = tables.strip_suffix(&EOI).unwrap_or(tables);
            stream = Vec::with_capacity(tables_body.len() + compressed.len());
            stream.extend_from_slice(tables_body);
            stream.extend_from_slice(&compressed[SOI.len()..]);
            &stream[..]
        }
        _ => compressed,
    };

    // Aperio declares PhotometricInterpretation = RGB and stores JPEG
    // components that are already R, G and B, but writes no Adobe APP14 marker
    // saying so. A decoder following JFIF convention assumes three components
    // means YCbCr and converts, which turns a white pixel of (243,243,243) into
    // (255,121,255): R and B saturate and G collapses. The result still looks
    // like a slide, which is why this is checked against QuPath's own decode
    // rather than by eye — see `tests/parity_slide_region.rs`.
    //
    // Asking for YCbCr *output* from a stream whose components are already RGB
    // is how the conversion is skipped: it is the decoder's identity path.
    let output = if already_rgb {
        ColorSpace::YCbCr
    } else {
        ColorSpace::RGB
    };
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(output)
        .set_strict_mode(false);
    let mut decoder = JpegDecoder::new_with_options(complete, options);

    let pixels = decoder
        .decode()
        .map_err(|error| format!("jpeg decode failed: {error}"))?;

    let expected = width as usize * height as usize * 3;
    if pixels.len() != expected {
        let (w, h) = decoder.dimensions().unwrap_or((0, 0));
        return Err(format!(
            "tile declared {width}x{height} but decoded {w}x{h} ({} bytes, expected {expected})",
            pixels.len()
        ));
    }

    out.clear();
    out.extend_from_slice(&pixels);
    Ok(())
}
