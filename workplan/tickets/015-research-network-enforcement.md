---
id: 015
title: "Survey zero-network enforcement options"
type: workplan:research
status: closed
assignee: research-subagent (fired 2026-07-17)
blocked-by: []
---

## Question

"100% offline" should be enforced and verifiable, not aspirational. What layers are
available for a Rust desktop app on Windows, and what does each actually catch:
build-time (cargo-deny/cargo-vet banning network crates, auditing transitive deps);
shell-level (Tauri v2 capability/permission config vs what a WebView2 webview does
on its own — telemetry, safe-browsing, font/CDN fetches — and whether that differs
for pure-Rust shells); OS-level (Windows Firewall outbound rules per-exe, WFP,
AppContainer); and test-time verification (network-namespace/loopback-only CI
checks, runtime socket monitoring). Recommend a layered checklist the spec can
adopt. Cite primary sources.

Findings file: [../research/network-enforcement.md](../research/network-enforcement.md)

## Resolution

Resolved 2026-07-17; full layered checklist with citations in
[the findings file](../research/network-enforcement.md).

- **Load-bearing chain**: (1) omit network crates entirely — absence beats
  permission; (2) `cargo-deny` bans/sources rules in CI as automated backstop;
  (3) runtime WFP connection auditing (Windows Security events 5156/5157) as
  empirical proof — the only layer that catches raw syscalls, shelled-out
  processes, and webview traffic; (4) installer-level Windows Firewall outbound
  block as a regression catcher.
- **Critical gap if Tauri is chosen**: Tauri capabilities/CSP govern only the
  JS↔Rust IPC bridge. WebView2 makes SmartScreen reputation checks and sends
  crash minidumps to Microsoft regardless of Tauri config; suppressing them
  requires the documented `IsReputationCheckingRequired` /
  `IsCustomCrashReportingEnabled` COM settings.
- **Belt-and-braces only**: cargo-vet (provenance, not behavior), AppContainer/
  LPAC (needs MSIX or hand-rolled plumbing for plain Win32; Win32 App Isolation
  still preview), ETW/Procmon (diagnosis, not CI gate).
- **Direct input to [Choose the GUI shell](007-choose-gui-shell.md)**: a pure-Rust
  shell eliminates the whole WebView2 problem class outright.
- Open questions logged: shell-decision interaction, MSIX vs plain exe packaging
  (affects AppContainer feasibility), CI mechanics for the WFP-audit gate.
