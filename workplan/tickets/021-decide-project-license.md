---
id: 021
title: "Decide LumenVault's project license"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: [020]
---

## Question

Given [Survey GPL-3.0 linkage implications of jpegxl-rs](020-research-gpl-linkage.md)'s
findings: what license does LumenVault itself ship under? If GPL-3.0-or-later
linkage forces the whole distributed binary into GPL-compatible terms, is that
acceptable, or does it change the earlier encoder choice — reopening
[Define the conversion policy](009-conversion-policy.md)'s decision to link
`jpegxl-rs` rather than shell out to the `cjxl`/`djxl` CLI (which the research
flagged as the way to sidestep this obligation)? This decision should not be made
without the research in hand — the linking mechanics determine whether this is
even a real fork or a non-issue.

## Resolution

**Dissolved by scope, not chosen.** The owner has clarified LumenVault is a
personal/internal project with no distribution outside themselves — it will never
be conveyed to anyone else. Per
[Survey GPL-3.0 linkage implications of jpegxl-rs](020-research-gpl-linkage.md),
GPL-3.0's copyleft obligation triggers specifically at *conveying* a combined
work — GPLv3 §2 permits private build and use unconditionally. Since that trigger
never fires here, the `jpegxl-rs` dependency chosen in
[Define the conversion policy](009-conversion-policy.md) carries no practical
licensing obligation, and no project-wide release license needs to be set. The
encoder choice in 009 stands unchanged — it was made on technical grounds (cleaner
in-process API over subprocess management), and the license question was always a
side-constraint on top of it, not the reason for it.

**This is scoped to "personal use only," not permanent.** If the distribution
scope ever changes — open-sourcing, sharing the binary, anything conveyed to
another person — this question needs to be reopened as a fresh decision, since a
real license choice and a GPL-compatibility reckoning would then actually be
required.
