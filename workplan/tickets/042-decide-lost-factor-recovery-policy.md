---
id: 042
title: "Decide lost-factor recovery policy"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [038]
---

## Question

With true AND-semantics (both password and keypair file required, no OR
bypass), losing either factor is unrecoverable by construction unless a
third escape hatch exists. The feature ask never mentioned a recovery
path. Should losing the password or the keypair file be permanently,
unrecoverably fatal to the vault — or should the design add a recovery
mechanism (e.g. a printable/exportable recovery key that alone can
unlock, bypassing the AND requirement)?

## Resolution

**No recovery mechanism, by design.** Losing either the password or the
keypair file permanently and unrecoverably locks the vault. This is the
honest consequence of "true multi-factor, no bypass" — a recovery key
would itself be a single secret sufficient to unlock, which weakens the
AND guarantee this whole feature exists to provide.

This differs from the app's existing "destructive actions are always
recoverable" norm (`CLAUDE.md`, the quarantine/retention-window pattern) —
that guarantee is about recovering from *accidental deletion*, not from
*lost credentials*, and the two are not the same promise.
[Ticket 043](043-design-setup-onboarding-ux.md) is responsible for making
this unrecoverability loud and explicit at setup time, since it's the
sharp edge of the whole feature and a user could otherwise reasonably
assume "locked" means "recoverable" the way everything else in this app
is.

No recovery-key/escrow mechanism is in scope for this map — see LOCK-MAP's
Out of scope section.
