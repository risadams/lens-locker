//! The command protocol spoken between the unprivileged main app process
//! and the separately-elevated `vault-helper` subprocess (ticket 040/052).
//!
//! The helper is launched fresh for each attach/detach/create call (never
//! a standing elevated session), reads one `VaultCommand` as JSON on
//! stdin, performs the operation, and writes one `VaultResponse` as JSON
//! on stdout before exiting. Keeping the wire format to "one command in,
//! one response out" means there's no long-lived elevated process to
//! reason about the trust boundary of.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum VaultCommand {
    /// Creates a new fixed-size VHDX, formats it NTFS, and enables
    /// BitLocker with `combined_secret_hex` as the password protector —
    /// ticket 043/050's new-vault and migration creation paths.
    CreateAndEncrypt {
        vhdx_path: PathBuf,
        size_bytes: u64,
        mount_point: PathBuf,
        combined_secret_hex: String,
    },
    /// Attaches an existing VHDX and BitLocker-unlocks it with
    /// `combined_secret_hex`, mounting it at `mount_point` — the unlock
    /// path (ticket 047).
    Attach {
        vhdx_path: PathBuf,
        mount_point: PathBuf,
        combined_secret_hex: String,
    },
    /// Detaches `vhdx_path` (best-effort `Lock-BitLocker` on `mount_point`
    /// first) — the lock path (ticket 046). Never force-detaches if the
    /// underlying `DetachVirtualDisk` call reports the volume busy; the
    /// caller is responsible for the clean-detach-first-then-error UX.
    Detach {
        vhdx_path: PathBuf,
        mount_point: PathBuf,
    },
    /// Detaches whatever VHDX is currently attached at `vhdx_path`
    /// unconditionally, used only for the stale-mount cleanup on launch
    /// (ticket 045/052) — a crashed previous session may have left the
    /// vault mounted. "Nothing was attached" counts as success here, not
    /// a failure.
    ForceDetachStale {
        vhdx_path: PathBuf,
        mount_point: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result")]
pub enum VaultResponse {
    Ok,
    /// `still_in_use = true` distinguishes ticket 046's "another process
    /// has a file open" case from every other failure, so the caller can
    /// show the specific "still in use, retry" UX rather than a generic
    /// error.
    Err { message: String, still_in_use: bool },
}

impl VaultResponse {
    pub fn err(message: impl Into<String>) -> Self {
        Self::Err {
            message: message.into(),
            still_in_use: false,
        }
    }

    pub fn still_in_use(message: impl Into<String>) -> Self {
        Self::Err {
            message: message.into(),
            still_in_use: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_round_trips_through_json() {
        let cmd = VaultCommand::Attach {
            vhdx_path: PathBuf::from(r"C:\vault\vault.vhdx"),
            mount_point: PathBuf::from(r"C:\vault\live"),
            combined_secret_hex: "ab".repeat(32),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let back: VaultCommand = serde_json::from_str(&json).unwrap();
        match back {
            VaultCommand::Attach { combined_secret_hex, .. } => {
                assert_eq!(combined_secret_hex, "ab".repeat(32));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn response_round_trips_through_json() {
        let resp = VaultResponse::still_in_use("a file is open");
        let json = serde_json::to_string(&resp).unwrap();
        let back: VaultResponse = serde_json::from_str(&json).unwrap();
        match back {
            VaultResponse::Err { still_in_use, .. } => assert!(still_in_use),
            _ => panic!("wrong variant"),
        }
    }
}
