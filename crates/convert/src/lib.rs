//! Conversion policy implementation (jpegxl-rs, TIFF recompression), per
//! workplan/SPEC.md §4.
//!
//! §4's matrix, implemented one module per source format:
//! - [`jpeg`]: baseline JPEG → JPEG XL, lossless transcode via jbrd,
//!   reconstruct-verify + byte-compare.
//! - [`raster`]: PNG/BMP → JPEG XL Modular (lossless), pixel-compare +
//!   metadata-box verify. PNG additionally gates on an ancillary-chunk
//!   allowlist before conversion is attempted at all.
//! - [`tiff`]: TIFF → TIFF, recompressed in place (Deflate), tag directory
//!   read back and compared after the round-trip.
//!
//! **Never-convert is a hard stop, not a degrade** (§4): any verification
//! failure below returns [`ConversionOutcome`]'s `Err` side, never a
//! partially-verified success. The caller (`lumenvault-import`) is
//! responsible for leaving the original file untouched when that happens.

pub mod jpeg;
pub mod raster;
pub mod tiff;

/// The four source formats §4 defines a conversion path for. Every other
/// format (WebP, GIF, AVIF, RAW, unrecognized) has no path at all —
/// `lumenvault-import` never calls into this crate for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertibleFormat {
    Jpeg,
    Png,
    Bmp,
    Tiff,
}

/// A successful conversion: the bytes to write to the content-addressed
/// store in place of the original, and the format extension/`stored_format`
/// value they should be recorded under.
#[derive(Debug)]
pub struct Converted {
    pub bytes: Vec<u8>,
    pub stored_format: &'static str,
}

/// Why a conversion attempt was abandoned. Every variant here corresponds to
/// §4's "On failure" column: the original is left untouched, never a
/// best-effort partial result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkipReason {
    /// A PNG carried an ancillary chunk outside the allowlist — cannot prove
    /// it's safe to drop on re-encode, so the file is never even attempted.
    #[error("ancillary chunk {chunk_type:?} is outside the allowlist")]
    UnrecognizedAncillaryChunk { chunk_type: String },
    /// JPEG jbrd reconstruction didn't come back byte-identical to the
    /// source (or libjxl fell back to pixel-only reconstruction).
    #[error("JPEG XL reconstruction did not byte-match the source JPEG")]
    ReconstructionMismatch,
    /// Decoded pixels after the round-trip didn't match the source exactly.
    #[error("decoded pixels did not match the source exactly")]
    PixelMismatch,
    /// EXIF/XMP/ICC present in the source could not be confirmed present
    /// (and matching) in the converted output.
    #[error("metadata (EXIF/XMP/ICC) did not survive conversion: {0}")]
    MetadataMismatch(String),
    /// TIFF's IFD tag directory didn't read back identically after
    /// recompression.
    #[error("TIFF tag directory did not survive recompression: {0}")]
    TagDirectoryMismatch(String),
    /// The encoder itself refused the input (unsupported subsampling,
    /// corrupt structure, etc.) — not a verification failure, but the same
    /// hard-stop applies.
    #[error("encoding failed: {0}")]
    EncodeFailed(String),
}

/// Either bytes to store, or a documented reason nothing was stored — never
/// a partial/best-effort result. See the module doc's "never-convert is a
/// hard stop" note.
pub type ConversionOutcome = Result<Converted, SkipReason>;

/// Internal-error type for conditions that aren't a §4 verification
/// failure but a genuine I/O/library-usage problem. `lumenvault-import`
/// treats this the same as a [`SkipReason`] — never touch the original —
/// but it's kept distinct because it signals a bug or environment problem,
/// not an expected "this file can't be converted".
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Tiff(#[from] ::tiff::TiffError),
    #[error(transparent)]
    JxlEncode(#[from] jpegxl_rs::EncodeError),
    #[error(transparent)]
    JxlDecode(#[from] jpegxl_rs::DecodeError),
    #[error(transparent)]
    Png(#[from] png::DecodingError),
}

/// Runs the §4 conversion path for `format` against `source_bytes`.
///
/// Never touches disk itself — pure bytes in, bytes-or-reason out. The
/// caller decides what to do with the original file based on the result.
pub fn convert(
    format: ConvertibleFormat,
    source_bytes: &[u8],
) -> Result<ConversionOutcome, ConvertError> {
    match format {
        ConvertibleFormat::Jpeg => jpeg::convert(source_bytes),
        ConvertibleFormat::Png => raster::convert_png(source_bytes),
        ConvertibleFormat::Bmp => raster::convert_bmp(source_bytes),
        ConvertibleFormat::Tiff => tiff::convert(source_bytes),
    }
}
