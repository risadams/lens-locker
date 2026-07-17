---
id: 010
title: "Design the import pipeline and managed store layout"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: [009, 011]
---

## Question

Given the move model and conversion policy: how is the managed store laid out on
disk (content-addressed vs human-readable date/folder structure — can the user
browse it outside the app?), and what are the import pipeline's exact semantics —
ordering of hash → dedupe-check → convert → verify → move-original-to-quarantine;
atomicity and crash recovery (journal? resumable imports?); cross-volume moves;
quarantine mechanics and the retention window from
[Adopt verify-plus-retention import safety](005-verify-plus-retention.md);
collision/conflict handling when an import matches an image already in the store —
now shaped by [Decide the duplicate-handling experience](011-dedupe-ux.md): exact
matches collapse to one blob automatically, RAW+JPEG pairs are detected and bonded
before perceptual matching runs, and perceptual matches queue for human review
rather than blocking the import. Also shaped by
[Decide metadata and tag portability](012-metadata-portability.md): the pipeline
must write an XMP sidecar alongside each managed file, and metadata preservation
across conversion must be verified before the original is quarantined. The
conversion step itself is now fully specified by
[Define the conversion policy](009-conversion-policy.md): the per-format target
matrix, the attempt→verify→never-convert-on-failure pattern, and the libjxl
(`jpegxl-rs`) dependency this pipeline must bundle and invoke.

## Resolution

**Store layout: content-addressed, keyed by the original (pre-conversion) BLAKE3
hash.** A file's address in the managed store is derived from the hash of the
source bytes as they arrived, not the converted output — so re-importing the same
original always resolves to the existing entry regardless of what it was converted
to. SQLite maps that identity to wherever the (possibly converted) bytes actually
live on disk; a second hash of the *stored* bytes is kept separately for blob
integrity verification. Exact-dupe collision at this layer is not a conflict to
resolve — per [Decide the duplicate-handling experience](011-dedupe-ux.md), it's
the auto-collapse case: add a reference, skip conversion (the blob already exists),
move the new original into quarantine like anything else. The store is not
intended to be Explorer-browsable; the app is the interface, and the
rebuild-from-sidecars path plus human-readable XMP sidecars cover out-of-app
inspection.

**Pipeline order**, per source file:
1. Hash source bytes (BLAKE3). Exact match in catalog → collapse, jump to step 6.
2. RAW+JPEG pairing check (filename stem + timestamp proximity) — bond before any
   perceptual matching runs.
3. Perceptual hash computed for near-dupe candidate detection against the catalog.
   A match does **not** pause this file's import — the file proceeds through every
   remaining step normally (converted, verified, quarantined) and is simultaneously
   added to the review queue. Reviewing later means confirming a merge (which then
   quarantines the now-redundant copy) or dismissing the match. Import throughput
   never waits on human attention.
4. If the library has conversion enabled: attempt conversion per
   [Define the conversion policy](009-conversion-policy.md); verify
   (reconstruction/pixel-compare + metadata-presence check). Failure at this step
   means the file proceeds unconverted, never degraded.
5. Hash the stored bytes (for blob integrity); write to the content-addressed path;
   write the XMP sidecar.
6. Move the original into quarantine (see below), replacing the earlier reference
   only after this succeeds.

Each step's completion is written to a **per-file journal table in SQLite as it
happens**, not just at batch end — this is what makes the pipeline actually
crash-safe: on restart, an interrupted import resumes each file from its last
completed step, never re-converting or re-verifying unnecessarily, never leaving a
file half-moved.

**Quarantine always lives on the managed store's own volume, never left on the
source volume.** This is binding even though it means every import from removable
media (SD card, USB drive) requires a cross-volume copy for the quarantined
original — the retention-window guarantee from
[Adopt verify-plus-retention import safety](005-verify-plus-retention.md) is only
real if it survives the source drive being unplugged, reformatted, or returned. As
a direct consequence, **cross-volume "moves" are implemented as copy + verify +
delete-source**, not an OS-level atomic rename (Windows can't rename across
volumes) — this is forced by the quarantine-location decision, not a separate
choice.

**Retention-window enforcement: launch-only sweep.** Expired quarantine entries
are cleaned up when the app starts; no in-session timer. Longer-than-configured
retention isn't a safety problem, only a delayed reclaim of disk space — accepted
as simpler than running a periodic background sweep.

**Feeds forward into** [Draft the catalog schema](017-draft-catalog-schema.md):
the per-file import journal table, the original-hash → stored-path mapping, the
quarantine table with expiry tracking, and the review-queue entries created
in-line with (not blocking) import.
