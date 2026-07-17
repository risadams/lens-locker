---
id: 009
title: "Define the conversion policy"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: [008]
---

## Question

Pin down what "conversions may save space but never sacrifice quality" means as an
enforceable policy: Is the bar **bit-exact reversibility** (original bytes
recoverable) or **pixel-lossless** (identical decoded pixels, original bytes gone)?
Which source→target conversions are allowed, per format? What is never converted
(RAW? anything with unrecognized metadata)? Is conversion opt-out per import or per
library? Decide with
[Survey quality-preserving image recompression options](008-research-recompression.md)
in hand.

**Binding constraint from [Decide metadata and tag portability](012-metadata-portability.md):**
EXIF/XMP/ICC preservation across conversion is now a hard requirement, not optional
— this policy must specify how each allowed conversion path preserves that
metadata (the recompression research found it's opt-in per-tool-flag, not
automatic), and the pipeline's verification step must confirm it survived.

## Resolution

**Reversibility bar: pixel-exact**, not universally byte-exact. "Never sacrifice
quality" means identical decoded pixels — a photo-library standard, not a
forensic/archival one. Byte-exact reconstruction is used where the tooling
provides it as a bonus verification signal (JPEG), not required elsewhere.

**Conversion matrix (v1):**

| Source | Target | Mechanism | Never-convert fallback |
|---|---|---|---|
| Baseline JPEG | JPEG XL (lossless transcode, jbrd) | `jpegxl-rs` encode + reconstruct-verify + byte-compare before the original is quarantined | If reconstruction fails or the JPEG is progressive/arithmetic/CMYK/otherwise unsupported, **leave untouched** — there is no pixel-exact fallback for JPEG that saves space, so failure means no conversion, not degraded conversion |
| PNG | JPEG XL, Modular mode (lossless) | `jpegxl-rs` encode + decode-verify (pixel compare) | If the PNG carries ancillary chunks outside the explicit allowlist (§5 of the research), leave untouched |
| BMP | JPEG XL, Modular mode (lossless) | Same as PNG | — |
| TIFF | TIFF, recompressed in place (LZW or Deflate) | Native container, tag directory (and its EXIF/XMP/ICC tags) untouched — the lowest-risk path surveyed | — |
| AVIF (lossless) | **Excluded entirely** | Not competitive on size (can lose to zipped BMP) and no Rust encoder implements AV1 lossless mode | N/A |
| Camera RAW | **Never converted** | Proprietary/undocumented sensor data; copy verbatim | N/A |
| Any file with unrecognized/unsafe-to-copy ancillary data | **Never converted** | Cannot prove round-trip safety | N/A |

**One consolidated target format** (JPEG XL) for JPEG/PNG/BMP, rather than a
second encoder (e.g. WebP) for PNG specifically — reduces the managed store to
one converted-image format, simplifying the schema, decode pipeline, and this
policy's own logic, at the cost of leaning further on libjxl's pre-1.0 maturity.
TIFF is the one deliberate exception: its native in-container recompression is
lower-risk than routing through JXL, and TIFF is a smaller share of most
libraries, so consistency-for-its-own-sake didn't win there.

**Encoder choice: link `jpegxl-rs` (GPL-3.0-or-later FFI to libjxl)** rather than
shelling out to the `cjxl`/`djxl` CLI or deferring JPEG conversion. Chosen for a
cleaner in-process Rust API over subprocess management — but this is a **real,
unresolved consequence**: GPL-3.0-or-later linkage very likely constrains
LumenVault's own distributable license. That question was previously out of scope
for this map and is now sharp enough to ticket:
[Survey GPL-3.0 linkage implications of jpegxl-rs](020-research-gpl-linkage.md) →
[Decide LumenVault's project license](021-decide-project-license.md).

**Metadata preservation is binding, per** [Decide metadata and tag portability](012-metadata-portability.md):
every conversion path must explicitly request full EXIF/XMP/ICC preservation
(never rely on tool defaults, which strip metadata unless asked) and verify its
presence post-conversion, before the original is quarantined. For JPEG→JXL, the
default `--allow_jpeg_reconstruction=1` path already keeps metadata automatically
as part of byte-exact reconstruction. For PNG→JXL, ICC handling has known rough
edges upstream (libjxl issues #4164, #2550) — verify per-file, don't assume.

**Never-convert is a hard stop, not a degrade**: whenever verification fails for
any reason (reconstruction, pixel-compare, or metadata-presence check), the file
is left in its original format and format-flagged in the catalog — the pipeline
never falls back to a lower-confidence conversion.

**Opt-out granularity: per-library, fixed at creation.** Each library has one
conversion setting (on/off) chosen when it's created; not changeable per import
and not toggleable per-file. Simplest mental model — mixed behavior means a second
library.

**Feeds forward into**
[Lock the v1 format matrix](014-v1-format-matrix.md) (this table is the conversion
half of that matrix), and
[Design the import pipeline and managed store layout](010-import-pipeline-store-layout.md)
(attempt→verify→quarantine sequencing per conversion path, and the libjxl
dependency to bundle offline).
