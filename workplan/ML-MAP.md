---
label: workplan:map
title: "LensLocker ML — route to a locked spec for tagging, categorization, and face recognition"
created: 2026-07-20
tracker: TRACKER.md
---

# LensLocker ML work-plan map

## Destination — REACHED (2026-07-20)

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

**→ [ML-SPEC.md](ML-SPEC.md) is the locked artifact.** Every ticket on this
map is closed. Building LensLocker's ML effort is no longer a work-plan
session — it's a build session working from the spec.

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

- [Survey redistributable face detection/embedding model alternatives to InsightFace](tickets/023-research-face-model-licensing.md) — clean pair found: YuNet (MIT) detection + SFace (Apache-2.0, 128-dim) embedding, both ONNX, both shipping in digiKam; InsightFace confirmed a redistribution dead end.
- [Legal/ethics review: face recognition on personal photos + model-card disclaimers](tickets/026-legal-review-face-recognition.md) — both concerns dissolve (CLIP/LAION "deployed use" disclaimer is non-binding ethics guidance outside LensLocker's actual use; face recognition on non-owner photos falls under the personal/household-activity carve-out most privacy regimes exempt), each via a different mechanism than GPL-3.0/HEVC's distribution-trigger dissolution — documented a general guardrail that field-of-use restrictions (e.g. InsightFace's "research-only") don't dissolve the same way.
- [Re-benchmark sqlite-vec at 100k+ scale on real hardware](tickets/025-rebenchmark-sqlite-vec.md) — **fails** the ~200ms interactive bar on real hardware: 352.8ms median / 376.7ms p95 on-disk (vs. sqlite-vec's own ≤75ms published figure), localized to on-disk file I/O rather than the brute-force math itself (in-memory hits 94.5ms median); feeds a real requirement, not a documented contingency, into ticket 024's runtime decision for `simsimd`/`hnsw_rs`.
- [Design face clustering/grouping UX](tickets/028-design-face-clustering-ux.md) — opt-in "People" view (mirrors the dedupe review-queue badge pattern); automation line sits at *naming*, not clustering, with a three-tier auto-attribute/review/no-match split for matching against already-named people (mirrors ticket 011's exact/perceptual/no-match structure); clusters never auto-pruned (recoverability principle), sorted by photo-count with a reversible Hide action; split needs a new multi-select-on-face-thumbnails primitive, merge reuses dedupe's union-on-confirm shape; photos show a "People in this photo" chip list, no bounding-box overlay.
- [Decide the tagging/categorization model & runtime for the ML effort](tickets/024-decide-tagging-model-runtime.md) — `sqlite-vec` kept but queried via an in-memory `vec0` mirror (not the on-disk table) to fix ticket 025's real-hardware failure; tagging checkpoint is SigLIP `so400m`, self-converted to ONNX (not Immich's export, not CLIP/OpenCLIP, not SigLIP2); one shared `ort`/DirectML environment with separate sessions per model (SigLIP/YuNet/SFace); zero-shot tagging confirmed as the categorization mechanism.
- [Design tag/category taxonomy](tickets/029-design-tag-taxonomy.md) — hybrid label set (curated starter list + user-extensible); auto-tags share the existing `tags` table with manual tags, provenance/confidence on the `image_tags` join row not the tag itself; two confidence thresholds (storage floor + display floor), binary chip by default; no category/tag hierarchy — flat tag space regardless of specificity.
- [Decide the background execution model](tickets/030-decide-background-execution-model.md) — decoupled automatic background backlog queue, not import-time or a manual button; per-image (not per-run) connection-lock acquisition to avoid repeating `ImportLock`'s documented mutex-hang bug; separate `AnalysisLock`/`analysis-progress` reusing import's plumbing but an ambient badge instead of a modal; model-upgrade re-analysis is prompted, old embeddings survive until replaced.
- [Design the model-provisioning/installer story](tickets/032-design-model-provisioning.md) — bundled directly in the NSIS installer at install time (Program Files, read-only at runtime), no separate extraction step; installer-size policy committed (quantize before reopening checkpoint choice) but the real DirectML DLL / SigLIP so400m sizes are flagged unconfirmed, not guessed at; license text is static app content (new About/Licenses UI needed), not a database concern; versioning story confirms tickets 030/016's mechanisms already compose without anything new.
- [Design auto-tag/face review & correction UX](tickets/033-design-review-correction-ux.md) — auto-tags get a `review_state` (unreviewed/confirmed/rejected) alongside source/confidence; rejection persists across re-scoring, confirming grants visual parity with manual tags without rewriting provenance; face correction reuses 028's two-entry-point shape (drawer chip + People view); bulk tag correction reuses 028's planned grid multi-select primitive rather than a second mechanism.
- [Design smart albums / saved-search UX](tickets/031-design-smart-albums-ux.md) — always live (never a static snapshot, which is flagged as a distinct future manual-collection feature); no separate query-builder, just "Save as Smart Album" on the grid's existing filter bar plus a new Person facet; same virtualized grid component pre-loaded with saved filters; just a name + serialized filter state, no image-membership table.
- [Design similarity-search UX](tickets/034-design-similarity-search-ux.md) — "Find Similar" from the detail drawer, same grid with a new similarity sort order and a minimum-similarity floor, no raw scores shown; kept strictly separate from the dedupe review queue (different questions, would flood it with noise); text-to-image search in scope as an additive companion to existing keyword search, effectively free given SigLIP's text encoder is already loaded for zero-shot tagging. Closes the ML work-plan's design phase — every blocker on ticket 035 is now closed.
- [Draft the ML schema extensions](tickets/035-draft-ml-schema.md) — full DDL prototype in `workplan/prototypes/035-ml-schema.md`: tag provenance/review-state on `image_tags` + a separate `rejected_tags` table (`tag_scores` dropped, superseded); new `persons`/`face_clusters`/`face_detections`/`face_match_review_queue` tables (the last mirroring `dedupe_review_queue`'s shape); three tunable face thresholds (one sourced, two flagged placeholders); `saved_albums` as one JSON filter blob; `sqlite-vec`'s vec0 mirror and `embeddings`/`models` need no schema change. Last blocker on ticket 036 (final assembly) is now closed.
- [Assemble the locked ML-SPEC.md](tickets/036-assemble-ml-spec.md) — **destination reached.** [ML-SPEC.md](ML-SPEC.md) locked: 13-section spec + 6-milestone build plan, every section citing its deciding ticket. One new judgment call surfaced during assembly and confirmed: ONNX Runtime's own telemetry surface needs the same explicit disable-and-verify treatment `SPEC.md` §8 already gave WebView2.

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
