# LensLocker ML schema — draft prototype

Ticket: [../tickets/035-draft-ml-schema.md](../tickets/035-draft-ml-schema.md)
Status: draft, iterating with the owner before the ticket closes.

This extends [the MVP catalog schema prototype](017-catalog-schema.md) —
same DDL-prototype pattern, same **[CALL]** convention for judgment calls the
map didn't already settle. Every table traces to a specific closed ML-MAP.md
decision; nothing here touches `crates/catalog/schema.sql` directly — per
that file's own header comment, a real migration (`migrations/0003_ml_extensions.sql`
or similar) gets written at actual build time, not by this prototype.

## Rationale — the big structural calls

**Auto-tag provenance/confidence live directly on `image_tags`, not a
separate table.** [Ticket 029](../tickets/029-design-tag-taxonomy.md)'s
core call: a manually-typed "beach" and a model-generated "beach" are one
canonical tag, so `tags` stays untouched and `image_tags` gains `source`,
`confidence`, and `review_state` columns instead of forking into a parallel
concept. `review_state` only covers `unreviewed`/`confirmed` — both are
*applied* states. Rejection is different in kind (per [ticket
033](../tickets/033-design-review-correction-ux.md)): a rejected tag is
**not** currently applied, so it has no business being an `image_tags` row
at all — it needs its own small memory of "don't re-suggest this," which is
what `rejected_tags` is for. **[CALL]**: the pre-existing `tag_scores`
table (schema.sql, Milestone-0 scaffold, predates any of this design work)
is superseded by `image_tags.confidence` and should be **dropped** when the
real migration ships — storing every raw score against every starter label
(including near-zero ones) would be ~10M+ rows at 100k images × ~100
labels for a debugging use case nothing has actually asked for. Flagging
rather than silently carrying forward a table that no longer matches the
design.

**Face schema mirrors `dedupe_review_queue`'s shape wherever the concepts
line up.** `face_match_review_queue` (medium-confidence match against an
already-named person) is structurally the same idea as the dedupe queue —
a `pending`/`confirmed`/`dismissed` row a human resolves — so it reuses
that exact column shape rather than inventing a new one. **[CALL]**:
`face_detections` stores a bounding box and a nullable `crop_thumbnail_path`
even though [ticket 028](../tickets/028-design-face-clustering-ux.md)'s Q5
rejected a bounding-box *overlay* in the main photo view — the box is
still needed to render the individual per-face crop thumbnails that 028's
Q4 split/merge flow depends on (multi-select over face crops, not full
photos). Storing a pre-cropped thumbnail path (mirroring the existing
`thumbnails` table's own pattern) avoids re-decoding the full image every
time a cluster's member grid renders — a performance call, not required by
any ticket's text directly, worth confirming.

**No merge/split audit table for faces**, matching how [ticket
011](../tickets/011-dedupe-ux.md)'s own merge already works (a status +
pointer on `images`, no separate log). Merging two clusters is just
re-pointing the losing cluster's `face_detections.face_cluster_id` rows to
the surviving cluster and deleting the now-empty one; splitting is the
same operation on a subset of rows, into a new cluster. **[CALL]**:
merging two already-**named** persons into one identity (the user
accidentally created two names for the same human) was never addressed by
028 or 033 — if it's needed later it reduces to reassigning one person's
clusters to the other and deleting the orphaned `persons` row, no new
schema — flagging that this case exists and is unhandled by any closed
ticket, not silently assuming it's covered.

**Three tunable thresholds on `app_settings`, mirroring the existing
`hamming_threshold` pattern — but only one has a real sourced default.**
[Ticket 028](../tickets/028-design-face-clustering-ux.md) decided
clustering/auto-attribute/review need separate, independently-tunable
thresholds, stricter in that order. SFace's own LFW-tuned verification
threshold (0.363 cosine, from
[the face-recognition-models research](../research/face-recognition-models.md))
is a real, sourced number — used here as `face_review_threshold`'s
default (the floor for "worth asking a human about at all"). **[CALL]**:
`face_cluster_threshold` and `face_auto_attribute_threshold` do **not**
have researched defaults — I've set them at illustrative placeholders
(0.30 and 0.50 respectively) that just preserve the *ordering* 028 requires
(cluster ≤ review ≤ auto-attribute), explicitly **not** validated against
real SFace output on real photos. This needs real calibration before
ship, the same way tickets 019/025 needed real hardware measurement rather
than paper reasoning — flagging so these three numbers aren't mistaken for
researched figures the other real ones (0.363, the ~200ms latency bar) are.

**Saved albums are one JSON blob column, not normalized filter columns.**
[Ticket 031](../tickets/031-design-smart-albums-ux.md): a name + the
serialized filter/search/sort state (the same shape as the existing
`FiltersDto` the frontend already sends `list_images`), no membership
table since the album is always re-evaluated live. A JSON blob means
adding a new filter facet later (e.g. this ML effort's own Person facet)
never needs a schema migration just to be saveable.

**`sqlite-vec`'s `vec0` virtual table is deliberately absent from this
DDL.** [Ticket 024](../tickets/024-decide-tagging-model-runtime.md)'s
resolution: the durable `embeddings`/`face_detections.embedding` BLOB
columns below are the source of truth; the actual `vec0` table queried at
runtime is built fresh in an **in-memory** connection at library-open,
mirroring these rows — that's Rust/runtime wiring, not persisted schema,
so it has no DDL to draft here.

**`models`/`embeddings`, and the license-note requirement, need no schema
change at all.** Both tables already exist (Milestone-0 forward-compat,
`schema.sql`) and already fit: `embeddings` is already shaped for "one
vector per image per model" (exactly SigLIP's case), `dimension` is
already a column on both tables (never hardcoded), and `models.license_note`
already satisfies [ticket 016](../tickets/016-research-offline-autotagging.md)/[032](../tickets/032-design-model-provisioning.md)'s
provenance-field requirement. This ticket's job here is confirming that,
not adding anything — populating real rows (YuNet, SFace, SigLIP `so400m`)
with real `license_note` text is build-time work, not a schema change.
`face_detections` needed its **own** embedding column rather than reusing
`embeddings` because the relationship is different in shape: `embeddings`
is one-row-per-image (`UNIQUE(image_id, model_id)`), but an image can carry
zero-to-many detected faces — the generic table's uniqueness constraint
would make that impossible.

---

## Schema DDL

```sql
-- ── Tag provenance/review-state (029, 033) ──────────────────────────────

ALTER TABLE image_tags ADD COLUMN source TEXT NOT NULL DEFAULT 'manual'
    CHECK (source IN ('manual','auto'));
ALTER TABLE image_tags ADD COLUMN confidence REAL;         -- only meaningful when source='auto'
ALTER TABLE image_tags ADD COLUMN review_state TEXT
    CHECK (review_state IN ('unreviewed','confirmed'));    -- null for manual tags

-- Explicit "don't re-suggest this" memory — deliberately NOT the same
-- table as image_tags, since a rejected tag is not a currently-applied one.
-- Only gates auto re-application; never blocks a user manually re-adding
-- the same tag on purpose.
CREATE TABLE rejected_tags (
    image_id    INTEGER NOT NULL REFERENCES images(id),
    tag_id      INTEGER NOT NULL REFERENCES tags(id),
    rejected_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (image_id, tag_id)
);

-- [CALL] superseded by image_tags.confidence above — drop when this ships.
-- DROP TABLE tag_scores;

-- ── Faces & persons (023, 028, 033) ──────────────────────────────────────

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
    crop_thumbnail_path  TEXT,                      -- [CALL] pre-cropped for the split/merge grid; see rationale
    embedding            BLOB NOT NULL,              -- SFace vector, this specific face
    dimension            INTEGER NOT NULL,           -- never hardcode 128 (016's reservation, applied to faces too)
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

CREATE INDEX idx_face_detections_image ON face_detections(image_id);
CREATE INDEX idx_face_detections_cluster ON face_detections(face_cluster_id);

-- Medium-confidence match against an existing NAMED person (028's three-tier
-- split) — same shape as dedupe_review_queue on purpose.
CREATE TABLE face_match_review_queue (
    id                 INTEGER PRIMARY KEY,
    face_detection_id  INTEGER NOT NULL REFERENCES face_detections(id),
    suggested_person_id INTEGER NOT NULL REFERENCES persons(id),
    similarity_score   REAL NOT NULL,
    status             TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending','confirmed','dismissed')),
    created_at         TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    resolved_at        TEXT
);

CREATE INDEX idx_face_match_queue_pending ON face_match_review_queue(status) WHERE status = 'pending';

-- ── Tunable thresholds (028) — see [CALL] above re: which defaults are real ─

ALTER TABLE app_settings ADD COLUMN face_cluster_threshold REAL NOT NULL DEFAULT 0.30;
ALTER TABLE app_settings ADD COLUMN face_review_threshold REAL NOT NULL DEFAULT 0.363; -- SFace's own published verification threshold
ALTER TABLE app_settings ADD COLUMN face_auto_attribute_threshold REAL NOT NULL DEFAULT 0.50;

-- ── Saved albums / smart searches (031) ──────────────────────────────────

CREATE TABLE saved_albums (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    filters    TEXT NOT NULL,     -- serialized JSON: the same shape as the frontend's FiltersDto
                                    -- + sort + search + any future facet (e.g. person), so a new
                                    -- filter facet never needs its own migration to be saveable
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);

-- ── No DDL needed here (see rationale): ──────────────────────────────────
-- `embeddings`, `models` (schema.sql, Milestone-0 forward-compat) already
-- fit the tagging-embedding shape and already carry `dimension`/
-- `license_note` unmodified. `sqlite-vec`'s vec0 mirror is runtime-only
-- (024), not persisted schema.
```

## Owner reaction (resolved)

All three flagged calls confirmed as drafted, no corrections:

1. **Drop `tag_scores`** — confirmed. Superseded by `image_tags.confidence`;
   storing every sub-threshold score against every starter label was data
   nobody asked for.
2. **`face_cluster_threshold`/`face_auto_attribute_threshold` as
   explicitly-flagged placeholders** (0.30/0.50), distinct from the one
   real sourced default (`face_review_threshold` = 0.363, SFace's own
   published verification threshold) — confirmed as drafted. Real
   calibration against actual SFace output on real photos remains a
   build-time task, same treatment as the unconfirmed installer-size
   numbers from [ticket 032](../tickets/032-design-model-provisioning.md).
3. **Store `crop_thumbnail_path` on `face_detections`**, despite 028
   rejecting bounding-box overlays in the main photo view — confirmed.
   Needed for the cluster split/merge grid's per-face thumbnails
   ([ticket 028](../tickets/028-design-face-clustering-ux.md) Q4); avoids
   re-decoding the full image every time that grid renders.
