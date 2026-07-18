//! BLAKE3 exact hashing + 64-bit perceptual hashing, per workplan/SPEC.md §1/§6.
//!
//! Milestone 1 implements exact hashing only; perceptual hashing lands in
//! Milestone 3 (workplan/SPEC.md Part 2).

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

#[cfg(test)]
mod tests {
    use std::fs;

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
        assert_eq!(hex.len(), 64, "a 32-byte digest must hex-encode to 64 characters");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
