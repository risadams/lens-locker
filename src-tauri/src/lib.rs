//! LumenVault Tauri app shell: real commands binding the domain crates to
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

use lumenvault_catalog::{GridImage, ImageFilters, SortOrder};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::Manager;

#[cfg(windows)]
mod webview2_hardening;

struct AppState {
    conn: Mutex<Connection>,
    paths: lumenvault_import::LibraryPaths,
    library_id: i64,
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
    Import(#[from] lumenvault_import::ImportError),
    #[error(transparent)]
    Xmp(#[from] lumenvault_xmp::XmpError),
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
    #[error("a LumenVault library already exists at this location — open it instead of creating a new one")]
    LibraryAlreadyExists,
    #[error("no LumenVault library was found at this location")]
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
        Self { id: g.id, thumbnail_path: g.thumbnail_path, capture_date: g.capture_date, tags: g.tags, verified: g.verified }
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
        Self { date_from: f.date_from, date_to: f.date_to, formats: f.formats, sources: f.sources, tags: f.tags }
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
        let (items, total) = lumenvault_catalog::list_images(
            &conn,
            &filters.into(),
            parse_sort(&sort),
            search.as_deref(),
            offset,
            limit,
        )?;
        Ok(ListImagesResult { items: items.into_iter().map(Into::into).collect(), total })
    })
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
    tags: Vec<String>,
    first_imported_at: String,
}

#[tauri::command]
fn get_image_detail(state: tauri::State<Mutex<LibraryState>>, id: i64) -> CmdResult<ImageDetailDto> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let d = lumenvault_catalog::get_image_detail(&conn, id)?.ok_or(CmdError::ImageNotFound(id))?;
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
            tags: d.tags,
            first_imported_at: d.first_imported_at,
        })
    })
}

#[tauri::command]
fn add_tag(state: tauri::State<Mutex<LibraryState>>, image_id: i64, tag: String) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lumenvault_catalog::add_tag(&conn, image_id, &tag)?;
        lumenvault_xmp::sync_sidecar(&conn, image_id)?;
        Ok(())
    })
}

#[tauri::command]
fn remove_tag(state: tauri::State<Mutex<LibraryState>>, image_id: i64, tag: String) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lumenvault_catalog::remove_tag(&conn, image_id, &tag)?;
        lumenvault_xmp::sync_sidecar(&conn, image_id)?;
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
        Ok(lumenvault_catalog::list_tags(&conn)?.into_iter().map(|t| TagCountDto { name: t.name, count: t.count }).collect())
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
        Ok(lumenvault_catalog::list_sources(&conn)?
            .into_iter()
            .map(|s| SourceCountDto { source_root: s.source_root, count: s.count })
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
fn list_review_queue(state: tauri::State<Mutex<LibraryState>>) -> CmdResult<Vec<ReviewQueueEntryDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lumenvault_catalog::list_review_queue(&conn)?
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
            "merge" => lumenvault_import::ReviewAction::Merge {
                keeper_id: keeper_id.ok_or(CmdError::MissingKeeper)?,
            },
            _ => lumenvault_import::ReviewAction::Dismiss,
        };
        lumenvault_import::resolve_review_pair(&conn, &app_state.paths, queue_id, action)?;
        Ok(())
    })
}

#[tauri::command]
fn copy_file_path(state: tauri::State<Mutex<LibraryState>>, id: i64) -> CmdResult<String> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let d = lumenvault_catalog::get_image_detail(&conn, id)?.ok_or(CmdError::ImageNotFound(id))?;
        Ok(d.stored_path)
    })
}

#[tauri::command]
async fn import_directory(app: tauri::AppHandle, state: tauri::State<'_, Mutex<LibraryState>>) -> CmdResult<usize> {
    use tauri_plugin_dialog::DialogExt;

    // Fail fast, before ever popping a native dialog, if there's no live
    // library to import into.
    if !matches!(&*state.lock().unwrap(), LibraryState::Ready(_)) {
        return Err(CmdError::LibraryNotConfigured);
    }

    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.recv().map_err(|_| CmdError::NoFolderChosen)?.ok_or(CmdError::NoFolderChosen)?;
    let source_root: PathBuf = folder.into_path().map_err(|_| CmdError::NoFolderChosen)?;

    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let batch_id = lumenvault_import::start_or_resume_batch(&conn, app_state.library_id, &source_root)?;
        let conversion_enabled = lumenvault_import::conversion_enabled(&conn, app_state.library_id)?;
        let ctx = lumenvault_import::ImportContext {
            conn: &conn,
            paths: &app_state.paths,
            library_id: app_state.library_id,
            batch_id,
            conversion_enabled,
        };

        let mut imported = 0usize;
        lumenvault_import::import_directory(&ctx, &source_root, |_path, outcome| {
            if matches!(outcome, lumenvault_import::FileOutcome::Imported { .. }) {
                imported += 1;
            }
        })?;

        Ok(imported)
    })
}

#[tauri::command]
async fn export_image(app: tauri::AppHandle, state: tauri::State<'_, Mutex<LibraryState>>, id: i64) -> CmdResult<String> {
    use tauri_plugin_dialog::DialogExt;

    if !matches!(&*state.lock().unwrap(), LibraryState::Ready(_)) {
        return Err(CmdError::LibraryNotConfigured);
    }

    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.recv().map_err(|_| CmdError::NoFolderChosen)?.ok_or(CmdError::NoFolderChosen)?;
    let dest_dir: PathBuf = folder.into_path().map_err(|_| CmdError::NoFolderChosen)?;

    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        let dest = lumenvault_import::export_image(&conn, id, &dest_dir)?;
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
        let s = lumenvault_catalog::get_app_settings(&conn)?;
        Ok(AppSettingsDto { hamming_threshold: s.hamming_threshold, retention_days: s.retention_days })
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
        lumenvault_catalog::update_app_settings(
            &conn,
            lumenvault_catalog::AppSettings { hamming_threshold, retention_days },
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
    app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from(".")).join("bootstrap.json")
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
    let config = BootstrapConfig { library_path: root.to_string_lossy().into_owned() };
    let json = serde_json::to_string_pretty(&config).map_err(|e| CmdError::Bootstrap(e.to_string()))?;
    std::fs::write(config_path, json)?;
    Ok(())
}

/// Opens (migrating if needed) the catalog at `root` and ensures its
/// `libraries` row exists — the shared plumbing behind both "app boots with
/// a previously-configured library" and "user picks an existing vault in
/// the first-run screen." Not used for the "create a brand-new vault"
/// path — that needs [`lumenvault_import::create_library_row`] instead, to
/// set `conversion_enabled` at creation per ticket 009.
fn try_init_state(root: &Path) -> CmdResult<AppState> {
    let paths = lumenvault_import::LibraryPaths::new(root);
    std::fs::create_dir_all(root)?;
    let mut conn = Connection::open(paths.catalog_db())?;
    lumenvault_catalog::migrate(&mut conn).map_err(|e| CmdError::Migration(e.to_string()))?;
    let library_id = lumenvault_import::ensure_library(&conn, root)?;
    // Launch-only retention sweep (workplan/SPEC.md §3).
    let _ = lumenvault_import::sweep_expired_quarantine(&conn);
    Ok(AppState { conn: Mutex::new(conn), paths, library_id })
}

/// Read on every launch, before anything else touches a catalog. Never
/// falls back to a default location — a missing or unreadable bootstrap
/// config, an unreachable recorded path, or a catalog that fails to open
/// (corrupt file, permissions) all route to [`LibraryState::NeedsSetup`]
/// rather than crashing the app or guessing a location.
fn load_initial_library_state(app: &tauri::AppHandle) -> LibraryState {
    let config_path = bootstrap_config_path(app);
    let Some(library_path) = read_bootstrap_config(&config_path) else {
        return LibraryState::NeedsSetup { unreachable_path: None };
    };

    let root = PathBuf::from(&library_path);
    if !root.is_dir() {
        return LibraryState::NeedsSetup { unreachable_path: Some(library_path) };
    }

    match try_init_state(&root) {
        Ok(app_state) => LibraryState::Ready(app_state),
        Err(err) => {
            eprintln!("[bootstrap] configured library at {library_path} could not be opened: {err}");
            LibraryState::NeedsSetup { unreachable_path: Some(library_path) }
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
        LibraryState::Ready(_) => LibraryStatusDto { ready: true, previous_path_unreachable: None },
        LibraryState::NeedsSetup { unreachable_path } => {
            LibraryStatusDto { ready: false, previous_path_unreachable: unreachable_path.clone() }
        }
    }
}

/// The real native folder picker for first-run setup — no default/pre-filled
/// path, matching the approved design. Returns `None` if the user cancels.
#[tauri::command]
async fn pick_library_folder(app: tauri::AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.recv().map_err(|_| CmdError::NoFolderChosen)?;
    Ok(folder.and_then(|f| f.into_path().ok()).map(|p| p.to_string_lossy().into_owned()))
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
    Ok(InspectFolderDto { existing_library, free_bytes })
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
    // LumenVault ships Windows-only (workplan/SPEC.md); this stub exists
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
    let paths = lumenvault_import::LibraryPaths::new(root);
    let mut conn = Connection::open(paths.catalog_db())?;
    lumenvault_catalog::migrate(&mut conn).map_err(|e| CmdError::Migration(e.to_string()))?;
    let library_id = lumenvault_import::create_library_row(&conn, root, conversion_enabled)?;
    let _ = lumenvault_import::sweep_expired_quarantine(&conn);

    Ok(AppState { conn: Mutex::new(conn), paths, library_id })
}

/// Opens a library that already exists at `path` — the catalog there is
/// already correctly set up (per §4/ticket 009, `conversion_enabled` is
/// fixed at creation and not re-decided here). Just points the bootstrap
/// config at it, loads it into `AppState`, and reports ready.
#[tauri::command]
fn open_existing_library(app: tauri::AppHandle, state: tauri::State<Mutex<LibraryState>>, path: String) -> CmdResult<()> {
    let root = PathBuf::from(&path);
    let app_state = open_existing_library_at(&root)?;
    write_bootstrap_config(&app, &root)?;
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

    let environment = webview2_hardening::create_environment()
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
            #[cfg(windows)]
            create_hardened_main_window(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            check_library_status,
            pick_library_folder,
            inspect_library_folder,
            create_library,
            open_existing_library,
            get_app_settings,
            update_app_settings,
            list_images,
            get_image_detail,
            add_tag,
            remove_tag,
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
    fn create_library_at_produces_a_working_catalog_and_libraries_row_with_the_right_conversion_flag() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("vault");

        let app_state = create_library_at(&root, false).unwrap();

        assert!(root.join("catalog.sqlite").is_file());
        let enabled: i64 = app_state
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT conversion_enabled FROM libraries WHERE id = ?1", [app_state.library_id], |row| {
                row.get(0)
            })
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
            .query_row("SELECT conversion_enabled FROM libraries WHERE id = ?1", [opened.library_id], |row| {
                row.get(0)
            })
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

        let before = lumenvault_catalog::get_app_settings(&conn).unwrap();
        assert_eq!(before, lumenvault_catalog::AppSettings { hamming_threshold: 5, retention_days: 30 });

        lumenvault_catalog::update_app_settings(
            &conn,
            lumenvault_catalog::AppSettings { hamming_threshold: 12, retention_days: 7 },
        )
        .unwrap();
        let after = lumenvault_catalog::get_app_settings(&conn).unwrap();

        assert_eq!(after, lumenvault_catalog::AppSettings { hamming_threshold: 12, retention_days: 7 });
    }
}
