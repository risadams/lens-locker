---
id: 034
title: "Design similarity-search UX and its overlap with the existing dedupe review queue"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-20)
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

Resolved 2026-07-20 via a grilling session. Four sub-decisions:

**1. Entry point: a "Find Similar" action in the per-image detail drawer,
always starting from a library-resident image.** LensLocker's whole
interaction model operates on images already in the library (import is a
destructive move; nothing else in the app has an "external file" concept)
— scoping to "start from a photo you're already looking at" stays
consistent rather than introducing a new external-file-picker flow. No
separate results screen: per [ticket 031](031-design-smart-albums-ux.md)'s
exact pattern, results are the same grid component, populated by a
similarity query instead of a filter query.

**2. Same grid, a new similarity sort order, minimum-similarity floor, no
raw scores shown.** A new `SortOrder::SimilarityDesc` alongside the
existing variants in `crates/catalog` reuses the same query/render
pipeline as every other grid view. A minimum-similarity floor (same shape
as [ticket 029](029-design-tag-taxonomy.md)'s tag storage floor) keeps
results meaningfully bounded rather than ranking the whole library down to
irrelevance. Raw percentage scores stay hidden by default, matching 029's
tag-confidence display decision — internals-leakage, not useful for
someone browsing their own photos.

**3. Kept strictly separate from the dedupe review queue — never
surfaced as potential duplicates.** [Ticket 011](011-dedupe-ux.md)'s
perceptual-hash dedupe queue is tight and meaningful precisely because it's
tuned to near-*identical* images; embedding similarity is a fundamentally
broader, fuzzier net (two different sunset photos score highly similar
despite being clearly non-duplicate) that would flood the dedupe queue with
noise and risk the exact "training the user to dismiss the queue" failure
mode ticket 011 already worried about for RAW+JPEG pairs. The two features
answer different questions — dedupe review is "should I delete one of
these," similarity search is "show me more like this" (browsing, not
cleanup) — conflating them would confuse both. A broader dedupe net, if
ever wanted, is a deliberate future reconsideration of ticket 011's own
threshold, not something this feature backdoors in.

**4. Text-to-image search: in scope, additive to existing keyword search,
not a replacement.** SigLIP's text encoder is already required for
zero-shot tagging (tickets 024/029); scoring an arbitrary typed phrase
against image embeddings is the same mechanism evaluated live instead of
stored, effectively free given the model is already loaded for this exact
comparison. Doesn't replace the existing FTS keyword search in
`ui/main.js` (precise/fast for exact known-word lookups); semantic text
search serves a fuzzier, different intent. Exact blending mechanics (mode
toggle vs. merged results) left as a UI-detail choice for build time.

**This closes the design phase of the ML work-plan.** Every ticket
blocking [ticket 035](035-draft-ml-schema.md) (025, 028, 029, 031, 033,
034) is now closed. **Feeds forward into** ticket 035 (needs the
`SortOrder::SimilarityDesc` variant, the minimum-similarity floor as a
concrete value to pin, and confirmation that no new dedupe-adjacent schema
surface is needed here) and [ticket
036](036-assemble-ml-spec.md) (assembly).
