//! Import pipeline orchestration: journal, dedupe-check, quarantine, per
//! workplan/SPEC.md §3.
//!
//! Milestone 1 built the six-step pipeline minus conversion, perceptual
//! dedupe/RAW+JPEG pairing, and XMP sidecars. Milestone 2 added conversion.
//! Milestone 3 (this milestone) adds:
//! - Perceptual hashing of every decode-validated image (§3 step 3), via
//!   [`lumenvault_hash::perceptual_hash`] over the already-decoded pixels
//!   `lumenvault_decode::probe` returns.
//! - RAW+JPEG pairing (§3 step 2) — see [`pair_raw_and_jpeg`].
//! - The `dedupe_review_queue` (§6) — see [`record_near_duplicates`].
//!
//! Milestone 4 adds the [`rebuild`] module: the rebuild-from-sidecars
//! recovery path (§7). Manual tagging itself is direct
//! `lumenvault-catalog`/`lumenvault-xmp` calls, not something this crate
//! wraps — no pipeline step needs it.
//!
//! **Design note on crash safety** (a genuine ambiguity SPEC.md §3 states as
//! a property, not a mechanism): every physical side effect below — blob
//! write, `images`/`image_sources` row insert, quarantine copy — is made
//! idempotent by checking existing state before acting, rather than trusting
//! the journal's recorded `step` as the sole source of truth. This means
//! `import_file` can simply be re-run in full for any file not yet `done`;
//! each sub-step safely no-ops if it already happened. The original source
//! file is never deleted until everything else (blob, catalog rows) is
//! durably committed — the only truly one-way action is the final
//! delete-source in the quarantine step, and it only runs after a verified
//! quarantine copy exists.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

pub mod rebuild;
pub use rebuild::{RebuildReport, rebuild_from_store};

/// Where a library's managed files live on disk, and the paths derived from it.
///
/// Layout (a Milestone 1 judgment call, not separately specified):
/// - `<root>/catalog.sqlite` — the SQLite catalog.
/// - `<root>/blobs/<first-2-hex-chars>/<full-hash-hex>.<ext>` — content-addressed store.
/// - `<root>/quarantine/<journal-id>/<original-filename>` — quarantined originals.
pub struct LibraryPaths {
    root: PathBuf,
}

impl LibraryPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn catalog_db(&self) -> PathBuf {
        self.root.join("catalog.sqlite")
    }

    fn blob_path(&self, hash_hex: &str, ext: &str) -> PathBuf {
        self.root.join("blobs").join(&hash_hex[0..2]).join(format!("{hash_hex}.{ext}"))
    }

    fn quarantine_path(&self, journal_id: i64, filename: &std::ffi::OsStr) -> PathBuf {
        self.root.join("quarantine").join(journal_id.to_string()).join(filename)
    }

    /// Where a merge-discarded image's *stored blob* (not an import
    /// original) gets quarantined — the `quarantine_type = 'merge_discard'`
    /// case (workplan/SPEC.md §3/§6/§10), keyed by the loser's image id
    /// rather than a journal id since a merge isn't a journal-tracked event.
    fn merge_quarantine_path(&self, image_id: i64, filename: &std::ffi::OsStr) -> PathBuf {
        self.root.join("quarantine").join(format!("merge-{image_id}")).join(filename)
    }

    /// Where a generated grid thumbnail lives — under the library root
    /// (workplan/SPEC.md §9), content-addressed by the image's stored hash
    /// like the blob store, so re-generating a thumbnail always lands at the
    /// same path.
    fn thumbnail_path(&self, hash_hex: &str) -> PathBuf {
        self.root.join("thumbnails").join(&hash_hex[0..2]).join(format!("{hash_hex}.jpg"))
    }

    /// Where a generated full-resolution browser-displayable preview lives —
    /// see [`lumenvault_decode::write_jpeg_preview`] for why this exists
    /// (a `.jxl` blob has no browser-native form, at any size). Same
    /// content-addressing scheme as `thumbnail_path`, in its own `previews/`
    /// subtree so the two variants never collide.
    fn preview_path(&self, hash_hex: &str) -> PathBuf {
        self.root.join("previews").join(&hash_hex[0..2]).join(format!("{hash_hex}.jpg"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(
        "blob integrity check failed for {path}: expected the rewritten copy to hash the same \
         as the source, but it didn't"
    )]
    BlobIntegrity { path: PathBuf },
    #[error("walking {root}: {source}")]
    Walk { root: PathBuf, source: walkdir::Error },
    #[error(transparent)]
    Xmp(#[from] lumenvault_xmp::XmpError),
    #[error(transparent)]
    Thumbnail(#[from] lumenvault_decode::ThumbnailError),
    #[error("review-queue entry {0} not found")]
    ReviewQueueEntryNotFound(i64),
    #[error("review-queue entry {0} is not pending (already resolved)")]
    ReviewQueueEntryNotPending(i64),
    #[error("keeper_id {keeper} is not one of the review-queue entry's two images ({a}, {b})")]
    InvalidKeeper { keeper: i64, a: i64, b: i64 },
}

/// Outcome of importing one source file.
#[derive(Debug, PartialEq, Eq)]
pub enum FileOutcome {
    /// New content: a fresh `images` row was created.
    Imported { image_id: i64 },
    /// Exact-hash match against an already-imported image (§6's auto-collapse).
    Collapsed { image_id: i64 },
    /// Not a decodable standard-format image; left untouched at its source path.
    Failed,
    /// Already fully processed by a prior run (crash-recovery no-op).
    AlreadyDone { image_id: Option<i64> },
}

/// The (connection, store layout, library, batch, conversion setting) tuple
/// every pipeline operation needs — bundled because `import_file` and
/// `import_directory` both thread the same values through every call.
/// `conversion_enabled` is read once by the caller (via
/// [`conversion_enabled`]) rather than re-queried per file: it's a
/// per-library, fixed-at-creation setting (workplan/SPEC.md §4), not
/// per-file state.
#[derive(Clone, Copy)]
pub struct ImportContext<'a> {
    pub conn: &'a Connection,
    pub paths: &'a LibraryPaths,
    pub library_id: i64,
    pub batch_id: i64,
    pub conversion_enabled: bool,
}

/// Finds or creates the `libraries` row for `root_path`. Idempotent —
/// re-running against the same root always returns the same id.
pub fn ensure_library(conn: &Connection, root_path: &Path) -> rusqlite::Result<i64> {
    let root = root_path.to_string_lossy();

    if let Some(id) = conn
        .query_row("SELECT id FROM libraries WHERE root_path = ?1", [root.as_ref()], |row| row.get(0))
        .optional()?
    {
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO libraries (name, root_path) VALUES (?1, ?2)",
        params![root_path.file_name().map(|n| n.to_string_lossy()).unwrap_or(root.clone()), root.as_ref()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Creates a fresh `libraries` row with `conversion_enabled` set explicitly
/// at creation time (workplan/SPEC.md §4/ticket 009: the setting is fixed
/// for the life of the vault, so it must be chosen *when the row is made*,
/// not defaulted and edited later). Unlike [`ensure_library`] this is not
/// idempotent — it's only called from the "new vault" branch of first-run
/// setup (Milestone 5.5), where the caller has already confirmed (by
/// checking for an existing `catalog.sqlite`) that no library exists at
/// this path yet.
pub fn create_library_row(conn: &Connection, root_path: &Path, conversion_enabled: bool) -> rusqlite::Result<i64> {
    let root = root_path.to_string_lossy();
    conn.execute(
        "INSERT INTO libraries (name, root_path, conversion_enabled) VALUES (?1, ?2, ?3)",
        params![
            root_path.file_name().map(|n| n.to_string_lossy()).unwrap_or(root.clone()),
            root.as_ref(),
            conversion_enabled as i64,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Reads `libraries.conversion_enabled` for `library_id` — the "opt-out
/// granularity: per-library, fixed at library creation" setting from
/// workplan/SPEC.md §4.
pub fn conversion_enabled(conn: &Connection, library_id: i64) -> rusqlite::Result<bool> {
    let enabled: i64 =
        conn.query_row("SELECT conversion_enabled FROM libraries WHERE id = ?1", [library_id], |row| row.get(0))?;
    Ok(enabled != 0)
}

/// Finds a still-`running` batch for this (library, source) pair to resume,
/// or starts a new one. This is what makes "run the same command again"
/// naturally resume an interrupted import rather than double-importing.
pub fn start_or_resume_batch(
    conn: &Connection,
    library_id: i64,
    source_root: &Path,
) -> rusqlite::Result<i64> {
    let source = source_root.to_string_lossy();

    if let Some(id) = conn
        .query_row(
            "SELECT id FROM import_batches
             WHERE library_id = ?1 AND source_root = ?2 AND status = 'running'",
            params![library_id, source.as_ref()],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO import_batches (library_id, source_root) VALUES (?1, ?2)",
        params![library_id, source.as_ref()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn complete_batch(conn: &Connection, batch_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE import_batches SET status = 'completed', completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1",
        [batch_id],
    )?;
    Ok(())
}

/// Launch-only retention sweep (workplan/SPEC.md §3): permanently deletes
/// any quarantined file whose retention window has expired.
pub fn sweep_expired_quarantine(conn: &Connection) -> Result<usize, ImportError> {
    let mut stmt = conn.prepare(
        "SELECT id, quarantined_path FROM quarantine
         WHERE expires_at < strftime('%Y-%m-%dT%H:%M:%fZ','now') AND purged_at IS NULL",
    )?;
    let expired: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let mut purged = 0;
    for (id, path) in expired {
        // Already-missing is fine — nothing to clean up, just record it purged.
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(ImportError::Io(e)),
        }
        conn.execute(
            "UPDATE quarantine SET purged_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
            [id],
        )?;
        purged += 1;
    }
    Ok(purged)
}

/// Launch-only backfill (same pattern as [`sweep_expired_quarantine`]):
/// generates the `preview_full` thumbnail variant (see
/// `LibraryPaths::preview_path` / `lumenvault_decode::write_jpeg_preview`)
/// for any active image that doesn't have one yet — images imported before
/// that variant existed. Best-effort per image: a blob that fails to
/// re-decode (missing file, RAW with no decoded pixels) is skipped rather
/// than aborting the whole backfill, since one bad image shouldn't block
/// previews for the rest of the library.
pub fn backfill_previews(conn: &Connection, paths: &LibraryPaths) -> Result<usize, ImportError> {
    let mut stmt = conn.prepare(
        "SELECT im.id, im.original_hash, im.stored_path FROM images im
         LEFT JOIN thumbnails th ON th.image_id = im.id AND th.variant = 'preview_full'
         WHERE th.id IS NULL AND im.status = 'active'",
    )?;
    let candidates: Vec<(i64, Vec<u8>, String)> =
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?.collect::<Result<_, _>>()?;
    drop(stmt);

    let mut backfilled = 0;
    for (image_id, original_hash, stored_path) in candidates {
        let Ok(probe) = lumenvault_decode::probe(Path::new(&stored_path)) else {
            continue; // undecodable (missing file) or RAW with no pixels — nothing to generate from
        };
        let original_hash_hex: String = original_hash.iter().map(|b| format!("{b:02x}")).collect();
        let preview_path = paths.preview_path(&original_hash_hex);
        lumenvault_decode::write_jpeg_preview(&probe.image, &preview_path)?;
        lumenvault_catalog::record_thumbnail(conn, image_id, "preview_full", "jpeg", &preview_path.to_string_lossy())?;
        backfilled += 1;
    }
    Ok(backfilled)
}

/// Recursively imports every regular file under `source_root`, invoking
/// `on_outcome` after each one (callers that don't need progress reporting
/// can pass `|_, _| {}`). Files that aren't decodable standard-format images
/// are skipped (journaled `failed`), not fatal to the batch.
pub fn import_directory(
    ctx: &ImportContext,
    source_root: &Path,
    mut on_outcome: impl FnMut(&Path, &FileOutcome),
) -> Result<(), ImportError> {
    for entry in walkdir::WalkDir::new(source_root) {
        let entry = entry.map_err(|source| ImportError::Walk { root: source_root.to_path_buf(), source })?;
        if entry.file_type().is_file() {
            let outcome = import_file(ctx, entry.path())?;
            on_outcome(entry.path(), &outcome);
        }
    }
    complete_batch(ctx.conn, ctx.batch_id)?;
    Ok(())
}

/// Imports a single source file, per the module doc's crash-safety design:
/// every side effect below is idempotent, so re-running this on a file left
/// mid-pipeline by a killed process is always safe.
pub fn import_file(ctx: &ImportContext, source_path: &Path) -> Result<FileOutcome, ImportError> {
    let ImportContext { conn, paths, library_id, batch_id, conversion_enabled } = *ctx;
    let source_str = source_path.to_string_lossy();

    // Resume check: if a prior run already finished this file, don't redo it.
    let existing_journal: Option<(i64, String, Option<i64>)> = conn
        .query_row(
            "SELECT id, step, image_id FROM import_journal WHERE batch_id = ?1 AND source_path = ?2",
            params![batch_id, source_str.as_ref()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let journal_id = match &existing_journal {
        Some((_, step, image_id)) if step == "done" => {
            return Ok(FileOutcome::AlreadyDone { image_id: *image_id });
        }
        Some((_, step, _)) if step == "failed" => {
            return Ok(FileOutcome::Failed);
        }
        Some((id, _, _)) => *id,
        None => {
            conn.execute(
                "INSERT INTO import_journal (batch_id, source_path, step) VALUES (?1, ?2, 'pending')",
                params![batch_id, source_str.as_ref()],
            )?;
            conn.last_insert_rowid()
        }
    };

    // Step: hash source bytes.
    let original_hash = lumenvault_hash::hash_file(source_path)?;
    let original_hash_hex = lumenvault_hash::to_hex(&original_hash);
    set_journal_step(conn, journal_id, "hashed", None)?;

    // Step: decode-validate, or fall back to RAW-extension recognition
    // (§5's "Full" RAW support isn't scheduled in any milestone yet — see
    // lumenvault-decode's module doc). A file that's neither decodable nor
    // a recognized RAW extension is refused, not imported — the original is
    // left untouched at its source path. Perceptual hash (§3 step 3) is
    // computed here too, from the pixels probe() already decoded; RAW files
    // get `None`, which both keeps `perceptual_hash` NULL in the catalog
    // and — since NULL never matches in `record_near_duplicates` — is what
    // excludes them from perceptual dedupe candidacy without extra logic.
    let (classified, decoded_image) = match lumenvault_decode::probe(source_path) {
        Ok(probe) => {
            let hash = lumenvault_hash::perceptual_hash(&probe.image);
            let classified = Classified {
                format: probe.format.as_str(),
                width: Some(probe.width),
                height: Some(probe.height),
                perceptual_hash: Some(hash as i64),
            };
            (classified, Some(probe.image))
        }
        Err(e) => match lumenvault_decode::raw_extension(source_path) {
            Some(raw_format) => {
                (Classified { format: raw_format, width: None, height: None, perceptual_hash: None }, None)
            }
            None => {
                set_journal_step(conn, journal_id, "failed", Some(&e.to_string()))?;
                return Ok(FileOutcome::Failed);
            }
        },
    };
    let Classified { format, width, height, perceptual_hash } = classified;

    // Step: exact-hash dedupe check (§6's auto-collapse falls out of this
    // content-addressed UNIQUE constraint on images.original_hash).
    let existing_image: Option<(i64, Vec<u8>)> = conn
        .query_row(
            "SELECT id, stored_hash FROM images WHERE original_hash = ?1",
            [original_hash.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    set_journal_step(conn, journal_id, "dedupe_checked", None)?;

    let (image_id, outcome) = if let Some((image_id, _)) = existing_image {
        (image_id, FileOutcome::Collapsed { image_id })
    } else {
        // Step: convert (§4's matrix, attempt→verify→never-degrade). Runs
        // before the content-addressed write because *what* gets written
        // depends on the outcome — the source bytes verbatim, or the
        // already-verified converted bytes.
        let conversion = resolve_conversion(format, conversion_enabled, source_path)?;
        set_journal_step(conn, journal_id, "converted", None)?;

        let blob_path = paths.blob_path(&original_hash_hex, conversion.stored_format);
        match &conversion.bytes {
            Some(bytes) => write_blob_bytes_idempotent(bytes, &blob_path)?,
            None => write_blob_idempotent(source_path, &blob_path)?,
        }
        set_journal_step(conn, journal_id, "verified", None)?;

        // Re-hash what's actually on disk — this is stored_hash. When
        // nothing was converted, it also doubles as the "verify the
        // managed copy" check SPEC.md §3 requires before the original is
        // touched: the blob is supposed to be a verbatim copy, so a
        // mismatch means the write itself was corrupted. When conversion
        // *did* happen, stored_hash is expected to differ from
        // original_hash by design (different bytes) — `lumenvault-convert`
        // already did its own pixel/byte-exact verification internally
        // before ever reporting success, so no further equality check
        // applies here.
        let stored_hash = lumenvault_hash::hash_file(&blob_path)?;
        if conversion.bytes.is_none() && stored_hash != original_hash {
            return Err(ImportError::BlobIntegrity { path: blob_path });
        }

        let image_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM images WHERE original_hash = ?1",
                [original_hash.as_slice()],
                |row| row.get(0),
            )
            .optional()?;
        let image_id = match image_id {
            Some(id) => id,
            None => {
                conn.execute(
                    "INSERT INTO images (
                        library_id, original_hash, stored_hash, stored_path,
                        original_format, stored_format, conversion_status,
                        file_size_bytes, width, height, perceptual_hash
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        library_id,
                        original_hash.as_slice(),
                        stored_hash.as_slice(),
                        blob_path.to_string_lossy(),
                        format,
                        conversion.stored_format,
                        conversion.conversion_status,
                        fs::metadata(&blob_path)?.len() as i64,
                        width,
                        height,
                        perceptual_hash,
                    ],
                )?;
                let new_id = conn.last_insert_rowid();

                // Thumbnail generation (§9), at import time — reuses the
                // pixels `probe()` already decoded (see the module doc on
                // `decoded_image`) rather than re-decoding the blob. Content-
                // addressed by the *original* hash (same key as the blob
                // path) so it's stable and collision-free; skipped for RAW
                // files, which have no decoded pixels this milestone (see
                // lumenvault-decode's module doc) — a genuine, documented gap,
                // not silently swallowed.
                if let Some(image) = &decoded_image {
                    let thumb_path = paths.thumbnail_path(&original_hash_hex);
                    lumenvault_decode::write_jpeg_thumbnail(image, &thumb_path, 256)?;
                    lumenvault_catalog::record_thumbnail(conn, new_id, "grid256", "jpeg", &thumb_path.to_string_lossy())?;

                    // Full-resolution browser-displayable preview (see
                    // `LibraryPaths::preview_path`) — a stored `.jxl` blob
                    // has no form WebView2 can show in an `<img>` at all,
                    // regardless of size, so the lightbox needs this to show
                    // more than the 256px grid thumbnail.
                    let preview_path = paths.preview_path(&original_hash_hex);
                    lumenvault_decode::write_jpeg_preview(image, &preview_path)?;
                    lumenvault_catalog::record_thumbnail(conn, new_id, "preview_full", "jpeg", &preview_path.to_string_lossy())?;
                }

                new_id
            }
        };
        (image_id, FileOutcome::Imported { image_id })
    };

    // Record which image this journal entry resolved to, so a resumed run
    // reading a 'done' row back (see the resume check above) can report it.
    conn.execute("UPDATE import_journal SET image_id = ?1 WHERE id = ?2", params![image_id, journal_id])?;

    // image_sources row: idempotent, guards against a duplicate on resume.
    let source_row_exists: bool = conn
        .query_row(
            "SELECT 1 FROM image_sources WHERE image_id = ?1 AND import_batch_id = ?2 AND source_path = ?3",
            params![image_id, batch_id, source_str.as_ref()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !source_row_exists {
        conn.execute(
            "INSERT INTO image_sources (image_id, import_batch_id, original_filename, source_path)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                image_id,
                batch_id,
                source_path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                source_str.as_ref(),
            ],
        )?;
    }
    // Keeps the full-text search index (§9's live search filter, Milestone
    // 5) current with the latest-known filename — cheap to re-run
    // unconditionally, same idempotent-retry rationale as the rest of this
    // function.
    lumenvault_catalog::sync_fts_row(conn, image_id)?;

    // §3 steps 2 and 3. Run unconditionally (not just for a fresh
    // `Imported` outcome) so both stay correct under this module's
    // idempotent-retry design: a crash could land between the images-row
    // insert above and here, in which case a resumed run sees this content
    // as `Collapsed` (the row already exists) even though pairing/dedupe
    // matching for it never actually ran. Both helpers below guard their
    // own inserts against re-running, so calling them again here is safe
    // whether this is a fresh import, a resumed one, or a genuine
    // exact-duplicate re-import under a new filename.
    pair_raw_and_jpeg(conn, batch_id, image_id, format, source_path)?;
    if let Some(hash) = perceptual_hash {
        record_near_duplicates(conn, library_id, image_id, hash)?;
    }

    quarantine_original(conn, paths, library_id, journal_id, image_id, source_path)?;
    set_journal_step(conn, journal_id, "done", None)?;

    Ok(outcome)
}

// ── Review-queue resolution (workplan/SPEC.md §6, Milestone 5) ──────────
//
// Milestone 3 built detection only (`dedupe_review_queue` rows) — this is
// the first resolution logic. Lives in this crate (not `lumenvault-catalog`)
// because a merge does real file I/O (quarantining the loser's stored blob),
// which `catalog` deliberately stays free of.

/// What the reviewer decided for one `dedupe_review_queue` entry.
#[derive(Debug, Clone, Copy)]
pub enum ReviewAction {
    /// Union both images' tags onto `keeper_id`, quarantine the other
    /// image's stored blob, mark the loser `merged`.
    Merge { keeper_id: i64 },
    /// "Keep both" — no data changes beyond the queue row itself.
    Dismiss,
}

/// Resolves one review-queue entry per §6's exact merge UX: on merge, tags
/// union onto the keeper (reusing [`lumenvault_catalog::add_tag`]), the
/// loser's `images.status` becomes `merged` (with `merged_into_id`/
/// `merged_at` set), its **stored blob** (not an import original) is
/// quarantined under `quarantine_type = 'merge_discard'` — the same
/// retention mechanism as any other removed original, never a hard delete —
/// and the queue row itself is marked `merged`/`resolved_at`. On dismiss,
/// only the queue row changes.
///
/// Idempotent: resolving an already-resolved entry a second time is a
/// silent no-op rather than an error, so a double-click or a retried IPC
/// call from the UI can't corrupt state.
pub fn resolve_review_pair(
    conn: &Connection,
    paths: &LibraryPaths,
    queue_id: i64,
    action: ReviewAction,
) -> Result<(), ImportError> {
    let entry: Option<(i64, i64, String)> = conn
        .query_row(
            "SELECT image_a_id, image_b_id, status FROM dedupe_review_queue WHERE id = ?1",
            [queue_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((image_a, image_b, status)) = entry else {
        return Err(ImportError::ReviewQueueEntryNotFound(queue_id));
    };
    if status != "pending" {
        // Already resolved (a prior call, or a UI double-click) — no-op,
        // not an error, to keep this safe to retry.
        return Ok(());
    }

    match action {
        ReviewAction::Dismiss => {
            conn.execute(
                "UPDATE dedupe_review_queue SET status = 'dismissed', resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id = ?1",
                [queue_id],
            )?;
        }
        ReviewAction::Merge { keeper_id } => {
            let loser_id = if keeper_id == image_a {
                image_b
            } else if keeper_id == image_b {
                image_a
            } else {
                return Err(ImportError::InvalidKeeper { keeper: keeper_id, a: image_a, b: image_b });
            };

            // Tags union onto the keeper (§6).
            for tag in lumenvault_catalog::tags_for_image(conn, loser_id)? {
                lumenvault_catalog::add_tag(conn, keeper_id, &tag)?;
            }
            lumenvault_xmp::sync_sidecar(conn, keeper_id)?;

            // Quarantine the loser's stored blob — quarantine_type =
            // 'merge_discard', the schema's dedicated case for this
            // (workplan/SPEC.md §3/§10), distinct from an import-time
            // original.
            let (library_id, stored_path): (i64, String) = conn.query_row(
                "SELECT library_id, stored_path FROM images WHERE id = ?1",
                [loser_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let stored_path = PathBuf::from(stored_path);
            if stored_path.exists() {
                let filename = stored_path.file_name().unwrap_or_default();
                let quarantine_path = paths.merge_quarantine_path(loser_id, filename);
                if let Some(parent) = quarantine_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                // Same volume as the blob store (both under the library
                // root) — a plain rename is always correct here, unlike
                // import-time quarantine's cross-volume case.
                fs::rename(&stored_path, &quarantine_path)?;

                let retention_days: i64 = conn.query_row(
                    "SELECT retention_days FROM app_settings WHERE id = 1",
                    [],
                    |row| row.get(0),
                )?;
                conn.execute(
                    "INSERT INTO quarantine (
                        library_id, quarantine_type, related_image_id, quarantined_path,
                        original_path, expires_at
                    ) VALUES (?1, 'merge_discard', ?2, ?3, ?4,
                              strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?5 || ' days'))",
                    params![
                        library_id,
                        loser_id,
                        quarantine_path.to_string_lossy(),
                        stored_path.to_string_lossy(),
                        retention_days,
                    ],
                )?;
            }

            conn.execute(
                "UPDATE images SET status = 'merged', merged_into_id = ?1, merged_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id = ?2",
                params![keeper_id, loser_id],
            )?;
            conn.execute(
                "UPDATE dedupe_review_queue SET status = 'merged', resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id = ?1",
                [queue_id],
            )?;
        }
    }

    Ok(())
}

// ── Export (workplan/SPEC.md §7, amended into v1 scope for Milestone 5) ──

/// Copies `image_id`'s stored file plus its `.xmp` sidecar (if one exists)
/// to `dest_dir`, keeping the currently-stored format verbatim — no format
/// round-trip, per §7's amendment. Returns the destination file path.
pub fn export_image(conn: &Connection, image_id: i64, dest_dir: &Path) -> Result<PathBuf, ImportError> {
    let (stored_path, sidecar_path): (String, Option<String>) = conn.query_row(
        "SELECT stored_path, sidecar_path FROM images WHERE id = ?1",
        [image_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let stored_path = Path::new(&stored_path);

    fs::create_dir_all(dest_dir)?;
    let filename = stored_path.file_name().ok_or_else(|| ImportError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("stored path has no filename: {}", stored_path.display()),
    )))?;
    let dest_file = dest_dir.join(filename);
    fs::copy(stored_path, &dest_file)?;

    if let Some(sidecar_path) = sidecar_path {
        let sidecar_path = Path::new(&sidecar_path);
        if sidecar_path.exists() {
            if let Some(sidecar_name) = sidecar_path.file_name() {
                fs::copy(sidecar_path, dest_dir.join(sidecar_name))?;
            }
        }
    }

    Ok(dest_file)
}

/// §3 step 2: RAW+JPEG camera-pair auto-detection, run before perceptual
/// matching (§3 step 3) so a bonded pair never gets a chance to also land in
/// the review queue. Matches this file's image against every other image
/// already imported in the *same batch* whose filename stem (case-
/// insensitive, extension stripped) is identical and whose format is the
/// opposite side of a RAW/JPEG pair.
///
/// Capture-date proximity — §3's named secondary signal — isn't used: the
/// RAW side never gets a decoded `capture_date` this milestone (extension-
/// only recognition, no EXIF parsing for RAW), so it's never available to
/// compare against. Stem-matching within the batch is sufficient signal on
/// its own for the realistic camera case (a RAW+JPEG pair sharing one
/// filename stem from the same shutter press) — building a proximity system
/// on top of data that doesn't exist yet would be scope creep the spec
/// doesn't demand.
fn pair_raw_and_jpeg(
    conn: &Connection,
    batch_id: i64,
    image_id: i64,
    format: &str,
    source_path: &Path,
) -> rusqlite::Result<()> {
    let is_raw = lumenvault_decode::is_raw_extension(format);
    let is_jpeg = format == "jpeg";
    if !is_raw && !is_jpeg {
        return Ok(());
    }
    let stem = filename_stem_lower(source_path);

    let mut stmt = conn.prepare(
        "SELECT im.id, im.original_format, s.source_path
         FROM images im
         JOIN image_sources s ON s.image_id = im.id
         WHERE s.import_batch_id = ?1 AND im.id != ?2",
    )?;
    let candidates: Vec<(i64, String, String)> = stmt
        .query_map(params![batch_id, image_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    for (other_id, other_format, other_source_path) in candidates {
        if filename_stem_lower(Path::new(&other_source_path)) != stem {
            continue;
        }
        let pair = if is_raw && other_format == "jpeg" {
            Some((image_id, other_id))
        } else if is_jpeg && lumenvault_decode::is_raw_extension(&other_format) {
            Some((other_id, image_id))
        } else {
            continue;
        };
        let Some((raw_image_id, jpeg_image_id)) = pair else { continue };

        let already_paired: bool = conn
            .query_row(
                "SELECT 1 FROM raw_jpeg_pairs WHERE raw_image_id = ?1 AND jpeg_image_id = ?2",
                params![raw_image_id, jpeg_image_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !already_paired {
            conn.execute(
                "INSERT INTO raw_jpeg_pairs (raw_image_id, jpeg_image_id) VALUES (?1, ?2)",
                params![raw_image_id, jpeg_image_id],
            )?;
        }
    }
    Ok(())
}

/// A path's filename stem (extension stripped), lowercased for
/// case-insensitive comparison — `IMG_0001.CR2` must match `img_0001.jpg`.
fn filename_stem_lower(path: &Path) -> String {
    path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// §3 step 3 + §6: compares this newly-hashed image against every other
/// **active** (not `merged`) image in the same library and files a
/// `dedupe_review_queue` row for every match within
/// `app_settings.hamming_threshold` (a global setting, read here rather
/// than hardcoded).
///
/// Deliberate choice, since §6 only says near-duplicates "always route to a
/// human review queue" without saying whether that means every match or
/// only the closest one: this records **every** match within threshold, not
/// just the nearest. A human reviewing later should see all real
/// candidates rather than have some silently dropped because a closer one
/// happened to exist — the more literal reading of "always route to
/// review," flagged here since the spec doesn't spell out this choice
/// explicitly.
fn record_near_duplicates(
    conn: &Connection,
    library_id: i64,
    image_id: i64,
    perceptual_hash: i64,
) -> rusqlite::Result<()> {
    let threshold: i64 =
        conn.query_row("SELECT hamming_threshold FROM app_settings WHERE id = 1", [], |row| row.get(0))?;

    let mut stmt = conn.prepare(
        "SELECT id, perceptual_hash FROM images
         WHERE library_id = ?1 AND id != ?2 AND status = 'active' AND perceptual_hash IS NOT NULL",
    )?;
    let candidates: Vec<(i64, i64)> = stmt
        .query_map(params![library_id, image_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    for (other_id, other_hash) in candidates {
        let distance = lumenvault_hash::hamming_distance(perceptual_hash as u64, other_hash as u64);
        if i64::from(distance) > threshold {
            continue;
        }

        let already_queued: bool = conn
            .query_row(
                "SELECT 1 FROM dedupe_review_queue
                 WHERE (image_a_id = ?1 AND image_b_id = ?2) OR (image_a_id = ?2 AND image_b_id = ?1)",
                params![image_id, other_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !already_queued {
            conn.execute(
                "INSERT INTO dedupe_review_queue (image_a_id, image_b_id, hamming_distance, status)
                 VALUES (?1, ?2, ?3, 'pending')",
                params![image_id, other_id, distance],
            )?;
        }
    }
    Ok(())
}

/// Writes `blob_path` from `source_path` if it doesn't already exist —
/// content-addressing means an existing file at that path already has the
/// right bytes, so a resumed retry can safely skip the copy.
fn write_blob_idempotent(source_path: &Path, blob_path: &Path) -> Result<(), ImportError> {
    if blob_path.exists() {
        return Ok(());
    }
    if let Some(parent) = blob_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source_path, blob_path)?;
    Ok(())
}

/// Writes `blob_path` from already-in-memory `bytes` (the converted output
/// of [`resolve_conversion`]) if it doesn't already exist — same
/// idempotency rationale as [`write_blob_idempotent`], just for bytes that
/// don't already live at a source path on disk.
fn write_blob_bytes_idempotent(bytes: &[u8], blob_path: &Path) -> Result<(), ImportError> {
    if blob_path.exists() {
        return Ok(());
    }
    if let Some(parent) = blob_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(blob_path, bytes)?;
    Ok(())
}

/// Maps a decode-validated source format to the §4 conversion path that
/// applies to it, if any. `None` means the format has no defined
/// conversion path at all (WebP, GIF, or anything outside JPEG/PNG/BMP/
/// TIFF) — exactly workplan/SPEC.md §4's `not_applicable` case.
fn convertible_format(format: &str) -> Option<lumenvault_convert::ConvertibleFormat> {
    use lumenvault_convert::ConvertibleFormat;
    match format {
        "jpeg" => Some(ConvertibleFormat::Jpeg),
        "png" => Some(ConvertibleFormat::Png),
        "bmp" => Some(ConvertibleFormat::Bmp),
        "tiff" => Some(ConvertibleFormat::Tiff),
        _ => None,
    }
}

/// What the decode-validate-or-RAW-recognize step (§3 steps 1-3's shared
/// prelude) determined about one source file, before conversion or dedupe
/// matching run. Grouped into a named struct — matching [`ConversionResult`]
/// below — rather than a same-shaped tuple, since these four fields always
/// travel together from here through the `images` row insert.
struct Classified {
    format: &'static str,
    width: Option<u32>,
    height: Option<u32>,
    perceptual_hash: Option<i64>,
}

/// Outcome of the §4 conversion attempt for one file. `bytes: None` means
/// "store the original bytes verbatim" (no conversion path, disabled by
/// library setting, or the attempt failed verification) — `stored_format`
/// and `conversion_status` together follow the mapping documented on
/// `images.conversion_status` in `crates/catalog/schema.sql`.
struct ConversionResult {
    bytes: Option<Vec<u8>>,
    stored_format: &'static str,
    conversion_status: &'static str,
}

/// Runs the §4 conversion policy for one file: attempt → verify →
/// never-degrade. Any verification failure inside `lumenvault-convert`, or
/// a genuine encode/library error, is treated identically — the original
/// is left untouched and `conversion_status` records `failed_kept_original`,
/// never a partial/best-effort result.
fn resolve_conversion(
    format: &'static str,
    conversion_enabled: bool,
    source_path: &Path,
) -> Result<ConversionResult, ImportError> {
    let Some(convertible) = convertible_format(format) else {
        return Ok(ConversionResult { bytes: None, stored_format: format, conversion_status: "not_applicable" });
    };
    if !conversion_enabled {
        return Ok(ConversionResult { bytes: None, stored_format: format, conversion_status: "not_attempted" });
    }

    let source_bytes = fs::read(source_path)?;
    Ok(match lumenvault_convert::convert(convertible, &source_bytes) {
        Ok(Ok(converted)) => ConversionResult {
            bytes: Some(converted.bytes),
            stored_format: converted.stored_format,
            conversion_status: "converted",
        },
        Ok(Err(_)) | Err(_) => {
            ConversionResult { bytes: None, stored_format: format, conversion_status: "failed_kept_original" }
        }
    })
}

/// Quarantines the original: same-volume rename where possible, falling
/// back to copy+verify+delete-source across volumes (workplan/SPEC.md §3 —
/// quarantine always ends up on the managed store's own volume). Idempotent:
/// if the source is already gone (a prior attempt already moved it), this
/// just ensures the quarantine row exists and returns.
fn quarantine_original(
    conn: &Connection,
    paths: &LibraryPaths,
    library_id: i64,
    journal_id: i64,
    image_id: i64,
    source_path: &Path,
) -> Result<(), ImportError> {
    let already_quarantined: bool = conn
        .query_row(
            "SELECT 1 FROM quarantine WHERE library_id = ?1 AND related_image_id = ?2 AND original_path = ?3",
            params![library_id, image_id, source_path.to_string_lossy()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if already_quarantined {
        return Ok(());
    }

    if !source_path.exists() {
        // Source already moved (prior attempt got this far but crashed
        // before the quarantine row committed) — nothing left to move, but
        // we have no record of where it went, which shouldn't be reachable
        // given the row-insert-before-delete ordering below. Treat as done.
        return Ok(());
    }

    let filename = source_path.file_name().unwrap_or_default();
    let quarantine_path = paths.quarantine_path(journal_id, filename);
    if let Some(parent) = quarantine_path.parent() {
        fs::create_dir_all(parent)?;
    }

    if !quarantine_path.exists() {
        if fs::rename(source_path, &quarantine_path).is_err() {
            // Cross-volume: rename fails (e.g. ERROR_NOT_SAME_DEVICE on
            // Windows) — copy, verify, then delete the source explicitly.
            fs::copy(source_path, &quarantine_path)?;
            let source_hash = lumenvault_hash::hash_file(source_path)?;
            let copy_hash = lumenvault_hash::hash_file(&quarantine_path)?;
            if source_hash != copy_hash {
                return Err(ImportError::BlobIntegrity { path: quarantine_path });
            }
            fs::remove_file(source_path)?;
        }
    } else if source_path.exists() {
        // Quarantine copy already exists from a prior attempt (crashed
        // between copy and delete) — finish the delete.
        fs::remove_file(source_path)?;
    }

    let retention_days: i64 = conn.query_row(
        "SELECT retention_days FROM app_settings WHERE id = 1",
        [],
        |row| row.get(0),
    )?;

    conn.execute(
        "INSERT INTO quarantine (
            library_id, quarantine_type, related_image_id, quarantined_path,
            original_path, expires_at
        ) VALUES (?1, 'import_original', ?2, ?3, ?4,
                  strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?5 || ' days'))",
        params![
            library_id,
            image_id,
            quarantine_path.to_string_lossy(),
            source_path.to_string_lossy(),
            retention_days,
        ],
    )?;

    Ok(())
}

fn set_journal_step(
    conn: &Connection,
    journal_id: i64,
    step: &str,
    error: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE import_journal
         SET step = ?1, error = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?3",
        params![step, error, journal_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn write_png(dir: &Path, name: &str, seed: u8) -> PathBuf {
        let path = dir.join(name);
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(8, 8, |x, y| Rgb([seed, (x % 256) as u8, (y % 256) as u8]));
        img.save(&path).unwrap();
        path
    }

    fn write_jpeg(dir: &Path, name: &str, seed: u8) -> PathBuf {
        let path = dir.join(name);
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(16, 16, |x, y| Rgb([seed, (x % 256) as u8, (y % 256) as u8]));
        img.save(&path).unwrap();
        path
    }

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        lumenvault_catalog::migrate(&mut conn).unwrap();
        conn
    }

    #[test]
    fn ensure_library_is_idempotent() {
        let conn = test_conn();
        let root = Path::new("A:/fake/library");

        let first = ensure_library(&conn, root).unwrap();
        let second = ensure_library(&conn, root).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn create_library_row_sets_conversion_enabled_at_creation() {
        let conn = test_conn();
        let root = Path::new("A:/fake/library-off");

        let library_id = create_library_row(&conn, root, false).unwrap();

        assert_eq!(conversion_enabled(&conn, library_id).unwrap(), false);
    }

    #[test]
    fn create_library_row_defaults_to_conversion_on_when_requested() {
        let conn = test_conn();
        let root = Path::new("A:/fake/library-on");

        let library_id = create_library_row(&conn, root, true).unwrap();

        assert_eq!(conversion_enabled(&conn, library_id).unwrap(), true);
    }

    #[test]
    fn importing_two_files_with_identical_content_collapses_to_one_image() {
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();

        // Same pixel content, different filenames — a real duplicate.
        let a = write_png(source_dir.path(), "a.png", 42);
        let b = write_png(source_dir.path(), "b.png", 42);

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let outcome_a = import_file(&ctx, &a).unwrap();
        let outcome_b = import_file(&ctx, &b).unwrap();

        let FileOutcome::Imported { image_id: id_a } = outcome_a else {
            panic!("expected the first import to create a new image, got {outcome_a:?}");
        };
        let FileOutcome::Collapsed { image_id: id_b } = outcome_b else {
            panic!("expected the second (identical) import to collapse, got {outcome_b:?}");
        };
        assert_eq!(id_a, id_b, "duplicate content must resolve to the same images row");

        let image_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0)).unwrap();
        assert_eq!(image_count, 1, "exactly one images row for two identical files");

        let source_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM image_sources", [], |row| row.get(0)).unwrap();
        assert_eq!(source_count, 2, "each import event still gets its own image_sources row");

        // Both originals were quarantined (moved out of the source dir).
        assert!(!a.exists());
        assert!(!b.exists());
    }

    #[test]
    fn a_file_that_isnt_a_standard_format_image_is_left_in_place() {
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();

        let bogus = source_dir.path().join("not-an-image.txt");
        fs::write(&bogus, b"nope").unwrap();

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let outcome = import_file(&ctx, &bogus).unwrap();

        assert_eq!(outcome, FileOutcome::Failed);
        assert!(bogus.exists(), "an undecodable file must never be moved/quarantined");

        let image_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0)).unwrap();
        assert_eq!(image_count, 0);
    }

    #[test]
    fn resuming_an_already_done_file_is_a_safe_no_op() {
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
        let path = write_png(source_dir.path(), "once.png", 7);

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let first = import_file(&ctx, &path).unwrap();
        // Second call simulates a resumed run re-scanning the same source
        // file after the batch was already fully processed for it.
        let second = import_file(&ctx, &path).unwrap();

        let FileOutcome::Imported { image_id } = first else { panic!("expected Imported, got {first:?}") };
        assert_eq!(second, FileOutcome::AlreadyDone { image_id: Some(image_id) });

        let image_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM images", [], |row| row.get(0)).unwrap();
        assert_eq!(image_count, 1, "resuming must never create a duplicate row");
    }

    #[test]
    fn interrupting_mid_pipeline_and_retrying_finishes_cleanly() {
        // Simulates a crash after the blob is written but before the
        // journal is marked done: re-running import_file on the same
        // source file must finish correctly without duplicating anything.
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
        let path = write_png(source_dir.path(), "interrupted.png", 99);

        // Hand-roll the "crashed after hashing, before quarantine" state:
        // insert a journal row stuck at 'dedupe_checked', matching what a
        // real interrupted run would leave behind.
        conn.execute(
            "INSERT INTO import_journal (batch_id, source_path, step) VALUES (?1, ?2, 'dedupe_checked')",
            params![batch_id, path.to_string_lossy()],
        )
        .unwrap();

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let outcome = import_file(&ctx, &path).unwrap();

        assert!(matches!(outcome, FileOutcome::Imported { .. }));
        assert!(!path.exists(), "resumed run must still finish quarantining the original");

        let step: String = conn
            .query_row("SELECT step FROM import_journal WHERE batch_id = ?1", [batch_id], |row| row.get(0))
            .unwrap();
        assert_eq!(step, "done");
    }

    #[test]
    fn retention_sweep_purges_only_expired_entries() {
        let conn = test_conn();
        let library_dir = tempfile::tempdir().unwrap();
        let library_id = ensure_library(&conn, library_dir.path()).unwrap();

        let expired_path = library_dir.path().join("expired.bin");
        fs::write(&expired_path, b"old").unwrap();
        conn.execute(
            "INSERT INTO quarantine (library_id, quarantine_type, quarantined_path, expires_at)
             VALUES (?1, 'import_original', ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-1 days'))",
            params![library_id, expired_path.to_string_lossy()],
        )
        .unwrap();

        let fresh_path = library_dir.path().join("fresh.bin");
        fs::write(&fresh_path, b"new").unwrap();
        conn.execute(
            "INSERT INTO quarantine (library_id, quarantine_type, quarantined_path, expires_at)
             VALUES (?1, 'import_original', ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+29 days'))",
            params![library_id, fresh_path.to_string_lossy()],
        )
        .unwrap();

        let purged = sweep_expired_quarantine(&conn).unwrap();

        assert_eq!(purged, 1);
        assert!(!expired_path.exists());
        assert!(fresh_path.exists());
    }

    #[test]
    fn a_jpeg_is_converted_to_jxl_through_the_real_pipeline() {
        // Proves §4's conversion policy is actually wired into import_file,
        // not just callable directly from lumenvault-convert: a JPEG source
        // file, run through the real import pipeline, ends up stored as a
        // .jxl blob with conversion_status = 'converted'.
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
        let path = write_jpeg(source_dir.path(), "photo.jpg", 11);

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let outcome = import_file(&ctx, &path).unwrap();

        let FileOutcome::Imported { image_id } = outcome else { panic!("expected Imported, got {outcome:?}") };
        let (original_format, stored_format, conversion_status, stored_path): (String, String, String, String) = conn
            .query_row(
                "SELECT original_format, stored_format, conversion_status, stored_path FROM images WHERE id = ?1",
                [image_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(original_format, "jpeg");
        assert_eq!(stored_format, "jxl");
        assert_eq!(conversion_status, "converted");
        assert!(stored_path.ends_with(".jxl"), "blob path should carry the converted extension: {stored_path}");
        assert!(std::path::Path::new(&stored_path).exists());
    }

    #[test]
    fn conversion_disabled_on_the_library_leaves_the_format_untouched() {
        // §4: "opt-out granularity: per-library, fixed at library creation."
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        conn.execute("UPDATE libraries SET conversion_enabled = 0 WHERE id = ?1", [library_id]).unwrap();
        assert!(!conversion_enabled(&conn, library_id).unwrap());

        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
        let path = write_jpeg(source_dir.path(), "photo.jpg", 22);

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: false };
        let outcome = import_file(&ctx, &path).unwrap();

        let FileOutcome::Imported { image_id } = outcome else { panic!("expected Imported, got {outcome:?}") };
        let (stored_format, conversion_status): (String, String) = conn
            .query_row(
                "SELECT stored_format, conversion_status FROM images WHERE id = ?1",
                [image_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(stored_format, "jpeg", "conversion disabled must leave the original format in place");
        assert_eq!(conversion_status, "not_attempted");
    }

    #[test]
    fn a_format_with_no_conversion_path_is_not_applicable() {
        // WebP has no §4 conversion path at all — distinct from
        // 'not_attempted' (a setting) or 'failed_kept_original' (a verification
        // failure): there was never a policy to attempt in the first place.
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
        let path = source_dir.path().join("photo.webp");
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(8, 8, |x, y| Rgb([(x * y) as u8, 1, 2]));
        img.save(&path).unwrap();

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let outcome = import_file(&ctx, &path).unwrap();

        let FileOutcome::Imported { image_id } = outcome else { panic!("expected Imported, got {outcome:?}") };
        let conversion_status: String =
            conn.query_row("SELECT conversion_status FROM images WHERE id = ?1", [image_id], |row| row.get(0)).unwrap();
        assert_eq!(conversion_status, "not_applicable");
    }

    // ── Milestone 5: thumbnail generation, review-queue resolution, export ──

    #[test]
    fn importing_a_standard_image_generates_and_records_a_grid_thumbnail() {
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
        let path = write_png(source_dir.path(), "photo.png", 5);

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let outcome = import_file(&ctx, &path).unwrap();
        let FileOutcome::Imported { image_id } = outcome else { panic!("expected Imported, got {outcome:?}") };

        let thumb_path: String = conn
            .query_row(
                "SELECT path FROM thumbnails WHERE image_id = ?1 AND variant = 'grid256'",
                [image_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(std::path::Path::new(&thumb_path).exists(), "thumbnail file must actually exist on disk");
        // A real, decodable JPEG — not just a placeholder file.
        let decoded = image::open(&thumb_path).unwrap();
        assert!(decoded.width() > 0 && decoded.height() > 0);
    }

    /// Two distinct-bytes, perceptually-identical images (same
    /// x/y-derived gradient, only the seed byte in an unused corner of the
    /// color space differs) — enough to avoid exact-hash collapse while
    /// still landing well within the default 5-bit Hamming threshold, so a
    /// real `dedupe_review_queue` row exists to resolve.
    fn library_with_two_review_candidates(conn: &Connection, library_dir: &std::path::Path) -> (i64, LibraryPaths, i64, i64, i64) {
        let paths = LibraryPaths::new(library_dir);
        let source_dir = tempfile::tempdir().unwrap();
        let library_id = ensure_library(conn, library_dir).unwrap();
        let batch_id = start_or_resume_batch(conn, library_id, source_dir.path()).unwrap();
        let path_a = write_png(source_dir.path(), "a.png", 1);
        let path_b = write_png(source_dir.path(), "b.png", 2);

        let ctx = ImportContext { conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let FileOutcome::Imported { image_id: image_a } = import_file(&ctx, &path_a).unwrap() else {
            panic!("expected a fresh import for a.png")
        };
        let outcome_b = import_file(&ctx, &path_b).unwrap();
        let image_b = match outcome_b {
            FileOutcome::Imported { image_id } => image_id,
            FileOutcome::Collapsed { image_id } => image_id,
            other => panic!("unexpected outcome for b.png: {other:?}"),
        };

        let queue_id: i64 = conn
            .query_row(
                "SELECT id FROM dedupe_review_queue WHERE (image_a_id = ?1 AND image_b_id = ?2) OR (image_a_id = ?2 AND image_b_id = ?1)",
                params![image_a, image_b],
                |row| row.get(0),
            )
            .unwrap();

        (library_id, paths, image_a, image_b, queue_id)
    }

    #[test]
    fn merging_a_review_pair_unions_tags_quarantines_the_loser_and_marks_state() {
        let conn = test_conn();
        let library_dir = tempfile::tempdir().unwrap();
        let (_library_id, paths, image_a, image_b, queue_id) = library_with_two_review_candidates(&conn, library_dir.path());

        lumenvault_catalog::add_tag(&conn, image_a, "keeper-tag").unwrap();
        lumenvault_catalog::add_tag(&conn, image_b, "loser-tag").unwrap();

        let loser_stored_path: String =
            conn.query_row("SELECT stored_path FROM images WHERE id = ?1", [image_b], |row| row.get(0)).unwrap();
        assert!(std::path::Path::new(&loser_stored_path).exists(), "precondition: loser blob exists before merge");

        resolve_review_pair(&conn, &paths, queue_id, ReviewAction::Merge { keeper_id: image_a }).unwrap();

        // Tags unioned onto the keeper.
        let keeper_tags = lumenvault_catalog::tags_for_image(&conn, image_a).unwrap();
        assert!(keeper_tags.contains(&"keeper-tag".to_string()));
        assert!(keeper_tags.contains(&"loser-tag".to_string()));

        // Loser marked merged.
        let (status, merged_into): (String, Option<i64>) = conn
            .query_row("SELECT status, merged_into_id FROM images WHERE id = ?1", [image_b], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(status, "merged");
        assert_eq!(merged_into, Some(image_a));

        // Loser's blob quarantined (moved, not deleted), quarantine_type = merge_discard.
        assert!(!std::path::Path::new(&loser_stored_path).exists(), "loser blob must be moved out of the store");
        let (quarantine_type, quarantined_path): (String, String) = conn
            .query_row(
                "SELECT quarantine_type, quarantined_path FROM quarantine WHERE related_image_id = ?1 AND quarantine_type = 'merge_discard'",
                [image_b],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(quarantine_type, "merge_discard");
        assert!(std::path::Path::new(&quarantined_path).exists(), "the quarantined copy must actually exist");

        // Queue row resolved.
        let queue_status: String =
            conn.query_row("SELECT status FROM dedupe_review_queue WHERE id = ?1", [queue_id], |row| row.get(0)).unwrap();
        assert_eq!(queue_status, "merged");
    }

    #[test]
    fn dismissing_a_review_pair_only_changes_the_queue_row() {
        let conn = test_conn();
        let library_dir = tempfile::tempdir().unwrap();
        let (_library_id, paths, image_a, image_b, queue_id) = library_with_two_review_candidates(&conn, library_dir.path());

        let (status_a_before, status_b_before): (String, String) = (
            conn.query_row("SELECT status FROM images WHERE id = ?1", [image_a], |row| row.get(0)).unwrap(),
            conn.query_row("SELECT status FROM images WHERE id = ?1", [image_b], |row| row.get(0)).unwrap(),
        );

        resolve_review_pair(&conn, &paths, queue_id, ReviewAction::Dismiss).unwrap();

        let queue_status: String =
            conn.query_row("SELECT status FROM dedupe_review_queue WHERE id = ?1", [queue_id], |row| row.get(0)).unwrap();
        assert_eq!(queue_status, "dismissed");

        let (status_a_after, status_b_after): (String, String) = (
            conn.query_row("SELECT status FROM images WHERE id = ?1", [image_a], |row| row.get(0)).unwrap(),
            conn.query_row("SELECT status FROM images WHERE id = ?1", [image_b], |row| row.get(0)).unwrap(),
        );
        assert_eq!(status_a_before, status_a_after);
        assert_eq!(status_b_before, status_b_after);

        let quarantine_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM quarantine WHERE quarantine_type = 'merge_discard'", [], |row| row.get(0)).unwrap();
        assert_eq!(quarantine_count, 0);
    }

    #[test]
    fn resolving_an_already_resolved_entry_twice_is_a_safe_no_op() {
        let conn = test_conn();
        let library_dir = tempfile::tempdir().unwrap();
        let (_library_id, paths, image_a, _image_b, queue_id) = library_with_two_review_candidates(&conn, library_dir.path());

        resolve_review_pair(&conn, &paths, queue_id, ReviewAction::Merge { keeper_id: image_a }).unwrap();
        // Second call must not error or double-quarantine.
        resolve_review_pair(&conn, &paths, queue_id, ReviewAction::Merge { keeper_id: image_a }).unwrap();

        let quarantine_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM quarantine WHERE quarantine_type = 'merge_discard'", [], |row| row.get(0)).unwrap();
        assert_eq!(quarantine_count, 1);
    }

    #[test]
    fn exporting_an_image_copies_the_stored_file_and_its_sidecar() {
        let conn = test_conn();
        let source_dir = tempfile::tempdir().unwrap();
        let library_dir = tempfile::tempdir().unwrap();
        let export_dir = tempfile::tempdir().unwrap();
        let paths = LibraryPaths::new(library_dir.path());

        let library_id = ensure_library(&conn, library_dir.path()).unwrap();
        let batch_id = start_or_resume_batch(&conn, library_id, source_dir.path()).unwrap();
        let path = write_png(source_dir.path(), "photo.png", 9);

        let ctx = ImportContext { conn: &conn, paths: &paths, library_id, batch_id, conversion_enabled: true };
        let FileOutcome::Imported { image_id } = import_file(&ctx, &path).unwrap() else { panic!("expected Imported") };

        lumenvault_catalog::add_tag(&conn, image_id, "vacation").unwrap();
        lumenvault_xmp::sync_sidecar(&conn, image_id).unwrap();

        let dest = export_image(&conn, image_id, export_dir.path()).unwrap();

        assert!(dest.exists(), "the exported image file must actually exist");
        let dest_ext = dest.extension().unwrap().to_string_lossy().to_string();
        let stored_format: String =
            conn.query_row("SELECT stored_format FROM images WHERE id = ?1", [image_id], |row| row.get(0)).unwrap();
        assert_eq!(dest_ext, stored_format, "export must keep the currently-stored format, no un-conversion");

        let exported_sidecar = dest.with_extension("xmp");
        assert!(exported_sidecar.exists(), "the sidecar must be exported alongside the image");
        let sidecar_tags = lumenvault_xmp::read_sidecar(&exported_sidecar).unwrap();
        assert_eq!(sidecar_tags, vec!["vacation".to_string()]);
    }
}
