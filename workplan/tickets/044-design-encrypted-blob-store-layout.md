---
id: 044
title: "Design encrypted blob store layout & keying"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [041]
---

## Question

The existing managed store is content-addressed:
`<library-root>/blobs/<first-2-hex>/<full-hash-hex>.<ext>`, keyed by the
pre-conversion hash of the plaintext. With per-file XChaCha20-Poly1305
encryption ([ticket 041](041-research-crypto-stack.md)) now sitting on top:

- Does the on-disk filename/path scheme change at all, or does the
  ciphertext simply replace the plaintext bytes at the same path (hash
  still computed over plaintext, before encryption)?
- Where does each file's random 192-bit nonce live — prepended to the
  ciphertext file, or in a separate small sidecar/index?
- Does the managed store gain a schema/format version marker so an
  unencrypted vault and an encrypted vault are distinguishable (relevant to
  [ticket 050](050-design-migration-path.md))?
- Does quarantine (which lives inside the managed store per
  [ticket 010](010-import-pipeline-store-layout.md)) get the same per-file
  encryption treatment — presumably yes, given
  [ticket 039](039-decide-encryption-scope.md)'s whole-vault scope, but
  confirm.

## Resolution

**REVISED 2026-07-22, mostly moot** — [ticket 040](040-choose-vault-mount-mechanism.md)
was revised to a BitLocker-encrypted VHDX mount. Blobs and quarantine now
live as **ordinary, unencrypted-from-the-app's-perspective files inside the
mounted volume** — Windows/BitLocker encrypts at the block level below the
filesystem, so none of the custom per-file format work below is needed.
The path/filename scheme (content-addressed `<first-2-hex>/<full-hash-hex>.<ext>`,
quarantine at its existing path pattern) carries over completely
unchanged, with **no** nonce-prepending, no per-file ciphertext framing,
and no special-casing anywhere — exactly as it worked before Locking
existed, just now sitting on a volume that happens to be encrypted at
rest.

**One piece survives, relocated**: the app still needs to know, before
attempting to mount anything, whether a given vault is a Locking-enabled
vault at all. That's no longer a blob-store-layout concern — it's now part
of [ticket 040](040-choose-vault-mount-mechanism.md)/[045](045-design-staging-directory-strategy.md)'s
vault-root layout (the VHDX container file plus a small plaintext status
marker living outside the encrypted volume, readable before any mount
attempt). See those tickets for the current design.

Original per-file-encryption resolution (superseded, kept for the record)
is below.

---

**Path/filename scheme is unchanged.** Blobs keep
`<first-2-hex>/<full-hash-hex>.<ext>`, keyed by the hash of the *plaintext*
computed before encryption, exactly as today — only the bytes at that path
become ciphertext. Every existing dedupe/lookup/catalog-reference code path
that reasons about blob identity by plaintext hash needs no change; only
the bottom-level read/write layer needs to know encryption exists.

**Nonce is prepended to the ciphertext file itself** (`nonce || ciphertext`
in one file), not a separate sidecar or index — one file per blob stays one
file per blob, with no risk of a nonce sidecar and its ciphertext file
drifting out of sync or surviving each other's deletion. The 24-byte
XChaCha20 nonce overhead per file is negligible.

**Vault-level encryption status lives in a small plaintext marker file at
the vault root** (e.g. `<library-root>/vault.json`, `{"encrypted": true,
"format_version": 1}`), *not* inside `app_settings` in the SQLite catalog.
This is a real necessity, not a style choice: if the catalog itself is
encrypted (per [ticket 039](039-decide-encryption-scope.md)), the app
can't open it to check whether it's encrypted — that would be circular. The
marker file must be readable before any unlock attempt, so the app knows
at launch whether to show the unlock UI or open the catalog directly. It
reveals only "this vault is locked," nothing about contents, and slots in
next to the existing `Ready`/`NeedsSetup` distinction the app already makes
before a catalog connection exists.

**Quarantine gets identical treatment** — same per-file XChaCha20-Poly1305,
same nonce-prepended-to-file format, just at quarantine's existing path
pattern (`<library-root>/quarantine/<journal-id>/<original-filename>`,
keeping the original filename rather than a content hash, unchanged from
today). No special-casing needed for retention-sweep/permanent-deletion
logic — it operates on the same (now-ciphertext) files the same way.
