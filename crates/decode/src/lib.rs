//! Format decode: image crate, jxl-oxide, re_rav1d/dav1d, resvg, Windows WIC (HEIC).
//! Camera RAW is deliberately excluded — see `lumenvault-raw-worker`.
//! Per workplan/SPEC.md §5.
//!
//! Milestone 1 implements standard-format probing only (JPEG, PNG, WebP, GIF,
//! BMP via the `image` crate) — everything else in §5's matrix (RAW, HEIC,
//! AVIF, JPEG XL, SVG, TIFF) lands in later milestones.

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
        }
    }

    fn from_image_format(format: ImageFormat) -> Option<Self> {
        match format {
            ImageFormat::Jpeg => Some(Self::Jpeg),
            ImageFormat::Png => Some(Self::Png),
            ImageFormat::WebP => Some(Self::WebP),
            ImageFormat::Gif => Some(Self::Gif),
            ImageFormat::Bmp => Some(Self::Bmp),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Probe {
    pub format: StandardFormat,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("could not read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Recognized by the `image` crate, but not one of this milestone's five
    /// standard formats (e.g. TIFF, ICO) — a distinct condition from
    /// `Decode`, which is "the bytes don't parse as any image at all."
    #[error("not a standard-format image (JPEG/PNG/WebP/GIF/BMP), got {0:?}")]
    UnsupportedFormat(ImageFormat),
    #[error("no recognizable image format signature")]
    UnrecognizedFormat,
    #[error("not a decodable image: {0}")]
    Decode(#[from] image::ImageError),
}

/// Attempts to decode `path` as one of the five standard formats and
/// reports its format and pixel dimensions. A full decode (not just a
/// header peek) is deliberate — it's the strongest signal available that
/// the file isn't corrupt before the managed store takes custody of it.
pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    let reader = image::ImageReader::open(path)
        .map_err(|source| ProbeError::Io { path: path.to_path_buf(), source })?
        .with_guessed_format()
        .map_err(|source| ProbeError::Io { path: path.to_path_buf(), source })?;

    let detected = reader.format().ok_or(ProbeError::UnrecognizedFormat)?;
    let format = StandardFormat::from_image_format(detected).ok_or(ProbeError::UnsupportedFormat(detected))?;

    let decoded = reader.decode()?;

    Ok(Probe { format, width: decoded.width(), height: decoded.height() })
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
}
