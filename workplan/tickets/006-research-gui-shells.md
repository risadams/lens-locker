---
id: 006
title: "Survey Rust GUI shells for an offline image grid"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-17)
blocked-by: []
---

## Question

For a Windows-first, 100% offline desktop app whose central view is a virtualized
grid of 100k+ thumbnails: how do **Tauri (v2)**, **egui**, **iced**, and **Slint**
compare today on — smooth large-grid/virtualized-list rendering and GPU-accelerated
image display; binary size and memory footprint; maturity/stability on Windows;
accessibility and i18n; and how completely network access can be disabled or proven
absent (webview update/network behavior included)? What do real image-heavy apps
built with each look like?

Findings file: [../research/gui-shells.md](../research/gui-shells.md)

## Resolution

Resolved 2026-07-17; full findings with citations in
[the findings file](../research/gui-shells.md). Key facts for
[Choose the GUI shell](007-choose-gui-shell.md):

- **Tauri v2**: fastest UI iteration; grid virtualization is a solved web problem.
  But its capability/CSP system governs only app-layer commands and page content —
  WebView2's own telemetry, SmartScreen, and crash reporting sit outside Tauri's
  reach (open, unresolved upstream issue). Offline WebView2 distribution is
  achievable (offline/fixed-version installer), but "offline" is a configuration
  checklist, not a structural property of the binary.
- **egui**: most battle-tested pure-Rust option at scale (Rerun), AccessKit
  accessibility on by default; but the built-in image loader has a documented
  unresolved RAM-ballooning issue and there is no built-in virtualized grid widget.
- **iced**: weakest maturity/accessibility — 0.14 is explicitly pre-1.0
  experimental; AccessKit integration unresolved since 2020. Precedent (COSMIC) is
  Linux-first.
- **Slint**: best out-of-box i18n, native Direct3D on Windows; partial
  accessibility gap for text widgets; requires a deliberate license choice
  (GPLv3 / royalty-free / commercial).
- **Structural point**: in the pure-Rust shells no HTTP stack is compiled in unless
  a dependency adds one — "no network" is provable via `cargo tree`, the opposite
  of Tauri's configured-offline model.
- Six open questions noted in the findings file — notably, no primary source
  benchmarks any shell at real 100k+ item scale, so the shell decision should
  consider a spike/prototype before locking.
