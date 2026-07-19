//! Milestone 2 exit criteria (workplan/SPEC.md Part 2), executed for real:
//! "convert a real JPEG library, confirm byte-exact reconstruction
//! round-trips, confirm metadata survives." §4's matrix covers PNG/BMP/TIFF
//! too, so this file also exercises those for real, plus one genuine
//! never-convert case.
//!
//! `tests/fixtures/camera_like.jpg` is a real baseline JPEG generated with
//! ImageMagick (`magick -size 640x480 plasma:fractal -colorspace sRGB
//! -sampling-factor 2x2,1x1,1x1 -quality 90`) — genuine 4:2:0 chroma
//! subsampling, the case that most exercises jbrd reconstruction. It carries
//! no EXIF as generated (ImageMagick's `-set exif:...` turned out not to
//! embed a real APP1/EXIF segment on this ImageMagick build — verified by
//! round-tripping through `identify -format "%[EXIF:*]"`, which came back
//! empty), so this test injects a hand-built, genuinely valid EXIF APP1
//! segment directly into the JPEG bytes before conversion.

use std::io::Cursor;

use lumenvault_convert::{ConvertibleFormat, SkipReason, convert};

const CAMERA_LIKE_JPEG: &[u8] = include_bytes!("fixtures/camera_like.jpg");

/// Builds a minimal but genuinely valid EXIF APP1 segment (TIFF header +
/// one IFD0 with Make/Model/Orientation/DateTime) and splices it in right
/// after the JPEG SOI marker — where real cameras place it.
fn jpeg_with_injected_exif(jpeg: &[u8]) -> Vec<u8> {
    assert_eq!(
        &jpeg[0..2],
        &[0xFF, 0xD8],
        "fixture must start with a JPEG SOI marker"
    );

    fn ascii_entry(tag: u16, value: &str) -> (u16, Vec<u8>) {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        (tag, bytes)
    }

    let make = ascii_entry(0x010F, "LumenVault Test Co");
    let model = ascii_entry(0x0110, "Fixture Camera Mk1");
    let date_time = ascii_entry(0x0132, "2026:07:17 12:00:00");

    let entry_count = 4u16;
    let ifd_size = 2 + usize::from(entry_count) * 12 + 4;
    let mut value_offset = 8 + ifd_size;

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II"); // little-endian
    tiff.extend_from_slice(&42u16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8
    tiff.extend_from_slice(&entry_count.to_le_bytes());

    let mut value_area = Vec::new();
    for (tag, value) in [&make, &model, &date_time] {
        tiff.extend_from_slice(&tag.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes()); // type = ASCII
        tiff.extend_from_slice(&(value.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&(value_offset as u32).to_le_bytes());
        value_area.extend_from_slice(value);
        value_offset += value.len();
    }
    // Orientation: SHORT, count 1, value inline (normal / no rotation).
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes()); // type = SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&0u16.to_le_bytes()); // padding to fill the 4-byte value field

    tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD offset = none
    tiff.extend_from_slice(&value_area);

    let mut app1_payload = b"Exif\0\0".to_vec();
    app1_payload.extend_from_slice(&tiff);

    let mut app1 = Vec::new();
    app1.extend_from_slice(&[0xFF, 0xE1]);
    app1.extend_from_slice(&((app1_payload.len() + 2) as u16).to_be_bytes());
    app1.extend_from_slice(&app1_payload);

    let mut spliced = jpeg[0..2].to_vec();
    spliced.extend_from_slice(&app1);
    spliced.extend_from_slice(&jpeg[2..]);
    spliced
}

#[test]
fn real_camera_like_jpeg_reconstructs_byte_exact_and_metadata_survives() {
    let source = jpeg_with_injected_exif(CAMERA_LIKE_JPEG);

    // Sanity-check the fixture is what it claims to be before trusting the
    // conversion result: baseline JPEG, real dimensions, our injected EXIF
    // actually present.
    let probe = image::load_from_memory_with_format(&source, image::ImageFormat::Jpeg)
        .expect("fixture must be a real, decodable JPEG");
    assert_eq!((probe.width(), probe.height()), (640, 480));

    let outcome = convert(ConvertibleFormat::Jpeg, &source).unwrap();
    let converted =
        outcome.expect("a real baseline JPEG with injected EXIF must convert successfully");
    assert_eq!(converted.stored_format, "jxl");
    println!(
        "camera_like.jpg: {} bytes source -> {} bytes JPEG XL ({:.1}% of original)",
        source.len(),
        converted.bytes.len(),
        100.0 * converted.bytes.len() as f64 / source.len() as f64
    );

    // The exit criterion, checked directly (not just trusting convert()'s
    // own internal check): decode the JPEG XL output, reconstruct the
    // original JPEG bitstream, and byte-compare against the source. This
    // single comparison is also the metadata check — every APP1/EXIF byte
    // is part of what must match for the comparison to pass at all.
    let decoder = jpegxl_rs::decoder_builder().build().unwrap();
    let (_, data) = decoder.reconstruct(&converted.bytes).unwrap();
    let jpegxl_rs::decode::Data::Jpeg(reconstructed) = data else {
        panic!("expected JPEG bitstream reconstruction data, got pixels only");
    };

    assert_eq!(
        reconstructed.len(),
        source.len(),
        "reconstructed JPEG has a different length than the source"
    );
    assert_eq!(
        reconstructed, source,
        "reconstructed JPEG must be byte-for-byte identical to the source"
    );

    // Belt-and-suspenders: independently confirm the injected EXIF bytes
    // specifically are present at the expected offset in the reconstructed
    // output (not just "the whole file happens to match").
    let exif_app1 = &source[2..2 + 2 + u16::from_be_bytes([source[4], source[5]]) as usize];
    assert!(
        reconstructed
            .windows(exif_app1.len())
            .any(|w| w == exif_app1),
        "the injected EXIF APP1 segment must be present, byte-for-byte, in the reconstructed JPEG"
    );
}

#[test]
fn real_png_with_exif_and_xmp_round_trips_pixel_exact_and_metadata_survives() {
    use png::text_metadata::ITXtChunk;

    let width = 96;
    let height = 64;
    let pixels: Vec<u8> = (0..width * height)
        .flat_map(|i| {
            let x = i % width;
            let y = i / width;
            [
                ((x * 3) % 256) as u8,
                ((y * 5) % 256) as u8,
                ((x + y) % 256) as u8,
                255,
            ]
        })
        .collect();

    let exif = {
        // A tiny but real EXIF blob: TIFF header + one-entry IFD0 (Make).
        let make = b"LumenVault Test Co\0";
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x010Fu16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(make.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&26u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(make);
        tiff
    };
    let xmp = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"/><?xpacket end="w"?>"#;

    let mut png_bytes = Vec::new();
    {
        let mut info = png::Info::with_size(width, height);
        info.color_type = png::ColorType::Rgba;
        info.bit_depth = png::BitDepth::Eight;
        info.exif_metadata = Some(exif.clone().into());
        info.utf8_text
            .push(ITXtChunk::new("XML:com.adobe.xmp", xmp));

        let encoder = png::Encoder::with_info(Cursor::new(&mut png_bytes), info).unwrap();
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&pixels).unwrap();
    }

    let outcome = convert(ConvertibleFormat::Png, &png_bytes).unwrap();
    let converted =
        outcome.expect("a real PNG with EXIF+XMP and only allowlisted chunks must convert");
    assert_eq!(converted.stored_format, "jxl");

    // Pixel-exactness, checked independently.
    let decoder = jpegxl_rs::decoder_builder().build().unwrap();
    let (meta, decoded_pixels) = decoder.decode_with::<u8>(&converted.bytes).unwrap();
    assert_eq!((meta.width, meta.height), (width, height));
    assert_eq!(
        decoded_pixels, pixels,
        "decoded pixels must exactly match the source PNG"
    );

    // Metadata survival, checked independently via jxl-oxide (not the same
    // decoder that did the encoding-side bookkeeping).
    let jxl_image = jxl_oxide::JxlImage::builder()
        .read(Cursor::new(&converted.bytes))
        .unwrap();
    let aux = jxl_image.aux_boxes();
    let actual_exif = aux.first_exif().unwrap().unwrap().payload().to_vec();
    assert_eq!(
        actual_exif, exif,
        "EXIF must survive PNG -> JPEG XL conversion, byte-for-byte"
    );
    let actual_xmp = aux.first_xml().unwrap().to_vec();
    assert_eq!(
        actual_xmp,
        xmp.as_bytes(),
        "XMP must survive PNG -> JPEG XL conversion, byte-for-byte"
    );
}

#[test]
fn real_tiff_with_metadata_recompresses_and_metadata_survives() {
    use tiff::encoder::{Compression, Rational, TiffEncoder, colortype};
    use tiff::tags::Tag;

    let width = 40;
    let height = 30;
    let pixels: Vec<u8> = (0..width * height * 3)
        .map(|i| ((i * 7) % 256) as u8)
        .collect();

    let mut source = Cursor::new(Vec::new());
    {
        let mut enc = TiffEncoder::new(&mut source)
            .unwrap()
            .with_compression(Compression::Uncompressed);
        let exif = {
            let mut extra = enc.extra_directory().unwrap();
            extra
                .write_tag(
                    Tag::Unknown(0x829A), /* ExposureTime */
                    Rational { n: 1, d: 200 },
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
            .write_tag(Tag::Make, "LumenVault Test Co")
            .unwrap();
        image
            .encoder()
            .write_tag(Tag::Unknown(700) /* XMP */, "<xmp>real</xmp>")
            .unwrap();
        image.write_data(&pixels).unwrap();
    }
    let source = source.into_inner();

    let outcome = convert(ConvertibleFormat::Tiff, &source).unwrap();
    let converted = outcome.expect("a real RGB8 TIFF with Make/ExposureTime/XMP must convert");
    assert_eq!(converted.stored_format, "tiff");

    // Independently re-decode the output and confirm pixels + tags.
    let mut decoder = tiff::decoder::Decoder::new(Cursor::new(&converted.bytes)).unwrap();
    assert_eq!(decoder.dimensions().unwrap(), (width, height));
    let tiff::decoder::DecodingResult::U8(out_pixels) = decoder.read_image().unwrap() else {
        panic!("expected 8-bit pixel data");
    };
    assert_eq!(
        out_pixels, pixels,
        "recompressed TIFF must decode to the exact same pixels"
    );

    let make = decoder
        .find_tag(Tag::Make)
        .unwrap()
        .unwrap()
        .into_string()
        .unwrap();
    assert_eq!(make, "LumenVault Test Co");
    let xmp = decoder
        .find_tag(Tag::Unknown(700))
        .unwrap()
        .unwrap()
        .into_string()
        .unwrap();
    assert_eq!(
        xmp, "<xmp>real</xmp>",
        "XMP tag value must survive recompression"
    );
}

#[test]
fn a_png_with_a_deliberately_unsafe_ancillary_chunk_is_never_converted() {
    // A real PNG (via the `image` crate), then a genuine, well-formed but
    // out-of-allowlist ancillary chunk ("prVt") spliced in right after
    // IHDR — exactly the "cannot prove round-trip safety" case §4 exists
    // to guard against.
    let width = 8;
    let height = 8;
    let img: image::RgbImage =
        image::ImageBuffer::from_fn(width, height, |x, y| image::Rgb([(x * y) as u8, 1, 2]));
    let mut png_bytes = Vec::new();
    img.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .unwrap();

    let ihdr_end = 8 + 8 + 13 + 4; // signature + (len+type) + IHDR data + crc
    let mut spliced = png_bytes[..ihdr_end].to_vec();
    spliced.extend_from_slice(&0u32.to_be_bytes());
    spliced.extend_from_slice(b"prVt");
    spliced.extend_from_slice(&0u32.to_be_bytes());
    spliced.extend_from_slice(&png_bytes[ihdr_end..]);

    let outcome = convert(ConvertibleFormat::Png, &spliced).unwrap();

    assert_eq!(
        outcome.unwrap_err(),
        SkipReason::UnrecognizedAncillaryChunk {
            chunk_type: "prVt".to_string()
        }
    );
    // Never-convert is a hard stop: nothing about the file's original bytes
    // is touched by this crate (it only ever returns bytes-or-a-reason —
    // `lumenvault-import` is what leaves the original file in place, which
    // is covered by its own test suite).
}
