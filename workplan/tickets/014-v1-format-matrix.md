---
id: 014
title: "Lock the v1 format matrix"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: [013]
---

## Question

Which formats can be **imported** in v1, and at what support level (full decode +
thumbnail + metadata vs metadata/thumbnail only)? Which are deferred behind feature
flags, and which are refused at import (given the managed store takes custody, a
file it can't decode is a liability)? Decide per family — standard web formats,
TIFF/HDR/EXR, RAW, HEIC/HEIF, AVIF, JXL, SVG — with
[Survey Windows codec sourcing and licensing](013-research-codec-licensing.md) in
hand. The conversion half of this matrix (which formats get recompressed on
import, and to what) is already decided in
[Define the conversion policy](009-conversion-policy.md) — this ticket covers
import/decode support level, independent of whether a format gets converted.

## Resolution

**v1 import matrix:**

| Family | v1 support | Notes |
|---|---|---|
| Standard web (JPEG, PNG, WebP, GIF, BMP) | Full | Native `image` crate decode |
| TIFF | Full | Native decode; recompressed in place per [Define the conversion policy](009-conversion-policy.md) |
| HDR, EXR | **Deferred to post-v1** | VFX/render formats, small fraction of typical photo libraries, outside the codec research's risk analysis — not worth importing custody-taking decode support for formats nobody's vetted yet |
| Camera RAW (CR2/NEF/ARW/DNG/RW2/CR3, etc.) | Full | Bundled `rawler` (pure Rust); decode runs in an isolated worker process/panic-catching boundary, since rawler's own maintainers flag it as not hardened against malformed input |
| HEIC/HEIF | **Conditional** | Delegate to Windows' own WIC/HEVC system codec when installed (fully offline, sidesteps the HEVC patent pools entirely — no bundled decoder). **If the OS codec is absent, refuse the file at import**, with a message pointing the user at the free Windows HEVC Video Extensions |
| AVIF | Full | Decode via the pure-Rust `re_rav1d` patch attempted first (keeps the no-C-toolchain goal); falls back to the C `dav1d` (`avif-native`) path if that doesn't build cleanly on Windows — a build-verification task for the build session, not a v1 exclusion |
| JPEG XL | Full | Decode via `jxl-oxide` (pure Rust); encode already decided (`jpegxl-rs`, GPL) for conversion-produced files per [Define the conversion policy](009-conversion-policy.md) |
| SVG | Full | `resvg`/`usvg`/`tiny-skia` rasterizes to a thumbnail like any other image; the perceptual hash runs against that rasterized bitmap, same as any raster format |

**HEIC refuse-on-missing-codec, not degrade-to-thumbnail**: chosen over importing a
metadata/embedded-thumbnail-only stub, to keep the format matrix's underlying
principle intact — the managed store shouldn't take custody of a file it can't
actually decode. A refused file is not destructive (nothing is moved or converted),
so the user loses nothing by retrying after installing the OS extension.

**Open compliance gate, not resolved here**: the codec research explicitly flagged
that the HEIC delegate-to-OS-codec strategy needs real legal review before ship —
this survey is engineering-facing, not a legal opinion. That's now sharp enough to
ticket: [Get legal review on the HEIC/HEVC delegation strategy](022-legal-review-hevc.md).

**Feeds forward into** [Assemble the locked spec and build plan](018-assemble-spec.md)
(this table is the v1 format matrix section) and the import pipeline's decode
layer (RAW sandboxing, HEIC OS-codec detection, AVIF patch-with-fallback).
