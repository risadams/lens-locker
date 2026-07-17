---
id: 018
title: "Assemble the locked spec and build plan"
type: workplan:prototype
status: closed
assignee: chris
blocked-by: [007, 009, 010, 011, 012, 014, 015, 016, 017, 019, 021]
---

## Question

The destination itself: fold every closed decision into a single spec document
(architecture, crate layout, store design, schema, format matrix, conversion
policy, dedupe UX, offline-enforcement checklist) plus a milestone build plan
(MVP milestone slicing, then the ML milestone sketch), each milestone sized for
build sessions to execute without further decisions. Drafted as a prototype the
owner reacts to, then locked. Closing this ticket completes the map.

## Resolution

**Locked**: [SPEC.md](../SPEC.md) — full technical specification (12 sections,
every one citing the ticket that decided it) plus a 7-milestone build plan
(Milestone 0 scaffolding through Milestone 6 the v1 ship gate, Milestone 7 the
post-MVP ML sketch).

Three things weren't decided anywhere on the map and needed a concrete answer for
the spec to be buildable without further decisions — both substantive ones
confirmed as drafted by the owner:
- **Crate layout** (§2 of the spec) — a synthesis of what the closed tickets
  implied, not previously decided; confirmed.
- **Thumbnail defaults** (§9) — 256×256 grid / 1600px preview, WebP, no v1
  eviction; confirmed. This is the one place the still-open "Thumbnail/preview
  cache strategy" fog item got a concrete answer, specifically so this spec could
  lock without leaving Milestone 5 undecided.
- **Milestone slicing** (Part 2) — one reasonable build order, explicitly marked
  reorderable, not individually reviewed.

**This closes the map.** Every ticket that was ever opened is now closed; the
route from the original loose idea to a locked, buildable spec is complete.
Sections 11–12 of the spec carry forward what's deliberately still fog/out of
scope (multi-library, export tooling, store integrity over time, backups, the ML
milestone's own detailed design) — those remain real, just beyond this
destination, exactly as the map's Not yet specified / Out of scope sections
always said they were.
