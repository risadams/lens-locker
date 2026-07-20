---
id: 023
title: "Survey redistributable face detection/embedding model alternatives to InsightFace"
type: workplan:research
status: open
assignee:
blocked-by: []
---

## Question

[Ticket 016's research](016-research-offline-autotagging.md) found that InsightFace
— the face-recognition model every local-first photo-app precedent (Immich,
PhotoPrism) actually uses — is **not freely redistributable**: Immich's own
re-hosted `buffalo_l` checkpoint is licensed for Immich's specific project use
only, explicitly not extending to redistribution by third parties.

For LensLocker's face auto-recognition destination (detection + embedding +
later grouping), what fully-local, redistributable-in-an-installer alternatives
exist today, covering:

- **Face detection** models (locate face bounding boxes/landmarks in an image)
  — e.g. SCRFD variants under permissive terms, MediaPipe/BlazeFace, OpenCV's
  YuNet (BSD), or other ONNX-exportable detectors with a clean license.
- **Face embedding** models (turn a detected face into a comparable vector for
  clustering/grouping) — e.g. permissively-licensed FaceNet-family or ArcFace-
  family re-trainings, or any embedding model whose *weights* (not just code)
  carry a redistribution-safe license (MIT/BSD/Apache-2.0), the same bar
  ticket 016 applied to SigLIP/CLIP.
- For each candidate: license (code AND weights, separately — ticket 016
  found these diverge), accuracy/quality signal if available, ONNX
  exportability (LensLocker's runtime is already `ort`+DirectML per ticket
  016 unless [Decide the tagging/categorization model &
  runtime](024-decide-tagging-model-runtime.md) changes that), and
  approximate weight size (installer budget).
- Whether any viable path exists to use InsightFace itself under a different
  arrangement (e.g. a differently-licensed InsightFace-family checkpoint, or
  training/fine-tuning a redistributable model from InsightFace's published
  *architecture* rather than its restricted weights) — flag if this is a
  dead end so the map can move on cleanly.

This is a hard requirement for the destination, not a nice-to-have: if no
clean option exists, that's a real finding this ticket should surface clearly
(closest-available option + its actual restriction), not paper over.

Findings file: `workplan/research/face-recognition-models.md` (to be created
by the research subagent).
