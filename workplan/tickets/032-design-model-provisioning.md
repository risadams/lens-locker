---
id: 032
title: "Design the model-provisioning/installer story (bundling face+tagging weights, size budget)"
type: workplan:grilling
status: open
assignee:
blocked-by: [023, 024]
---

## Question

[Ticket 016's research](016-research-offline-autotagging.md) found no direct
precedent to copy: Immich fetches models from Hugging Face on first use
(assumes at least one network-connected run), which conflicts with
LensLocker's zero-network-ever standing constraint. With the tagging model
([ticket 024](024-decide-tagging-model-runtime.md)) and face model ([ticket
023](023-research-face-model-licensing.md)) both chosen, design how their
weights actually get onto the user's machine:

- Bundled directly in the NSIS installer (per the existing installer,
  referenced in `docs/verify-wfp-audit.ps1`/Milestone 6 work) vs. a separate
  one-time local extraction step.
- Total installer size budget — sum of the ONNX Runtime DLL (~12MB CPU-only
  per ticket 016, DirectML variant unconfirmed), the tagging model weights,
  and the face model weights (size depends on ticket 023's finding).
  Confirm this doesn't blow past whatever's a reasonable installer size for
  a "personal desktop app."
- License/provenance field for bundled weights (ticket 016 already flagged
  this as a schema-reservation requirement) — where the actual license text/
  attribution gets shipped (installer, about screen, docs folder).
- Versioning story — what happens on an app update that bumps a model
  version (ties into [ticket 030's](030-decide-background-execution-model.md)
  re-analysis-trigger question).

## Resolution
