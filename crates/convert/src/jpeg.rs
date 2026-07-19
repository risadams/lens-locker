//! Baseline JPEG → JPEG XL, lossless transcode with jbrd, per
//! workplan/SPEC.md §4's JPEG row.
//!
//! The whole point of jbrd (JPEG bitstream reconstruction data) is that the
//! JPEG XL container remembers exactly how to rebuild the original JPEG
//! byte-for-byte — so the verification here is the strongest one in the
//! whole matrix: encode, then immediately decode-and-reconstruct, and
//! byte-compare against the source before this crate ever reports success.
//! Since a byte-identical reconstruction implies every marker segment
//! (including APP1/EXIF and APP1/XMP) survived unchanged, this single
//! comparison *is* the metadata-preservation check for this path — no
//! separate EXIF/XMP inspection is needed here (contrast with `raster`,
//! where pixel-exactness does not imply metadata survival).

use jpegxl_rs::decode::Data;

use crate::{ConversionOutcome, ConvertError, Converted, SkipReason};

pub fn convert(source_bytes: &[u8]) -> Result<ConversionOutcome, ConvertError> {
    let mut encoder = jpegxl_rs::encoder_builder().use_container(true).build()?;

    let encoded = match encoder.encode_jpeg(source_bytes) {
        Ok(result) => result,
        Err(e) => return Ok(Err(SkipReason::EncodeFailed(e.to_string()))),
    };

    let decoder = jpegxl_rs::decoder_builder().build()?;
    let reconstructed = match decoder.reconstruct(&encoded.data) {
        Ok((_, Data::Jpeg(bytes))) => bytes,
        // libjxl couldn't reconstruct the exact JPEG bitstream (fell back to
        // pixels) — §4 has no pixel-exact fallback for JPEG, so this is a
        // hard stop, not a degrade.
        Ok((_, Data::Pixels(_))) => return Ok(Err(SkipReason::ReconstructionMismatch)),
        Err(_) => return Ok(Err(SkipReason::ReconstructionMismatch)),
    };

    if reconstructed != source_bytes {
        return Ok(Err(SkipReason::ReconstructionMismatch));
    }

    Ok(Ok(Converted {
        bytes: encoded.data,
        stored_format: "jxl",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but genuinely baseline JPEG (encoded by the `image` crate,
    /// which produces standard 4:2:0-capable JFIF baseline output),
    /// carrying no metadata — the plumbing test. The exit-criteria test in
    /// `tests/exit_criteria.rs` covers a real camera-like fixture with
    /// actual EXIF.
    fn synthetic_jpeg() -> Vec<u8> {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(64, 48, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        buf
    }

    #[test]
    fn converting_a_baseline_jpeg_round_trips_byte_exact() {
        let source = synthetic_jpeg();

        let outcome = convert(&source).unwrap();

        let converted = outcome.expect("a valid baseline JPEG must convert successfully");
        assert_eq!(converted.stored_format, "jxl");
        assert!(!converted.bytes.is_empty());

        // Independently re-verify: decode what convert() produced and
        // confirm it's still byte-identical (guards against convert()
        // lying about its own verification).
        let decoder = jpegxl_rs::decoder_builder().build().unwrap();
        let (_, data) = decoder.reconstruct(&converted.bytes).unwrap();
        let Data::Jpeg(reconstructed) = data else {
            panic!("expected JPEG reconstruction data to be present");
        };
        assert_eq!(
            reconstructed, source,
            "reconstructed bytes must be byte-identical to the source JPEG"
        );
    }

    #[test]
    fn converting_garbage_bytes_is_a_hard_stop_not_a_degrade() {
        let outcome = convert(b"not a jpeg at all").unwrap();
        assert!(
            outcome.is_err(),
            "non-JPEG input must never report a successful conversion"
        );
    }
}
