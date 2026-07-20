---
id: 030
title: "Decide the background execution model (when tagging/detection runs, GPU/CPU budget, progress UI)"
type: workplan:grilling
status: open
assignee:
blocked-by: [023, 024]
---

## Question

With both the tagging model ([ticket
024](024-decide-tagging-model-runtime.md)) and face model ([ticket
023](023-research-face-model-licensing.md)) chosen, decide how/when
inference actually runs:

- At import time (like hashing/conversion already do) vs. a separate
  background job after import completes vs. user-triggered ("Analyze
  library" button)? Import-time inference risks slowing down the import
  pipeline's exit criteria (Milestones 1-2 were measured on raw
  import/convert speed); a separate background pass avoids that but adds a
  second async system.
- GPU/CPU resource budget — does ML inference compete with normal app
  responsiveness (thumbnail rendering, UI thread) during a large library's
  first analysis pass; any throttling/pause-on-user-activity behavior.
- Progress/cancellation UI — precedent: real import progress + cancellation
  UI already exists (commit `a1b4d42`, "Import progress UI, real
  cancellation, dialog-modal fix, and JIT previews"); does ML analysis reuse
  that pattern or need its own.
- Re-analysis triggers — a model upgrade (new SigLIP checkpoint version)
  needs to re-embed the whole library; is this automatic, prompted, or
  manual-only.

## Resolution
