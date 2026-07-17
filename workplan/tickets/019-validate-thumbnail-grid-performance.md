---
id: 019
title: "Validate virtualized thumbnail-grid performance at 100k+ images"
type: workplan:prototype
status: closed
assignee: chris
blocked-by: []
---

## Question

Graduated from [Choose the GUI shell](007-choose-gui-shell.md): no primary source
benchmarked any shell — Tauri included — rendering a virtualized grid of 100k+
GPU-decoded thumbnails. Build a throwaway Tauri prototype (synthetic thumbnail set
is fine, real store layout not required) that virtualizes a grid at that scale and
measures scroll/frame performance, memory footprint, and thumbnail-decode latency
under the webview. If it doesn't hold up, this ticket is where that surfaces —
before the grid architecture is locked into the spec, not after.

## Resolution

Built and ran a real Tauri v2 prototype (100,000-item virtualized grid, 78 DOM
`<img>` elements at any time, synthetic 200×200 JPEG thumbnails). Full
methodology, numbers, and caveats:
[../research/thumbnail-grid-benchmark.md](../research/thumbnail-grid-benchmark.md).

**Verdict: holds up.** Under a realistic momentum-scroll simulation (repeated
flick-and-decay, not a synthetic linear sweep), the grid sustained a flat 60.0fps
with **zero dropped frames** across two runs, and every requested thumbnail decode
completed cleanly (2,496/2,496, ~2.6–3.1ms avg latency). An artificial worst-case
stress test (~330,000px/s linear sweep, unrealistic — reloads nearly the whole
visible grid every frame) did show real but graceful degradation: ~52–58fps,
worst frame 116ms, 1–3% of frames over 25ms. Process footprint: the app's own exe
is trivial (~30MB); the WebView2/Chromium engine itself carries an estimated
365–591MB overhead, consistent with what
[Survey Rust GUI shells for an offline image grid](006-research-gui-shells.md)
already flagged as Tauri's tradeoff against a pure-Rust shell.

**This validates [Choose the GUI shell](007-choose-gui-shell.md)'s Tauri decision
rather than calling it into question** — no reopening.

**One real bug worth carrying forward, not resolved here**: a custom
`register_uri_scheme_protocol` handler (the natural mechanism for serving
generated/on-demand thumbnails without writing 100k static files) failed silently
on Windows/WebView2 — `fetch()` returned `TypeError: Failed to fetch` with no
error surfaced through `<img>` `onerror` either. Root cause not isolated within
this ticket's time-box; the prototype worked around it via Tauri's default
static-asset pipeline instead. Whoever builds the real thumbnail-serving mechanism
for [Design the import pipeline and managed store layout](010-import-pipeline-store-layout.md)'s
thumbnail cache should budget time to investigate this, or plan to serve
thumbnails as static files (which the benchmark shows works fine) rather than via
a custom protocol.

**Feeds forward into** the still-fog "Thumbnail/preview cache strategy" item and
[Assemble the locked spec and build plan](018-assemble-spec.md).

The prototype itself was left in place (not deleted) at the session's scratchpad
path (`tauri-grid-bench/`, ~3.2GB mostly Rust build artifacts) rather than
committed anywhere, in case a human wants to actually launch
`target\release\tauri-grid-bench.exe` and watch it scroll — the numbers above are
objective instrumentation only, no human has visually confirmed the feel.
