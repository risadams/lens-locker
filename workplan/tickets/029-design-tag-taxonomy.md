---
id: 029
title: "Design tag/category taxonomy (open-vocabulary zero-shot vs. fixed list)"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-20)
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

Resolved 2026-07-20 via a grilling session, building directly on [ticket
024](024-decide-tagging-model-runtime.md)'s zero-shot mechanism decision.
Four sub-decisions:

**1. Label set: hybrid — a curated starter list, zero-shot-scored
automatically, freely user-extensible.** Pure fixed taxonomy would waste
zero-shot's actual advantage (a new label costs one text-embed, no
retraining); pure fully-open with no starter list creates a cold-start
problem (a fresh install shows zero auto-tags until the user thinks to type
something). A curated starter list (a few dozen to ~100 common photo-content
labels) gets useful tags with zero configuration; custom labels layer on
top via the same mechanism. **Adding a custom label is a cheap backfill**,
not a re-embed: score the one new label against every existing image's
already-stored embedding — reserve this as an explicit operation for
[ticket 035](035-draft-ml-schema.md), not something discovered later.

**2. Auto-tags and manual tags share one `tags` identity table; provenance
lives on the `image_tags` join row, not the tag itself.** A manually-typed
"beach" and a model-generated "beach" collapse into one canonical tag
whose filter/count/search covers both origins — fragmenting them into
parallel concepts would be pure downside, and keeping one `tags` table
means every already-built tag surface (filter popover, `list_tags` counts,
FTS sync, XMP sidecar export) keeps working unmodified for auto-tags with
zero new query surface. What changes: `image_tags` gains a `source`
(`manual`/`auto`) column and a nullable `confidence` column — the same
"one relationship, provenance attributes on the join" shape this schema
already uses (`thumbnails.variant`), not a table-per-origin split.
Provenance living at the join-row level (not tag-identity level) is what
lets re-scoring add/remove `source='auto'` rows freely without ever
touching a `source='manual'` row of the same name. **Flagged forward, not
settled here**: whether a third state (`rejected`, distinct from
present/absent) is needed so re-scoring doesn't silently reapply a tag the
user explicitly dismissed — [ticket
033](033-design-review-correction-ux.md)'s call on the actual
accept/reject mechanism; [ticket 035](035-draft-ml-schema.md) should
reserve schema room for it regardless of how 033 resolves the UX.

**3. Two confidence thresholds (storage floor + display floor), binary
chip by default, provenance still visually distinguishable.** A single
image scores against every label in the set, most near-zero — a **storage
floor** keeps `image_tags` from filling with noise; a separate, higher
**display floor** decides what surfaces as a visible chip by default,
leaving room between the two floors for [ticket
033](033-design-review-correction-ux.md) to use if it wants a "maybe" tier.
Default display is a plain chip, not a visible percentage — raw ML
confidence is internals-leakage for someone organizing personal photos, not
useful information for that task — but auto-provenance should be visually
distinguishable somehow (not identical to a manual chip), both so users
understand "the app suggested this" and so 033's review flow has something
concrete to target ("auto tags not yet reviewed"). The confidence number
itself stays stored and inspectable (tooltip/drawer), just not the primary
display. Exact visual treatment is 033's call.

**4. No category/tag distinction — flat tag space regardless of
specificity.** "Landscape" (broad) and "beach"/"sunset" (specific) are just
different entries in the same starter list from #1, scored and stored
identically per #2/#3. A real category/tag hierarchy is ongoing structural
complexity (a taxonomy to design, populate, let users edit) that nothing in
this ticket or ML-MAP.md's fog items has actually called for yet — matches
this ML effort's consistent pattern of not building structure ahead of a
proven need (SigLIP2, HNSW both deferred the same way). If [ticket
031](031-design-smart-albums-ux.md) later finds it genuinely wants
grouping/hierarchy for browsing, that's a presentation-layer addition on
top of a flat tag space, not a schema-level split decided now.

**Unblocks** [ticket 031](031-design-smart-albums-ux.md) (smart albums,
given the flat tag space from #4 and confidence data from #3 to filter/sort
on), [ticket 033](033-design-review-correction-ux.md) (review/correction
UX, given the source/confidence schema shape from #2/#3 to design accept/
reject/rename against), and [ticket
034](034-design-similarity-search-ux.md) (similarity search, given the
zero-shot label-set mechanism confirmed unchanged from ticket 024).
