---
label: workplan:map
title: "LensLocker ML — route to a locked spec for tagging, categorization, and face recognition"
created: 2026-07-20
tracker: TRACKER.md
---

# LensLocker ML work-plan map

## Destination

A locked technical spec + milestone build plan, in a **standalone
`workplan/ML-SPEC.md`**, for LensLocker's ML feature set: automatic tagging
and categorization from image embeddings (SigLIP/CLIP-family), smart
albums/saved searches built on those tags and categories, similarity search,
and face auto-recognition (detection, grouping, naming) — including a
resolved, redistributable licensing path for the face model(s), since
InsightFace (the precedent every other local-first photo app uses) is not
freely redistributable. Every architectural and product decision resolved;
a build session can execute the plan without needing to decide anything —
same bar [the MVP map](MAP.md) reached for `SPEC.md`.

## Notes

- This is a **new effort**, not a reopening of [the MVP map](MAP.md) — that
  map's destination was reached 2026-07-17 and is closed. It left "ML
  auto-tagging design" in its own Not yet specified section; this map is
  where that fog graduates, broadened at kickoff (2026-07-20) to also cover
  categorization/smart-albums and face auto-recognition, which the MVP's
  Milestone 7 sketch (`SPEC.md` §11) explicitly excluded.
- **Prior art, read before re-deriving anything**: [ticket 016's
  research](tickets/016-research-offline-autotagging.md) already surveyed
  runtimes/models/storage for tagging (findings:
  `workplan/research/offline-autotagging.md`) — recommended stack `ort`
  (DirectML) + SigLIP ONNX + `sqlite-vec`, with the schema-reservation
  requirements it found. It also already flagged the InsightFace licensing
  problem for faces. Tickets here should ratify/extend that work, not repeat
  the survey.
- Domain: local-first desktop software, Rust, on-device ML inference.
  Resolve grilling tickets via `ink-and-agency:grill-me` (one question at a
  time, recommendation first) — this install has no separate
  `domain-modeling` skill, so grilling alone covers what the work-plan
  skill's default instructions split across both.
- Standing constraints, inherited unchanged from the MVP map: **zero network
  access** (including at model-inference time — all weights bundled, no
  per-user download); **quality/accuracy never silently degraded** to save
  space or compute; scale target **100k+ images**; **personal/internal
  project, never distributed outside the owner** (this is *why* the
  redistribution-license questions below are answerable at all — same
  dissolution logic as the MVP map's GPL-3.0/HEVC questions, reopen both if
  distribution scope ever changes).
- Plan, don't do: this map produces decisions and `ML-SPEC.md`.
  Implementation happens after the assembly ticket closes.
- Tracker conventions: see [TRACKER.md](TRACKER.md) — shared with the MVP
  map; ticket ids continue that sequence (023+). Open tickets are found by
  query (frontier = open, unassigned, blockers all closed), not listed here.

## Decisions so far

## Not yet specified

- **Active cross-import face re-identification** — once a person is named
  from a face cluster, whether/how future imports auto-suggest that same
  person. Sharpens after [Design face clustering/grouping
  UX](tickets/028-design-face-clustering-ux.md) lands; may turn out to be
  free from the clustering design or may need its own ticket.
- **Multi-person-per-image tagging depth** — how many faces/tags per image
  the UI surfaces before it gets noisy. Sharpens alongside the review/
  correction UX ticket.

## Out of scope

- Everything already out of scope on [the MVP map](MAP.md): network-touching
  features, video file management, non-Windows packaging. This effort
  inherits those unchanged.
- **Video/face recognition in video** — this destination is images only,
  same as the MVP.
