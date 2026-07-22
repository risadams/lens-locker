//! The separately-elevated helper process for LensLocker's Locking
//! feature (`workplan/LOCK-SPEC.md` §3). Launched fresh (with a UAC
//! prompt) for each attach/detach/create call — never a standing elevated
//! session — mirroring `raw-worker`'s existing precedent of isolating a
//! risky/native operation into its own subprocess rather than growing the
//! main app's privilege or trust surface.
//!
//! Invocation: `lenslocker-vault-helper.exe <request-file> <response-file>`.
//! Reads one `VaultCommand` as JSON from the request file, performs the
//! operation, writes one `VaultResponse` as JSON to the response file,
//! exits. **Not stdin/stdout** — a process launched elevated via the UAC
//! consent broker doesn't reliably inherit the parent's standard handles,
//! so file paths are the dependable transport across that boundary (see
//! `lenslocker_vault::protocol`'s doc comment).

#[cfg(windows)]
mod bitlocker;
#[cfg(windows)]
mod vhd;

use std::path::PathBuf;
use std::process::ExitCode;

use lenslocker_vault::protocol::{read_command, write_response};
use lenslocker_vault::{VaultCommand, VaultResponse};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(request_path), Some(response_path)) = (args.next(), args.next()) else {
        eprintln!(
            "usage: lenslocker-vault-helper.exe <request-file> <response-file>"
        );
        return ExitCode::FAILURE;
    };
    let request_path = PathBuf::from(request_path);
    let response_path = PathBuf::from(response_path);

    let response = match read_command(&request_path) {
        Ok(command) => dispatch(command),
        Err(e) => VaultResponse::err(format!("invalid command: {e}")),
    };

    let is_ok = matches!(response, VaultResponse::Ok);
    if let Err(e) = write_response(&response_path, &response) {
        // Nothing left to report to — the caller is waiting on this
        // process's exit code and the response file it never got. Exit
        // code alone still distinguishes success from failure.
        eprintln!("failed to write response file: {e}");
        return ExitCode::FAILURE;
    }
    if is_ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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

    // Deliberately left attached and unlocked — `Enable-BitLocker` doesn't
    // lock the volume it just set up, so it's already live at this point.
    // Locking+detaching here would just force an immediate second UAC
    // prompt for the extremely common "use the vault right after creating
    // it" case; the caller treats a successful CreateAndEncrypt as already
    // mounted, ready to open the catalog against.
    VaultResponse::Ok
}

#[cfg(windows)]
fn attach(
    vhdx_path: &std::path::Path,
    mount_point: &std::path::Path,
    combined_secret_hex: &str,
) -> VaultResponse {
    // Defensive stale-mount cleanup (ticket 045/052) folded into the
    // normal unlock path, not a separate command — a crashed previous
    // session may have left this VHDX attached, and detaching before a
    // fresh attach keeps unlock to a single UAC prompt rather than two.
    // Ignored: there's nothing stale to clear on the common case.
    let _ = vhd::detach(vhdx_path);

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
