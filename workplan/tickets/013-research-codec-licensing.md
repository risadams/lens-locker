---
id: 013
title: "Survey Windows codec sourcing and licensing"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-17)
blocked-by: []
---

## Question

For each format family beyond what the pure-Rust `image` crate covers, what is the
realistic Windows story for a **statically bundled, fully offline** Rust app:
**HEIC/HEIF** (libheif + a HEVC decoder — patent/royalty exposure of shipping HEVC
decode, how open-source apps like ImageGlass/IrfanView/darktable handle it, viability
of `libheif-rs` builds on Windows); **camera RAW** (libraw licensing — LGPL/CDDL —
static vs dynamic linking implications, `libraw`/`rawloader`/`rawler` Rust options);
**AVIF** (dav1d/libavif vs pure-Rust decode); **JPEG XL** (jxl-oxide maturity);
**SVG** (resvg). Which are safe to bundle, which need feature flags or user-supplied
codecs? Cite licenses and primary sources.

Findings file: [../research/codec-licensing.md](../research/codec-licensing.md)

## Resolution

Resolved 2026-07-17; full findings with citations in
[the findings file](../research/codec-licensing.md). Key facts for
[Lock the v1 format matrix](014-v1-format-matrix.md):

- **HEIC/HEIF: do not bundle a decoder.** HEVC patent exposure (MPEG LA, Access
  Advance, Velos Media, unpooled holders) attaches to the algorithm, not the
  implementation or its license; Access Advance offers no blanket free-software
  exemption. Recommended: delegate to Windows' own WIC/Media Foundation HEVC codec
  — fully local, so it doesn't threaten the offline guarantee, but it is
  Windows-only and must live in the platform shell per the portable-core decision
  (HEIC support then degrades gracefully on future platforms).
- **RAW: `rawler`** (LGPL-2.1, pure Rust, active, CR3 support) over stale
  `rawloader`; LibRaw static linking is only clean under its CDDL option.
- **AVIF**: safe to bundle, but the `image` crate's default avif feature is
  encode-only — decode needs C `dav1d` or patching in pure-Rust `rav1d`/`re_rav1d`
  (BSD-2-Clause) for a fully static build.
- **JPEG XL**: `jxl-oxide` (MIT/Apache-2.0) is a safe mature decoder; still no
  permissive pure-Rust encoder — consistent with
  [Survey quality-preserving image recompression options](008-research-recompression.md).
- **SVG**: `resvg` stack safe to bundle, no concerns.
- Not legal advice; unreachable primary sources (mpegla.com, jpeg.org) are flagged
  in the findings file's open questions.
