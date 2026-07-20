---
id: 032
title: "Design the model-provisioning/installer story (bundling face+tagging weights, size budget)"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-20)
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

Resolved 2026-07-20 via a grilling session. Four sub-decisions:

**1. Bundled directly in the NSIS installer, extracted at install time —
no separate first-run extraction step.** Zero-network-ever means the
models must be present before any run regardless; there's no benefit to
deferring extraction to first launch. Model files are static, versioned
assets shipped *by* the app (same category as `webview2_hardening`'s
config and the app's icons, which already live in the install directory,
not AppData) — not user data, so `tauri.conf.json`'s `installMode:
perMachine` (Program Files) is exactly right, since these files are only
ever read at runtime, never written to. A model-version-bumping app update
just overwrites the old `.onnx` files in the same install directory via
the normal update mechanism — no special extraction logic, and this is
the exact file-replacement mechanism [ticket
030](030-decide-background-execution-model.md)'s prompted re-analysis flow
already assumes underneath it.

**2. Policy committed; total size deliberately left unconfirmed rather
than fabricated.** Confirmed: the face pair (YuNet+SFace) is ~37MB.
Unconfirmed, flagged explicitly rather than guessed: the DirectML-specific
`onnxruntime.dll` size (ticket 016's research only confirmed the ~12MB
*CPU-only* build) and SigLIP `so400m`'s real on-disk ONNX size (also
explicitly flagged unconfirmed in the research). **Policy**: bundle
everything per #1; if the real confirmed total proves uncomfortably large,
the first lever is int8-quantizing the tagging model (standard ONNX
tooling) before reopening [ticket 024](024-decide-tagging-model-runtime.md)'s
checkpoint choice itself — same escalation order already reserved for
YuNet/SFace's own quantized variants. **Concrete follow-up, not a design
question**: confirm the real DirectML EP DLL size and the real `so400m`
ONNX file size before `ML-SPEC.md` locks an actual installer-size number —
a verification task in the same shape as tickets 019/025's real-hardware
benchmarks, not another grilling round. Soft ceiling (a judgment call, not
a researched fact): "a few hundred MB to low single-digit GB" reads as
acceptable for a personal desktop photo app with AI features.

**3. License/provenance text is static app content, not database
schema.** Two genuinely different things the ticket's phrasing could
conflate, now split: the actual license text/attribution (MIT for YuNet,
Apache-2.0 for SFace and SigLIP — Apache-2.0 requires NOTICE preservation)
ships as a static `LICENSES` file in the install directory plus a simple
About/Licenses screen in the app UI — checking `ui/index.html`, no such
screen exists yet, so this is genuinely new (if small) UI surface, named
rather than assumed free. The *database*-facing "license/provenance field"
[ticket 016](016-research-offline-autotagging.md) flagged as a
schema-reservation requirement is a different thing — already fully
covered by the model-identity/version-per-embedding requirement that
ticket already established and [ticket
030](030-decide-background-execution-model.md)'s version-mismatch
detection relies on. No new schema column for license text; a static
model-name/version → license lookup lives in code/config.

**4. Versioning story: nothing new — confirms #1 and ticket 030 already
compose correctly.** An app update overwrites model files (#1); the
catalog already tracks model version per embedding (ticket 016); the
version-mismatch check on launch triggers ticket 030's already-resolved
prompted-re-analysis flow. Provisioning's only job is ensuring the new file
is actually on disk when that check runs, which #1's install-time
replacement already guarantees.

**Feeds forward into** [ticket 035](035-draft-ml-schema.md) (needs the
model-version-per-embedding field this resolution relies on, already
scoped by ticket 016 — no new schema surface from this ticket itself) and
the eventual `ML-SPEC.md` build plan (needs the real DirectML DLL and
`so400m` size numbers confirmed before its installer-size line is
written).
