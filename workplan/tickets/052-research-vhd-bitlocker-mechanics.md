---
id: 052
title: "Verify VHD + BitLocker mount mechanics on Windows"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-22)
blocked-by: []
---

## Question

[Ticket 040](040-choose-vault-mount-mechanism.md) was revised 2026-07-22 to
commit to a dynamically-expanding VHDX container, BitLocker-encrypted, as
the vault's mount mechanism — after the original per-file
decrypt-to-staging-directory approach turned out to require ~2x disk space
while unlocked, a dealbreaker for 100k+-image libraries. The user accepted
dropping cross-platform/cloud-sync-folder goals for this feature in
exchange for the much simpler, storage-efficient native approach.

Before the mechanics get baked into `LOCK-SPEC.md`, verify for real (don't
assume — this project's convention is real benchmarks/prototypes over
published/assumed numbers, e.g.
[ticket 019](019-validate-thumbnail-grid-performance.md),
[ticket 025](025-rebenchmark-sqlite-vec.md)):

- **Edition/licensing requirements**: does creating/mounting a
  BitLocker-encrypted VHD via a password protector work on Windows Home,
  or does it require Pro/Enterprise/Education (full BitLocker has
  historically been Pro+; confirm current state and what a Home-edition
  user would need instead, e.g. "Device Encryption" limitations)?
- **API mechanics**: the concrete Win32/COM surface for creating a dynamic
  VHDX, formatting it NTFS, enabling BitLocker with a password protector,
  and attaching/detaching it programmatically from a Rust app (Virtual
  Disk API — `CreateVirtualDisk`/`AttachVirtualDisk`/`DetachVirtualDisk` —
  plus the BitLocker WMI/PowerShell-cmdlet surface or `bdeapi`/`fveapi`)
  — and whether a maintained Rust crate wraps any of this cleanly, or it
  needs direct FFI (relevant to this workspace's `#[allow(unsafe_code)]`
  justification pattern).
- **Privilege requirements**: does creating/attaching/detaching a
  BitLocker VHD require admin elevation, and if so, what does that mean
  for a personal desktop app that otherwise runs unprivileged?
- **Attach persistence/crash behavior**: does a VHD attached via the
  Virtual Disk API auto-detach when the attaching process exits or
  crashes, or can it persist at the OS/session level regardless — this
  directly determines whether [ticket 045](045-design-staging-directory-strategy.md)'s
  "detect and force-detach a stale mount on next launch" policy is even
  reachable, or whether crashes leave the vault silently unlocked.
- **Dynamic VHDX growth behavior**: real growth increments/performance as
  the vault fills toward 100k+ images — any surprises versus the
  documented behavior.
- **Mount/unmount latency**: real numbers for attach+unlock and
  detach+lock on realistic hardware, since these now sit directly in the
  app's launch/close path.

Findings should directly confirm or revise
[ticket 040](040-choose-vault-mount-mechanism.md)'s mechanism choice, and
feed concrete answers into [045](045-design-staging-directory-strategy.md),
[046](046-design-encrypted-catalog-mechanics.md), and
[047](047-design-lock-unlock-lifecycle.md).

## Resolution

**Edition/licensing — hard blocker, resolved by scoping.** BitLocker
(including BitLocker-on-VHD via a password protector) is **not available
on Windows Home** — Pro/Enterprise/Education only ([Microsoft
Q&A](https://learn.microsoft.com/en-us/answers/questions/5855684/unable-to-enable-bitlocker-on-windows-home-edition),
[BitLocker
FAQ](https://learn.microsoft.com/en-us/windows/security/operating-system-security/data-protection/bitlocker/faq)).
Home's "Device Encryption" substitute has no VHD support and no
password-protector model — not a usable fallback. **Resolved in
[ticket 040](040-choose-vault-mount-mechanism.md): Windows Home is
explicitly unsupported for Locking**, no fallback path designed.

**API surface**: `windows-rs` (official `microsoft/windows-rs`) exposes raw
unsafe FFI to `CreateVirtualDisk`/`AttachVirtualDisk`/`OpenVirtualDisk`/
`SetVirtualDiskInformation` in `windows::Win32::Storage::Vhd`. The
community crate [`virtdisk-rs`](https://lib.rs/crates/virtdisk-rs) wraps
these more safely but doesn't eliminate the unsafe surface entirely —
expect a justified `#[allow(unsafe_code)]` module here, matching this
workspace's existing pattern. **No Rust crate wraps BitLocker management**
(protectors, unlock) at all — needs direct WMI/COM calls against the FVE
interface, or shelling out to `manage-bde.exe`/PowerShell's
`Enable-BitLocker`/`Unlock-BitLocker` via `std::process::Command`.

**Privilege requirements — second real blocker, resolved by
architecture.** Both mounting a VHD ([confirmed
admin-required](https://www.diskinternals.com/vmfs-recovery/how-to-mount-vhd-files-in-windows-10/))
and enabling BitLocker (`manage-bde`) require administrator elevation.
Whether routine *unlock* of an already-configured BitLocker VHD also
requires elevation is genuinely undocumented/ambiguous — needs a real
local test before implementation, not an assumption. **Resolved in
[ticket 040](040-choose-vault-mount-mechanism.md): a small elevated helper
process** (mirroring `raw-worker`'s isolation-of-risky-native-operations
precedent) handles just the attach/detach/BitLocker-unlock calls, prompting
UAC once per lock/unlock rather than running the whole app elevated.

**Attach persistence**: confirmed VHD attachment does **not** survive a
full reboot — always detaches
([docs](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/attach-vdisk)).
Behavior across a process crash *without* reboot isn't directly documented,
but attachment is OS/session-scoped, not tied to the calling process's
lifetime — a crashed LensLocker plausibly leaves the vault mounted and
accessible until the session ends. This **validates** (doesn't undermine)
[ticket 045](045-design-staging-directory-strategy.md)'s "force-detach any
already-attached VHDX found on launch" policy — it's a necessary safeguard,
not a hypothetical one.

**Dynamic VHDX growth — real caveat, not a non-issue.** Multiple sources
describe dynamic VHD/VHDX causing genuine fragmentation and performance
degradation over time under many-small-files workloads; production
guidance generally favors fixed-size VHDX for sustained I/O. A 100k+-image
library growing a dynamic container incrementally matches the exact
fragmentation-prone pattern these sources warn about. **Resolved in
[ticket 040](040-choose-vault-mount-mechanism.md): default to a
fixed-size VHDX, sized generously at encryption time**, not dynamic —
consistent with this project's "quality/scale is never silently
sacrificed" norm. Growing an already-fixed vault later is a real,
non-trivial operation, deliberately left as fog rather than solved here
(see `LOCK-MAP.md`'s Not yet specified).

**Mount/unmount latency**: no credible real-world benchmark numbers found
anywhere for BitLocker-VHD attach+unlock/detach+lock specifically — this
needs an actual local measurement during implementation, not a citation.
Flagged as an open implementation-time verification, not blocking the
architecture decision.

Sources: [BitLocker
FAQ](https://learn.microsoft.com/en-us/windows/security/operating-system-security/data-protection/bitlocker/faq),
[Unable to Enable BitLocker on Windows
Home](https://learn.microsoft.com/en-us/answers/questions/5855684/unable-to-enable-bitlocker-on-windows-home-edition),
[How Do I Enable BitLocker for
VHD](https://www.diskpart.com/articles/bitlocker-vhd-1796-gc.html),
[virtdisk-rs](https://lib.rs/crates/virtdisk-rs), [AttachVirtualDisk
docs](https://learn.microsoft.com/hu-hu/windows/win32/api/virtdisk/nf-virtdisk-attachvirtualdisk),
[attach vdisk
command](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/attach-vdisk),
[Mount VHD requires
admin](https://www.diskinternals.com/vmfs-recovery/how-to-mount-vhd-files-in-windows-10/),
[manage-bde admin
requirement](https://www.itechtics.com/manage-bitlocker-command-line/),
[Fixed vs Dynamic VHD/VHDX
Performance](https://hyper-v-backup.backupchain.com/fixed-vs-dynamic-vhd-and-vhdx-performance/).
