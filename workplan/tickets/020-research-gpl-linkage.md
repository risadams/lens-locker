---
id: 020
title: "Survey GPL-3.0 linkage implications of jpegxl-rs"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-17)
blocked-by: []
---

## Question

Graduated from [Define the conversion policy](009-conversion-policy.md): LumenVault
now links `jpegxl-rs` (GPL-3.0-or-later, FFI to libjxl) for JPEG/PNG/BMP→JPEG XL
conversion. What does that actually obligate for LumenVault's own distribution?
Specifically: does `jpegxl-rs`'s FFI link statically or dynamically against
libjxl by default, and does that distinction matter under GPL-3.0 (as it would
under LGPL)? Per the FSF's own GPL FAQ and GPL-3.0 text (primary sources — not
secondary blog summaries), what are the actual obligations on a combined work that
statically or dynamically links a GPL-3.0-or-later library — must the whole
LumenVault binary be distributed under GPL-3.0-compatible terms if shipped to
end users? Are there precedents (other closed-source or permissively-licensed Rust
desktop apps that link `jpegxl-rs` or GPL C++ libraries generally) worth surveying?
What would switching to the `cjxl`/`djxl` CLI-subprocess approach (flagged in the
recompression research as sidestepping this exact question) change about the
obligation? Not legal advice — flag clearly where real legal review is required
before this becomes a release blocker.

Findings file: [../research/gpl-linkage.md](../research/gpl-linkage.md)

## Resolution

Resolved 2026-07-17; full findings with citations in
[the findings file](../research/gpl-linkage.md). Key facts for
[Decide LumenVault's project license](021-decide-project-license.md):

- **Static vs. dynamic linking is legally irrelevant for GPL** (unlike LGPL): per
  the FSF's own GPL FAQ, either produces a "combined work" that must be conveyed
  under GPL-3.0-compatible terms as a whole. `jpegxl-rs` links dynamically by
  default; static linking is opt-in via a `vendored` feature — neither side-steps
  the obligation.
- **The obligation only triggers at conveying (distributing) a build to someone
  else** — GPLv3 §2 permits private build/run freely. LumenVault has never shipped
  a build, so nothing is urgent yet, but the spec must account for it before v1
  ships to anyone.
- **A real third option, not in the original ticket**: libjxl itself is
  BSD-3-Clause, and even the `jpegxl-src` vendoring/cmake-build crate is
  BSD-3-Clause — only the hand-written `jpegxl-sys`/`jpegxl-rs` binding code is
  GPL by its author's individual choice. Writing fresh bindings against libjxl's
  BSD C API is a viable middle path between "accept GPL" and "shell out to CLI."
- **The CLI-subprocess approach has real primary-source backing** (FSF's "mere
  aggregation" doctrine) but isn't a guaranteed-safe harbor — FSF itself calls the
  separate-vs-combined line "a legal question... judges will decide."
- **No closed-source/commercial precedent exists** among real-world `jpegxl-rs`
  consumers; small FOSS projects split between going GPL, gating it behind a
  non-default feature, or avoiding it. None show evidence of actual legal review.
