---
id: 033
title: "Design auto-tag/face review & correction UX (accept/reject/rename)"
type: workplan:prototype
status: closed
assignee: chris (grilling session 2026-07-20)
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

Resolved 2026-07-20 via a grilling session (not a separate mockup artifact —
see the ticket-type note below). Three sub-decisions:

**1. Auto-tag correction: a `review_state` (`unreviewed`/`confirmed`/
`rejected`) alongside the `source`/`confidence` columns [ticket
029](029-design-tag-taxonomy.md) already settled.** Removing a wrong
auto-tag can't just delete the `image_tags` row — the next re-scoring pass
([ticket 030](030-decide-background-execution-model.md)'s backlog) would
silently re-add the same wrong tag if the underlying score doesn't change.
This is the "rejected" third state 029 flagged as fog and deferred here:
rejection persists across re-scoring. Confirming a low-confidence "maybe"
tag (029's band between storage floor and display floor) does **not**
rewrite `source` to `manual` — it genuinely was the model's suggestion, and
honest provenance matters for audit and for not confusing future
re-scoring logic — instead `review_state` flips to `confirmed`. This
refines 029's "auto-tags should be visually distinguishable" answer: that
distinction exists to flag *unreviewed* suggestions specifically; once
confirmed, a tag graduates to full visual parity with a manual chip, while
an unreviewed one keeps the marker. Adding a missing tag is just the
existing manual `add_tag` flow — nothing new, indistinguishable from a
user typing any other tag.

**2. Face correction entry point: both, extending 028's existing
two-entry-point shape rather than inventing a third mechanism.** [Ticket
028](028-design-face-clustering-ux.md) already decided naming works from
both the dedicated People view and the per-photo detail drawer's "People in
this photo" chip list, specifically because incidental correction while
browsing is likely more common than a deliberate review session.
Correction follows the identical shape: the drawer's chip list gets a
lightweight inline action (click a wrongly-named chip → "not \[Name\]" /
reassign) for single-photo fixes; the dedicated People view's cluster view
carries 028's heavier multi-select split/merge machinery for bulk-shaped
fixes.

**3. Bulk auto-tag correction: real requirement, reusing 028's planned
multi-select primitive — not a second from-scratch mechanism.** The
load-bearing case is a model consistently mis-tagging something across
many photos, where one-at-a-time correction would be genuinely painful.
[Ticket 028](028-design-face-clustering-ux.md) already committed to
building grid multi-select (none exists today) for face-cluster splitting;
extending "select multiple images, apply a tag-rejection action to all of
them" once that primitive exists is incremental reuse. **Explicit note for
whoever builds it**: design the multi-select primitive generically enough
to serve both call sites (cluster-thumbnail selection and main-grid
selection), not narrowly scoped to faces alone.

**Ticket-type note**: this ticket is typed `workplan:prototype`, and the
owner was asked whether to also produce a static HTML mockup (in the shape
of `workplan/design/lenslocker-design.html`) versus a text-only
resolution like the preceding grilling tickets — chose text-only for this
pass. No mockup artifact was produced.

**Feeds forward into** [ticket 035](035-draft-ml-schema.md) (needs the
`review_state` column, the rejection-persists-across-rescoring behavior,
and the shared multi-select primitive as concrete schema/logic
requirements) and the eventual `ML-SPEC.md` build plan (a visual mockup of
this flow remains an option to produce later, e.g. during actual UI build
work, per the ticket-type note above).
