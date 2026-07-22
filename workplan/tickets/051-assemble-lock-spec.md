---
id: 051
title: "Assemble the locked LOCK-SPEC.md"
type: workplan:prototype
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [043, 044, 045, 046, 047, 048, 049, 050]
---

## Question

The destination itself: fold every closed decision on this map into a
single standalone `workplan/LOCK-SPEC.md` — threat model, crypto stack and
key-combination scheme, encrypted blob-store/catalog layout, staging
directory strategy, lock/unlock lifecycle and app-state integration,
Argon2id parameters, setup/onboarding UX, export verification, and the
existing-vault migration path — each section citing its deciding ticket,
plus a milestone build plan sized the way `SPEC.md`'s Part 2 and
`ML-SPEC.md`'s milestone plan were. Mirrors
[ticket 018](018-assemble-spec.md) and
[ticket 036](036-assemble-ml-spec.md)'s shape and rigor.

## Resolution

**Destination reached.** [`LOCK-SPEC.md`](../LOCK-SPEC.md) locked: 10-section
spec + 5-milestone build plan (L0–L4), every section citing its deciding
ticket. Two judgment calls surfaced during assembly and recorded: the
`crates/vault` + `vault-helper` subprocess crate layout (a synthesis of
[040](040-choose-vault-mount-mechanism.md)'s elevated-helper decision,
following the existing `raw-worker` isolation precedent — not itself
separately decided anywhere), and a new requirement that the BitLocker
enablement flow must never trigger Windows' own Microsoft-account
recovery-key-backup prompt, which no prior ticket had considered but which
directly conflicts with the zero-network-ever guarantee if missed — made a
release-gate exit criterion (Milestone L4) rather than left implicit.
