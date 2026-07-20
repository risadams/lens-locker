---
id: 035
title: "Draft the ML schema extensions (faces, persons, tags, embeddings, saved searches)"
type: workplan:prototype
status: open
assignee:
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
