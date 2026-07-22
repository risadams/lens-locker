//! The vault-root plaintext status marker (`vault.json`) — ticket 044.
//!
//! Lives outside the encrypted VHDX, at the vault root. Must be readable
//! before any mount/unlock attempt, since the app can't open an encrypted
//! catalog to check whether it's encrypted — that would be circular. It
//! reveals only "this vault is locked" plus the (non-secret) KDF salt,
//! never anything about contents.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::VaultError;
use crate::secret::KDF_SALT_LEN;

pub const MARKER_FILE_NAME: &str = "vault.json";
pub const VHDX_FILE_NAME: &str = "vault.vhdx";
pub const MOUNT_DIR_NAME: &str = "live";
pub const CURRENT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultMarker {
    pub encrypted: bool,
    pub format_version: u32,
    /// Hex-encoded per-vault KDF salt (ticket 041/048) — not secret, must
    /// stay stable for the vault's lifetime once written.
    pub kdf_salt_hex: String,
}

impl VaultMarker {
    pub fn new(salt: &[u8; KDF_SALT_LEN]) -> Self {
        let mut kdf_salt_hex = String::with_capacity(KDF_SALT_LEN * 2);
        for byte in salt {
            kdf_salt_hex.push_str(&format!("{byte:02x}"));
        }
        Self {
            encrypted: true,
            format_version: CURRENT_FORMAT_VERSION,
            kdf_salt_hex,
        }
    }

    pub fn salt(&self) -> Result<[u8; KDF_SALT_LEN], VaultError> {
        let bytes = hex_decode(&self.kdf_salt_hex)
            .ok_or_else(|| VaultError::Marker("kdf_salt_hex is not valid hex".into()))?;
        bytes
            .try_into()
            .map_err(|_| VaultError::Marker(format!("kdf salt must be {KDF_SALT_LEN} bytes")))
    }
}

pub fn marker_path(vault_root: &Path) -> PathBuf {
    vault_root.join(MARKER_FILE_NAME)
}

pub fn vhdx_path(vault_root: &Path) -> PathBuf {
    vault_root.join(VHDX_FILE_NAME)
}

pub fn mount_point(vault_root: &Path) -> PathBuf {
    vault_root.join(MOUNT_DIR_NAME)
}

/// `Ok(None)` means no marker exists — the vault at this root is not
/// Locking-enabled (either non-encrypted, or not a vault at all yet).
pub fn read_marker(vault_root: &Path) -> Result<Option<VaultMarker>, VaultError> {
    let path = marker_path(vault_root);
    if !path.is_file() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path).map_err(VaultError::Io)?;
    let marker: VaultMarker =
        serde_json::from_str(&contents).map_err(|e| VaultError::Marker(e.to_string()))?;
    Ok(Some(marker))
}

pub fn write_marker(vault_root: &Path, marker: &VaultMarker) -> Result<(), VaultError> {
    let path = marker_path(vault_root);
    let contents =
        serde_json::to_string_pretty(marker).map_err(|e| VaultError::Marker(e.to_string()))?;
    fs::write(&path, contents).map_err(VaultError::Io)
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("lenslocker-vault-marker-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let salt = [7u8; KDF_SALT_LEN];
        let marker = VaultMarker::new(&salt);
        write_marker(&dir, &marker).unwrap();

        let read_back = read_marker(&dir).unwrap().expect("marker should exist");
        assert!(read_back.encrypted);
        assert_eq!(read_back.format_version, CURRENT_FORMAT_VERSION);
        assert_eq!(read_back.salt().unwrap(), salt);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_marker_is_none_not_error() {
        let dir = std::env::temp_dir().join(format!("lenslocker-vault-marker-missing-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert!(read_marker(&dir).unwrap().is_none());
        fs::remove_dir_all(&dir).ok();
    }
}
