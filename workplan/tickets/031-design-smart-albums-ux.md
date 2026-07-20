---
id: 031
title: "Design smart albums / saved-search UX on top of tags, categories, and faces"
type: workplan:prototype
status: closed
assignee: chris (grilling session 2026-07-20)
blocked-by: [028, 029]
---

## Question

The old MVP map flagged "saved searches/smart albums" as a post-MVP organizer
feature, not yet specified; this effort pulls it in because it consumes
exactly the tags/categories ([ticket 029](029-design-tag-taxonomy.md)) and
face identities ([ticket 028](028-design-face-clustering-ux.md)) this map
produces. Prototype (via `/prototype`) the actual UX:

- What a "smart album" is — a saved filter query (e.g. "tag:beach AND
  person:Alex") that live-updates as new matching images are imported, vs. a
  static snapshot.
- Filter/query builder UI — how a user combines tags, categories, faces,
  dates, and existing metadata (per [ticket
  012](012-metadata-portability.md)) into a saved search.
- Where smart albums live relative to the existing grid view (per [the
  thumbnail-grid performance
  validation](019-validate-thumbnail-grid-performance.md)) — a filtered view
  of the same grid, presumably, but confirm.
- Whether smart albums are purely a view (no data written) or create any
  persistent state beyond the saved query itself.

## Resolution

Resolved 2026-07-20 via a grilling session (text-only, no separate mockup —
same call as [ticket 033](033-design-review-correction-ux.md)). Four
sub-decisions:

**1. Always live, never a static snapshot.** Matches the ticket's own
framing ("live-updates as new matching images are imported"). A static
snapshot is a genuinely different feature — a fixed curated set regardless
of future imports — flagged as a distinct, not-yet-mapped manual-album/
collection feature, not conflated with smart albums here.

**2. No separate query-builder screen — "Save as Smart Album" on the
grid's existing filter bar.** The grid already has real filter UI
(formats/sources/tags popovers, date range, search) from Milestone 5;
building a standalone builder would duplicate it for no benefit, matching
this project's consistent preference for reusing existing UI patterns over
parallel ones. Since [ticket 029](029-design-tag-taxonomy.md) already made
categories *be* tags (flat space, no hierarchy), "the query builder" is
really: add a Person facet (from [ticket
028](028-design-face-clustering-ux.md)) to the existing filter system, then
let any combination of existing facets be named and saved.

**3. Same virtualized grid component, pre-loaded with a saved filter
state.** No new rendering surface — reuses [ticket
019](019-validate-thumbnail-grid-performance.md)'s already-validated
100k-item grid directly. Saved albums get their own nav entry (alongside
the People/review-queue rail buttons); opening one shows the same grid with
that album's filters pre-applied and editable.

**4. Just a name + serialized filter/search/sort parameters — no
image-membership table.** Since the album is always re-evaluated live
(#1), there's no fixed membership set to track; an `album_images`-style
join table would actively misrepresent the feature. **Flagged for [ticket
035](035-draft-ml-schema.md)**: if the static-snapshot manual-collection
feature from #1 is ever built later, that one *would* need a real
membership table — the two concepts should not share one schema shape.

**Feeds forward into** [ticket 035](035-draft-ml-schema.md) (needs a
saved-album table: name + serialized filter state, no membership table)
and confirms [ticket 034](034-design-similarity-search-ux.md) can reuse the
same filter-facet/grid-reuse pattern if its similarity-search UX turns out
to want one.
