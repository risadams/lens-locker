---
id: 035
title: "Draft the ML schema extensions (faces, persons, tags, embeddings, saved searches)"
type: workplan:prototype
status: closed
assignee: chris (grilling session 2026-07-20)
blocked-by: [025, 028, 029, 031, 033, 034]
---

## Question

With sqlite-vec's real-hardware capacity confirmed ([ticket
025](025-rebenchmark-sqlite-vec.md)) and every behavior decision that shapes
the data model landed (face clustering [028], tag taxonomy [029], smart
albums [031], review/correction [033], similarity search [034]), draft the
concrete schema extension to [the existing catalog schema
prototype](../prototypes/017-catalog-schema.md) — the same DDL-prototype
pattern that ticket produced for the MVP:

- Embedding storage (per image, model-identity/version-tagged, dimension not
  hardcoded — per ticket 016's reservation requirement), `sqlite-vec` virtual
  table wiring.
- Tag/category tables with the auto-vs-manual provenance flag from 029, and
  confidence scores.
- Face/person tables: detected faces (bounding box, embedding, source
  image), clusters, named persons, and the merge/split history from 028.
- Saved-search/smart-album storage from 031.
- License/provenance field for bundled model weights (ticket 016's
  reservation, concretized here).

## Resolution

Drafted and resolved 2026-07-20; full DDL + rationale in [the prototype
file](../prototypes/035-ml-schema.md), same DDL-prototype pattern [ticket
017](017-draft-catalog-schema.md) used for the MVP schema. Every table
traces to a specific closed ML-MAP.md decision. Three flagged judgment
calls, all confirmed as drafted:

- **`tag_scores` (Milestone-0 forward-compat) is dropped**, superseded by
  `image_tags.confidence`/`source`/`review_state` (029, 033) — storing
  every sub-threshold score against every starter label would be ~10M+
  rows nothing asked for.
- **Tag rejection gets its own small table** (`rejected_tags`), not a
  third `image_tags.review_state` value — a rejected tag isn't currently
  applied, so it shouldn't be an `image_tags` row; the table only gates
  auto re-application, never blocks a manual re-add.
- **Faces get their own tables** (`persons`, `face_clusters`,
  `face_detections`, `face_match_review_queue`) rather than reusing the
  generic `embeddings` table, because an image can carry zero-to-many
  faces — incompatible with `embeddings`' one-row-per-image uniqueness
  constraint. `face_match_review_queue` deliberately mirrors
  `dedupe_review_queue`'s exact shape (028's three-tier match design is
  the same "pending human review" idea). Merge/split has no audit table,
  matching how [ticket 011](011-dedupe-ux.md)'s own merge already works
  (status + pointer, not a log).
- **Three tunable thresholds on `app_settings`** (028), only one with a
  real sourced default (`face_review_threshold` = 0.363, SFace's own
  published verification threshold); the other two are explicitly-flagged
  placeholders preserving the required ordering, not researched figures —
  confirmed acceptable pending real build-time calibration.
- **`face_detections` stores a bounding box + pre-cropped thumbnail path**
  despite 028 rejecting overlay UI — needed for the split/merge grid's
  per-face thumbnails, not the main photo view.
- **Saved albums are one JSON filter blob**, no membership table (031).
  **`sqlite-vec`'s `vec0` mirror is deliberately absent** — runtime-only
  per [ticket 024](024-decide-tagging-model-runtime.md), not persisted
  schema. **`embeddings`/`models`/`license_note` need no change at all** —
  already fit, already reserved since Milestone 0.

**This was the last blocker on [ticket 036](036-assemble-ml-spec.md)** —
every ticket on the ML-MAP.md frontier is now closed.
