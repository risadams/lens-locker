---
id: 030
title: "Decide the background execution model (when tagging/detection runs, GPU/CPU budget, progress UI)"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-20)
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

Resolved 2026-07-20 via a grilling session. Four sub-decisions:

**1. Decoupled, automatic background pass — not import-time, not a manual
button.** Import already treats speed as a tracked exit criterion
(Milestones 1-2 measured raw import/convert speed); a per-image neural-net
forward pass is meaningfully slower than the existing hash/decode/convert
steps, so running it inline would regress that measured property.
Automatic, not gated behind an "Analyze library" button, because starting
analysis requires no human judgment call — unlike naming a face cluster or
confirming a merge, there's nothing to review at kickoff, so a button would
just be friction with no safety benefit (the real judgment calls are
already covered by tickets 028/033). Concretely: an "images needing
analysis" backlog tracked in the catalog (rows with no embedding yet),
worked through continuously in the background whenever the app runs,
resumable across app restarts the same way the import journal already is —
analyzing 100k images will span many sessions.

**2. Low-priority, never blocks interactive commands — a concrete
implementation constraint, not just a preference.** `ImportLock`'s own doc
comment (`src-tauri/src/lib.rs`) documents a real, observed Windows
"Application Hang" from holding `AppState.conn`'s mutex across an entire
long synchronous run — enough piled-up ordinary commands (`list_images`,
tag edits) can stop the app processing IPC entirely. A multi-hour analysis
pass over 100k images is exactly the shape of operation that reproduces
this if it copies import's per-run lock-holding pattern. **Requirement:
analysis acquires/releases the connection lock per-image (or small
batches), never across the whole pass** — the opposite of import's current
pattern, not a copy of it. GPU contention between DirectML inference and
WebView2's own compositing (rendering the thumbnail grid) is flagged, not
fully resolved — needs real empirical testing once built, the same way
tickets 019 and 025 both found measured behavior diverging from
paper-reasoning. Commit for now: deliberately low priority/small batch
sizes, so a bad case degrades to "analysis is slow," not "the UI stutters."

**3. Separate lock/event pair, reusing `ImportLock`'s architectural shape
but not its UI presence.** A new `AnalysisLock` (same shape: atomic
running-flag, cancel-requested flag, RAII guard) and a new
`analysis-progress` event, distinct from import's `ImportLock`/
`import-progress` — import and analysis are legitimately concurrent (new
imports while an older backlog is still analyzing), so sharing one lock
would block one for no reason. UI presence diverges deliberately from
import's modal: import is a user-triggered foreground action, so a modal
progress bar fits; analysis is ambient background maintenance the user
didn't explicitly start, so a modal would be the same nagging interruption
tickets 026/028 already steered away from. Surface it like the People/
review-queue badges instead — an unobtrusive status indicator ("Analyzing:
2,345 / 100,000") with cancel/pause, never demanding attention.

**4. Re-analysis on model upgrade: prompted, not automatic and not
silently manual-only.** Bulk re-embedding 100k images is long and
resource-intensive, happening rarely (only on a shipped model-version
bump) — silently launching a multi-hour GPU-intensive pass right after an
update contradicts this app's standing posture of never surprising the
user with expensive background work (the same instinct behind "never
silently default the library location," applied to compute cost instead of
data risk). But leaving stale embeddings forever with no visibility a
better model exists is also wrong. On detecting a model-version mismatch
(schema already reserves model-version-per-embedding per [ticket
016](../research/offline-autotagging.md)), surface a one-time notice
("newer model available, re-analysis takes ~X, run now or later") — either
choice just adds/doesn't-add a large batch to the same backlog queue from
#1, using the same low-priority execution from #2 and progress surface from
#3. No new mechanism, just a new backlog source plus a heads-up.
**Implementation detail for [ticket 035](035-draft-ml-schema.md)**: the old
embedding for an image must stay in place until the new one is
successfully computed and stored, not deleted first — a canceled/crashed
re-analysis pass must never leave an image worse off (no embedding at all)
than before the upgrade started, the same idempotent-crash-safety principle
`lenslocker-import` already documents for its own pipeline.

**Feeds forward into** [ticket 032](032-design-model-provisioning.md)
(installer/provisioning doesn't change, but the backlog-queue execution
model here is what 032's provisioning UX hands off into on first launch)
and [ticket 035](035-draft-ml-schema.md) (needs the analysis-backlog
tracking mechanism, the model-version-mismatch detection, and the
old-embedding-survives-until-replaced ordering as concrete schema/logic
requirements).
