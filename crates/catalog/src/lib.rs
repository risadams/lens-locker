//! SQLite catalog access layer: migrations, schema, queries. Per
//! workplan/SPEC.md §10; DDL sourced from workplan/prototypes/017-catalog-schema.md.

use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};

pub mod vec_mirror;
pub use vec_mirror::VecMirror;

fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("../schema.sql")),
        // Milestone 5 — see migrations/0002_standard_fts.sql's own comment
        // for why images_fts is redefined here rather than in schema.sql.
        M::up(include_str!("../migrations/0002_standard_fts.sql")),
        // ML-SPEC.md Milestone ML-1 — see migrations/0003_ml_extensions.sql's
        // own header comment; DDL sourced from workplan/prototypes/035-ml-schema.md.
        M::up(include_str!("../migrations/0003_ml_extensions.sql")),
        // ML-SPEC.md Milestone ML-2 — see migrations/0004_tag_confidence_thresholds.sql's
        // own header comment.
        M::up(include_str!("../migrations/0004_tag_confidence_thresholds.sql")),
    ])
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
// UI yet, just the CRUD this crate's callers (`lenslocker-xmp`'s sidecar
// sync, and eventually the UI) need. Kept pure SQL, no file I/O, matching
// this crate's existing character — sidecar mirroring is `lenslocker-xmp`'s
// job, not this crate's.

/// Finds or creates the `tags` row for `name` and returns its id.
fn find_or_create_tag(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    if let Some(id) = conn
        .query_row("SELECT id FROM tags WHERE name = ?1", [name], |row| {
            row.get(0)
        })
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
    sync_fts_row(conn, image_id)?;
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
    sync_fts_row(conn, image_id)?;
    Ok(())
}

/// The tags currently applied to `image_id`, sorted alphabetically —
/// deterministic ordering so sidecar output (`lenslocker-xmp`) doesn't
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

// ── ML: auto-tag provenance (ML-SPEC.md §4/§5, Milestone ML-2) ───────────

/// Applies (or refreshes the confidence of) an auto-generated tag on
/// `image_id`, unless it's been explicitly rejected (`rejected_tags` —
/// §5's "don't re-suggest this" memory) or a human already applied the
/// same tag manually. Re-scoring never demotes a manual tag's
/// provenance: the `ON CONFLICT` update is conditioned on the existing
/// row still being `source = 'auto'`, so a manual "beach" stays manual
/// even if the model independently scores "beach" on a later pass.
pub fn apply_auto_tag(conn: &Connection, image_id: i64, tag_name: &str, confidence: f64) -> rusqlite::Result<()> {
    let tag_id = find_or_create_tag(conn, tag_name)?;

    let rejected = conn
        .query_row(
            "SELECT 1 FROM rejected_tags WHERE image_id = ?1 AND tag_id = ?2",
            params![image_id, tag_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if rejected {
        return Ok(());
    }

    conn.execute(
        "INSERT INTO image_tags (image_id, tag_id, source, confidence, review_state)
         VALUES (?1, ?2, 'auto', ?3, 'unreviewed')
         ON CONFLICT(image_id, tag_id) DO UPDATE SET confidence = excluded.confidence
         WHERE image_tags.source = 'auto'",
        params![image_id, tag_id, confidence],
    )?;
    sync_fts_row(conn, image_id)?;
    Ok(())
}

/// Removes a tag from `image_id` and records the rejection (§5) so
/// re-scoring never silently reapplies it. Distinct from [`remove_tag`]:
/// that's the general "delete this tag" a human can also use on a manual
/// tag they just want gone, without the "don't re-suggest" memory (manual
/// tags are never auto-re-suggested in the first place).
pub fn reject_tag(conn: &Connection, image_id: i64, tag_name: &str) -> rusqlite::Result<()> {
    let tag_id = find_or_create_tag(conn, tag_name)?;
    conn.execute(
        "DELETE FROM image_tags WHERE image_id = ?1 AND tag_id = ?2",
        params![image_id, tag_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO rejected_tags (image_id, tag_id) VALUES (?1, ?2)",
        params![image_id, tag_id],
    )?;
    sync_fts_row(conn, image_id)?;
    Ok(())
}

/// Flips an auto-tag's `review_state` to `confirmed` without touching its
/// `source` — confirming doesn't turn a model-generated tag into a
/// manually-typed one, keeping provenance honest (§4). A no-op if
/// `tag_name` isn't currently an auto tag on `image_id`.
pub fn confirm_auto_tag(conn: &Connection, image_id: i64, tag_name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE image_tags SET review_state = 'confirmed'
         WHERE image_id = ?1 AND tag_id = (SELECT id FROM tags WHERE name = ?2) AND source = 'auto'",
        params![image_id, tag_name],
    )?;
    Ok(())
}

// ── ML: models/embeddings (ML-SPEC.md §2/§11, Milestone ML-1 schema,
// Milestone ML-2 first real callers) ──────────────────────────────────────

/// Finds or creates the `models` row identifying a specific model
/// name+version, returning its id. `dimension`/`license_note` only take
/// effect on first insert — an existing row wins on a repeat call
/// (matching [`find_or_create_tag`]'s find-first pattern), since changing
/// either without a version bump would silently corrupt how
/// already-stored embeddings for that model are interpreted.
pub fn find_or_create_model(conn: &Connection, name: &str, version: &str, dimension: i64, license_note: &str) -> rusqlite::Result<i64> {
    if let Some(id) = conn
        .query_row("SELECT id FROM models WHERE name = ?1 AND version = ?2", params![name, version], |row| row.get(0))
        .optional()?
    {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO models (name, version, dimension, license_note) VALUES (?1, ?2, ?3, ?4)",
        params![name, version, dimension, license_note],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Stores (or replaces) `image_id`'s embedding for `model_id`. `vector` is
/// opaque bytes (little-endian `f32`s, `crates/ml`'s encoding to produce)
/// — this layer never interprets embedding content, matching this crate's
/// "pure SQL, no format-specific logic" layering discipline.
pub fn upsert_embedding(conn: &Connection, image_id: i64, model_id: i64, vector: &[u8], dimension: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO embeddings (image_id, model_id, vector, dimension) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(image_id, model_id) DO UPDATE SET vector = excluded.vector, dimension = excluded.dimension",
        params![image_id, model_id, vector, dimension],
    )?;
    Ok(())
}

/// Up to `limit` active images that don't yet have a stored embedding for
/// `model_id` — the background backlog (§9), computed implicitly from
/// `embeddings`'s absence rather than a separate persisted queue table
/// (`workplan/prototypes/035-ml-schema.md`: `embeddings`/`models` "need no
/// schema change at all"). Ordered by id for deterministic, resumable
/// batching — a canceled/restarted backlog pass just re-runs this query
/// and picks up where it left off.
pub fn images_needing_embedding(conn: &Connection, model_id: i64, limit: i64) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT i.id FROM images i
         WHERE i.status = 'active'
         AND NOT EXISTS (SELECT 1 FROM embeddings e WHERE e.image_id = i.id AND e.model_id = ?1)
         ORDER BY i.id
         LIMIT ?2",
    )?;
    stmt.query_map(params![model_id, limit], |row| row.get(0))?.collect()
}

// ── Full-text search index sync (workplan/SPEC.md §9's "text search" filter,
// Milestone 5) ───────────────────────────────────────────────────────────
//
// `images_fts` (schema.sql) is a `content=''` (contentless) FTS5 table: it
// stores only the search index, not a queryable copy of the column text —
// callers get matching rowids back from a MATCH query, then join against
// `images` for the real data. Nothing populated this table before Milestone
// 5; [`sync_fts_row`] is the one function that keeps it current, called
// after every event that changes what a search should match (a new image's
// filename/camera becoming known, or its tag list changing).

/// Rebuilds `images_fts`'s row for `image_id` from the image's current
/// camera fields, its most-recently-recorded source filename, and its
/// current tags — a full delete+reinsert rather than an in-place update,
/// since FTS5 doesn't support updating individual columns of a contentless
/// row. Idempotent and safe to call as often as needed (e.g. after every
/// `add_tag`/`remove_tag`).
pub fn sync_fts_row(conn: &Connection, image_id: i64) -> rusqlite::Result<()> {
    let filename: Option<String> = conn
        .query_row(
            "SELECT original_filename FROM image_sources
             WHERE image_id = ?1 ORDER BY imported_at DESC LIMIT 1",
            [image_id],
            |row| row.get(0),
        )
        .optional()?;
    let (camera_make, camera_model): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT camera_make, camera_model FROM images WHERE id = ?1",
            [image_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or((None, None));
    let tags = tags_for_image(conn, image_id)?;
    let tag_names = tags.join(" ");

    conn.execute("DELETE FROM images_fts WHERE rowid = ?1", [image_id])?;
    conn.execute(
        "INSERT INTO images_fts (rowid, original_filename, camera_make, camera_model, tag_names)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            image_id,
            filename.unwrap_or_default(),
            camera_make.unwrap_or_default(),
            camera_model.unwrap_or_default(),
            tag_names
        ],
    )?;
    Ok(())
}

// ── Grid queries (workplan/SPEC.md §9, Milestone 5) ─────────────────────
//
// `list_images` backs the virtualized grid: real SQL with filter/sort/
// search/pagination, never a full-table fetch into memory. It returns only
// what a grid card needs (§9's own scoping call), not full metadata —
// [`get_image_detail`] is the separate, per-image call for the drawer.

/// Filter selections for [`list_images`] — an empty `Vec`/`None` means "no
/// filter on this facet." Per workplan/SPEC.md §9: multi-select facets
/// (`formats`, `sources`, `tags`) are OR-within-the-facet, AND across
/// facets — matching the approved design's documented filter behavior.
#[derive(Debug, Default, Clone)]
pub struct ImageFilters {
    /// Inclusive ISO-8601 lower bound on `capture_date`.
    pub date_from: Option<String>,
    /// Inclusive ISO-8601 upper bound on `capture_date`.
    pub date_to: Option<String>,
    /// `images.original_format` values; OR'd together.
    pub formats: Vec<String>,
    /// `import_batches.source_root` values (the folder chosen at import
    /// time) — the vault isn't Explorer-browsable by design (SPEC.md §3),
    /// so "source" means *where a photo was imported from*, not a live
    /// on-disk path. OR'd together.
    pub sources: Vec<String>,
    /// Tag names; OR'd together.
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    CapturedDesc,
    CapturedAsc,
    ImportedDesc,
    FilenameAsc,
    SizeDesc,
}

impl SortOrder {
    fn order_by_clause(self) -> &'static str {
        match self {
            // NULLS LAST isn't native pre-3.30 SQLite syntax portability
            // concerns aside, bundled rusqlite ships a modern SQLite that
            // supports it directly.
            Self::CapturedDesc => "im.capture_date DESC NULLS LAST, im.id DESC",
            Self::CapturedAsc => "im.capture_date ASC NULLS LAST, im.id ASC",
            Self::ImportedDesc => "latest_import DESC, im.id DESC",
            Self::FilenameAsc => "latest_filename COLLATE NOCASE ASC, im.id ASC",
            Self::SizeDesc => "im.file_size_bytes DESC, im.id DESC",
        }
    }
}

/// One grid card's worth of data — deliberately not full metadata (see
/// [`get_image_detail`] for that), per workplan/SPEC.md §9's scoping.
#[derive(Debug, Clone)]
pub struct GridImage {
    pub id: i64,
    pub thumbnail_path: Option<String>,
    pub capture_date: Option<String>,
    pub tags: Vec<String>,
    /// Always `true` in this schema: every stored image already passed
    /// blob-integrity verification before its `images` row was ever
    /// committed (workplan/SPEC.md §3) — there is no reachable "unverified"
    /// state to represent. Kept as an explicit field (rather than dropped)
    /// because the approved design surfaces a verified glyph on every card.
    pub verified: bool,
}

/// Real SQL against `images` (joined to `image_tags`/`tags` for tag
/// filtering, `image_sources`/`import_batches` for source filtering and
/// imported-sort, `thumbnails` for the grid thumbnail, `images_fts` for
/// text search) — never a full-table fetch. Returns `(page, total_matching)`
/// so the virtualized grid can size its scroll extent without a second
/// query.
pub fn list_images(
    conn: &Connection,
    filters: &ImageFilters,
    sort: SortOrder,
    search: Option<&str>,
    offset: i64,
    limit: i64,
) -> rusqlite::Result<(Vec<GridImage>, i64)> {
    let mut where_clauses = vec!["im.status = 'active'".to_string()];
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(from) = &filters.date_from {
        where_clauses.push("im.capture_date >= ?".to_string());
        args.push(Box::new(from.clone()));
    }
    if let Some(to) = &filters.date_to {
        where_clauses.push("im.capture_date <= ?".to_string());
        args.push(Box::new(to.clone()));
    }
    if !filters.formats.is_empty() {
        let placeholders = vec!["?"; filters.formats.len()].join(",");
        where_clauses.push(format!("im.original_format IN ({placeholders})"));
        for f in &filters.formats {
            args.push(Box::new(f.clone()));
        }
    }
    if !filters.sources.is_empty() {
        let placeholders = vec!["?"; filters.sources.len()].join(",");
        where_clauses.push(format!(
            "im.id IN (SELECT s.image_id FROM image_sources s \
             JOIN import_batches b ON b.id = s.import_batch_id \
             WHERE b.source_root IN ({placeholders}))"
        ));
        for s in &filters.sources {
            args.push(Box::new(s.clone()));
        }
    }
    if !filters.tags.is_empty() {
        let placeholders = vec!["?"; filters.tags.len()].join(",");
        where_clauses.push(format!(
            "im.id IN (SELECT it.image_id FROM image_tags it \
             JOIN tags t ON t.id = it.tag_id WHERE t.name IN ({placeholders}))"
        ));
        for t in &filters.tags {
            args.push(Box::new(t.clone()));
        }
    }
    if let Some(q) = search.filter(|q| !q.trim().is_empty()) {
        // FTS5 query syntax treats bare special characters (quotes,
        // hyphens...) as syntax errors, not literal text; wrapping the raw
        // search term in double quotes with internal quotes escaped turns
        // it into a single FTS5 string-literal phrase match, which is what
        // a "search box" experience needs (match this text), not a
        // FTS5-query-language experience.
        let escaped = q.replace('"', "\"\"");
        where_clauses
            .push("im.id IN (SELECT rowid FROM images_fts WHERE images_fts MATCH ?)".to_string());
        args.push(Box::new(format!("\"{escaped}\"")));
    }

    let where_sql = where_clauses.join(" AND ");
    let count_sql = format!("SELECT COUNT(*) FROM images im WHERE {where_sql}");
    let total: i64 = conn.query_row(
        &count_sql,
        rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
        |row| row.get(0),
    )?;

    let sql = format!(
        "SELECT im.id, th.path, im.capture_date,
                (SELECT MAX(s.imported_at) FROM image_sources s WHERE s.image_id = im.id) AS latest_import,
                (SELECT s.original_filename FROM image_sources s WHERE s.image_id = im.id ORDER BY s.imported_at DESC LIMIT 1) AS latest_filename
         FROM images im
         LEFT JOIN thumbnails th ON th.image_id = im.id AND th.variant = 'grid256'
         WHERE {where_sql}
         ORDER BY {order}
         LIMIT ? OFFSET ?",
        order = sort.order_by_clause(),
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut all_args: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    all_args.push(&limit);
    all_args.push(&offset);

    let rows: Vec<(i64, Option<String>, Option<String>)> = stmt
        .query_map(rusqlite::params_from_iter(all_args), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let mut page = Vec::with_capacity(rows.len());
    for (id, thumbnail_path, capture_date) in rows {
        let tags = tags_for_image(conn, id)?;
        page.push(GridImage {
            id,
            thumbnail_path,
            capture_date,
            tags,
            verified: true,
        });
    }

    Ok((page, total))
}

/// Full metadata for one image, for the detail drawer — everything the
/// approved design's drawer shows (workplan/SPEC.md §9/Milestone 5).
#[derive(Debug, Clone)]
pub struct ImageDetail {
    pub id: i64,
    pub filename: String,
    pub original_format: String,
    pub stored_format: String,
    pub conversion_status: String,
    pub capture_date: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub original_hash_hex: String,
    pub file_size_bytes: i64,
    pub stored_path: String,
    pub tags: Vec<String>,
    pub first_imported_at: String,
}

pub fn get_image_detail(conn: &Connection, image_id: i64) -> rusqlite::Result<Option<ImageDetail>> {
    let row: Option<(String, String, String, Option<String>, Option<String>, Option<String>, Option<i64>, Option<i64>, Vec<u8>, i64, String, String)> = conn
        .query_row(
            "SELECT original_format, stored_format, conversion_status, capture_date, camera_make,
                    camera_model, width, height, original_hash, file_size_bytes, stored_path, first_imported_at
             FROM images WHERE id = ?1",
            [image_id],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                    row.get(10)?, row.get(11)?,
                ))
            },
        )
        .optional()?;

    let Some((
        original_format,
        stored_format,
        conversion_status,
        capture_date,
        camera_make,
        camera_model,
        width,
        height,
        original_hash,
        file_size_bytes,
        stored_path,
        first_imported_at,
    )) = row
    else {
        return Ok(None);
    };

    let filename: String = conn
        .query_row(
            "SELECT original_filename FROM image_sources WHERE image_id = ?1 ORDER BY imported_at DESC LIMIT 1",
            [image_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default();

    let original_hash_hex = original_hash.iter().map(|b| format!("{b:02x}")).collect();
    let tags = tags_for_image(conn, image_id)?;

    Ok(Some(ImageDetail {
        id: image_id,
        filename,
        original_format,
        stored_format,
        conversion_status,
        capture_date,
        camera_make,
        camera_model,
        width,
        height,
        original_hash_hex,
        file_size_bytes,
        stored_path,
        tags,
        first_imported_at,
    }))
}

/// A tag and how many active images carry it — for the tag filter popover's
/// counts.
#[derive(Debug, Clone)]
pub struct TagCount {
    pub name: String,
    pub count: i64,
}

pub fn list_tags(conn: &Connection) -> rusqlite::Result<Vec<TagCount>> {
    let mut stmt = conn.prepare(
        "SELECT t.name, COUNT(*) FROM tags t
         JOIN image_tags it ON it.tag_id = t.id
         JOIN images im ON im.id = it.image_id AND im.status = 'active'
         GROUP BY t.id ORDER BY COUNT(*) DESC, t.name ASC",
    )?;
    stmt.query_map([], |row| {
        Ok(TagCount {
            name: row.get(0)?,
            count: row.get(1)?,
        })
    })?
    .collect()
}

#[derive(Debug, Clone)]
pub struct SourceCount {
    pub source_root: String,
    pub count: i64,
}

/// Distinct import source roots (the folder chosen at import time) with a
/// count of active images that came from each — for the source filter
/// popover. See [`ImageFilters::sources`] for why "source" means this and
/// not a live filesystem path.
pub fn list_sources(conn: &Connection) -> rusqlite::Result<Vec<SourceCount>> {
    let mut stmt = conn.prepare(
        "SELECT b.source_root, COUNT(DISTINCT im.id) FROM import_batches b
         JOIN image_sources s ON s.import_batch_id = b.id
         JOIN images im ON im.id = s.image_id AND im.status = 'active'
         GROUP BY b.source_root ORDER BY COUNT(DISTINCT im.id) DESC",
    )?;
    stmt.query_map([], |row| {
        Ok(SourceCount {
            source_root: row.get(0)?,
            count: row.get(1)?,
        })
    })?
    .collect()
}

/// Records a generated thumbnail (workplan/SPEC.md §9's `thumbnails` table)
/// — upserts on `(image_id, variant)` so regenerating a thumbnail (e.g. a
/// future eviction/regen pass) doesn't violate the schema's `UNIQUE`
/// constraint.
pub fn record_thumbnail(
    conn: &Connection,
    image_id: i64,
    variant: &str,
    format: &str,
    path: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO thumbnails (image_id, variant, format, path)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (image_id, variant) DO UPDATE SET format = excluded.format, path = excluded.path,
             generated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        params![image_id, variant, format, path],
    )?;
    Ok(())
}

// ── App settings (workplan/SPEC.md §5.5, Milestone 5.5) ─────────────────
//
// `app_settings` is a singleton row (`id = 1`, seeded by schema.sql) holding
// the two values tickets 011/005 explicitly called user-tunable but no
// milestone ever gave a UI: dedupe sensitivity and quarantine retention.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AppSettings {
    pub hamming_threshold: i64,
    pub retention_days: i64,
    /// §4's storage floor — an auto-tag score below this never gets an
    /// `image_tags` row at all (migration 0004; placeholder pending real
    /// calibration, see that migration's own header comment).
    pub tag_storage_threshold: f64,
    /// §4's display floor — an auto-tag at or above the storage floor but
    /// below this still gets a row (so confirming it later doesn't need a
    /// re-score), it just doesn't surface as a visible chip by default.
    pub tag_display_threshold: f64,
}

pub fn get_app_settings(conn: &Connection) -> rusqlite::Result<AppSettings> {
    conn.query_row(
        "SELECT hamming_threshold, retention_days, tag_storage_threshold, tag_display_threshold FROM app_settings WHERE id = 1",
        [],
        |row| {
            Ok(AppSettings {
                hamming_threshold: row.get(0)?,
                retention_days: row.get(1)?,
                tag_storage_threshold: row.get(2)?,
                tag_display_threshold: row.get(3)?,
            })
        },
    )
}

pub fn update_app_settings(conn: &Connection, settings: AppSettings) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE app_settings SET hamming_threshold = ?1, retention_days = ?2,
         tag_storage_threshold = ?3, tag_display_threshold = ?4 WHERE id = 1",
        params![
            settings.hamming_threshold,
            settings.retention_days,
            settings.tag_storage_threshold,
            settings.tag_display_threshold
        ],
    )?;
    Ok(())
}

// ── Review queue (workplan/SPEC.md §6, Milestone 5) ─────────────────────
//
// Milestone 3 built detection (`dedupe_review_queue` rows); resolution
// never existed until now.

#[derive(Debug, Clone)]
pub struct ReviewQueueEntry {
    pub queue_id: i64,
    pub hamming_distance: i64,
    pub image_a: GridImage,
    pub image_b: GridImage,
}

/// Every `pending` review-queue row, joined to each side's summary data for
/// the side-by-side compare UI.
pub fn list_review_queue(conn: &Connection) -> rusqlite::Result<Vec<ReviewQueueEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, image_a_id, image_b_id, hamming_distance FROM dedupe_review_queue
         WHERE status = 'pending' ORDER BY created_at ASC",
    )?;
    let rows: Vec<(i64, i64, i64, i64)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    let mut entries = Vec::with_capacity(rows.len());
    for (queue_id, a_id, b_id, hamming_distance) in rows {
        let Some(image_a) = grid_image_by_id(conn, a_id)? else {
            continue;
        };
        let Some(image_b) = grid_image_by_id(conn, b_id)? else {
            continue;
        };
        entries.push(ReviewQueueEntry {
            queue_id,
            hamming_distance,
            image_a,
            image_b,
        });
    }
    Ok(entries)
}

fn grid_image_by_id(conn: &Connection, image_id: i64) -> rusqlite::Result<Option<GridImage>> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT th.path, im.capture_date FROM images im
             LEFT JOIN thumbnails th ON th.image_id = im.id AND th.variant = 'grid256'
             WHERE im.id = ?1",
            [image_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((thumbnail_path, capture_date)) = row else {
        return Ok(None);
    };
    let tags = tags_for_image(conn, image_id)?;
    Ok(Some(GridImage {
        id: image_id,
        thumbnail_path,
        capture_date,
        tags,
        verified: true,
    }))
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
            "rejected_tags",
            "persons",
            "face_clusters",
            "face_detections",
            "face_match_review_queue",
            "saved_albums",
        ] {
            assert!(
                table_exists(table),
                "expected table `{table}` to exist after migration"
            );
        }

        // tag_scores is superseded by image_tags.confidence — dropped by
        // migration 0003 (workplan/prototypes/035-ml-schema.md [CALL] 1).
        assert!(
            !table_exists("tag_scores"),
            "expected tag_scores to be dropped by migration 0003"
        );
    }

    #[test]
    fn image_tags_carry_provenance_columns_with_sane_defaults() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        super::add_tag(&conn, image_id, "beach").unwrap();

        let (source, confidence, review_state): (String, Option<f64>, Option<String>) = conn
            .query_row(
                "SELECT it.source, it.confidence, it.review_state
                 FROM image_tags it JOIN tags t ON t.id = it.tag_id
                 WHERE it.image_id = ?1 AND t.name = 'beach'",
                [image_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(source, "manual");
        assert_eq!(confidence, None);
        assert_eq!(review_state, None);

        let result = conn.execute(
            "UPDATE image_tags SET source = 'bogus'
             WHERE image_id = ?1 AND tag_id = (SELECT id FROM tags WHERE name = 'beach')",
            [image_id],
        );
        assert!(result.is_err(), "expected the source CHECK constraint to reject an unknown value");
    }

    #[test]
    fn app_settings_carries_face_thresholds_in_required_order() {
        let conn = migrated_conn();
        let (cluster, review, auto_attribute): (f64, f64, f64) = conn
            .query_row(
                "SELECT face_cluster_threshold, face_review_threshold, face_auto_attribute_threshold
                 FROM app_settings WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        // workplan/prototypes/035-ml-schema.md: cluster <= review <= auto-attribute is a
        // hard ordering requirement (028), independent of the two
        // placeholder values ever being recalibrated.
        assert!(cluster <= review);
        assert!(review <= auto_attribute);
        assert_eq!(review, 0.363, "face_review_threshold is SFace's own sourced LFW verification threshold");
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
            .query_row(
                "SELECT id FROM libraries WHERE root_path = 'A:/lib'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
            .unwrap_or_else(|| {
                conn.execute(
                    "INSERT INTO libraries (name, root_path) VALUES ('lib', 'A:/lib')",
                    [],
                )
                .unwrap();
                conn.last_insert_rowid()
            });
        // original_hash is UNIQUE — a fresh random-ish value per call so
        // tests inserting more than one image against the shared library
        // above don't collide.
        let unique_hash: i64 = conn
            .query_row("SELECT COUNT(*) FROM images", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
            + 1;
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

        assert_eq!(
            super::tags_for_image(&conn, image_id).unwrap(),
            vec!["sunset".to_string()]
        );
        let tag_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            tag_count, 1,
            "re-tagging must not create a duplicate tags row"
        );
    }

    #[test]
    fn apply_auto_tag_inserts_with_auto_provenance_and_unreviewed_state() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::apply_auto_tag(&conn, image_id, "beach", 0.87).unwrap();

        let (source, confidence, review_state): (String, Option<f64>, Option<String>) = conn
            .query_row(
                "SELECT it.source, it.confidence, it.review_state
                 FROM image_tags it JOIN tags t ON t.id = it.tag_id
                 WHERE it.image_id = ?1 AND t.name = 'beach'",
                [image_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(source, "auto");
        assert_eq!(confidence, Some(0.87));
        assert_eq!(review_state, Some("unreviewed".to_string()));
    }

    #[test]
    fn apply_auto_tag_refreshes_confidence_on_a_later_rescore() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::apply_auto_tag(&conn, image_id, "beach", 0.60).unwrap();
        super::apply_auto_tag(&conn, image_id, "beach", 0.92).unwrap();

        let confidence: f64 = conn
            .query_row(
                "SELECT it.confidence FROM image_tags it JOIN tags t ON t.id = it.tag_id
                 WHERE it.image_id = ?1 AND t.name = 'beach'",
                [image_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(confidence, 0.92);
    }

    #[test]
    fn apply_auto_tag_never_demotes_an_existing_manual_tags_provenance() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::add_tag(&conn, image_id, "beach").unwrap();
        super::apply_auto_tag(&conn, image_id, "beach", 0.99).unwrap();

        let (source, confidence): (String, Option<f64>) = conn
            .query_row(
                "SELECT it.source, it.confidence FROM image_tags it JOIN tags t ON t.id = it.tag_id
                 WHERE it.image_id = ?1 AND t.name = 'beach'",
                [image_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(source, "manual", "a manual tag must not be overwritten by a later auto-score");
        assert_eq!(confidence, None);
    }

    #[test]
    fn reject_tag_removes_it_and_blocks_future_auto_reapplication() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::apply_auto_tag(&conn, image_id, "beach", 0.80).unwrap();
        super::reject_tag(&conn, image_id, "beach").unwrap();
        assert_eq!(super::tags_for_image(&conn, image_id).unwrap(), Vec::<String>::new());

        // A later re-score must not silently reapply the rejected tag.
        super::apply_auto_tag(&conn, image_id, "beach", 0.95).unwrap();
        assert_eq!(super::tags_for_image(&conn, image_id).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn reject_tag_never_blocks_a_deliberate_manual_re_add() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::apply_auto_tag(&conn, image_id, "beach", 0.80).unwrap();
        super::reject_tag(&conn, image_id, "beach").unwrap();
        super::add_tag(&conn, image_id, "beach").unwrap();

        assert_eq!(super::tags_for_image(&conn, image_id).unwrap(), vec!["beach".to_string()]);
    }

    #[test]
    fn confirm_auto_tag_flips_review_state_without_changing_source() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::apply_auto_tag(&conn, image_id, "beach", 0.55).unwrap();
        super::confirm_auto_tag(&conn, image_id, "beach").unwrap();

        let (source, review_state): (String, Option<String>) = conn
            .query_row(
                "SELECT it.source, it.review_state FROM image_tags it JOIN tags t ON t.id = it.tag_id
                 WHERE it.image_id = ?1 AND t.name = 'beach'",
                [image_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(source, "auto");
        assert_eq!(review_state, Some("confirmed".to_string()));
    }

    #[test]
    fn find_or_create_model_is_idempotent_and_keyed_on_name_and_version() {
        let conn = migrated_conn();
        let id_a = super::find_or_create_model(&conn, "siglip-so400m", "v1", 1152, "Apache-2.0").unwrap();
        let id_b = super::find_or_create_model(&conn, "siglip-so400m", "v1", 1152, "Apache-2.0").unwrap();
        let id_c = super::find_or_create_model(&conn, "siglip-so400m", "v2", 1152, "Apache-2.0").unwrap();
        assert_eq!(id_a, id_b);
        assert_ne!(id_a, id_c);
    }

    #[test]
    fn images_needing_embedding_excludes_images_with_an_existing_embedding() {
        let conn = migrated_conn();
        let image_with = insert_test_image(&conn);
        let image_without = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "siglip-so400m", "v1", 1152, "Apache-2.0").unwrap();

        super::upsert_embedding(&conn, image_with, model_id, &[0u8; 4], 1).unwrap();

        let backlog = super::images_needing_embedding(&conn, model_id, 100).unwrap();
        assert_eq!(backlog, vec![image_without]);
    }

    #[test]
    fn images_needing_embedding_respects_the_limit_and_orders_by_id() {
        let conn = migrated_conn();
        let a = insert_test_image(&conn);
        let b = insert_test_image(&conn);
        let _c = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "siglip-so400m", "v1", 1152, "Apache-2.0").unwrap();

        let backlog = super::images_needing_embedding(&conn, model_id, 2).unwrap();
        assert_eq!(backlog, vec![a, b]);
    }

    #[test]
    fn upsert_embedding_replaces_the_vector_on_a_second_call() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "siglip-so400m", "v1", 4, "Apache-2.0").unwrap();

        super::upsert_embedding(&conn, image_id, model_id, &[1, 2, 3, 4], 1).unwrap();
        super::upsert_embedding(&conn, image_id, model_id, &[9, 9, 9, 9], 1).unwrap();

        let vector: Vec<u8> = conn
            .query_row(
                "SELECT vector FROM embeddings WHERE image_id = ?1 AND model_id = ?2",
                params![image_id, model_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(vector, vec![9, 9, 9, 9]);

        let row_count: i64 = conn.query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0)).unwrap();
        assert_eq!(row_count, 1, "upsert must replace, not duplicate");
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
            vec![
                "apple".to_string(),
                "mango".to_string(),
                "zebra".to_string()
            ]
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

        assert_eq!(
            super::tags_for_image(&conn, image_id).unwrap(),
            vec!["keep".to_string()]
        );
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
        assert_eq!(
            super::tags_for_image(&conn, image_b).unwrap(),
            vec!["shared".to_string()]
        );
    }

    // ── Milestone 5: grid queries, review queue ─────────────────────────

    /// Inserts a fully-specified image (format/date/size/source) for the
    /// grid-query tests below, which — unlike the tag tests — need those
    /// fields to actually differ between rows to exercise filtering/sorting.
    #[allow(clippy::too_many_arguments)]
    fn insert_full_image(
        conn: &Connection,
        library_id: i64,
        original_hash: u8,
        original_format: &str,
        capture_date: &str,
        size_bytes: i64,
        source_root: &str,
        filename: &str,
    ) -> i64 {
        conn.execute(
            "INSERT INTO images (
                library_id, original_hash, stored_hash, stored_path,
                original_format, stored_format, file_size_bytes, capture_date
            ) VALUES (?1, ?2, x'00', ?3, ?4, ?4, ?5, ?6)",
            params![
                library_id,
                [original_hash; 32].to_vec(),
                format!("blob-{original_hash}"),
                original_format,
                size_bytes,
                capture_date
            ],
        )
        .unwrap();
        let image_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO import_batches (library_id, source_root, status) VALUES (?1, ?2, 'completed')",
            params![library_id, source_root],
        )
        .unwrap();
        let batch_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO image_sources (image_id, import_batch_id, original_filename, source_path)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                image_id,
                batch_id,
                filename,
                format!("{source_root}/{filename}")
            ],
        )
        .unwrap();
        image_id
    }

    fn test_library(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO libraries (name, root_path) VALUES ('lib', 'A:/lib')",
            [],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn list_images_filters_by_format_and_returns_total_count() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        insert_full_image(
            &conn,
            library_id,
            1,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/src",
            "a.jpg",
        );
        insert_full_image(
            &conn,
            library_id,
            2,
            "png",
            "2026-01-02T00:00:00Z",
            200,
            "D:/src",
            "b.png",
        );

        let filters = super::ImageFilters {
            formats: vec!["jpeg".to_string()],
            ..Default::default()
        };
        let (page, total) =
            super::list_images(&conn, &filters, super::SortOrder::CapturedDesc, None, 0, 10)
                .unwrap();

        assert_eq!(total, 1);
        assert_eq!(page.len(), 1);
        assert!(page[0].verified);
    }

    #[test]
    fn list_images_paginates_with_offset_and_limit() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        for i in 0..5u8 {
            insert_full_image(
                &conn,
                library_id,
                i + 10,
                "jpeg",
                &format!("2026-01-0{}T00:00:00Z", i + 1),
                100,
                "D:/src",
                "x.jpg",
            );
        }

        let (page1, total) = super::list_images(
            &conn,
            &super::ImageFilters::default(),
            super::SortOrder::CapturedAsc,
            None,
            0,
            2,
        )
        .unwrap();
        let (page2, _) = super::list_images(
            &conn,
            &super::ImageFilters::default(),
            super::SortOrder::CapturedAsc,
            None,
            2,
            2,
        )
        .unwrap();

        assert_eq!(total, 5);
        assert_eq!(page1.len(), 2);
        assert_eq!(page2.len(), 2);
        assert_ne!(
            page1[0].id, page2[0].id,
            "different pages must return different rows"
        );
    }

    #[test]
    fn list_images_sorts_by_captured_date_descending() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let older = insert_full_image(
            &conn,
            library_id,
            20,
            "jpeg",
            "2020-01-01T00:00:00Z",
            100,
            "D:/src",
            "old.jpg",
        );
        let newer = insert_full_image(
            &conn,
            library_id,
            21,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/src",
            "new.jpg",
        );

        let (page, _) = super::list_images(
            &conn,
            &super::ImageFilters::default(),
            super::SortOrder::CapturedDesc,
            None,
            0,
            10,
        )
        .unwrap();

        assert_eq!(page[0].id, newer);
        assert_eq!(page[1].id, older);
    }

    #[test]
    fn list_images_filters_by_tag_with_or_semantics_within_the_facet() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let sunset = insert_full_image(
            &conn,
            library_id,
            30,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/src",
            "s.jpg",
        );
        let beach = insert_full_image(
            &conn,
            library_id,
            31,
            "jpeg",
            "2026-01-02T00:00:00Z",
            100,
            "D:/src",
            "b.jpg",
        );
        let untagged = insert_full_image(
            &conn,
            library_id,
            32,
            "jpeg",
            "2026-01-03T00:00:00Z",
            100,
            "D:/src",
            "u.jpg",
        );
        super::add_tag(&conn, sunset, "sunset").unwrap();
        super::add_tag(&conn, beach, "beach").unwrap();

        let filters = super::ImageFilters {
            tags: vec!["sunset".to_string(), "beach".to_string()],
            ..Default::default()
        };
        let (page, total) =
            super::list_images(&conn, &filters, super::SortOrder::CapturedDesc, None, 0, 10)
                .unwrap();

        let ids: Vec<i64> = page.iter().map(|g| g.id).collect();
        assert_eq!(total, 2);
        assert!(ids.contains(&sunset) && ids.contains(&beach) && !ids.contains(&untagged));
    }

    #[test]
    fn list_images_filters_by_source_root() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let from_a = insert_full_image(
            &conn,
            library_id,
            40,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/tripA",
            "a.jpg",
        );
        let _from_b = insert_full_image(
            &conn,
            library_id,
            41,
            "jpeg",
            "2026-01-02T00:00:00Z",
            100,
            "D:/tripB",
            "b.jpg",
        );

        let filters = super::ImageFilters {
            sources: vec!["D:/tripA".to_string()],
            ..Default::default()
        };
        let (page, total) =
            super::list_images(&conn, &filters, super::SortOrder::CapturedDesc, None, 0, 10)
                .unwrap();

        assert_eq!(total, 1);
        assert_eq!(page[0].id, from_a);
    }

    #[test]
    fn list_images_excludes_merged_images() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let keeper = insert_full_image(
            &conn,
            library_id,
            50,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/src",
            "k.jpg",
        );
        let loser = insert_full_image(
            &conn,
            library_id,
            51,
            "jpeg",
            "2026-01-02T00:00:00Z",
            100,
            "D:/src",
            "l.jpg",
        );
        conn.execute(
            "UPDATE images SET status = 'merged', merged_into_id = ?1 WHERE id = ?2",
            params![keeper, loser],
        )
        .unwrap();

        let (page, total) = super::list_images(
            &conn,
            &super::ImageFilters::default(),
            super::SortOrder::CapturedDesc,
            None,
            0,
            10,
        )
        .unwrap();

        assert_eq!(total, 1);
        assert_eq!(page[0].id, keeper);
    }

    #[test]
    fn text_search_matches_synced_filenames_and_tags() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let sunset_pic = insert_full_image(
            &conn,
            library_id,
            60,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/src",
            "vacation_beach.jpg",
        );
        let other = insert_full_image(
            &conn,
            library_id,
            61,
            "jpeg",
            "2026-01-02T00:00:00Z",
            100,
            "D:/src",
            "receipt_scan.jpg",
        );
        super::add_tag(&conn, sunset_pic, "sunset").unwrap();
        super::sync_fts_row(&conn, sunset_pic).unwrap();
        super::sync_fts_row(&conn, other).unwrap();

        let (by_tag, _) = super::list_images(
            &conn,
            &super::ImageFilters::default(),
            super::SortOrder::CapturedDesc,
            Some("sunset"),
            0,
            10,
        )
        .unwrap();
        let (by_filename, _) = super::list_images(
            &conn,
            &super::ImageFilters::default(),
            super::SortOrder::CapturedDesc,
            Some("vacation"),
            0,
            10,
        )
        .unwrap();

        assert_eq!(by_tag.len(), 1);
        assert_eq!(by_tag[0].id, sunset_pic);
        assert_eq!(by_filename.len(), 1);
        assert_eq!(by_filename[0].id, sunset_pic);
    }

    #[test]
    fn get_image_detail_returns_full_metadata_including_tags() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let image_id = insert_full_image(
            &conn,
            library_id,
            70,
            "jpeg",
            "2026-01-01T00:00:00Z",
            1234,
            "D:/src",
            "photo.jpg",
        );
        super::add_tag(&conn, image_id, "family").unwrap();

        let detail = super::get_image_detail(&conn, image_id).unwrap().unwrap();

        assert_eq!(detail.filename, "photo.jpg");
        assert_eq!(detail.original_format, "jpeg");
        assert_eq!(detail.file_size_bytes, 1234);
        assert_eq!(detail.tags, vec!["family".to_string()]);
        assert_eq!(detail.original_hash_hex.len(), 64);
    }

    #[test]
    fn get_image_detail_returns_none_for_a_nonexistent_image() {
        let conn = migrated_conn();
        assert!(super::get_image_detail(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn list_tags_returns_counts_across_active_images_only() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let a = insert_full_image(
            &conn,
            library_id,
            80,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/src",
            "a.jpg",
        );
        let b = insert_full_image(
            &conn,
            library_id,
            81,
            "jpeg",
            "2026-01-02T00:00:00Z",
            100,
            "D:/src",
            "b.jpg",
        );
        super::add_tag(&conn, a, "family").unwrap();
        super::add_tag(&conn, b, "family").unwrap();

        let tags = super::list_tags(&conn).unwrap();

        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "family");
        assert_eq!(tags[0].count, 2);
    }

    #[test]
    fn list_sources_returns_distinct_import_roots_with_counts() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        insert_full_image(
            &conn,
            library_id,
            90,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/tripA",
            "a.jpg",
        );
        insert_full_image(
            &conn,
            library_id,
            91,
            "jpeg",
            "2026-01-02T00:00:00Z",
            100,
            "D:/tripA",
            "b.jpg",
        );
        insert_full_image(
            &conn,
            library_id,
            92,
            "jpeg",
            "2026-01-03T00:00:00Z",
            100,
            "D:/tripB",
            "c.jpg",
        );

        let sources = super::list_sources(&conn).unwrap();

        assert_eq!(sources.len(), 2);
        let trip_a = sources
            .iter()
            .find(|s| s.source_root == "D:/tripA")
            .unwrap();
        assert_eq!(trip_a.count, 2);
    }

    #[test]
    fn record_thumbnail_upserts_on_image_and_variant() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);

        super::record_thumbnail(&conn, image_id, "grid256", "jpeg", "/a/b.jpg").unwrap();
        super::record_thumbnail(&conn, image_id, "grid256", "jpeg", "/a/c.jpg").unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM thumbnails WHERE image_id = ?1",
                [image_id],
                |r| r.get(0),
            )
            .unwrap();
        let path: String = conn
            .query_row(
                "SELECT path FROM thumbnails WHERE image_id = ?1",
                [image_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "re-recording the same variant must not duplicate the row"
        );
        assert_eq!(path, "/a/c.jpg");
    }

    #[test]
    fn list_review_queue_returns_only_pending_entries_with_both_sides() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let a = insert_full_image(
            &conn,
            library_id,
            100,
            "jpeg",
            "2026-01-01T00:00:00Z",
            100,
            "D:/src",
            "a.jpg",
        );
        let b = insert_full_image(
            &conn,
            library_id,
            101,
            "jpeg",
            "2026-01-01T00:00:01Z",
            100,
            "D:/src",
            "b.jpg",
        );
        conn.execute(
            "INSERT INTO dedupe_review_queue (image_a_id, image_b_id, hamming_distance) VALUES (?1, ?2, 3)",
            params![a, b],
        )
        .unwrap();
        // A resolved entry must not show up.
        let c = insert_full_image(
            &conn,
            library_id,
            102,
            "jpeg",
            "2026-01-01T00:00:02Z",
            100,
            "D:/src",
            "c.jpg",
        );
        let d = insert_full_image(
            &conn,
            library_id,
            103,
            "jpeg",
            "2026-01-01T00:00:03Z",
            100,
            "D:/src",
            "d.jpg",
        );
        conn.execute(
            "INSERT INTO dedupe_review_queue (image_a_id, image_b_id, hamming_distance, status) VALUES (?1, ?2, 2, 'dismissed')",
            params![c, d],
        )
        .unwrap();

        let entries = super::list_review_queue(&conn).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hamming_distance, 3);
        assert_eq!(entries[0].image_a.id, a);
        assert_eq!(entries[0].image_b.id, b);
    }

    #[test]
    fn app_settings_read_the_schema_defaults() {
        let conn = migrated_conn();

        let settings = super::get_app_settings(&conn).unwrap();

        assert_eq!(
            settings,
            super::AppSettings {
                hamming_threshold: 5,
                retention_days: 30,
                tag_storage_threshold: 0.1,
                tag_display_threshold: 0.5,
            }
        );
    }

    #[test]
    fn app_settings_round_trip_through_update() {
        let conn = migrated_conn();

        super::update_app_settings(
            &conn,
            super::AppSettings {
                hamming_threshold: 8,
                retention_days: 14,
                tag_storage_threshold: 0.2,
                tag_display_threshold: 0.6,
            },
        )
        .unwrap();
        let settings = super::get_app_settings(&conn).unwrap();

        assert_eq!(
            settings,
            super::AppSettings {
                hamming_threshold: 8,
                retention_days: 14,
                tag_storage_threshold: 0.2,
                tag_display_threshold: 0.6,
            }
        );
    }

    #[test]
    fn tag_thresholds_preserve_the_required_storage_le_display_ordering() {
        let conn = migrated_conn();
        let settings = super::get_app_settings(&conn).unwrap();
        assert!(
            settings.tag_storage_threshold <= settings.tag_display_threshold,
            "storage floor must never exceed the display floor"
        );
    }
}
