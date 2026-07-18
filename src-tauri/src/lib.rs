//! LumenVault Tauri app shell: real commands binding the domain crates to
//! the UI, per workplan/SPEC.md §2/§9, Milestone 5.
//!
//! **Library location, a judgment call not specified anywhere**: no
//! milestone builds a "create/choose a library" UI, and the approved design
//! (workplan/design/lumenvault-design.html) assumes an already-populated
//! library exists. This app therefore opens (creating if absent) exactly
//! one library at `<app-data-dir>/library` on startup — a single-user,
//! single-library app matching SPEC.md §12's explicit "multi-library
//! support" exclusion. `conversion_enabled` defaults to the schema's own
//! default (`1`/on).
//!
//! **State**: one shared `rusqlite::Connection` behind a `Mutex`, matching
//! every prior milestone's single-connection-per-library pattern (the test
//! suites all use one `Connection` per scenario) — SQLite's own locking
//! makes a single serialized connection the simplest correct choice for a
//! single-user desktop app, not a bottleneck at this scale.

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

#[derive(Debug, thiserror::Error)]
enum CmdError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Import(#[from] lumenvault_import::ImportError),
    #[error(transparent)]
    Xmp(#[from] lumenvault_xmp::XmpError),
    #[error("image {0} not found")]
    ImageNotFound(i64),
    #[error("no folder was chosen")]
    NoFolderChosen,
    #[error("a merge action requires keeper_id")]
    MissingKeeper,
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

#[tauri::command]
fn list_images(
    state: tauri::State<AppState>,
    filters: FiltersDto,
    sort: String,
    search: Option<String>,
    offset: i64,
    limit: i64,
) -> CmdResult<ListImagesResult> {
    let conn = state.conn.lock().unwrap();
    let (items, total) = lumenvault_catalog::list_images(
        &conn,
        &filters.into(),
        parse_sort(&sort),
        search.as_deref(),
        offset,
        limit,
    )?;
    Ok(ListImagesResult { items: items.into_iter().map(Into::into).collect(), total })
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
fn get_image_detail(state: tauri::State<AppState>, id: i64) -> CmdResult<ImageDetailDto> {
    let conn = state.conn.lock().unwrap();
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
}

#[tauri::command]
fn add_tag(state: tauri::State<AppState>, image_id: i64, tag: String) -> CmdResult<()> {
    let conn = state.conn.lock().unwrap();
    lumenvault_catalog::add_tag(&conn, image_id, &tag)?;
    lumenvault_xmp::sync_sidecar(&conn, image_id)?;
    Ok(())
}

#[tauri::command]
fn remove_tag(state: tauri::State<AppState>, image_id: i64, tag: String) -> CmdResult<()> {
    let conn = state.conn.lock().unwrap();
    lumenvault_catalog::remove_tag(&conn, image_id, &tag)?;
    lumenvault_xmp::sync_sidecar(&conn, image_id)?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct TagCountDto {
    name: String,
    count: i64,
}

#[tauri::command]
fn list_tags(state: tauri::State<AppState>) -> CmdResult<Vec<TagCountDto>> {
    let conn = state.conn.lock().unwrap();
    Ok(lumenvault_catalog::list_tags(&conn)?.into_iter().map(|t| TagCountDto { name: t.name, count: t.count }).collect())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceCountDto {
    source_root: String,
    count: i64,
}

#[tauri::command]
fn list_sources(state: tauri::State<AppState>) -> CmdResult<Vec<SourceCountDto>> {
    let conn = state.conn.lock().unwrap();
    Ok(lumenvault_catalog::list_sources(&conn)?
        .into_iter()
        .map(|s| SourceCountDto { source_root: s.source_root, count: s.count })
        .collect())
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
fn list_review_queue(state: tauri::State<AppState>) -> CmdResult<Vec<ReviewQueueEntryDto>> {
    let conn = state.conn.lock().unwrap();
    Ok(lumenvault_catalog::list_review_queue(&conn)?
        .into_iter()
        .map(|e| ReviewQueueEntryDto {
            queue_id: e.queue_id,
            hamming_distance: e.hamming_distance,
            image_a: e.image_a.into(),
            image_b: e.image_b.into(),
        })
        .collect())
}

#[tauri::command]
fn resolve_review_pair(
    state: tauri::State<AppState>,
    queue_id: i64,
    action: String,
    keeper_id: Option<i64>,
) -> CmdResult<()> {
    let conn = state.conn.lock().unwrap();
    let action = match action.as_str() {
        "merge" => lumenvault_import::ReviewAction::Merge {
            keeper_id: keeper_id.ok_or(CmdError::MissingKeeper)?,
        },
        _ => lumenvault_import::ReviewAction::Dismiss,
    };
    lumenvault_import::resolve_review_pair(&conn, &state.paths, queue_id, action)?;
    Ok(())
}

#[tauri::command]
fn copy_file_path(state: tauri::State<AppState>, id: i64) -> CmdResult<String> {
    let conn = state.conn.lock().unwrap();
    let d = lumenvault_catalog::get_image_detail(&conn, id)?.ok_or(CmdError::ImageNotFound(id))?;
    Ok(d.stored_path)
}

#[tauri::command]
async fn import_directory(app: tauri::AppHandle, state: tauri::State<'_, AppState>) -> CmdResult<usize> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.recv().map_err(|_| CmdError::NoFolderChosen)?.ok_or(CmdError::NoFolderChosen)?;
    let source_root: PathBuf = folder.into_path().map_err(|_| CmdError::NoFolderChosen)?;

    let conn = state.conn.lock().unwrap();
    let batch_id = lumenvault_import::start_or_resume_batch(&conn, state.library_id, &source_root)?;
    let conversion_enabled = lumenvault_import::conversion_enabled(&conn, state.library_id)?;
    let ctx = lumenvault_import::ImportContext {
        conn: &conn,
        paths: &state.paths,
        library_id: state.library_id,
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
}

#[tauri::command]
async fn export_image(app: tauri::AppHandle, state: tauri::State<'_, AppState>, id: i64) -> CmdResult<String> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    let folder = rx.recv().map_err(|_| CmdError::NoFolderChosen)?.ok_or(CmdError::NoFolderChosen)?;
    let dest_dir: PathBuf = folder.into_path().map_err(|_| CmdError::NoFolderChosen)?;

    let conn = state.conn.lock().unwrap();
    let dest = lumenvault_import::export_image(&conn, id, &dest_dir)?;
    Ok(dest.to_string_lossy().into_owned())
}

fn default_library_root(app: &tauri::App) -> PathBuf {
    app.path().app_data_dir().unwrap_or_else(|_| PathBuf::from(".")).join("library")
}

fn init_state(root: &Path) -> AppState {
    let paths = lumenvault_import::LibraryPaths::new(root);
    std::fs::create_dir_all(root).expect("could not create library root");
    let mut conn = Connection::open(paths.catalog_db()).expect("could not open catalog database");
    lumenvault_catalog::migrate(&mut conn).expect("could not migrate catalog schema");
    let library_id = lumenvault_import::ensure_library(&conn, root).expect("could not ensure library row");
    // Launch-only retention sweep (workplan/SPEC.md §3).
    let _ = lumenvault_import::sweep_expired_quarantine(&conn);
    AppState { conn: Mutex::new(conn), paths, library_id }
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
            let root = default_library_root(app);
            app.manage(init_state(&root));
            #[cfg(windows)]
            create_hardened_main_window(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
