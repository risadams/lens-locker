# LumenVault catalog schema — draft prototype

Ticket: [../tickets/017-draft-catalog-schema.md](../tickets/017-draft-catalog-schema.md)
Status: draft, iterating with the owner before the ticket closes.

This is a concrete artifact to react to, not a final schema. Every table traces
to a specific closed decision on the map; where I made a judgment call the map
didn't already settle, it's flagged inline as **[CALL]** — those are the places
most worth pushing back on.

## Rationale — the big structural calls

**`images` is 1:1 with unique content, not 1:1 with "an import."** [CALL]
[Decide the duplicate-handling experience](../tickets/011-dedupe-ux.md) says exact
dupes collapse to "one stored blob, referenced by every logical image row that
matched it" — I read "logical image row" as *an import event*, not a second
visible catalog entry, because two byte-identical files are the same photo, full
stop. So `images` holds one row per unique original-hash (the thing you'd see as
one card in the grid), and every filename/path/timestamp that ever resolved to it
lives in `image_sources` (many rows, one per import event). If you actually want
duplicate imports to show as separate grid entries pointing at shared storage,
that's a different shape — worth confirming before this locks.

**Storage and catalog fields live in one `images` table, not split into
`blobs` + `images`.** Since they're 1:1 under the above reading, splitting them
added a join with no independent lifecycle to justify it. `image_sources` is the
table that actually has a real one-to-many shape.

**Tags are global, not per-library.** [CALL] The MVP milestone in the original
brief is explicitly single-library, and multi-library tag scoping is still fog
([Not yet specified](../MAP.md)). Scoping tags per-library now would be a guess at
an unmade decision — I left them global and flagged it as something the
multi-library fog item needs to revisit, rather than pre-deciding it here.

**Conversion setting lives on `libraries`; dedupe threshold and retention window
are global app settings.** [Define the conversion policy](../tickets/009-conversion-policy.md)
fixed conversion as a per-library, at-creation setting — that stays on
`libraries`. The owner confirmed the Hamming threshold and the quarantine
retention window should be single app-wide settings instead (simpler than a
per-library variable, revisit only if multi-library usage later shows real
divergence) — both moved to a new `app_settings` singleton table.

**Quarantine handles two distinct payload types.** Import-time quarantine
(the original source file, replaced by its converted/verified copy) and
merge-time quarantine (a `stored` blob discarded because a human confirmed a
perceptual-dupe merge in the review queue) are different events with different
triggers, but both need the same expiry/retention mechanics — so one table,
tagged by `quarantine_type`, rather than two.

**Merge outcome is a status on `images`, not a separate log table.**
`status = 'merged'` + `merged_into_id` covers "what happened," is trivially
queryable, and satisfies [the metadata-portability ticket](../tickets/012-metadata-portability.md)'s
note that merge history is *not* sidecar-recoverable — it only exists here.

**Embeddings, models, and tag_scores are included now** even though
[the auto-tagging milestone](../tickets/016-research-offline-autotagging.md) is
post-MVP, because retrofitting a model-versioned vector column later is exactly
the kind of migration that research ticket said to design around up front. They're
empty tables until that milestone ships.

**Thumbnails get a minimal table, not a fully-specified one.** Sizing/eviction
policy is still fog (Not yet specified: "Thumbnail/preview cache strategy"). This
table exists so MVP has *somewhere* to put generated thumbnails; `variant` is a
free-text tag (e.g. `grid_256`) deliberately left loose until that fog item
resolves.

---

## Schema DDL

```sql
PRAGMA foreign_keys = ON;

CREATE TABLE schema_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO schema_meta (key, value) VALUES ('schema_version', '1');

-- ── Libraries ────────────────────────────────────────────────────────────

CREATE TABLE libraries (
    id                 INTEGER PRIMARY KEY,
    name               TEXT NOT NULL,
    root_path          TEXT NOT NULL UNIQUE,   -- managed store root on disk
    conversion_enabled INTEGER NOT NULL DEFAULT 1 CHECK (conversion_enabled IN (0,1)),
                                                 -- fixed at creation, per 009
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- Global app-wide settings (singleton row) — dedupe threshold and retention
-- window are app-wide, not per-library, per owner confirmation
CREATE TABLE app_settings (
    id                 INTEGER PRIMARY KEY CHECK (id = 1),
    hamming_threshold  INTEGER NOT NULL DEFAULT 5,   -- perceptual review-queue cutoff, per 011
    retention_days     INTEGER NOT NULL DEFAULT 30   -- quarantine window, per 005
);
INSERT INTO app_settings (id, hamming_threshold, retention_days) VALUES (1, 5, 30);

-- ── Images: the catalog entity, 1:1 with unique content ─────────────────

CREATE TABLE images (
    id               INTEGER PRIMARY KEY,
    library_id       INTEGER NOT NULL REFERENCES libraries(id),

    -- content identity & storage (010: content-addressed, keyed pre-conversion)
    original_hash    BLOB NOT NULL UNIQUE,   -- BLAKE3 of source bytes as they arrived
    stored_hash      BLOB NOT NULL,          -- BLAKE3 of what's actually on disk (integrity)
    stored_path      TEXT NOT NULL,          -- relative path within library root
    original_format  TEXT NOT NULL,          -- e.g. 'jpeg', 'png', 'cr3', 'heic'
    stored_format     TEXT NOT NULL,         -- what's on disk now: original_format or 'jxl'/'tiff'
    conversion_status TEXT NOT NULL DEFAULT 'not_applicable'
        CHECK (conversion_status IN ('not_applicable','not_attempted','converted','failed_kept_original')),
    file_size_bytes  INTEGER NOT NULL,

    -- perceptual dedupe (001, 011)
    perceptual_hash  INTEGER,               -- 64-bit pHash, nullable (e.g. RAW without a raster path yet)

    -- EXIF-derived, intrinsic to the content (011: exact dupes are byte-identical, so these
    -- belong to the content, not to any one import event)
    capture_date     TEXT,
    camera_make      TEXT,
    camera_model     TEXT,
    width            INTEGER,
    height           INTEGER,
    orientation      INTEGER,

    -- metadata portability (012)
    sidecar_path     TEXT,                  -- .xmp path, mirrors this row's tags/metadata
    sidecar_synced_at TEXT,                 -- null/stale means a rebuild-from-sidecar gap

    -- lifecycle
    status           TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active','merged')),
    merged_into_id   INTEGER REFERENCES images(id),  -- set when status = 'merged'
    merged_at        TEXT,

    first_imported_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_images_library ON images(library_id);
CREATE INDEX idx_images_capture_date ON images(capture_date);
CREATE INDEX idx_images_perceptual_hash ON images(perceptual_hash);

-- Every filename/path/timestamp that ever resolved to this content (many:1)
CREATE TABLE image_sources (
    id                INTEGER PRIMARY KEY,
    image_id          INTEGER NOT NULL REFERENCES images(id),
    import_batch_id   INTEGER NOT NULL REFERENCES import_batches(id),
    original_filename TEXT NOT NULL,
    source_path       TEXT NOT NULL,        -- informational/audit only; source is gone post-move
    imported_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_image_sources_image ON image_sources(image_id);

-- RAW+JPEG bonded pairs, excluded from the perceptual review queue (011)
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
    name TEXT NOT NULL UNIQUE   -- global for MVP; see [CALL] above re: multi-library
);

CREATE TABLE image_tags (
    image_id  INTEGER NOT NULL REFERENCES images(id),
    tag_id    INTEGER NOT NULL REFERENCES tags(id),
    tagged_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (image_id, tag_id)
);

-- Full-text search over filenames, camera info, and tag names
CREATE VIRTUAL TABLE images_fts USING fts5(
    original_filename, camera_make, camera_model, tag_names,
    content='', tokenize='porter unicode61'
);

-- ── Import pipeline: batches, per-file journal, crash recovery (010) ────

CREATE TABLE import_batches (
    id           INTEGER PRIMARY KEY,
    library_id   INTEGER NOT NULL REFERENCES libraries(id),
    source_root  TEXT NOT NULL,
    started_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    completed_at TEXT,
    status       TEXT NOT NULL DEFAULT 'running'
        CHECK (status IN ('running','completed','completed_with_errors'))
);

CREATE TABLE import_journal (
    id           INTEGER PRIMARY KEY,
    batch_id     INTEGER NOT NULL REFERENCES import_batches(id),
    source_path  TEXT NOT NULL,
    step         TEXT NOT NULL DEFAULT 'pending'
        CHECK (step IN ('pending','hashed','dedupe_checked','converted','verified',
                         'sidecar_written','quarantined','done','failed')),
    image_id     INTEGER REFERENCES images(id),   -- set once resolved
    error        TEXT,
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_import_journal_batch_step ON import_journal(batch_id, step);

-- ── Quarantine: import-originals and merge-discards, one retention model (005, 010, 011) ─

CREATE TABLE quarantine (
    id              INTEGER PRIMARY KEY,
    library_id      INTEGER NOT NULL REFERENCES libraries(id),
    quarantine_type TEXT NOT NULL CHECK (quarantine_type IN ('import_original','merge_discard')),
    related_image_id INTEGER REFERENCES images(id),
    quarantined_path TEXT NOT NULL,   -- always on the managed-store volume (010)
    original_path    TEXT,            -- informational
    quarantined_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    expires_at        TEXT NOT NULL,
    purged_at          TEXT           -- null until the launch-only sweep deletes it
);

CREATE INDEX idx_quarantine_expiry ON quarantine(expires_at) WHERE purged_at IS NULL;

-- ── Perceptual near-duplicate review queue (011) ─────────────────────────

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

-- ── Thumbnails — minimal, sizing/eviction still fog ──────────────────────

CREATE TABLE thumbnails (
    id           INTEGER PRIMARY KEY,
    image_id     INTEGER NOT NULL REFERENCES images(id),
    variant      TEXT NOT NULL,       -- e.g. 'grid_256' — loose until the fog item resolves
    format       TEXT NOT NULL,       -- e.g. 'webp'
    path         TEXT NOT NULL,
    generated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (image_id, variant)
);

-- ── ML forward-compat: empty until the post-MVP milestone (016) ─────────

CREATE TABLE models (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL,          -- e.g. 'siglip-base-patch16-224'
    version       TEXT NOT NULL,
    dimension     INTEGER NOT NULL,
    license_note  TEXT NOT NULL,          -- provenance/redistribution terms, per 016
    bundled_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (name, version)
);

CREATE TABLE embeddings (
    id         INTEGER PRIMARY KEY,
    image_id   INTEGER NOT NULL REFERENCES images(id),
    model_id   INTEGER NOT NULL REFERENCES models(id),
    vector     BLOB NOT NULL,
    dimension  INTEGER NOT NULL,          -- redundant with models.dimension, kept for a quick sanity check
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (image_id, model_id)            -- multiple model versions can coexist during a migration
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
```

## Owner reaction (resolved)

All three flagged calls confirmed against the draft, with one correction:

1. **Grid unit: one card per unique photo** — confirmed as drafted.
   `images`/`image_sources` split stands.
2. **Tags: global** — confirmed as drafted. Multi-library tag scoping stays in
   the fog, to be revisited if/when that milestone is designed.
3. **Hamming threshold / retention window: global app settings, not per-library**
   — corrected from the draft. Moved off `libraries` onto a new `app_settings`
   singleton table (see DDL).
