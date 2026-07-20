---
id: 024
title: "Decide the tagging/categorization model & runtime for the ML effort"
type: workplan:grilling
status: open
assignee:
blocked-by: []
---

## Question

[Ticket 016's research](016-research-offline-autotagging.md) recommended `ort`
(ONNX Runtime, DirectML feature) + a SigLIP ONNX export + `sqlite-vec`
brute-force search, for the narrower MVP-era "auto-tagging + similarity
search" sketch (`SPEC.md` §11). This ticket makes that an actual **decision**
for the wider ML effort (tagging + categorization + smart albums, and
whatever the face-recognition runtime turns out to need per [ticket
023](023-research-face-model-licensing.md)), not just a research finding:

- Ratify the `ort`+DirectML+SigLIP+`sqlite-vec` stack, or revisit it given
  ~1 year has passed since that research (2026-07-17) — re-check nothing's
  gone stale (`ort` version, SigLIP export availability, `sqlite-vec`
  maturity).
- Pick the **specific SigLIP/CLIP checkpoint** (size/quality/license
  tradeoff — ticket 016's table has candidates) rather than leaving it as
  "a SigLIP export."
- Decide whether the face-recognition runtime (once 023 lands) should share
  this same `ort` session/runtime or run as a separate inference path — a
  single shared runtime is the likely default but confirm it isn't blocked
  by something 023 turns up (e.g. a face model only available in a
  non-ONNX format).
- Confirm the **open-vocabulary zero-shot tagging approach** (score image
  embeddings against a label set) is still the intended mechanism for
  "categorization," vs. e.g. a fixed fine-tuned classifier — this feeds
  directly into [Design tag/category
  taxonomy](029-design-tag-taxonomy.md).

## Resolution
