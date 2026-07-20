---
id: 034
title: "Design similarity-search UX and its overlap with the existing dedupe review queue"
type: workplan:grilling
status: open
assignee:
blocked-by: [029]
---

## Question

Embedding-based similarity search ("find images like this one") was always
part of ticket 016's sketch, distinct from perceptual-hash dedupe (already
built, per [ticket 011](011-dedupe-ux.md)) which catches near-identical
images, not semantically similar ones. With taxonomy decided ([ticket
029](029-design-tag-taxonomy.md)), design:

- Entry point — a "find similar" action from any image, a dedicated search-
  by-example mode, or both.
- How results are presented — ranked list, threshold cutoff, integrated into
  the existing grid view.
- Relationship to the dedupe review queue — are embedding-similarity results
  ever surfaced as *potential duplicates* (broader net than perceptual hash),
  or kept strictly separate as a "similar, not duplicate" concept so the two
  systems don't confuse each other.
- Whether text-to-image search (typing a query, scored against the same
  embedding space CLIP/SigLIP models support natively) is in scope alongside
  image-to-image search.

## Resolution
