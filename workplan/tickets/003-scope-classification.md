---
id: 003
title: "Scope classification: metadata MVP, ML later"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: []
---

## Question

The idea says the app helps "classify" images. Does that mean organizing by
extracted metadata, or content understanding via offline ML — and in which
milestone?

## Resolution

**MVP classifies by extracted facts only** — capture date, format, camera EXIF,
dimensions, source folder, manual tags. **Offline ML auto-tagging is in scope as a
committed post-MVP milestone** (e.g. CLIP-style embeddings via a bundled local
runtime, enabling auto-tags and similarity search). The spec must reserve room for
it (schema, pipeline hooks) without letting it into the MVP critical path.
Groundwork: [Survey offline auto-tagging options](016-research-offline-autotagging.md).
