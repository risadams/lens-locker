---
id: 039
title: "Decide encryption scope: blob-only vs. whole-vault"
type: workplan:grilling
status: closed
assignee: chris (grilling session 2026-07-22)
blocked-by: []
---

## Question

The vault has three kinds of on-disk data: the content-addressed **blob
store** (actual image bytes), the **SQLite catalog** (filenames, tags,
paths, dedupe history, ML embeddings/face data — the single source of
truth), and **XMP sidecars** (mirrored tags/metadata, human-readable by
design). Does "not viewable on disk" mean encrypting everything under the
vault root, or just the image bytes in the blob store?

## Resolution

**Everything under the vault root** — blobs, the SQLite catalog file, and
XMP sidecars all become unreadable without unlocking.

A blob-only scheme would leave every filename, tag, dedupe relationship,
and face-cluster label sitting in the clear on a stolen disk — for a photo
library, the metadata layer is often more sensitive than the pixels, and
"my vault is locked" would be a misleading claim if only image bytes were
protected.

This is the decision that makes
[encrypted-catalog mechanics](046-design-encrypted-catalog-mechanics.md) its
own real ticket rather than an afterthought: the app's existing
single-connection-per-library pattern needs to open the SQLite connection
against a decrypted view, not the raw vault-root path, which is a genuine
architectural change, not just "encrypt some files."
