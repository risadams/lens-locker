---
id: 024
title: "Decide the tagging/categorization model & runtime for the ML effort"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-20)
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

Resolved 2026-07-20 via a grilling session, informed directly by [ticket
023](023-research-face-model-licensing.md)'s closed face-model research and
[ticket 025](025-rebenchmark-sqlite-vec.md)'s closed real-hardware benchmark.
Four sub-decisions:

**1. `sqlite-vec`'s real-hardware failure — fixed architecturally, not by
swapping tools.** Ticket 025 measured a real on-disk brute-force query at
352.8ms median / 376.7ms p95 at 100k×768-dim — a clear fail against the
~200ms interactive bar — but its own diagnostic in-memory run hit 94.5ms
median, isolating the shortfall to on-disk file I/O, not the brute-force
distance math `sqlite-vec` is built to do fast. **Decision: keep
`sqlite-vec`, but query an in-memory `vec0` mirror rather than the on-disk
table.** The on-disk table stays the durable source of truth; at
library-open, its rows load into an in-memory `vec0` mirror (matching the
scale of the existing launch-time passes — `sweep_expired_quarantine`,
`backfill_missing_grid_thumbnails` — that already run once per library-open
in `src-tauri/src/lib.rs`); new embeddings write to both. 100k×768-dim is
~293MB of raw floats — trivial next to the ~365–591MB WebView2 footprint
[ticket 019](019-validate-thumbnail-grid-performance.md) already measured
as this app's baseline resource cost. `simsimd` and `hnsw_rs` (both
surveyed in [ticket 016's research](../research/offline-autotagging.md) §3)
are the **documented fallback**, not adopted now — revisit if the library
grows meaningfully past 100k, or if the real combined RAM budget (tagging +
face vectors together) proves tighter than this estimate once [ticket
035](035-draft-ml-schema.md) sizes it for real.

**2. Checkpoint: SigLIP `so400m`, self-converted to ONNX from the original
Google checkpoint — not CLIP/OpenCLIP, and not SigLIP2.** SigLIP over
CLIP/OpenCLIP because it's the only family with **no ethics-disclaimer
language on its model card at all** (CLIP/OpenCLIP's "deployed use always
out-of-scope for surveillance/facial-recognition" language is resolved per
[ticket 026](026-legal-review-face-recognition.md), but there's no reason
to keep re-litigating it when SigLIP avoids the question entirely).
SigLIP1 over SigLIP2 specifically because SigLIP1's Apache-2.0 grant is
independently confirmed via the HF API `license` field, while SigLIP2's was
only pattern-matched as "likely" from the same org, not verified per
checkpoint — this ticket-chain has consistently preferred confirmed-clean
over higher-accuracy-but-unverified (the same logic that picked SFace over
ArcFace/AdaFace). `so400m` over `base` for meaningfully better zero-shot
quality (self-reported 84.5% zero-shot ImageNet-1k), justified because
LensLocker is a one-time installer-bundled desktop app, not a
size-constrained mobile target.
**Self-converted, not Immich's pre-built ONNX export**: a straightforward
ONNX export (no fine-tuning, just re-serializing the same weights) doesn't
create new licensing terms, so Immich's export is almost certainly fine to
bundle too — but this ticket-chain has consistently preferred a
provenance chain traceable directly to the original rights-holder over a
third party's convenience repackaging (again, the YuNet/SFace precedent:
both self-hosted by their own creators), and a standard SigLIP ONNX export
via Hugging Face's `optimum` library is routine, low-cost engineering, not
a research problem. This is now a concrete build-plan item (an export step,
scheduled alongside [ticket 032](032-design-model-provisioning.md)'s
provisioning work), not a licensing risk carried forward unresolved.

**3. Runtime: one shared `ort` environment/DirectML setup, three separate
`Session` objects** (SigLIP tagging, YuNet detection, SFace embedding) —
the natural shape of the `ort` API itself (a lightweight, reusable
environment; each model is a distinct graph needing its own session), and
avoids redundant DirectML device/adapter initialization three separate
environments would cost for no benefit. Checked whether this needs
`lenslocker-raw-worker`-style process isolation (untrusted-input hardening)
— it doesn't: by the time pixels reach any of these three models they've
already passed `lenslocker-decode`'s validation, and ONNX Runtime itself is
a mature, widely-deployed runtime, not a bespoke parser facing untrusted
file bytes. In-process, run via `tauri::async_runtime::spawn_blocking`
matching the existing CPU-heavy-work pattern (`import_directory`,
`get_full_preview`). **Scheduling** (whether tagging and face-detection run
in the same background pass over a decoded image, or on separate
triggers/cadences) is explicitly left to [ticket
030](030-decide-background-execution-model.md) — this only settles the
runtime/session architecture, not when things run.

**4. Zero-shot tagging confirmed**, no change from ticket 016's framing.
Ticket 016's own schema-reservation language ("auto-tags are a derived,
re-runnable projection of the embedding against a label set, not raw model
output") already assumed this; there was never a seriously-considered
fixed-classifier alternative to reopen. Justification restated for the
record: open-vocabulary without retraining (a fixed classifier bakes in its
label set at training time; LensLocker has no training-pipeline story at
all, every ML decision so far is inference-only); re-runnable by design
(changing the label set later re-runs the cheap similarity step, not a
re-embed); and this is exactly what unblocks [ticket
029](029-design-tag-taxonomy.md) to design the label-set/taxonomy UX rather
than re-litigate the tagging mechanism itself.

**Unblocks** [ticket 029](029-design-tag-taxonomy.md) (taxonomy UX, given
zero-shot confirmed in #4), [ticket
030](030-decide-background-execution-model.md) (background scheduling,
given both models' runtime shape from #1/#3 — both models are small/
CPU-viable per [ticket 023's research](../research/face-recognition-models.md),
so import-time-vs-background-pass remains a free design choice, not forced
by model cost), and [ticket 032](032-design-model-provisioning.md) (model
provisioning/installer, given #2's self-conversion commitment and the
combined installer-size budget: SigLIP `so400m` + the ~37MB YuNet/SFace
pair + the `onnxruntime.dll` DirectML runtime).
