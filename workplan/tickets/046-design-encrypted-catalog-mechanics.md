---
id: 046
title: "Design encrypted SQLite catalog mechanics"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [045]
---

## Question

**Reframed 2026-07-22** after [ticket 040](040-choose-vault-mount-mechanism.md)
was revised to a BitLocker-encrypted VHDX mount
([045](045-design-staging-directory-strategy.md)): the catalog is no
longer decrypted into a staging copy — it's an ordinary SQLite file living
at a fixed path inside the mounted volume (e.g.
`<library-root>\live\catalog.db`), continuously protected by BitLocker's
block-level encryption exactly like every other file on that volume. This
significantly simplifies the original question, but a few things still
need designing:

- Connection lifecycle: the app can only open its
  `rusqlite::Connection` after the volume is mounted, and must close it
  cleanly *before* the volume is detached on lock — what does that
  sequencing look like against `AppState`/`LibraryState` in
  `src-tauri/src/lib.rs`?
- Durability: does BitLocker's transparent block-level encryption mean
  SQLite's normal WAL/journal durability guarantees are unaffected (most
  likely yes, since nothing app-level is doing extra encryption/decryption
  passes anymore) — confirm there's no new crash-safety gap introduced
  versus today's unencrypted-vault behavior.
- Interaction with `LibraryState`: does "vault locked" become a new
  explicit state alongside `Ready`/`NeedsSetup`, or is that
  [ticket 047](047-design-lock-unlock-lifecycle.md)'s call entirely?
- Any change needed to the `sqlite-vec` in-memory `vec0` mirror pattern
  ([ML ticket 024](024-decide-tagging-model-runtime.md)) given the
  underlying catalog file's on-disk path now sits under the mount point
  rather than the vault root directly?

## Resolution

**Connection lifecycle**: the `rusqlite::Connection` is opened only after
[ticket 047](047-design-lock-unlock-lifecycle.md)'s unlock flow confirms
the volume is mounted, and is explicitly closed (dropped) *before* the app
signals the elevated helper to detach — never the reverse. On lock, the
sequence is: stop accepting new catalog operations, close the connection
cleanly (WAL checkpoint happens naturally via SQLite's normal close path,
nothing extra needed), then request detach.

**Detach failure handling**: attempt a clean detach first; if Windows
refuses because another process still has a file open on the mounted
volume (Explorer thumbnailing, antivirus, an image opened directly from
the mount path in another app), **surface a clear "still in use" error**
with a manual retry action — never force-detach (risks corrupting whatever
the other process was mid-writing) and never silently retry forever.

**App close is blocked on a successful lock.** Closing the window doesn't
let the process exit until detach succeeds — since "closed app ⇒ locked
vault" is this feature's core promise, letting the app exit while leaving
the vault mounted would silently break it. If detach keeps failing, the
close action shows the same "still in use" error rather than proceeding.
A user who force-kills the process instead falls under
[ticket 045](045-design-staging-directory-strategy.md)'s stale-mount
force-detach-on-next-launch policy, same as any other crash.

**Durability**: confirmed unaffected. BitLocker is a transparent
block-level filter beneath the filesystem — functionally identical to
running SQLite on any BitLocker-encrypted physical drive, a standard,
well-supported scenario. No new crash-safety gap versus today's
unencrypted-vault behavior; SQLite's own WAL/journal guarantees are
untouched.

**`LibraryState` integration**: deferred entirely to
[ticket 047](047-design-lock-unlock-lifecycle.md), as flagged in the
original question — that ticket owns whether "locked" becomes a new
explicit state.

**`sqlite-vec` in-memory `vec0` mirror**: no design change needed. The
mirror is rebuilt from the on-disk table at connection-open time
regardless of where that file physically lives; relocating the catalog
path from the vault root to the mount point is transparent to it.
