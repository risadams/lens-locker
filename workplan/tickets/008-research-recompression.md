---
id: 008
title: "Survey quality-preserving image recompression options"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-17)
blocked-by: []
---

## Question

The managed store may convert file types to save space but must never sacrifice
quality. What conversions genuinely qualify? Specifically: **JPEG → JPEG XL
lossless transcode** (bit-exact reversibility guarantee, typical savings, maturity
of encoder/decoder, Rust bindings — jxl-oxide/jpegxl-rs — and cjxl CLI); **PNG →
lossless WebP / lossless JXL / lossless AVIF** (savings, decode support); TIFF/BMP
recompression; what must never be touched (camera RAW, files with unknown
ancillary data?); and **whether EXIF/XMP/ICC metadata survives each conversion
round-trip**. Cite primary sources (format specs, encoder docs).

Findings file: [../research/recompression.md](../research/recompression.md)

## Resolution

Resolved 2026-07-17; full findings with citations in
[the findings file](../research/recompression.md). Key facts for
[Define the conversion policy](009-conversion-policy.md):

- **JPEG → JPEG XL lossless transcode** is the only conversion with a spec-backed
  **byte-exact** reconstruction guarantee (libjxl `jbrd` mechanism, ~20% savings) —
  but it reliably covers baseline JPEGs only; progressive/arithmetic/CMYK and even
  some ordinary YCbCr JPEGs can fail. Policy must be *attempt → verify → keep
  original on failure*, never assumed.
- **PNG → JXL Modular (~35%) and PNG → lossless WebP (~26–42%)** are pixel-exact
  but not byte-exact; EXIF/XMP/ICC survive only with explicit per-tool flags —
  naive invocations silently strip metadata.
- **Lossless AVIF is out**: not competitive (can lose to zipped BMP), and Rust
  encoders (ravif/rav1e) don't implement AV1 lossless mode.
- **Never convert**: camera RAW, and any file carrying unrecognized ancillary data
  (cf. the PNG spec's "safe-to-copy" distinction).
- **Rust tooling gap**: no pure-Rust permissive library both encodes JXL and
  supports jbrd reconstruction — jxl-oxide/jxl-rs are decode-only; options are the
  GPL-licensed jpegxl-rs bindings or shelling out to the pre-1.0 `cjxl`/`djxl`
  CLIs. This constrains architecture and licensing and must be weighed in the
  conversion-policy decision.
