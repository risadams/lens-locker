---
id: 007
title: "Choose the GUI shell"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: [006]
---

## Question

Tauri (webview, best UI velocity, larger attack surface to neutralize) vs a
pure-Rust GUI (egui/iced/Slint — smaller offline surface, less polish headroom):
which shell does LumenVault commit to for v1? Decide with the survey findings from
[Survey Rust GUI shells for an offline image grid](006-research-gui-shells.md) in
hand, weighing: thumbnail-grid performance at 100k+ images, offline
enforceability, binary size, and how much UI iteration speed matters to the owner.

## Resolution

**Tauri v2**, with the WebView2 network-behavior gap closed by a binding v1
mitigation package rather than accepted as residual risk. UI iteration speed and
the size/maturity of the Tauri ecosystem outweigh the structural
"provable-by-dependency-graph" cleanliness of a pure-Rust shell — the owner traded
that theoretical cleanliness for velocity, on condition the gap is actually closed
in practice.

**Binding v1 requirement — all four layers from
[Survey zero-network enforcement options](015-research-network-enforcement.md),
none deferred:**
1. WebView2 COM settings disabling `IsReputationCheckingRequired` (SmartScreen) and
   `IsCustomCrashReportingEnabled` (crash minidump upload to Microsoft).
2. `cargo-deny` `[bans]`/`[sources]` rules banning network crates (reqwest, hyper,
   tokio net features, etc., transitively), enforced as a CI gate.
3. A runtime WFP connection-audit check (Windows Security events 5156/5157) run at
   least once before release as an empirical proof, not just a static audit.
4. An install-time Windows Firewall outbound-block rule on the app's executable, as
   a regression catcher.

This checklist is a direct input to
[Design the import pipeline and managed store layout](010-import-pipeline-store-layout.md)
(nothing there, but the installer/release process must implement layers 1 and 4)
and to [Assemble the locked spec and build plan](018-assemble-spec.md), which must
document all four as v1 acceptance criteria, not aspirational hardening.

**Follow-up ticket opened**: no primary source benchmarked any of the four shells
at real 100k+ item grid scale, and Tauri's own maturity there is unproven for this
app's specific case (virtualized grid + GPU-decoded thumbnails inside a webview).
See [Validate virtualized thumbnail-grid performance at 100k+ images](019-validate-thumbnail-grid-performance.md).
