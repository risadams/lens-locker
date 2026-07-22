//! Launches `lenslocker-vault-helper.exe` elevated (a real UAC prompt) and
//! waits for it to finish — `workplan/LOCK-SPEC.md` §3's elevated-helper
//! design, mirroring `raw-worker`'s existing precedent of isolating a
//! risky/native operation into its own subprocess.
//!
//! `ShellExecuteExW`'s `"runas"` verb is the only way to get a real UAC
//! consent prompt from an unprivileged process — `Command::spawn` cannot
//! elevate a child process. The launched process does not reliably inherit
//! this process's stdio (a real Windows limitation), which is exactly why
//! `lenslocker_vault::protocol` uses a file-based request/response
//! transport instead of stdin/stdout — see that module's doc comment.
//!
//! **Not independently verifiable from here**: everything past the
//! `ShellExecuteExW` call requires a human to approve (or deny) the UAC
//! prompt. This module type-checks and the request/response file handling
//! is exercised the same way `vault-helper`'s own non-elevated smoke test
//! was, but the live elevated round-trip needs manual verification.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use lenslocker_vault::protocol::{read_response, write_command};
use lenslocker_vault::{VaultCommand, VaultResponse};

use crate::{CmdError, CmdResult};

/// How long to wait for the elevated helper before giving up — generous
/// enough for a `CreateAndEncrypt` on a large fixed-size VHDX (disk-speed
/// bound), but not infinite, so a wedged helper doesn't hang the app
/// forever. Real timing data comes from Milestone L4's release-gate
/// measurement (`LOCK-SPEC.md` §3) — this is a starting value, not a
/// benchmarked one.
const HELPER_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Resolves `lenslocker-vault-helper.exe`'s path — a sibling of the
/// currently-running executable, same convention as
/// [`crate::get_licenses_file_path`]. Packaging/installer work to ensure
/// both binaries actually ship together is future work, same open item
/// this project already has for `raw-worker`.
fn helper_exe_path() -> CmdResult<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or(CmdError::ExeDirUnresolvable)?;
    Ok(dir.join("lenslocker-vault-helper.exe"))
}

/// Runs `command` via the elevated helper and returns its response.
/// Blocks the calling thread until the helper exits or `HELPER_TIMEOUT`
/// elapses — callers on the Tauri async command path must run this via
/// `spawn_blocking`, same discipline `import_directory`/`get_full_preview`
/// already follow for other long-running native work
/// (`workplan/SPEC.md`/`CLAUDE.md`'s architecture notes).
pub fn run_elevated(command: &VaultCommand) -> CmdResult<VaultResponse> {
    let helper = helper_exe_path()?;
    if !helper.is_file() {
        return Err(CmdError::Vault(format!(
            "lenslocker-vault-helper.exe not found at {}",
            helper.display()
        )));
    }

    let temp_dir = std::env::temp_dir();
    let unique = format!("{}-{}", std::process::id(), fastrand_u64());
    let request_path = temp_dir.join(format!("lenslocker-vault-req-{unique}.json"));
    let response_path = temp_dir.join(format!("lenslocker-vault-resp-{unique}.json"));

    write_command(&request_path, command).map_err(|e| CmdError::Vault(e.to_string()))?;

    let result = launch_and_wait(&helper, &request_path, &response_path);

    // Best-effort cleanup regardless of outcome — these are scratch files,
    // never the vault's own data.
    std::fs::remove_file(&request_path).ok();

    let outcome = match result {
        Ok(()) => read_response(&response_path).map_err(|e| CmdError::Vault(e.to_string())),
        Err(e) => Err(e),
    };
    std::fs::remove_file(&response_path).ok();
    outcome
}

#[cfg(windows)]
fn launch_and_wait(helper: &Path, request_path: &Path, response_path: &Path) -> CmdResult<()> {
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::WaitForSingleObject;
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::core::{HSTRING, PCWSTR};

    let parameters = format!(
        "\"{}\" \"{}\"",
        request_path.display(),
        response_path.display()
    );
    let file = HSTRING::from(helper.as_os_str());
    let verb = HSTRING::from("runas");
    let params = HSTRING::from(parameters);

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: 1, // SW_SHOWNORMAL
        ..Default::default()
    };

    unsafe {
        if let Err(e) = ShellExecuteExW(&mut info) {
            return Err(if e.code() == ERROR_CANCELLED.to_hresult() {
                CmdError::Vault("elevation was cancelled".into())
            } else {
                CmdError::Vault(format!("failed to launch the elevated helper: {e}"))
            });
        }
    }

    let process: HANDLE = info.hProcess;
    if process.is_invalid() {
        return Err(CmdError::Vault(
            "elevated helper launched but returned no process handle".into(),
        ));
    }

    let wait_result = unsafe { WaitForSingleObject(process, HELPER_TIMEOUT.as_millis() as u32) };
    let _ = unsafe { CloseHandle(process) };

    match wait_result {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(CmdError::Vault(
            "the elevated helper did not finish in time".into(),
        )),
        _ => Err(CmdError::Vault(
            "failed while waiting for the elevated helper".into(),
        )),
    }
}

#[cfg(not(windows))]
fn launch_and_wait(_helper: &Path, _request_path: &Path, _response_path: &Path) -> CmdResult<()> {
    // LensLocker ships Windows-only (workplan/SPEC.md); this stub exists
    // only so the crate still type-checks on other hosts.
    Err(CmdError::Vault(
        "vault elevation is only supported on Windows".into(),
    ))
}

/// A tiny, dependency-free unique-suffix generator for temp file names —
/// doesn't need to be cryptographically random, just distinct across
/// concurrent/rapid invocations on the same machine.
fn fastrand_u64() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
