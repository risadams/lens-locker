---
id: 049
title: "Verify export-flow interaction with the encrypted vault"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: [045]
---

## Question

The original ask requires images remain exportable. With the
BitLocker-VHDX mount ([ticket 045](045-design-staging-directory-strategy.md)),
export presumably just reads from the mounted volume's file path the same
way it reads from the plaintext store today — confirm this holds with no
hidden coupling to the old vault-root-is-always-the-live-path assumption
(the live path now sits under a mount point, not the vault root directly),
and confirm export is only available while mounted (there's nothing to
export from while locked). Likely a short ticket; sharpen and close
quickly once [ticket 046](046-design-encrypted-catalog-mechanics.md) lands.

## Resolution

Confirmed against the real code, no gaps found — no changes needed beyond
what tickets 044–047 already established.

`export_image` (`crates/import/src/lib.rs:853`) reads the catalog's
`stored_path` column and does a plain `fs::copy(stored_path, &dest_file)`.
It has no hardcoded assumption about the vault root being the live path —
it just trusts whatever absolute path the catalog gives it. Since blobs
live at ordinary paths under the mount point once Locking is enabled (per
[044](044-design-encrypted-blob-store-layout.md)/[046](046-design-encrypted-catalog-mechanics.md)),
`stored_path` values written at import/migration time will already point
there, and `fs::copy` works completely unchanged.

The Tauri command wrapper (`src-tauri/src/lib.rs:1272`) already gates on
`matches!(&*state.lock().unwrap(), LibraryState::Ready(_))`, returning
`CmdError::LibraryNotConfigured` otherwise. Now that
[ticket 047](047-design-lock-unlock-lifecycle.md) makes `Locked` a distinct
`LibraryState` variant from `Ready`, this existing guard already blocks
export while locked for free — no code change required, the behavior falls
out of the state-machine design.
