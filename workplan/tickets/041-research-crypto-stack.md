---
id: 041
title: "Survey Rust crypto stack for per-file vault encryption"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-22)
blocked-by: [040]
---

## Question

Given [ticket 040](040-choose-vault-mount-mechanism.md)'s commitment to
per-file encryption at rest + decrypt-to-staging, and the confirmed
requirement for true AND multi-factor unlock (password + [the SSH-format
keypair file](038-clarify-ssh-key-meaning.md)), what is the cleanest,
current, fully-offline-buildable Rust crate stack for: per-file
authenticated encryption, combining both factors into one key, password
key-derivation, staging-directory secure cleanup, and cross-platform
staging-path selection? Must respect the project's hard constraints:
`cargo-deny`-enforced no networking crates, workspace lints deny
`unsafe_code`, and everything must build fully offline.

## Resolution

**REVISED 2026-07-22** — [ticket 040](040-choose-vault-mount-mechanism.md)
was revised to a BitLocker-encrypted VHDX mount instead of per-file
decrypt-to-staging (the staging approach's ~2x storage cost was a
dealbreaker). What survives from this research below: the **Argon2id+HKDF
two-factor combination scheme** (password + keypair-file bytes → one
combined secret) — that combined secret now becomes **BitLocker's password
protector** directly, rather than unwrapping a custom master key. What's
now unnecessary: the per-file `chacha20poly1305`/XChaCha20-Poly1305 blob
encryption and the staging-directory secure-cleanup design (BitLocker
handles bulk encryption transparently; there's no separate plaintext copy
to wipe, since detaching the volume removes access outright — see
[ticket 045](045-design-staging-directory-strategy.md)'s revised
resolution). The Argon2id parameter-tuning question
([048](048-tune-argon2-parameters.md)) and the licensing findings both
still stand unchanged. Original findings kept below for the record.

---

**Per-file AEAD**: `chacha20poly1305` (XChaCha20-Poly1305), not
`aes-gcm`. Both are NCC Group-audited RustCrypto AEADs with no
vulnerabilities found, so security parity isn't the deciding factor.
XChaCha20's 192-bit nonce makes random-nonce generation per file
collision-safe at any library size (AES-GCM's 96-bit nonce needs more
careful nonce-uniqueness discipline as file count grows), and ChaCha20 has
no AES-NI dependency, giving uniform performance across the cross-platform
future this map targets rather than depending on hardware AES support per
device.

**Key-combination scheme — build it ourselves, don't force `age`**:
`age`/`rage` (v0.12.1, dual Apache-2.0/MIT, actively maintained) does
support SSH ed25519 as a recipient type natively via `ssh-to-age`-style
conversion. But age's multi-recipient/passphrase model is explicitly
**OR-based** — any one recipient unlocks the file — which is the opposite
of the AND requirement, and age's own docs note passphrase-based
encryption specifically can't combine with multiple recipients for exactly
that reason. Cleanest path: use `ssh-to-age` (or direct OpenSSH-key
parsing) purely to extract raw key bytes from the ed25519 keypair file,
then do the AND-combination as a small amount of custom, auditable code —
`Argon2id(password)` combined via HKDF with the raw keypair bytes produces
one Key-Encryption-Key; that KEK unwraps a single random master key; the
master key drives all per-file blob (and catalog-file — see
[ticket 039](039-decide-encryption-scope.md)) encryption. Don't fight
age's recipient model against its own design intent.

**Argon2id params**: still the right password-hashing choice generally.
Server-interactive guidance (m=64MiB, t=3, p=1, ~100ms) is tuned for
per-request hashing load and is too weak here — this is a once-per-launch
desktop unlock defending an offline brute-force attempt against a stolen
disk, so it should run far heavier, comparable to local password-manager
defaults (m=256MiB+, t=3, p=4). Exact tuning is its own ticket
([048](048-tune-argon2-parameters.md)), not decided here.

**Secure delete — be honest about the limit**: multi-pass overwrite is
largely theater on modern SSDs — TRIM/wear-leveling/controller remapping
mean data can persist in remapped/over-provisioned cells invisible to the
OS regardless of overwrite passes. The correct framing for the spec is
**not** "securely wiped" but "best-effort overwrite of the staging
directory, with the real security guarantee coming from cryptographic
erasure" — the ciphertext blob store, which is what's actually exposed if
the device is imaged while locked, never contains the plaintext. This
caveat must be documented explicitly in `LOCK-SPEC.md`, not glossed over.

**Staging directory**: `directories` crate (cross-platform XDG/Known-Folder/
Standard-Directories lookup) for a persistent, platform-correct location;
avoid `tempfile`'s RAII-based cleanup (`TempDir`/`NamedTempFile`) for the
actual per-session plaintext directory, since destructor-based cleanup
doesn't survive crashes/SIGKILL — needs app-level crash-safe cleanup logic
(detect and wipe a stale staging dir on next launch) instead. Full design
in [ticket 045](045-design-staging-directory-strategy.md).

**Licensing**: `chacha20poly1305`, `argon2`, `ssh-to-age`, and `directories`
are all permissive (MIT/Apache-2.0) — no GPL exposure regardless of this
project's never-distributed status.
