//! Partition/format and BitLocker management via PowerShell.
//!
//! Per ticket 052's finding, no Rust crate wraps BitLocker management (or
//! volume partitioning) at all — implemented by shelling out to
//! Microsoft's own first-party cmdlets rather than hand-rolling COM/WMI
//! (VDS, FVE) interop from scratch, which would be a much larger unsafe/
//! untested surface for comparatively little benefit. Every value that
//! isn't a fixed literal is passed as a separate process argument (`$args`
//! inside the script), never spliced into the script text itself, so
//! there's no command-injection surface even though the values this
//! module actually receives (a disk number, a filesystem path, a hex
//! string) are already safe by construction.

use std::path::Path;
use std::process::Command;

fn run_powershell(script: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .args(args)
        .output()
        .map_err(|e| format!("failed to launch powershell.exe: {e}"))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "powershell command failed (exit {:?}): {stderr}",
            output.status.code()
        ))
    }
}

/// Extracts the trailing disk number from a `\\.\PhysicalDriveN` path, as
/// returned by `vhd::attach`.
pub fn disk_number_from_physical_path(physical_path: &str) -> Result<String, String> {
    physical_path
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("could not parse a disk number out of '{physical_path}'"))
}

/// Initializes the freshly-created (blank) disk GPT, creates a single
/// maximum-size partition, formats it NTFS, and assigns `mount_point` as
/// its access path — `mount_point` must already exist as an empty
/// directory on an NTFS volume (`Add-PartitionAccessPath`'s own
/// requirement); the caller creates it before calling this.
pub fn partition_and_format(disk_number: &str, mount_point: &Path) -> Result<(), String> {
    const SCRIPT: &str = r#"
        $ErrorActionPreference = "Stop"
        $diskNumber = [int]$args[0]
        $mountPoint = $args[1]
        Initialize-Disk -Number $diskNumber -PartitionStyle GPT
        $part = New-Partition -DiskNumber $diskNumber -UseMaximumSize -AssignDriveLetter:$false
        $part | Format-Volume -FileSystem NTFS -Confirm:$false -Force | Out-Null
        Add-PartitionAccessPath -InputObject $part -AccessPath $mountPoint
    "#;
    run_powershell(SCRIPT, &[disk_number, &mount_point.to_string_lossy()])
}

/// Enables BitLocker on the volume at `mount_point` with `password_hex`
/// (ticket 040/041's app-combined secret) as its sole password protector.
///
/// **Milestone L4 release-gate finding, verified against Microsoft's own
/// `Enable-BitLocker`/`Add-BitLockerKeyProtector` reference docs, not
/// assumed**: `-PasswordProtector`/`-Password` and
/// `-RecoveryPasswordProtector`/`-RecoveryKeyProtector` are mutually
/// exclusive parameter sets — a single call can only use one, and this one
/// never requests the latter. A recovery protector is *only* ever added by
/// a separate, explicit `Add-BitLockerKeyProtector` call, which nothing in
/// this codebase makes. The interactive "back up your recovery key to your
/// Microsoft account" screen is part of the separate Settings/Explorer
/// "Turn on BitLocker" GUI wizard — `Enable-BitLocker` the cmdlet is fully
/// headless and has no documented UI-prompt or cloud-escrow behavior of
/// its own. (Automatic AD escrow exists only via a domain-joined Group
/// Policy, inapplicable to this app's personal, non-domain-joined target.)
/// So: no code-level guard was actually needed here — the risk this
/// doc comment originally flagged doesn't exist for a cmdlet-only,
/// GUI-wizard-free invocation path. `LOCK-SPEC.md` §9/§10.
///
/// `-EncryptionMethod XtsAes256` and `-UsedSpaceOnly` added per the same
/// verification pass — Microsoft's own docs recommend `XtsAes256`
/// explicitly (unspecified defaults to the weaker XTS-AES-128), and
/// `-UsedSpaceOnly` is relevant here since a freshly-created fixed-size
/// VHDX is mostly unallocated space that doesn't need encrypting yet.
pub fn enable_bitlocker(mount_point: &Path, password_hex: &str) -> Result<(), String> {
    const SCRIPT: &str = r#"
        $ErrorActionPreference = "Stop"
        $mountPoint = $args[0]
        $secure = ConvertTo-SecureString -String $args[1] -AsPlainText -Force
        Enable-BitLocker -MountPoint $mountPoint -PasswordProtector -Password $secure -EncryptionMethod XtsAes256 -UsedSpaceOnly -SkipHardwareTest
    "#;
    run_powershell(SCRIPT, &[&mount_point.to_string_lossy(), password_hex])
}

/// Unlocks an already-BitLocker-enabled volume at `mount_point` with
/// `password_hex`. A wrong combined secret fails here — the caller maps
/// that failure to the generic "incorrect password or key" UX (ticket
/// 047), never distinguishing which of the two factors was wrong.
pub fn unlock_bitlocker(mount_point: &Path, password_hex: &str) -> Result<(), String> {
    const SCRIPT: &str = r#"
        $ErrorActionPreference = "Stop"
        $mountPoint = $args[0]
        $secure = ConvertTo-SecureString -String $args[1] -AsPlainText -Force
        Unlock-BitLocker -MountPoint $mountPoint -Password $secure
    "#;
    run_powershell(SCRIPT, &[&mount_point.to_string_lossy(), password_hex])
}

/// Best-effort re-lock before detach. Not load-bearing on its own —
/// detaching the VHDX makes the volume inaccessible regardless — but
/// leaves the volume in the expected BitLocker-locked state if anything
/// ever inspects it while still attached.
pub fn lock_bitlocker(mount_point: &Path) -> Result<(), String> {
    const SCRIPT: &str = r#"
        $ErrorActionPreference = "Stop"
        Lock-BitLocker -MountPoint $args[0] -ForceDismount
    "#;
    run_powershell(SCRIPT, &[&mount_point.to_string_lossy()])
}
