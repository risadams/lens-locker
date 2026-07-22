//! Per-file journal for migrating an existing unencrypted vault into a
//! Locking-enabled one — `workplan/LOCK-SPEC.md` §8, ticket 050. Mirrors
//! the import pipeline's crash-safe-by-idempotency journal shape and
//! discipline exactly (`migrations/0007_migration_journal.sql`'s header
//! comment has the full reasoning).
//!
//! This module owns planning the journal and rewriting path columns —
//! pure catalog/SQL concerns. The actual file copy/verify/delete and the
//! encrypted-VHDX provisioning are `src-tauri`'s job (parallel to how
//! `crates/import` orchestrates hashing/decoding but leaves format-
//! specific work to other crates).

use rusqlite::{Connection, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalKind {
    Blob,
    Sidecar,
    Quarantine,
    Thumbnail,
}

impl JournalKind {
    fn as_str(self) -> &'static str {
        match self {
            JournalKind::Blob => "blob",
            JournalKind::Sidecar => "sidecar",
            JournalKind::Quarantine => "quarantine",
            JournalKind::Thumbnail => "thumbnail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalStep {
    Pending,
    Copied,
    Verified,
    OriginalRemoved,
}

impl JournalStep {
    fn as_str(self) -> &'static str {
        match self {
            JournalStep::Pending => "pending",
            JournalStep::Copied => "copied",
            JournalStep::Verified => "verified",
            JournalStep::OriginalRemoved => "original_removed",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "copied" => JournalStep::Copied,
            "verified" => JournalStep::Verified,
            "original_removed" => JournalStep::OriginalRemoved,
            _ => JournalStep::Pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MigrationJournalRow {
    pub id: i64,
    pub kind: JournalKind,
    pub old_path: String,
    pub new_path: String,
    pub step: JournalStep,
}

fn rewrite_prefix(path: &str, old_root: &str, new_root: &str) -> Option<String> {
    path.strip_prefix(old_root).map(|rest| format!("{new_root}{rest}"))
}

/// Plans the migration by inserting `pending` journal rows for every
/// vault-internal file the catalog currently references — idempotent: a
/// row already planned for a given `(kind, old_path)` is never duplicated,
/// so re-running this after a crash mid-plan is safe. Does **not** plan
/// the catalog file itself, or `image_sources.source_path`/
/// `import_batches.source_root` (both pre-import, external filesystem
/// locations that have nothing to do with the vault's internal layout —
/// migrating them would be a real correctness bug, not a missed detail).
pub fn plan_migration(
    conn: &Connection,
    library_id: i64,
    old_root: &str,
    new_root: &str,
) -> rusqlite::Result<usize> {
    let mut planned = 0usize;

    let mut stmt = conn.prepare(
        "SELECT stored_path, sidecar_path FROM images
         WHERE library_id = ?1 AND (stored_path LIKE ?2 || '%' OR sidecar_path LIKE ?2 || '%')",
    )?;
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map(params![library_id, old_root], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (stored_path, sidecar_path) in rows {
        if let Some(new_path) = rewrite_prefix(&stored_path, old_root, new_root) {
            planned += insert_planned(conn, library_id, JournalKind::Blob, &stored_path, &new_path)?;
        }
        if let Some(sidecar_path) = sidecar_path {
            if let Some(new_path) = rewrite_prefix(&sidecar_path, old_root, new_root) {
                planned += insert_planned(conn, library_id, JournalKind::Sidecar, &sidecar_path, &new_path)?;
            }
        }
    }

    let mut stmt = conn.prepare(
        "SELECT quarantined_path FROM quarantine
         WHERE library_id = ?1 AND quarantined_path LIKE ?2 || '%' AND purged_at IS NULL",
    )?;
    let rows: Vec<String> = stmt
        .query_map(params![library_id, old_root], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for quarantined_path in rows {
        if let Some(new_path) = rewrite_prefix(&quarantined_path, old_root, new_root) {
            planned += insert_planned(conn, library_id, JournalKind::Quarantine, &quarantined_path, &new_path)?;
        }
    }

    let mut stmt = conn.prepare(
        "SELECT t.path FROM thumbnails t
         JOIN images i ON i.id = t.image_id
         WHERE i.library_id = ?1 AND t.path LIKE ?2 || '%'",
    )?;
    let rows: Vec<String> = stmt
        .query_map(params![library_id, old_root], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for path in rows {
        if let Some(new_path) = rewrite_prefix(&path, old_root, new_root) {
            planned += insert_planned(conn, library_id, JournalKind::Thumbnail, &path, &new_path)?;
        }
    }

    Ok(planned)
}

fn insert_planned(
    conn: &Connection,
    library_id: i64,
    kind: JournalKind,
    old_path: &str,
    new_path: &str,
) -> rusqlite::Result<usize> {
    let already_planned: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM migration_journal WHERE library_id = ?1 AND kind = ?2 AND old_path = ?3)",
        params![library_id, kind.as_str(), old_path],
        |row| row.get(0),
    )?;
    if already_planned {
        return Ok(0);
    }
    conn.execute(
        "INSERT INTO migration_journal (library_id, kind, old_path, new_path) VALUES (?1, ?2, ?3, ?4)",
        params![library_id, kind.as_str(), old_path, new_path],
    )?;
    Ok(1)
}

/// Every journal row not yet fully migrated (`original_removed`) — what a
/// resumed migration run still needs to process. Ordered by `id` so
/// progress is deterministic across resumes (not load-bearing for
/// correctness, just makes a partial run's behavior predictable).
pub fn pending_journal_rows(conn: &Connection, library_id: i64) -> rusqlite::Result<Vec<MigrationJournalRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, old_path, new_path, step FROM migration_journal
         WHERE library_id = ?1 AND step != 'original_removed'
         ORDER BY id",
    )?;
    stmt.query_map(params![library_id], |row| {
        let kind: String = row.get(1)?;
        let step: String = row.get(4)?;
        Ok(MigrationJournalRow {
            id: row.get(0)?,
            kind: match kind.as_str() {
                "sidecar" => JournalKind::Sidecar,
                "quarantine" => JournalKind::Quarantine,
                "thumbnail" => JournalKind::Thumbnail,
                _ => JournalKind::Blob,
            },
            old_path: row.get(2)?,
            new_path: row.get(3)?,
            step: JournalStep::from_str(&step),
        })
    })?
    .collect()
}

pub fn set_journal_step(conn: &Connection, row_id: i64, step: JournalStep) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE migration_journal SET step = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
        params![step.as_str(), row_id],
    )?;
    Ok(())
}

/// `true` once every planned row reports `original_removed` — the signal
/// that it's safe to rewrite path columns and migrate the catalog file
/// itself. `false` (not an error) if nothing has been planned yet.
pub fn migration_fully_complete(conn: &Connection, library_id: i64) -> rusqlite::Result<bool> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM migration_journal WHERE library_id = ?1",
        params![library_id],
        |row| row.get(0),
    )?;
    if total == 0 {
        return Ok(false);
    }
    let remaining: i64 = conn.query_row(
        "SELECT COUNT(*) FROM migration_journal WHERE library_id = ?1 AND step != 'original_removed'",
        params![library_id],
        |row| row.get(0),
    )?;
    Ok(remaining == 0)
}

/// Rewrites every path column that pointed under `old_root` to point
/// under `new_root` instead — `images.stored_path`/`sidecar_path`,
/// `quarantine.quarantined_path`, `thumbnails.path`, `libraries.root_path`.
/// Called once, after [`migration_fully_complete`] confirms every file is
/// physically in place. Deliberately does **not** touch
/// `image_sources.source_path`/`import_batches.source_root` (pre-import,
/// external locations — see [`plan_migration`]'s doc) or
/// `quarantine.original_path` (a historical "this used to live here"
/// record, not a path anything actually reads to locate a live file).
pub fn rewrite_paths(conn: &Connection, library_id: i64, old_root: &str, new_root: &str) -> rusqlite::Result<()> {
    let old_prefix_len = old_root.len() as i64 + 1;

    conn.execute(
        "UPDATE images SET stored_path = ?1 || substr(stored_path, ?2)
         WHERE library_id = ?3 AND stored_path LIKE ?4 || '%'",
        params![new_root, old_prefix_len, library_id, old_root],
    )?;
    conn.execute(
        "UPDATE images SET sidecar_path = ?1 || substr(sidecar_path, ?2)
         WHERE library_id = ?3 AND sidecar_path LIKE ?4 || '%'",
        params![new_root, old_prefix_len, library_id, old_root],
    )?;
    conn.execute(
        "UPDATE quarantine SET quarantined_path = ?1 || substr(quarantined_path, ?2)
         WHERE library_id = ?3 AND quarantined_path LIKE ?4 || '%'",
        params![new_root, old_prefix_len, library_id, old_root],
    )?;
    conn.execute(
        "UPDATE thumbnails SET path = ?1 || substr(path, ?2)
         WHERE path LIKE ?3 || '%' AND image_id IN (SELECT id FROM images WHERE library_id = ?4)",
        params![new_root, old_prefix_len, old_root, library_id],
    )?;
    conn.execute(
        "UPDATE libraries SET root_path = ?1 WHERE id = ?2",
        params![new_root, library_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrate;

    fn setup_library(conn: &mut Connection, root: &str) -> i64 {
        migrate(conn).unwrap();
        conn.execute(
            "INSERT INTO libraries (name, root_path) VALUES ('test', ?1)",
            params![root],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn plans_blob_and_sidecar_paths_and_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        let library_id = setup_library(&mut conn, "/vault");

        conn.execute(
            "INSERT INTO images (library_id, original_hash, stored_hash, stored_path, sidecar_path, original_format, stored_format, file_size_bytes)
             VALUES (?1, X'01', X'01', '/vault/blobs/ab/abcdef.jxl', '/vault/blobs/ab/abcdef.xmp', 'jpeg', 'jxl', 100)",
            params![library_id],
        ).unwrap();

        let planned = plan_migration(&conn, library_id, "/vault", "/vault/live").unwrap();
        assert_eq!(planned, 2, "one blob row + one sidecar row");

        let rows = pending_journal_rows(&conn, library_id).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.kind == JournalKind::Blob && r.new_path == "/vault/live/blobs/ab/abcdef.jxl"));
        assert!(rows.iter().any(|r| r.kind == JournalKind::Sidecar && r.new_path == "/vault/live/blobs/ab/abcdef.xmp"));

        // Re-planning must not duplicate rows — the resumability guarantee.
        let planned_again = plan_migration(&conn, library_id, "/vault", "/vault/live").unwrap();
        assert_eq!(planned_again, 0);
        assert_eq!(pending_journal_rows(&conn, library_id).unwrap().len(), 2);
    }

    #[test]
    fn migration_fully_complete_is_false_until_every_row_is_original_removed() {
        let mut conn = Connection::open_in_memory().unwrap();
        let library_id = setup_library(&mut conn, "/vault");
        conn.execute(
            "INSERT INTO images (library_id, original_hash, stored_hash, stored_path, original_format, stored_format, file_size_bytes)
             VALUES (?1, X'01', X'01', '/vault/blobs/ab/abcdef.jxl', 'jpeg', 'jxl', 100)",
            params![library_id],
        ).unwrap();
        plan_migration(&conn, library_id, "/vault", "/vault/live").unwrap();

        assert!(!migration_fully_complete(&conn, library_id).unwrap());

        let rows = pending_journal_rows(&conn, library_id).unwrap();
        set_journal_step(&conn, rows[0].id, JournalStep::Verified).unwrap();
        assert!(!migration_fully_complete(&conn, library_id).unwrap());

        set_journal_step(&conn, rows[0].id, JournalStep::OriginalRemoved).unwrap();
        assert!(migration_fully_complete(&conn, library_id).unwrap());
    }

    #[test]
    fn rewrite_paths_updates_every_vault_internal_column_but_not_external_ones() {
        let mut conn = Connection::open_in_memory().unwrap();
        let library_id = setup_library(&mut conn, "/vault");

        conn.execute(
            "INSERT INTO images (library_id, original_hash, stored_hash, stored_path, sidecar_path, original_format, stored_format, file_size_bytes)
             VALUES (?1, X'01', X'01', '/vault/blobs/ab/abcdef.jxl', '/vault/blobs/ab/abcdef.xmp', 'jpeg', 'jxl', 100)",
            params![library_id],
        ).unwrap();
        let image_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO import_batches (library_id, source_root) VALUES (?1, '/original/camera/sd-card')",
            params![library_id],
        ).unwrap();
        let batch_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO image_sources (image_id, import_batch_id, original_filename, source_path)
             VALUES (?1, ?2, 'IMG_0001.jpg', '/original/camera/sd-card/IMG_0001.jpg')",
            params![image_id, batch_id],
        ).unwrap();

        conn.execute(
            "INSERT INTO thumbnails (image_id, variant, format, path) VALUES (?1, 'grid256', 'webp', '/vault/thumbnails/ab/abcdef.webp')",
            params![image_id],
        ).unwrap();

        rewrite_paths(&conn, library_id, "/vault", "/vault/live").unwrap();

        let (stored_path, sidecar_path): (String, String) = conn
            .query_row("SELECT stored_path, sidecar_path FROM images WHERE id = ?1", params![image_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(stored_path, "/vault/live/blobs/ab/abcdef.jxl");
        assert_eq!(sidecar_path, "/vault/live/blobs/ab/abcdef.xmp");

        let thumb_path: String = conn
            .query_row("SELECT path FROM thumbnails WHERE image_id = ?1", params![image_id], |r| r.get(0))
            .unwrap();
        assert_eq!(thumb_path, "/vault/live/thumbnails/ab/abcdef.webp");

        let root_path: String = conn
            .query_row("SELECT root_path FROM libraries WHERE id = ?1", params![library_id], |r| r.get(0))
            .unwrap();
        assert_eq!(root_path, "/vault/live");

        // External, pre-import paths must survive untouched.
        let source_path: String = conn
            .query_row("SELECT source_path FROM image_sources WHERE image_id = ?1", params![image_id], |r| r.get(0))
            .unwrap();
        assert_eq!(source_path, "/original/camera/sd-card/IMG_0001.jpg");
        let source_root: String = conn
            .query_row("SELECT source_root FROM import_batches WHERE id = ?1", params![batch_id], |r| r.get(0))
            .unwrap();
        assert_eq!(source_root, "/original/camera/sd-card");
    }
}
