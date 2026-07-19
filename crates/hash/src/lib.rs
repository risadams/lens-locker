//! BLAKE3 exact hashing + 64-bit perceptual hashing, per workplan/SPEC.md §1/§6.
//!
//! Milestone 1 implemented exact hashing only. Milestone 3 adds the 64-bit
//! perceptual hash and its Hamming-distance comparison, hand-rolled on top
//! of the `image` crate's generic resize/grayscale ops rather than pulling
//! in a dedicated (possibly unmaintained) perceptual-hashing crate — this
//! project has consistently preferred minimal, well-understood dependencies.

use std::fs;
use std::io;
use std::path::Path;

/// A BLAKE3 digest — raw bytes, matching the schema's `BLOB` hash columns
/// (`images.original_hash`, `images.stored_hash`).
pub type Digest = [u8; 32];

/// Hashes a file's full contents with BLAKE3.
pub fn hash_file(path: &Path) -> io::Result<Digest> {
    let bytes = fs::read(path)?;
    Ok(hash_bytes(&bytes))
}

/// Hashes a byte slice with BLAKE3.
pub fn hash_bytes(bytes: &[u8]) -> Digest {
    *blake3::hash(bytes).as_bytes()
}

/// Lowercase hex encoding of a digest, used for content-addressed paths.
pub fn to_hex(digest: &Digest) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A 64-bit perceptual hash — unlike [`Digest`], designed so visually
/// similar images produce hashes with a small Hamming distance rather than
/// changing completely for a one-bit difference in the source bytes.
/// Matches the `images.perceptual_hash` catalog column (stored as a signed
/// 64-bit `INTEGER`; callers cast with `as i64`/`as u64` at that boundary).
pub type PerceptualHash = u64;

/// Computes a difference hash (dHash) of `image`, per workplan/SPEC.md §1/§6.
///
/// The image is resized to a 9x8 grayscale grid, then for each of the 8 rows
/// each of the 8 adjacent-pixel-pairs contributes one bit: 1 if the left
/// pixel is brighter than its right neighbor, 0 otherwise. 8 rows x 8 bits =
/// 64 bits. This is the simplest well-specified perceptual hash and is
/// robust to the kind of differences near-duplicates actually have (JPEG
/// re-encoding at a different quality, minor crops/resizes) without needing
/// any external dependency beyond `image`'s own generic resize/grayscale
/// support.
pub fn perceptual_hash(image: &image::DynamicImage) -> PerceptualHash {
    let small = image
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .into_luma8();

    let mut hash: u64 = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let left = small.get_pixel(x, y).0[0];
            let right = small.get_pixel(x + 1, y).0[0];
            hash = (hash << 1) | u64::from(left > right);
        }
    }
    hash
}

/// Hamming distance between two perceptual hashes: popcount of the XOR.
/// This is the value workplan/SPEC.md §6 compares against
/// `app_settings.hamming_threshold` (default 5) to decide whether a pair of
/// images is a near-duplicate candidate for the review queue.
pub fn hamming_distance(a: PerceptualHash, b: PerceptualHash) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use image::{DynamicImage, ImageBuffer, Rgb};

    fn gradient(width: u32, height: u32) -> DynamicImage {
        // A smooth gradient scaled by *fraction* of width/height (not a
        // fixed-period modulo) so the same visual content, resized to any
        // resolution, still resamples down to a near-identical 9x8 dHash
        // grid — a fixed-period pattern would alias differently at each
        // resolution and defeat the point of this fixture.
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
            let r = (x as f32 / width as f32 * 255.0) as u8;
            let g = (y as f32 / height as f32 * 255.0) as u8;
            let b = ((x + y) as f32 / (width + height) as f32 * 255.0) as u8;
            Rgb([r, g, b])
        });
        DynamicImage::ImageRgb8(img)
    }

    fn checkerboard(width: u32, height: u32) -> DynamicImage {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
            if (x / 4 + y / 4) % 2 == 0 {
                Rgb([255, 255, 255])
            } else {
                Rgb([0, 0, 0])
            }
        });
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn hamming_distance_of_a_hash_against_itself_is_zero() {
        let a = super::hamming_distance(0b1010_1010, 0b1010_1010);
        assert_eq!(a, 0);
    }

    #[test]
    fn hamming_distance_counts_differing_bits() {
        let distance = super::hamming_distance(0b0000_0000, 0b0000_1111);
        assert_eq!(distance, 4);
    }

    #[test]
    fn perceptual_hash_of_the_same_image_twice_is_identical() {
        let a = super::perceptual_hash(&gradient(64, 64));
        let b = super::perceptual_hash(&gradient(64, 64));
        assert_eq!(a, b);
        assert_eq!(super::hamming_distance(a, b), 0);
    }

    #[test]
    fn perceptual_hash_is_stable_across_a_resize() {
        // A near-duplicate signal this milestone actually relies on: the
        // same visual content at a different resolution should hash close
        // to identically, since dHash itself normalizes to a fixed 9x8 grid.
        let a = super::perceptual_hash(&gradient(64, 64));
        let b = super::perceptual_hash(&gradient(256, 256));
        let distance = super::hamming_distance(a, b);
        assert!(
            distance <= 5,
            "expected a resized duplicate to stay within the default threshold, got {distance}"
        );
    }

    #[test]
    fn perceptual_hash_of_very_different_images_is_far_apart() {
        let a = super::perceptual_hash(&gradient(64, 64));
        let b = super::perceptual_hash(&checkerboard(64, 64));
        let distance = super::hamming_distance(a, b);
        assert!(
            distance > 5,
            "expected visually distinct images to exceed the default threshold, got {distance}"
        );
    }

    #[test]
    fn hashing_the_same_bytes_twice_is_deterministic() {
        let a = super::hash_bytes(b"lumenvault");
        let b = super::hash_bytes(b"lumenvault");
        assert_eq!(a, b);
    }

    #[test]
    fn different_bytes_hash_differently() {
        let a = super::hash_bytes(b"lumenvault");
        let b = super::hash_bytes(b"lumenvault2");
        assert_ne!(a, b);
    }

    #[test]
    fn hashing_a_file_matches_hashing_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.bin");
        fs::write(&path, b"content-addressed").unwrap();

        let from_file = super::hash_file(&path).unwrap();
        let from_bytes = super::hash_bytes(b"content-addressed");

        assert_eq!(from_file, from_bytes);
    }

    #[test]
    fn to_hex_round_trips_through_a_known_length() {
        let digest = super::hash_bytes(b"lumenvault");
        let hex = super::to_hex(&digest);
        assert_eq!(
            hex.len(),
            64,
            "a 32-byte digest must hex-encode to 64 characters"
        );
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
