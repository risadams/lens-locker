---
label: workplan:map
title: "LensLocker Locking — route to a locked spec for at-rest vault encryption"
created: 2026-07-22
tracker: TRACKER.md
---

# LensLocker Locking work-plan map

## Destination — REACHED (2026-07-22)

A locked technical spec + milestone build plan, in a standalone
`workplan/LOCK-SPEC.md`, for LensLocker's **Locking** feature — at-rest
encryption of the entire vault (blob store, SQLite catalog, XMP sidecars,
all of it, not just image bytes) via a **fixed-size VHDX container,
BitLocker-encrypted**, mounted at a fixed NTFS folder mount point (not a
drive letter) while unlocked. Unlocking requires **both** a user password
(Argon2id-stretched) **and** a per-vault, install-time-chosen ed25519
keypair file stored outside the vault (SSH-format, user-backupable) —
combined via Argon2id+HKDF into a single secret that becomes BitLocker's
password protector, giving true AND semantics enforced at the app layer
(BitLocker itself only ever sees one opaque combined secret). Attach/detach
and BitLocker unlock run inside a small, separately elevated helper process
(UAC-prompted once per lock/unlock), keeping the main app process
unprivileged.

Mounted, the vault is a normal folder any application can read at a
standard path — with **zero duplicated storage**, since BitLocker decrypts
transparently at the block level rather than the app maintaining a separate
plaintext copy. On lock, the volume is detached, making it inaccessible
instantly; there is no bulk re-encryption step and no leftover plaintext to
clean up.

**Threat model**: offline/at-rest device theft or imaging while the app is
closed. Malware or other processes reading plaintext while unlocked is an
accepted, explicitly out-of-scope risk — required by "other apps must read
the files while unlocked."

**Standing constraints**: images remain exportable in plaintext via
existing export flows while mounted; losing either factor (password or
keypair file) is permanently unrecoverable by design — no escrow/
recovery-key mechanism. **This feature requires Windows Pro/Enterprise/
Education — Windows Home is explicitly unsupported**, no fallback
encryption path designed for it (BitLocker doesn't exist there at all).
The feature is also Windows-only in general — a deliberate, informed
trade made mid-map (see Decisions so far, [ticket
040](tickets/040-choose-vault-mount-mechanism.md)) to avoid a ~2x-storage
cost that an earlier cross-platform-friendly design required; it diverges
from the MVP's general portable-core principle for this feature
specifically.

**→ [LOCK-SPEC.md](LOCK-SPEC.md) is the locked artifact.** Every ticket on
this map is closed. Building LensLocker's Locking feature is no longer a
work-plan session — it's a build session working from the spec.

## Notes

- This is a **new effort**, not a reopening of [the MVP map](MAP.md) or
  [the ML map](ML-MAP.md) — both those maps' destinations were already
  reached (`SPEC.md`, `ML-SPEC.md` locked). Locking is LensLocker's next
  major feature, chartered fresh 2026-07-22.
- Domain: local-first desktop software, Rust, applied cryptography on
  Windows. Resolve grilling tickets via `ink-and-agency:grill-me` (one
  question at a time, recommendation first) — same convention as the two
  prior maps.
- Inherited unchanged from the MVP map: **zero network access, ever**;
  **quality/destructive-action safety norms** (this feature adds a new kind
  of "recoverable" — recoverable from accidental deletion — that does
  *not* extend to lost encryption factors, which are unrecoverable by
  design per [ticket 042](tickets/042-decide-lost-factor-recovery-policy.md));
  personal/internal project, never distributed (irrelevant to this map's
  licensing posture since every crate chosen so far is permissive
  MIT/Apache-2.0 — no GPL question arises here the way it did for the
  JPEG XL encoder).
- **Diverges from the MVP map's Windows-first-with-portable-core
  principle**: this feature is Windows-only, full stop, not "ships
  Windows-only for v1 with the door left open." That's a deliberate
  trade made mid-map — see
  [ticket 040](tickets/040-choose-vault-mount-mechanism.md)'s revision —
  in exchange for a BitLocker-backed design with no storage-doubling cost.
  An earlier draft of this map targeted a cross-platform-portable, per-file
  encryption scheme instead; superseded, kept in the affected tickets'
  history for the record.
- Plan, don't do: this map produces decisions and `LOCK-SPEC.md`.
  Implementation happens after the assembly ticket closes.
- Tracker conventions: see [TRACKER.md](TRACKER.md) — shared with the MVP
  and ML maps; ticket ids continue that sequence (037+). Open tickets are
  found by query (frontier = open, unassigned, blockers all closed), not
  listed here.

## Decisions so far

- [Confirm Locking's threat model](tickets/037-confirm-threat-model.md) — offline/at-rest device theft while the app is closed; malware-while-unlocked is an accepted, out-of-scope risk (it's structurally required by "other apps read plaintext while unlocked").
- [Clarify the "SSH key" factor's meaning](tickets/038-clarify-ssh-key-meaning.md) — a locally-generated ed25519 keypair in SSH-familiar file format, used purely as an offline possession factor; no SSH protocol, transport, or agent involvement anywhere.
- [Decide encryption scope: blob-only vs. whole-vault](tickets/039-decide-encryption-scope.md) — whole vault: blob store, SQLite catalog, and XMP sidecars all encrypted at rest, not just image bytes — a metadata-only-plaintext vault would still leak filenames/tags/face-names.
- [Choose the vault mount mechanism](tickets/040-choose-vault-mount-mechanism.md) — **revised twice mid-map**: a fixed-size VHDX, BitLocker-encrypted, mounted via the Virtual Disk API using the app's combined password+keypair secret as BitLocker's password protector; detach on lock, zero duplicated storage. Superseded an earlier per-file-encryption-plus-staging-directory design once that turned out to cost ~2x disk space while unlocked. After [ticket 052](tickets/052-research-vhd-bitlocker-mechanics.md)'s research surfaced two real blockers (BitLocker doesn't exist on Windows Home; mounting a VHD requires admin elevation), resolved as: Windows Home explicitly unsupported (no fallback), and a small separately-elevated helper process handles attach/detach/unlock so the main app stays unprivileged. Dynamic VHDX's fragmentation risk at 100k+-file scale also pushed the container to fixed-size, not dynamic.
- [Survey Rust crypto stack for per-file vault encryption](tickets/041-research-crypto-stack.md) — **partially superseded**: the Argon2id+HKDF two-factor-combination scheme survives (now produces BitLocker's password protector instead of a custom master key); the per-file XChaCha20-Poly1305 blob-encryption recommendation is no longer needed now that BitLocker handles bulk encryption. Argon2id-tuning and licensing findings still stand.
- [Decide lost-factor recovery policy](tickets/042-decide-lost-factor-recovery-policy.md) — no recovery mechanism, by design: losing either the password or the keypair file permanently and unrecoverably locks the vault. No escrow/recovery-key third factor.
- [Design vault setup/onboarding UX](tickets/043-design-setup-onboarding-ux.md) — opt-in per vault, decided at first-run library creation; keypair generated per-vault (not per-installation) at that same moment, primary location user-chosen and persisted per-vault; password is minimum-length-only, full-Unicode/spaces-allowed, no complexity rules; no-recovery warning is a blocking typed-confirmation step, not skimmable text.
- [Design encrypted blob store layout & keying](tickets/044-design-encrypted-blob-store-layout.md) — **moot, superseded**: with the BitLocker-VHDX mount, blobs/quarantine are ordinary files inside the mounted volume, unchanged path scheme, no custom per-file format needed at all. The vault-root plaintext status-marker concept survives, relocated to tickets 040/045.
- [Design staging directory strategy](tickets/045-design-staging-directory-strategy.md) — **reframed mid-resolution to "design the mount point"**: no staging directory exists; the VHDX + status-marker file live at the vault root, mounted at a fixed NTFS folder mount point (not a drive letter) under the vault root. Eager/lazy-decrypt and best-effort-wipe questions no longer apply. Crash-recovery policy: force-detach any already-attached VHDX found on launch before allowing a fresh unlock — validated as a real necessity, not a hypothetical, by [ticket 052](tickets/052-research-vhd-bitlocker-mechanics.md)'s finding that attachment is OS/session-scoped, not tied to the app process's lifetime.
- [Verify VHD + BitLocker mount mechanics on Windows](tickets/052-research-vhd-bitlocker-mechanics.md) — confirmed two real blockers: BitLocker is Pro/Enterprise/Education-only (no Home fallback exists), and mounting a VHD requires admin elevation as a general Windows behavior. Also confirmed dynamic VHDX has a real fragmentation/performance risk at many-small-files scale (favor fixed-size), and that attach doesn't survive reboot but may survive a same-session process crash. No Rust crate wraps BitLocker management — needs WMI/COM or shelling out to `manage-bde`/PowerShell. No credible real-world attach/detach latency numbers exist anywhere; needs local measurement at implementation time.
- [Design encrypted SQLite catalog mechanics](tickets/046-design-encrypted-catalog-mechanics.md) — connection opens only after mount confirmed, closes cleanly before detach; on detach failure (another process still has a file open), surface a clear "still in use" error and let the user retry rather than force-detaching or silently retrying forever; app close itself blocks on a successful lock, since "closed app ⇒ locked vault" is the feature's core promise; durability/WAL guarantees are unaffected by BitLocker (transparent block-level encryption); `sqlite-vec` mirror pattern needs no change.
- [Design lock/unlock lifecycle & app state integration](tickets/047-design-lock-unlock-lifecycle.md) — new `LibraryState::Locked` variant, gated the same way `NeedsSetup` already is; re-lock is app-close only, no idle timeout (matches the confirmed threat model, avoids disrupting external apps mid-read); `ImportLock`/`AnalysisLock` get a graceful bounded wind-down on close (finish current unit, don't start the next — import already tolerates this via its crash-safe journal), not a full wait; unlock UI distinguishes "key file not found at its path" from a generic "incorrect password or key" for everything else, deliberately not revealing which factor failed (prevents narrowing an attacker's search space).
- [Tune Argon2id parameters for a desktop one-shot unlock](tickets/048-tune-argon2-parameters.md) — real benchmark on the user's actual machine (i9-11900KF, 16 logical CPUs): chosen default 1 GiB memory / t=3 / p=4 measures 1.8s. Found the `argon2` crate doesn't multi-thread across `p_cost` regardless of CPU count — these are honest single-threaded numbers, not an underused-feature artifact.
- [Verify export-flow interaction with the encrypted vault](tickets/049-verify-export-flow-interaction.md) — confirmed against the real code, no gaps: `export_image` just trusts the catalog's `stored_path` and `fs::copy`s it, which already resolves correctly under the mount point; the Tauri command's existing `LibraryState::Ready(_)` gate already blocks export while locked for free, no code change needed.
- [Design migration path for existing unencrypted vaults](tickets/050-design-migration-path.md) — "enable Locking" is available on an existing library from the setup UX, not just at first-run; new VHDX sized at current-used-bytes × a generous multiplier (flat default for empty vaults); free-space precondition check refuses to start rather than risk running out mid-migration; crash-safety and verify-before-delete mirror the import pipeline's existing journal/quarantine pattern exactly; migration gets its own new exclusive lock alongside `ImportLock`/`AnalysisLock`.
- [Assemble the locked LOCK-SPEC.md](tickets/051-assemble-lock-spec.md) — **destination reached.** [LOCK-SPEC.md](LOCK-SPEC.md) locked: 10-section spec + 5-milestone build plan (L0–L4), every section citing its deciding ticket. Two judgment calls surfaced during assembly: the `crates/vault` + `vault-helper` subprocess crate layout (synthesized from 040's helper-process decision, following the `raw-worker` isolation precedent), and a new requirement that BitLocker enablement must never trigger Windows' own Microsoft-account recovery-key-backup prompt — a direct conflict with the zero-network-ever guarantee if missed, made a Milestone L4 release-gate exit criterion.

## Not yet specified

- **Recurring/scheduled re-lock (idle timeout)** — whether the vault
  auto-locks after inactivity in addition to app-close, or close is the
  only trigger. Sharpens once
  [Design lock/unlock lifecycle & app state integration](tickets/047-design-lock-unlock-lifecycle.md)
  lands.
- **Growing an already-encrypted vault past its fixed VHDX size** — the
  fixed-size-over-dynamic decision ([ticket 040](tickets/040-choose-vault-mount-mechanism.md))
  avoids a real fragmentation risk but means the container has a ceiling;
  resizing a fixed VHDX later is a real, non-trivial operation, deliberately
  not designed now. Sharpens once real usage patterns or
  [the migration-path ticket](tickets/050-design-migration-path.md) makes
  initial sizing concrete.

## Out of scope

- **Recovery-key/escrow mechanisms** — ruled out by
  [ticket 042](tickets/042-decide-lost-factor-recovery-policy.md); a
  materially different, bigger feature with its own storage/backup/UX
  surface, and it would weaken the true-AND-multi-factor guarantee.
- **Defending against malware or other processes while the vault is
  unlocked** — structurally impossible given the requirement that other
  applications read plaintext at a standard path while unlocked; ruled out
  by [ticket 037](tickets/037-confirm-threat-model.md).
- **Windows Home support** — BitLocker doesn't exist on Home at all, and no
  fallback encryption path is being designed for it; ruled out by
  [ticket 052](tickets/052-research-vhd-bitlocker-mechanics.md)'s findings,
  resolved in [ticket 040](tickets/040-choose-vault-mount-mechanism.md).
  Locking requires Windows Pro/Enterprise/Education.
- **Cross-platform support and cloud-sync-folder-hosted vaults, for this
  feature** — an earlier draft of this map treated both as standing
  constraints and rejected VHD/BitLocker on that basis; **reversed** when
  [ticket 040](tickets/040-choose-vault-mount-mechanism.md) was revised —
  the user explicitly accepted Windows-only in exchange for avoiding a
  ~2x-storage cost. If either capability becomes a real requirement later,
  this is the ticket to reopen, and the original per-file-encryption design
  (preserved in 040/041/044's ticket history) is the fallback starting
  point.
- **WinFsp/Dokan-style virtual filesystems** — ruled out throughout (both
  the original and revised versions of
  [ticket 040](tickets/040-choose-vault-mount-mechanism.md)) due to the
  signed-kernel-driver-per-OS cost; not reconsidered by the Windows-only
  pivot, since VHD+BitLocker is strictly simpler on Windows alone too.
