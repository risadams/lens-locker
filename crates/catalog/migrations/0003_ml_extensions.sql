-- Migration 3 (ML-SPEC.md Milestone ML-1): auto-tag provenance, faces/persons,
-- saved albums. DDL sourced verbatim from workplan/prototypes/035-ml-schema.md
-- (owner-confirmed, see that file's "Owner reaction" section) — do not
-- hand-edit once shipped, per crates/catalog/schema.sql's own migration
-- discipline comment (line 3); add a new migration file instead.

-- ── Tag provenance/review-state (tickets 029, 033) ────────────────────────

ALTER TABLE image_tags ADD COLUMN source TEXT NOT NULL DEFAULT 'manual'
    CHECK (source IN ('manual','auto'));
ALTER TABLE image_tags ADD COLUMN confidence REAL;         -- only meaningful when source='auto'
ALTER TABLE image_tags ADD COLUMN review_state TEXT
    CHECK (review_state IN ('unreviewed','confirmed'));    -- null for manual tags

-- Explicit "don't re-suggest this" memory — deliberately NOT the same table
-- as image_tags, since a rejected tag is not a currently-applied one. Only
-- gates auto re-application; never blocks a user manually re-adding the
-- same tag on purpose.
CREATE TABLE rejected_tags (
    image_id    INTEGER NOT NULL REFERENCES images(id),
    tag_id      INTEGER NOT NULL REFERENCES tags(id),
    rejected_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (image_id, tag_id)
);

-- Superseded by image_tags.confidence above (workplan/prototypes/035-ml-schema.md's
-- [CALL] 1, owner-confirmed): storing every raw score against every starter label,
-- including near-zero ones, is ~10M+ rows at 100k images x ~100 labels for a
-- debugging use case nothing has asked for.
DROP TABLE tag_scores;

-- ── Faces & persons (tickets 023, 028, 033) ───────────────────────────────

CREATE TABLE persons (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE TABLE face_clusters (
    id         INTEGER PRIMARY KEY,
    person_id  INTEGER REFERENCES persons(id),   -- null = unnamed/provisional (028)
    hidden     INTEGER NOT NULL DEFAULT 0 CHECK (hidden IN (0,1)),  -- reversible Hide (028)
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_face_clusters_person ON face_clusters(person_id);

CREATE TABLE face_detections (
    id                   INTEGER PRIMARY KEY,
    image_id             INTEGER NOT NULL REFERENCES images(id),
    face_cluster_id      INTEGER REFERENCES face_clusters(id),  -- null until clustering catches up
    model_id             INTEGER NOT NULL REFERENCES models(id), -- the embedding model (SFace); detection
                                                                   -- is re-run alongside embedding on any
                                                                   -- upgrade, so its own version isn't
                                                                   -- tracked separately here
    detection_confidence REAL NOT NULL,             -- YuNet's own score; sub-floor faces are never inserted
    bbox_x               INTEGER NOT NULL,
    bbox_y               INTEGER NOT NULL,
    bbox_width           INTEGER NOT NULL,
    bbox_height          INTEGER NOT NULL,
    crop_thumbnail_path  TEXT,                      -- pre-cropped for the split/merge grid (028 Q4); avoids
                                                       -- re-decoding the full image every time that grid renders
    embedding             BLOB NOT NULL,              -- SFace vector, this specific face
    dimension             INTEGER NOT NULL,           -- never hardcode 128 (016's reservation, applied to faces too)
    created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_face_detections_image ON face_detections(image_id);
CREATE INDEX idx_face_detections_cluster ON face_detections(face_cluster_id);

-- Medium-confidence match against an existing NAMED person (028's three-tier
-- split) — same shape as dedupe_review_queue on purpose.
CREATE TABLE face_match_review_queue (
    id                   INTEGER PRIMARY KEY,
    face_detection_id    INTEGER NOT NULL REFERENCES face_detections(id),
    suggested_person_id  INTEGER NOT NULL REFERENCES persons(id),
    similarity_score      REAL NOT NULL,
    status                TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','confirmed','dismissed')),
    created_at            TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    resolved_at           TEXT
);

CREATE INDEX idx_face_match_queue_pending ON face_match_review_queue(status) WHERE status = 'pending';

-- ── Tunable thresholds (028) — only face_review_threshold is a real sourced
-- default (SFace's own published LFW verification threshold, 0.363 cosine).
-- face_cluster_threshold/face_auto_attribute_threshold (0.30/0.50) are
-- explicitly-flagged placeholders preserving required ordering
-- (cluster <= review <= auto-attribute), NOT validated against real SFace
-- output — needs real calibration before ship (035-draft-ml-schema.md).

ALTER TABLE app_settings ADD COLUMN face_cluster_threshold REAL NOT NULL DEFAULT 0.30;
ALTER TABLE app_settings ADD COLUMN face_review_threshold REAL NOT NULL DEFAULT 0.363;
ALTER TABLE app_settings ADD COLUMN face_auto_attribute_threshold REAL NOT NULL DEFAULT 0.50;

-- ── Saved albums / smart searches (031) ───────────────────────────────────

CREATE TABLE saved_albums (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    filters    TEXT NOT NULL,     -- serialized JSON: same shape as the frontend's FiltersDto
                                    -- + sort + search + any future facet (e.g. person), so a new
                                    -- filter facet never needs its own migration to be saveable
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- `embeddings`/`models` (schema.sql, Milestone-0 forward-compat) need no
-- schema change here — already fit the one-embedding-per-image-per-model
-- shape (SigLIP) and already carry dimension/license_note. sqlite-vec's
-- vec0 mirror is runtime-only (024), not persisted schema.
