---
id: 031
title: "Design smart albums / saved-search UX on top of tags, categories, and faces"
type: workplan:prototype
status: open
assignee:
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
