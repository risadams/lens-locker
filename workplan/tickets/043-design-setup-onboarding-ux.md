---
id: 043
title: "Design vault setup/onboarding UX"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [042]
---

## Question

Design the actual experience of turning Locking on:

- Is encryption opt-in per library, or mandatory for every vault going
  forward (mirrors the precedent set by the conversion opt-out in
  [ticket 009](009-conversion-policy.md), which is per-library)?
- When is the ed25519 keypair generated — at app install time (an NSIS
  installer hook, following the precedent already set for the firewall
  rule in `src-tauri/windows/hooks.nsh`) or at first-run/first-vault-setup
  time?
- Exact storage path for the keypair file (outside the vault, per the
  original ask) — a fixed app-config location, or user-choosable?
- Password-creation flow: any strength requirements, confirmation step?
- How the app surfaces the backup instructions for the keypair file, and
  whether it can detect/warn if the file goes missing on a later launch.
- The loud, explicit no-recovery warning required by
  [ticket 042](042-decide-lost-factor-recovery-policy.md) — where it
  appears and what it says.

## Resolution

**Opt-in, decided at library creation (first-run), fixed for that vault's
lifetime.** A brand-new vault is asked "encrypt this vault?" at the same
moment it's asked where to live (the existing "library location is never
silently defaulted" rule). Once chosen, it isn't a casual settings toggle
— retrofitting it onto an already-unencrypted vault is the separate,
heavier operation covered by [ticket 050](050-design-migration-path.md).

**Keypair is per-vault, not per-installation** — generated at first-run
vault-creation time, only when that specific library opts into encryption
(not at app install time, despite the feature's original wording). Each
vault is therefore **fully independent**: its own password *and* its own
keypair, no sharing across vaults on the same machine. This resolves
`LOCK-MAP.md`'s "multi-library encryption independence" fog item —
removed from Not yet specified below.

**Keypair's primary location is user-chosen per vault**, prompted during
the encryption-setup step (e.g. so it can live on a USB key day-to-day
rather than the local disk at all). The app persists that chosen path
(keyed to the vault) so it knows where to look at unlock time. This
introduces a real, distinct unlock-time failure state — "key file not
found at its configured path" — which is not the same as a wrong
password/wrong key content, and needs its own clear message rather than
being folded into a generic "unlock failed." Feeds directly into
[ticket 047](047-design-lock-unlock-lifecycle.md)'s unlock-error-states
design.

**Password requirements**: minimum length only (recommended default: 12
characters), no forced complexity rules — Argon2id stretching plus the
required second factor make complexity mandates unnecessary theater.
**Must accept any Unicode character, including spaces** — no charset
restriction, no silent normalization or truncation; the password is
treated as an opaque UTF-8 byte string end to end (generation, storage of
the Argon2id salt/params, and every future unlock attempt must handle the
full Unicode input identically). A strength meter is shown as a nudge, not
a gate. Confirmation field on entry.

**No-recovery warning**: a blocking, typed-confirmation step (e.g. type "I
understand this cannot be recovered") shown *after* the keypair
backup/location step, gating the "Encrypt this vault" action until
confirmed — not a line of text the user can skim past.
