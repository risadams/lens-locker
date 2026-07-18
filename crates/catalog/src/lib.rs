//! SQLite catalog access layer: migrations, schema, queries. Per
//! workplan/SPEC.md §10; DDL sourced from workplan/prototypes/017-catalog-schema.md.

use rusqlite::Connection;
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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

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
}
