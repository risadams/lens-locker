---
id: 028
title: "Design face clustering/grouping UX (detect, group, name, merge, split)"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-20)
blocked-by: [023, 026]
---

## Question

Once a face detection + embedding model is chosen ([ticket
023](023-research-face-model-licensing.md)) and the legal/ethics pass
([ticket 026](026-legal-review-face-recognition.md)) has landed, design the
actual face auto-recognition user experience:

- Detection happens automatically at import/background time — do unnamed
  face clusters surface in the UI proactively, or only when the user opts
  into a "People" view?
- Clustering: automatic grouping of similar face embeddings into unnamed
  clusters — what's the review/confirm flow before a cluster is "real"
  (per [ticket 011's dedupe-UX
  precedent](011-dedupe-ux.md), never auto-merge silently for anything
  ambiguous)?
- Naming: how a user assigns a name/identity to a cluster; whether unnamed
  clusters persist indefinitely or get pruned.
- Merge/split: correcting a wrong grouping (two people merged into one
  cluster, or one person split across two) — this is expected to happen
  often with any local face model, so the correction flow is load-bearing,
  not an edge case.
- Multiple faces per image — how they're presented (e.g. bounding-box
  overlay vs. a simple "people in this photo" tag list).

## Resolution

Resolved 2026-07-20 via a grilling session. Five sub-decisions, all mirroring
already-established LensLocker UX precedent rather than inventing new shapes
where an existing one transposes cleanly.

**1. Surfacing: opt-in "People" view, not proactive interruption.** Same
shape as the existing dedupe review queue (`ui/index.html`'s `rail-btn` +
hidden-until-nonzero `rail-badge` pattern): a People nav rail button with a
badge count, no toast/interrupt when background detection finds new faces.
Face clustering will produce a lot of low-value noise in a real personal
library (blurry faces, partial profiles, strangers in the background) —
proactively pushing that at the user would be naggy and inconsistent with
import's own non-interruptive progress-bar-only behavior. The People view's
intro line (mirroring the review queue's own explanatory paragraph)
doubles as first-open onboarding, since unlike dedupe this is an
open-ended, ongoing background process rather than a bounded queue.

**2. Clustering automation line sits at *naming*, not at *clustering
itself*** — the real transposition of ticket 011's "automation stops
exactly where ambiguity starts," not a literal copy, since face embeddings
have no zero-ambiguity tier the way BLAKE3 exact-hash does.
- Grouping raw detections into **unnamed, provisional clusters** runs
  silently and automatically — it asserts no real-world identity claim, so
  there's nothing to review yet.
- **Naming a cluster is the confirmation gate** — the one act that turns
  "some grouped faces" into an actual person, mirroring dedupe's
  confirm-to-merge gate.
- Matching a **new** face against an **already-named** person gets its own
  three-tier split, directly mirroring ticket 011's exact/perceptual/no-match
  structure:
  1. High-confidence match → auto-attribute silently (the "exact-match" tier).
  2. Medium-confidence match → human review ("is this also Alice?"), never
     auto-applied — genuine ambiguity, same as the perceptual near-dupe queue.
  3. No plausible match → falls into ordinary unnamed clustering (no
     identity claim at all).
  The auto-attribute (tier 1) threshold should run **stricter** than the
  general unnamed-clustering threshold — misattributing to the *wrong named*
  person is more consequential than two unnamed faces sharing a provisional
  group for a while. Both thresholds are user-tunable (mirroring the
  existing dedupe Hamming-threshold `app_settings` pattern); pinning actual
  default values is left to whichever of [ticket
  033](033-design-review-correction-ux.md)/[ticket
  035](035-draft-ml-schema.md) gets hands-on with real SFace output. A
  **detection-confidence floor** (from YuNet) silently discards unusable
  detections before they ever reach clustering — a quality gate, not an
  ambiguity-review gate.

**3. Naming flow and persistence.** Naming is available both from the
People view (click a cluster, see its member thumbnails + photo count) and
inline from the per-image detail drawer — browsing normally is likely a
common path to naming, not just a dedicated review session. The name field
**autocompletes against already-named people**, so naming the same person
from two different clusters attaches to one identity instead of silently
creating duplicates. **Unnamed clusters are never auto-deleted or
auto-pruned** — this follows directly from LensLocker's standing
recoverability principle; discarding a cluster throws away grouping work
even for a person never named. Clutter is solved via **default sort by
photo-count descending** (real/frequent people surface first, one-off/junk
clusters sink to the bottom), plus an explicit, reversible **"Hide"** action
for a cluster the user never wants resurfaced (moves to a hidden bucket,
never deletes underlying detections/embeddings — mirrors quarantine's
"recoverable, not gone" shape). Quarantine's 30-day retention window does
**not** transfer here: that exists to reclaim real disk space from
duplicate original files, and face clusters (embeddings + rowid refs) carry
no comparable storage pressure.

**4. Merge/split — load-bearing, not an edge case, expected to happen
often.**
- **Split** (a cluster wrongly groups two+ different people): show every
  member face thumbnail (individual crops, not one representative), let the
  user **multi-select** a subset, move them to a new or existing
  cluster/person. This is a **genuinely new UI primitive** — the existing
  image grid (`ui/main.js`) has no bulk-selection today — call this out as
  real implementation cost, not a rename of something already built.
- **Merge** (the same person wrongly split across clusters): reuses
  dedupe's already-locked review-card shape directly (side-by-side, a
  pre-selected suggestion, human can override, confirm produces the
  **union** of both clusters' member faces — exactly like dedupe's
  tag-union-on-merge). A name conflict (both sides already named
  differently) is resolved the same way dedupe resolves a keeper conflict:
  present both, human picks one (or types a third) — never silently choose.
- **Left as fog for [ticket 033](033-design-review-correction-ux.md)**:
  whether the app should *proactively suggest* merge candidates (mirroring
  how the dedupe queue proactively surfaces near-dupes) vs. leaving merge
  to the user noticing duplicates themselves.

**5. Multiple faces per image: a "People in this photo" chip list, reusing
the existing tag-chip UI pattern — no bounding-box overlay.** The usual
argument for overlays (correction precision in a crowded photo) is already
solved by #4's individual face-crop thumbnails in the split/merge flow, so
an overlay would be decoration on an already-solved problem, at real cost
(coordinate-scaling math against a full-size preview viewable at different
zoom levels) that cuts against the frontend's established
plain-HTML/CSS/JS, no-new-primitives-where-avoidable posture. Named faces
in a photo show as chips (clickable to jump to that person's cluster);
unnamed detections in the same photo collapse into a clickable "+N
unidentified" chip, so discoverability doesn't require a separate trip to
the People view.

**Feeds forward into** [Design smart albums / saved-search
UX](031-design-smart-albums-ux.md) (filtering/albums by named person),
[Design auto-tag/face review & correction
UX](033-design-review-correction-ux.md) (proactive merge-suggestion
mechanics, the exact naming interaction, pinning real threshold defaults),
and [Draft the ML schema
extensions](035-draft-ml-schema.md) (needs a `persons` table distinct from
face-cluster rows, per-detection face-crop coordinates, both tunable
thresholds as settings, hidden-cluster state, and headroom for the
cross-import re-identification question ML-MAP.md already flagged as fog).
