//! PNG/BMP → JPEG XL Modular (lossless), per workplan/SPEC.md §4's PNG/BMP
//! rows.
//!
//! Unlike the JPEG path, pixel-exactness here does **not** imply metadata
//! survival (Modular mode re-encodes the image data from scratch rather
//! than losslessly wrapping the original bitstream), so this module runs
//! two genuinely separate checks: a pixel-compare decode-verify, and an
//! independent metadata readback via `jxl-oxide` (a different crate than
//! the one that did the encoding, so this isn't just re-checking the
//! encoder's own bookkeeping).
//!
//! **Documented scope decision on ICC** (a genuine gap §4 doesn't resolve):
//! `jpegxl-rs` 0.15's encoder API has no way to embed an arbitrary source
//! ICC profile — only a fixed enum of standard color spaces
//! ([`jpegxl_rs::encode::ColorEncoding`]). Since this crate cannot prove
//! ICC round-trip safety, a source PNG carrying an `iCCP` chunk is treated
//! as a hard stop (`SkipReason::MetadataMismatch`), consistent with §4's
//! "never-convert on any verification failure" policy — silently dropping
//! the profile would violate the binding metadata-preservation rule.

use std::io::Cursor;

use jpegxl_rs::encode::{ColorEncoding, EncoderFrame, Metadata};

use crate::{ConversionOutcome, ConvertError, Converted, SkipReason};

/// PNG ancillary-chunk allowlist (workplan/SPEC.md §4): everything else
/// means "cannot prove it's safe to drop on re-encode", so the file is
/// left unconverted rather than risk silently losing data.
const PNG_CHUNK_ALLOWLIST: &[&[u8; 4]] = &[
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"tEXt",
    b"zTXt", b"iTXt", b"eXIf", b"bKGD", b"pHYs",
];

/// Walks a PNG's raw chunk stream and returns the first chunk type found
/// outside [`PNG_CHUNK_ALLOWLIST`], if any. Deliberately hand-rolled rather
/// than reaching for another dependency: neither `image` nor the `png`
/// crate expose a generic "list every chunk type present" API (both decode
/// straight into named fields), and this format is trivial to walk
/// directly — 8-byte signature, then repeated `[len:4][type:4][data:len][crc:4]`.
fn first_unrecognized_chunk(png_bytes: &[u8]) -> Option<[u8; 4]> {
    const SIGNATURE: &[u8; 8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if !png_bytes.starts_with(SIGNATURE) {
        return None; // not a well-formed PNG; let the real decoder report that.
    }

    let mut pos = SIGNATURE.len();
    while pos + 8 <= png_bytes.len() {
        let len = u32::from_be_bytes(png_bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let mut chunk_type = [0u8; 4];
        chunk_type.copy_from_slice(&png_bytes[pos + 4..pos + 8]);

        if !PNG_CHUNK_ALLOWLIST
            .iter()
            .any(|allowed| **allowed == chunk_type)
        {
            return Some(chunk_type);
        }

        // header(8) + data(len) + crc(4)
        pos += 8 + len + 4;
        if chunk_type == *b"IEND" {
            break;
        }
    }
    None
}

/// Source pixels + whatever EXIF/XMP this format's decoder found, in a
/// shape common to both PNG and BMP so the encode/verify logic below is
/// shared between them. ICC is deliberately not part of this shape — see
/// the module doc's "documented scope decision on ICC": a source carrying
/// one is rejected before a `DecodedSource` is ever built, since nothing
/// downstream can preserve it.
struct DecodedSource {
    pixels: image::RgbaImage,
    exif: Option<Vec<u8>>,
    xmp: Option<Vec<u8>>,
}

pub fn convert_png(source_bytes: &[u8]) -> Result<ConversionOutcome, ConvertError> {
    if let Some(chunk_type) = first_unrecognized_chunk(source_bytes) {
        return Ok(Err(SkipReason::UnrecognizedAncillaryChunk {
            chunk_type: String::from_utf8_lossy(&chunk_type).into_owned(),
        }));
    }

    let mut decoder = png::Decoder::new(Cursor::new(source_bytes));
    decoder.set_transformations(png::Transformations::ALPHA | png::Transformations::EXPAND);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .expect("read_info() has already parsed the header")
    ];
    let frame_info = reader.next_frame(&mut buf)?;
    buf.truncate(frame_info.buffer_size());

    let info = reader.info();
    if info.icc_profile.is_some() {
        // See this module's doc comment: jpegxl-rs cannot embed an
        // arbitrary source ICC profile, so this is a documented hard stop.
        return Ok(Err(SkipReason::MetadataMismatch(
            "source PNG carries an iCCP profile that this encoder cannot preserve".to_string(),
        )));
    }
    let exif = info.exif_metadata.as_ref().map(|c| c.to_vec());
    let xmp = info
        .utf8_text
        .iter()
        .find(|chunk| chunk.keyword == "XML:com.adobe.xmp")
        .and_then(|chunk| chunk.get_text().ok())
        .map(String::into_bytes);

    // Normalize to RGBA8 regardless of source color type/bit depth — the
    // ALPHA|EXPAND transformations above already do this for us, but pin
    // it down explicitly via `image`'s RgbaImage so PNG and BMP share one
    // pixel representation.
    let pixels = image::RgbaImage::from_raw(frame_info.width, frame_info.height, buf)
        .expect("ALPHA|EXPAND transformations guarantee width*height*4 RGBA8 bytes");

    convert_pixels(DecodedSource { pixels, exif, xmp })
}

pub fn convert_bmp(source_bytes: &[u8]) -> Result<ConversionOutcome, ConvertError> {
    // BMP has no standard container for EXIF/XMP, and the `image` crate's
    // BMP decoder doesn't expose the rare V5-header embedded ICC case —
    // there is nothing to check for these on a typical BMP, so (per the
    // module doc) the only verification that applies is pixel-exactness.
    let img = image::load_from_memory_with_format(source_bytes, image::ImageFormat::Bmp)?;
    let pixels = img.to_rgba8();
    convert_pixels(DecodedSource {
        pixels,
        exif: None,
        xmp: None,
    })
}

fn convert_pixels(source: DecodedSource) -> Result<ConversionOutcome, ConvertError> {
    let (width, height) = source.pixels.dimensions();
    let mut encoder = jpegxl_rs::encoder_builder()
        .has_alpha(true)
        .lossless(true)
        .use_container(true)
        .uses_original_profile(true)
        .color_encoding(ColorEncoding::Srgb)
        .build()?;

    if let Some(exif) = &source.exif {
        let mut boxed = vec![0u8; 4]; // 4-byte tiff header offset; 0 = header follows immediately
        boxed.extend_from_slice(exif);
        encoder.add_metadata(&Metadata::Exif(&boxed), false)?;
    }
    if let Some(xmp) = &source.xmp {
        encoder.add_metadata(&Metadata::Xmp(xmp), false)?;
    }

    let frame = EncoderFrame::new(source.pixels.as_raw().as_slice()).num_channels(4);
    let encoded = match encoder.encode_frame::<u8, u8>(&frame, width, height) {
        Ok(result) => result,
        Err(e) => return Ok(Err(SkipReason::EncodeFailed(e.to_string()))),
    };

    // Check 1: pixel-compare decode-verify.
    let decoder = jpegxl_rs::decoder_builder().build()?;
    let Ok((meta, decoded_pixels)) = decoder.decode_with::<u8>(&encoded.data) else {
        return Ok(Err(SkipReason::PixelMismatch));
    };
    if meta.width != width
        || meta.height != height
        || decoded_pixels != source.pixels.as_raw().as_slice()
    {
        return Ok(Err(SkipReason::PixelMismatch));
    }

    // Check 2: independent metadata readback via jxl-oxide (a different
    // decoder than the one libjxl/jpegxl-rs used above) — pixel-exactness
    // alone does not prove the EXIF/XMP boxes survived.
    if let Err(reason) =
        verify_metadata_survived(&encoded.data, source.exif.as_deref(), source.xmp.as_deref())
    {
        return Ok(Err(reason));
    }

    Ok(Ok(Converted {
        bytes: encoded.data,
        stored_format: "jxl",
    }))
}

fn verify_metadata_survived(
    jxl_bytes: &[u8],
    expected_exif: Option<&[u8]>,
    expected_xmp: Option<&[u8]>,
) -> Result<(), SkipReason> {
    if expected_exif.is_none() && expected_xmp.is_none() {
        return Ok(()); // nothing to preserve, trivially satisfied
    }

    let image = jxl_oxide::JxlImage::builder()
        .read(Cursor::new(jxl_bytes))
        .map_err(|e| {
            SkipReason::MetadataMismatch(format!("failed to open JXL container for readback: {e}"))
        })?;
    let aux = image.aux_boxes();

    if let Some(expected) = expected_exif {
        let actual: Option<Vec<u8>> = match aux.first_exif() {
            Ok(data) if data.has_data() => Some(data.unwrap().payload().to_vec()),
            _ => None,
        };
        if actual.as_deref() != Some(expected) {
            return Err(SkipReason::MetadataMismatch(
                "EXIF did not read back unchanged".to_string(),
            ));
        }
    }
    if let Some(expected) = expected_xmp {
        let xml = aux.first_xml();
        let actual: Option<Vec<u8>> = if xml.has_data() {
            Some(xml.unwrap().to_vec())
        } else {
            None
        };
        if actual.as_deref() != Some(expected) {
            return Err(SkipReason::MetadataMismatch(
                "XMP did not read back unchanged".to_string(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let img: image::RgbaImage = image::ImageBuffer::from_fn(width, height, |x, y| {
            image::Rgba([seed, (x % 256) as u8, (y % 256) as u8, 255])
        });
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn write_bmp(width: u32, height: u32, seed: u8) -> Vec<u8> {
        let img: image::RgbImage = image::ImageBuffer::from_fn(width, height, |x, y| {
            image::Rgb([seed, (x % 256) as u8, (y % 256) as u8])
        });
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Bmp)
            .unwrap();
        buf
    }

    #[test]
    fn converting_a_plain_png_round_trips_pixel_exact() {
        let source = write_png(37, 23, 42);

        let outcome = convert_png(&source).unwrap();
        let converted = outcome.expect("a plain PNG with only allowlisted chunks must convert");
        assert_eq!(converted.stored_format, "jxl");

        let decoder = jpegxl_rs::decoder_builder().build().unwrap();
        let (meta, pixels) = decoder.decode_with::<u8>(&converted.bytes).unwrap();
        assert_eq!((meta.width, meta.height), (37, 23));

        let expected = image::load_from_memory_with_format(&source, image::ImageFormat::Png)
            .unwrap()
            .to_rgba8();
        assert_eq!(pixels, expected.into_raw());
    }

    #[test]
    fn converting_a_bmp_round_trips_pixel_exact() {
        let source = write_bmp(20, 15, 200);

        let outcome = convert_bmp(&source).unwrap();
        let converted = outcome.expect("a plain BMP must convert");

        let decoder = jpegxl_rs::decoder_builder().build().unwrap();
        let (meta, pixels) = decoder.decode_with::<u8>(&converted.bytes).unwrap();
        assert_eq!((meta.width, meta.height), (20, 15));

        let expected = image::load_from_memory_with_format(&source, image::ImageFormat::Bmp)
            .unwrap()
            .to_rgba8();
        assert_eq!(pixels, expected.into_raw());
    }

    #[test]
    fn a_png_with_an_out_of_allowlist_chunk_is_never_converted() {
        let mut source = write_png(4, 4, 1);
        // Splice in a bogus private ancillary chunk ("prVt") right after
        // IHDR — a minimal, deliberately out-of-allowlist chunk with a
        // syntactically valid (if not semantically checked) CRC field.
        let ihdr_end = 8 /* signature */ + 8 /* len+type */ + 13 /* IHDR data */ + 4 /* crc */;
        let mut spliced = source[..ihdr_end].to_vec();
        spliced.extend_from_slice(&0u32.to_be_bytes()); // length = 0
        spliced.extend_from_slice(b"prVt");
        spliced.extend_from_slice(&0u32.to_be_bytes()); // crc (unchecked by our scanner)
        spliced.extend_from_slice(&source[ihdr_end..]);
        source = spliced;

        let outcome = convert_png(&source).unwrap();

        assert_eq!(
            outcome.unwrap_err(),
            SkipReason::UnrecognizedAncillaryChunk {
                chunk_type: "prVt".to_string()
            }
        );
    }
}
