---
id: 016
title: "Survey offline auto-tagging options"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-17)
blocked-by: []
---

## Question

For the committed post-MVP milestone (offline ML auto-tagging + similarity search),
what are the viable fully-local options from Rust today: runtimes (ONNX Runtime via
`ort`, candle, burn — Windows CPU/GPU story, binary/model size); models
(CLIP/SigLIP/OpenCLIP variants for embeddings + zero-shot tags — quality, size,
**license terms for redistribution in an app**); embedding storage/search at 100k+
images (sqlite-vec, brute-force, HNSW); and precedents from local-first photo apps
(Immich's ML pipeline, PhotoPrism) for what works. This informs the spec's post-MVP
section and the schema's forward-compatibility only — no decision is taken here.

Findings file: [../research/offline-autotagging.md](../research/offline-autotagging.md)

## Resolution

Resolved 2026-07-17; full findings with citations in
[the findings file](../research/offline-autotagging.md). (The research agent's
session ended mid-self-check; the findings file was verified complete by the
orchestrating session.)

- **Recommended starting stack**: `ort` (ONNX Runtime, DirectML feature = the only
  cross-vendor Windows GPU path) + an ONNX SigLIP/CLIP export (Immich publishes
  ready-made ones) + `sqlite-vec` brute-force search inside the existing catalog —
  its published benchmark is ≤75ms/query at 100k×768-dim, adequate with headroom.
  Alternatives: candle (pure Rust, but CPU-only realistically on Windows) and burn
  (wgpu GPU path but models must be ported).
- **Licenses**: SigLIP is the cleanest (Apache-2.0, no disclaimer language);
  OpenCLIP/LAION checkpoints MIT with explicit tags; OpenAI CLIP MIT but with an
  ethics "deployed use out of scope" disclaimer worth a legal read. InsightFace
  face models are NOT freely redistributable — constrains any future face-grouping
  milestone.
- **What the schema must reserve**: per-image embedding with model identity/version
  (dimension varies 512/768 — don't hardcode); zero-shot tag scores separate from
  embeddings; embeddings keyed to BLAKE3 content identity, not path; headroom for a
  sidecar ANN index file; a license/provenance field for bundled weights.
- **Precedent caveats**: Immich validates the models/runtime but not the
  architecture (microservice + Postgres/pgvector, network-fetched models);
  PhotoPrism proves in-process single-binary ML but warns about SQLite at scale —
  re-benchmark sqlite-vec on real hardware before locking the schema.
