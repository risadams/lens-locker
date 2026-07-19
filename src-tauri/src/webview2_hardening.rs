//! WebView2 COM settings hardening — layer 1 of `workplan/SPEC.md` §8's
//! offline-enforcement package (Milestone 6). WebView2 makes two kinds of
//! outbound call **on its own**, independent of anything Tauri's IPC
//! capability/CSP system governs (that system only covers the JS↔Rust
//! bridge and content loading, per `workplan/research/network-enforcement.md`
//! §2):
//!
//! - **SmartScreen** (URL/file reputation checks sent to Microsoft on
//!   navigation), controlled by `ICoreWebView2Settings8::IsReputationCheckingRequired`.
//!   This is an ordinary per-webview setting with a post-creation setter, so
//!   it's applied via Tauri's `with_webview` escape hatch after the webview
//!   exists.
//! - **Crash minidump upload** to Microsoft, controlled by
//!   `ICoreWebView2EnvironmentOptions3::IsCustomCrashReportingEnabled`. This
//!   property has **no post-creation setter** — it can only be set on the
//!   `ICoreWebView2EnvironmentOptions` passed to
//!   `CreateCoreWebView2EnvironmentWithOptions`, before the environment
//!   exists. Tauri's `WebviewWindowBuilder::from_config` normally creates its
//!   own default environment (which leaves this off, i.e. crash minidumps
//!   ARE uploaded by default) the moment the window is built, so this module
//!   builds the environment itself first and hands it to Tauri via
//!   `.with_environment(...)`.
//!
//! Interface names verified against the current WebView2 API surface via the
//! `webview2-com-sys` crate's own generated bindings (`ICoreWebView2Settings8`,
//! `ICoreWebView2EnvironmentOptions3`) rather than assumed from older research —
//! see the Milestone 6 commit message for the verification trail.
//!
//! `webview2-com`/`windows` are not new dependencies in spirit: both are
//! already transitive dependencies of `wry`/`tauri-runtime-wry` at the same
//! resolved version (confirmed via `cargo tree`); this module just gets
//! direct access to interfaces Tauri already links.
//!
//! The workspace denies `unsafe_code` by default (`Cargo.toml`
//! `[workspace.lints]`); this module is the sole, deliberate exception —
//! every COM vtable call below is unavoidably `unsafe` (raw FFI into
//! WebView2), and Tauri's own `with_webview` example uses the identical
//! pattern.

#![allow(unsafe_code)]

use std::path::Path;
use std::sync::mpsc;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2Environment, ICoreWebView2Settings8,
};
use webview2_com::{CoreWebView2EnvironmentOptions, CreateCoreWebView2EnvironmentCompletedHandler};
use windows::Win32::Foundation::{E_POINTER, E_UNEXPECTED};
use windows::core::{HSTRING, Interface, PCWSTR};

/// Build a WebView2 environment with `IsCustomCrashReportingEnabled` set —
/// i.e. WebView2's own minidump-upload-to-Microsoft path disabled. Must run
/// before any webview is created for this app (see module docs): the
/// property has no setter once an environment exists.
///
/// `user_data_folder` is passed through to
/// `CreateCoreWebView2EnvironmentWithOptions` explicitly rather than left
/// empty (wry's own default when no `data_directory` is configured, per
/// `tauri.conf.json`'s window config — this app's doesn't set one). An
/// empty string makes WebView2 default to `<exe_dir>\<exe_name>.WebView2\`,
/// which only works when the exe's own directory is writable by the running
/// user — true in `target\debug`, false once installed to `C:\Program
/// Files` (`installMode: perMachine`), where it fails outright with
/// `HRESULT(0x80070005)` ("Access is denied.") rather than falling back to
/// anywhere writable. The caller passes the app's own local-data directory
/// (writable regardless of install location) instead.
pub fn create_environment(
    user_data_folder: &Path,
) -> Result<ICoreWebView2Environment, Box<dyn std::error::Error>> {
    // wry's own environment creation (`Webview::new_in_hwnd`) calls this
    // before touching any WebView2 COM interface; because this function
    // runs *before* wry ever gets a chance to (we build our own environment
    // ahead of `.build()`), we have to call it ourselves on this thread too
    // — otherwise `CreateCoreWebView2EnvironmentWithOptions` fails with
    // `CO_E_NOTINITIALIZED` ("CoInitialize has not been called."). Ignoring
    // the result matches wry: a second `CoInitializeEx` on an
    // already-initialized thread returns `S_FALSE`, not an error.
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
    }

    let options = CoreWebView2EnvironmentOptions::default();
    // Safety: these setters just write into `CoreWebView2EnvironmentOptions`'s
    // own `UnsafeCell` fields (see webview2-com's options.rs) before the
    // struct is ever shared across threads or handed to WebView2 — no
    // concurrent access is possible at this point.
    unsafe {
        options.set_additional_browser_arguments(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection".to_string(),
        );
        options.set_is_custom_crash_reporting_enabled(true);
    }

    std::fs::create_dir_all(user_data_folder)?;
    let user_data_folder = HSTRING::from(user_data_folder.as_os_str());

    let (tx, rx) = mpsc::channel();
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &user_data_folder,
            &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2EnvironmentOptions::from(
                options,
            ),
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
                move |error_code, environment| {
                    let result = (|| {
                        error_code?;
                        environment.ok_or_else(|| windows::core::Error::from(E_POINTER))
                    })();
                    tx.send(result)
                        .map_err(|_| windows::core::Error::from(E_UNEXPECTED))
                },
            )),
        )?;
    }

    let environment = webview2_com::wait_with_pump(rx)??;
    Ok(environment)
}

/// Disable SmartScreen on the given webview and read the value back from
/// the live `ICoreWebView2Settings8` COM object, so callers get real
/// evidence the setting took effect on the running webview rather than
/// just "the `Set` call didn't error". Returns the read-back value —
/// callers should treat anything other than `Ok(false)` as verification
/// failure.
///
/// `IsReputationCheckingRequired` is a per-webview setting shared by every
/// webview using the same user data folder (this app has exactly one
/// webview).
pub fn disable_smartscreen(
    webview: tauri::webview::PlatformWebview,
) -> windows::core::Result<bool> {
    let controller = webview.controller();
    let core_webview2 = unsafe { controller.CoreWebView2()? };
    let settings = unsafe { core_webview2.Settings()? };
    let settings8: ICoreWebView2Settings8 = settings.cast()?;
    unsafe { settings8.SetIsReputationCheckingRequired(false)? };

    let mut read_back = windows::core::BOOL(1);
    unsafe { settings8.IsReputationCheckingRequired(&mut read_back)? };
    Ok(read_back.as_bool())
}

/// Returns `true` if `webview`'s live WebView2 environment is the exact COM
/// object this module created (by `IUnknown` pointer identity), i.e. real
/// evidence Tauri actually used the hardened environment from
/// [`create_environment`] rather than silently falling back to a default
/// one built with crash reporting left on.
///
/// Takes the expected environment's raw pointer as a plain `usize` (rather
/// than the COM interface itself) because `tauri::WebviewWindow::with_webview`
/// requires its closure to be `Send`, and COM interfaces — being raw
/// `NonNull<c_void>` under the hood — are not.
pub fn is_hardened_environment(
    webview: &tauri::webview::PlatformWebview,
    expected_raw_ptr: usize,
) -> bool {
    let actual = webview.environment();
    actual.as_raw() as usize == expected_raw_ptr
}
