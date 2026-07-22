---
id: 047
title: "Design lock/unlock lifecycle & app state integration"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [045, 046]
---

## Question

Design how locking/unlocking integrates with the existing Tauri app shell:

- Does the app gain a new `LibraryState` variant (e.g. `Locked`) alongside
  `Ready`/`NeedsSetup`, blocking on unlock at launch the same way
  `NeedsSetup` blocks on first-run configuration?
- Re-lock trigger: app close only, or also an idle timeout? (Flagged as
  fog on `LOCK-MAP.md` — sharpen it here.)
- Interaction with `ImportLock` and `AnalysisLock`
  ([ML ticket 030](030-decide-background-execution-model.md)): can import
  or background ML analysis run at all while locked (presumably not, since
  there's nothing decrypted to work on) — what happens to an in-flight
  background analysis job if the vault locks mid-run? Does locking need to
  wait for in-flight work to finish, or does it need a cancellation path?
- What the unlock UI looks like — password + keypair-file-path entry,
  error states (wrong password, missing/wrong keypair file — indistinguishable
  by design, or does the app say which one failed? consider whether
  distinguishing leaks information useful to an attacker). Per
  [ticket 043](043-design-setup-onboarding-ux.md), "key file not found at
  its configured path" is a distinct, expected error state (not a
  security-sensitive one — it's the user's own configured path, not a
  guess about vault contents) and should be surfaced clearly rather than
  folded into a generic "unlock failed."

**Note (2026-07-22)**: "unlock" and "lock" now concretely mean "attach and
mount the vault's VHDX" / "detach it" per
[ticket 045](045-design-staging-directory-strategy.md)'s revised
resolution — there's no decrypt/re-encrypt pass to sequence, just
attach-before-open-catalog and close-catalog-before-detach ordering. Real
attach/detach latency and crash-persistence behavior come from
[ticket 052](052-research-vhd-bitlocker-mechanics.md).

## Resolution

**New `LibraryState::Locked` variant.** Three states total:
`NeedsSetup` (no library configured), `Locked` (an encryption-enabled
library configured but not mounted), `Ready(AppState)` (mounted and open —
also the only state a non-encrypted vault ever needs, unchanged from
today). At launch, the app reads the plaintext vault-root status marker
([ticket 044](044-design-encrypted-blob-store-layout.md)) to pick between
them. `Locked` blocks the UI behind an unlock screen exactly the way
`NeedsSetup` already blocks behind first-run setup, via the existing
`with_ready` helper — an encryption-enabled, locked library is just
another "not ready" state, not a special case threaded through every
command.

**Re-lock trigger: app close only, no idle timeout.** Matches the
confirmed threat model ([ticket 037](037-confirm-threat-model.md)) exactly
— there's no threat an idle timeout would additionally defend against, and
it would actively conflict with the requirement that other applications
keep reading the vault at a normal path (a slow external edit or export
shouldn't get yanked out from under the user by a timer with no visibility
into that activity).

**`ImportLock`/`AnalysisLock` on close: graceful, bounded wind-down, not a
full wait.** Both get signaled to stop after their current in-flight unit
(current file for import, current image for analysis,
[ML ticket 030](030-decide-background-execution-model.md)'s per-image
lock-acquisition design) rather than starting the next one. Import is
already crash-safe/resumable from its journal
([ticket 010](010-import-pipeline-store-layout.md)), so an app-close
mid-import is treated as a graceful version of the interruption it already
tolerates from a crash — it simply resumes on next unlock. Once the
current unit finishes, the connection closes and
[ticket 046](046-design-encrypted-catalog-mechanics.md)'s lock sequence
proceeds. No indefinite wait, no forced full-completion.

**Unlock UI**: password field + the keypair file's already-configured path
(no browse-and-select at unlock time — it was fixed at setup per
[ticket 043](043-design-setup-onboarding-ux.md)). Error states:
- **Key file not found at its configured path** — its own distinct,
  clearly-worded message (not security-sensitive; it's the user's own
  local file layout, not a guess about vault contents).
- **Key file found but the combined secret is wrong** (bad password, or a
  substituted/tampered key) — a single **generic "incorrect password or
  key"** message, deliberately not distinguishing which factor was wrong.
  This is a security property, not a UX limitation: distinguishing would
  let an attacker holding one correct factor test it independently against
  a stolen vault, narrowing the search space. Both factors stay equally
  necessary to get any useful feedback.
