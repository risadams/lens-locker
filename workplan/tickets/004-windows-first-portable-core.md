---
id: 004
title: "Target Windows-first with a portable core"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: []
---

## Question

Which platforms must the v1 spec commit to?

## Resolution

**Windows-first for v1; the core stays portable.** Packaging, codec sourcing, and
installer work cover Windows only. Core crates must remain OS-agnostic — no
Windows-only APIs outside a thin platform shell — so Linux/macOS can follow later
without a rewrite. (Their v1 packaging is explicitly out of scope on the map.)
