# LensLocker ML — Technical Specification & Build Plan

Status: **LOCKED** (2026-07-20). Assembled from every closed ticket on
[the ML work-plan map](ML-MAP.md); each section cites the ticket that
decided it. Standalone from [the MVP `SPEC.md`](SPEC.md) — this supersedes
§11's post-MVP sketch there with a real, buildable design. A build session
should be able to execute the milestones below without deciding anything
further.

## 1. What this effort is

Automatic tagging/categorization from image embeddings, smart albums/saved
searches built on tags and faces, similarity search, and face
auto-recognition (detection, grouping, naming) — for the same single-user,
single-machine, never-distributed LensLocker described in `SPEC.md` §1.
This is a new effort, not a reopening of the MVP map: it graduated from
that map's "ML auto-tagging design" fog item, broadened at kickoff to also
cover categorization/smart-albums and faces, which the MVP's own §11 sketch
explicitly excluded. ([ML-MAP.md](ML-MAP.md))

**Standing constraints, inherited unchanged from `SPEC.md` §1, plus one ML
extension**: zero network access — **including at model-inference time**,
all weights bundled in the installer, no per-user download, ever; quality/
accuracy never silently degraded to save space or compute; scale target
100k+ images; personal/internal project, never distributed outside the
owner (this is *why* the redistribution-license and biometric-processing
questions below are answerable at all — same dissolution logic `SPEC.md`
§1 applies to its own GPL-3.0/HEVC questions, reopen all of them together
if distribution scope ever changes).

## 2. Model & runtime stack

**Tagging**: SigLIP `so400m`, self-converted to ONNX from the original
Google checkpoint (not Immich's pre-built export, not CLIP/OpenCLIP, not
SigLIP2) — chosen over CLIP/OpenCLIP for carrying no ethics-disclaimer
language at all, and over SigLIP2 because SigLIP1's Apache-2.0 grant is
independently confirmed via the HF API `license` field while SigLIP2's was
only pattern-matched, unconfirmed per checkpoint. Self-conversion (via
Hugging Face's `optimum` library) rather than bundling a third party's
convenience export keeps the redistribution provenance chain directly
traceable to the rights-holder. ([024](tickets/024-decide-tagging-model-runtime.md))

**Faces**: **YuNet** (detection, MIT code+weights, ~233KB ONNX) + **SFace**
(embedding, Apache-2.0 code+weights, 128-dim, ~37MB fp32 ONNX) — the OpenCV
Zoo pair, both native ONNX, both already the shipping default face
pipeline in digiKam (the closest real precedent to LensLocker's
single-binary offline-desktop shape). **InsightFace is a confirmed dead
end for redistribution** — every published checkpoint (buffalo/antelope,
SCRFD, ArcFace) is licensed "non-commercial **research** purposes only," a
field-of-use restriction LensLocker's non-commercial-but-not-research
personal use does not satisfy regardless of its never-distributed posture
— do not revisit. ([023](tickets/023-research-face-model-licensing.md),
[../research/face-recognition-models.md](research/face-recognition-models.md))

**Runtime**: one shared `ort` (ONNX Runtime, DirectML feature — the only
cross-vendor Windows GPU path) environment, three separate `Session`
objects (SigLIP, YuNet, SFace) — the natural shape of the `ort` API itself,
avoiding redundant DirectML device initialization. No process isolation
(unlike `lenslocker-raw-worker`'s untrusted-input hardening for RAW
decode) — by the time pixels reach any of these models they've already
passed `lenslocker-decode`'s validation, and ONNX Runtime is a mature,
widely-deployed runtime, not a bespoke parser facing untrusted bytes.
Run via `tauri::async_runtime::spawn_blocking`, matching the existing
CPU-heavy-work pattern (`import_directory`, `get_full_preview`).
([024](tickets/024-decide-tagging-model-runtime.md))

**Categorization mechanism**: open-vocabulary zero-shot — score an image's
embedding against a label set, never a fixed fine-tuned classifier. A
hybrid label set: a curated starter list (a few dozen to ~100 common
photo-content labels), zero-shot-scored automatically, freely
user-extensible — adding a custom label is a cheap backfill (score the one
new label against every existing image's already-stored embedding), not a
re-embed. Text-to-image search (§9) reuses this exact mechanism with an
ad-hoc query string instead of a stored label.
([024](tickets/024-decide-tagging-model-runtime.md),
[029](tickets/029-design-tag-taxonomy.md))

**Embedding search**: `sqlite-vec` (kept, not replaced) — but queried via
an **in-memory `vec0` mirror**, not the on-disk table. A real-hardware
benchmark measured the on-disk brute-force path at 352.8ms median / 376.7ms
p95 at 100k×768-dim — a clear fail against the ~200ms interactive bar —
while an in-memory run of the identical workload hit 94.5ms median,
isolating the shortfall to on-disk file I/O, not the distance math.
100k×768-dim is ~293MB of raw floats, trivial next to the ~365–591MB
WebView2 footprint `SPEC.md` §9 already measured as this app's resource
baseline. The on-disk table stays the durable source of truth; at
library-open, rows load into an in-memory mirror (matching the scale of
existing launch-time passes like `sweep_expired_quarantine`); new
embeddings write to both. `simsimd`/`hnsw_rs` are the documented fallback,
not adopted now — revisit only if the library grows meaningfully past 100k
or the real combined RAM budget proves tighter than estimated.
([025](tickets/025-rebenchmark-sqlite-vec.md),
[024](tickets/024-decide-tagging-model-runtime.md),
[../research/sqlite-vec-100k-benchmark.md](research/sqlite-vec-100k-benchmark.md))

## 3. Legal & ethics posture

Two concerns, both dissolve — through **different mechanisms** than
`SPEC.md` §1's GPL-3.0/HEVC dissolutions, worth being precise about which:

**CLIP/LAION model-card "deployed use" disclaimers** (not applicable to
the chosen SigLIP stack, which carries no such language, but documented
since it was a real question raised) dissolve because the disclaimer is
non-binding ethics guidance, not a license term — MIT/Apache-2.0 grant no
use restriction to inherit — and because LensLocker's actual use (zero-shot
tagging of the owner's own photos) is inside the model's intended purpose,
not the surveillance/third-party-facial-recognition abuse case the
disclaimer targets.

**Face recognition on photos of people who aren't the owner** dissolves via
a **personal/household-activity carve-out**, not a licensing argument.
Privacy frameworks that regulate biometric-like data generally exempt
exactly this activity (GDPR Art. 2(2)(c)/Recital 18 places "purely personal
or household activity" entirely outside its scope, naming photography as
the paradigm case; biometric-specific statutes like Illinois BIPA are
drafted around commercial/business collection from others, not private
undistributed hobby software). Not legal advice — a reasoned, documented
judgment call, not a citation-backed finding.

**General guardrail, worth carrying forward**: the personal/non-commercial/
never-distributed posture cleanly dissolves **distribution-triggered**
obligations (GPL-3.0, HEVC patent exposure) but does **not**
automatically dissolve **field-of-use-triggered** restrictions (InsightFace's
"non-commercial research only" — LensLocker fails the research-use
condition regardless of distribution status). Check what kind of
restriction a license imposes before assuming the standing posture
dissolves it.

**Standing design commitment**: face embeddings, clusters, and person names
never leave the device, never transmitted, stored locally only in the
SQLite catalog — the concrete fact that makes the carve-out reasoning
hold. **Forward-flagged, not resolved here**: if a user later exports a
photo carrying a named-face tag and shares it themselves, that's the
owner's own act (same shape as regular tag export, `SPEC.md` §7) — not
something this app's software transmits automatically.
([026](tickets/026-legal-review-face-recognition.md))

## 4. Tag/category taxonomy

Auto-tags and manual tags share **one `tags` identity table**; provenance
lives on the `image_tags` join row, not the tag itself — a manually-typed
"beach" and a model-generated "beach" collapse into one canonical tag
whose filter/count/search covers both origins, and every already-built tag
surface (filter popover, `list_tags`, FTS sync, XMP export) keeps working
unmodified. `image_tags` carries `source` (`manual`/`auto`), `confidence`
(nullable, auto only), and `review_state` (`unreviewed`/`confirmed`,
null for manual). **No category/tag hierarchy** — "landscape" (broad) and
"beach"/"sunset" (specific) are just different starter-list labels, scored
and stored identically; a real taxonomy tree is structural complexity
nothing has asked for yet.

**Two confidence thresholds**: a low storage floor (keeps `image_tags` from
filling with near-zero scores across every starter label) and a higher
display floor (what surfaces as a visible chip by default). Chips show as
plain text, matching manual tags visually — no raw percentage — but
*unreviewed* auto-tags stay visually distinguishable from confirmed/manual
ones; confirming an auto-tag grants full visual parity without rewriting
its `source` to `manual` (honest provenance, doesn't confuse future
re-scoring). ([029](tickets/029-design-tag-taxonomy.md))

## 5. Auto-tag & face review/correction

**Auto-tag correction**: removing a wrong auto-tag records a **rejection**
(`rejected_tags`, a separate table from `image_tags` — a rejected tag
isn't currently applied, so it shouldn't be a live join row) that persists
across re-scoring, so a dismissed tag stays dismissed rather than silently
reappearing. Confirming a low-confidence "maybe" tag flips `review_state`
to `confirmed` without touching `source`. Adding a missing tag is just the
existing manual `add_tag` flow.

**Face correction** reuses §6's two-entry-point shape (drawer chip list +
People view) for both naming and correction — no separate mechanism.

**Bulk correction** (e.g. the model consistently mis-tagging something
across many photos) reuses one shared **multi-select primitive** that §6's
face-cluster splitting also needs — the main grid has no multi-select
today; build it generically enough to serve both call sites, not narrowly
scoped to faces. ([033](tickets/033-design-review-correction-ux.md))

## 6. Face clustering & the People view

**Surfacing**: an opt-in "People" nav rail button with a badge count,
mirroring the existing dedupe review queue's pattern exactly — no
proactive interruption when background detection finds new faces (a real
personal library will produce a lot of low-value noise: blurry faces,
partial profiles, strangers in the background).

**The automation/ambiguity line sits at naming, not clustering.** Grouping
raw detections into unnamed, provisional clusters runs silently — it
asserts no identity claim. Naming a cluster is the confirmation gate.
Matching a **new** face against an **already-named** person gets its own
three-tier split (mirroring `SPEC.md` §6's exact/perceptual/no-match
dedupe structure): high-confidence → auto-attribute silently;
medium-confidence → human review (`face_match_review_queue`, same shape as
`dedupe_review_queue`); no plausible match → ordinary unnamed clustering.
The auto-attribute threshold runs stricter than the general clustering
threshold — misattributing to the *wrong named* person is more
consequential than two unnamed faces sharing a provisional group.

**Naming**: available from both the People view and the per-image detail
drawer's "People in this photo" chip list, autocompleting against
already-named people to avoid duplicate identities. **Persistence**:
unnamed clusters are never auto-pruned (recoverability principle); the
People view sorts by photo-count descending, with a reversible **Hide**
action (not delete) for clusters never wanted resurfaced. Quarantine's
30-day retention window does **not** transfer here — face clusters carry
no comparable storage pressure.

**Merge/split**: splitting a wrongly-merged cluster needs a new
multi-select-on-face-thumbnails primitive (shared with §5's bulk tag
correction); merging reuses `SPEC.md` §6's dedupe merge shape directly
(side-by-side, pre-selected suggestion, human overridable, union on
confirm). Name conflicts resolved the same way as a dedupe keeper
conflict — human picks, never silently chosen.

**Multiple faces per image**: a "People in this photo" chip list, reusing
the existing tag-chip UI — **no bounding-box overlay**. The correction-
precision argument for overlays is already solved by the split/merge
flow's individual face-crop thumbnails, so an overlay would be decoration
on an already-solved problem at real cost (coordinate-scaling math against
a variable-zoom preview). ([028](tickets/028-design-face-clustering-ux.md))

## 7. Smart albums / saved searches

**Always live** — re-evaluated on open, never a static snapshot (a
different, not-yet-mapped manual-collection feature). **No separate
query-builder UI**: "Save as Smart Album" on the grid's existing filter
bar, saving whatever combination of facets is currently applied — a new
Person facet joins the existing formats/sources/tags/date facets, but the
mechanism is unchanged. **Same virtualized grid component**, pre-loaded
with a saved filter state — no new rendering surface. **Persistent state
is just a name + serialized filter/search/sort JSON**, no image-membership
table, since there's no fixed member set to track.
([031](tickets/031-design-smart-albums-ux.md))

## 8. Similarity search

**Entry point**: a "Find Similar" action in the per-image detail drawer,
always starting from a library-resident image — consistent with
LensLocker having no "external file" concept anywhere else. Results are
the same grid, a new `SortOrder::SimilarityDesc`, with a minimum-similarity
floor and no raw scores shown by default (matching §4's tag-confidence
display treatment).

**Kept strictly separate from the dedupe review queue** — never surfaced
as potential duplicates. Perceptual-hash dedupe is tuned to near-*identical*
images; embedding similarity is a fundamentally broader net (two different
sunset photos score highly similar despite being clearly non-duplicate).
Mixing them would flood the dedupe queue with noise and risk training the
user to dismiss it, the same failure mode `SPEC.md` §6 already guards
against for RAW+JPEG pairs. The two features answer different questions —
"should I delete one of these" vs. "show me more like this."

**Text-to-image search is in scope**, additive to the existing FTS keyword
search, not a replacement — SigLIP's text encoder is already required for
zero-shot tagging (§2), so scoring a typed phrase is the same mechanism
evaluated live instead of stored, effectively free.
([034](tickets/034-design-similarity-search-ux.md))

## 9. Background execution model

**Decoupled, automatic background backlog** — not import-time (would
regress import's own tracked speed exit criteria), not a manual "Analyze"
button (no human judgment call exists at kickoff). An "images needing
analysis" backlog, worked through continuously whenever the app runs,
resumable across restarts like the import journal.

**Never blocks interactive commands** — a concrete requirement, not a
preference: `ImportLock`'s own doc comment in `src-tauri/src/lib.rs`
documents a real, observed Windows "Application Hang" from holding
`AppState.conn`'s mutex across a whole long run. Analysis must
acquire/release the connection lock per-image (or small batches), never
across a whole pass — the opposite of import's current pattern. GPU
contention between DirectML and WebView2's own compositing is flagged for
real empirical testing once built, not assumed away — the same discipline
`SPEC.md` §9's grid benchmark and this spec's §2 sqlite-vec benchmark both
required.

**Progress/cancellation**: a separate `AnalysisLock`/`analysis-progress`
pair, same architectural shape as `ImportLock`/`import-progress` but a
distinct instance (import and analysis can legitimately run concurrently).
Surfaced as an ambient status indicator, not a modal — analysis is
background maintenance the user didn't explicitly start, unlike import.

**Re-analysis on a model upgrade**: prompted, not automatic and not
silently manual-only — a one-time notice on detecting a model-version
mismatch, adding a batch to the same backlog queue. The old embedding for
an image survives until the new one is successfully computed and stored,
so a canceled/crashed re-analysis pass never leaves an image worse off
than before the upgrade started.
([030](tickets/030-decide-background-execution-model.md))

## 10. Model provisioning & installer

**Bundled directly in the NSIS installer**, extracted at install time into
the Program Files install directory (read-only at runtime, alongside
`webview2_hardening`'s config and the app's icons) — no separate first-run
extraction step, since zero-network-ever means the models must be present
before any run regardless. A model-version-bumping app update just
overwrites the old `.onnx` files via the normal update mechanism.

**Installer size — policy committed, real total deliberately unconfirmed
rather than fabricated.** Confirmed: the face pair is ~37MB. Unconfirmed,
flagged for verification before this number is load-bearing anywhere:
the DirectML-specific `onnxruntime.dll` size (only the ~12MB CPU-only
build is confirmed) and SigLIP `so400m`'s real on-disk ONNX size. If the
real total proves uncomfortably large, int8-quantizing the tagging model
is the first lever, before reopening the checkpoint choice. Soft ceiling
(a judgment call, not research): "a few hundred MB to low single-digit GB"
reads as acceptable for a personal desktop app with AI features.

**License text is static app content** (a `LICENSES` file + a new
About/Licenses UI screen — none exists today), **not database schema** —
the database-facing requirement is already fully satisfied by
`models.license_note`, reserved since Milestone 0.
([032](tickets/032-design-model-provisioning.md))

**New offline-enforcement acceptance criterion, surfaced during
assembly, not previously raised on any ticket** — see §12.

## 11. Schema

Full DDL and rationale:
[prototypes/035-ml-schema.md](prototypes/035-ml-schema.md). Structural
summary:

- **Tag provenance** lives on `image_tags` (`source`, `confidence`,
  `review_state`); **`rejected_tags`** is the separate do-not-reapply
  memory. The Milestone-0 `tag_scores` table is **dropped**, superseded.
- **`persons`**, **`face_clusters`** (nullable `person_id` = unnamed,
  `hidden` flag), **`face_detections`** (bounding box + a pre-cropped
  `crop_thumbnail_path` for the split/merge grid, embedding BLOB,
  `dimension` never hardcoded), and **`face_match_review_queue`**
  (mirrors `dedupe_review_queue`'s exact shape) implement §6.
- **Three tunable face thresholds** on `app_settings`
  (`face_cluster_threshold`, `face_review_threshold`,
  `face_auto_attribute_threshold`) — only `face_review_threshold` (0.363)
  is a real sourced default (SFace's own published verification
  threshold); the other two are explicitly-flagged placeholders pending
  real build-time calibration against actual SFace output.
- **`saved_albums`** is one JSON filter-blob column + a name, no
  membership table.
- **`embeddings`**/**`models`** (Milestone-0 forward-compat) need **no
  schema change** — already fit the one-embedding-per-image-per-model
  shape (SigLIP) and already carry `dimension`/`license_note`.
  `sqlite-vec`'s `vec0` mirror is deliberately absent from persisted
  schema — it's runtime-only, per §2.

([035](tickets/035-draft-ml-schema.md))

## 12. Offline enforcement — extending `SPEC.md` §8

`SPEC.md` §8's four layers (WebView2 COM hardening, `cargo-deny`,
WFP audit, install-time firewall block) were verified against the MVP's
dependency tree. This effort adds a genuinely new class of dependency
(`ort`/ONNX Runtime) that needs its own explicit check, not an assumption
that the existing layers automatically cover it:

- **ONNX Runtime ships its own optional telemetry collection on
  Windows.** This is the same shape of risk `SPEC.md` §8 already had to
  handle explicitly for WebView2 (`IsReputationCheckingRequired`,
  `IsCustomCrashReportingEnabled`) — a bundled third-party component with
  its own opt-in/opt-out telemetry surface, distinct from anything
  `cargo-deny`'s crate-level bans would catch. **New binding acceptance
  criterion**: explicitly disable ONNX Runtime's telemetry (its own
  disable-telemetry API) and verify it's off, the same driven-verification
  bar §8's Milestone 6 already met for WebView2.
- **Re-run `cargo deny check` and the WFP connection audit** with `ort`,
  `sqlite-vec`, and their transitive dependencies actually in the graph —
  `SPEC.md` §8.2's bans are crate-name-based and graph-wide, so they
  already generically cover a new dependency tree, but this needs to be
  *verified* empirically (the same discipline `SPEC.md` §8's Milestone 6
  applied originally), not assumed to still pass.

This was not raised on any closed ML ticket — flagged during assembly,
matching how `SPEC.md`'s own assembly session (§18) caught real gaps not
previously decided anywhere.

## 13. Explicitly out of scope / still fog

Carried from [ML-MAP.md](ML-MAP.md)'s Not yet specified / Out of scope
sections — deliberate exclusions, not gaps:

- **Active cross-import face re-identification** — once a person is named,
  whether/how future imports auto-suggest that same person. May turn out
  free from §6's clustering design or may need its own future ticket.
- **Multi-person-per-image tagging depth** — how many faces/tags per image
  the UI surfaces before it gets noisy.
- **Merging two already-named persons into one identity** (§6/§11) —
  reduces to reassigning clusters and deleting the orphaned `persons` row
  if ever needed; no schema exists for it now.
- **Proactive face-cluster merge suggestions** (§6) — left as fog by
  ticket 028, never picked up by 033; the dedicated `face_match_review_queue`
  pattern could be extended to this later if wanted.
- Everything already out of scope on [`SPEC.md`](SPEC.md) §12: multi-library
  support, video, non-Windows packaging, any network-touching feature.

---

# Part 2: Milestone Build Plan

Each milestone is sized to be handed to a build session as-is. Continues
the numbering convention `SPEC.md` used, but as a standalone sequence for
this effort — supersedes `SPEC.md` §11/Milestone 7's sketch entirely.

**Milestone ML-1 — Runtime & schema scaffolding.** Apply §11's DDL as a
real migration (`migrations/0003_ml_extensions.sql` or similar, including
dropping `tag_scores`); wire `ort`+DirectML into a dependency graph that
still passes `cargo deny check` cleanly; bundle real YuNet/SFace/SigLIP
`.onnx` files into the NSIS installer per §10; prove a `Session` loads and
runs a forward pass for each of the three models. No inference pipeline
logic yet, no UI. Exit criteria: all three models load and run a smoke-test
inference in a test harness; installer builds with the real files bundled.

**Build-time addendum to ML-1 (found during implementation, 2026-07-20,
after this spec locked; corrected 2026-07-20, same day, once ML-3
implementation actually tested the first fix):** §2's "one shared `ort`
environment, three separate `Session` objects... avoiding redundant
DirectML device initialization" does not hold as built. A `Session`
created with the DirectML EP in a process reliably crashes the whole
process (`STATUS_ACCESS_VIOLATION`) at session-creation time — reproduced
against the official stable `onnxruntime` v1.27.1 DirectML build,
independent of model/shape/file-vs-memory loading. Root cause unresolved.

**Corrected finding, verified empirically**: the crash is not about
*concurrent* DirectML sessions — it's that **at most one DirectML session
ever succeeds per process, full stop, for the process's entire lifetime**,
regardless of whether earlier ones were already dropped. This session's
first-pass "fix" (load a model, use it, drop the session, load the next)
was itself never actually tested against the sequential case before being
written down here — it does **not** work; a second DirectML session
still crashes even after the first is fully out of scope. Plain CPU
sessions (no execution provider registered) are unaffected: any number,
any order, before or after the one DirectML session. Full repro notes:
`crates/ml/src/lib.rs`'s `load_session` doc comment.

**Adopted design, going forward**: exactly one model gets DirectML per
process — SigLIP (Milestone ML-2's `TaggingModel`, the heaviest of the
three models and run most often). YuNet and SFace (Milestone ML-3) both
run CPU-only (`crates/ml::load_session_cpu`). Revisit only if CPU-only
face-pipeline throughput proves inadequate on real hardware — untested at
real scale as of this writing.

**Milestone ML-2 — Tagging pipeline, no UI.** SigLIP embedding generation
via the background backlog (§9); zero-shot scoring against the starter
label set (§2/§4); auto-tag insertion into `image_tags` with
`source`/`confidence`/`review_state`; the in-memory `vec0` mirror built at
library-open. Verify via direct SQLite queries / a test harness. Exit
criteria: import a real folder, confirm auto-tags land with correct
provenance/confidence, confirm a similarity query against the in-memory
mirror returns in well under 200ms.

**Milestone ML-3 — Face pipeline, no UI.** YuNet detection + SFace
embedding; `face_detections`/`face_clusters`/`persons` population; the
three-tier named-person match logic (§6) populating
`face_match_review_queue`. Verify via direct SQLite queries. Exit
criteria: import a folder with repeated faces, confirm clustering groups
them reasonably and the review queue populates for ambiguous matches.

**Milestone ML-4 — Review, correction & People UI.** The People view
(§6), the shared grid multi-select primitive (§5/§6), face
naming/merge/split, auto-tag review/reject with `rejected_tags`
persistence, bulk correction. This is the milestone where the ML effort's
output becomes reviewable/correctable — mirrors `SPEC.md`'s Milestone 5
role of "becomes a usable app."

**Milestone ML-5 — Smart albums & similarity search UI.** "Save as Smart
Album" on the existing filter bar, the new Person facet, `saved_albums`
persistence; "Find Similar" entry point, `SortOrder::SimilarityDesc`,
text-to-image search additive to the existing search box.

**Milestone ML-6 — Background polish, provisioning & offline
re-verification (ship gate for this effort).** `AnalysisLock`/
`analysis-progress` ambient UI; prompted re-analysis on model-version
bump; the About/Licenses screen; confirm real installer-size numbers
(§10's flagged DirectML DLL / SigLIP `so400m` sizes) before locking them
anywhere further; **§12's new offline-enforcement criteria** — ONNX
Runtime telemetry disabled and verified, `cargo deny check` and a real WFP
audit re-run with the ML dependency tree in place. Nothing before this
milestone needs to be network-clean with the new dependencies; this one
verifies that it is.

---

## Judgment calls made during assembly (owner-confirmed)

Everything above traces to a closed ticket except this one, surfaced
during assembly because the spec needed to be buildable without further
decisions:

1. **ONNX Runtime's own telemetry surface (§12)** — not raised on any
   closed ML ticket. `SPEC.md` §8 already established the precedent
   (WebView2's SmartScreen/crash-reporting needed explicit COM-level
   disabling, not just `cargo-deny`'s crate-level bans) — `ort` is exactly
   this shape of new risk, and needed a named, binding acceptance
   criterion rather than being silently assumed covered by existing layers.

Milestone slicing (Part 2) is one reasonable build order, not reviewed
line by line — reorderable, same caveat `SPEC.md`'s own assembly gave its
milestone plan.

**This closes [ML-MAP.md](ML-MAP.md).** Every ticket opened on this map
(023–036) is now closed; the route from "ML auto-tagging design" as a
fog item on the MVP map to a locked, buildable spec is complete.
