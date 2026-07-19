//! Rebuild-from-sidecars recovery path, per workplan/SPEC.md §7: "if the
//! catalog is lost, the app rescans the store, re-hashes everything (BLAKE3
//! + perceptual), and re-imports tags/metadata from sidecars."
//!
//! **Why this lives in `lumenvault-import`, not `lumenvault-xmp`**: rebuild
//! needs `lumenvault-decode` (re-probe every blob), `lumenvault-hash`
//! (re-hash), `lumenvault-catalog` (recreate rows, re-apply tags), and
//! `lumenvault-xmp` (read sidecars) all at once — exactly the set of
//! dependencies `lumenvault-import` already has (plus `lumenvault-xmp`,
//! added this milestone) as the crate that "owns pipeline orchestration"
//! per its own module doc. Putting it in `lumenvault-xmp` instead would
//! make the sidecar crate depend on `decode`/`convert`-adjacent concerns
//! it has no other reason to know about.
//!
//! **Two things this rebuild cannot recover, by design** (§7 explicitly
//! names dedupe-merge history and ML embeddings as unrecoverable; the
//! following two are this module's own necessary extensions of that same
//! principle, flagged here rather than silently assumed):
//!
//! 1. **`original_hash`** is *not* re-derived by hashing on-disk bytes —
//!    for a converted file, the original pre-conversion bytes are gone
//!    forever, only the converted bytes remain. It's recovered instead by
//!    parsing the hash back out of the blob's own path
//!    (`<root>/blobs/<first-2-hex>/<full-hash-hex>.<ext>`), which is
//!    exactly what that hash was used to address in the first place.
//!    `stored_hash` *is* re-derived, by re-hashing the actual bytes on disk
//!    right now.
//! 2. **`image_sources` provenance** (original filename, source import
//!    path) is not recoverable from an XMP sidecar (which carries tag
//!    data, not import history) any more than dedupe-merge history is. One
//!    synthetic `import_batches` row is created to represent "recovered via
//!    sidecar rescan," and each recovered image gets one `image_sources`
//!    row using the blob's own on-disk filename as a placeholder
//!    `original_filename`/`source_path` — a documented stand-in, not a
//!    claim of recovering the real value.
//!
//! **A third gap, not named in §7 either, resolved the same way**: for a
//! blob whose `stored_format` is `jxl`, the true pre-conversion
//! `original_format` (JPEG, PNG, or BMP — §4's three JXL-producing source
//! formats) isn't recoverable from the store either — nothing on disk
//! records which one it was. Rebuild sets `original_format` equal to
//! `stored_format` in that case (both `"jxl"`), the same honest
//! "unrecoverable, don't fabricate a specific guess" treatment as the two
//! points above. `conversion_status` is set to `'converted'` when
//! `stored_format == "jxl"` (nothing else produces that extension) and
//! `'not_applicable'` otherwise — a heuristic, not a recovery of the
//! original per-file decision (which could have been `'not_attempted'` or
//! `'failed_kept_original'` too, states this rebuild has no way to
//! distinguish from `'not_applicable'` after the fact).

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{ImportError, LibraryPaths};

/// Outcome of one [`rebuild_from_store`] run.
#[derive(Debug, Default)]
pub struct RebuildReport {
    /// One `images` row was recreated for each of these blobs.
    pub images_recovered: usize,
    /// Total tag applications re-imported from `.xmp` sidecars across all
    /// recovered images.
    pub tags_recovered: usize,
    /// Blob files under `<root>/blobs` that could not be classified at all
    /// (not decodable by `lumenvault-decode`, not a recognized RAW
    /// extension, or not a validly-named content-addressed blob) —
    /// `(path, reason)`. Rebuild does not abort on these; it records and
    /// continues, since one corrupt/foreign file in the store shouldn't
    /// block recovering everything else.
    pub skipped: Vec<(PathBuf, String)>,
}

/// Rescans every blob under `paths.root()`'s `blobs/` directory and
/// recreates the catalog from scratch: `images` rows (content identity,
/// re-hashed `stored_hash`, path-recovered `original_hash`, re-decoded
/// `width`/`height`/`perceptual_hash`), one synthetic `import_batches` +
/// `image_sources` row per image (see module doc), and tags re-imported
/// from each blob's `.xmp` sidecar, if one exists next to it.
///
/// `conn` must already be freshly migrated (`lumenvault_catalog::migrate`)
/// — this function only populates rows, it doesn't apply the schema.
pub fn rebuild_from_store(
    conn: &Connection,
    paths: &LibraryPaths,
) -> Result<RebuildReport, ImportError> {
    let library_id = crate::ensure_library(conn, paths.root())?;
    let batch_id = create_recovery_batch(conn, library_id)?;

    let mut report = RebuildReport::default();
    let blobs_dir = paths.root().join("blobs");
    if !blobs_dir.exists() {
        return Ok(report); // nothing to recover from an empty/nonexistent store
    }

    for entry in walkdir::WalkDir::new(&blobs_dir) {
        let entry = entry.map_err(|source| ImportError::Walk {
            root: blobs_dir.clone(),
            source,
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let blob_path = entry.path();
        // Sidecars live next to their blob in the same directory (same
        // basename, `.xmp` extension) — not blobs themselves, so the walk
        // must skip them rather than trying to recover an "image" from
        // one.
        if blob_path.extension().and_then(|e| e.to_str()) == Some("xmp") {
            continue;
        }

        match recover_one_blob(conn, library_id, batch_id, blob_path) {
            Ok(RecoverOutcome::Recovered { tags_applied }) => {
                report.images_recovered += 1;
                report.tags_recovered += tags_applied;
            }
            Ok(RecoverOutcome::Skipped { reason }) => {
                report.skipped.push((blob_path.to_path_buf(), reason));
            }
            Err(e) => return Err(e),
        }
    }

    conn.execute(
        "UPDATE import_batches SET status = 'completed', completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1",
        [batch_id],
    )?;

    Ok(report)
}

enum RecoverOutcome {
    Recovered { tags_applied: usize },
    Skipped { reason: String },
}

fn create_recovery_batch(conn: &Connection, library_id: i64) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO import_batches (library_id, source_root, status)
         VALUES (?1, 'recovered via sidecar rescan (rebuild_from_store)', 'running')",
        [library_id],
    )?;
    Ok(conn.last_insert_rowid())
}

fn recover_one_blob(
    conn: &Connection,
    library_id: i64,
    batch_id: i64,
    blob_path: &Path,
) -> Result<RecoverOutcome, ImportError> {
    let Some(original_hash) = hash_from_blob_path(blob_path) else {
        return Ok(RecoverOutcome::Skipped {
            reason: "filename is not a valid content-addressed blob name (64 hex chars)"
                .to_string(),
        });
    };

    // Already recovered by an earlier run against the same fresh catalog —
    // shouldn't happen in a single rebuild_from_store call, but keeps this
    // function itself idempotent/safe to retry.
    let already_recovered: bool = conn
        .query_row(
            "SELECT 1 FROM images WHERE original_hash = ?1",
            [original_hash.as_slice()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if already_recovered {
        return Ok(RecoverOutcome::Skipped {
            reason: "already recovered".to_string(),
        });
    }

    let Some(stored_format) = blob_path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return Ok(RecoverOutcome::Skipped {
            reason: "blob has no file extension".to_string(),
        });
    };

    let stored_hash = lumenvault_hash::hash_file(blob_path)?;

    // Re-decode (or RAW-recognize) exactly like import time — see
    // lumenvault-decode's module doc for why RAW gets no pixel data.
    let (width, height, perceptual_hash) = match lumenvault_decode::probe(blob_path) {
        Ok(probe) => {
            let hash = lumenvault_hash::perceptual_hash(&probe.image);
            (Some(probe.width), Some(probe.height), Some(hash as i64))
        }
        Err(e) => {
            if lumenvault_decode::raw_extension(blob_path).is_some() {
                (None, None, None)
            } else {
                return Ok(RecoverOutcome::Skipped {
                    reason: format!("undecodable and not a recognized RAW extension: {e}"),
                });
            }
        }
    };

    // original_format: recoverable exactly when the file was never
    // converted (stored_format IS the original) — see module doc for the
    // jxl case, where it genuinely isn't.
    let original_format = stored_format.clone();
    let conversion_status = if stored_format == "jxl" {
        "converted"
    } else {
        "not_applicable"
    };

    let file_size_bytes = fs::metadata(blob_path)?.len() as i64;

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
            original_format,
            stored_format,
            conversion_status,
            file_size_bytes,
            width,
            height,
            perceptual_hash,
        ],
    )?;
    let image_id = conn.last_insert_rowid();

    // Placeholder provenance (module doc point 2): the blob's own on-disk
    // filename stands in for the real, unrecoverable original filename.
    let placeholder_name = blob_path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    conn.execute(
        "INSERT INTO image_sources (image_id, import_batch_id, original_filename, source_path)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            image_id,
            batch_id,
            placeholder_name,
            blob_path.to_string_lossy()
        ],
    )?;
    // Keeps search (Milestone 5) working after a rebuild even for images
    // with no tags at all — add_tag below already re-syncs when tags exist,
    // but an untagged image otherwise never gets its filename indexed.
    lumenvault_catalog::sync_fts_row(conn, image_id)?;

    // Re-import tags from the sidecar, if one exists next to this blob.
    let sidecar_path = blob_path.with_extension("xmp");
    let mut tags_applied = 0;
    if sidecar_path.exists() {
        let tags = lumenvault_xmp::read_sidecar(&sidecar_path)?;
        for tag in &tags {
            lumenvault_catalog::add_tag(conn, image_id, tag)?;
            tags_applied += 1;
        }
        // Stamp sidecar_path/sidecar_synced_at to reflect that this image's
        // sidecar is in sync as of the rebuild — sync_sidecar re-derives
        // the same path and re-writes the same tags, so this is a no-op on
        // content, just catalog bookkeeping.
        lumenvault_xmp::sync_sidecar(conn, image_id)?;
    }

    Ok(RecoverOutcome::Recovered { tags_applied })
}

/// Parses the BLAKE3 digest back out of a content-addressed blob path
/// (`.../<first-2-hex>/<full-hash-hex>.<ext>`) — see the module doc's point
/// 1 for why this is how `original_hash` is recovered, not by re-hashing.
fn hash_from_blob_path(blob_path: &Path) -> Option<[u8; 32]> {
    let stem = blob_path.file_stem()?.to_str()?;
    if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&stem[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}
