---
id: 045
title: "Design staging directory strategy"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [041, 044]
---

## Question

[Ticket 040](040-choose-vault-mount-mechanism.md) originally committed to
decrypting into a plaintext staging directory on unlock, at a standard
path other applications can read, best-effort wiped on close. The
specifics:

- Exact path — fixed under a platform-conventional location (via the
  `directories` crate per [ticket 041](041-research-crypto-stack.md)), or
  user-configurable?
- **Eager vs. lazy decrypt**: does unlock decrypt the entire vault upfront
  (simple, but could be slow/heavy for 100k+ images), or decrypt
  on-first-access with a resident cache (faster unlock, but "other
  applications reading arbitrary files unpredictably" makes on-demand
  triggering harder to guarantee)?
- Crash-recovery: how does the app detect a stale staging directory left
  behind by a previous crash/kill on next launch, and what happens to it
  (wipe before allowing a fresh unlock, presumably)?
- What "best-effort wipe" actually does in code — confirm it's a
  documented best-effort overwrite-then-delete per
  [ticket 041](041-research-crypto-stack.md)'s honesty framing, not a
  false "securely erased" claim anywhere in the UI or spec.

**Mid-resolution pivot (2026-07-22)**: working through the eager-vs-lazy
question surfaced that lazy decryption isn't actually achievable given
[ticket 040](040-choose-vault-mount-mechanism.md)'s original mechanism
(no hook exists to decrypt-on-demand for a third-party app's unpredictable
read), which forces *eager* full-vault decryption — and eager decryption
into a *separate* staged copy costs ~2x disk space for the whole time the
vault is unlocked, a stated dealbreaker for 100k+-image libraries.
Revisiting [ticket 040](040-choose-vault-mount-mechanism.md) with that
tradeoff made concrete, the user accepted going Windows-only in exchange
for a BitLocker-encrypted VHDX mount instead — no separate staging copy at
all. This ticket's remaining job changed from "design the staging
directory" to "design the mount point," resolved below.

## Resolution

**No staging directory — the vault mounts as a folder mount point, not a
drive letter.** The VHDX container file lives at the vault root (e.g.
`<library-root>\vault.vhdx`) alongside the small plaintext status-marker
file ([ticket 044](044-design-encrypted-blob-store-layout.md)'s surviving
piece, e.g. `<library-root>\vault.json`). On unlock, the app attaches the
VHDX and mounts its volume at a fixed NTFS folder mount point under the
vault root (e.g. `<library-root>\live\`), which only resolves to real
content while attached. This is a normal path any application can browse,
with zero duplicated storage — the mounted volume *is* the live data,
transparently decrypted by BitLocker at the block level, not a copy of it.

Rejected: a drive letter — avoided for the same reason the app already
treats the library root as an arbitrary user-chosen path rather than
something drive-letter-shaped; drive letters reintroduce
environment-dependent fragility (availability, reassignment across
sessions/machines) this app's path handling otherwise avoids.

**Eager vs. lazy decrypt no longer applies** — mounting is atomic; there's
no bulk decrypt pass and no per-file access-triggered decryption to choose
between. **"Best-effort wipe" no longer applies either** — there's no
plaintext ever written outside the encrypted volume, so cryptographic
erasure is complete and immediate on detach, not a best-effort overwrite of
a leftover copy.

**Crash-recovery policy**: on launch, if the vault's VHDX is found already
attached/mounted (e.g. from a previous crash), force-detach it before
allowing a fresh unlock rather than attempting to silently reuse the stale
mount. The exact attach-persistence behavior across process crashes (does
Windows auto-detach when the attaching process dies, or can a mount
outlive it) is not assumed here — verified in the new
[ticket 052](052-research-vhd-bitlocker-mechanics.md), which this policy
depends on for its precise implementation.
