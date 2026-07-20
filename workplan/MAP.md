---
label: workplan:map
title: "LensLocker — route to a locked spec for the offline image manager"
created: 2026-07-17
tracker: TRACKER.md
---

# LensLocker work-plan map

## Destination — REACHED (2026-07-17)

A locked technical spec + milestone build plan for **LensLocker** — a 100% offline
Windows-first Rust desktop app that imports local image libraries into a managed,
space-efficient store and organizes, classifies, de-duplicates, and tags them.
Every architectural and product decision resolved; a build session can execute the
plan without needing to decide anything.

**→ [SPEC.md](SPEC.md) is the locked artifact.** Every ticket on this map is
closed. Building LensLocker is no longer a work-plan session — it's a build
session working from the spec.

## Notes

- Domain: local-first desktop software, Rust. Consult `ink-and-agency:rust-engineer`
  when a ticket turns on Rust specifics; resolve grilling tickets via
  `ink-and-agency:grill-me` (one question at a time, recommendation first).
- Standing constraints, non-negotiable: **zero network access** (no telemetry, no
  update pings, nothing); **quality is never sacrificed** by storage optimizations;
  scale target **100k+ images**; destructive actions always previewed/recoverable
  per the verify-plus-retention decision.
- **Personal/internal project — never distributed outside the owner.** This
  dissolved both the GPL-3.0 linkage concern
  ([009](tickets/009-conversion-policy.md), [020](tickets/020-research-gpl-linkage.md),
  [021](tickets/021-decide-project-license.md)) and the HEVC patent-exposure
  concern ([014](tickets/014-v1-format-matrix.md),
  [022](tickets/022-legal-review-hevc.md)) — both attach to distribution, not
  personal use. If distribution scope ever changes, both questions need reopening
  as fresh decisions.
- Plan, don't do: this map produces decisions and the spec. Implementation happens
  after [Assemble the locked spec and build plan](tickets/018-assemble-spec.md) closes.
- Tracker conventions: see [TRACKER.md](TRACKER.md). Open tickets are found by query
  (frontier = open, unassigned, blockers all closed), not listed here.

## Decisions so far

- [Adopt the core stack](tickets/001-adopt-core-stack.md) — Rust core, SQLite catalog, BLAKE3 exact + pHash perceptual dedupe; GUI shell deliberately excluded.
- [Adopt the managed-library move model](tickets/002-managed-library-move-model.md) — import MOVEs files into an app-controlled, space-efficient store; quality-preserving format conversion allowed.
- [Scope classification: metadata MVP, ML later](tickets/003-scope-classification.md) — MVP classifies by extracted facts only; offline ML auto-tagging is a committed post-MVP milestone.
- [Target Windows-first with a portable core](tickets/004-windows-first-portable-core.md) — v1 ships Windows-only; core crates stay OS-agnostic.
- [Adopt verify-plus-retention import safety](tickets/005-verify-plus-retention.md) — originals only removed after managed-copy verification, then quarantined for a configurable window before permanent deletion.
- [Survey quality-preserving image recompression options](tickets/008-research-recompression.md) — only JPEG→JXL is byte-exact (baseline JPEGs, attempt-and-verify); PNG→JXL/WebP pixel-exact with metadata caveats; AVIF excluded; RAW never converted; Rust JXL-encode tooling has a licensing gap.
- [Survey Rust GUI shells for an offline image grid](tickets/006-research-gui-shells.md) — Tauri: best velocity but offline-by-configuration (WebView2 telemetry outside its reach); egui most proven pure-Rust with provable no-network; iced immature; Slint license-gated; no shell has real 100k-grid benchmarks — a spike may be needed.
- [Survey zero-network enforcement options](tickets/015-research-network-enforcement.md) — load-bearing chain: no network crates + cargo-deny CI + WFP connection auditing + firewall outbound block; Tauri capabilities can't silence WebView2's own SmartScreen/crash traffic (COM settings can); pure-Rust shell removes that class entirely.
- [Survey Windows codec sourcing and licensing](tickets/013-research-codec-licensing.md) — never bundle HEVC/HEIC decode (patent minefield; use Windows' local WIC codec via the platform shell); RAW via pure-Rust rawler; AVIF bundleable with decode caveats; JXL decode safe (jxl-oxide), encode still unlicensable; SVG (resvg) clean.
- [Survey offline auto-tagging options](tickets/016-research-offline-autotagging.md) — realistic stack: ort+DirectML with SigLIP ONNX (Apache-2.0, cleanest license) + sqlite-vec brute-force (benchmarked fine at 100k); schema must reserve model-versioned embeddings keyed to BLAKE3, tag scores, ANN-sidecar headroom; face models (InsightFace) not redistributable.
- [Choose the GUI shell](tickets/007-choose-gui-shell.md) — Tauri v2, UI velocity over structural offline-cleanliness; binding v1 requirement to close the WebView2 gap: COM settings + cargo-deny CI + WFP runtime audit + install-time firewall block, all four, none deferred.
- [Decide the duplicate-handling experience](tickets/011-dedupe-ux.md) — exact dupes auto-collapse silently; perceptual near-dupes always human-reviewed (never auto-merged), default ≤5-bit Hamming threshold, user-tunable; RAW+JPEG pairs auto-excluded from the queue; on merge, tags union and the loser is quarantined, not deleted.
- [Decide metadata and tag portability](tickets/012-metadata-portability.md) — SQLite authoritative, mirrored to XMP sidecars (managed image bytes never rewritten for a tag edit); rebuild-from-sidecars is a committed v1 recovery path (dedupe history/embeddings excluded); metadata preservation across conversion is now a hard requirement; export = file + sidecar, sufficient for v1.
- [Define the conversion policy](tickets/009-conversion-policy.md) — pixel-exact is the bar; JPEG→JXL (jbrd, byte-exact bonus), PNG/BMP→JXL Modular, TIFF stays TIFF (native recompress); AVIF/RAW/unknown-chunk files never converted; encoder is `jpegxl-rs` (GPL-3.0) — opened a real, unresolved license question; opt-out is per-library, fixed.
- [Design the import pipeline and managed store layout](tickets/010-import-pipeline-store-layout.md) — content-addressed store keyed by pre-conversion hash; perceptual matches never pause import (flag-after, not block); per-file SQLite journal makes crashes resumable; quarantine always lives with the managed store (forces cross-volume copy for removable-media imports); retention sweep runs at launch only.
- [Lock the v1 format matrix](tickets/014-v1-format-matrix.md) — standard formats/TIFF/RAW/AVIF/JXL/SVG full v1 support (RAW decode sandboxed, AVIF prefers a pure-Rust patch with C fallback); HDR/EXR deferred; HEIC delegates to the Windows OS codec and refuses (not degrades) if it's absent; the delegation strategy itself needs real legal review before ship.
- [Survey GPL-3.0 linkage implications of jpegxl-rs](tickets/020-research-gpl-linkage.md) — static/dynamic linking is irrelevant to GPL (unlike LGPL); obligation triggers only at distribution, not private builds; libjxl itself is BSD, only jpegxl-rs's bindings are GPL — a fresh-bindings middle path exists; CLI-subprocess has real but non-guaranteed FSF backing.
- [Draft the catalog schema](tickets/017-draft-catalog-schema.md) — DDL prototype in `workplan/prototypes/017-catalog-schema.md`; images 1:1 with unique content (one grid card per photo), tags global, dedupe threshold/retention global settings; content-addressed storage, unified quarantine, merge-as-status, empty-but-shaped ML tables reserved for later.
- [Validate virtualized thumbnail-grid performance at 100k+ images](tickets/019-validate-thumbnail-grid-performance.md) — real prototype, not estimated: flat 60fps/zero dropped frames under realistic momentum-scroll at 100k items; graceful (not catastrophic) degradation under an unrealistic worst-case sweep; validates the Tauri shell choice. Custom URI-scheme protocol serving failed silently on Windows — static-asset serving works and is the fallback plan.
- [Decide LensLocker's project license](tickets/021-decide-project-license.md) — dissolved, not chosen: GPL-3.0 obligation triggers only at distribution (GPLv3 §2), and this never ships to anyone else; the 009 encoder choice stands unchanged. Reopen if distribution scope ever changes.
- [Get legal review on the HEIC/HEVC delegation strategy](tickets/022-legal-review-hevc.md) — dissolved, not reviewed: HEVC patent exposure attaches to distributing decode software, not personal use; no vendor role exists here. The OS-codec-delegation technical decision (014) stands unchanged.
- [Assemble the locked spec and build plan](tickets/018-assemble-spec.md) — **destination reached.** [SPEC.md](SPEC.md) locked: 12-section spec + 7-milestone build plan, every section citing its deciding ticket. Crate layout and thumbnail defaults (never separately ticketed) confirmed during assembly.

## Not yet specified

- **ML auto-tagging design** — graduated 2026-07-20 into its own effort:
  [LensLocker ML work-plan map](ML-MAP.md) (tagging, categorization, smart
  albums, similarity search, and face auto-recognition — broader than this
  item's original framing).
- **Thumbnail/preview cache strategy** — sizes, format, eviction, GPU decode, and
  now also *serving mechanism* (static files vs. a custom protocol — the latter
  failed silently on Windows/WebView2 per
  [the grid-performance benchmark](tickets/019-validate-thumbnail-grid-performance.md),
  so static-file serving is the known-working fallback). GUI shell is decided
  (Tauri) and the grid architecture is now validated at scale; this item still
  waits on [Design the import pipeline and managed store layout](tickets/010-import-pipeline-store-layout.md)
  (already closed — thumbnails weren't in its scope) to sharpen fully once
  someone designs the actual generation/caching pipeline.
- **Multi-library support** — several managed stores, cross-library dedupe, moving
  images between libraries.
- **Export/interop story** — getting images back OUT with tags intact; depends on the
  metadata-portability decision.
- **Store integrity over time** — scheduled rescans, bit-rot detection, behavior when
  the store is modified externally.
- **Catalog + store backup story**.
- **Post-MVP organizer features** from the original brief — cleanup suggestions,
  saved searches/smart albums, move/rename history UI.

## Out of scope

- **Any network-touching feature** — sync, sharing, online metadata lookup, update
  checks. The destination is a fully offline app; these return only as a fresh effort
  if the destination is ever redrawn.
- **Video file management** — images only.
- **macOS/Linux packaging for v1** — the portable-core decision keeps the door open,
  but v1 packaging/codec work covers Windows only.
