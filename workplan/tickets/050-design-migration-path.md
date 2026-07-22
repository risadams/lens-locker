---
id: 050
title: "Design migration path for existing unencrypted vaults"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [043, 044, 045, 046]
---

## Question

Users with an existing unencrypted vault (built before Locking existed)
need a path to enable it. **Reframed 2026-07-22** after
[ticket 040](040-choose-vault-mount-mechanism.md)'s revision to a
BitLocker-encrypted VHDX mount: this is no longer an "encrypt every file
in place" pass, but "create a new VHDX, bulk-copy the existing plaintext
vault's contents into its mounted volume, verify, then remove the old
plaintext originals":

- Triggered from the setup UX ([ticket 043](043-design-setup-onboarding-ux.md))
  as an "enable Locking" action on an existing library, not just at
  first-run for a brand-new vault?
- Crash-safety: this is a bulk, potentially long-running, irreversible-ish
  operation over every blob + the catalog — needs the same idempotent,
  resumable-from-any-point discipline as the import pipeline's journal
  ([ticket 010](010-import-pipeline-store-layout.md)), not a single
  all-or-nothing transaction over potentially 100k+ files.
- Does the original plaintext get verified-then-removed only after a
  confirmed-good copy exists inside the mounted VHDX (mirrors the import
  pipeline's verify-before-delete pattern), or is there a different safety
  story for this migration?
- Sizing the new VHDX: dynamic growth ([ticket 052](052-research-vhd-bitlocker-mechanics.md))
  means no need to pre-calculate an exact size, but does migration need a
  free-space precondition check before starting (the vault temporarily
  needs both the old plaintext copy and the new VHDX's consumed space
  simultaneously, until originals are removed)?
- Interaction with `ImportLock`/`AnalysisLock` — presumably migration needs
  its own exclusive lock, since it's touching every file the same way
  import does.

## Resolution

**Note**: the Question text above still references dynamic VHDX growth
sizing away the free-space precondition question — that's stale.
[Ticket 040](040-choose-vault-mount-mechanism.md) settled on **fixed-size**
VHDX after [ticket 052](052-research-vhd-bitlocker-mechanics.md) found a
real fragmentation risk with dynamic containers, which makes sizing and the
free-space precondition real questions again, resolved below.

**Triggered from the setup UX** ([ticket 043](043-design-setup-onboarding-ux.md))
as an "enable Locking" action available on an existing library, not just at
first-run for a brand-new vault — same opt-in decision point, just
reachable later too.

**Sizing**: the new VHDX is sized at the existing library's current used
bytes × a generous multiplier (e.g. 3×), measured directly before migration
starts, giving real headroom without needing
[the deferred resize operation](../LOCK-MAP.md) any time soon. A brand-new,
empty vault (nothing to measure) falls back to a reasonable flat default,
user-overridable at setup time.

**Free-space precondition**: yes — migration needs both the old plaintext
copy and the new VHDX's consumed space simultaneously until originals are
removed, so the app checks for that much free space up front and refuses
to start (with a clear "not enough free space" message, not a partial
attempt) rather than running out of disk mid-migration.

**Crash-safety and verify-before-delete: mirrors the import pipeline's
existing pattern.** A per-file journal in the catalog tracks each
blob/sidecar/catalog-row's migration status (not-started /
copied-into-VHDX / verified / original-removed), making a crash at any
point resumable — already-verified files are skipped on re-run, incomplete
ones redone, matching `crates/import`'s "crash-safe by idempotency, not by
trusting a journal step" philosophy exactly. The original plaintext file is
deleted only after a byte-verified copy exists inside the mounted VHDX —
the same verify-then-delete discipline as the import pipeline's quarantine
dance, and it stays the last, most-irreversible step.

**Locking**: migration acquires its own new exclusive lock (alongside
`ImportLock`/`AnalysisLock`), since it touches every file in the library
the same way import does and shouldn't run concurrently with either.
