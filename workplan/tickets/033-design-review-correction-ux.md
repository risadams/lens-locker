---
id: 033
title: "Design auto-tag/face review & correction UX (accept/reject/rename)"
type: workplan:prototype
status: open
assignee:
blocked-by: [028, 029]
---

## Question

No ML tagging or face-grouping system is accurate enough to ship without a
correction path — prototype (via `/prototype`) how a user reviews and fixes
what the model got wrong, now that both taxonomy ([ticket
029](029-design-tag-taxonomy.md)) and face clustering ([ticket
028](028-design-face-clustering-ux.md)) UX are decided:

- Auto-tag correction: remove a wrong auto-tag, confirm/promote a
  low-confidence one, add a missing one — and whether a corrected auto-tag
  becomes indistinguishable from a manual tag afterward (ties to 029's
  provenance-flag question).
- Face correction: covered mostly by 028's merge/split/rename, but confirm
  the *entry point* — where in the normal browsing flow a user notices and
  fixes a wrong face grouping (inline on the grid, a dedicated review queue
  like [the dedupe review
  queue](011-dedupe-ux.md), or both).
- Bulk operations — correcting many images at once (e.g. "these 40 photos
  are actually tagged wrong") vs. one-at-a-time only.

## Resolution
