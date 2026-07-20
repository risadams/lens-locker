-- LensLocker catalog schema — v1
-- Source of truth: workplan/prototypes/017-catalog-schema.md
-- Applied as a single rusqlite_migration step; do not hand-edit an existing
-- migration once it has shipped — add a new one instead.

CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO schema_meta (key, value) VALUES ('schema_version', '1');

-- ── Libraries ────────────────────────────────────────────────────────────

CREATE TABLE libraries (
    id                 INTEGER PRIMARY KEY,
    name               TEXT NOT NULL,
    root_path          TEXT NOT NULL UNIQUE,
    conversion_enabled INTEGER NOT NULL DEFAULT 1 CHECK (conversion_enabled IN (0,1)),
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- Global app-wide settings (singleton row)
CREATE TABLE app_settings (
    id                 INTEGER PRIMARY KEY CHECK (id = 1),
    hamming_threshold  INTEGER NOT NULL DEFAULT 5,
    retention_days     INTEGER NOT NULL DEFAULT 30
);
INSERT INTO app_settings (id, hamming_threshold, retention_days) VALUES (1, 5, 30);

-- ── Images: the catalog entity, 1:1 with unique content ─────────────────

CREATE TABLE images (
    id               INTEGER PRIMARY KEY,
    library_id       INTEGER NOT NULL REFERENCES libraries(id),

    original_hash    BLOB NOT NULL UNIQUE,
    stored_hash      BLOB NOT NULL,
    stored_path      TEXT NOT NULL,
    original_format  TEXT NOT NULL,
    stored_format    TEXT NOT NULL,
    conversion_status TEXT NOT NULL DEFAULT 'not_applicable'
        CHECK (conversion_status IN ('not_applicable','not_attempted','converted','failed_kept_original')),
    file_size_bytes  INTEGER NOT NULL,

    perceptual_hash  INTEGER,

    capture_date     TEXT,
    camera_make      TEXT,
    camera_model     TEXT,
    width            INTEGER,
    height           INTEGER,
    orientation      INTEGER,

    sidecar_path      TEXT,
    sidecar_synced_at TEXT,

    status           TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active','merged')),
    merged_into_id   INTEGER REFERENCES images(id),
    merged_at        TEXT,

    first_imported_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_images_library ON images(library_id);
CREATE INDEX idx_images_capture_date ON images(capture_date);
CREATE INDEX idx_images_perceptual_hash ON images(perceptual_hash);

CREATE TABLE import_batches (
    id           INTEGER PRIMARY KEY,
    library_id   INTEGER NOT NULL REFERENCES libraries(id),
    source_root  TEXT NOT NULL,
    started_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    completed_at TEXT,
    status       TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running','completed','completed_with_errors'))
);

-- Every filename/path/timestamp that ever resolved to this content (many:1)
CREATE TABLE image_sources (
    id                INTEGER PRIMARY KEY,
    image_id          INTEGER NOT NULL REFERENCES images(id),
    import_batch_id   INTEGER NOT NULL REFERENCES import_batches(id),
    original_filename TEXT NOT NULL,
    source_path       TEXT NOT NULL,
    imported_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_image_sources_image ON image_sources(image_id);

-- RAW+JPEG bonded pairs, excluded from the perceptual review queue
CREATE TABLE raw_jpeg_pairs (
    id            INTEGER PRIMARY KEY,
    raw_image_id  INTEGER NOT NULL REFERENCES images(id),
    jpeg_image_id INTEGER NOT NULL REFERENCES images(id),
    paired_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (raw_image_id, jpeg_image_id)
);

-- ── Tags ──────────────────────────────────────────────────────────────────

CREATE TABLE tags (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE image_tags (
    image_id  INTEGER NOT NULL REFERENCES images(id),
    tag_id    INTEGER NOT NULL REFERENCES tags(id),
    tagged_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (image_id, tag_id)
);

-- Full-text search over filenames, camera info, and tag names.
-- NOTE: redefined as a standard (non-contentless) table by
-- migrations/0002_standard_fts.sql (Milestone 5) — see that file's comment.
-- Left as originally drafted here since this file is an already-shipped
-- migration step (per this file's own header comment: don't hand-edit it).
CREATE VIRTUAL TABLE images_fts USING fts5(
    original_filename, camera_make, camera_model, tag_names,
    content='', tokenize='porter unicode61'
);

-- ── Import pipeline: per-file journal, crash recovery ────────────────────

CREATE TABLE import_journal (
    id           INTEGER PRIMARY KEY,
    batch_id     INTEGER NOT NULL REFERENCES import_batches(id),
    source_path  TEXT NOT NULL,
    step         TEXT NOT NULL DEFAULT 'pending'
        CHECK (step IN ('pending','hashed','dedupe_checked','converted','verified',
                         'sidecar_written','quarantined','done','failed')),
    image_id     INTEGER REFERENCES images(id),
    error        TEXT,
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_import_journal_batch_step ON import_journal(batch_id, step);

-- ── Quarantine: import-originals and merge-discards, one retention model ─

CREATE TABLE quarantine (
    id                INTEGER PRIMARY KEY,
    library_id        INTEGER NOT NULL REFERENCES libraries(id),
    quarantine_type   TEXT NOT NULL CHECK (quarantine_type IN ('import_original','merge_discard')),
    related_image_id  INTEGER REFERENCES images(id),
    quarantined_path  TEXT NOT NULL,
    original_path     TEXT,
    quarantined_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    expires_at        TEXT NOT NULL,
    purged_at         TEXT
);

CREATE INDEX idx_quarantine_expiry ON quarantine(expires_at) WHERE purged_at IS NULL;

-- ── Perceptual near-duplicate review queue ────────────────────────────────

CREATE TABLE dedupe_review_queue (
    id               INTEGER PRIMARY KEY,
    image_a_id       INTEGER NOT NULL REFERENCES images(id),
    image_b_id       INTEGER NOT NULL REFERENCES images(id),
    hamming_distance INTEGER NOT NULL,
    status           TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','merged','dismissed')),
    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    resolved_at      TEXT
);

CREATE INDEX idx_dedupe_queue_pending ON dedupe_review_queue(status) WHERE status = 'pending';

-- ── Thumbnails — minimal, per SPEC.md §9 defaults ─────────────────────────

CREATE TABLE thumbnails (
    id           INTEGER PRIMARY KEY,
    image_id     INTEGER NOT NULL REFERENCES images(id),
    variant      TEXT NOT NULL,
    format       TEXT NOT NULL,
    path         TEXT NOT NULL,
    generated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (image_id, variant)
);

-- ── ML forward-compat: empty until the post-MVP milestone (SPEC.md §11) ──

CREATE TABLE models (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL,
    version       TEXT NOT NULL,
    dimension     INTEGER NOT NULL,
    license_note  TEXT NOT NULL,
    bundled_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (name, version)
);

CREATE TABLE embeddings (
    id         INTEGER PRIMARY KEY,
    image_id   INTEGER NOT NULL REFERENCES images(id),
    model_id   INTEGER NOT NULL REFERENCES models(id),
    vector     BLOB NOT NULL,
    dimension  INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (image_id, model_id)
);

CREATE TABLE tag_scores (
    id         INTEGER PRIMARY KEY,
    image_id   INTEGER NOT NULL REFERENCES images(id),
    model_id   INTEGER NOT NULL REFERENCES models(id),
    label      TEXT NOT NULL,
    score      REAL NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_tag_scores_image ON tag_scores(image_id);
