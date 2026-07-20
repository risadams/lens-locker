---
id: 029
title: "Design tag/category taxonomy (open-vocabulary zero-shot vs. fixed list)"
type: workplan:grilling
status: open
assignee:
blocked-by: [024]
---

## Question

With the tagging model/mechanism decided ([ticket
024](024-decide-tagging-model-runtime.md)), design what "categorization"
actually means for LensLocker's images:

- **Open-vocabulary zero-shot** (score each image's embedding against an
  arbitrary label set at query/tag time — flexible, but label set quality
  drives result quality) vs. **fixed taxonomy** (a curated label list shipped
  with the app, simpler UX, less flexible) vs. a hybrid (fixed starter list,
  user-extensible).
- How auto-tags relate to LensLocker's existing **manual tags** (already
  built, per [ticket 012's metadata-portability
  decision](012-metadata-portability.md), SQLite-authoritative + XMP
  sidecar-mirrored) — same table with a provenance flag (auto vs. manual),
  or a separate concept entirely? This is likely the single most
  schema-shaping decision this map makes.
- Confidence-score threshold/display — does a low-confidence auto-tag show
  at all, and does the user see the score or just a binary tag.
- Category vs. tag distinction, if any (e.g. "landscape" as a broad category
  vs. "beach", "sunset" as tags) — or are they the same mechanism at
  different specificity.

## Resolution
