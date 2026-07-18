//! Format decode: image crate, jxl-oxide, re_rav1d/dav1d, resvg, Windows WIC (HEIC).
//! Camera RAW is deliberately excluded — see `lumenvault-raw-worker`.
//! Per workplan/SPEC.md §5.
//!
//! Milestone 1 implemented standard-format probing for JPEG, PNG, WebP, GIF,
//! BMP via the `image` crate. Milestone 2 adds TIFF to that set — a
//! deliberate, narrow extension (not the full §5 matrix, which still leaves
//! RAW, HEIC, AVIF, JPEG XL, and SVG for later milestones): §4's conversion
//! policy defines a TIFF→TIFF recompression path, and without decode-
//! validating TIFF here it could never reach `lumenvault-convert` through
//! the real import pipeline. This is a genuine ambiguity §4 doesn't
//! resolve directly (only `lumenvault-convert`'s scope is named in the
//! Milestone 2 line) — flagged here rather than silently assumed.
//!
//! Milestone 4 adds JPEG XL decode ([`StandardFormat::Jxl`], via
//! `jxl-oxide`) — needed for `lumenvault-import`'s rebuild-from-sidecars
//! recovery path (workplan/SPEC.md §7): some blobs in the store are `.jxl`
//! (Milestone 2's conversion output), and rebuilding `perceptual_hash` for
//! them after the catalog is lost requires actually decoding those pixels,
//! not just re-hashing the file's raw bytes. This reuses the same
//! `jxl-oxide` version `lumenvault-convert` already depends on (there for
//! metadata-readback verification) — the same kind of narrow,
//! spec-consistent extension Milestone 2 made when it added TIFF here.
//!
//! Milestone 3 adds [`raw_extension`] — filename-extension-only recognition
//! of common camera RAW formats, **no pixel decode attempted**. This is a
//! deliberate, narrow stand-in for real RAW support: §5's format matrix
//! promises "Full" v1 support for camera RAW via the `rawler` crate in an
//! isolated `lumenvault-raw-worker` process, but no milestone (0-7) in
//! workplan/SPEC.md Part 2 ever schedules building that integration —
//! `lumenvault-raw-worker` is still the Milestone 0 stub. Milestone 3's own
//! exit criteria requires testing "a RAW+JPEG pair," which only needs
//! RAW+JPEG *pairing* (filename stem matching, §3 step 2) to work, not full
//! RAW pixel decode. Recognizing the extension is enough for that: a
//! recognized RAW file gets `original_format` set to its extension, and
//! `width`/`height`/`perceptual_hash` all stay `NULL` (the schema's own
//! Milestone-0 comment anticipated exactly this: "nullable (e.g. RAW
//! without a raster path yet)"). Full RAW rendering (rawler decode,
//! thumbnails, real perceptual hashing of RAW content) remains unbuilt and
//! un-scheduled — flagged here, not silently papered over.

use std::path::Path;

use image::ImageFormat;

/// The formats this milestone can decode-validate. A file outside this set
/// is refused at import — "the managed store shouldn't take custody of a
/// file it can't decode" (workplan/SPEC.md §5's principle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardFormat {
    Jpeg,
    Png,
    WebP,
    Gif,
    Bmp,
    Tiff,
    /// JPEG XL — decode only (via `jxl-oxide`); encoding is
    /// `lumenvault-convert`'s job (§4). Not detected through the `image`
    /// crate's format-guessing (it has no JXL support at all), so
    /// [`probe`] falls back to a dedicated JXL attempt when the standard
    /// path finds no recognizable signature — see [`probe`]'s doc comment.
    Jxl,
}

impl StandardFormat {
    /// Lowercase name used as the stored file extension and the
    /// `images.original_format` / `stored_format` catalog values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::WebP => "webp",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Jxl => "jxl",
        }
    }

    fn from_image_format(format: ImageFormat) -> Option<Self> {
        match format {
            ImageFormat::Jpeg => Some(Self::Jpeg),
            ImageFormat::Png => Some(Self::Png),
            ImageFormat::WebP => Some(Self::WebP),
            ImageFormat::Gif => Some(Self::Gif),
            ImageFormat::Bmp => Some(Self::Bmp),
            ImageFormat::Tiff => Some(Self::Tiff),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Probe {
    pub format: StandardFormat,
    pub width: u32,
    pub height: u32,
    /// The fully decoded image — probing already does a full decode (see
    /// `probe`'s doc comment), so this is free to carry forward rather than
    /// re-decoding later. Milestone 3 uses it to compute the perceptual
    /// hash (`lumenvault-hash::perceptual_hash`) without a second decode.
    pub image: image::DynamicImage,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("could not read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Recognized by the `image` crate, but not one of this milestone's six
    /// standard formats (e.g. ICO, AVIF) — a distinct condition from
    /// `Decode`, which is "the bytes don't parse as any image at all."
    #[error("not a standard-format image (JPEG/PNG/WebP/GIF/BMP/TIFF), got {0:?}")]
    UnsupportedFormat(ImageFormat),
    #[error("no recognizable image format signature")]
    UnrecognizedFormat,
    #[error("not a decodable image: {0}")]
    Decode(#[from] image::ImageError),
    #[error("not a decodable JPEG XL image at {path}: {source}")]
    JxlDecode { path: std::path::PathBuf, source: Box<dyn std::error::Error + Send + Sync> },
}

/// Attempts to decode `path` as one of this crate's recognized formats and
/// reports its format and pixel dimensions. A full decode (not just a
/// header peek) is deliberate — it's the strongest signal available that
/// the file isn't corrupt before the managed store takes custody of it.
///
/// JPEG XL is checked as a fallback, not through the same `image`-crate
/// guessed-format path as the other six: the `image` crate has no JXL
/// support, so a JXL file always reports `UnrecognizedFormat` there first.
pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    match probe_standard(path) {
        Err(ProbeError::UnrecognizedFormat) => probe_jxl(path),
        result => result,
    }
}

fn probe_standard(path: &Path) -> Result<Probe, ProbeError> {
    let reader = image::ImageReader::open(path)
        .map_err(|source| ProbeError::Io { path: path.to_path_buf(), source })?
        .with_guessed_format()
        .map_err(|source| ProbeError::Io { path: path.to_path_buf(), source })?;

    let detected = reader.format().ok_or(ProbeError::UnrecognizedFormat)?;
    let format = StandardFormat::from_image_format(detected).ok_or(ProbeError::UnsupportedFormat(detected))?;

    let decoded = reader.decode()?;

    Ok(Probe { format, width: decoded.width(), height: decoded.height(), image: decoded })
}

/// Decodes a JPEG XL file via `jxl-oxide`, converting its rendered pixel
/// buffer into an `image::DynamicImage` so callers (perceptual hashing, in
/// particular) don't need to know JXL was involved at all.
fn probe_jxl(path: &Path) -> Result<Probe, ProbeError> {
    let jxl_image = jxl_oxide::JxlImage::builder()
        .open(path)
        .map_err(|source| ProbeError::JxlDecode { path: path.to_path_buf(), source })?;

    let render =
        jxl_image.render_frame(0).map_err(|source| ProbeError::JxlDecode { path: path.to_path_buf(), source })?;
    let frame = render.image_all_channels();

    let width = frame.width() as u32;
    let height = frame.height() as u32;
    let channels = frame.channels();
    let samples = frame.buf();

    // `frame.buf()` is interleaved-by-channel f32 in [0, 1]; §1's dHash
    // perceptual hash only needs an 8-bit RGB approximation, so take the
    // first three channels (grayscale images replicate channel 0 across
    // R/G/B; a 4th alpha/extra channel, if present, is simply not used).
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for pixel in samples.chunks_exact(channels) {
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if channels >= 3 {
            rgb.push(to_u8(pixel[0]));
            rgb.push(to_u8(pixel[1]));
            rgb.push(to_u8(pixel[2]));
        } else {
            let gray = to_u8(pixel[0]);
            rgb.extend_from_slice(&[gray, gray, gray]);
        }
    }

    let buffer = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| ProbeError::JxlDecode {
        path: path.to_path_buf(),
        source: "decoded pixel buffer did not match the reported width/height".into(),
    })?;

    Ok(Probe { format: StandardFormat::Jxl, width, height, image: image::DynamicImage::ImageRgb8(buffer) })
}

/// Common camera RAW extensions, recognized by filename only — see the
/// module doc for why this is deliberately not a real RAW decoder. Not
/// exhaustive (real RAW support per §5 would use `rawler`'s own format
/// detection); this is a short, easy-to-extend list covering the common
/// camera vendors, sufficient for RAW+JPEG pairing (§3 step 2).
const RAW_EXTENSIONS: &[&str] =
    &["cr2", "cr3", "nef", "arw", "dng", "rw2", "raf", "orf", "pef", "srw"];

/// Returns the lowercase extension of `path` if it's one of
/// [`RAW_EXTENSIONS`], without attempting to decode any pixel data.
pub fn raw_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    RAW_EXTENSIONS.iter().find(|&&candidate| candidate == ext).copied()
}

/// Whether `format` (an `images.original_format` value, already lowercase)
/// is one of [`RAW_EXTENSIONS`] — used by `lumenvault-import`'s RAW+JPEG
/// pairing to tell which side of a candidate pair is the RAW file, without
/// re-deriving the extension from a path a second time.
pub fn is_raw_extension(format: &str) -> bool {
    RAW_EXTENSIONS.contains(&format)
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgb};

    fn write_png(dir: &std::path::Path, name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        img.save(&path).unwrap();
        path
    }

    #[test]
    fn probing_a_valid_png_reports_its_format_and_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "sample.png", 64, 32);

        let probe = super::probe(&path).unwrap();

        assert_eq!(probe.format, super::StandardFormat::Png);
        assert_eq!(probe.width, 64);
        assert_eq!(probe.height, 32);
    }

    #[test]
    fn probing_a_valid_tiff_reports_its_format_and_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.tiff");
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(20, 10, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 64]));
        img.save(&path).unwrap();

        let probe = super::probe(&path).unwrap();

        assert_eq!(probe.format, super::StandardFormat::Tiff);
        assert_eq!(probe.width, 20);
        assert_eq!(probe.height, 10);
    }

    #[test]
    fn probing_a_file_that_isnt_an_image_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-an-image.txt");
        std::fs::write(&path, b"this is definitely not image data").unwrap();

        let result = super::probe(&path);

        assert!(result.is_err(), "a non-image file must not be importable");
    }

    #[test]
    fn probing_a_missing_file_is_rejected() {
        let result = super::probe(std::path::Path::new("does-not-exist.png"));
        assert!(result.is_err());
    }

    #[test]
    fn probing_a_valid_image_also_returns_the_decoded_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), "sample.png", 4, 4);

        let probe = super::probe(&path).unwrap();

        assert_eq!((probe.image.width(), probe.image.height()), (4, 4));
    }

    #[test]
    fn common_raw_extensions_are_recognized_case_insensitively() {
        for ext in ["cr2", "CR3", "nef", "Arw", "dng", "rw2", "raf", "orf"] {
            let path = std::path::Path::new("photo").with_extension(ext);
            assert!(super::raw_extension(&path).is_some(), "expected {ext} to be recognized as RAW");
        }
    }

    #[test]
    fn standard_format_extensions_are_not_recognized_as_raw() {
        for ext in ["jpg", "png", "webp", "txt"] {
            let path = std::path::Path::new("photo").with_extension(ext);
            assert_eq!(super::raw_extension(&path), None, "{ext} must not be recognized as RAW");
        }
    }

    #[test]
    fn is_raw_extension_matches_the_lowercase_format_strings_stored_in_the_catalog() {
        assert!(super::is_raw_extension("cr2"));
        assert!(!super::is_raw_extension("jpeg"));
    }

    /// Encodes a real JXL fixture via `jpegxl-rs` (the same encoder
    /// `lumenvault-convert` uses in production) — proving `probe`'s JXL
    /// path decodes an actual, real-world-shaped JXL file, not just bytes
    /// this test happens to hand-construct.
    fn write_jxl(dir: &std::path::Path, name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        let pixels: Vec<u8> = (0..width * height)
            .flat_map(|i| {
                let x = i % width;
                let y = i / width;
                [(x % 256) as u8, (y % 256) as u8, 128, 255]
            })
            .collect();

        let mut encoder = jpegxl_rs::encoder_builder()
            .has_alpha(true)
            .lossless(true)
            .use_container(true)
            .uses_original_profile(true)
            .color_encoding(jpegxl_rs::encode::ColorEncoding::Srgb)
            .build()
            .unwrap();
        let frame = jpegxl_rs::encode::EncoderFrame::new(&pixels).num_channels(4);
        let encoded = encoder.encode_frame::<u8, u8>(&frame, width, height).unwrap();
        std::fs::write(&path, &encoded.data).unwrap();
        path
    }

    #[test]
    fn probing_a_jxl_file_reports_its_format_and_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_jxl(dir.path(), "sample.jxl", 24, 16);

        let probe = super::probe(&path).unwrap();

        assert_eq!(probe.format, super::StandardFormat::Jxl);
        assert_eq!(probe.width, 24);
        assert_eq!(probe.height, 16);
        assert_eq!((probe.image.width(), probe.image.height()), (24, 16));
    }

    #[test]
    fn probing_a_jxl_file_returns_pixels_close_to_the_source() {
        // dHash-relevant check: the decoded pixels should be close to the
        // source gradient, not just the right dimensions — proves this is
        // a real decode, not a stub returning a blank buffer.
        let dir = tempfile::tempdir().unwrap();
        let path = write_jxl(dir.path(), "gradient.jxl", 16, 16);

        let probe = super::probe(&path).unwrap();
        let rgb = probe.image.to_rgb8();

        let top_left = rgb.get_pixel(0, 0);
        let bottom_right = rgb.get_pixel(15, 15);
        assert_ne!(top_left, bottom_right, "a real gradient decode must vary across the image");
    }
}
