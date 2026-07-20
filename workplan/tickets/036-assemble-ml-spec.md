---
id: 036
title: "Assemble the locked ML-SPEC.md"
type: workplan:prototype
status: closed
assignee: chris (grilling session 2026-07-20)
blocked-by: [023, 024, 025, 026, 028, 029, 030, 031, 032, 033, 034, 035]
---

## Question

The destination itself: fold every closed decision on this map into a single
standalone `workplan/ML-SPEC.md` — model/runtime choices, face-recognition
licensing resolution, legal/ethics findings, taxonomy design, clustering/
correction/smart-album/similarity-search UX, execution model, provisioning
story, and the schema extension — plus a milestone build plan sized for build
sessions to execute without further decisions, the same bar [ticket 018
reached](018-assemble-spec.md) for the MVP's `SPEC.md`. Drafted as a
prototype the owner reacts to, then locked. Closing this ticket completes
the map.

## Resolution

**Locked**: [ML-SPEC.md](../ML-SPEC.md) — full technical specification (13
sections, every one citing the ticket that decided it) plus a 6-milestone
build plan (Milestone ML-1 runtime/schema scaffolding through Milestone
ML-6, this effort's own ship gate).

One thing wasn't raised on any of the 12 closed tickets and needed a
concrete answer for the spec to be buildable without further decisions —
confirmed as drafted by the owner:
- **ONNX Runtime's own telemetry surface (§12)** — `SPEC.md` §8 already
  established that a bundled component can carry its own opt-in telemetry
  distinct from anything `cargo-deny`'s crate-level bans catch (WebView2's
  SmartScreen/crash-reporting needed explicit COM-level disabling); `ort`
  is exactly this shape of risk and needed a named, binding acceptance
  criterion rather than being silently assumed covered by the existing
  four layers.

**This closes [ML-MAP.md](../ML-MAP.md).** Every ticket opened on this map
(023–036) is now closed; the route from "ML auto-tagging design" as a fog
item on the MVP map to a locked, buildable spec is complete.
