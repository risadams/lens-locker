//! The separately-elevated helper process for LensLocker's Locking
//! feature (`workplan/LOCK-SPEC.md` §3). Launched fresh (with a UAC
//! prompt) for each attach/detach/create call — never a standing elevated
//! session — mirroring `raw-worker`'s existing precedent of isolating a
//! risky/native operation into its own subprocess rather than growing the
//! main app's privilege or trust surface.
//!
//! Protocol: reads one `VaultCommand` as JSON on stdin, performs the
//! operation, writes one `VaultResponse` as JSON on stdout, exits.

#[cfg(windows)]
mod bitlocker;
#[cfg(windows)]
mod vhd;

use std::io::{self, Read, Write};
use std::process::ExitCode;

use lenslocker_vault::{VaultCommand, VaultResponse};

fn main() -> ExitCode {
    let mut input = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input) {
        return respond(VaultResponse::err(format!("failed to read command: {e}")));
    }

    let command: VaultCommand = match serde_json::from_str(&input) {
        Ok(c) => c,
        Err(e) => return respond(VaultResponse::err(format!("invalid command: {e}"))),
    };

    let response = dispatch(command);
    respond(response)
}

fn respond(response: VaultResponse) -> ExitCode {
    let json = serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"result":"Err","message":"failed to serialize response","still_in_use":false}"#.into()
    });
    let _ = writeln!(io::stdout(), "{json}");
    match response {
        VaultResponse::Ok => ExitCode::SUCCESS,
        VaultResponse::Err { .. } => ExitCode::FAILURE,
    }
}

#[cfg(windows)]
fn dispatch(command: VaultCommand) -> VaultResponse {
    match command {
        VaultCommand::CreateAndEncrypt {
            vhdx_path,
            size_bytes,
            mount_point,
            combined_secret_hex,
        } => create_and_encrypt(&vhdx_path, size_bytes, &mount_point, &combined_secret_hex),
        VaultCommand::Attach {
            vhdx_path,
            mount_point,
            combined_secret_hex,
        } => attach(&vhdx_path, &mount_point, &combined_secret_hex),
        VaultCommand::Detach {
            vhdx_path,
            mount_point,
        } => detach(&vhdx_path, &mount_point, false),
        VaultCommand::ForceDetachStale {
            vhdx_path,
            mount_point,
        } => detach(&vhdx_path, &mount_point, true),
    }
}

#[cfg(windows)]
fn create_and_encrypt(
    vhdx_path: &std::path::Path,
    size_bytes: u64,
    mount_point: &std::path::Path,
    combined_secret_hex: &str,
) -> VaultResponse {
    if let Err(e) = std::fs::create_dir_all(mount_point) {
        return VaultResponse::err(format!("could not create mount point directory: {e}"));
    }

    if let Err(e) = vhd::create_fixed_vhdx(vhdx_path, size_bytes) {
        return VaultResponse::err(e);
    }

    let physical_path = match vhd::attach(vhdx_path) {
        Ok(p) => p,
        Err(e) => return VaultResponse::err(e),
    };

    let disk_number = match bitlocker::disk_number_from_physical_path(&physical_path) {
        Ok(n) => n,
        Err(e) => return VaultResponse::err(e),
    };

    if let Err(e) = bitlocker::partition_and_format(&disk_number, mount_point) {
        return VaultResponse::err(e);
    }

    if let Err(e) = bitlocker::enable_bitlocker(mount_point, combined_secret_hex) {
        return VaultResponse::err(e);
    }

    // Provisioning is done as a distinct step from mounting for use — the
    // caller issues a separate Attach command when it actually wants the
    // vault live. See vault crate/protocol.rs's doc comment.
    let _ = bitlocker::lock_bitlocker(mount_point);
    if let Err(e) = vhd::detach(vhdx_path) {
        return VaultResponse::err(e);
    }

    VaultResponse::Ok
}

#[cfg(windows)]
fn attach(
    vhdx_path: &std::path::Path,
    mount_point: &std::path::Path,
    combined_secret_hex: &str,
) -> VaultResponse {
    if let Err(e) = vhd::attach(vhdx_path) {
        return VaultResponse::err(e);
    }
    if let Err(e) = bitlocker::unlock_bitlocker(mount_point, combined_secret_hex) {
        // Deliberately generic from the caller's perspective (ticket
        // 047) — this covers both "wrong password" and "tampered/wrong
        // keypair file produced the wrong combined secret," and the two
        // are indistinguishable by design.
        let _ = vhd::detach(vhdx_path);
        return VaultResponse::err(e);
    }
    VaultResponse::Ok
}

#[cfg(windows)]
fn detach(vhdx_path: &std::path::Path, mount_point: &std::path::Path, force_stale: bool) -> VaultResponse {
    let _ = bitlocker::lock_bitlocker(mount_point);

    match vhd::detach(vhdx_path) {
        Ok(()) => VaultResponse::Ok,
        Err(e) if force_stale => {
            // Best-effort cleanup of a crash-orphaned mount (ticket
            // 045/052) — "nothing was attached" is success here, not a
            // real failure.
            if e.to_lowercase().contains("not") {
                VaultResponse::Ok
            } else {
                VaultResponse::err(e)
            }
        }
        Err(e) => VaultResponse::still_in_use(e),
    }
}

#[cfg(not(windows))]
fn dispatch(_command: VaultCommand) -> VaultResponse {
    // LensLocker ships Windows-only (workplan/SPEC.md); this stub exists
    // only so the crate still type-checks on other hosts.
    VaultResponse::err("lenslocker-vault-helper only runs on Windows")
}
