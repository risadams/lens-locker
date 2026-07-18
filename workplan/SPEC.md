# LumenVault — Technical Specification & Build Plan

Status: **LOCKED** (2026-07-17). Assembled from every closed ticket on
[the work-plan map](MAP.md); each section cites the ticket that decided it. This
document is the destination — a build session should be able to execute the MVP
milestones below without deciding anything further.

## 1. What LumenVault is

A 100% offline, Windows-first desktop application that takes custody of a local
image library — importing files with a destructive move, converting them
losslessly where it saves real space, de-duplicating exact and near-duplicate
images, tagging, and organizing — for exactly one user, on one machine, never
distributed. ([002](tickets/002-managed-library-move-model.md),
[004](tickets/004-windows-first-portable-core.md))

**Non-negotiable constraints:**
- **Zero network access.** No telemetry, no update checks, no calling home, ever.
- **Quality is never sacrificed** by any storage optimization (pixel-exact is the
  bar — [009](tickets/009-conversion-policy.md)).
- **Scale target: 100k+ images**, validated at that scale for the UI
  ([019](tickets/019-validate-thumbnail-grid-performance.md)).
- **Destructive actions are always previewed/recoverable**
  ([005](tickets/005-verify-plus-retention.md)).
- **Personal/internal use only, never distributed.** This is a standing scope
  constraint, not just a current-state fact — it's why the GPL-3.0 dependency and
  the HEIC/HEVC delegation strategy needed no license/legal resolution
  ([021](tickets/021-decide-project-license.md),
  [022](tickets/022-legal-review-hevc.md)). **If this ever changes — sharing the
  binary, open-sourcing, anything conveyed to another person — both of those
  questions must be reopened before shipping**, and this spec's compliance posture
  (§8) is void until they are.

## 2. Architecture

**Stack**: Rust core, Tauri v2 shell, SQLite catalog (via `rusqlite`), BLAKE3
exact hashing, a 64-bit perceptual hash compared by Hamming distance.
([001](tickets/001-adopt-core-stack.md), [007](tickets/007-choose-gui-shell.md))

**Platform**: Windows-first for v1 — packaging, codec sourcing, and installer work
target Windows only. Core crates stay OS-agnostic (no Windows-only APIs outside a
thin platform shell), so Linux/macOS can follow later without a rewrite.
([004](tickets/004-windows-first-portable-core.md))

**Crate layout** *(judgment call — see §9; not separately decided anywhere, this
is a reasonable synthesis of everything above)*:

```
lumenvault/
├── crates/
│   ├── core/        domain types (Image, Library, Tag…), no I/O
│   ├── catalog/      SQLite access layer, migrations, queries (owns schema.sql, §5)
│   ├── hash/          BLAKE3 + perceptual hash
│   ├── decode/       format decode: image crate, jxl-oxide, re_rav1d/dav1d,
│   │                  resvg, Windows WIC (HEIC) — everything except RAW
│   ├── raw-worker/   separate small binary; sandboxed RAW decode (rawler),
│   │                  isolated per §6.4
│   ├── convert/      conversion policy implementation (jpegxl-rs, TIFF recompress)
│   ├── xmp/           sidecar read/write
│   └── import/        pipeline orchestration: journal, dedupe-check, quarantine
├── src-tauri/        Tauri v2 app: Rust commands binding the crates above
└── ui/                 vanilla JS/TS frontend: virtualized grid, review queue, tagging
```

## 3. Managed store & import pipeline

**Model**: the app owns the library location. Import is a **destructive move**,
not a copy. The store must be space-efficient — exact duplicates are never stored
twice, and lossless conversion may reduce size. ([002](tickets/002-managed-library-move-model.md))

**Store layout: content-addressed**, keyed by the BLAKE3 hash of the **original
(pre-conversion) source bytes** — re-importing the same original always resolves
to its existing entry regardless of what it was converted to. A second hash of the
*stored* bytes is kept for blob integrity verification. The store is not
Explorer-browsable by design; the app is the interface.
([010](tickets/010-import-pipeline-store-layout.md))

**Import pipeline**, per source file:
1. Hash source bytes (BLAKE3). Exact match in catalog → collapse (add reference,
   skip conversion, jump to step 6).
2. RAW+JPEG pairing check (filename stem + timestamp proximity) — bond before any
   perceptual matching.
3. Perceptual hash computed; a match adds a review-queue entry but **never pauses
   this file's import** — it proceeds through every remaining step normally.
4. If the library has conversion enabled: attempt conversion per §4; verify.
   Failure means the file proceeds **unconverted**, never degraded.
5. Hash stored bytes; write to the content-addressed path; write the XMP sidecar.
6. Move the original into quarantine.

Each step's completion is journaled in SQLite **as it happens** — an interrupted
import resumes each file from its last completed step on restart, never
re-converting or re-verifying, never leaving a file half-moved.
([010](tickets/010-import-pipeline-store-layout.md))

**Quarantine & retention**: verify the managed copy before touching the original;
move the original to quarantine, retained for a configurable window (default 30
days) before permanent deletion. **Quarantine always lives on the managed store's
own volume**, never the source volume — even though this forces a cross-volume
copy for every SD-card/USB import — because the retention guarantee must survive
the source drive being unplugged or reformatted. Cross-volume "moves" are
therefore copy + verify + delete-source, not an OS rename. Retention is enforced
by a **launch-only sweep** (no in-session timer).
([005](tickets/005-verify-plus-retention.md), [010](tickets/010-import-pipeline-store-layout.md))

## 4. Conversion policy

**Reversibility bar: pixel-exact**, not universally byte-exact — a photo-library
standard. Byte-exact is used as a bonus verification signal where available
(JPEG). ([009](tickets/009-conversion-policy.md))

| Source | Target | Mechanism | On failure |
|---|---|---|---|
| Baseline JPEG | JPEG XL (lossless transcode, jbrd) | `jpegxl-rs` encode + reconstruct-verify + byte-compare | Leave untouched — no pixel-exact fallback saves space for JPEG |
| PNG | JPEG XL, Modular (lossless) | `jpegxl-rs` encode + pixel-compare | Leave untouched if ancillary chunks fall outside the allowlist |
| BMP | JPEG XL, Modular (lossless) | Same as PNG | — |
| TIFF | TIFF, recompressed in place (LZW/Deflate) | Native container, tag directory untouched | — |
| AVIF (lossless) | **Excluded** | Not competitive on size; no Rust encoder implements it | N/A |
| Camera RAW | **Never converted** | Proprietary sensor data; copy verbatim | N/A |
| Unrecognized ancillary data | **Never converted** | Cannot prove round-trip safety | N/A |

One consolidated target format (JPEG XL) for JPEG/PNG/BMP; TIFF is the deliberate
exception (native recompression is lower-risk). Encoder: **`jpegxl-rs`
(GPL-3.0-or-later)** — a technical choice (cleaner in-process API over subprocess
management), unconstrained by the license question per §1's personal-use scope.
Opt-out granularity: **per-library, fixed at library creation**.

Metadata (EXIF/XMP/ICC) preservation across conversion is **binding** — every
conversion path explicitly requests full preservation and verifies its presence
post-conversion before the original is quarantined. Never-convert is a hard stop
on any verification failure, never a degrade.

## 5. Format matrix (import support, v1)

| Family | Support | Notes |
|---|---|---|
| Standard web (JPEG, PNG, WebP, GIF, BMP) | Full | Native `image` crate |
| TIFF | Full | Native decode; recompressed per §4 |
| HDR, EXR | **Deferred to post-v1** | Small share of typical libraries, unvetted risk surface |
| Camera RAW (CR2/NEF/ARW/DNG/RW2/CR3…) | Full | `rawler` (pure Rust), decode isolated in `raw-worker` — not hardened against malformed input upstream |
| HEIC/HEIF | **Conditional** | Delegates to Windows' own WIC/HEVC codec if installed; **refuses** the file (not degrade) if absent, pointing the user at the free Windows HEVC Video Extensions |
| AVIF | Full | Pure-Rust `re_rav1d` patch attempted first, falls back to C `dav1d` if it doesn't build cleanly |
| JPEG XL | Full | Decode via `jxl-oxide`; encode via `jpegxl-rs` for conversion output |
| SVG | Full | `resvg`/`usvg`/`tiny-skia` rasterizes to a thumbnail; perceptual hash runs on the raster |

([014](tickets/014-v1-format-matrix.md))

## 6. Duplicate handling

- **Exact duplicates (BLAKE3 match) auto-collapse silently** at import — one
  stored blob, referenced by every image that matched it. No review step; the
  import log records every collapse.
- **Perceptual near-duplicates always route to a human review queue** — never
  auto-merged. Default threshold **≤5 Hamming bits**, a **global app setting**
  (not per-library — corrected during schema drafting, §9 of
  [017](tickets/017-draft-catalog-schema.md)).
- **RAW+JPEG camera pairs are auto-detected and excluded from the review queue**
  entirely (filename stem + timestamp proximity), so the queue stays focused on
  genuine junk dupes.
- **Merge UX**: side-by-side with resolution/format/size/date surfaced; the
  higher-quality copy is pre-selected but overridable. On confirm, tags **union**
  onto the keeper, and the discarded blob is quarantined through the same
  retention mechanism as any other removed original — never deleted outright.

([011](tickets/011-dedupe-ux.md))

## 7. Metadata & tag portability

**SQLite is authoritative; every tag/metadata change mirrors to an XMP sidecar**
next to its managed file. Managed image bytes are never rewritten for a tag edit.
**Rebuild-from-sidecars is a shipped v1 recovery path**: if the catalog is lost,
the app rescans the store, re-hashes everything (BLAKE3 + perceptual), and
re-imports tags/metadata from sidecars. Dedupe-merge history and ML embeddings are
explicitly **not** recoverable this way — an accepted limitation.

**Export is in v1 scope** (amended 2026-07-18, during Milestone 5's UI design —
the original ticket deferred it, but the owner reviewed a working design with a
real Export action and chose to bring it forward rather than ship a UI that
lies about what it does). The mechanism is unchanged from the original design:
copying the managed file plus its `.xmp` sidecar to a location the owner
chooses — no format round-trip, the file exports in whatever format is
currently stored (e.g. a converted `.jxl`, not un-converted back to the
original JPEG). Sufficient given XMP is widely read (Adobe products, digiKam,
darktable).

([012](tickets/012-metadata-portability.md))

## 8. Offline enforcement — binding v1 acceptance criteria

Not aspirational hardening — these four layers ship in v1 and are verified before
release, because Tauri's capability/CSP system governs only the JS↔Rust IPC
bridge, not WebView2's own background traffic:

1. **WebView2 COM settings** disabling `IsReputationCheckingRequired` (SmartScreen)
   and `IsCustomCrashReportingEnabled` (crash minidump upload).
2. **`cargo-deny`** `[bans]`/`[sources]` rules banning network crates
   (reqwest, hyper, tokio net features…) transitively, as a CI gate.
3. **Runtime WFP connection-audit** (Windows Security events 5156/5157), run at
   least once before release as empirical proof, not just static audit.
4. **Install-time Windows Firewall outbound-block** rule on the app's executable,
   as a regression catcher.

([007](tickets/007-choose-gui-shell.md), [015](tickets/015-research-network-enforcement.md))

**Milestone 6 implementation status** (added 2026-07-18, does not change any
binding criterion above — records where each layer's real artifact lives):

1. Implemented and driven-verified: `src-tauri/src/webview2_hardening.rs`,
   wired into `src-tauri/src/lib.rs`'s window setup. The compiled app was
   launched and both settings were read back from the live COM objects
   (`IsReputationCheckingRequired` read back `false`; the webview's live
   `ICoreWebView2Environment` confirmed identical, by COM pointer, to the one
   built with `IsCustomCrashReportingEnabled` set) — see the Milestone 6
   commit message for the full transcript.
2. Confirmed still passing (`cargo deny check` — advisories/bans/licenses/
   sources all ok) with everything Milestone 5 added.
3. Blocked in the environment this milestone was built in — both `auditpol`
   and `Get-WinEvent -LogName Security` require elevation this session
   didn't have. Real, ready-to-run artifact left behind:
   `docs/verify-wfp-audit.ps1` (script) / `docs/wfp-audit-runbook.md`
   (procedure) — run from an elevated session before release.
4. Hook script written and syntax-validated with a real `makensis` compile
   (`src-tauri/windows/hooks.nsh`, wired via `tauri.conf.json`'s
   `bundle.windows.nsis.installerHooks`, `installMode: "perMachine"` so the
   installer itself runs elevated). NSIS installed cleanly via
   `winget install NSIS.NSIS` with no elevation needed. A full
   `tauri build --bundles nsis` could not complete in this environment,
   though — `jpegxl-sys`'s release-profile CMake build needs a
   `Visual Studio 17 2022` generator that isn't installed here (a different,
   deeper gap than Milestone 2's Clang-component fix, and out of this
   milestone's scope to resolve). The firewall rule itself is therefore
   unverified end-to-end; that needs a release build from an environment
   with Visual Studio installed.

## 9. GUI shell & thumbnail grid

**Tauri v2**, chosen for UI velocity over the pure-Rust shells' structural
offline-cleanliness — acceptable specifically because §8's mitigation package
closes the gap. ([007](tickets/007-choose-gui-shell.md))

**Validated, not assumed, at scale**: a real 100,000-item virtualized grid
prototype sustained a **flat 60fps with zero dropped frames** under realistic
momentum-scroll, with clean thumbnail decode latency (~2.6–3.1ms avg). Graceful
(not catastrophic) degradation under an artificial worst-case sweep. WebView2
process overhead: ~365–591MB, an accepted tradeoff of the shell choice.
([019](tickets/019-validate-thumbnail-grid-performance.md))

**Known issue, workaround adopted**: a custom URI-scheme protocol handler for
serving generated thumbnails **fails silently on Windows/WebView2**
(`fetch()` throws, no error surfaces through `<img>`). **Thumbnails must be served
as static files** (written to disk, referenced by path) — this is validated
working, not a fallback to revisit.

**Thumbnail defaults** *(judgment call — see §9 "Judgment calls to confirm";
sizing/eviction was flagged as fog throughout the map and needed a concrete answer
for this spec to be buildable without further decisions)*:
- Grid thumbnail: **256×256, WebP** (lossy, quality 85 — this is a UI convenience
  copy, not a managed-store asset, so §4's pixel-exact bar doesn't apply to it).
- Preview (single-image view): **1600px longest edge, WebP**.
- **No eviction in v1** — at ~20–40KB/grid-thumbnail, 100k images is roughly
  2–4GB, an acceptable fixed cost. Revisit if real usage shows otherwise.
- Generated lazily on first grid view, written to the schema's `thumbnails` table
  (§10) as static files under the library root, served via Tauri's default
  static-asset pipeline per the validated workaround above.

## 10. Catalog schema

Full DDL and rationale: [prototypes/017-catalog-schema.md](prototypes/017-catalog-schema.md).
Structural summary:

- **`images`** is 1:1 with unique content (one grid card per photo, not per
  import) — content identity, storage location, EXIF-derived fields, perceptual
  hash, sidecar sync state, and merge lifecycle all live here.
- **`image_sources`** holds the many:1 audit trail of every filename/path that
  ever resolved to that content.
- **`libraries`** holds the one per-library setting (`conversion_enabled`);
  **`app_settings`** is a global singleton holding the Hamming threshold and
  retention window.
- **`import_batches`** / **`import_journal`** implement the crash-safe pipeline
  (§3).
- **`quarantine`** unifies import-time and merge-time discards under one
  retention model.
- **`dedupe_review_queue`**, **`raw_jpeg_pairs`**, **`tags`**/**`image_tags`**,
  **`thumbnails`** implement §5–§7 directly.
- **`models`**, **`embeddings`**, **`tag_scores`** are shaped now, empty until
  §11 ships — model-versioned, keyed to content hash not path, dimension not
  hardcoded.

## 11. Post-MVP milestone: offline ML auto-tagging (sketch only)

Not built in v1; the schema (§10) already reserves for it. When this milestone is
picked up:

- **Stack**: `ort` (ONNX Runtime, DirectML feature — the only cross-vendor
  Windows GPU path) + an ONNX SigLIP export (Apache-2.0, cleanest license of the
  surveyed options; Immich publishes ready-made exports) + `sqlite-vec`
  brute-force search inside the existing catalog (benchmarked ≤75ms/query at
  100k×768-dim, adequate with headroom).
- **Re-benchmark `sqlite-vec` on real hardware** before locking this milestone's
  own spec — the published number isn't LumenVault-specific.
- Face-grouping is explicitly **not** covered by this sketch — InsightFace-style
  models aren't freely redistributable, unlike SigLIP/CLIP; a separate license
  and design pass would be needed.

([003](tickets/003-scope-classification.md), [016](tickets/016-research-offline-autotagging.md))

## 12. Explicitly out of scope (this spec does not cover)

Carried from [the map](MAP.md)'s Not yet specified / Out of scope sections —
**not gaps in this spec, deliberate exclusions**:

- Multi-library support (cross-library dedupe, moving images between libraries).
- A built export/interop feature (the metadata model supports it; nothing is
  built).
- Store integrity over time (scheduled rescans, bit-rot detection, externally
  modified store).
- Catalog/store backup tooling.
- Post-MVP organizer features (cleanup suggestions, saved searches, move/rename
  history UI).
- Any network-touching feature, video file management, non-Windows packaging.

---

# Part 2: Milestone Build Plan

Each milestone is sized to be handed to a build session as-is — no further
decisions required, only implementation.

**Milestone 0 — Workspace scaffolding.** Cargo workspace per §2's crate layout;
Tauri v2 app shell; `cargo-deny` config wired into CI (§8.2) from day one, not
bolted on later; empty `catalog` crate applies §10's DDL via `rusqlite_migration`
or equivalent.

**Milestone 1 — Import pipeline, no UI.** `hash`, `decode` (standard formats
only), `import` crates: implement the six-step pipeline (§3) end-to-end against a
single hardcoded library path, driven by a CLI or test harness — hashing,
content-addressed write, journal, quarantine, retention sweep. No conversion, no
dedupe, no thumbnails yet. Exit criteria: import a real folder of JPEGs/PNGs,
kill the process mid-batch, confirm resume works correctly.

**Milestone 2 — Conversion.** `convert` crate: `jpegxl-rs` integration, the §4
matrix, attempt→verify→never-degrade. Exit criteria: convert a real JPEG library,
confirm byte-exact reconstruction round-trips, confirm metadata survives.

**Milestone 3 — Dedupe.** Exact-collapse (should mostly fall out of the
content-addressed design already built in Milestone 1) + perceptual hashing +
`dedupe_review_queue` + RAW+JPEG pairing. No review-queue UI yet — verify via
direct SQLite queries. Exit criteria: import a folder with known dupes and a
RAW+JPEG pair, confirm the queue and the pairing table are both correct.

**Milestone 4 — Metadata & tags.** `xmp` crate: sidecar write-on-change, the
rebuild-from-sidecars recovery path (test by deleting the `.sqlite` file and
rescanning). Manual tagging via direct catalog calls (still no UI).

**Milestone 5 — Grid UI.** `ui` crate: the virtualized grid from
[the validated prototype](tickets/019-validate-thumbnail-grid-performance.md)
(reuse its approach directly — it's proven), thumbnail generation per §9's
defaults served as static files, filtering (date/format/source/tags, not just
date/format/directory — "source" replaced "directory" since the vault isn't
Explorer-browsable by design, see §3), sorting, live search, the review-queue UI
with real merge/dismiss backend logic (§6 — this had never actually been
implemented behind the M3 review-queue detection), the tagging UI, an import
trigger (implied by needing any way to get photos into the grid, never
separately scoped), a full-size lightbox (zoom/pan/keyboard prev-next), and
export (§7, amended into v1 scope alongside this milestone). Full interaction
design, owner-approved before build: `workplan/design/lumenvault-design.html`.
This is the milestone where LumenVault becomes a usable app.

**Milestone 6 — Offline hardening & release gate.** Implement all four §8 layers
for real (not just documented); run the WFP connection audit at least once;
package the Windows installer with the firewall rule. This milestone is the v1
ship gate — nothing before it needs to be network-clean, this one verifies that it
is.

**Milestone 7 (post-MVP) — ML auto-tagging.** Per §11's sketch, once picked up as
its own effort: re-benchmark `sqlite-vec`, integrate `ort`+DirectML, bundle a
SigLIP ONNX export, populate `embeddings`/`tag_scores`, build the similarity-search
UI.

**Milestone 5.5 — First-run vault setup & configuration** (added 2026-07-18,
after all seven MVP milestones were built — a real gap in the original
breakdown, not a deferred item). No milestone ever specified how a user
*chooses* where the library lives; Milestone 5's own build session flagged this
as an unresolved judgment call and defaulted to `<app-data-dir>/library` — which
turned out to be a real problem, not a placeholder: AppData is typically a small
system-drive folder, actively wrong for something meant to hold a large personal
photo library. **The app must never silently default the library location
anywhere, under any circumstance.** Owner-approved design:
`workplan/design/lumenvault-design.html`'s first-run screen (no path
pre-filled — a folder must be explicitly chosen; free space shown after
choosing; an existing library at the chosen path is detected and opened rather
than re-created).

Scope: a first-run screen (folder picker via the existing dialog plugin, no
default; free-space display; existing-library detection routes to "Open," not
"Create"; the conversion toggle only appears for a genuinely new vault, since
§4/ticket 009 fixes `conversion_enabled` at creation) plus the minimal
app-level bootstrap needed to support it — a small config file *outside* the
library (an ordinary AppData location is fine for this, since it holds only a
path string, not photo data) recording which folder the real library lives in,
checked on every launch before the catalog is opened. If the recorded path is
missing or inaccessible (e.g. an external drive unplugged), the app must
route back to this same screen — never fall back to a default location.

Also scope: exposing `app_settings.hamming_threshold` and `.retention_days` —
both explicitly decided as user-tunable (tickets 011, 005) but never given a
UI in any milestone — via a Settings screen. `libraries.root_path` is
**not** made changeable after creation in this pass; moving an existing
library's data to a new location is real future work, not solved here.

---

## Judgment calls made during assembly (owner-confirmed)

Everything above traces to a closed ticket except these three, made during
assembly because the spec needed to be buildable without further decisions.
Both substantive ones were confirmed as drafted:

1. **Crate layout (§2)** — confirmed as drafted. Not decided anywhere prior; a
   synthesis of every crate the closed tickets implied (raw-worker isolation,
   convert, decode, xmp, import). Names/folder nesting are still free to change
   at Milestone 0 — the separation of concerns is what's locked, not the labels.
2. **Thumbnail defaults (§9)** — confirmed as drafted: grid 256×256 WebP, preview
   1600px WebP, no v1 eviction. Cheap to revisit later since thumbnails are fully
   regenerable from the managed store.
3. **Milestone slicing (Part 2)** — one reasonable build order, not reviewed line
   by line with the owner. Reorder freely if a different sequence suits better
   (e.g. thumbnails before dedupe) — this is sequencing, not a locked decision.

Everything else in this document is a direct restatement of a decision already
made and closed on [the map](MAP.md) — nothing else here is new.
