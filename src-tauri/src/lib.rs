//! LensLocker Tauri app shell: real commands binding the domain crates to
//! the UI, per workplan/SPEC.md §2/§9, Milestone 5 — and, as of Milestone
//! 5.5, first-run vault setup.
//!
//! **Library location — no more hardcoded default.** Milestone 5's own
//! build session flagged "where does the library live" as an unresolved
//! judgment call and defaulted to `<app-data-dir>/library`. That turned out
//! to be a real bug, not a placeholder: AppData is a small system-drive
//! folder, actively wrong for a large personal photo library. Per
//! workplan/SPEC.md's Milestone 5.5 section, **the app must never silently
//! default the library location anywhere, under any circumstance.**
//!
//! The fix: a tiny bootstrap config file (see [`BootstrapConfig`]) living in
//! the ordinary Tauri app-config directory — outside the library itself,
//! since the app needs to know where the library is before it can even open
//! the catalog inside it — records only the chosen library path. On every
//! launch, [`load_initial_library_state`] reads it; if it's missing, or the
//! path it names isn't a reachable directory, the app starts in
//! [`LibraryState::NeedsSetup`] rather than falling back to any default.
//! The frontend calls `check_library_status` on boot and shows the
//! first-run screen for that case, driving `pick_library_folder` /
//! `inspect_library_folder` / `create_library` / `open_existing_library` to
//! get a live library, at which point `AppState` is swapped in — no
//! restart required.
//!
//! **State**: `AppState` — one shared `rusqlite::Connection` behind a
//! `Mutex`, matching every prior milestone's single-connection-per-library
//! pattern — is now itself wrapped in [`LibraryState`], managed behind an
//! outer `Mutex`, since a live library is no longer guaranteed to exist for
//! the whole life of the process.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use lenslocker_catalog::{GridImage, ImageFilters, SortOrder};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

#[cfg(windows)]
mod webview2_hardening;

struct AppState {
    conn: Mutex<Connection>,
    paths: lenslocker_import::LibraryPaths,
    library_id: i64,
}

/// Guards against two `import_directory` calls running concurrently.
///
/// A real bug, not just a hypothetical: `import_directory` holds
/// `AppState.conn`'s mutex (a plain `std::sync::Mutex`, not an async one)
/// for its entire — potentially long — synchronous, CPU-heavy run (decode,
/// hash, convert, write thumbnails/previews for every file). Running that
/// *inline* inside an `async fn` command, on Tauri's shared tokio worker
/// pool, means a second concurrent import doesn't just wait politely: it
/// parks a worker thread on `.lock()`, and if enough ordinary commands
/// (`list_images`, `list_review_queue`, …) pile up doing the same while
/// that pool is small, the whole app can stop processing IPC messages
/// entirely — observed as a genuine Windows "Application Hang," not a
/// graceful error. Making a second import impossible to *start* (rather
/// than just visually discouraged via a disabled frontend button, which a
/// user can route around by reopening the modal) removes the contention at
/// its source. See also `import_directory`'s use of
/// `tauri::async_runtime::spawn_blocking`, which keeps a single import from
/// starving the worker pool even on its own.
///
/// Also carries `cancel_requested`: clicking Cancel in the import modal
/// used to only hide the frontend UI, leaving the backend import running
/// (still holding `AppState.conn`'s mutex) with no way to actually stop it
/// — the exact trap that produced the worker-pool contention above in
/// practice, since the frontend has no way to tell a second import attempt
/// apart from "the first one is stuck." `cancel_import` sets this flag;
/// `import_directory`'s per-file callback checks it and stops the walk
/// early, safe by the crate's own crash-safety design (see
/// `lenslocker_import::import_directory`'s doc) — a canceled import needs
/// no special cleanup, it's resumable exactly like a killed process would
/// leave it.
#[derive(Default)]
struct ImportLock {
    running: std::sync::atomic::AtomicBool,
    cancel_requested: std::sync::atomic::AtomicBool,
}

/// RAII handle on a successful [`ImportLock`] acquisition — releases
/// automatically on drop, including on early return via `?`, so there's no
/// path that leaves the lock held after `import_directory` exits.
struct ImportGuard<'a>(&'a ImportLock);

impl ImportLock {
    fn try_acquire(&self) -> Option<ImportGuard<'_>> {
        self.running
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .ok()
            .map(|_| {
                // Clear any stale request from a previous run — this run
                // hasn't been asked to cancel yet.
                self.cancel_requested
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                ImportGuard(self)
            })
    }

    fn is_cancel_requested(&self) -> bool {
        self.cancel_requested
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for ImportGuard<'_> {
    fn drop(&mut self) {
        self.0
            .running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Whether a live library is available yet. Replaces the Milestone 5
/// assumption that `AppState` always exists — a true first run, or a
/// previously-configured library whose path is no longer reachable (e.g. an
/// external drive unplugged), are both legitimate startup states now.
enum LibraryState {
    Ready(AppState),
    NeedsSetup {
        /// Set when a bootstrap config *was* found but its recorded path
        /// couldn't be opened — lets the frontend show a "we couldn't find
        /// your previous vault" banner instead of a bare first-run screen.
        /// `None` for a genuine first run (no config file at all).
        unreachable_path: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
enum CmdError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Import(#[from] lenslocker_import::ImportError),
    #[error(transparent)]
    Xmp(#[from] lenslocker_xmp::XmpError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("image {0} not found")]
    ImageNotFound(i64),
    #[error("no folder was chosen")]
    NoFolderChosen,
    #[error("a merge action requires keeper_id")]
    MissingKeeper,
    #[error("no library is configured yet")]
    LibraryNotConfigured,
    #[error("an import is already in progress")]
    ImportAlreadyRunning,
    #[error("background task panicked: {0}")]
    TaskPanicked(String),
    #[error(
        "a LensLocker library already exists at this location — open it instead of creating a new one"
    )]
    LibraryAlreadyExists,
    #[error("no LensLocker library was found at this location")]
    LibraryNotFound,
    #[error("could not set up the catalog database: {0}")]
    Migration(String),
    #[error("could not determine free disk space: {0}")]
    DiskSpace(String),
    #[error("could not save the vault location: {0}")]
    Bootstrap(String),
}

// Tauri commands need their error type to serialize across the IPC bridge;
// the message text is all the frontend needs (it's surfaced via a toast,
// not programmatically branched on).
impl Serialize for CmdError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

type CmdResult<T> = Result<T, CmdError>;

// ── DTOs — the wire shape the frontend actually consumes ────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GridImageDto {
    id: i64,
    thumbnail_path: Option<String>,
    capture_date: Option<String>,
    tags: Vec<String>,
    verified: bool,
}

impl From<GridImage> for GridImageDto {
    fn from(g: GridImage) -> Self {
        Self {
            id: g.id,
            thumbnail_path: g.thumbnail_path,
            capture_date: g.capture_date,
            tags: g.tags,
            verified: g.verified,
        }
    }
}

#[derive(Debug, Serialize)]
struct ListImagesResult {
    items: Vec<GridImageDto>,
    total: i64,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct FiltersDto {
    #[serde(default)]
    date_from: Option<String>,
    #[serde(default)]
    date_to: Option<String>,
    #[serde(default)]
    formats: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
}

impl From<FiltersDto> for ImageFilters {
    fn from(f: FiltersDto) -> Self {
        Self {
            date_from: f.date_from,
            date_to: f.date_to,
            formats: f.formats,
            sources: f.sources,
            tags: f.tags,
        }
    }
}

fn parse_sort(sort: &str) -> SortOrder {
    match sort {
        "captured-asc" => SortOrder::CapturedAsc,
        "imported-desc" => SortOrder::ImportedDesc,
        "filename-asc" => SortOrder::FilenameAsc,
        "size-desc" => SortOrder::SizeDesc,
        _ => SortOrder::CapturedDesc,
    }
}

/// Every command that touches the catalog needs a live `AppState`; this is
/// the single place that maps "no library configured yet" to a clear error
/// instead of a panic. The frontend never calls these commands before
/// `check_library_status` reports ready, so in practice this is a
/// belt-and-braces guard, not a path users hit in normal use.
fn with_ready<T>(
    state: &tauri::State<Mutex<LibraryState>>,
    f: impl FnOnce(&AppState) -> CmdResult<T>,
) -> CmdResult<T> {
    let guard = state.lock().unwrap();
    match &*guard {
        LibraryState::Ready(app_state) => f(app_state),
        LibraryState::NeedsSetup { .. } => Err(CmdError::LibraryNotConfigured),
    }
}

/// A native folder-picker builder, parented to the main window.
///
/// `tauri_plugin_dialog`'s `FileDialogBuilder` has `parent: None` by
/// default — `app.dialog().file()` alone does *not* make the dialog
/// application-modal to any window. On Windows that means the picker can
/// end up behind the main window with no OS-enforced link between the two:
/// the user can keep interacting with (and closing) LensLocker's own modals
/// while the real native dialog is still open elsewhere, waiting on a
/// choice that never comes — the backend command stays blocked on it
/// forever, and any UI state gated on that command (e.g. a disabled
/// "Choose Folder…" button) never recovers. Every `pick_folder` call must
/// go through this helper instead of `app.dialog().file()` directly.
fn file_dialog(app: &tauri::AppHandle) -> tauri_plugin_dialog::FileDialogBuilder<tauri::Wry> {
    use tauri_plugin_dialog::DialogExt;
    let builder = app.dialog().file();
    match app.get_webview_window("main") {
        Some(window) => builder.set_parent(&window),
        None => builder,
    }
}

#[tauri::command]
fn list_images(
    state: tauri::State<Mutex<LibraryState>>,
    filters: FiltersDto,
    sort: String,
    search: Option<String>,
    offset: i64,
    limit: i64,
) -> CmdResult<ListImagesResult> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let (items, total) = lenslocker_catalog::list_images(
            &conn,
            &filters.into(),
            parse_sort(&sort),
            search.as_deref(),
            offset,
            limit,
        )?;
        Ok(ListImagesResult {
            items: items.into_iter().map(Into::into).collect(),
            total,
        })
    })
}

/// A tag's provenance, wire shape for the drawer's review UI (Milestone
/// ML-4) — `confidence`/`reviewState` are only meaningful when
/// `source == "auto"`, both `null` for a manually-typed tag.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TagDto {
    name: String,
    source: String,
    confidence: Option<f64>,
    review_state: Option<String>,
}

impl From<lenslocker_catalog::TagWithProvenance> for TagDto {
    fn from(t: lenslocker_catalog::TagWithProvenance) -> Self {
        Self { name: t.name, source: t.source, confidence: t.confidence, review_state: t.review_state }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageDetailDto {
    id: i64,
    filename: String,
    original_format: String,
    stored_format: String,
    conversion_status: String,
    capture_date: Option<String>,
    camera_make: Option<String>,
    camera_model: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    original_hash_hex: String,
    file_size_bytes: i64,
    stored_path: String,
    tags: Vec<TagDto>,
    first_imported_at: String,
}

#[tauri::command]
fn get_image_detail(
    state: tauri::State<Mutex<LibraryState>>,
    id: i64,
) -> CmdResult<ImageDetailDto> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let d =
            lenslocker_catalog::get_image_detail(&conn, id)?.ok_or(CmdError::ImageNotFound(id))?;
        Ok(ImageDetailDto {
            id: d.id,
            filename: d.filename,
            original_format: d.original_format,
            stored_format: d.stored_format,
            conversion_status: d.conversion_status,
            capture_date: d.capture_date,
            camera_make: d.camera_make,
            camera_model: d.camera_model,
            width: d.width,
            height: d.height,
            original_hash_hex: d.original_hash_hex,
            file_size_bytes: d.file_size_bytes,
            stored_path: d.stored_path,
            tags: d.tags.into_iter().map(TagDto::from).collect(),
            first_imported_at: d.first_imported_at,
        })
    })
}

/// Renders `id`'s stored blob as a full-resolution, browser-displayable
/// JPEG and returns it as a `data:` URL — generated fresh on every call,
/// nothing is written to disk (see `lenslocker_import::render_full_preview_bytes`'s
/// doc for why: an earlier design cached this to disk at import time and it
/// roughly doubled the vault's footprint for photos nobody ever opened
/// full-size). Run via `spawn_blocking` since decode+encode is real,
/// synchronous CPU work — same reasoning as `import_directory`'s use of it.
/// `Ok(None)` if the blob can't be decoded (RAW, or missing/corrupt); the
/// frontend falls back to the grid thumbnail in that case.
#[tauri::command]
async fn get_full_preview(app: tauri::AppHandle, id: i64) -> CmdResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        with_ready(&app.state::<Mutex<LibraryState>>(), |app_state| {
            let conn = app_state.conn.lock().unwrap();
            let bytes = lenslocker_import::render_full_preview_bytes(&conn, id)?;
            Ok(bytes.map(|b| format!("data:image/jpeg;base64,{}", BASE64_STANDARD.encode(b))))
        })
    })
    .await
    .map_err(|e| CmdError::TaskPanicked(e.to_string()))?
}

#[tauri::command]
fn add_tag(state: tauri::State<Mutex<LibraryState>>, image_id: i64, tag: String) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lenslocker_catalog::add_tag(&conn, image_id, &tag)?;
        lenslocker_xmp::sync_sidecar(&conn, image_id)?;
        Ok(())
    })
}

#[tauri::command]
fn remove_tag(
    state: tauri::State<Mutex<LibraryState>>,
    image_id: i64,
    tag: String,
) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lenslocker_catalog::remove_tag(&conn, image_id, &tag)?;
        lenslocker_xmp::sync_sidecar(&conn, image_id)?;
        Ok(())
    })
}

/// Flips an auto-tag's `review_state` to `confirmed` (ML-SPEC.md §4/§5) —
/// grants full visual parity with a manual tag without rewriting its
/// `source`, so re-scoring later still knows this one came from the model.
#[tauri::command]
fn confirm_auto_tag(state: tauri::State<Mutex<LibraryState>>, image_id: i64, tag: String) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lenslocker_catalog::confirm_auto_tag(&conn, image_id, &tag)?;
        lenslocker_xmp::sync_sidecar(&conn, image_id)?;
        Ok(())
    })
}

/// Removes a tag and records the rejection (`rejected_tags`) so it's never
/// silently re-suggested by a later re-score (ML-SPEC.md §5) — the
/// auto-tag counterpart to [`remove_tag`], which is the general
/// "delete this tag" a human can also use on a manual tag without that
/// "don't re-suggest" memory.
#[tauri::command]
fn reject_auto_tag(state: tauri::State<Mutex<LibraryState>>, image_id: i64, tag: String) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lenslocker_catalog::reject_tag(&conn, image_id, &tag)?;
        lenslocker_xmp::sync_sidecar(&conn, image_id)?;
        Ok(())
    })
}

#[derive(Debug, Serialize)]
struct TagCountDto {
    name: String,
    count: i64,
}

#[tauri::command]
fn list_tags(state: tauri::State<Mutex<LibraryState>>) -> CmdResult<Vec<TagCountDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::list_tags(&conn)?
            .into_iter()
            .map(|t| TagCountDto {
                name: t.name,
                count: t.count,
            })
            .collect())
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceCountDto {
    source_root: String,
    count: i64,
}

#[tauri::command]
fn list_sources(state: tauri::State<Mutex<LibraryState>>) -> CmdResult<Vec<SourceCountDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::list_sources(&conn)?
            .into_iter()
            .map(|s| SourceCountDto {
                source_root: s.source_root,
                count: s.count,
            })
            .collect())
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewQueueEntryDto {
    queue_id: i64,
    hamming_distance: i64,
    image_a: GridImageDto,
    image_b: GridImageDto,
}

#[tauri::command]
fn list_review_queue(
    state: tauri::State<Mutex<LibraryState>>,
) -> CmdResult<Vec<ReviewQueueEntryDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::list_review_queue(&conn)?
            .into_iter()
            .map(|e| ReviewQueueEntryDto {
                queue_id: e.queue_id,
                hamming_distance: e.hamming_distance,
                image_a: e.image_a.into(),
                image_b: e.image_b.into(),
            })
            .collect())
    })
}

#[tauri::command]
fn resolve_review_pair(
    state: tauri::State<Mutex<LibraryState>>,
    queue_id: i64,
    action: String,
    keeper_id: Option<i64>,
) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let action = match action.as_str() {
            "merge" => lenslocker_import::ReviewAction::Merge {
                keeper_id: keeper_id.ok_or(CmdError::MissingKeeper)?,
            },
            _ => lenslocker_import::ReviewAction::Dismiss,
        };
        lenslocker_import::resolve_review_pair(&conn, &app_state.paths, queue_id, action)?;
        Ok(())
    })
}

#[tauri::command]
fn copy_file_path(state: tauri::State<Mutex<LibraryState>>, id: i64) -> CmdResult<String> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let d =
            lenslocker_catalog::get_image_detail(&conn, id)?.ok_or(CmdError::ImageNotFound(id))?;
        Ok(d.stored_path)
    })
}

/// Emitted to the `import-progress` frontend event as each source file is
/// processed, so the import modal can show "X of Y" / a progress bar rather
/// than sitting on an indefinite "Importing…" — `total` is a plain
/// filesystem walk done up front (`count_importable_files`), not the
/// import's own lazy `walkdir` traversal, since that one doesn't know its
/// total until it's finished.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportProgressPayload {
    current: usize,
    total: usize,
    imported: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportResultDto {
    imported: usize,
    canceled: bool,
}

/// Sets the flag `import_directory`'s per-file callback checks after every
/// file, stopping the walk early — safe with no cleanup needed, since a
/// canceled import is resumable exactly like one left behind by a killed
/// process (see `lenslocker_import::import_directory`'s doc). A no-op if no
/// import is currently running. Cannot close a native folder-picker dialog
/// that hasn't resolved yet (there's no API for that here) — it only stops
/// an already-running import loop, which is the case the frontend's Cancel
/// button actually needs to handle.
#[tauri::command]
fn cancel_import(import_lock: tauri::State<ImportLock>) {
    import_lock
        .cancel_requested
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

#[tauri::command]
async fn import_directory(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<LibraryState>>,
    import_lock: tauri::State<'_, ImportLock>,
) -> CmdResult<ImportResultDto> {
    // Fail fast, before ever popping a native dialog, if there's no live
    // library to import into.
    if !matches!(&*state.lock().unwrap(), LibraryState::Ready(_)) {
        return Err(CmdError::LibraryNotConfigured);
    }
    // Held for this whole command, across both blocking sections below —
    // see ImportLock's doc for why a second concurrent import must be
    // impossible to *start*, not just discouraged by a disabled button.
    let _guard = import_lock
        .try_acquire()
        .ok_or(CmdError::ImportAlreadyRunning)?;

    // The folder-picker wait and the import loop below are both long,
    // fully synchronous blocking work (an indefinite wait on user input,
    // then CPU-heavy decode/hash/convert per file) — run on Tauri's
    // dedicated blocking-task pool via `spawn_blocking` rather than inline
    // on the async command's own worker thread, so neither one can starve
    // the pool other commands (list_images, list_review_queue, …) share.
    let dialog_app = app.clone();
    let source_root: PathBuf =
        tauri::async_runtime::spawn_blocking(move || -> CmdResult<PathBuf> {
            let (tx, rx) = std::sync::mpsc::channel();
            file_dialog(&dialog_app).pick_folder(move |folder| {
                let _ = tx.send(folder);
            });
            let folder = rx
                .recv()
                .map_err(|_| CmdError::NoFolderChosen)?
                .ok_or(CmdError::NoFolderChosen)?;
            folder.into_path().map_err(|_| CmdError::NoFolderChosen)
        })
        .await
        .map_err(|e| CmdError::TaskPanicked(e.to_string()))??;

    let total = lenslocker_import::count_importable_files(&source_root)?;
    let _ = app.emit(
        "import-progress",
        ImportProgressPayload {
            current: 0,
            total,
            imported: 0,
        },
    );

    let import_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        with_ready(&import_app.state::<Mutex<LibraryState>>(), |app_state| {
            let conn = app_state.conn.lock().unwrap();
            let batch_id = lenslocker_import::start_or_resume_batch(
                &conn,
                app_state.library_id,
                &source_root,
            )?;
            let conversion_enabled =
                lenslocker_import::conversion_enabled(&conn, app_state.library_id)?;
            let ctx = lenslocker_import::ImportContext {
                conn: &conn,
                paths: &app_state.paths,
                library_id: app_state.library_id,
                batch_id,
                conversion_enabled,
            };

            let cancel_flag = import_app.state::<ImportLock>();
            let mut current = 0usize;
            let mut imported = 0usize;
            let mut canceled = false;
            lenslocker_import::import_directory(&ctx, &source_root, |_path, outcome| {
                current += 1;
                if matches!(outcome, lenslocker_import::FileOutcome::Imported { .. }) {
                    imported += 1;
                }
                let _ = import_app.emit(
                    "import-progress",
                    ImportProgressPayload {
                        current,
                        total,
                        imported,
                    },
                );
                if cancel_flag.is_cancel_requested() {
                    canceled = true;
                    return false;
                }
                true
            })?;

            Ok(ImportResultDto { imported, canceled })
        })
    })
    .await
    .map_err(|e| CmdError::TaskPanicked(e.to_string()))?
}

#[tauri::command]
async fn export_image(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<LibraryState>>,
    id: i64,
) -> CmdResult<String> {
    if !matches!(&*state.lock().unwrap(), LibraryState::Ready(_)) {
        return Err(CmdError::LibraryNotConfigured);
    }

    let (tx, rx) = std::sync::mpsc::channel();
    file_dialog(&app).pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx
        .recv()
        .map_err(|_| CmdError::NoFolderChosen)?
        .ok_or(CmdError::NoFolderChosen)?;
    let dest_dir: PathBuf = folder.into_path().map_err(|_| CmdError::NoFolderChosen)?;

    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let dest = lenslocker_import::export_image(&conn, id, &dest_dir)?;
        Ok(dest.to_string_lossy().into_owned())
    })
}

// ── Settings (workplan/SPEC.md §5.5, Milestone 5.5) ──────────────────────
//
// `hamming_threshold`/`retention_days` were both decided as user-tunable
// (tickets 011, 005) but never given a UI until now.

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSettingsDto {
    hamming_threshold: i64,
    retention_days: i64,
}

#[tauri::command]
fn get_app_settings(state: tauri::State<Mutex<LibraryState>>) -> CmdResult<AppSettingsDto> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let s = lenslocker_catalog::get_app_settings(&conn)?;
        Ok(AppSettingsDto {
            hamming_threshold: s.hamming_threshold,
            retention_days: s.retention_days,
        })
    })
}

#[tauri::command]
fn update_app_settings(
    state: tauri::State<Mutex<LibraryState>>,
    hamming_threshold: i64,
    retention_days: i64,
) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        // This command has no UI concept of the tag-confidence thresholds
        // yet (Milestone ML-2 added them; a settings UI is Milestone
        // ML-6's job) — read-then-write so this update never silently
        // resets them back to their schema defaults.
        let current = lenslocker_catalog::get_app_settings(&conn)?;
        lenslocker_catalog::update_app_settings(
            &conn,
            lenslocker_catalog::AppSettings {
                hamming_threshold,
                retention_days,
                ..current
            },
        )?;
        Ok(())
    })
}

// ── First-run vault setup (workplan/SPEC.md's Milestone 5.5 section) ─────

/// The bootstrap config file's on-disk shape — deliberately just a path
/// string. It lives outside the library (an ordinary Tauri app-config
/// location) since the app needs to know where the library is *before* it
/// can open the catalog database inside it.
#[derive(Debug, Serialize, Deserialize)]
struct BootstrapConfig {
    library_path: String,
}

fn bootstrap_config_path(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("bootstrap.json")
}

fn read_bootstrap_config(config_path: &Path) -> Option<String> {
    let bytes = std::fs::read(config_path).ok()?;
    let config: BootstrapConfig = serde_json::from_slice(&bytes).ok()?;
    Some(config.library_path)
}

fn write_bootstrap_config(app: &tauri::AppHandle, root: &Path) -> CmdResult<()> {
    write_bootstrap_config_at(&bootstrap_config_path(app), root)
}

/// The actual read/modify/write logic, factored out from [`write_bootstrap_config`]
/// so it's testable without a live `AppHandle`.
fn write_bootstrap_config_at(config_path: &Path, root: &Path) -> CmdResult<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let config = BootstrapConfig {
        library_path: root.to_string_lossy().into_owned(),
    };
    let json =
        serde_json::to_string_pretty(&config).map_err(|e| CmdError::Bootstrap(e.to_string()))?;
    std::fs::write(config_path, json)?;
    Ok(())
}

/// Opens (migrating if needed) the catalog at `root` and ensures its
/// `libraries` row exists — the shared plumbing behind both "app boots with
/// a previously-configured library" and "user picks an existing vault in
/// the first-run screen." Not used for the "create a brand-new vault"
/// path — that needs [`lenslocker_import::create_library_row`] instead, to
/// set `conversion_enabled` at creation per ticket 009.
fn try_init_state(root: &Path) -> CmdResult<AppState> {
    let paths = lenslocker_import::LibraryPaths::new(root);
    std::fs::create_dir_all(root)?;
    let mut conn = Connection::open(paths.catalog_db())?;
    lenslocker_catalog::migrate(&mut conn).map_err(|e| CmdError::Migration(e.to_string()))?;
    let library_id = lenslocker_import::ensure_library(&conn, root)?;
    // Launch-only retention sweep (workplan/SPEC.md §3).
    let _ = lenslocker_import::sweep_expired_quarantine(&conn);
    // Launch-only cleanup: reclaim any preview_full files/rows left behind
    // by the old eager-generation design (see `sweep_stale_previews`'s doc).
    let _ = lenslocker_import::sweep_stale_previews(&conn);
    // Launch-only repair: images left with no grid256 thumbnail by a fixed
    // bug (see `backfill_missing_grid_thumbnails`'s doc) — a crash between
    // the images row insert and thumbnail generation used to be permanent.
    let _ = lenslocker_import::backfill_missing_grid_thumbnails(&conn, &paths);
    Ok(AppState {
        conn: Mutex::new(conn),
        paths,
        library_id,
    })
}

/// `tauri.conf.json`'s `assetProtocol.scope` is a static `$APPDATA/**`
/// entry, fixed at build time. Milestone 5.5 made the library location
/// runtime-chosen — it can be any drive — so `convertFileSrc` thumbnail/
/// blob URLs outside `$APPDATA` are silently denied by WebView2 unless the
/// chosen root is added to the scope at runtime. Called everywhere a
/// library becomes [`LibraryState::Ready`]: initial boot, and both
/// first-run branches (`create_library`/`open_existing_library`).
fn allow_library_in_asset_scope(app: &tauri::AppHandle, root: &Path) {
    if let Err(err) = app.asset_protocol_scope().allow_directory(root, true) {
        eprintln!(
            "[bootstrap] could not widen asset scope to {}: {err}",
            root.display()
        );
    }
}

/// Read on every launch, before anything else touches a catalog. Never
/// falls back to a default location — a missing or unreadable bootstrap
/// config, an unreachable recorded path, or a catalog that fails to open
/// (corrupt file, permissions) all route to [`LibraryState::NeedsSetup`]
/// rather than crashing the app or guessing a location.
fn load_initial_library_state(app: &tauri::AppHandle) -> LibraryState {
    let config_path = bootstrap_config_path(app);
    let Some(library_path) = read_bootstrap_config(&config_path) else {
        return LibraryState::NeedsSetup {
            unreachable_path: None,
        };
    };

    let root = PathBuf::from(&library_path);
    if !root.is_dir() {
        return LibraryState::NeedsSetup {
            unreachable_path: Some(library_path),
        };
    }

    match try_init_state(&root) {
        Ok(app_state) => {
            allow_library_in_asset_scope(app, &root);
            LibraryState::Ready(app_state)
        }
        Err(err) => {
            eprintln!(
                "[bootstrap] configured library at {library_path} could not be opened: {err}"
            );
            LibraryState::NeedsSetup {
                unreachable_path: Some(library_path),
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryStatusDto {
    ready: bool,
    previous_path_unreachable: Option<String>,
}

/// Called once on frontend boot to decide: show the first-run screen, or go
/// straight to the main app.
#[tauri::command]
fn check_library_status(state: tauri::State<Mutex<LibraryState>>) -> LibraryStatusDto {
    match &*state.lock().unwrap() {
        LibraryState::Ready(_) => LibraryStatusDto {
            ready: true,
            previous_path_unreachable: None,
        },
        LibraryState::NeedsSetup { unreachable_path } => LibraryStatusDto {
            ready: false,
            previous_path_unreachable: unreachable_path.clone(),
        },
    }
}

/// The real native folder picker for first-run setup — no default/pre-filled
/// path, matching the approved design. Returns `None` if the user cancels.
#[tauri::command]
async fn pick_library_folder(app: tauri::AppHandle) -> CmdResult<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    file_dialog(&app).pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.recv().map_err(|_| CmdError::NoFolderChosen)?;
    Ok(folder
        .and_then(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectFolderDto {
    existing_library: bool,
    free_bytes: u64,
}

/// Reports on a folder the user just chose: does it already hold a
/// `catalog.sqlite` (existing library → the frontend routes to "Open," not
/// "Create," and hides the conversion toggle per ticket 009's
/// fixed-at-creation rule), and how much free space is on its volume.
#[tauri::command]
fn inspect_library_folder(path: String) -> CmdResult<InspectFolderDto> {
    inspect_library_folder_at(&PathBuf::from(&path))
}

fn inspect_library_folder_at(root: &Path) -> CmdResult<InspectFolderDto> {
    let existing_library = root.join("catalog.sqlite").is_file();
    let free_bytes = free_space_bytes(root)?;
    Ok(InspectFolderDto {
        existing_library,
        free_bytes,
    })
}

// The workspace denies `unsafe_code` by default (`Cargo.toml`'s lint table)
// — `GetDiskFreeSpaceExW` is a raw Win32 FFI call, unavoidably `unsafe`,
// same rationale as `webview2_hardening`'s module-level allow.
#[cfg(windows)]
#[allow(unsafe_code)]
fn free_space_bytes(path: &Path) -> CmdResult<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::HSTRING;

    let wide = HSTRING::from(path.to_string_lossy().as_ref());
    let mut free_bytes_available: u64 = 0;
    unsafe {
        GetDiskFreeSpaceExW(&wide, Some(&mut free_bytes_available), None, None)
            .map_err(|e| CmdError::DiskSpace(e.to_string()))?;
    }
    Ok(free_bytes_available)
}

#[cfg(not(windows))]
fn free_space_bytes(_path: &Path) -> CmdResult<u64> {
    // LensLocker ships Windows-only (workplan/SPEC.md); this stub exists
    // only so the crate still type-checks on other hosts.
    Ok(0)
}

/// Creates a brand-new vault at `path`: makes the directory if needed,
/// initializes a fresh catalog, creates the `libraries` row with
/// `conversion_enabled` fixed at creation (ticket 009), points the
/// bootstrap config at it, and swaps the app's live `AppState` in — no
/// restart required. The caller (via `inspect_library_folder`) is expected
/// to have already confirmed no library exists here yet; this is
/// re-checked so the invariant holds even if the frontend gets it wrong.
#[tauri::command]
fn create_library(
    app: tauri::AppHandle,
    state: tauri::State<Mutex<LibraryState>>,
    path: String,
    conversion_enabled: bool,
) -> CmdResult<()> {
    let root = PathBuf::from(&path);
    let app_state = create_library_at(&root, conversion_enabled)?;
    write_bootstrap_config(&app, &root)?;
    allow_library_in_asset_scope(&app, &root);
    *state.lock().unwrap() = LibraryState::Ready(app_state);
    Ok(())
}

/// The actual "make a fresh vault" logic, factored out of the
/// [`create_library`] command so it's testable without a live `AppHandle`
/// or `tauri::State`.
fn create_library_at(root: &Path, conversion_enabled: bool) -> CmdResult<AppState> {
    if root.join("catalog.sqlite").is_file() {
        return Err(CmdError::LibraryAlreadyExists);
    }

    std::fs::create_dir_all(root)?;
    let paths = lenslocker_import::LibraryPaths::new(root);
    let mut conn = Connection::open(paths.catalog_db())?;
    lenslocker_catalog::migrate(&mut conn).map_err(|e| CmdError::Migration(e.to_string()))?;
    let library_id = lenslocker_import::create_library_row(&conn, root, conversion_enabled)?;
    let _ = lenslocker_import::sweep_expired_quarantine(&conn);

    Ok(AppState {
        conn: Mutex::new(conn),
        paths,
        library_id,
    })
}

/// Opens a library that already exists at `path` — the catalog there is
/// already correctly set up (per §4/ticket 009, `conversion_enabled` is
/// fixed at creation and not re-decided here). Just points the bootstrap
/// config at it, loads it into `AppState`, and reports ready.
#[tauri::command]
fn open_existing_library(
    app: tauri::AppHandle,
    state: tauri::State<Mutex<LibraryState>>,
    path: String,
) -> CmdResult<()> {
    let root = PathBuf::from(&path);
    let app_state = open_existing_library_at(&root)?;
    write_bootstrap_config(&app, &root)?;
    allow_library_in_asset_scope(&app, &root);
    *state.lock().unwrap() = LibraryState::Ready(app_state);
    Ok(())
}

/// The actual "open a pre-existing vault" logic, factored out of the
/// [`open_existing_library`] command so it's testable directly.
fn open_existing_library_at(root: &Path) -> CmdResult<AppState> {
    if !root.join("catalog.sqlite").is_file() {
        return Err(CmdError::LibraryNotFound);
    }
    try_init_state(root)
}

/// Builds the main window with a WebView2 environment that has crash-report
/// upload disabled (workplan/SPEC.md §8.1), then applies the SmartScreen
/// setting once the webview exists. Tauri normally auto-creates
/// config-declared windows *before* `.setup()` runs with its own default
/// environment; `tauri.conf.json` sets `"create": false` on the main window
/// specifically so this function — not Tauri's default path — is what
/// creates it.
#[cfg(windows)]
fn create_hardened_main_window(app: &tauri::App) -> tauri::Result<()> {
    let window_config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
        .expect("tauri.conf.json must declare a \"main\" window");

    // WebView2's own default (an empty user-data-folder) resolves to
    // `<exe_dir>\<exe_name>.WebView2\`, which isn't writable once installed
    // to `C:\Program Files` (see webview2_hardening::create_environment's
    // doc) — use the app's own local-data directory instead, which is
    // writable regardless of install location.
    let webview2_data_dir = app
        .path()
        .app_local_data_dir()
        .expect("could not resolve app local data dir")
        .join("WebView2");
    let environment = webview2_hardening::create_environment(&webview2_data_dir)
        .expect("failed to create a hardened WebView2 environment");
    // `ICoreWebView2Environment` (a COM interface) isn't `Send`, but
    // `with_webview` below requires its closure to be — the raw pointer
    // value is plain data and Send, and is all identity comparison needs.
    use windows::core::Interface;
    let environment_raw_ptr = environment.as_raw() as usize;

    let window = tauri::WebviewWindowBuilder::from_config(app, &window_config)?
        .with_environment(environment)
        .build()?;

    // Real, printed evidence that both COM settings took effect on the live
    // running webview — not just "the code compiled" (workplan/SPEC.md §8's
    // Milestone 6 verification bar).
    window.with_webview(move |webview| {
        let used_hardened_env =
            webview2_hardening::is_hardened_environment(&webview, environment_raw_ptr);
        println!(
            "[webview2-hardening] using our environment (crash reporting redirected, not uploaded): {used_hardened_env}"
        );

        match webview2_hardening::disable_smartscreen(webview) {
            Ok(smartscreen_still_enabled) => println!(
                "[webview2-hardening] IsReputationCheckingRequired read back as: {smartscreen_still_enabled} (want: false)"
            ),
            Err(err) => {
                // Not fatal: the browser-flag backstop
                // (--disable-features=...msSmartScreenProtection) still
                // applies even if the COM setting failed for some reason
                // (e.g. an older WebView2 runtime missing
                // ICoreWebView2Settings8).
                eprintln!(
                    "warning: failed to disable WebView2 SmartScreen via COM settings: {err}"
                );
            }
        }
    })?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_handle = app.handle().clone();
            let library_state = load_initial_library_state(&app_handle);
            app.manage(Mutex::new(library_state));
            app.manage(ImportLock::default());
            #[cfg(windows)]
            create_hardened_main_window(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_library_status,
            cancel_import,
            pick_library_folder,
            inspect_library_folder,
            create_library,
            open_existing_library,
            get_app_settings,
            update_app_settings,
            list_images,
            get_image_detail,
            get_full_preview,
            add_tag,
            remove_tag,
            confirm_auto_tag,
            reject_auto_tag,
            list_tags,
            list_sources,
            list_review_queue,
            resolve_review_pair,
            copy_file_path,
            import_directory,
            export_image,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_library_at_produces_a_working_catalog_and_libraries_row_with_the_right_conversion_flag()
     {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");

        let app_state = create_library_at(&root, false).unwrap();

        assert!(root.join("catalog.sqlite").is_file());
        let enabled: i64 = app_state
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT conversion_enabled FROM libraries WHERE id = ?1",
                [app_state.library_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(enabled, 0);
    }

    #[test]
    fn create_library_at_refuses_to_overwrite_an_existing_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        create_library_at(&root, true).unwrap();

        let result = create_library_at(&root, true);

        assert!(matches!(result, Err(CmdError::LibraryAlreadyExists)));
    }

    #[test]
    fn open_existing_library_at_loads_a_previously_created_vault_without_reinitializing_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");
        let created = create_library_at(&root, false).unwrap();
        let original_library_id = created.library_id;
        drop(created);

        let opened = open_existing_library_at(&root).unwrap();

        assert_eq!(opened.library_id, original_library_id);
        let enabled: i64 = opened
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT conversion_enabled FROM libraries WHERE id = ?1",
                [opened.library_id],
                |row| row.get(0),
            )
            .unwrap();
        // conversion_enabled must still read false — opening must not have
        // re-created the row with the schema's default (on).
        assert_eq!(enabled, 0);
    }

    #[test]
    fn open_existing_library_at_rejects_a_folder_with_no_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("empty");
        std::fs::create_dir_all(&root).unwrap();

        let result = open_existing_library_at(&root);

        assert!(matches!(result, Err(CmdError::LibraryNotFound)));
    }

    #[test]
    fn inspect_library_folder_at_reports_new_folder_as_not_an_existing_library() {
        let dir = tempfile::tempdir().unwrap();

        let report = inspect_library_folder_at(dir.path()).unwrap();

        assert!(!report.existing_library);
        // A real temp directory always has some free space to report; this
        // is a genuine Win32 call, not a mock.
        assert!(report.free_bytes > 0);
    }

    #[test]
    fn inspect_library_folder_at_detects_an_existing_catalog() {
        let dir = tempfile::tempdir().unwrap();
        create_library_at(&dir.path().join("nested"), true).unwrap();
        let root = dir.path().join("nested");

        let report = inspect_library_folder_at(&root).unwrap();

        assert!(report.existing_library);
    }

    #[test]
    fn bootstrap_config_round_trips_the_library_path() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("bootstrap.json");
        let library_root = dir.path().join("my-vault");

        write_bootstrap_config_at(&config_path, &library_root).unwrap();
        let read_back = read_bootstrap_config(&config_path).unwrap();

        assert_eq!(read_back, library_root.to_string_lossy());
    }

    #[test]
    fn read_bootstrap_config_returns_none_when_the_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("does-not-exist.json");

        assert!(read_bootstrap_config(&config_path).is_none());
    }

    #[test]
    fn app_settings_round_trip_through_a_real_library() {
        let dir = tempfile::tempdir().unwrap();
        let app_state = create_library_at(&dir.path().join("vault"), true).unwrap();
        let conn = app_state.conn.lock().unwrap();

        let before = lenslocker_catalog::get_app_settings(&conn).unwrap();
        assert_eq!(
            before,
            lenslocker_catalog::AppSettings {
                hamming_threshold: 5,
                retention_days: 30,
                tag_storage_threshold: 0.1,
                tag_display_threshold: 0.5,
            }
        );

        lenslocker_catalog::update_app_settings(
            &conn,
            lenslocker_catalog::AppSettings {
                hamming_threshold: 12,
                retention_days: 7,
                ..before
            },
        )
        .unwrap();
        let after = lenslocker_catalog::get_app_settings(&conn).unwrap();

        assert_eq!(
            after,
            lenslocker_catalog::AppSettings {
                hamming_threshold: 12,
                retention_days: 7,
                ..before
            }
        );
    }
}
