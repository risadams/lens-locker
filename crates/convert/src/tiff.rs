//! TIFF → TIFF, recompressed in place, per workplan/SPEC.md §4's TIFF row:
//! "Native container, tag directory untouched."
//!
//! Unlike the JPEG XL paths, the target format doesn't change — only the
//! pixel-data compression does (Deflate here; §4 allows LZW or Deflate).
//! "Tag directory untouched" is interpreted here as: every tag that isn't
//! part of the pixel-storage layout itself (dimensions, compression,
//! photometric interpretation, strip/tile geometry, sample format — all of
//! which necessarily change because the new file's storage layout is
//! genuinely different) is copied through unchanged, including the EXIF
//! sub-IFD (workplan/SPEC.md: "EXIF/XMP/ICC, which TIFF stores as
//! first-class tags"). XMP (tag 700) and ICC (tag 34675) are ordinary
//! top-level tags, so the generic tag copy below preserves them with no
//! special-casing needed — only the EXIF sub-IFD, which is a *pointer* to a
//! nested directory, needs its own recursive handling.
//!
//! **Documented scope limitation**: only 8-bit grayscale and 8-bit RGB
//! source images are supported this milestone (`tiff::encoder::colortype`
//! requires a compile-time color type per image, so this dispatches on the
//! two cases §4's exit criteria actually needs). GPS/Interop/generic
//! sub-IFDs (tags other than the EXIF IFD) are not recursively copied —
//! a real gap, flagged rather than silently accepted, since GPS data would
//! be lost. Verification below only compares the EXIF sub-IFD and the
//! top-level directory, consistent with what's actually copied.

use std::io::{Cursor, Read, Seek};

use tiff::Directory;
use tiff::decoder::ifd::{Entry, Value};
use tiff::decoder::{Decoder, DecodingResult};
use tiff::encoder::{
    Compression, DeflateLevel, DirectoryEncoder, TiffEncoder, TiffKindStandard, colortype,
};
use tiff::tags::{IfdPointer, Tag, Type};

use crate::{ConversionOutcome, ConvertError, Converted, SkipReason};

/// Tags that describe the pixel-storage layout itself — necessarily
/// different in the recompressed output, so these must come from the
/// encoder's own bookkeeping rather than being copied from the source.
fn is_structural_tag(id: u16) -> bool {
    matches!(
        id,
        254 // NewSubfileType
        | 256 // ImageWidth
        | 257 // ImageLength
        | 258 // BitsPerSample
        | 259 // Compression
        | 262 // PhotometricInterpretation
        | 266 // FillOrder
        | 273 // StripOffsets
        | 277 // SamplesPerPixel
        | 278 // RowsPerStrip
        | 279 // StripByteCounts
        | 284 // PlanarConfiguration
        | 317 // Predictor
        | 322 | 323 | 324 | 325 // Tile{Width,Length,Offsets,ByteCounts}
        | 338 // ExtraSamples
        | 339 // SampleFormat
    )
}

const EXIF_DIRECTORY_TAG: u16 = 0x8769;

/// Converts a decoder [`Value`] to its on-disk `(Type, bytes)` form in the
/// host's native byte order — matching `tiff::encoder`'s own convention
/// (its `TiffValue::write` implementations are documented as "all integers
/// are taken as native endian and thus transparent").
fn value_bytes(value: &Value) -> Option<(Type, Vec<u8>)> {
    match value {
        Value::Byte(v) => Some((Type::BYTE, vec![*v])),
        Value::SignedByte(v) => Some((Type::SBYTE, vec![*v as u8])),
        Value::Short(v) => Some((Type::SHORT, v.to_ne_bytes().to_vec())),
        Value::SignedShort(v) => Some((Type::SSHORT, v.to_ne_bytes().to_vec())),
        Value::Unsigned(v) => Some((Type::LONG, v.to_ne_bytes().to_vec())),
        Value::Signed(v) => Some((Type::SLONG, v.to_ne_bytes().to_vec())),
        Value::UnsignedBig(v) => Some((Type::LONG8, v.to_ne_bytes().to_vec())),
        Value::SignedBig(v) => Some((Type::SLONG8, v.to_ne_bytes().to_vec())),
        Value::Float(v) => Some((Type::FLOAT, v.to_ne_bytes().to_vec())),
        Value::Double(v) => Some((Type::DOUBLE, v.to_ne_bytes().to_vec())),
        Value::Rational(n, d) => {
            let mut bytes = n.to_ne_bytes().to_vec();
            bytes.extend_from_slice(&d.to_ne_bytes());
            Some((Type::RATIONAL, bytes))
        }
        Value::SRational(n, d) => {
            let mut bytes = n.to_ne_bytes().to_vec();
            bytes.extend_from_slice(&d.to_ne_bytes());
            Some((Type::SRATIONAL, bytes))
        }
        Value::Ascii(s) => {
            let mut bytes = s.as_bytes().to_vec();
            bytes.push(0);
            Some((Type::ASCII, bytes))
        }
        Value::List(items) => {
            let mut out = Vec::new();
            let mut ty = None;
            for item in items {
                let (item_ty, item_bytes) = value_bytes(item)?;
                ty.get_or_insert(item_ty);
                out.extend(item_bytes);
            }
            ty.map(|ty| (ty, out))
        }
        // Pointer-typed and deprecated variants: not representable as a
        // plain value copy (a raw offset copied verbatim would point to the
        // wrong place in the new file's layout), and any future
        // non-exhaustive addition to this enum — treated the same way.
        _ => None,
    }
}

/// Writes every `(tag, value)` pair into `enc` as a plain value copy,
/// skipping anything [`value_bytes`] can't represent generically, and
/// skipping `skip` (already handled specially by the caller, e.g. the EXIF
/// pointer tag).
fn copy_tags<W: std::io::Write + Seek>(
    enc: &mut DirectoryEncoder<'_, W, TiffKindStandard>,
    tags: &[(Tag, Value)],
    skip: impl Fn(u16) -> bool,
) -> tiff::TiffResult<()> {
    for (tag, value) in tags {
        if skip(tag.to_u16()) {
            continue;
        }
        let Some((ty, bytes)) = value_bytes(value) else {
            continue;
        };
        let entry: Entry = enc.write_entry_bytes(ty, &bytes)?;
        let mut tmp = Directory::empty();
        tmp.extend([(*tag, entry)]);
        enc.extend_from(&tmp);
    }
    Ok(())
}

/// Decodes every tag in `ifd` to `(Tag, Value)` pairs, dropping anything
/// that describes pixel-storage layout — the part of the directory that's
/// legitimately different in a recompressed file (see the module doc) —
/// and the EXIF pointer tag itself, whose raw offset value is expected to
/// differ between the source and output files even when its *contents*
/// (verified separately) are identical.
fn comparable_tags<R: Read + Seek>(
    decoder: &mut Decoder<R>,
    ifd: &Directory,
) -> tiff::TiffResult<Vec<(Tag, Value)>> {
    let mut tags: Vec<(Tag, Value)> = decoder
        .read_directory_tags(ifd)
        .tag_iter()
        .filter(|r| !matches!(r, Ok((tag, _)) if is_structural_tag(tag.to_u16()) || tag.to_u16() == EXIF_DIRECTORY_TAG))
        .collect::<tiff::TiffResult<_>>()?;
    tags.sort_by_key(|(tag, _)| tag.to_u16());
    Ok(tags)
}

/// Re-reads the current image's own IFD as a standalone [`Directory`] (the
/// decoder only exposes the private, borrowed-for-decoding form directly,
/// so this goes through the public `ifd_pointer`/`read_directory` pair
/// instead).
fn current_directory<R: Read + Seek>(decoder: &mut Decoder<R>) -> tiff::TiffResult<Directory> {
    let ptr = decoder.ifd_pointer().ok_or(tiff::TiffError::FormatError(
        tiff::TiffFormatError::ImageFileDirectoryNotFound,
    ))?;
    decoder.read_directory(ptr)
}

fn exif_directory<R: Read + Seek>(decoder: &mut Decoder<R>) -> tiff::TiffResult<Option<Directory>> {
    let Some(value) = decoder.find_tag(Tag::ExifDirectory)? else {
        return Ok(None);
    };
    // A pointer tag's on-disk `Type` is conventionally LONG (decodes as
    // `Value::Unsigned`) for TiffKindStandard files — including ones this
    // crate's own encoder produces, see `TiffKindStandard::OffsetType = u32`
    // — not the dedicated `Value::Ifd` variant, which only appears when a
    // writer explicitly chose the IFD field type. Accept both.
    let offset = match value {
        Value::Ifd(v) | Value::Unsigned(v) => u64::from(v),
        Value::IfdBig(v) | Value::UnsignedBig(v) => v,
        _ => return Ok(None),
    };
    Ok(Some(decoder.read_directory(IfdPointer(offset))?))
}

pub fn convert(source_bytes: &[u8]) -> Result<ConversionOutcome, ConvertError> {
    let mut decoder = Decoder::new(Cursor::new(source_bytes))?;
    let (width, height) = decoder.dimensions()?;
    let colortype = decoder.colortype()?;

    let pixels = match decoder.read_image()? {
        DecodingResult::U8(bytes) => bytes,
        _ => {
            return Ok(Err(SkipReason::EncodeFailed(
                "only 8-bit-per-sample TIFF images are supported this milestone".to_string(),
            )));
        }
    };

    let source_ifd0_dir = current_directory(&mut decoder)?;
    let source_ifd0_tags: Vec<(Tag, Value)> = decoder
        .read_directory_tags(&source_ifd0_dir)
        .tag_iter()
        .collect::<tiff::TiffResult<_>>()?;
    let source_exif_dir = exif_directory(&mut decoder)?;
    let source_exif_tags = source_exif_dir
        .as_ref()
        .map(|dir| {
            decoder
                .read_directory_tags(dir)
                .tag_iter()
                .collect::<tiff::TiffResult<Vec<_>>>()
        })
        .transpose()?;

    let mut out = Cursor::new(Vec::new());
    let build_result = match colortype {
        tiff::ColorType::RGB(8) => build::<colortype::RGB8>(
            &mut out,
            width,
            height,
            &pixels,
            &source_ifd0_tags,
            source_exif_tags.as_deref(),
        ),
        tiff::ColorType::Gray(8) => build::<colortype::Gray8>(
            &mut out,
            width,
            height,
            &pixels,
            &source_ifd0_tags,
            source_exif_tags.as_deref(),
        ),
        other => {
            return Ok(Err(SkipReason::EncodeFailed(format!(
                "unsupported TIFF color type for this milestone: {other:?}"
            ))));
        }
    };
    if let Err(e) = build_result {
        return Ok(Err(SkipReason::EncodeFailed(e.to_string())));
    }
    let converted_bytes = out.into_inner();

    // Verify 1: pixel data round-trips exactly.
    let mut check_decoder = Decoder::new(Cursor::new(&converted_bytes))?;
    let (out_width, out_height) = check_decoder.dimensions()?;
    let out_pixels = match check_decoder.read_image()? {
        DecodingResult::U8(bytes) => bytes,
        _ => return Ok(Err(SkipReason::PixelMismatch)),
    };
    if (out_width, out_height) != (width, height) || out_pixels != pixels {
        return Ok(Err(SkipReason::PixelMismatch));
    }

    // Verify 2: tag directory (including EXIF sub-IFD) reads back
    // identically — "reading tags back after the round-trip", not assumed.
    let expected_ifd0 = comparable_tags(&mut decoder, &source_ifd0_dir)?;
    let actual_ifd0_dir = current_directory(&mut check_decoder)?;
    let actual_ifd0 = comparable_tags(&mut check_decoder, &actual_ifd0_dir)?;
    if expected_ifd0 != actual_ifd0 {
        return Ok(Err(SkipReason::TagDirectoryMismatch(
            "top-level tag directory did not read back unchanged".to_string(),
        )));
    }

    if let Some(source_exif_dir) = &source_exif_dir {
        let expected_exif = comparable_tags(&mut decoder, source_exif_dir)?;
        let Some(actual_exif_dir) = exif_directory(&mut check_decoder)? else {
            return Ok(Err(SkipReason::TagDirectoryMismatch(
                "EXIF sub-IFD was not present in the recompressed output".to_string(),
            )));
        };
        let actual_exif = comparable_tags(&mut check_decoder, &actual_exif_dir)?;
        if expected_exif != actual_exif {
            return Ok(Err(SkipReason::TagDirectoryMismatch(
                "EXIF sub-IFD did not read back unchanged".to_string(),
            )));
        }
    }

    Ok(Ok(Converted {
        bytes: converted_bytes,
        stored_format: "tiff",
    }))
}

fn build<C: colortype::ColorType<Inner = u8>>(
    out: &mut Cursor<Vec<u8>>,
    width: u32,
    height: u32,
    pixels: &[u8],
    source_ifd0_tags: &[(Tag, Value)],
    source_exif_tags: Option<&[(Tag, Value)]>,
) -> tiff::TiffResult<()>
where
    [u8]: tiff::encoder::TiffValue,
{
    let mut enc =
        TiffEncoder::new(out)?.with_compression(Compression::Deflate(DeflateLevel::default()));

    let exif_offset = if let Some(exif_tags) = source_exif_tags {
        let mut extra = enc.extra_directory()?;
        copy_tags(&mut extra, exif_tags, |_| false)?;
        Some(extra.finish_with_offsets()?)
    } else {
        None
    };

    let mut image = enc.new_image::<C>(width, height)?;
    if let Some(offset) = exif_offset {
        image
            .encoder()
            .write_tag(Tag::ExifDirectory, offset.offset)?;
    }
    copy_tags(image.encoder(), source_ifd0_tags, |id| {
        is_structural_tag(id) || id == EXIF_DIRECTORY_TAG
    })?;
    image.write_data(pixels)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but real RGB8 TIFF, LZW-compressed, carrying an ASCII
    /// `Make` tag and an EXIF sub-IFD with an `ExposureTime` rational — the
    /// two shapes of tag (plain, and pointer-to-nested-directory) this
    /// module has to round-trip.
    fn write_rgb8_tiff_with_exif(width: u32, height: u32) -> Vec<u8> {
        let pixels: Vec<u8> = (0..width * height * 3).map(|i| (i % 256) as u8).collect();
        let mut buf = Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut buf)
                .unwrap()
                .with_compression(Compression::Lzw);

            let exif = {
                let mut extra = enc.extra_directory().unwrap();
                extra
                    .write_tag(
                        Tag::Unknown(0x829A), /* ExposureTime */
                        tiff::encoder::Rational { n: 1, d: 125 },
                    )
                    .unwrap();
                extra.finish_with_offsets().unwrap()
            };

            let mut image = enc.new_image::<colortype::RGB8>(width, height).unwrap();
            image
                .encoder()
                .write_tag(Tag::ExifDirectory, exif.offset)
                .unwrap();
            image
                .encoder()
                .write_tag(Tag::Make, "LensLocker Test Co")
                .unwrap();
            image.write_data(&pixels).unwrap();
        }
        buf.into_inner()
    }

    fn write_gray16_tiff(width: u32, height: u32) -> Vec<u8> {
        let pixels: Vec<u16> = (0..width * height).map(|i| i as u16).collect();
        let mut buf = Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut buf).unwrap();
            enc.new_image::<colortype::Gray16>(width, height)
                .unwrap()
                .write_data(&pixels)
                .unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn converting_an_rgb8_tiff_preserves_pixels_and_the_exif_sub_ifd() {
        let source = write_rgb8_tiff_with_exif(12, 9);

        let outcome = convert(&source).unwrap();
        let converted = outcome.expect("a plain RGB8 TIFF with an EXIF sub-IFD must convert");
        assert_eq!(converted.stored_format, "tiff");

        // Independently re-verify pixels and the Make/ExposureTime tags via
        // a fresh decode of what convert() produced.
        let mut decoder = Decoder::new(Cursor::new(&converted.bytes)).unwrap();
        assert_eq!(decoder.dimensions().unwrap(), (12, 9));
        let make = decoder
            .find_tag(Tag::Make)
            .unwrap()
            .unwrap()
            .into_string()
            .unwrap();
        assert_eq!(make, "LensLocker Test Co");
        let exif_dir = exif_directory(&mut decoder)
            .unwrap()
            .expect("EXIF sub-IFD must survive");
        let exposure = decoder
            .read_directory_tags(&exif_dir)
            .find_tag(Tag::Unknown(0x829A) /* ExposureTime */)
            .unwrap()
            .unwrap();
        assert_eq!(exposure, Value::Rational(1, 125));
    }

    #[test]
    fn converting_a_tiff_with_an_unsupported_bit_depth_is_a_hard_stop() {
        let source = write_gray16_tiff(4, 4);

        let outcome = convert(&source).unwrap();

        assert!(
            outcome.is_err(),
            "16-bit TIFF is out of this milestone's scope and must not be converted"
        );
    }
}
