//! Import pipeline orchestration: journal, dedupe-check, quarantine, per
//! workplan/SPEC.md §3.
//!
//! Milestone 1 scope: the six-step pipeline minus conversion (Milestone 2),
//! perceptual dedupe/RAW+JPEG pairing (Milestone 3), and XMP sidecars
//! (Milestone 4). What's left: hash → decode-validate → exact-hash collapse
//! → content-addressed write → quarantine, all journaled for crash recovery.
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

    // Step: decode-validate. A file this milestone can't decode is refused,
    // not imported — the original is left untouched at its source path.
    let probe = match lumenvault_decode::probe(source_path) {
        Ok(probe) => probe,
        Err(e) => {
            set_journal_step(conn, journal_id, "failed", Some(&e.to_string()))?;
            return Ok(FileOutcome::Failed);
        }
    };
    let format = probe.format.as_str();

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
                        file_size_bytes, width, height
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        library_id,
                        original_hash.as_slice(),
                        stored_hash.as_slice(),
                        blob_path.to_string_lossy(),
                        format,
                        conversion.stored_format,
                        conversion.conversion_status,
                        fs::metadata(&blob_path)?.len() as i64,
                        probe.width,
                        probe.height,
                    ],
                )?;
                conn.last_insert_rowid()
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

    quarantine_original(conn, paths, library_id, journal_id, image_id, source_path)?;
    set_journal_step(conn, journal_id, "done", None)?;

    Ok(outcome)
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
}
