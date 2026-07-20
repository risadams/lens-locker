---
id: 028
title: "Design face clustering/grouping UX (detect, group, name, merge, split)"
type: workplan:grilling
status: open
assignee:
blocked-by: [023, 026]
---

## Question

Once a face detection + embedding model is chosen ([ticket
023](023-research-face-model-licensing.md)) and the legal/ethics pass
([ticket 026](026-legal-review-face-recognition.md)) has landed, design the
actual face auto-recognition user experience:

- Detection happens automatically at import/background time — do unnamed
  face clusters surface in the UI proactively, or only when the user opts
  into a "People" view?
- Clustering: automatic grouping of similar face embeddings into unnamed
  clusters — what's the review/confirm flow before a cluster is "real"
  (per [ticket 011's dedupe-UX
  precedent](011-dedupe-ux.md), never auto-merge silently for anything
  ambiguous)?
- Naming: how a user assigns a name/identity to a cluster; whether unnamed
  clusters persist indefinitely or get pruned.
- Merge/split: correcting a wrong grouping (two people merged into one
  cluster, or one person split across two) — this is expected to happen
  often with any local face model, so the correction flow is load-bearing,
  not an edge case.
- Multiple faces per image — how they're presented (e.g. bounding-box
  overlay vs. a simple "people in this photo" tag list).

## Resolution
