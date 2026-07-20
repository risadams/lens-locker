---
ticket: 006
title: "Rust GUI shell survey — Tauri v2 vs egui vs iced vs Slint"
date: 2026-07-17
---

# GUI shell survey for a 100k+ thumbnail grid, 100% offline

## Decision-oriented summary

The trade-off isn't "which renders a grid fastest" — all four can, with work. It's **where virtualization is already solved vs. where you build it, traded against whether "offline" is structural or configured.**

- **Tauri v2** embeds a full WebView2 (Chromium) engine. Grid virtualization is a solved web problem (`react-window`/canvas), and UI iteration is fastest. But Tauri's capability/CSP system governs *app-layer* commands and page content — not WebView2's own telemetry, SmartScreen reputation checks, or crash reporting, which are Windows/WebView2 settings outside Tauri's reach. "100% offline" becomes a checklist of things configured, not a property of the binary. Chromium also runs a network-service process by architecture, even when nothing ever dials out.
- **egui, iced, Slint** are pure-Rust with no embedded browser: if nothing in the dependency tree does networking, the binary structurally cannot — provable via `cargo tree`, not configuration. Cost: none ships a ready-made virtualized *image grid* — all three need hand-built viewport windowing plus a texture LRU. egui's own image loader is documented to balloon RAM past a handful of images, so 100k thumbnails needs manual texture lifecycle management regardless. iced is the least proven — its 0.14 (Dec 2025) is billed as "final experimental release before 1.0," and its accessibility issue has sat open since 2020. Slint has the best out-of-the-box a11y/i18n story but ships under a license (GPLv3 / royalty-free / commercial) that needs a conscious pick.

**For LensLocker**: zero-network is a standing non-negotiable constraint (see `workplan/MAP.md`) and the central view *is* the 100k+ grid. That weighs toward a pure-Rust shell (egui is the most battle-tested at large-data-scale via Rerun; Slint if its license and better a11y/i18n matter more than ecosystem size) — unless UI velocity is valued enough to accept "prove WebView2 is neutered by config" as an ongoing burden. That call belongs to [Choose the GUI shell](../tickets/007-choose-gui-shell.md), not this ticket.

---

## Tauri v2

**Rendering/GPU at 100k+**: Renders via the OS webview — mature virtualized-list libraries and canvas/WebGL image display are off-the-shelf web patterns, not built from scratch. Tauri's strongest dimension here.

**Binary size / memory**: Tauri's own size page claims small binaries but gives no numbers ([tauri.app/concept/size](https://v2.tauri.app/concept/size/)). A dated (Feb 2023, Linux-only) independent benchmark found ≈5MB for Tauri vs. 17MB (iced)/18MB (egui) hello-worlds, with the author warning not to generalize ([source](http://lukaskalbertodt.github.io/2023/02/03/tauri-iced-egui-performance-comparison.html)). Memory is dominated by WebView2, not the Rust process — "only about 5MB is the actual Tauri/Rust process, with the rest being WebView2"; a benchmark-fix issue found Tauri (125MB USS) roughly matching Electron (118MB USS) on Ubuntu once shared memory is counted correctly ([tauri-apps/tauri#5889](https://github.com/tauri-apps/tauri/issues/5889)). Chromium's network service runs as a separate utility process by default on desktop regardless of whether any request occurs ([chromium network service README](https://chromium.googlesource.com/chromium/src/+/HEAD/services/network/README.md)).

**Maturity/stability on Windows**: Tauri 2.0 went stable Oct 2024 ([blog/tauri-20](https://v2.tauri.app/blog/tauri-20/)); WebView2 is a mature, Microsoft-maintained Chromium fork preinstalled on Windows 11 and pushed to managed Windows 10 devices ([evergreen-vs-fixed-version](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/evergreen-vs-fixed-version)).

**Accessibility & i18n**: Gets Chromium's accessibility tree essentially free. i18n is whatever the frontend framework provides; WebView2's `navigator.language` has been reported to not reliably follow Windows system language, needing manual workarounds ([tauri-template i18n-patterns](https://github.com/dannysmith/tauri-template/blob/main/docs/developer/i18n-patterns.md)).

**Network disable-ability (primary focus)**:
- Capabilities/permissions gate app-layer commands and plugins (fs, http, custom `invoke`) per window — "Capabilities define which permissions are granted or denied for which windows or webviews" ([capabilities](https://v2.tauri.app/security/capabilities/), [permissions](https://v2.tauri.app/security/permissions/)). Tauri's own security-boundary list explicitly excludes "0-days or unpatched 1-days in the system WebView" — the engine itself is out of scope.
- CSP restricts what the *loaded page* can fetch, and is **off by default**: "The CSP protection is only enabled if set on the Tauri configuration file" ([csp](https://v2.tauri.app/security/csp/)).
- An open, unresolved request — "Allow to completely sandbox WebViews from network access" — confirms there is no built-in way to fully block the webview from all network protocols; suggested workarounds (strict CSP, isolation pattern, wry's unexposed `with_navigation_handler`) are partial ([tauri-apps/tauri#5755](https://github.com/tauri-apps/tauri/issues/5755)).
- WebView2 telemetry is governed by the **Windows-wide** Diagnostic Data setting, not Tauri: "Regardless of the Windows Diagnostic data setting, WebView2 collects required data that's necessary to maintain performance and reliability." SmartScreen is on by default (`IsReputationCheckingRequired`); crash minidumps go to Microsoft unless `IsCustomCrashReportingEnabled` is set ([data-privacy](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/data-privacy)). These are raw WebView2 environment options, not exposed through Tauri's own capability config.
- WebView2 **distribution** can be made fully offline: `offlineInstaller` embeds the ~127MB installer (no network at app-install time); `fixedVersion` bundles an exact pinned runtime (~180MB) that never auto-updates itself; the *default* (`downloadBootstrapper`) requires internet at install ([windows-installer](https://v2.tauri.app/distribute/windows-installer/), [evergreen-vs-fixed-version](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/evergreen-vs-fixed-version)).

**Real image/list-heavy apps**: **Spacedrive**, an open-source file explorer with thumbnail grids over "millions of files," is built on Tauri with a pure-Rust core specifically to undercut Electron's bundle size/memory ([spacedriveapp/spacedrive](https://github.com/spacedriveapp/spacedrive)).

---

## egui

**Rendering/GPU at 100k+**: `eframe` renders via `wgpu` or `glow` (GPU accelerated). No built-in virtualized grid; documented pattern is `ScrollArea::show_rows`/`show_viewport` since laying out very long content every frame is sluggish ([egui#2906](https://github.com/emilk/egui/issues/2906), [discussion #2443](https://github.com/emilk/egui/discussions/2443)). Community crates (`egui_virtual_list`, `egui_infinite_scroll`) add list virtualization; a 2D photo grid is still app code. The built-in image loader retains full decoded data indefinitely and is documented to use far more RAM than source files justify — 10×1MB images pushed memory from 140MB to 1.1GB, issue open, unresolved — so 100k thumbnails needs manual `TextureHandle`/`forget_image` management rather than the default loader ([egui#5439](https://github.com/emilk/egui/issues/5439)).

**Binary size / memory**: ≈18MB hello-world in the dated benchmark cited above (largest of the three tested). No embedded browser; idle memory is whatever `wgpu`/`glow` plus resident textures cost — an app-design question more than a framework limit.

**Maturity/stability on Windows**: Not yet 1.0 — latest is 0.35.0 (2026-06-25); egui's own README says it "works well for what it does, but it lacks many features and the interfaces are still in flux. New releases will have breaking changes" ([emilk/egui](https://github.com/emilk/egui)). Recent releases still list "improved IME features," suggesting Windows text-input edge cases are still being actively fixed.

**Accessibility & i18n**: AccessKit integration is enabled by default in `eframe`, implements native accessibility APIs on Windows and macOS, and is lazy-initialized (no cost until a screen reader asks) ([egui#2294](https://github.com/emilk/egui/pull/2294), [accesskit.dev](https://accesskit.dev/)). No first-party i18n; community discussion around `fluent` notes a real tension — runtime lookup cost matters because egui re-evaluates UI every frame ([discussion #1751](https://github.com/emilk/egui/discussions/1751)).

**Real image/list-heavy apps**: **Rerun**, a robotics/multimodal data visualizer handling "millions of points and many gigabytes of data," is built on egui+wgpu and moved hot paths to GPU rendering for scale ([ARCHITECTURE.md](https://github.com/rerun-io/rerun/blob/main/ARCHITECTURE.md), [rerun.io](https://rerun.io/)). Smaller precedent: `img_viewer` and `avis-imgv` are egui-based image viewers, the latter built "to help sort through large collections of images" (scale unstated).

---

## iced

**Rendering/GPU at 100k+**: `iced_wgpu` is the default native renderer (Vulkan/Metal/DX11/DX12), with images/SVG as GPU-backed primitives ([iced_wgpu docs](https://docs.rs/iced_wgpu)). No dedicated virtualized-grid widget; `widget::Lazy` skips rebuilding an unchanged subtree but isn't viewport windowing ([Lazy docs](https://docs.iced.rs/iced/widget/struct.Lazy.html)) — as with egui, a 100k-item grid needs app-built virtualization. Iced 0.14 (Dec 2025) added reactive rendering (redraw only changed regions) and "concurrent image decoding and uploading," both directly useful for a thumbnail grid ([Release 0.14.0](https://github.com/iced-rs/iced/releases/tag/0.14.0)).

**Binary size / memory**: ≈17MB hello-world in the same dated benchmark.

**Maturity/stability on Windows**: 0.14.0 is explicitly billed as "the final experimental release before 1.0" — the project itself signals it isn't yet at its intended stability point ([Release 0.14.0](https://github.com/iced-rs/iced/releases/tag/0.14.0)). No Windows-specific red flags found, but no Windows-specific maturity statement either.

**Accessibility & i18n**: The AccessKit-based accessibility tracking issue has been open since **October 2020** with no indication it has landed ([iced-rs/iced#552](https://github.com/iced-rs/iced/issues/552)) — the largest a11y gap of the four. No first-party i18n; COSMIC's `libcosmic` layer (built on iced) adds fluent-based translations on top, evidence it's solvable but not part of iced itself.

**Real image/list-heavy apps**: **System76's COSMIC desktop** is built on `libcosmic` (a design layer over iced); its file manager **cosmic-files** provides tab/dual-pane browsing with search ([system76.com/cosmic](https://system76.com/cosmic), [cosmic-files](https://rustutils.com/tools/cosmic-files/)) — real evidence of iced scaling to a full desktop shell with list UI in production, though Linux-first and not Windows-proven; grid-layout widgets for it were still being built out per the sourced coverage.

---

## Slint

**Rendering/GPU at 100k+**: Two GPU renderer choices — **Skia** (OpenGL/Metal/Vulkan/**Direct3D on Windows**, sophisticated but heavier disk footprint) and **FemtoVG** (lighter, OpenGL ES2 by default, or `renderer-femtovg-wgpu` for Metal/Vulkan/Direct3D); a software renderer exists for no-GPU targets. Selectable at runtime via `SLINT_BACKEND` (e.g. `winit-skia`) ([backends-and-renderers](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backends_and_renderers/)). `ListView` virtualizes natively — "elements are only instantiated if they are visible, which guarantees stable performance with a practically unlimited number of items" ([ListView docs](https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/listview/)). No built-in GridView/image-grid widget — a live discussion asks for exactly this, confirming a 2D thumbnail grid must be hand-built (e.g. `Repeater` over a windowed model) on top of the virtualized primitives, same as egui/iced ([slint-ui/slint#5147](https://github.com/slint-ui/slint/discussions/5147)).

**Binary size / memory**: No primary-source numbers found. Skia is explicitly documented as having "heavy disk-footprint compared to other renderers" — renderer choice is a real size trade within Slint itself.

**Maturity/stability on Windows**: Native Direct3D support in both GPU renderers makes Windows a first-class target, not an afterthought. Licensing needs a conscious decision: GPLv3, a **Royalty-free license** (free for desktop/mobile/web with attribution, excludes embedded), or paid Commercial — a deliberate post-1.0 addition, not the original single-license state ([Slint 1.1 blog](https://slint.dev/blog/slint-1.1-released), [LICENSE.md](https://github.com/slint-ui/slint/blob/master/LICENSE.md)). A governance signal worth weighing even though it's not a technical maturity gap.

**Accessibility & i18n**: i18n is first-party and comparatively mature: `@tr()` marks translatable strings, `slint-tr-extractor` produces gettext `.pot` files, resolved through standard Gettext — established tooling, not bolted on ([translation-infrastructure](https://slint.dev/blog/translation-infrastructure)). Accessibility exists (AccessKit-backed on the winit backend) but has a documented gap: text-input widgets aren't fully exposed to screen readers yet ([slint-ui/slint#2895](https://github.com/slint-ui/slint/issues/2895)).

**Real image/list-heavy apps**: The official "Made with Slint" showcase lists production apps (audio plugins, a WSL dashboard, **Cargo UI**) ([madewithslint.com](https://madewithslint.com/)), but none found here are image-gallery or 100k-item-grid apps — the showcase skews embedded/automotive and small utilities, not large-scale desktop data browsers.

---

## Open questions

1. No primary source measured any of the four at actual 100k+ item/texture scale — all virtualization claims are extrapolated from "large list" docs (typically thousands, not hundreds of thousands, of items) or from Rerun's point-cloud scale (a different problem than a photo grid). A throwaway per-shell benchmark is the only way to close this before [ticket 007](../tickets/007-choose-gui-shell.md).
2. Whether Tauri's raw `WebView2Settings`/`CoreWebView2EnvironmentOptions` (`IsReputationCheckingRequired`, `IsCustomCrashReportingEnabled`) are reachable through Tauri's own Rust API, or require dropping to `wry`/raw `webview2-com` bindings, wasn't confirmed — only that Tauri's capability/CSP layers don't cover them.
3. No official Windows-specific Tauri/Electron memory benchmark found; the only concrete USS/PSS numbers (#5889) were measured on Ubuntu.
4. Slint binary-size numbers (any renderer) weren't found in primary sources — worth a from-scratch build-and-measure before committing.
5. Whether egui's image-loader RAM issue (#5439) has a maintainer-endorsed fix path, vs. "manage textures yourself," is unresolved — no maintainer response was found in what was retrieved.
6. iced's and Slint's screen-reader support was evaluated from tracking-issue text, not hands-on NVDA/Narrator testing on Windows — actual usability for a blind user wasn't verified for any of the four.
