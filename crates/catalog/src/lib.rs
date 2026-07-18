//! SQLite catalog access layer: migrations, schema, queries. Per
//! workplan/SPEC.md §10; DDL sourced from workplan/prototypes/017-catalog-schema.md.

use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{Migrations, M};

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![M::up(include_str!("../schema.sql"))])
}

/// Brings `conn` to the current schema version and enables foreign-key
/// enforcement for its lifetime. SQLite does not enforce `REFERENCES`
/// constraints unless `PRAGMA foreign_keys = ON` is set per-connection —
/// every caller must route new connections through this before use, since
/// the schema (workplan/prototypes/017-catalog-schema.md) relies on FK
/// constraints actually being enforced.
pub fn migrate(conn: &mut Connection) -> rusqlite_migration::Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrations().to_latest(conn)
}

// ── Tags (workplan/SPEC.md §7, Milestone 4) ─────────────────────────────
//
// "Manual tagging via direct catalog calls" per the Milestone 4 line — no
// UI yet, just the CRUD this crate's callers (`lumenvault-xmp`'s sidecar
// sync, and eventually the UI) need. Kept pure SQL, no file I/O, matching
// this crate's existing character — sidecar mirroring is `lumenvault-xmp`'s
// job, not this crate's.

/// Finds or creates the `tags` row for `name` and returns its id.
fn find_or_create_tag(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    if let Some(id) = conn
        .query_row("SELECT id FROM tags WHERE name = ?1", [name], |row| row.get(0))
        .optional()?
    {
        return Ok(id);
    }
    conn.execute("INSERT INTO tags (name) VALUES (?1)", [name])?;
    Ok(conn.last_insert_rowid())
}

/// Tags `image_id` with `tag_name`, creating the `tags` row if it doesn't
/// already exist. Idempotent — tagging an image with a tag it already has
/// is a no-op, not an error (`INSERT OR IGNORE` against the `image_tags`
/// composite primary key).
pub fn add_tag(conn: &Connection, image_id: i64, tag_name: &str) -> rusqlite::Result<()> {
    let tag_id = find_or_create_tag(conn, tag_name)?;
    conn.execute(
        "INSERT OR IGNORE INTO image_tags (image_id, tag_id) VALUES (?1, ?2)",
        params![image_id, tag_id],
    )?;
    Ok(())
}

/// Removes `tag_name` from `image_id`, if present. Idempotent — removing a
/// tag that isn't there, or that doesn't exist at all, is a no-op.
pub fn remove_tag(conn: &Connection, image_id: i64, tag_name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM image_tags
         WHERE image_id = ?1 AND tag_id = (SELECT id FROM tags WHERE name = ?2)",
        params![image_id, tag_name],
    )?;
    Ok(())
}

/// The tags currently applied to `image_id`, sorted alphabetically —
/// deterministic ordering so sidecar output (`lumenvault-xmp`) doesn't
/// churn on every sync from a nondeterministic row order.
pub fn tags_for_image(conn: &Connection, image_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT t.name FROM tags t
         JOIN image_tags it ON it.tag_id = t.id
         WHERE it.image_id = ?1
         ORDER BY t.name ASC",
    )?;
    stmt.query_map([image_id], |row| row.get(0))?.collect()
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, OptionalExtension, params};

    fn migrated_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        super::migrate(&mut conn).unwrap();
        conn
    }

    #[test]
    fn migrating_a_fresh_database_creates_the_v1_schema() {
        let conn = migrated_conn();

        let table_exists = |name: &str| -> bool {
            conn.query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |_| Ok(()),
            )
            .is_ok()
        };

        for table in [
            "libraries",
            "app_settings",
            "images",
            "image_sources",
            "raw_jpeg_pairs",
            "tags",
            "image_tags",
            "import_batches",
            "import_journal",
            "quarantine",
            "dedupe_review_queue",
            "thumbnails",
            "models",
            "embeddings",
            "tag_scores",
        ] {
            assert!(table_exists(table), "expected table `{table}` to exist after migration");
        }
    }

    #[test]
    fn foreign_key_violations_are_rejected() {
        let conn = migrated_conn();

        // images.library_id references libraries(id); no library with id 999 exists.
        let result = conn.execute(
            "INSERT INTO images (
                library_id, original_hash, stored_hash, stored_path,
                original_format, stored_format, file_size_bytes
            ) VALUES (999, x'00', x'00', 'x', 'jpeg', 'jpeg', 0)",
            [],
        );

        assert!(
            result.is_err(),
            "expected a foreign key violation to be rejected, but the insert succeeded"
        );
    }

    /// A minimal `images` row, just enough to satisfy the FK the `tags`
    /// tests need — content of the row itself is irrelevant to tag CRUD.
    /// Finds-or-creates a single shared `libraries` row per test connection
    /// (tests each get a fresh in-memory `conn`, so this is still isolated
    /// across tests) rather than inserting a new one per call, since
    /// `root_path` is `UNIQUE` and some tests call this more than once.
    fn insert_test_image(conn: &Connection) -> i64 {
        let library_id: i64 = conn
            .query_row("SELECT id FROM libraries WHERE root_path = 'A:/lib'", [], |row| row.get(0))
            .optional()
            .unwrap()
            .unwrap_or_else(|| {
                conn.execute("INSERT INTO libraries (name, root_path) VALUES ('lib', 'A:/lib')", [])
                    .unwrap();
                conn.last_insert_rowid()
            });
        // original_hash is UNIQUE — a fresh random-ish value per call so
        // tests inserting more than one image against the shared library
        // above don't collide.
        let unique_hash: i64 = conn.query_row("SELECT COUNT(*) FROM images", [], |row| row.get::<_, i64>(0)).unwrap() + 1;
        conn.execute(
            "INSERT INTO images (
                library_id, original_hash, stored_hash, stored_path,
                original_format, stored_format, file_size_bytes
            ) VALUES (?1, ?2, x'00', 'x', 'jpeg', 'jpeg', 0)",
            params![library_id, unique_hash.to_le_bytes()],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn adding_a_tag_twice_is_idempotent() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::add_tag(&conn, image_id, "sunset").unwrap();
        super::add_tag(&conn, image_id, "sunset").unwrap();

        assert_eq!(super::tags_for_image(&conn, image_id).unwrap(), vec!["sunset".to_string()]);
        let tag_count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0)).unwrap();
        assert_eq!(tag_count, 1, "re-tagging must not create a duplicate tags row");
    }

    #[test]
    fn tags_for_image_are_sorted() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::add_tag(&conn, image_id, "zebra").unwrap();
        super::add_tag(&conn, image_id, "apple").unwrap();
        super::add_tag(&conn, image_id, "mango").unwrap();

        assert_eq!(
            super::tags_for_image(&conn, image_id).unwrap(),
            vec!["apple".to_string(), "mango".to_string(), "zebra".to_string()]
        );
    }

    #[test]
    fn removing_a_tag_that_was_never_applied_is_a_no_op() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::remove_tag(&conn, image_id, "nonexistent").unwrap();

        assert!(super::tags_for_image(&conn, image_id).unwrap().is_empty());
    }

    #[test]
    fn removing_a_tag_leaves_other_tags_on_the_image_intact() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::add_tag(&conn, image_id, "keep").unwrap();
        super::add_tag(&conn, image_id, "drop").unwrap();
        super::remove_tag(&conn, image_id, "drop").unwrap();

        assert_eq!(super::tags_for_image(&conn, image_id).unwrap(), vec!["keep".to_string()]);
    }

    #[test]
    fn two_images_can_share_the_same_tag_independently() {
        let conn = migrated_conn();
        let image_a = insert_test_image(&conn);
        let image_b = insert_test_image(&conn);

        super::add_tag(&conn, image_a, "shared").unwrap();
        super::add_tag(&conn, image_b, "shared").unwrap();
        super::remove_tag(&conn, image_a, "shared").unwrap();

        assert!(super::tags_for_image(&conn, image_a).unwrap().is_empty());
        assert_eq!(super::tags_for_image(&conn, image_b).unwrap(), vec!["shared".to_string()]);
    }
}
