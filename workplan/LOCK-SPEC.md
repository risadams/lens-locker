# LensLocker Locking — Technical Specification & Build Plan

Status: **LOCKED** (2026-07-22). Assembled from every closed ticket on
[the Locking work-plan map](LOCK-MAP.md); each section cites the ticket that
decided it. This document is the destination — a build session should be able
to execute the milestones below without deciding anything further.

## 1. What Locking is

At-rest encryption of an entire LensLocker vault — blob store, SQLite
catalog, XMP sidecars, quarantine, all of it, not just image bytes — unlocked
by **both** a user password **and** a per-vault, install-time-chosen ed25519
keypair file stored outside the vault. True AND semantics: losing either
factor permanently and unrecoverably locks the vault, by design.
([037](tickets/037-confirm-threat-model.md),
[039](tickets/039-decide-encryption-scope.md),
[042](tickets/042-decide-lost-factor-recovery-policy.md))

**Threat model**: offline/at-rest device theft or imaging while the app is
closed. Malware or another process reading plaintext while the vault is
unlocked is an accepted, explicitly out-of-scope risk — structurally
required by the vault being readable at a normal file path by other
applications while unlocked. ([037](tickets/037-confirm-threat-model.md))

**Non-negotiable constraints:**
- **Zero network access, ever** — inherited unchanged from `SPEC.md`. See §10
  for a Locking-specific addition to the offline-enforcement checklist.
- **No recovery mechanism.** Losing the password or the keypair file
  permanently locks the vault. No escrow, no recovery key, no bypass.
  ([042](tickets/042-decide-lost-factor-recovery-policy.md))
- **Windows Pro/Enterprise/Education only — Windows Home is explicitly
  unsupported**, with no fallback encryption path. BitLocker does not exist
  on Home at all. ([052](tickets/052-research-vhd-bitlocker-mechanics.md),
  [040](tickets/040-choose-vault-mount-mechanism.md))
- **This feature is Windows-only**, diverging from `SPEC.md` §2's
  portable-core principle for this feature specifically — a deliberate trade
  to avoid a ~2x-storage cost an earlier cross-platform design required.
  ([040](tickets/040-choose-vault-mount-mechanism.md))
- Images remain exportable in plaintext via the existing export flow while
  the vault is mounted; nothing to export while locked.
  ([049](tickets/049-verify-export-flow-interaction.md))

## 2. Multi-factor key derivation

Two factors, combined at the app layer before anything else ever sees a
single secret:

1. **Password** — Argon2id-stretched. Must accept any Unicode character,
   including spaces — treated as an opaque UTF-8 byte string end to end, no
   charset restriction, no silent normalization or truncation. Minimum
   length only (12 characters recommended default), no complexity rules.
   ([038](tickets/038-clarify-ssh-key-meaning.md),
   [043](tickets/043-design-setup-onboarding-ux.md))
2. **Keypair file** — a locally-generated ed25519 keypair in SSH-familiar
   file format (the same shape `ssh-keygen -t ed25519` produces), used
   purely as a local possession factor. No SSH protocol, transport, or
   agent involvement anywhere. Generated **per-vault** (not per-installation)
   at the moment that vault opts into encryption, at a **user-chosen
   primary location** outside the vault, which the app persists (keyed to
   the vault) so it knows where to look at unlock time.
   ([038](tickets/038-clarify-ssh-key-meaning.md),
   [043](tickets/043-design-setup-onboarding-ux.md))

**Combination scheme**: `ssh-to-age` (or direct OpenSSH-key parsing)
extracts raw key bytes from the keypair file; `Argon2id(password)` is
combined with those bytes via HKDF into a single combined secret. This
combined secret becomes **BitLocker's password protector** directly (§3) —
BitLocker itself only ever sees one opaque secret, so true AND-semantics are
enforced by the app before BitLocker is ever invoked, not by BitLocker's own
(OR-based) multi-protector model. Deliberately not built on `age`'s
recipient model, which is OR-based by design and can't express an AND
requirement. ([041](tickets/041-research-crypto-stack.md))

**Argon2id parameters**: **1024 MiB memory, t=3 iterations, p=4
parallelism** — measured at **1.8 seconds** on real target hardware (Intel
Core i9-11900KF, 16 logical CPUs; see the ticket for the full comparison
table). The `argon2` crate does not multi-thread across `p_cost` regardless
of available CPU cores — these are honest single-threaded timings. Expect
longer unlock times on older/weaker hardware, bounded to single-digit
seconds on any remotely modern machine; not adaptively tuned for v1.
([048](tickets/048-tune-argon2-parameters.md))

Crates: `chacha20poly1305`, `argon2`, `ssh-to-age` — all permissive
(MIT/Apache-2.0), no GPL exposure. (Note: `chacha20poly1305` was surveyed
for an earlier per-file-encryption design that was superseded by §3 below;
kept here only if any per-file encryption remains needed outside the
BitLocker-encrypted volume — see §4's blob/quarantine layout, which needs
none. The Argon2id+HKDF combination scheme is what survives into this
spec.) ([041](tickets/041-research-crypto-stack.md))

## 3. Vault container & mount mechanism

A **fixed-size VHDX container**, BitLocker-encrypted with the combined
secret (§2) as its password protector, mounted at a **fixed NTFS folder
mount point** under the vault root (e.g. `<library-root>\live\`) — not a
drive letter. Mounted, the volume is a normal folder any application can
read at a standard path, with **zero duplicated storage**: BitLocker
decrypts transparently at the block level, there is no separate plaintext
copy anywhere. Detaching on lock makes the volume inaccessible instantly —
no bulk re-encryption step, no leftover plaintext to clean up.
([040](tickets/040-choose-vault-mount-mechanism.md),
[045](tickets/045-design-staging-directory-strategy.md))

**Fixed-size, not dynamic**: dynamic VHDX carries a real, documented
fragmentation/performance-degradation risk under a many-small-files
workload at 100k+ scale; production guidance favors fixed-size for
sustained I/O. Sized at the vault's current used bytes × a generous
multiplier (e.g. 3×) for migration of an existing library ([§9](#9-migration-path-for-existing-vaults)),
or a reasonable flat, user-overridable default for a brand-new empty vault.
Growing an already-fixed vault later is real, non-trivial future work,
deliberately not designed now. ([040](tickets/040-choose-vault-mount-mechanism.md),
[052](tickets/052-research-vhd-bitlocker-mechanics.md),
[050](tickets/050-design-migration-path.md))

**Elevation**: mounting a VHD and enabling/unlocking BitLocker both require
administrator elevation as a general Windows behavior. Handled by a **small,
separately-elevated helper process**, not by running the whole app
elevated — the main LensLocker process (UI, catalog, IPC, WebView2) stays
unprivileged; the helper is launched with a UAC prompt only to perform the
attach/detach and BitLocker-unlock calls, then exits. This mirrors
`raw-worker`'s existing precedent of isolating a risky/native operation into
its own subprocess rather than growing the main process's privilege or
trust surface. At most one UAC prompt per unlock and one per lock, not a
standing elevated session. ([040](tickets/040-choose-vault-mount-mechanism.md),
[052](tickets/052-research-vhd-bitlocker-mechanics.md))

**API surface**: `windows-rs`'s raw FFI to `CreateVirtualDisk`/
`AttachVirtualDisk`/`DetachVirtualDisk` (`windows::Win32::Storage::Vhd`),
optionally through the community crate `virtdisk-rs` for a safer wrapper —
expect a justified `#[allow(unsafe_code)]` module here, matching this
workspace's existing FFI-exception pattern. No Rust crate wraps BitLocker
management (protectors, unlock) — implemented via WMI/COM against the FVE
interface, or by shelling out to `manage-bde.exe`/PowerShell's
`Enable-BitLocker`/`Unlock-BitLocker`.
([052](tickets/052-research-vhd-bitlocker-mechanics.md))

**Crash recovery**: VHD attachment does not survive a full reboot (always
detaches), but is OS/session-scoped, not tied to the attaching process's
lifetime — a crashed LensLocker plausibly leaves the vault mounted and
accessible until the session ends. On launch, the app detects an
already-attached vault VHDX and **force-detaches it before allowing a fresh
unlock**, rather than attempting to silently reuse a stale mount.
([045](tickets/045-design-staging-directory-strategy.md),
[052](tickets/052-research-vhd-bitlocker-mechanics.md))

## 4. Vault-root layout

At the vault root, outside the encrypted volume: the VHDX container file
(e.g. `vault.vhdx`) and a small **plaintext status-marker file** (e.g.
`vault.json`, `{"encrypted": true, "format_version": 1}`) — readable before
any mount attempt, revealing only "this vault is locked," nothing about
contents. This is a real necessity, not a style choice: the app can't open
an encrypted catalog to check whether it's encrypted, so the marker has to
live outside the encrypted region entirely.
([044](tickets/044-design-encrypted-blob-store-layout.md))

**Inside the mounted volume**, everything is unchanged from the
unencrypted-vault design: blobs keep the existing content-addressed
`<first-2-hex>/<full-hash-hex>.<ext>` layout keyed by plaintext hash;
quarantine keeps its existing `<journal-id>/<original-filename>` path
pattern; the SQLite catalog lives at a fixed path (e.g.
`<mount-point>\catalog.db`). No custom per-file format, no nonce-prepending,
no special-casing anywhere — BitLocker's block-level encryption makes all of
it transparent to every existing dedupe/lookup/catalog-reference code path.
([044](tickets/044-design-encrypted-blob-store-layout.md),
[039](tickets/039-decide-encryption-scope.md))

## 5. App state & lock/unlock lifecycle

**New `LibraryState::Locked` variant**, alongside `NeedsSetup` and
`Ready(AppState)` (unchanged for non-encrypted vaults). At launch, the app
reads the vault-root status marker (§4) to pick between `NeedsSetup`,
`Locked`, or `Ready`. `Locked` blocks the UI behind an unlock screen exactly
the way `NeedsSetup` already blocks behind first-run setup, via the
existing `with_ready` helper. ([047](tickets/047-design-lock-unlock-lifecycle.md))

**Unlock**: password field + the keypair file's already-configured path (no
browse-and-select at unlock time). On success, the helper process (§3)
attaches and BitLocker-unlocks the VHDX, then the app opens its
`rusqlite::Connection` against the catalog file under the mount point.
Error states:
- **Key file not found at its configured path** — its own distinct,
  clearly-worded message (not security-sensitive; the user's own local file
  layout, not a guess about vault contents).
- **Key file found but the combined secret is wrong** — a single generic
  "incorrect password or key" message, deliberately not distinguishing
  which factor failed (prevents an attacker holding one correct factor from
  narrowing their search space).

**Lock**: triggered by app close only — no idle timeout, since the
confirmed threat model (§1) doesn't call for one, and a timeout would
conflict with the requirement that other apps keep reading the vault
unpredictably. Sequence: `ImportLock`/`AnalysisLock` get a **graceful,
bounded wind-down** (finish the current in-flight unit — current file for
import, current image for analysis — don't start the next; import already
tolerates this via its crash-safe journal, so this is a graceful version of
an interruption it already handles), then the catalog connection closes
cleanly, then the helper process detaches the volume. **App close blocks on
a successful lock** — "closed app ⇒ locked vault" is the feature's core
promise. If detach fails because another process still has a file open on
the mounted volume, surface a clear "still in use" error with a manual
retry action; never force-detach, never silently retry forever.
([046](tickets/046-design-encrypted-catalog-mechanics.md),
[047](tickets/047-design-lock-unlock-lifecycle.md))

**Durability**: unaffected by encryption — BitLocker is a transparent
block-level filter beneath the filesystem, functionally identical to
running SQLite on any BitLocker-encrypted physical drive. No new
crash-safety gap versus the unencrypted-vault behavior; the `sqlite-vec`
in-memory `vec0` mirror pattern needs no change, since it rebuilds from the
on-disk table at connection-open time regardless of the file's physical
path. ([046](tickets/046-design-encrypted-catalog-mechanics.md))

## 6. Setup & onboarding

**Opt-in per vault**, decided at first-run library creation (or later, via
an "enable Locking" action on an existing library — §9) — never mandatory,
never silently defaulted. Fixed for that vault's lifetime once chosen.
([043](tickets/043-design-setup-onboarding-ux.md))

Flow: keypair generated at the moment encryption is opted into, at a
user-chosen location the app persists; password entry with a confirmation
field and a strength meter shown as a nudge, not a gate (§2's minimum-length,
full-Unicode rule); a **blocking, typed-confirmation no-recovery warning**
(e.g. type "I understand this cannot be recovered") shown *after* the
keypair backup/location step, gating the "Encrypt this vault" action until
confirmed — not skimmable text. The setup UX must also detect and clearly
explain the Windows Home restriction (§1) rather than failing confusingly.
([042](tickets/042-decide-lost-factor-recovery-policy.md),
[043](tickets/043-design-setup-onboarding-ux.md))

## 7. Export

No changes needed. `export_image` (`crates/import/src/lib.rs`) reads the
catalog's `stored_path` and does a plain `fs::copy` — since blobs live at
ordinary paths under the mount point (§4), this resolves correctly
unchanged. The existing Tauri command's `LibraryState::Ready(_)` gate
already blocks export while locked for free, now that `Locked` is a
distinct state (§5) — no code change required.
([049](tickets/049-verify-export-flow-interaction.md))

## 8. Migration path for existing vaults

"Enable Locking" is reachable from the setup UX (§6) on an existing,
already-populated library, not just at first-run for a brand-new vault. The
new VHDX is sized at the library's current used bytes × a generous
multiplier, measured before migration starts; a free-space precondition
check refuses to start (clear "not enough free space" message) rather than
risk running out mid-migration, since both the old plaintext copy and the
new VHDX's consumed space are needed simultaneously until originals are
removed.
([050](tickets/050-design-migration-path.md))

**Crash-safety mirrors the import pipeline's existing pattern exactly**: a
per-file journal in the catalog tracks each blob/sidecar/catalog-row's
migration status (not-started / copied-into-VHDX / verified /
original-removed), making a crash at any point resumable — already-verified
files skipped on re-run, incomplete ones redone. The original plaintext
file is deleted only after a byte-verified copy exists inside the mounted
VHDX — the same verify-then-delete discipline as the import pipeline's
quarantine dance, staying the last, most-irreversible step. Migration
acquires its own new exclusive lock, alongside `ImportLock`/`AnalysisLock`,
and doesn't run concurrently with either.
([050](tickets/050-design-migration-path.md))

## 9. Offline enforcement — extending `SPEC.md` §8

Inherited unchanged: zero network access, `cargo-deny` bans, WFP audit,
install-time firewall rule. One Locking-specific addition, surfaced during
assembly and **verified during Milestone L4** against Microsoft's own
`Enable-BitLocker`/`Add-BitLockerKeyProtector` reference documentation, not
assumed: the app-driven BitLocker-enable flow (§3) must never trigger
Windows' own "back up your recovery key to your Microsoft account" prompt.
Confirmed this risk doesn't actually exist for this codebase's invocation
path — `-PasswordProtector`/`-Password` and `-RecoveryPasswordProtector`/
`-RecoveryKeyProtector` are mutually exclusive `Enable-BitLocker` parameter
sets (a recovery protector is only ever added by a separate, explicit
`Add-BitLockerKeyProtector` call this codebase never makes), and the
interactive recovery-key-backup screen belongs to the separate Settings/
Explorer "Turn on BitLocker" GUI wizard, not the headless cmdlet this code
actually calls. (Automatic AD escrow exists only via a domain-joined Group
Policy, inapplicable to this app's personal, non-domain-joined target.) No
code-level guard was needed beyond what `bitlocker.rs`'s `enable_bitlocker`
already did — confirmed correct-by-construction, not patched.
`-EncryptionMethod XtsAes256` and `-UsedSpaceOnly` were added to the same
call as real best-practice findings from the same verification pass
(Microsoft's docs explicitly recommend `XtsAes256` over the unspecified
default, and `-UsedSpaceOnly` is a meaningful win for a freshly-created,
mostly-unallocated fixed-size VHDX).

**Also resolved during L4**: whether *routine* unlock (not just initial
BitLocker setup) requires elevation, separately from whether enabling
BitLocker does. Already effectively answered by
[ticket 052](tickets/052-research-vhd-bitlocker-mechanics.md)'s original
research, not something L4 needed to re-investigate: mounting a VHD
requires administrator elevation as a general Windows behavior,
independent of BitLocker — so every `Attach` (§5, every unlock) already
goes through the elevated helper unconditionally, regardless of what
`Unlock-BitLocker` alone would require. There is no "routine unlock skips
elevation" case to design for; §3/§5's elevated-helper-per-unlock design
was already correct as built, not a gap needing adjustment.

## 10. Explicitly out of scope

Carried from [LOCK-MAP.md](LOCK-MAP.md)'s Out of scope section — not gaps in
this spec, deliberate exclusions:

- Recovery-key/escrow mechanisms.
- Defending against malware or other processes while the vault is unlocked.
- Windows Home support.
- Cross-platform support and cloud-sync-folder-hosted vaults, for this
  feature (the original per-file-encryption design, preserved in
  [040](tickets/040-choose-vault-mount-mechanism.md)/
  [041](tickets/041-research-crypto-stack.md)/
  [044](tickets/044-design-encrypted-blob-store-layout.md)'s ticket history,
  is the fallback starting point if this is ever reopened).
- WinFsp/Dokan-style virtual filesystems.
- Recurring/scheduled re-lock (idle timeout) — fog, not decided; sharpens if
  ever revisited.
- Growing an already-encrypted vault past its fixed VHDX size — fog,
  deliberately deferred.

---

# Part 2: Milestone Build Plan

Each milestone is sized to be handed to a build session as-is — no further
decisions required, only implementation. Assumes `SPEC.md`'s MVP milestones
(0–6) are already built.

**Milestone L0 — Vault crate scaffolding.** New `crates/vault` (package
`lenslocker-vault`): the Argon2id+HKDF combination scheme (§2), VHDX
create/attach/detach FFI wrappers (§3) via `windows-rs`/`virtdisk-rs`. A
new small subprocess binary, `vault-helper` (mirroring `raw-worker`'s
pattern), performs the actual elevated attach/detach/BitLocker-unlock
calls. `#[allow(unsafe_code)]` module(s) justified per this workspace's
existing exception pattern. Exit criteria: a standalone test creates a
fixed-size VHDX, BitLocker-encrypts it with a password protector, mounts
and unmounts it via the helper process, all without admin-elevating the
test harness itself beyond the helper's own UAC prompt.

**Milestone L1 — Setup & new-vault creation.** First-run "encrypt this
vault?" opt-in (§6), per-vault keypair generation with a user-chosen
storage path, password entry (min-length + full-Unicode validation, §2),
the blocking no-recovery confirmation, Windows-Home detection/explanation.
Creates the vault-root status marker (§4) and a fixed-size VHDX sized per
§8's flat-default rule for a brand-new empty vault. Exit criteria: create a
new encrypted vault end-to-end, confirm the resulting VHDX mounts with the
correct combined secret and refuses with a wrong one.

**Milestone L2 — Lock/unlock lifecycle.** `LibraryState::Locked` (§5), the
unlock screen and its two distinct error states, `ImportLock`/`AnalysisLock`
graceful wind-down on close, app-close blocking on successful lock,
detach-failure "still in use" handling, stale-mount force-detach on launch
(§3). Wire the tuned Argon2id parameters (§2) in for real. Exit criteria:
close and reopen the app against a real encrypted vault repeatedly,
including with a background analysis job in flight; kill the process
mid-unlock and confirm the stale-mount cleanup on next launch works.

**Milestone L3 — Migration path.** "Enable Locking" on an existing,
populated library (§9): sizing, free-space precondition, the per-file
migration journal, verify-then-remove-original. Exit criteria: migrate a
real populated library (not a toy fixture), kill the process mid-migration
at least once, confirm resume-from-journal completes correctly and no
plaintext original survives once migration reports done.

**Milestone L4 — Release-gate hardening.** Two of this milestone's three
original items are resolved (§9): the recovery-key-prompt risk was
verified against Microsoft's own docs to not exist for this codebase's
invocation path (two real hardening parameters, `-EncryptionMethod
XtsAes256`/`-UsedSpaceOnly`, added along the way), and the
routine-unlock-elevation question turned out to already be answered by
ticket 052 — VHD attach always requires elevation regardless of
BitLocker, so there's no "skip the UAC prompt" case to build. What
remains, genuinely requiring a human at the keyboard rather than more
code: **real local measurement of mount/unmount latency** (§3 — no
credible published numbers exist anywhere, per
[052](tickets/052-research-vhd-bitlocker-mechanics.md)) — time a real
attach+BitLocker-unlock and detach+lock cycle on real target hardware and
record the actual numbers, the same real-hardware-over-assumption
discipline [019](tickets/019-validate-thumbnail-grid-performance.md) and
[025](tickets/025-rebenchmark-sqlite-vec.md) already established for this
project. This milestone is Locking's ship gate — nothing before it needs
to be fully offline-verified for this feature, this one confirms it is.

---

## Judgment calls made during assembly (owner-confirmed)

Everything above traces to a closed ticket except these two, made during
assembly because the spec needed to be buildable without further decisions:

1. **Crate/module layout (§3, Milestone L0)** — `crates/vault` +
   `vault-helper` subprocess. Not decided anywhere prior as a concrete
   name/structure; a synthesis of [040](tickets/040-choose-vault-mount-mechanism.md)'s
   elevated-helper-process decision, following the existing `raw-worker`
   isolation precedent. Names/folder nesting are free to change at
   Milestone L0 — the separation of concerns (unprivileged main process,
   privileged helper) is what's locked, not the labels.
2. **BitLocker Microsoft-account recovery-key-backup suppression (§9)** —
   surfaced during assembly, not from any closed ticket. No ticket
   considered whether the automation path could accidentally trigger
   Windows' own cloud key-escrow prompt; flagged here as a hard requirement
   and a release-gate exit criterion (Milestone L4) because it's a direct
   conflict with the zero-network-ever guarantee if missed.
