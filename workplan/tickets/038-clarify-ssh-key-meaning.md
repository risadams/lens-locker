---
id: 038
title: "Clarify the \"SSH key\" factor's meaning"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: []
---

## Question

The feature ask calls for "an SSH key created at install time... stored as
a config file path somewhere not in the vault... that the user can back
up," used as the second unlock factor alongside a password. "SSH key" is a
loaded term that usually implies the SSH *protocol* (agent, known_hosts,
remote auth) — which has no role in a fully offline, single-machine,
zero-network app. Before designing around it: what does the user actually
mean?

## Resolution

An **asymmetric keypair in the SSH-familiar *format*** (ed25519, the same
curve `ssh-keygen -t ed25519` produces, OpenSSH-style file on disk) —
generated locally at install time, private key written to a file path
outside the vault (exact location decided in
[ticket 043](043-design-setup-onboarding-ux.md)) — used purely as a local
cryptographic **possession factor**. Never used for actual SSH transport,
agent forwarding, or any network auth of any kind; the SSH protocol itself
plays no role.

The user specifically said "SSH key" rather than "recovery key" or "key
file," signaling they want the familiar asymmetric-keypair *shape* and its
backup ergonomics, even though the protocol is irrelevant here. This
confirmed reading fed directly into
[ticket 041](041-research-crypto-stack.md)'s recommendation to extract raw
key bytes from the file (via `ssh-to-age` or direct OpenSSH parsing) rather
than using any SSH-protocol tooling.
