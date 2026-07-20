---
id: 012
title: "Decide metadata and tag portability"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: []
---

## Question

Where do tags and edited metadata live: SQLite only (fast, but locked into the app),
XMP sidecar files in the managed store, or written back into image files
(EXIF/XMP embed — mutates managed files)? What must survive a future export out of
LensLocker, and what must survive format conversion at import? Does the managed
store remain meaningful if the catalog database is lost — is that a design
requirement (rebuildable catalog) or not?

## Resolution

**SQLite is authoritative; every tag/metadata change is also mirrored to an XMP
sidecar file next to its managed image.** SQLite stays the fast path for queries,
dedupe-merge logic ([Decide the duplicate-handling experience](011-dedupe-ux.md)),
and the future embedding store; the managed image bytes themselves are never
rewritten for a tag edit — no re-encode risk on files the app has already promised
to keep verified. XMP is a real, widely-read standard (Adobe products, digiKam,
darktable all consume it), not a lossy compromise.

**Rebuild-from-sidecars is a committed v1 requirement, not just a theoretical
property.** If the `.sqlite` catalog is lost or corrupted, the app ships a recovery
path: rescan the managed store, re-hash every file (BLAKE3 + perceptual), and
re-import each sidecar's tags/metadata. This is the concrete payoff of choosing
sidecars at all — it must be built, not left implicit. Explicitly **not**
recoverable this way: dedupe-merge history and ML embeddings, which would need
recomputation. That's an accepted limitation, not a gap to close later.

**Format conversion at import must preserve EXIF/XMP/ICC — this is now a hard
requirement, not a nice-to-have.** The recompression research
([Survey quality-preserving image recompression options](008-research-recompression.md))
found metadata survival through JXL/WebP conversion is opt-in per-tool-flag, not
automatic — naive invocations silently strip it. That finding is now binding:
[Define the conversion policy](009-conversion-policy.md) must specify how each
conversion path preserves metadata, and verification (per
[Adopt verify-plus-retention import safety](005-verify-plus-retention.md)) must
confirm it survived before the original is quarantined.

**Export (future, out of scope to build now) means copying the managed file plus
its `.xmp` sidecar — sufficient for v1 scope.** In-file EXIF/XMP embedding at
export time is a post-MVP nicety; it doesn't constrain the metadata model now, so
it isn't designed for today.

**Feeds forward into**
[Define the conversion policy](009-conversion-policy.md) (metadata preservation
now binding per conversion path),
[Design the import pipeline and managed store layout](010-import-pipeline-store-layout.md)
(sidecar write timing during import), and
[Draft the catalog schema](017-draft-catalog-schema.md) (sidecar-mirror sync
tracking, rebuild-from-sidecar mapping).
