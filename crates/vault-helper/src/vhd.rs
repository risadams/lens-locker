//! Raw Virtual Disk API FFI — `workplan/LOCK-SPEC.md` §3.
//!
//! The workspace denies `unsafe_code` by default (`Cargo.toml`'s lint
//! table); this module is a narrow, justified exception, same rationale as
//! `src-tauri/src/webview2_hardening.rs` and its `free_space_bytes`
//! (`GetDiskFreeSpaceExW`) sibling — there is no safe Rust wrapper for the
//! Virtual Disk API (ticket 052 confirmed no maintained crate covers this
//! cleanly), so the raw Win32 calls are wrapped here behind a safe,
//! narrowly-scoped interface and used nowhere else in the codebase.
#![allow(unsafe_code)]

use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WIN32_ERROR};
use windows::Win32::Storage::Vhd::{
    ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER, ATTACH_VIRTUAL_DISK_FLAG_PERMANENT_LIFETIME,
    ATTACH_VIRTUAL_DISK_PARAMETERS,
    ATTACH_VIRTUAL_DISK_VERSION_1, AttachVirtualDisk, CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION,
    CREATE_VIRTUAL_DISK_PARAMETERS, CREATE_VIRTUAL_DISK_PARAMETERS_0, CREATE_VIRTUAL_DISK_PARAMETERS_0_1,
    CREATE_VIRTUAL_DISK_VERSION_2, CreateVirtualDisk, DETACH_VIRTUAL_DISK_FLAG_NONE, DetachVirtualDisk,
    GetVirtualDiskPhysicalPath, OPEN_VIRTUAL_DISK_FLAG_NONE, OPEN_VIRTUAL_DISK_PARAMETERS,
    OPEN_VIRTUAL_DISK_PARAMETERS_0, OPEN_VIRTUAL_DISK_PARAMETERS_0_1, OPEN_VIRTUAL_DISK_VERSION_2,
    OpenVirtualDisk, VIRTUAL_DISK_ACCESS_ALL, VIRTUAL_DISK_ACCESS_NONE, VIRTUAL_STORAGE_TYPE,
    VIRTUAL_STORAGE_TYPE_DEVICE_VHDX, VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
};
use windows::core::PCWSTR;

fn vhdx_storage_type() -> VIRTUAL_STORAGE_TYPE {
    VIRTUAL_STORAGE_TYPE {
        DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
        VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn check(err: WIN32_ERROR, what: &str) -> Result<(), String> {
    if err.0 == 0 {
        Ok(())
    } else {
        Err(format!("{what} failed: Win32 error {}", err.0))
    }
}

/// Creates a new **fixed-size** VHDX at `path` — `CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION`
/// forces full allocation rather than the dynamically-expanding default,
/// per ticket 040/052's fixed-size decision (dynamic VHDX has a real
/// fragmentation risk at many-small-files scale). Closes the handle
/// before returning — callers that need to immediately attach the disk
/// they just created should call `attach` separately.
pub fn create_fixed_vhdx(path: &Path, size_bytes: u64) -> Result<(), String> {
    let storage_type = vhdx_storage_type();
    let wide = wide_path(path);

    let mut params = CREATE_VIRTUAL_DISK_PARAMETERS {
        Version: CREATE_VIRTUAL_DISK_VERSION_2,
        Anonymous: CREATE_VIRTUAL_DISK_PARAMETERS_0 {
            Version2: CREATE_VIRTUAL_DISK_PARAMETERS_0_1 {
                MaximumSize: size_bytes,
                ..Default::default()
            },
        },
    };

    let mut handle = HANDLE::default();
    let err = unsafe {
        CreateVirtualDisk(
            &storage_type,
            PCWSTR(wide.as_ptr()),
            VIRTUAL_DISK_ACCESS_NONE,
            None,
            CREATE_VIRTUAL_DISK_FLAG_FULL_PHYSICAL_ALLOCATION,
            0,
            &mut params,
            None,
            &mut handle,
        )
    };
    check(err, "CreateVirtualDisk")?;
    unsafe {
        let _ = CloseHandle(handle);
    }
    Ok(())
}

/// Opens `path` and attaches it, with `ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER`
/// so Windows never auto-assigns a drive letter — ticket 045 committed to
/// a folder mount point instead, assigned separately once the volume is
/// formatted/partitioned. Returns the `\\.\PhysicalDriveN` path so the
/// caller can find the disk number for the partition/format step.
pub fn attach(path: &Path) -> Result<String, String> {
    let storage_type = vhdx_storage_type();
    let wide = wide_path(path);

    let mut open_params = OPEN_VIRTUAL_DISK_PARAMETERS {
        Version: OPEN_VIRTUAL_DISK_VERSION_2,
        Anonymous: OPEN_VIRTUAL_DISK_PARAMETERS_0 {
            Version2: OPEN_VIRTUAL_DISK_PARAMETERS_0_1 {
                GetInfoOnly: false.into(),
                ReadOnly: false.into(),
                ResiliencyGuid: windows_core_guid_zero(),
            },
        },
    };

    let mut handle = HANDLE::default();
    let err = unsafe {
        OpenVirtualDisk(
            &storage_type,
            PCWSTR(wide.as_ptr()),
            // Documented Win32 quirk: the access-mask parameter is
            // ignored (and must be VIRTUAL_DISK_ACCESS_NONE) once
            // OPEN_VIRTUAL_DISK_VERSION_2+ params are used — passing
            // VIRTUAL_DISK_ACCESS_ALL here fails with
            // ERROR_INVALID_PARAMETER, confirmed against this real build.
            VIRTUAL_DISK_ACCESS_NONE,
            OPEN_VIRTUAL_DISK_FLAG_NONE,
            Some(&mut open_params),
            &mut handle,
        )
    };
    check(err, "OpenVirtualDisk")?;

    let attach_params = ATTACH_VIRTUAL_DISK_PARAMETERS {
        Version: ATTACH_VIRTUAL_DISK_VERSION_1,
        ..Default::default()
    };
    // Real bug, found via live testing (Milestone L4): without
    // PERMANENT_LIFETIME, the attach succeeded at the raw Win32 level
    // (a valid `\\.\PhysicalDriveN` path) but the disk never appeared in
    // `Get-Disk`/Storage Management's CIM view at all — not a transient
    // enumeration lag (a 10-attempt retry didn't help), the disk was
    // never registered as a first-class manageable object in the first
    // place. Microsoft's own official sample
    // (microsoft/Windows-classic-samples, Hyper-V/Storage/cpp/AttachVirtualDisk.cpp)
    // always passes this flag; this code originally didn't.
    let err = unsafe {
        AttachVirtualDisk(
            handle,
            None,
            ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER | ATTACH_VIRTUAL_DISK_FLAG_PERMANENT_LIFETIME,
            0,
            Some(&attach_params),
            None,
        )
    };
    if let Err(e) = check(err, "AttachVirtualDisk") {
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(e);
    }

    let physical_path = physical_path_of(handle);

    unsafe {
        let _ = CloseHandle(handle);
    }

    physical_path
}

fn physical_path_of(handle: HANDLE) -> Result<String, String> {
    let mut size_in_bytes: u32 = 1024;
    let mut buf: Vec<u16> = vec![0u16; (size_in_bytes / 2) as usize];
    let err = unsafe {
        GetVirtualDiskPhysicalPath(
            handle,
            &mut size_in_bytes,
            windows::core::PWSTR(buf.as_mut_ptr()),
        )
    };
    check(err, "GetVirtualDiskPhysicalPath")?;
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(String::from_utf16_lossy(&buf[..len]))
}

/// Opens and detaches the VHDX at `path`. Never force-closes handles held
/// by other processes — ticket 046's "surface a clear error, don't
/// force-detach" policy is enforced by the caller inspecting the error
/// this returns, not by this function silently retrying or overriding.
pub fn detach(path: &Path) -> Result<(), String> {
    let storage_type = vhdx_storage_type();
    let wide = wide_path(path);

    let mut handle = HANDLE::default();
    let err = unsafe {
        OpenVirtualDisk(
            &storage_type,
            PCWSTR(wide.as_ptr()),
            VIRTUAL_DISK_ACCESS_ALL,
            OPEN_VIRTUAL_DISK_FLAG_NONE,
            None,
            &mut handle,
        )
    };
    check(err, "OpenVirtualDisk (for detach)")?;

    let err = unsafe { DetachVirtualDisk(handle, DETACH_VIRTUAL_DISK_FLAG_NONE, 0) };
    let result = check(err, "DetachVirtualDisk");

    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}

fn windows_core_guid_zero() -> windows::core::GUID {
    windows::core::GUID::zeroed()
}
