---
id: 001
title: "Adopt the core stack"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: []
---

## Question

The arriving brief argues for Rust + SQLite + BLAKE3 (exact dedupe) + pHash
(perceptual dedupe) and Tauri. Which of these does the map adopt as decided, and
which are re-opened?

## Resolution

Adopt everything **except the GUI shell**: Rust for the core, SQLite as the single
embedded catalog (relational queries over images/tags/hashes, zero network surface),
BLAKE3 for exact content hashing, and a 64-bit perceptual hash compared by Hamming
distance for near-duplicate detection. The GUI shell (Tauri vs pure-Rust
alternatives) is deliberately excluded and re-opened as its own decision — it shapes
the offline attack surface, binary size, and every UI milestone. See
[Choose the GUI shell](007-choose-gui-shell.md).
