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

/// One `image_tags` row's provenance — `source`, `confidence` (only
/// meaningful when `source == "auto"`), `review_state` (`None` for manual
/// tags). Milestone ML-4's review UI needs this; [`tags_for_image`] stays
/// bare names for the callers that only ever wanted that (the grid, XMP
/// sidecar sync) — not replaced, so their sort/shape guarantees don't
/// change under them.
#[derive(Debug, Clone, PartialEq)]
pub struct TagWithProvenance {
    pub name: String,
    pub source: String,
    pub confidence: Option<f64>,
    pub review_state: Option<String>,
}

/// Same ordering guarantee as [`tags_for_image`] (alphabetical by name),
/// with each row's full provenance attached.
pub fn tags_with_provenance_for_image(conn: &Connection, image_id: i64) -> rusqlite::Result<Vec<TagWithProvenance>> {
    let mut stmt = conn.prepare(
        "SELECT t.name, it.source, it.confidence, it.review_state
         FROM tags t
         JOIN image_tags it ON it.tag_id = t.id
         WHERE it.image_id = ?1
         ORDER BY t.name ASC",
    )?;
    stmt.query_map([image_id], |row| {
        Ok(TagWithProvenance { name: row.get(0)?, source: row.get(1)?, confidence: row.get(2)?, review_state: row.get(3)? })
    })?
    .collect()
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

/// The `source` (`"manual"`/`"auto"`) of `tag_name` on `image_id`, if
/// that tag is currently applied there — `None` if the image doesn't
/// carry this tag at all. Lets a caller route between [`remove_tag`] and
/// [`reject_tag`] per image, since the same tag name can be manual on one
/// image and auto-sourced on another (Milestone ML-4's bulk correction —
/// §5 — needs exactly this per-image branch; a single-image drawer
/// already has the tag's provenance in hand and doesn't need to ask).
pub fn tag_source_for_image(conn: &Connection, image_id: i64, tag_name: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT it.source FROM image_tags it
         JOIN tags t ON t.id = it.tag_id
         WHERE it.image_id = ?1 AND t.name = ?2",
        params![image_id, tag_name],
        |row| row.get(0),
    )
    .optional()
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

// ── ML: faces (ML-SPEC.md §6/§11, Milestone ML-3) ─────────────────────────
//
// Embedding *content* stays opaque bytes at this layer throughout, matching
// upsert_embedding's own boundary — decoding/comparing/centroid math is
// crates/ml's job, this crate only stores rows and reports associations.

pub struct NewFaceDetection {
    pub image_id: i64,
    pub model_id: i64,
    pub detection_confidence: f64,
    pub bbox_x: i64,
    pub bbox_y: i64,
    pub bbox_width: i64,
    pub bbox_height: i64,
    pub embedding: Vec<u8>,
    pub dimension: i64,
}

/// Records where a face detection's pre-cropped display thumbnail lives on
/// disk (ticket 028 decision #4: crops are stored, not regenerated on every
/// render, unlike `get_full_preview`'s deliberately-uncached full images —
/// the split/merge grid and People view both need many small crops at once,
/// which is the case that caching exists for). Called once the crop file
/// has actually been written, since the path is keyed by this row's own id.
pub fn set_face_crop_thumbnail_path(conn: &Connection, detection_id: i64, path: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE face_detections SET crop_thumbnail_path = ?1 WHERE id = ?2",
        params![path, detection_id],
    )?;
    Ok(())
}

/// Inserts a new, not-yet-clustered face detection. Returns its id.
pub fn insert_face_detection(conn: &Connection, detection: &NewFaceDetection) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO face_detections (
            image_id, model_id, detection_confidence,
            bbox_x, bbox_y, bbox_width, bbox_height, embedding, dimension
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            detection.image_id,
            detection.model_id,
            detection.detection_confidence,
            detection.bbox_x,
            detection.bbox_y,
            detection.bbox_width,
            detection.bbox_height,
            detection.embedding,
            detection.dimension
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub struct ClusterMember {
    pub face_detection_id: i64,
    pub cluster_id: i64,
    pub person_id: Option<i64>,
    pub embedding: Vec<u8>,
}

/// Every already-clustered, non-hidden face detection's embedding for
/// `model_id` — the comparison set for matching a new face against
/// existing clusters/persons (§6's three-tier split).
pub fn clustered_face_embeddings(conn: &Connection, model_id: i64) -> rusqlite::Result<Vec<ClusterMember>> {
    let mut stmt = conn.prepare(
        "SELECT fd.id, fd.face_cluster_id, fc.person_id, fd.embedding
         FROM face_detections fd
         JOIN face_clusters fc ON fc.id = fd.face_cluster_id
         WHERE fd.model_id = ?1 AND fc.hidden = 0",
    )?;
    stmt.query_map([model_id], |row| {
        Ok(ClusterMember {
            face_detection_id: row.get(0)?,
            cluster_id: row.get(1)?,
            person_id: row.get(2)?,
            embedding: row.get(3)?,
        })
    })?
    .collect()
}

/// Creates a new, unnamed cluster and attaches `face_detection_id` to it
/// as its first member — "grouping raw detections into unnamed,
/// provisional clusters runs silently" (§6). Returns the new cluster's id.
pub fn create_cluster_with_member(conn: &Connection, face_detection_id: i64) -> rusqlite::Result<i64> {
    conn.execute("INSERT INTO face_clusters DEFAULT VALUES", [])?;
    let cluster_id = conn.last_insert_rowid();
    attach_face_detection_to_cluster(conn, face_detection_id, cluster_id)?;
    Ok(cluster_id)
}

/// Attaches an already-inserted face detection to an existing cluster
/// (auto-attribute or ordinary clustering — §6) and bumps the cluster's
/// `updated_at`.
pub fn attach_face_detection_to_cluster(conn: &Connection, face_detection_id: i64, cluster_id: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE face_detections SET face_cluster_id = ?1 WHERE id = ?2", params![cluster_id, face_detection_id])?;
    conn.execute(
        "UPDATE face_clusters SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
        params![cluster_id],
    )?;
    Ok(())
}

/// Queues a medium-confidence match against an already-named person for
/// human review (§6's three-tier split — mirrors `dedupe_review_queue`'s
/// shape). Returns the new queue row's id.
pub fn queue_face_match_review(conn: &Connection, face_detection_id: i64, suggested_person_id: i64, similarity_score: f64) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO face_match_review_queue (face_detection_id, suggested_person_id, similarity_score)
         VALUES (?1, ?2, ?3)",
        params![face_detection_id, suggested_person_id, similarity_score],
    )?;
    Ok(conn.last_insert_rowid())
}

// ── ML: People view (ML-SPEC.md §6, ticket 028, Milestone ML-4 Slice C) ────

pub struct Person {
    pub id: i64,
    pub name: String,
}

/// Every named person, alphabetical — the naming autocomplete's source
/// list (028 decision #3: naming the same person from two different
/// clusters must attach to one identity, not silently create duplicates).
pub fn list_persons(conn: &Connection) -> rusqlite::Result<Vec<Person>> {
    let mut stmt = conn.prepare("SELECT id, name FROM persons ORDER BY name COLLATE NOCASE")?;
    stmt.query_map([], |row| Ok(Person { id: row.get(0)?, name: row.get(1)? }))?.collect()
}

pub struct FaceClusterSummary {
    pub id: i64,
    pub person_id: Option<i64>,
    pub person_name: Option<String>,
    pub photo_count: i64,
    pub representative_crop_path: Option<String>,
    pub hidden: bool,
}

/// Every face cluster (named or not), sorted by photo count descending —
/// 028 decision #3's answer to clutter: "real/frequent people surface
/// first, one-off/junk clusters sink to the bottom" instead of pruning
/// anything. `include_hidden` is false for the normal People view and true
/// only for an eventual "manage hidden clusters" surface (not built in
/// Slice C). `photo_count` counts distinct images, not detections, since
/// the same person can appear twice in one photo.
pub fn list_face_clusters(conn: &Connection, include_hidden: bool) -> rusqlite::Result<Vec<FaceClusterSummary>> {
    let mut stmt = conn.prepare(
        "SELECT fc.id, fc.person_id, p.name, COUNT(DISTINCT fd.image_id) AS photo_count,
                (SELECT crop_thumbnail_path FROM face_detections
                 WHERE face_cluster_id = fc.id AND crop_thumbnail_path IS NOT NULL
                 ORDER BY detection_confidence DESC LIMIT 1) AS representative_crop_path,
                fc.hidden
         FROM face_clusters fc
         JOIN face_detections fd ON fd.face_cluster_id = fc.id
         LEFT JOIN persons p ON p.id = fc.person_id
         WHERE fc.hidden = 0 OR ?1
         GROUP BY fc.id
         ORDER BY photo_count DESC, fc.id ASC",
    )?;
    stmt.query_map([include_hidden], |row| {
        Ok(FaceClusterSummary {
            id: row.get(0)?,
            person_id: row.get(1)?,
            person_name: row.get(2)?,
            photo_count: row.get(3)?,
            representative_crop_path: row.get(4)?,
            hidden: row.get::<_, i64>(5)? != 0,
        })
    })?
    .collect()
}

/// Finds a person by case-insensitive name (matching `list_persons`'s own
/// `COLLATE NOCASE` ordering — typing "alice" without picking the
/// autocomplete suggestion for "Alice" must still resolve to one identity,
/// not silently create a second person), creating one otherwise. Shared by
/// [`name_cluster`] and [`merge_clusters`] — both need "resolve a typed
/// name to a person id" as one step in a larger operation.
fn find_or_create_person(conn: &Connection, person_name: &str) -> rusqlite::Result<i64> {
    let name = person_name.trim();
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM persons WHERE name = ?1 COLLATE NOCASE", params![name], |row| row.get(0))
        .optional()?;
    match existing {
        Some(id) => Ok(id),
        None => {
            conn.execute("INSERT INTO persons (name) VALUES (?1)", params![name])?;
            Ok(conn.last_insert_rowid())
        }
    }
}

/// Names a cluster — "the confirmation gate" (028 decision #2) that turns
/// "some grouped faces" into an actual person; naming the same person from
/// two different clusters attaches to one identity (028 decision #3).
/// Returns the person's id.
pub fn name_cluster(conn: &Connection, cluster_id: i64, person_name: &str) -> rusqlite::Result<i64> {
    let person_id = find_or_create_person(conn, person_name)?;
    conn.execute(
        "UPDATE face_clusters SET person_id = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
        params![person_id, cluster_id],
    )?;
    Ok(person_id)
}

/// Toggles a cluster's reversible "Hide" state (028 decision #3: "moves to
/// a hidden bucket, never deletes underlying detections/embeddings" —
/// mirrors quarantine's recoverable-not-gone shape, but with no retention
/// window, since hidden clusters carry no disk-space pressure to reclaim).
pub fn set_cluster_hidden(conn: &Connection, cluster_id: i64, hidden: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE face_clusters SET hidden = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
        params![hidden, cluster_id],
    )?;
    Ok(())
}

/// Merges `loser_cluster_id` into `keeper_cluster_id` (028 decision #4:
/// "confirm produces the union of both clusters' member faces — exactly
/// like dedupe's tag-union-on-merge") — reassigns every one of the
/// loser's face detections to the keeper, then deletes the (now-empty)
/// loser row. `resulting_person_name`, when given, is resolved via
/// [`find_or_create_person`] and set on the keeper — the caller (the
/// People view's merge confirmation UI) is what decides this name, since
/// only it knows whether the two clusters' existing names conflicted and,
/// if so, which one the human picked (028 decision #4: "present both,
/// human picks one — never silently choose"); `None` leaves the keeper's
/// current name/unnamed state untouched, for the no-conflict cases where
/// only one side was ever named (or neither was) and there's nothing to
/// resolve. `keeper_cluster_id` and `loser_cluster_id` must be different
/// clusters — which one is "keeper" carries no meaning of its own beyond
/// which row survives (the merged result's representative crop is picked
/// fresh from all member detections either way).
pub fn merge_clusters(conn: &Connection, keeper_cluster_id: i64, loser_cluster_id: i64, resulting_person_name: Option<&str>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE face_detections SET face_cluster_id = ?1 WHERE face_cluster_id = ?2",
        params![keeper_cluster_id, loser_cluster_id],
    )?;
    match resulting_person_name {
        Some(name) => {
            let person_id = find_or_create_person(conn, name)?;
            conn.execute(
                "UPDATE face_clusters SET person_id = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
                params![person_id, keeper_cluster_id],
            )?;
        }
        None => {
            conn.execute(
                "UPDATE face_clusters SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
                [keeper_cluster_id],
            )?;
        }
    }
    conn.execute("DELETE FROM face_clusters WHERE id = ?1", [loser_cluster_id])?;
    Ok(())
}

/// Moves `detection_ids` out of whatever cluster(s) they're currently in
/// and into one brand-new cluster (028 decision #4's Split: "show every
/// member face thumbnail... let the user multi-select a subset, move them
/// to a new or existing cluster/person"). `person_name`, when given, is
/// resolved via [`find_or_create_person`] and set on the new cluster — the
/// same "always a *fresh* cluster for a specific person, never guess which
/// existing one" reasoning [`confirm_face_match`] already uses, so moving
/// a subset "to an existing person" and moving it "to a new group" are the
/// same operation here, just with or without a name. Any source cluster
/// left with zero detections after the move is deleted (mirrors
/// [`merge_clusters`]'s loser cleanup) — every other source cluster is
/// left alone; its `photo_count` etc. simply reflects fewer members next
/// time it's queried, nothing there needs updating directly. Requires
/// `detection_ids` non-empty (the caller — the People view's split
/// selection — never enables the move action with nothing selected).
/// Returns the new cluster's id.
pub fn move_detections_to_new_cluster(conn: &Connection, detection_ids: &[i64], person_name: Option<&str>) -> rusqlite::Result<i64> {
    let placeholders = vec!["?"; detection_ids.len()].join(",");

    let source_cluster_ids: Vec<i64> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT face_cluster_id FROM face_detections WHERE id IN ({placeholders}) AND face_cluster_id IS NOT NULL"
        ))?;
        stmt.query_map(rusqlite::params_from_iter(detection_ids), |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
    };

    conn.execute("INSERT INTO face_clusters DEFAULT VALUES", [])?;
    let new_cluster_id = conn.last_insert_rowid();

    let mut update_params: Vec<i64> = Vec::with_capacity(detection_ids.len() + 1);
    update_params.push(new_cluster_id);
    update_params.extend_from_slice(detection_ids);
    conn.execute(
        &format!("UPDATE face_detections SET face_cluster_id = ? WHERE id IN ({placeholders})"),
        rusqlite::params_from_iter(update_params.iter()),
    )?;

    if let Some(name) = person_name {
        let person_id = find_or_create_person(conn, name)?;
        conn.execute(
            "UPDATE face_clusters SET person_id = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
            params![person_id, new_cluster_id],
        )?;
    }

    for source_cluster_id in source_cluster_ids {
        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM face_detections WHERE face_cluster_id = ?1",
            [source_cluster_id],
            |row| row.get(0),
        )?;
        if remaining == 0 {
            conn.execute("DELETE FROM face_clusters WHERE id = ?1", [source_cluster_id])?;
        }
    }

    Ok(new_cluster_id)
}

pub struct NamedFaceChip {
    pub cluster_id: i64,
    pub person_id: i64,
    pub person_name: String,
}

/// An unnamed cluster's detection count on one image — split out from a
/// bare total so the drawer can offer inline naming (028 decision #3) in
/// the common single-stranger case (exactly one unnamed cluster, nothing
/// left over in `unclustered_count`) while falling back to the People view
/// when the target is genuinely ambiguous (2+ distinct unnamed clusters in
/// the same photo) — there's no per-face-crop picker in the drawer to
/// disambiguate further than that (028 decision #5 rules out a bounding-box
/// overlay), so ambiguity past that point is Slice D's job, not guessed at
/// here.
pub struct UnnamedFaceGroup {
    pub cluster_id: i64,
    pub count: i64,
}

pub struct ImagePeople {
    pub named: Vec<NamedFaceChip>,
    pub unnamed_clustered: Vec<UnnamedFaceGroup>,
    pub unclustered_count: i64,
}

/// The drawer's "People in this photo" chip list source (028 decision #5):
/// named faces become individual clickable chips; unnamed/unclustered
/// detections collapse into one count for a "+N unidentified" chip.
/// Detections belonging to a hidden cluster are excluded from all three —
/// a hidden cluster shouldn't resurface anonymously either.
pub fn people_for_image(conn: &Connection, image_id: i64) -> rusqlite::Result<ImagePeople> {
    let mut named_stmt = conn.prepare(
        "SELECT DISTINCT fc.id, p.id, p.name
         FROM face_detections fd
         JOIN face_clusters fc ON fc.id = fd.face_cluster_id
         JOIN persons p ON p.id = fc.person_id
         WHERE fd.image_id = ?1 AND fc.hidden = 0
         ORDER BY p.name COLLATE NOCASE",
    )?;
    let named = named_stmt
        .query_map([image_id], |row| {
            Ok(NamedFaceChip { cluster_id: row.get(0)?, person_id: row.get(1)?, person_name: row.get(2)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut unnamed_stmt = conn.prepare(
        "SELECT fc.id, COUNT(*)
         FROM face_detections fd
         JOIN face_clusters fc ON fc.id = fd.face_cluster_id
         WHERE fd.image_id = ?1 AND fc.person_id IS NULL AND fc.hidden = 0
         GROUP BY fc.id",
    )?;
    let unnamed_clustered = unnamed_stmt
        .query_map([image_id], |row| Ok(UnnamedFaceGroup { cluster_id: row.get(0)?, count: row.get(1)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let unclustered_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM face_detections WHERE image_id = ?1 AND face_cluster_id IS NULL",
        [image_id],
        |row| row.get(0),
    )?;

    Ok(ImagePeople { named, unnamed_clustered, unclustered_count })
}

pub struct FaceCrop {
    pub detection_id: i64,
    pub crop_thumbnail_path: String,
}

/// A cluster's individual member face crops — 028 decision #3's "click a
/// cluster, see its member thumbnails + photo count", and (Slice D3) the
/// same crops made *selectable* for Split ("show every member face
/// thumbnail... let the user multi-select a subset"). Carries each crop's
/// `detection_id` (not just the display path) so a selection can be
/// traced back to a real `face_detections` row and moved. Detections with
/// no crop yet (`crop_thumbnail_path` still `NULL`) are skipped rather
/// than rendered as broken images — meaning such a detection also can't
/// be selected for Split from this view, a real (small) gap flagged
/// rather than worked around, since nothing else in the UI offers a
/// crop-less selection target either.
pub fn face_crops_for_cluster(conn: &Connection, cluster_id: i64) -> rusqlite::Result<Vec<FaceCrop>> {
    let mut stmt = conn.prepare(
        "SELECT id, crop_thumbnail_path FROM face_detections
         WHERE face_cluster_id = ?1 AND crop_thumbnail_path IS NOT NULL
         ORDER BY id",
    )?;
    stmt.query_map([cluster_id], |row| Ok(FaceCrop { detection_id: row.get(0)?, crop_thumbnail_path: row.get(1)? }))?
        .collect()
}

/// Pending `face_match_review_queue` count — the People nav badge (028
/// decision #1: "mirroring the existing dedupe review queue's pattern
/// exactly"). [`list_pending_face_matches`] (Milestone ML-4 Slice D1) is
/// what the badge now leads to.
pub fn pending_face_review_count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM face_match_review_queue WHERE status = 'pending'", [], |row| row.get(0))
}

pub struct PendingFaceMatch {
    pub queue_id: i64,
    pub face_detection_id: i64,
    pub image_id: i64,
    pub crop_thumbnail_path: Option<String>,
    pub suggested_person_id: i64,
    pub suggested_person_name: String,
    pub similarity_score: f64,
}

/// Every pending §6-tier-2 match — the medium-confidence "is this also
/// Alice?" human-review queue (028 decision #2), oldest first (FIFO,
/// matching dedupe's own `list_review_queue` ordering).
pub fn list_pending_face_matches(conn: &Connection) -> rusqlite::Result<Vec<PendingFaceMatch>> {
    let mut stmt = conn.prepare(
        "SELECT q.id, fd.id, fd.image_id, fd.crop_thumbnail_path, p.id, p.name, q.similarity_score
         FROM face_match_review_queue q
         JOIN face_detections fd ON fd.id = q.face_detection_id
         JOIN persons p ON p.id = q.suggested_person_id
         WHERE q.status = 'pending'
         ORDER BY q.created_at",
    )?;
    stmt.query_map([], |row| {
        Ok(PendingFaceMatch {
            queue_id: row.get(0)?,
            face_detection_id: row.get(1)?,
            image_id: row.get(2)?,
            crop_thumbnail_path: row.get(3)?,
            suggested_person_id: row.get(4)?,
            suggested_person_name: row.get(5)?,
            similarity_score: row.get(6)?,
        })
    })?
    .collect()
}

/// Confirms a pending match ("yes, this is also {suggested person}") —
/// creates a **new** cluster for the detection with `person_id` set
/// directly, rather than attaching into one of that person's existing
/// clusters: `face_match_review_queue` only ever recorded a
/// `suggested_person_id`, never which specific cluster the medium-
/// confidence comparison matched against (`crate::faces::FaceMatchDecision::ReviewQueue`
/// carries no cluster id), so there's no data-honest existing cluster to
/// pick. A person ending up with 2+ clusters this way is expected, not a
/// bug — Slice D2's cluster Merge is what consolidates them. Returns the
/// new cluster's id.
pub fn confirm_face_match(conn: &Connection, queue_id: i64) -> rusqlite::Result<i64> {
    let (face_detection_id, suggested_person_id): (i64, i64) = conn.query_row(
        "SELECT face_detection_id, suggested_person_id FROM face_match_review_queue WHERE id = ?1",
        [queue_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let cluster_id = create_cluster_with_member(conn, face_detection_id)?;
    conn.execute(
        "UPDATE face_clusters SET person_id = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
        params![suggested_person_id, cluster_id],
    )?;
    mark_face_match_resolved(conn, queue_id, "confirmed")?;
    Ok(cluster_id)
}

/// Dismisses a pending match ("no, not the same person") — falls back to
/// an ordinary new unnamed cluster for the detection (§6's "no plausible
/// match" tier), the same outcome any face with nothing to compare against
/// gets. **Deliberately not** a re-run of real embedding comparison
/// against the remaining unnamed clusters: that needs `crate::faces::match_face`,
/// which lives in `lenslocker-ml` — a dependency `src-tauri` doesn't have
/// and Milestone ML-6 (the real background-pipeline wiring) is what's
/// meant to add, not this slice. Returns the new cluster's id.
pub fn dismiss_face_match(conn: &Connection, queue_id: i64) -> rusqlite::Result<i64> {
    let face_detection_id: i64 = conn.query_row(
        "SELECT face_detection_id FROM face_match_review_queue WHERE id = ?1",
        [queue_id],
        |row| row.get(0),
    )?;
    let cluster_id = create_cluster_with_member(conn, face_detection_id)?;
    mark_face_match_resolved(conn, queue_id, "dismissed")?;
    Ok(cluster_id)
}

/// Shared by [`confirm_face_match`]/[`dismiss_face_match`] — both end by
/// marking the queue row resolved, differing only in the terminal status.
fn mark_face_match_resolved(conn: &Connection, queue_id: i64, status: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE face_match_review_queue SET status = ?1, resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?2",
        params![status, queue_id],
    )?;
    Ok(())
}

// ── Saved albums / smart searches (ML-SPEC.md §7, ticket 031, Milestone
// ML-5) ─────────────────────────────────────────────────────────────────
//
// "Always live — re-evaluated on open, never a static snapshot" (ticket
// 031 decision #1): a saved album is nothing but a name + a serialized
// filter/sort/search blob (`saved_albums.filters` — same JSON shape as
// the frontend's own `filtersDto()` plus `sort`/`search`, per the
// schema's own comment), re-run through the ordinary `list_images` query
// every time it's opened. There is deliberately no image-membership table
// (decision #4) — this crate never even parses `filters`'s contents; it's
// an opaque blob to `catalog`, assembled and consumed entirely by the
// frontend.

pub struct SavedAlbum {
    pub id: i64,
    pub name: String,
    pub filters: String,
    pub created_at: String,
}

/// Every saved album, alphabetical (matching [`list_persons`]'s own
/// findability-first ordering for a small, user-curated list).
pub fn list_saved_albums(conn: &Connection) -> rusqlite::Result<Vec<SavedAlbum>> {
    let mut stmt = conn.prepare("SELECT id, name, filters, created_at FROM saved_albums ORDER BY name COLLATE NOCASE")?;
    stmt.query_map([], |row| {
        Ok(SavedAlbum { id: row.get(0)?, name: row.get(1)?, filters: row.get(2)?, created_at: row.get(3)? })
    })?
    .collect()
}

/// Saves the current filter/sort/search state under `name` — always a new
/// row (ticket 031's resolution never asks for update/rename-in-place
/// semantics, and the schema itself has no unique constraint on `name`
/// stopping one); duplicate names are the user's own choice to manage via
/// delete, not something this layer prevents. Returns the new row's id.
pub fn save_album(conn: &Connection, name: &str, filters_json: &str) -> rusqlite::Result<i64> {
    conn.execute("INSERT INTO saved_albums (name, filters) VALUES (?1, ?2)", params![name.trim(), filters_json])?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_saved_album(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM saved_albums WHERE id = ?1", [id])?;
    Ok(())
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
    /// `persons.id` values (ticket 031's Person facet, Milestone ML-5) —
    /// OR'd together like `tags`. Matches an image if any of its face
    /// detections belong to a non-hidden cluster attributed to one of
    /// these people — hidden clusters are excluded by default, mirroring
    /// the People view's own `list_face_clusters(include_hidden: false)`
    /// default, since a cluster the user hid shouldn't resurface photos
    /// via a filter either.
    pub persons: Vec<i64>,
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
    if !filters.persons.is_empty() {
        let placeholders = vec!["?"; filters.persons.len()].join(",");
        where_clauses.push(format!(
            "im.id IN (SELECT fd.image_id FROM face_detections fd \
             JOIN face_clusters fc ON fc.id = fd.face_cluster_id \
             WHERE fc.hidden = 0 AND fc.person_id IN ({placeholders}))"
        ));
        for p in &filters.persons {
            args.push(Box::new(*p));
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
    pub tags: Vec<TagWithProvenance>,
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
    let tags = tags_with_provenance_for_image(conn, image_id)?;

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
    fn tag_source_for_image_reports_the_right_source_per_image() {
        let conn = migrated_conn();
        let manual_image = insert_test_image(&conn);
        let auto_image = insert_test_image(&conn);
        let untagged_image = insert_test_image(&conn);

        super::add_tag(&conn, manual_image, "beach").unwrap();
        super::apply_auto_tag(&conn, auto_image, "beach", 0.9).unwrap();

        assert_eq!(super::tag_source_for_image(&conn, manual_image, "beach").unwrap(), Some("manual".to_string()));
        assert_eq!(super::tag_source_for_image(&conn, auto_image, "beach").unwrap(), Some("auto".to_string()));
        assert_eq!(super::tag_source_for_image(&conn, untagged_image, "beach").unwrap(), None);
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

    fn new_face(image_id: i64, model_id: i64, embedding: &[u8]) -> super::NewFaceDetection {
        super::NewFaceDetection {
            image_id,
            model_id,
            detection_confidence: 0.95,
            bbox_x: 10,
            bbox_y: 10,
            bbox_width: 50,
            bbox_height: 50,
            embedding: embedding.to_vec(),
            dimension: 1,
        }
    }

    #[test]
    fn create_cluster_with_member_makes_an_unnamed_cluster() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();

        let cluster_id = super::create_cluster_with_member(&conn, detection_id).unwrap();

        let person_id: Option<i64> = conn.query_row("SELECT person_id FROM face_clusters WHERE id = ?1", [cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(person_id, None, "a freshly created cluster must be unnamed");

        let stored_cluster_id: Option<i64> =
            conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
        assert_eq!(stored_cluster_id, Some(cluster_id));
    }

    #[test]
    fn clustered_face_embeddings_excludes_unclustered_and_hidden() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();

        let clustered = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        super::create_cluster_with_member(&conn, clustered).unwrap();

        let unclustered = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let _ = unclustered; // deliberately never attached to a cluster

        let in_hidden = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[3])).unwrap();
        let hidden_cluster_id = super::create_cluster_with_member(&conn, in_hidden).unwrap();
        conn.execute("UPDATE face_clusters SET hidden = 1 WHERE id = ?1", [hidden_cluster_id]).unwrap();

        let members = super::clustered_face_embeddings(&conn, model_id).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].face_detection_id, clustered);
    }

    #[test]
    fn attach_face_detection_to_cluster_moves_it_and_touches_updated_at() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let first = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let cluster_a = super::create_cluster_with_member(&conn, first).unwrap();
        conn.execute("INSERT INTO face_clusters DEFAULT VALUES", []).unwrap();
        let cluster_b = conn.last_insert_rowid();

        super::attach_face_detection_to_cluster(&conn, first, cluster_b).unwrap();

        let stored_cluster_id: i64 =
            conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [first], |row| row.get(0)).unwrap();
        assert_eq!(stored_cluster_id, cluster_b);
        assert_ne!(cluster_a, cluster_b);
    }

    #[test]
    fn queue_face_match_review_inserts_a_pending_row() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('Alex')", []).unwrap();
        let person_id = conn.last_insert_rowid();

        let queue_id = super::queue_face_match_review(&conn, detection_id, person_id, 0.42).unwrap();

        let (status, similarity): (String, f64) =
            conn.query_row("SELECT status, similarity_score FROM face_match_review_queue WHERE id = ?1", [queue_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(status, "pending");
        assert_eq!(similarity, 0.42);
    }

    #[test]
    fn name_cluster_creates_a_new_person_and_assigns_it() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let cluster_id = super::create_cluster_with_member(&conn, detection_id).unwrap();

        let person_id = super::name_cluster(&conn, cluster_id, "  Alice  ").unwrap();

        let name: String = conn.query_row("SELECT name FROM persons WHERE id = ?1", [person_id], |row| row.get(0)).unwrap();
        assert_eq!(name, "Alice", "name must be trimmed before storing");
        let stored_person_id: Option<i64> = conn.query_row("SELECT person_id FROM face_clusters WHERE id = ?1", [cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(stored_person_id, Some(person_id));
    }

    #[test]
    fn name_cluster_reuses_an_existing_person_by_exact_name() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let first_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let cluster_a = super::create_cluster_with_member(&conn, first_detection).unwrap();
        let second_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let cluster_b = super::create_cluster_with_member(&conn, second_detection).unwrap();

        let person_a = super::name_cluster(&conn, cluster_a, "Alice").unwrap();
        let person_b = super::name_cluster(&conn, cluster_b, "Alice").unwrap();

        assert_eq!(person_a, person_b, "naming two different clusters the same name must attach to one identity, not create duplicates");
        let person_count: i64 = conn.query_row("SELECT COUNT(*) FROM persons", [], |row| row.get(0)).unwrap();
        assert_eq!(person_count, 1);
    }

    #[test]
    fn name_cluster_matches_an_existing_person_case_insensitively() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let first_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let cluster_a = super::create_cluster_with_member(&conn, first_detection).unwrap();
        let second_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let cluster_b = super::create_cluster_with_member(&conn, second_detection).unwrap();

        let person_a = super::name_cluster(&conn, cluster_a, "Alice").unwrap();
        let person_b = super::name_cluster(&conn, cluster_b, "alice").unwrap();

        assert_eq!(person_a, person_b, "typing a different case than the autocomplete suggestion must still attach to the same identity");
        let person_count: i64 = conn.query_row("SELECT COUNT(*) FROM persons", [], |row| row.get(0)).unwrap();
        assert_eq!(person_count, 1);
    }

    #[test]
    fn set_cluster_hidden_toggles_and_is_reversible() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let cluster_id = super::create_cluster_with_member(&conn, detection_id).unwrap();

        super::set_cluster_hidden(&conn, cluster_id, true).unwrap();
        let hidden: i64 = conn.query_row("SELECT hidden FROM face_clusters WHERE id = ?1", [cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(hidden, 1);

        super::set_cluster_hidden(&conn, cluster_id, false).unwrap();
        let hidden: i64 = conn.query_row("SELECT hidden FROM face_clusters WHERE id = ?1", [cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(hidden, 0, "hiding must be reversible — 028 decision #3");
    }

    #[test]
    fn merge_clusters_reassigns_the_losers_detections_and_deletes_its_row() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();

        let keeper_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let keeper_id = super::create_cluster_with_member(&conn, keeper_detection).unwrap();
        let loser_detection_a = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let loser_id = super::create_cluster_with_member(&conn, loser_detection_a).unwrap();
        let loser_detection_b = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[3])).unwrap();
        super::attach_face_detection_to_cluster(&conn, loser_detection_b, loser_id).unwrap();

        super::merge_clusters(&conn, keeper_id, loser_id, None).unwrap();

        for detection_id in [keeper_detection, loser_detection_a, loser_detection_b] {
            let stored_cluster_id: Option<i64> =
                conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
            assert_eq!(stored_cluster_id, Some(keeper_id), "every detection must end up on the keeper cluster");
        }
        let loser_still_exists: i64 = conn.query_row("SELECT COUNT(*) FROM face_clusters WHERE id = ?1", [loser_id], |row| row.get(0)).unwrap();
        assert_eq!(loser_still_exists, 0, "the loser cluster row must be deleted, not left empty");
    }

    #[test]
    fn merge_clusters_with_no_resulting_name_leaves_the_keepers_existing_name_untouched() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let keeper_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let keeper_id = super::create_cluster_with_member(&conn, keeper_detection).unwrap();
        let alice_id = super::name_cluster(&conn, keeper_id, "Alice").unwrap();
        let loser_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let loser_id = super::create_cluster_with_member(&conn, loser_detection).unwrap();

        super::merge_clusters(&conn, keeper_id, loser_id, None).unwrap();

        let stored_person_id: Option<i64> = conn.query_row("SELECT person_id FROM face_clusters WHERE id = ?1", [keeper_id], |row| row.get(0)).unwrap();
        assert_eq!(stored_person_id, Some(alice_id), "an unnamed loser merged with no explicit resulting name must not un-name the keeper");
    }

    #[test]
    fn merge_clusters_with_a_resulting_name_resolves_it_via_find_or_create_person() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let keeper_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let keeper_id = super::create_cluster_with_member(&conn, keeper_detection).unwrap();
        super::name_cluster(&conn, keeper_id, "Alice").unwrap();
        let loser_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let loser_id = super::create_cluster_with_member(&conn, loser_detection).unwrap();
        let bob_id = super::name_cluster(&conn, loser_id, "Bob").unwrap();

        // Human resolved the Alice/Bob name conflict by picking "Bob".
        super::merge_clusters(&conn, keeper_id, loser_id, Some("Bob")).unwrap();

        let stored_person_id: Option<i64> = conn.query_row("SELECT person_id FROM face_clusters WHERE id = ?1", [keeper_id], |row| row.get(0)).unwrap();
        assert_eq!(stored_person_id, Some(bob_id));
        let person_count: i64 = conn.query_row("SELECT COUNT(*) FROM persons", [], |row| row.get(0)).unwrap();
        assert_eq!(person_count, 2, "must reuse Bob's existing person row, not create a third");
    }

    #[test]
    fn merge_clusters_with_a_brand_new_typed_name_creates_that_person() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let keeper_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let keeper_id = super::create_cluster_with_member(&conn, keeper_detection).unwrap();
        super::name_cluster(&conn, keeper_id, "Alice").unwrap();
        let loser_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let loser_id = super::create_cluster_with_member(&conn, loser_detection).unwrap();
        super::name_cluster(&conn, loser_id, "Bob").unwrap();

        // Human resolved the conflict by typing a third name entirely.
        super::merge_clusters(&conn, keeper_id, loser_id, Some("Carol")).unwrap();

        let stored_person_name: Option<String> = conn
            .query_row(
                "SELECT p.name FROM face_clusters fc JOIN persons p ON p.id = fc.person_id WHERE fc.id = ?1",
                [keeper_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_person_name.as_deref(), Some("Carol"));
    }

    #[test]
    fn move_detections_to_new_cluster_reassigns_only_the_selected_detections() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let stays = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let source_cluster_id = super::create_cluster_with_member(&conn, stays).unwrap();
        let moves = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        super::attach_face_detection_to_cluster(&conn, moves, source_cluster_id).unwrap();

        let new_cluster_id = super::move_detections_to_new_cluster(&conn, &[moves], None).unwrap();

        let moved_cluster: Option<i64> = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [moves], |row| row.get(0)).unwrap();
        assert_eq!(moved_cluster, Some(new_cluster_id));
        let stayed_cluster: Option<i64> = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [stays], |row| row.get(0)).unwrap();
        assert_eq!(stayed_cluster, Some(source_cluster_id), "a detection not selected for the split must stay put");
    }

    #[test]
    fn move_detections_to_new_cluster_deletes_a_source_left_with_zero_detections() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let only_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let source_cluster_id = super::create_cluster_with_member(&conn, only_detection).unwrap();

        super::move_detections_to_new_cluster(&conn, &[only_detection], None).unwrap();

        let source_still_exists: i64 = conn.query_row("SELECT COUNT(*) FROM face_clusters WHERE id = ?1", [source_cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(source_still_exists, 0, "emptying the source cluster must delete it, mirroring merge_clusters's loser cleanup");
    }

    #[test]
    fn move_detections_to_new_cluster_leaves_a_non_empty_source_alone() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let stays = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let source_cluster_id = super::create_cluster_with_member(&conn, stays).unwrap();
        let moves = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        super::attach_face_detection_to_cluster(&conn, moves, source_cluster_id).unwrap();

        super::move_detections_to_new_cluster(&conn, &[moves], None).unwrap();

        let source_still_exists: i64 = conn.query_row("SELECT COUNT(*) FROM face_clusters WHERE id = ?1", [source_cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(source_still_exists, 1, "a source cluster with a remaining member must not be deleted");
    }

    #[test]
    fn move_detections_to_new_cluster_with_a_person_name_resolves_and_sets_it() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        super::create_cluster_with_member(&conn, detection_id).unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('Alice')", []).unwrap();
        let alice_id = conn.last_insert_rowid();

        let new_cluster_id = super::move_detections_to_new_cluster(&conn, &[detection_id], Some("Alice")).unwrap();

        let stored_person_id: Option<i64> = conn.query_row("SELECT person_id FROM face_clusters WHERE id = ?1", [new_cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(stored_person_id, Some(alice_id), "must reuse the existing Alice, not create a duplicate");
    }

    #[test]
    fn move_detections_to_new_cluster_can_move_several_detections_at_once() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let a = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let cluster_id = super::create_cluster_with_member(&conn, a).unwrap();
        let b = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        super::attach_face_detection_to_cluster(&conn, b, cluster_id).unwrap();
        let c = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[3])).unwrap();
        super::attach_face_detection_to_cluster(&conn, c, cluster_id).unwrap();

        let new_cluster_id = super::move_detections_to_new_cluster(&conn, &[a, b], None).unwrap();

        for detection_id in [a, b] {
            let stored_cluster: Option<i64> = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
            assert_eq!(stored_cluster, Some(new_cluster_id));
        }
        let c_cluster: Option<i64> = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [c], |row| row.get(0)).unwrap();
        assert_eq!(c_cluster, Some(cluster_id), "the un-selected detection stays on the original cluster, which survives since it's not empty");
    }

    #[test]
    fn list_face_clusters_sorts_by_photo_count_descending_and_excludes_hidden_by_default() {
        let conn = migrated_conn();
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();

        // Cluster A: two photos.
        let image_1 = insert_test_image(&conn);
        let image_2 = insert_test_image(&conn);
        let det_a1 = super::insert_face_detection(&conn, &new_face(image_1, model_id, &[1])).unwrap();
        let cluster_a = super::create_cluster_with_member(&conn, det_a1).unwrap();
        let det_a2 = super::insert_face_detection(&conn, &new_face(image_2, model_id, &[2])).unwrap();
        super::attach_face_detection_to_cluster(&conn, det_a2, cluster_a).unwrap();

        // Cluster B: one photo.
        let image_3 = insert_test_image(&conn);
        let det_b1 = super::insert_face_detection(&conn, &new_face(image_3, model_id, &[3])).unwrap();
        let cluster_b = super::create_cluster_with_member(&conn, det_b1).unwrap();

        // Cluster C: hidden, must not appear by default.
        let image_4 = insert_test_image(&conn);
        let det_c1 = super::insert_face_detection(&conn, &new_face(image_4, model_id, &[4])).unwrap();
        let cluster_c = super::create_cluster_with_member(&conn, det_c1).unwrap();
        super::set_cluster_hidden(&conn, cluster_c, true).unwrap();

        let visible = super::list_face_clusters(&conn, false).unwrap();
        assert_eq!(visible.iter().map(|c| c.id).collect::<Vec<_>>(), vec![cluster_a, cluster_b]);
        assert_eq!(visible[0].photo_count, 2);
        assert_eq!(visible[1].photo_count, 1);

        let with_hidden = super::list_face_clusters(&conn, true).unwrap();
        assert!(with_hidden.iter().any(|c| c.id == cluster_c && c.hidden));
    }

    #[test]
    fn people_for_image_splits_named_chips_from_unnamed_clusters_and_unclustered() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();

        let named_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let named_cluster = super::create_cluster_with_member(&conn, named_detection).unwrap();
        super::name_cluster(&conn, named_cluster, "Alice").unwrap();

        let unnamed_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let unnamed_cluster = super::create_cluster_with_member(&conn, unnamed_detection).unwrap();

        let unclustered = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[3])).unwrap();
        let _ = unclustered; // never clustered at all

        let hidden_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[4])).unwrap();
        let hidden_cluster = super::create_cluster_with_member(&conn, hidden_detection).unwrap();
        super::name_cluster(&conn, hidden_cluster, "Bob").unwrap();
        super::set_cluster_hidden(&conn, hidden_cluster, true).unwrap();

        let people = super::people_for_image(&conn, image_id).unwrap();
        assert_eq!(people.named.len(), 1);
        assert_eq!(people.named[0].person_name, "Alice");
        assert_eq!(people.unnamed_clustered.len(), 1, "the hidden named cluster must not appear here");
        assert_eq!(people.unnamed_clustered[0].cluster_id, unnamed_cluster);
        assert_eq!(people.unnamed_clustered[0].count, 1);
        assert_eq!(people.unclustered_count, 1);
    }

    #[test]
    fn face_crops_for_cluster_skips_detections_with_no_crop_yet() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();

        let first = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        let cluster_id = super::create_cluster_with_member(&conn, first).unwrap();
        super::set_face_crop_thumbnail_path(&conn, first, "thumbnails/faces/00/1.jpg").unwrap();

        let second = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        super::attach_face_detection_to_cluster(&conn, second, cluster_id).unwrap();
        // second never gets set_face_crop_thumbnail_path — still NULL.

        let crops = super::face_crops_for_cluster(&conn, cluster_id).unwrap();
        assert_eq!(crops.len(), 1);
        assert_eq!(crops[0].detection_id, first);
        assert_eq!(crops[0].crop_thumbnail_path, "thumbnails/faces/00/1.jpg");
    }

    #[test]
    fn pending_face_review_count_only_counts_pending() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('Alex')", []).unwrap();
        let person_id = conn.last_insert_rowid();
        let pending_id = super::queue_face_match_review(&conn, detection_id, person_id, 0.42).unwrap();

        let resolved_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let resolved_queue_id = super::queue_face_match_review(&conn, resolved_detection, person_id, 0.5).unwrap();
        conn.execute("UPDATE face_match_review_queue SET status = 'confirmed' WHERE id = ?1", [resolved_queue_id]).unwrap();

        assert_eq!(super::pending_face_review_count(&conn).unwrap(), 1);
        let _ = pending_id;
    }

    #[test]
    fn list_pending_face_matches_reports_only_pending_with_person_and_crop() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        super::set_face_crop_thumbnail_path(&conn, detection_id, "thumbnails/faces/00/1.jpg").unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('Alex')", []).unwrap();
        let person_id = conn.last_insert_rowid();
        let queue_id = super::queue_face_match_review(&conn, detection_id, person_id, 0.42).unwrap();

        let other_detection = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[2])).unwrap();
        let other_queue_id = super::queue_face_match_review(&conn, other_detection, person_id, 0.5).unwrap();
        conn.execute("UPDATE face_match_review_queue SET status = 'confirmed' WHERE id = ?1", [other_queue_id]).unwrap();

        let entries = super::list_pending_face_matches(&conn).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].queue_id, queue_id);
        assert_eq!(entries[0].face_detection_id, detection_id);
        assert_eq!(entries[0].image_id, image_id);
        assert_eq!(entries[0].crop_thumbnail_path.as_deref(), Some("thumbnails/faces/00/1.jpg"));
        assert_eq!(entries[0].suggested_person_id, person_id);
        assert_eq!(entries[0].suggested_person_name, "Alex");
        assert_eq!(entries[0].similarity_score, 0.42);
    }

    #[test]
    fn confirm_face_match_creates_a_new_cluster_for_the_suggested_person_and_resolves_the_queue() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('Alex')", []).unwrap();
        let person_id = conn.last_insert_rowid();
        let queue_id = super::queue_face_match_review(&conn, detection_id, person_id, 0.42).unwrap();

        let cluster_id = super::confirm_face_match(&conn, queue_id).unwrap();

        let stored_person_id: Option<i64> = conn.query_row("SELECT person_id FROM face_clusters WHERE id = ?1", [cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(stored_person_id, Some(person_id));
        let stored_cluster_id: Option<i64> = conn.query_row("SELECT face_cluster_id FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
        assert_eq!(stored_cluster_id, Some(cluster_id));
        let status: String = conn.query_row("SELECT status FROM face_match_review_queue WHERE id = ?1", [queue_id], |row| row.get(0)).unwrap();
        assert_eq!(status, "confirmed");
        assert!(super::list_pending_face_matches(&conn).unwrap().is_empty());
    }

    #[test]
    fn dismiss_face_match_creates_an_unnamed_cluster_and_resolves_the_queue() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('Alex')", []).unwrap();
        let person_id = conn.last_insert_rowid();
        let queue_id = super::queue_face_match_review(&conn, detection_id, person_id, 0.42).unwrap();

        let cluster_id = super::dismiss_face_match(&conn, queue_id).unwrap();

        let stored_person_id: Option<i64> = conn.query_row("SELECT person_id FROM face_clusters WHERE id = ?1", [cluster_id], |row| row.get(0)).unwrap();
        assert_eq!(stored_person_id, None, "a dismissed match must not be attributed to the suggested person");
        let status: String = conn.query_row("SELECT status FROM face_match_review_queue WHERE id = ?1", [queue_id], |row| row.get(0)).unwrap();
        assert_eq!(status, "dismissed");
        assert!(super::list_pending_face_matches(&conn).unwrap().is_empty());
    }

    #[test]
    fn list_persons_is_alphabetical() {
        let conn = migrated_conn();
        conn.execute("INSERT INTO persons (name) VALUES ('Zed')", []).unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('alice')", []).unwrap();
        conn.execute("INSERT INTO persons (name) VALUES ('Bob')", []).unwrap();

        let names: Vec<String> = super::list_persons(&conn).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["alice".to_string(), "Bob".to_string(), "Zed".to_string()]);
    }

    #[test]
    fn set_face_crop_thumbnail_path_updates_the_row() {
        let conn = migrated_conn();
        let image_id = insert_test_image(&conn);
        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();
        let detection_id = super::insert_face_detection(&conn, &new_face(image_id, model_id, &[1])).unwrap();

        super::set_face_crop_thumbnail_path(&conn, detection_id, "thumbnails/faces/00/1.jpg").unwrap();

        let path: Option<String> = conn.query_row("SELECT crop_thumbnail_path FROM face_detections WHERE id = ?1", [detection_id], |row| row.get(0)).unwrap();
        assert_eq!(path.as_deref(), Some("thumbnails/faces/00/1.jpg"));
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
    fn list_images_filters_by_person_and_excludes_hidden_clusters() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let alices_photo = insert_full_image(&conn, library_id, 50, "jpeg", "2026-01-01T00:00:00Z", 100, "D:/src", "a.jpg");
        let bobs_photo = insert_full_image(&conn, library_id, 51, "jpeg", "2026-01-02T00:00:00Z", 100, "D:/src", "b.jpg");
        let hidden_alice_photo = insert_full_image(&conn, library_id, 52, "jpeg", "2026-01-03T00:00:00Z", 100, "D:/src", "c.jpg");
        let nobodys_photo = insert_full_image(&conn, library_id, 53, "jpeg", "2026-01-04T00:00:00Z", 100, "D:/src", "d.jpg");

        let model_id = super::find_or_create_model(&conn, "sface", "v1", 128, "Apache-2.0").unwrap();

        let alice_detection = super::insert_face_detection(&conn, &new_face(alices_photo, model_id, &[1])).unwrap();
        let alice_cluster = super::create_cluster_with_member(&conn, alice_detection).unwrap();
        let alice_id = super::name_cluster(&conn, alice_cluster, "Alice").unwrap();

        let bob_detection = super::insert_face_detection(&conn, &new_face(bobs_photo, model_id, &[2])).unwrap();
        let bob_cluster = super::create_cluster_with_member(&conn, bob_detection).unwrap();
        super::name_cluster(&conn, bob_cluster, "Bob").unwrap();

        let hidden_detection = super::insert_face_detection(&conn, &new_face(hidden_alice_photo, model_id, &[3])).unwrap();
        let hidden_cluster = super::create_cluster_with_member(&conn, hidden_detection).unwrap();
        super::name_cluster(&conn, hidden_cluster, "Alice").unwrap();
        super::set_cluster_hidden(&conn, hidden_cluster, true).unwrap();

        let _ = nobodys_photo;

        let filters = super::ImageFilters { persons: vec![alice_id], ..Default::default() };
        let (page, total) = super::list_images(&conn, &filters, super::SortOrder::CapturedDesc, None, 0, 10).unwrap();

        let ids: Vec<i64> = page.iter().map(|g| g.id).collect();
        assert_eq!(total, 1, "must match only the non-hidden Alice photo");
        assert!(ids.contains(&alices_photo));
        assert!(!ids.contains(&hidden_alice_photo), "a hidden cluster's photos must not surface via the Person filter");
        assert!(!ids.contains(&bobs_photo));
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
        assert_eq!(
            detail.tags,
            vec![super::TagWithProvenance { name: "family".to_string(), source: "manual".to_string(), confidence: None, review_state: None }]
        );
        assert_eq!(detail.original_hash_hex.len(), 64);
    }

    #[test]
    fn get_image_detail_reports_auto_tag_provenance() {
        let conn = migrated_conn();
        let library_id = test_library(&conn);
        let image_id = insert_full_image(&conn, library_id, 70, "jpeg", "2026-01-01T00:00:00Z", 1234, "D:/src", "photo.jpg");
        super::apply_auto_tag(&conn, image_id, "beach", 0.77).unwrap();

        let detail = super::get_image_detail(&conn, image_id).unwrap().unwrap();

        assert_eq!(
            detail.tags,
            vec![super::TagWithProvenance {
                name: "beach".to_string(),
                source: "auto".to_string(),
                confidence: Some(0.77),
                review_state: Some("unreviewed".to_string())
            }]
        );
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

    #[test]
    fn save_album_inserts_and_list_saved_albums_returns_it_alphabetically() {
        let conn = migrated_conn();
        super::save_album(&conn, "Zebras", r#"{"tags":["zebra"]}"#).unwrap();
        super::save_album(&conn, "  Alpine Trip  ", r#"{"tags":["alpine"]}"#).unwrap();

        let albums = super::list_saved_albums(&conn).unwrap();
        let names: Vec<&str> = albums.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Alpine Trip", "Zebras"], "alphabetical, and the name must be trimmed before storing");
        assert_eq!(albums[0].filters, r#"{"tags":["alpine"]}"#);
    }

    #[test]
    fn save_album_always_inserts_a_new_row_even_for_a_duplicate_name() {
        let conn = migrated_conn();
        super::save_album(&conn, "Beach", "{}").unwrap();
        super::save_album(&conn, "Beach", "{}").unwrap();

        let albums = super::list_saved_albums(&conn).unwrap();
        assert_eq!(albums.len(), 2, "Save always creates a new row — no update-by-name semantics");
    }

    #[test]
    fn delete_saved_album_removes_only_the_targeted_row() {
        let conn = migrated_conn();
        let keep_id = super::save_album(&conn, "Keep", "{}").unwrap();
        let delete_id = super::save_album(&conn, "Delete Me", "{}").unwrap();

        super::delete_saved_album(&conn, delete_id).unwrap();

        let albums = super::list_saved_albums(&conn).unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].id, keep_id);
    }
}
