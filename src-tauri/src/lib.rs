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
    #[error("image {0} hasn't been analyzed yet — similarity search needs it to have been processed first")]
    ImageNotAnalyzedYet(i64),
    #[error(transparent)]
    Ml(#[from] lenslocker_ml::MlError),
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
    #[serde(default)]
    persons: Vec<i64>,
}

impl From<FiltersDto> for ImageFilters {
    fn from(f: FiltersDto) -> Self {
        Self {
            date_from: f.date_from,
            date_to: f.date_to,
            formats: f.formats,
            sources: f.sources,
            tags: f.tags,
            persons: f.persons,
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

/// A named face chip for the drawer's "People in this photo" list (028
/// decision #5) — clickable to jump to that person's cluster in the
/// People view. Unnamed detections don't get individual chips; they
/// collapse into `ImageDetailDto::unnamed_clustered`/`unclustered_face_count`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NamedFaceChipDto {
    cluster_id: i64,
    person_id: i64,
    person_name: String,
}

impl From<lenslocker_catalog::NamedFaceChip> for NamedFaceChipDto {
    fn from(c: lenslocker_catalog::NamedFaceChip) -> Self {
        Self { cluster_id: c.cluster_id, person_id: c.person_id, person_name: c.person_name }
    }
}

/// One unnamed cluster's detection count on this image — split out from a
/// bare total (rather than one combined "+N unidentified" number) so the
/// drawer can offer inline naming when there's exactly one unambiguous
/// target (028 decision #3) and fall back to the People view only when
/// genuinely ambiguous.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnnamedFaceGroupDto {
    cluster_id: i64,
    count: i64,
}

impl From<lenslocker_catalog::UnnamedFaceGroup> for UnnamedFaceGroupDto {
    fn from(g: lenslocker_catalog::UnnamedFaceGroup) -> Self {
        Self { cluster_id: g.cluster_id, count: g.count }
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
    named_faces: Vec<NamedFaceChipDto>,
    unnamed_clustered: Vec<UnnamedFaceGroupDto>,
    unclustered_face_count: i64,
}

/// ML-SPEC.md §4's display floor: a manual tag is always visible; an
/// auto-tag is visible "by default" only once its confidence clears
/// `display_threshold` (a *higher* bar than the storage floor it already
/// cleared just to have a row at all — `AppSettings::tag_storage_threshold`
/// vs `tag_display_threshold`). Factored out of [`get_image_detail`] so
/// it's unit-testable without needing a real `tauri::State`.
fn tag_is_visible_by_default(tag: &lenslocker_catalog::TagWithProvenance, display_threshold: f64) -> bool {
    tag.source != "auto" || tag.confidence.unwrap_or(0.0) >= display_threshold
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
        // ML-SPEC.md §4's display floor: an auto-tag that cleared the
        // (lower) storage floor still gets a real image_tags row — so
        // confirming it later never needs a re-score — but only surfaces
        // as a visible chip "by default" once it also clears the higher
        // display floor. Manual tags have no confidence and are always
        // shown; filtered here (not in get_image_detail itself) since
        // this is a display rule, not something the catalog layer should
        // decide — get_image_detail keeps returning everything.
        let display_threshold = lenslocker_catalog::get_app_settings(&conn)?.tag_display_threshold;
        let visible_tags = d.tags.into_iter().filter(|t| tag_is_visible_by_default(t, display_threshold)).map(TagDto::from).collect();
        let people = lenslocker_catalog::people_for_image(&conn, id)?;
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
            tags: visible_tags,
            first_imported_at: d.first_imported_at,
            named_faces: people.named.into_iter().map(Into::into).collect(),
            unnamed_clustered: people.unnamed_clustered.into_iter().map(Into::into).collect(),
            unclustered_face_count: people.unclustered_count,
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

/// Candidate pool size for both similarity-search commands below — a
/// generous over-fetch from `VecMirror` (well past any single page) so
/// the ordinary grid filters (§7's reuse pattern) still have enough
/// floor-cleared candidates to page through after composing with them,
/// without querying the mirror again per page.
const SIMILARITY_CANDIDATE_POOL: usize = 500;

/// Shared by [`find_similar_images`] and [`search_by_text`] — both
/// resolve to "get a query vector, rank every other analyzed photo
/// against it via `VecMirror`, floor-filter, compose with the ordinary
/// grid filters via [`lenslocker_catalog::list_images_by_similarity`]."
/// They differ only in where `query_vector` comes from and how a raw
/// dot product becomes a score (`score_transform` — the identity for
/// image-to-image cosine similarity, SigLIP's calibrated sigmoid for
/// text-to-image; see [`lenslocker_catalog::VecMirror::query_similar_cosine`]'s
/// own doc comment for why one dot product serves both). `exclude_id`
/// drops a candidate from its own "similar to me" results — only
/// meaningful for image-to-image.
fn rank_and_paginate(
    conn: &Connection,
    model_id: i64,
    query_vector: &[u8],
    floor: f64,
    exclude_id: Option<i64>,
    score_transform: impl Fn(f64) -> f64,
    filters: lenslocker_catalog::ImageFilters,
    offset: i64,
    limit: i64,
) -> CmdResult<ListImagesResult> {
    let mirror = lenslocker_catalog::VecMirror::build(conn, model_id, lenslocker_ml::tagging::EMBEDDING_DIM)?;
    let ranked: Vec<(i64, f64)> = mirror
        .query_similar_cosine(query_vector, SIMILARITY_CANDIDATE_POOL)?
        .into_iter()
        .map(|(id, dot)| (id, score_transform(dot)))
        .filter(|(id, score)| Some(*id) != exclude_id && *score >= floor)
        .collect();

    let (items, total) = lenslocker_catalog::list_images_by_similarity(conn, &ranked, &filters, offset, limit)?;
    Ok(ListImagesResult { items: items.into_iter().map(Into::into).collect(), total })
}

/// "Find Similar" (ML-SPEC.md §8, ticket 034) — ranks every other
/// analyzed photo by cosine similarity to `image_id`'s own stored SigLIP
/// embedding. Run via `spawn_blocking`: `VecMirror::build` loads every
/// stored embedding for the model fresh on each call (no persistent
/// background-kept mirror yet — that's Milestone ML-6's job), real
/// synchronous work scaling with library size, same reasoning as
/// `get_full_preview`'s use of it.
///
/// `Err(ImageNotAnalyzedYet)` if `image_id` has no stored embedding —
/// nothing populates these automatically in the live app yet either (the
/// ML-2 backlog isn't wired to run until Milestone ML-6), so this is a
/// real, expected state today, not a defensive-only guard.
#[tauri::command]
async fn find_similar_images(
    app: tauri::AppHandle,
    image_id: i64,
    filters: FiltersDto,
    offset: i64,
    limit: i64,
) -> CmdResult<ListImagesResult> {
    tauri::async_runtime::spawn_blocking(move || {
        with_ready(&app.state::<Mutex<LibraryState>>(), |app_state| {
            let conn = app_state.conn.lock().unwrap();
            let model_id = lenslocker_ml::similarity::resolve_siglip_model_id(&conn)?;
            let source_vector = lenslocker_catalog::embedding_for_image(&conn, image_id, model_id)?
                .ok_or(CmdError::ImageNotAnalyzedYet(image_id))?;
            let floor = lenslocker_catalog::get_app_settings(&conn)?.similarity_search_floor;

            rank_and_paginate(&conn, model_id, &source_vector, floor, Some(image_id), |dot| dot, filters.into(), offset, limit)
        })
    })
    .await
    .map_err(|e| CmdError::TaskPanicked(e.to_string()))?
}

/// Text-to-image search (ML-SPEC.md §8, ticket 034 decision #4) — embeds
/// `query` live via SigLIP's text tower (CPU-only:
/// [`lenslocker_ml::similarity::embed_text_query`]'s own doc comment
/// explains why DirectML is off the table here), then ranks every
/// analyzed photo by SigLIP's own calibrated zero-shot probability
/// against that query — the same formula zero-shot tagging already uses,
/// reused via `zero_shot_probability_from_dot` rather than decoding every
/// candidate and re-dotting. Additive to the existing FTS keyword
/// search, not a replacement — the frontend's own search-mode toggle
/// decides which one a given query routes through, never both at once
/// (ticket 034 leaves blending mechanics unspecified; a mode toggle
/// avoids inventing a cross-metric score-blending scheme the spec
/// doesn't ask for).
#[tauri::command]
async fn search_by_text(
    app: tauri::AppHandle,
    query: String,
    filters: FiltersDto,
    offset: i64,
    limit: i64,
) -> CmdResult<ListImagesResult> {
    tauri::async_runtime::spawn_blocking(move || {
        with_ready(&app.state::<Mutex<LibraryState>>(), |app_state| {
            let conn = app_state.conn.lock().unwrap();
            let model_id = lenslocker_ml::similarity::resolve_siglip_model_id(&conn)?;

            let text_vector = lenslocker_ml::similarity::embed_text_query(&lenslocker_ml::models_dir(), &query)?;
            let query_bytes = lenslocker_ml::encode_embedding(&text_vector);
            let floor = lenslocker_catalog::get_app_settings(&conn)?.similarity_search_floor;

            rank_and_paginate(
                &conn,
                model_id,
                &query_bytes,
                floor,
                None,
                |dot| lenslocker_ml::tagging::zero_shot_probability_from_dot(dot as f32) as f64,
                filters.into(),
                offset,
                limit,
            )
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

/// Applies `tag` to every id in `image_ids` — the grid's bulk-correction
/// entry point (ML-SPEC.md §5's "reuses one shared multi-select
/// primitive"). One connection lock for the whole batch, not `image_ids.len()`
/// separate commands/round-trips. No explicit SQL transaction wrapping the
/// loop — this codebase has none anywhere (import's own crash-safety is
/// idempotent-retry, not transactional atomicity; see CLAUDE.md/`crates/import`),
/// so this matches that: stops at the first error, already-applied ids in
/// the batch stay applied (each `add_tag` call is already its own
/// idempotent, committed operation) — a caller can safely retry the same
/// selection, since re-applying an already-tagged image is a no-op.
#[tauri::command]
fn bulk_add_tag(state: tauri::State<Mutex<LibraryState>>, image_ids: Vec<i64>, tag: String) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        for image_id in image_ids {
            lenslocker_catalog::add_tag(&conn, image_id, &tag)?;
            lenslocker_xmp::sync_sidecar(&conn, image_id)?;
        }
        Ok(())
    })
}

/// Removes `tag` from every id in `image_ids` — [`bulk_add_tag`]'s
/// counterpart, same partial-application-on-error contract. Per image,
/// routes through [`lenslocker_catalog::reject_tag`] instead of
/// [`lenslocker_catalog::remove_tag`] when that image's copy of the tag
/// is auto-sourced — matching the single-image drawer's own
/// `confirm_auto_tag`/`reject_auto_tag` routing (`ui/main.js`'s
/// `renderDrawerTags`) — rather than always plain-deleting. §5's own
/// motivating example for bulk correction is literally "the model
/// consistently mis-tagging something across many photos"; always using
/// `remove_tag` would let that same wrong auto-tag silently reappear on
/// every image in the selection the next time it re-scores, exactly what
/// `rejected_tags` exists to prevent. The same tag name can be manual on
/// one selected image and auto-sourced on another, so this is decided per
/// image, not once for the whole batch.
#[tauri::command]
fn bulk_remove_tag(state: tauri::State<Mutex<LibraryState>>, image_ids: Vec<i64>, tag: String) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        for image_id in image_ids {
            let source = lenslocker_catalog::tag_source_for_image(&conn, image_id, &tag)?;
            if source.as_deref() == Some("auto") {
                lenslocker_catalog::reject_tag(&conn, image_id, &tag)?;
            } else {
                lenslocker_catalog::remove_tag(&conn, image_id, &tag)?;
            }
            lenslocker_xmp::sync_sidecar(&conn, image_id)?;
        }
        Ok(())
    })
}

// ── People view (ML-SPEC.md §6, ticket 028, Milestone ML-4 Slice C) ────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FaceClusterDto {
    id: i64,
    person_id: Option<i64>,
    person_name: Option<String>,
    photo_count: i64,
    representative_crop_path: Option<String>,
    hidden: bool,
}

impl From<lenslocker_catalog::FaceClusterSummary> for FaceClusterDto {
    fn from(c: lenslocker_catalog::FaceClusterSummary) -> Self {
        Self {
            id: c.id,
            person_id: c.person_id,
            person_name: c.person_name,
            photo_count: c.photo_count,
            representative_crop_path: c.representative_crop_path,
            hidden: c.hidden,
        }
    }
}

/// Clusters for the People view, sorted by photo count descending (028
/// decision #3). `include_hidden` is always `false` from the People view
/// itself — Slice C has no "manage hidden clusters" surface yet.
#[tauri::command]
fn list_face_clusters(state: tauri::State<Mutex<LibraryState>>, include_hidden: bool) -> CmdResult<Vec<FaceClusterDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::list_face_clusters(&conn, include_hidden)?.into_iter().map(Into::into).collect())
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersonDto {
    id: i64,
    name: String,
}

/// Every named person, for the naming input's autocomplete (028 decision
/// #3 — naming the same person twice must attach to one identity).
#[tauri::command]
fn list_persons(state: tauri::State<Mutex<LibraryState>>) -> CmdResult<Vec<PersonDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::list_persons(&conn)?.into_iter().map(|p| PersonDto { id: p.id, name: p.name }).collect())
    })
}

/// Names (or renames) a cluster — the confirmation gate (028 decision #2).
#[tauri::command]
fn name_face_cluster(state: tauri::State<Mutex<LibraryState>>, cluster_id: i64, person_name: String) -> CmdResult<i64> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::name_cluster(&conn, cluster_id, &person_name)?)
    })
}

/// Reversible Hide/unhide (028 decision #3) — never deletes detections.
#[tauri::command]
fn set_face_cluster_hidden(state: tauri::State<Mutex<LibraryState>>, cluster_id: i64, hidden: bool) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lenslocker_catalog::set_cluster_hidden(&conn, cluster_id, hidden)?;
        Ok(())
    })
}

/// Merges two clusters (028 decision #4, Milestone ML-4 Slice D2) —
/// `resulting_person_name` is whatever the People view's merge
/// confirmation card already resolved (one side's name when there was no
/// conflict, or the human's pick/typed name when both sides disagreed);
/// see [`lenslocker_catalog::merge_clusters`] for why `None` means "leave
/// the keeper's name untouched" rather than "unname it."
#[tauri::command]
fn merge_face_clusters(
    state: tauri::State<Mutex<LibraryState>>,
    keeper_cluster_id: i64,
    loser_cluster_id: i64,
    resulting_person_name: Option<String>,
) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lenslocker_catalog::merge_clusters(&conn, keeper_cluster_id, loser_cluster_id, resulting_person_name.as_deref())?;
        Ok(())
    })
}

/// A cluster's individual member face crops (028 decision #3: "click a
/// cluster, see its member thumbnails + photo count") — also Slice D3's
/// Split selection source, since each crop carries its own `detectionId`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FaceCropDto {
    detection_id: i64,
    crop_thumbnail_path: String,
}

impl From<lenslocker_catalog::FaceCrop> for FaceCropDto {
    fn from(c: lenslocker_catalog::FaceCrop) -> Self {
        Self { detection_id: c.detection_id, crop_thumbnail_path: c.crop_thumbnail_path }
    }
}

#[tauri::command]
fn list_cluster_face_crops(state: tauri::State<Mutex<LibraryState>>, cluster_id: i64) -> CmdResult<Vec<FaceCropDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::face_crops_for_cluster(&conn, cluster_id)?.into_iter().map(Into::into).collect())
    })
}

/// Split (028 decision #4): moves the selected face detections out of
/// whatever cluster(s) they're in and into one new cluster, optionally
/// named. See [`lenslocker_catalog::move_detections_to_new_cluster`] for
/// why "move to an existing person" and "move to a new group" are the
/// same operation here.
#[tauri::command]
fn move_face_detections_to_new_cluster(
    state: tauri::State<Mutex<LibraryState>>,
    detection_ids: Vec<i64>,
    person_name: Option<String>,
) -> CmdResult<i64> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::move_detections_to_new_cluster(&conn, &detection_ids, person_name.as_deref())?)
    })
}

/// The People nav badge count — mirrors [`list_review_queue`]'s
/// badge-via-length pattern. [`list_pending_face_matches`] is what it
/// leads to (Milestone ML-4 Slice D1).
#[tauri::command]
fn pending_face_review_count(state: tauri::State<Mutex<LibraryState>>) -> CmdResult<i64> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::pending_face_review_count(&conn)?)
    })
}

/// A pending §6-tier-2 match, wire shape for the People view's "Needs
/// review" section (Milestone ML-4 Slice D1) — mirrors [`ReviewQueueEntryDto`]'s
/// role for dedupe.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingFaceMatchDto {
    queue_id: i64,
    face_detection_id: i64,
    image_id: i64,
    crop_thumbnail_path: Option<String>,
    suggested_person_id: i64,
    suggested_person_name: String,
    similarity_score: f64,
}

impl From<lenslocker_catalog::PendingFaceMatch> for PendingFaceMatchDto {
    fn from(m: lenslocker_catalog::PendingFaceMatch) -> Self {
        Self {
            queue_id: m.queue_id,
            face_detection_id: m.face_detection_id,
            image_id: m.image_id,
            crop_thumbnail_path: m.crop_thumbnail_path,
            suggested_person_id: m.suggested_person_id,
            suggested_person_name: m.suggested_person_name,
            similarity_score: m.similarity_score,
        }
    }
}

#[tauri::command]
fn list_pending_face_matches(state: tauri::State<Mutex<LibraryState>>) -> CmdResult<Vec<PendingFaceMatchDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::list_pending_face_matches(&conn)?.into_iter().map(Into::into).collect())
    })
}

/// "Yes, this is also {suggested person}" — see
/// [`lenslocker_catalog::confirm_face_match`] for why this always creates
/// a fresh cluster rather than attaching to an existing one.
#[tauri::command]
fn confirm_face_match(state: tauri::State<Mutex<LibraryState>>, queue_id: i64) -> CmdResult<i64> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::confirm_face_match(&conn, queue_id)?)
    })
}

/// "No, not the same person" — falls back to an ordinary unnamed cluster;
/// see [`lenslocker_catalog::dismiss_face_match`] for why this doesn't
/// re-run real clustering.
#[tauri::command]
fn dismiss_face_match(state: tauri::State<Mutex<LibraryState>>, queue_id: i64) -> CmdResult<i64> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::dismiss_face_match(&conn, queue_id)?)
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

// ── Saved albums (ML-SPEC.md §7, ticket 031, Milestone ML-5) ───────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SavedAlbumDto {
    id: i64,
    name: String,
    filters: String,
    created_at: String,
}

impl From<lenslocker_catalog::SavedAlbum> for SavedAlbumDto {
    fn from(a: lenslocker_catalog::SavedAlbum) -> Self {
        Self { id: a.id, name: a.name, filters: a.filters, created_at: a.created_at }
    }
}

#[tauri::command]
fn list_saved_albums(state: tauri::State<Mutex<LibraryState>>) -> CmdResult<Vec<SavedAlbumDto>> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::list_saved_albums(&conn)?.into_iter().map(Into::into).collect())
    })
}

/// `filters` is an opaque JSON blob to this whole layer — assembled and
/// consumed entirely by the frontend (`filtersDto()` + `sort` + `search`),
/// never parsed here or in `catalog`. See [`lenslocker_catalog::save_album`]
/// for why this always inserts a new row rather than upserting by name.
#[tauri::command]
fn save_album(state: tauri::State<Mutex<LibraryState>>, name: String, filters: String) -> CmdResult<i64> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        Ok(lenslocker_catalog::save_album(&conn, &name, &filters)?)
    })
}

#[tauri::command]
fn delete_saved_album(state: tauri::State<Mutex<LibraryState>>, id: i64) -> CmdResult<()> {
    with_ready(&state, |app_state| {
        let conn = app_state.conn.lock().unwrap();
        lenslocker_catalog::delete_saved_album(&conn, id)?;
        Ok(())
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
            find_similar_images,
            search_by_text,
            add_tag,
            remove_tag,
            confirm_auto_tag,
            reject_auto_tag,
            bulk_add_tag,
            bulk_remove_tag,
            list_tags,
            list_sources,
            list_saved_albums,
            save_album,
            delete_saved_album,
            list_face_clusters,
            list_persons,
            name_face_cluster,
            set_face_cluster_hidden,
            merge_face_clusters,
            list_cluster_face_crops,
            move_face_detections_to_new_cluster,
            pending_face_review_count,
            list_pending_face_matches,
            confirm_face_match,
            dismiss_face_match,
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
                similarity_search_floor: 0.5,
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

    fn tag(source: &str, confidence: Option<f64>) -> lenslocker_catalog::TagWithProvenance {
        lenslocker_catalog::TagWithProvenance { name: "x".to_string(), source: source.to_string(), confidence, review_state: None }
    }

    #[test]
    fn manual_tags_are_always_visible_by_default() {
        assert!(tag_is_visible_by_default(&tag("manual", None), 0.5));
    }

    #[test]
    fn auto_tags_below_the_display_threshold_are_hidden_by_default() {
        assert!(!tag_is_visible_by_default(&tag("auto", Some(0.2)), 0.5));
    }

    #[test]
    fn auto_tags_at_or_above_the_display_threshold_are_visible() {
        assert!(tag_is_visible_by_default(&tag("auto", Some(0.5)), 0.5));
        assert!(tag_is_visible_by_default(&tag("auto", Some(0.9)), 0.5));
    }

    /// `bulk_add_tag`/`bulk_remove_tag` (Milestone ML-4 Slice B) are thin
    /// loops over `lenslocker_catalog::add_tag`/`remove_tag` with no
    /// `#[tauri::command]`-specific logic of their own — this repo has no
    /// established pattern for constructing a real `tauri::State` outside
    /// a running app (grepped: no other test does), so rather than force
    /// that scaffolding for two trivial loops, this exercises the exact
    /// real behavior those loops depend on directly against a real
    /// library/connection: an id that doesn't exist trips the `images`
    /// foreign-key constraint (`migrate`'s own doc comment: FK enforcement
    /// is on for every connection), and ids processed *before* the
    /// failure stay tagged — matching the "no transactions anywhere in
    /// this codebase, partial application on error" contract documented
    /// on `bulk_add_tag` itself.
    #[test]
    fn bulk_add_tag_loop_stops_on_a_bad_id_but_leaves_earlier_ids_tagged() {
        let dir = tempfile::tempdir().unwrap();
        let app_state = create_library_at(&dir.path().join("vault"), true).unwrap();
        let conn = app_state.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO images (
                library_id, original_hash, stored_hash, stored_path,
                original_format, stored_format, file_size_bytes
            ) VALUES (?1, x'01', x'01', 'a', 'jpeg', 'jpeg', 0)",
            rusqlite::params![app_state.library_id],
        )
        .unwrap();
        let good_id = conn.last_insert_rowid();
        let bad_id = good_id + 999; // no such image

        let mut result = Ok(());
        for image_id in [good_id, bad_id] {
            result = lenslocker_catalog::add_tag(&conn, image_id, "beach");
            if result.is_err() {
                break;
            }
        }

        assert!(result.is_err(), "a nonexistent image id must trip the images FK constraint");
        assert_eq!(
            lenslocker_catalog::tags_for_image(&conn, good_id).unwrap(),
            vec!["beach".to_string()],
            "the id processed before the failing one must stay tagged, not be rolled back"
        );
    }

    #[test]
    fn bulk_add_tag_loop_is_idempotent_across_a_retry_of_the_same_selection() {
        let dir = tempfile::tempdir().unwrap();
        let app_state = create_library_at(&dir.path().join("vault"), true).unwrap();
        let conn = app_state.conn.lock().unwrap();

        conn.execute(
            "INSERT INTO images (
                library_id, original_hash, stored_hash, stored_path,
                original_format, stored_format, file_size_bytes
            ) VALUES (?1, x'02', x'02', 'a', 'jpeg', 'jpeg', 0)",
            rusqlite::params![app_state.library_id],
        )
        .unwrap();
        let image_id = conn.last_insert_rowid();

        lenslocker_catalog::add_tag(&conn, image_id, "beach").unwrap();
        lenslocker_catalog::add_tag(&conn, image_id, "beach").unwrap(); // retry, e.g. after a bad_id further down the same selection failed

        assert_eq!(lenslocker_catalog::tags_for_image(&conn, image_id).unwrap(), vec!["beach".to_string()]);
    }

    /// `bulk_remove_tag` itself is a thin loop with no `#[tauri::command]`
    /// logic of its own (same reasoning as the two tests above) — this
    /// exercises the real per-image routing decision it makes: an
    /// auto-sourced tag goes through `reject_tag` (persists the
    /// rejection), a manual one through plain `remove_tag`, even when
    /// both images are in the same bulk selection with the same tag name.
    #[test]
    fn bulk_remove_routes_auto_tags_through_reject_and_manual_tags_through_remove() {
        let dir = tempfile::tempdir().unwrap();
        let app_state = create_library_at(&dir.path().join("vault"), true).unwrap();
        let conn = app_state.conn.lock().unwrap();

        let insert_image = |hash: u8| -> i64 {
            conn.execute(
                "INSERT INTO images (
                    library_id, original_hash, stored_hash, stored_path,
                    original_format, stored_format, file_size_bytes
                ) VALUES (?1, ?2, x'00', 'a', 'jpeg', 'jpeg', 0)",
                rusqlite::params![app_state.library_id, vec![hash]],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let manual_image = insert_image(1);
        let auto_image = insert_image(2);

        lenslocker_catalog::add_tag(&conn, manual_image, "beach").unwrap();
        lenslocker_catalog::apply_auto_tag(&conn, auto_image, "beach", 0.9).unwrap();

        // Mirrors bulk_remove_tag's own per-image routing decision (the
        // command itself isn't callable here — no tauri::State outside a
        // running app; see the tests above).
        for image_id in [manual_image, auto_image] {
            let source = lenslocker_catalog::tag_source_for_image(&conn, image_id, "beach").unwrap();
            if source.as_deref() == Some("auto") {
                lenslocker_catalog::reject_tag(&conn, image_id, "beach").unwrap();
            } else {
                lenslocker_catalog::remove_tag(&conn, image_id, "beach").unwrap();
            }
        }

        assert_eq!(lenslocker_catalog::tags_for_image(&conn, manual_image).unwrap(), Vec::<String>::new());
        assert_eq!(lenslocker_catalog::tags_for_image(&conn, auto_image).unwrap(), Vec::<String>::new());

        // The auto image's rejection must persist — re-scoring must not
        // silently reapply it; the manual image has no such memory.
        lenslocker_catalog::apply_auto_tag(&conn, auto_image, "beach", 0.95).unwrap();
        assert_eq!(lenslocker_catalog::tags_for_image(&conn, auto_image).unwrap(), Vec::<String>::new(), "rejection must survive a re-score");
    }
}
