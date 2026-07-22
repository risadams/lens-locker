//! The command protocol spoken between the unprivileged main app process
//! and the separately-elevated `vault-helper` subprocess (ticket 040/052).
//!
//! The helper is launched fresh for each attach/detach/create call (never
//! a standing elevated session) via `ShellExecuteExW`'s `"runas"` verb (the
//! only way to get a real UAC elevation prompt from an unprivileged
//! process). **Transport is two temp files, not stdin/stdout** — an
//! elevated process launched through the UAC consent broker does not
//! reliably inherit the parent's standard handles (this is a well-known
//! Windows limitation, not a design preference), so stdio redirection
//! across that boundary isn't dependable. The helper is invoked as
//! `lenslocker-vault-helper.exe <request-file> <response-file>`: it reads
//! one `VaultCommand` as JSON from the request file, performs the
//! operation, and writes one `VaultResponse` as JSON to the response file
//! before exiting — the caller waits on the process handle, then reads the
//! response file. Keeping the protocol to "one command in, one response
//! out" per invocation means there's no long-lived elevated process to
//! reason about the trust boundary of.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::VaultError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum VaultCommand {
    /// Creates a new fixed-size VHDX, formats it NTFS, and enables
    /// BitLocker with `combined_secret_hex` as the password protector —
    /// ticket 043/050's new-vault and migration creation paths. On success
    /// the volume is left **attached and unlocked** (`Enable-BitLocker`
    /// doesn't lock what it just set up) — avoids forcing a second UAC
    /// prompt for the common "use it right after creating it" case. The
    /// caller is responsible for a follow-up `Detach` once it's done
    /// using the freshly-created vault in this session, if it isn't kept
    /// open for immediate use.
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

/// Writes `command` as JSON to `path` — the request-file half of the
/// file-based transport. Used by the unprivileged caller before launching
/// the elevated helper.
pub fn write_command(path: &Path, command: &VaultCommand) -> Result<(), VaultError> {
    let json = serde_json::to_string(command).map_err(|e| VaultError::Protocol(e.to_string()))?;
    std::fs::write(path, json).map_err(VaultError::Io)
}

/// Reads a `VaultCommand` as JSON from `path` — used by `vault-helper`
/// itself.
pub fn read_command(path: &Path) -> Result<VaultCommand, VaultError> {
    let json = std::fs::read_to_string(path).map_err(VaultError::Io)?;
    serde_json::from_str(&json).map_err(|e| VaultError::Protocol(e.to_string()))
}

/// Writes `response` as JSON to `path` — used by `vault-helper` itself.
pub fn write_response(path: &Path, response: &VaultResponse) -> Result<(), VaultError> {
    let json = serde_json::to_string(response).map_err(|e| VaultError::Protocol(e.to_string()))?;
    std::fs::write(path, json).map_err(VaultError::Io)
}

/// Reads a `VaultResponse` as JSON from `path` — the response-file half of
/// the transport. Used by the unprivileged caller after the elevated
/// helper process exits.
pub fn read_response(path: &Path) -> Result<VaultResponse, VaultError> {
    let json = std::fs::read_to_string(path).map_err(VaultError::Io)?;
    serde_json::from_str(&json).map_err(|e| VaultError::Protocol(e.to_string()))
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

    #[test]
    fn command_round_trips_through_a_real_file() {
        let path = std::env::temp_dir().join(format!(
            "lenslocker-vault-protocol-cmd-test-{}.json",
            std::process::id()
        ));
        let cmd = VaultCommand::Detach {
            vhdx_path: PathBuf::from(r"C:\vault\vault.vhdx"),
            mount_point: PathBuf::from(r"C:\vault\live"),
        };
        write_command(&path, &cmd).unwrap();
        let back = read_command(&path).unwrap();
        assert!(matches!(back, VaultCommand::Detach { .. }));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn response_round_trips_through_a_real_file() {
        let path = std::env::temp_dir().join(format!(
            "lenslocker-vault-protocol-resp-test-{}.json",
            std::process::id()
        ));
        write_response(&path, &VaultResponse::Ok).unwrap();
        let back = read_response(&path).unwrap();
        assert!(matches!(back, VaultResponse::Ok));
        std::fs::remove_file(&path).ok();
    }
}
