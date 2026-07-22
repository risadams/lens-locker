//! Portable logic for LensLocker's Locking feature (`workplan/LOCK-SPEC.md`).
//!
//! Deliberately has no Windows-specific dependency and no elevated
//! operations of its own — those live in `vault-helper`
//! (`workplan/LOCK-SPEC.md` §3, Milestone L0's judgment call). This split
//! mirrors `SPEC.md` §2's "core crates stay OS-agnostic, platform-specific
//! work sits in a thin shell" principle: `vault` is the shell-agnostic
//! logic (combined-secret derivation, the marker file format, the
//! command/response protocol spoken to the helper), `vault-helper` is the
//! Windows-specific, separately-elevated process that actually calls
//! `AttachVirtualDisk`/BitLocker.

pub mod keypair;
pub mod marker;
pub mod protocol;
pub mod secret;

pub use keypair::{generate_and_write_keypair, read_raw_key_bytes};
pub use marker::{VaultMarker, mount_point, read_marker, vhdx_path, write_marker};
pub use protocol::{VaultCommand, VaultResponse};
pub use secret::{CombinedSecret, derive_combined_secret};

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("key derivation failed: {0}")]
    KeyDerivation(String),
    #[error("vault marker error: {0}")]
    Marker(String),
    #[error("vault-helper protocol error: {0}")]
    Protocol(String),
    #[error("keypair error: {0}")]
    Keypair(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
