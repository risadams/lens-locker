---
id: 017
title: "Draft the catalog schema"
type: workplan:prototype
status: closed
assignee: chris
blocked-by: [010, 011, 012]
---

## Question

Produce a concrete SQLite schema draft to react to — images, stored blobs (one blob,
many logical images, per the dedupe decision), libraries, tags, hashes (BLAKE3 +
perceptual), import/quarantine journal, EXIF/metadata fields worth first-class
columns vs a JSON blob, FTS for text search, and forward-compatibility hooks for
the ML milestone (embedding table). Must reflect the store layout
([Design the import pipeline and managed store layout](010-import-pipeline-store-layout.md)),
the duplicate model ([Decide the duplicate-handling experience](011-dedupe-ux.md)),
and the portability decision
([Decide metadata and tag portability](012-metadata-portability.md)).
The prototype is the schema DDL + a page of rationale, iterated with the owner.

## Resolution

Prototype: [../prototypes/017-catalog-schema.md](../prototypes/017-catalog-schema.md)
(full DDL + rationale). Iterated with the owner on three flagged judgment calls:

- **`images` is 1:1 with unique content** (one grid card per photo, not per
  import) — confirmed. `image_sources` holds the many:1 audit trail of every
  filename/path that resolved to that content.
- **Tags are global**, not per-library — confirmed, consistent with MVP's
  single-library scope; multi-library tag scoping stays fog for later.
- **Hamming threshold and retention window are global app settings**, not
  per-library as first drafted — corrected by the owner; conversion-enabled
  remains the one per-library setting, since that one *is* explicitly decided
  as per-library in [Define the conversion policy](009-conversion-policy.md).

Structurally: content-addressed storage keyed by pre-conversion hash
([010](010-import-pipeline-store-layout.md)), quarantine unified across
import-time and merge-time discards with one retention model, merge outcome as
`images.status`/`merged_into_id` rather than a separate log (satisfies
[012](012-metadata-portability.md)'s "merge history isn't sidecar-recoverable"),
and empty-but-shaped `models`/`embeddings`/`tag_scores` tables reserved for the
post-MVP ML milestone per [016](016-research-offline-autotagging.md).
Thumbnails get a deliberately minimal table — sizing/eviction stays fog.

**Feeds forward into** [Assemble the locked spec and build plan](018-assemble-spec.md).
