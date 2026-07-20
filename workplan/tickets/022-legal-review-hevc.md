---
id: 022
title: "Get legal review on the HEIC/HEVC delegation strategy"
type: workplan:task
status: closed
assignee: chris
blocked-by: []
---

## Question

Graduated from [Lock the v1 format matrix](014-v1-format-matrix.md): v1 handles
HEIC/HEIF by delegating to Windows' own WIC/HEVC system codec rather than bundling
a decoder, specifically to avoid the HEVC patent pools (MPEG LA, Access Advance,
Velos Media). The codec research
([Survey Windows codec sourcing and licensing](013-research-codec-licensing.md))
is engineering-facing, not a legal opinion, and explicitly flags this as needing
real review: confirm with counsel that calling the OS-provided codec — rather than
bundling one — actually avoids HEVC patent-licensing obligations for LensLocker as
the app vendor, not just for Microsoft/the OEM who already pay into the pools.

This is a HITL task: the agent cannot stand in for counsel here. Resolution is
whatever legal review actually concludes, recorded on this ticket, not a
built artifact. It gates shipping (not import-matrix design, which is already
decided), so it doesn't block [Assemble the locked spec and build plan](018-assemble-spec.md)'s
drafting — but the spec should note it as an open pre-ship gate.

## Resolution

**Dissolved by scope, not reviewed.** The owner has clarified LensLocker is
personal/internal use only, never distributed to anyone else. The patent exposure
this ticket worried about attaches specifically to *distributing* HEVC-decoding
software — per
[Survey Windows codec sourcing and licensing](013-research-codec-licensing.md),
Access Advance's own royalty-free carve-out language is framed around "software
downloaded to mobile devices or personal computers" (a distribution act), and
MPEG LA's royalty structure is unit-sold-based. Personal, non-distributed use of
software that calls Windows' own already-licensed HEVC system codec carries no
realistic patent exposure for the owner — there is no "vendor" role to review here
if nothing is ever conveyed to another party. No formal legal review is needed
under this scope.

**The technical decision is unchanged**: [Lock the v1 format matrix](014-v1-format-matrix.md)'s
choice to delegate to the OS codec and refuse (not degrade) if it's absent stands
as-is — that was already the right call independent of the legal question. Only
the compliance-gate requirement dissolves.

**Scoped to "personal use only."** Reopen this ticket if the distribution scope
ever changes.
