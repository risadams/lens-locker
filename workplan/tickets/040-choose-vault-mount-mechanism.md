---
id: 040
title: "Choose the vault mount mechanism"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [039]
---

## Question

"Readable on disk from other applications via a standard file path" while
also "encrypted and not viewable on disk" implies a transparent
decrypt-on-mount layer of some kind — Windows has no native FUSE, so this
needs real technical grounding. Candidates: a native Windows virtual
encrypted disk (VHD + BitLocker), a third-party virtual-filesystem driver
(WinFsp/Dokan), or decrypting to a plaintext staging directory on unlock
and wiping it on close. Given the confirmed [threat
model](037-confirm-threat-model.md) (offline theft while closed, not
malware-while-unlocked), which mechanism is cleanest — and should the
choice stay open as a research ticket, or is it decidable now given other
constraints?

A late addition sharpened this further: the user wants to keep a
**cross-platform future** open, and wants the vault to work when **hosted
inside a cloud-sync folder** (Dropbox/OneDrive/Syncthing-style). Both
rule out the virtual-disk and virtual-filesystem-driver options outright.

## Resolution

**REVISED 2026-07-22** (during [ticket 045](045-design-staging-directory-strategy.md)'s
grilling): the original per-file decrypt-to-staging resolution below turned
out to require ~2x disk space while unlocked (ciphertext store + full
plaintext staging copy simultaneously) — a stated dealbreaker for
100k+-image libraries. Revisiting with the user surfaced that
cross-platform-future and cloud-sync-folder hosting, the constraints that
disqualified VHD+BitLocker the first time, are **acceptable to drop** in
exchange for a materially simpler and storage-efficient implementation.
**New decision: a dynamically-expanding VHDX container, BitLocker-encrypted,
mounted via the Windows Virtual Disk API using the app's combined
password+keypair secret as BitLocker's password protector.** BitLocker
itself only ever sees one opaque combined secret (still produced by the
Argon2id+HKDF combination scheme from
[ticket 041](041-research-crypto-stack.md)), so true AND-semantics are
preserved — enforced by the app before BitLocker is ever invoked, not by
BitLocker's own (OR-based) multi-protector model. Mounted, the volume is a
real folder any application can read normally; detaching on lock makes it
inaccessible instantly, with **no bulk decrypt/re-encrypt step and no
duplicated storage** — Windows handles block-level encryption
transparently. This is Windows-only going forward for this feature (see
`LOCK-MAP.md`'s revised Notes/Out-of-scope). Real API/edition/privilege
mechanics are verified in the new
[ticket 052](052-research-vhd-bitlocker-mechanics.md) rather than assumed.

This cascades: [041](041-research-crypto-stack.md)'s per-file
XChaCha20-Poly1305 blob-encryption recommendation is no longer needed
(BitLocker handles bulk encryption; the KEK-combination scheme survives),
[044](044-design-encrypted-blob-store-layout.md)'s custom on-disk format is
moot (blobs are ordinary files inside the mounted volume), and
[045](045-design-staging-directory-strategy.md)/[046](046-design-encrypted-catalog-mechanics.md)/[047](047-design-lock-unlock-lifecycle.md)
are reframed around mount/unmount rather than decrypt-to-copy/wipe. All
updated accordingly.

**Further revision 2026-07-22** (after [ticket 052](052-research-vhd-bitlocker-mechanics.md)'s
research landed): two real blockers surfaced — BitLocker doesn't exist on
Windows Home at all (Pro/Enterprise/Education only, no VHD-capable
substitute), and mounting a VHD requires administrator elevation as a
general Windows behavior (BitLocker management requires it too; whether
routine unlock of an already-configured volume also needs it remains
undocumented). Resolved with the user directly:

- **Windows Home is explicitly unsupported for Locking.** No fallback
  encryption path for Home is being designed — this feature requires
  Windows Pro/Enterprise/Education. `LOCK-SPEC.md` must state this
  plainly, and the setup UX ([ticket 043](043-design-setup-onboarding-ux.md))
  should detect and clearly explain the restriction rather than failing
  confusingly if attempted on Home.
- **Elevation is handled by a small, separate elevated helper process, not
  by running the whole app elevated.** The main LensLocker process (UI,
  catalog, IPC, WebView2) stays unprivileged exactly as it does today; a
  minimal helper binary is launched with a UAC prompt only to perform
  `AttachVirtualDisk`/`DetachVirtualDisk` and the BitLocker unlock call,
  then exits. This mirrors the existing `raw-worker` precedent of isolating
  a risky/native operation into its own subprocess rather than growing the
  main process's privilege or trust surface. At most one UAC prompt per
  unlock (attach) and one per lock (detach), not a standing elevated
  session. Feeds directly into [ticket 047](047-design-lock-unlock-lifecycle.md)'s
  lifecycle design.

Also noted for the spec: dynamic VHDX growth carries a real, documented
fragmentation/performance-degradation risk under a many-small-files
workload at 100k+ scale (production guidance generally favors fixed-size
VHDX for sustained I/O). Given this project's "quality/scale is never
silently sacrificed" norm, `LOCK-SPEC.md` should default to a **fixed-size
VHDX sized generously at vault-encryption time** (not dynamic), accepting
that growing the vault later is a real, non-trivial maintenance operation
— flagged as fog below rather than solved here.

---

**Original resolution (superseded above), kept for the record:**

Per-file encryption at rest + decrypt-to-a-plaintext-staging-directory on
unlock, best-effort wiped on close. No virtual filesystem, no virtual
disk, no kernel driver.

Rejected alternatives and why:
- **VHD + BitLocker** — Windows-only API, doesn't survive a cross-platform
  future at all. Also a poor fit for a cloud-synced vault: a VHD is one
  large container file, so any internal change re-uploads the *whole*
  container to whatever sync client is watching that folder — slow, and
  risks container corruption if sync uploads mid-write.
- **WinFsp/Dokan-style virtual filesystems** — need a signed kernel driver
  *per OS* (Dokan on Windows, macFUSE on macOS, FUSE on Linux) — three
  driver dependencies to build and maintain instead of one, directly
  against "portable core."

Why decrypt-to-staging won on pure engineering grounds, at the time: it's
pure application-level Rust crypto, no OS-specific driver anywhere — the
same code decrypts to a staging directory on Windows, macOS, or Linux
unchanged. It's also the cloud-sync-friendly shape: small,
independently-encrypted per-file blobs (mirroring the store's existing
content-addressed layout) sync incrementally, rather than one monolithic
container re-uploading on every write. And it matched the confirmed threat
model exactly — plaintext simply doesn't exist on disk during the window
the attacker is assumed to have (device stolen/imaged while the app is
closed). All still true — just outweighed by the storage cost once that
tradeoff was made concrete.
