//! Ed25519 keypair generation and raw-key extraction — the "SSH key"
//! factor (ticket 038/043). Generated per-vault, in the SSH-familiar
//! OpenSSH file format, at a user-chosen location outside the vault.

use std::path::{Path, PathBuf};

use rand_core::OsRng;
use ssh_key::{Algorithm, LineEnding, PrivateKey};

use crate::VaultError;

pub const PRIVATE_KEY_FILE_NAME: &str = "lenslocker-vault-key";
pub const PUBLIC_KEY_FILE_NAME: &str = "lenslocker-vault-key.pub";

/// Generates a fresh ed25519 keypair and writes both halves (OpenSSH file
/// format) into `dest_dir`. Returns the private key file's path — what the
/// app persists as this vault's configured key location (ticket 043).
pub fn generate_and_write_keypair(dest_dir: &Path, comment: &str) -> Result<PathBuf, VaultError> {
    std::fs::create_dir_all(dest_dir).map_err(VaultError::Io)?;

    let mut private_key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)
        .map_err(|e| VaultError::Keypair(e.to_string()))?;
    private_key
        .set_comment(comment);

    let private_path = dest_dir.join(PRIVATE_KEY_FILE_NAME);
    let public_path = dest_dir.join(PUBLIC_KEY_FILE_NAME);

    private_key
        .write_openssh_file(&private_path, LineEnding::default())
        .map_err(|e| VaultError::Keypair(e.to_string()))?;

    private_key
        .public_key()
        .write_openssh_file(&public_path)
        .map_err(|e| VaultError::Keypair(e.to_string()))?;

    Ok(private_path)
}

/// Extracts raw key material from an existing OpenSSH-format ed25519
/// private key file, for the Argon2id+HKDF combination scheme (ticket
/// 041). Parses directly via `ssh-key` rather than converting through
/// `age`'s format first — `LOCK-SPEC.md` §2 named `ssh-to-age` as one
/// option, but since the combination scheme is entirely custom (not age's
/// recipient model), going straight to the OpenSSH key's own raw bytes is
/// simpler and avoids depending on a second crate for the same file.
pub fn read_raw_key_bytes(path: &Path) -> Result<Vec<u8>, VaultError> {
    let private_key = PrivateKey::read_openssh_file(path)
        .map_err(|e| VaultError::Keypair(format!("could not read keypair file: {e}")))?;

    let ed25519 = private_key
        .key_data()
        .ed25519()
        .ok_or_else(|| VaultError::Keypair("keypair file is not an ed25519 key".into()))?;

    Ok(ed25519.private.as_ref().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_a_readable_ed25519_keypair() {
        let dir = std::env::temp_dir().join(format!(
            "lenslocker-vault-keypair-test-{}",
            std::process::id()
        ));

        let private_path = generate_and_write_keypair(&dir, "lenslocker-vault-test").unwrap();
        assert!(private_path.is_file());
        assert!(dir.join(PUBLIC_KEY_FILE_NAME).is_file());

        let raw = read_raw_key_bytes(&private_path).unwrap();
        assert_eq!(raw.len(), 32, "ed25519 private key seed should be 32 bytes");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_generated_keypairs_have_different_raw_bytes() {
        let dir_a = std::env::temp_dir().join(format!(
            "lenslocker-vault-keypair-test-a-{}",
            std::process::id()
        ));
        let dir_b = std::env::temp_dir().join(format!(
            "lenslocker-vault-keypair-test-b-{}",
            std::process::id()
        ));

        let a = generate_and_write_keypair(&dir_a, "a").unwrap();
        let b = generate_and_write_keypair(&dir_b, "b").unwrap();

        assert_ne!(read_raw_key_bytes(&a).unwrap(), read_raw_key_bytes(&b).unwrap());

        std::fs::remove_dir_all(&dir_a).ok();
        std::fs::remove_dir_all(&dir_b).ok();
    }
}
