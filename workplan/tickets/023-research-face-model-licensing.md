---
id: 023
title: "Survey redistributable face detection/embedding model alternatives to InsightFace"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-20)
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

## Resolution

Resolved 2026-07-20; full findings with primary-source citations in
[the findings file](../research/face-recognition-models.md). A clean,
fully-redistributable, ONNX face pipeline exists and does **not** require
InsightFace. Key facts for [Design face clustering/grouping UX](028-design-face-clustering-ux.md),
[Decide the background execution model](030-decide-background-execution-model.md),
and [Design the model-provisioning/installer story](032-design-model-provisioning.md):

- **Recommended clean pair: YuNet (detection) + SFace (embedding)**, both
  from the OpenCV Zoo, both native ONNX (drop-in for the `ort`+DirectML
  runtime from ticket 016), and both already the *shipping default* face
  pipeline in **digiKam** — a mature, fully-offline, single-binary desktop
  photo manager (the missing precedent that matches LensLocker's shape and
  avoids InsightFace, unlike Immich/PhotoPrism).
- **YuNet — MIT on both code AND weights.** ~233 KB ONNX, 5 landmarks,
  WIDER-Face AP 0.884/0.866/0.750. Negligible installer budget.
- **SFace — Apache-2.0 on both code AND weights.** MobileFaceNet, **128-dim**
  embedding (cosine-comparable; LFW-tuned verification threshold 0.363),
  LFW 99.60%, ~36.9 MB fp32 ONNX (int8 variant smaller). 128-dim is cheaper
  for `sqlite-vec` scan at 100k+ than the 512-dim ArcFace/FaceNet vectors.
- **Installer budget for the face pair ≈ 37 MB** (SFace-dominated), before
  the tagging model + ONNX Runtime DLL. Ship MIT (YuNet) + Apache-2.0 (SFace)
  attribution/NOTICE text. digiKam self-hosts its ONNX (no Hugging Face
  fetch) — the packaging pattern to copy for bundle-in-installer.
- **InsightFace is a confirmed dead end for redistribution.** Its *code* is
  MIT, but *every* model it ships (buffalo/antelope, its SCRFD detector
  weights, its ArcFace embedding weights, auto- and manual-download alike) is
  "non-commercial research purposes only" — a blanket policy, no cleaner
  family checkpoint. Retraining from its (Apache-2.0/MIT) SCRFD/ArcFace
  *architecture* is technically possible but redundant, because YuNet and
  SFace already are exactly that (independently-trained permissive weights).
  Do not plan around InsightFace or an InsightFace-architecture training run.
- **Backups if SFace/YuNet fall short:** MediaPipe BlazeFace (Apache-2.0,
  needs an unofficial ONNX conversion) for detection; facenet-pytorch (MIT
  code, MIT-lineage 512-dim weights, `torch.onnx`-exportable) for embedding.
  **Avoid Ultralytics YOLO-face — AGPL-3.0 weights** (copyleft trap).
- **One item for [the legal/ethics pass](026-legal-review-face-recognition.md),
  not a blocker:** the clean *weights* licenses are the publisher's grant; the
  underlying *training data* (CASIA-WebFace / VGGFace2 — research-only) is a
  provenance chain to document, structurally identical to ticket 016's LAION
  note. Dissolves entirely under LensLocker's never-distributed posture.
