---
id: 037
title: "Confirm Locking's threat model"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: []
---

## Question

The Locking feature ask says files should be "encrypted and not viewable on
disk," but that phrase means very different things depending on the
adversary. Which threat model is this actually defending against — a
stolen/lost/imaged device while the app is closed, another OS user account
on a shared machine, or malware/another process running as the user while
the vault is unlocked?

This shapes nearly every downstream decision: if malware-while-unlocked were
in scope, it would directly contradict the requirement that other
applications read plaintext at a standard file path while the vault is
unlocked.

## Resolution

**Offline/at-rest device theft or imaging while the app is closed** — a
stolen laptop, external drive, or backup, where the attacker has the disk
but not a running, logged-in, unlocked session.

Explicitly **not** in scope:
- Another OS user account on the same shared machine — already covered by
  NTFS ACLs; app-level encryption adds little a privileged local admin
  can't already bypass.
- Malware or another process reading plaintext while the vault is
  unlocked — structurally impossible to defend against given the
  requirement that other applications read the files at a normal path
  while unlocked. Accepted as a residual, out-of-scope risk rather than
  something the design pretends to solve.

This threat model is the frame every other ticket on this map designs
against — e.g. it's why [ticket 042](042-decide-lost-factor-recovery-policy.md)'s
"no recovery" answer is acceptable (the attacker never has both factors)
and why [ticket 040](040-choose-vault-mount-mechanism.md)'s staging-directory
approach (plaintext only exists during the unlocked window) is sufficient
rather than a compromise.
