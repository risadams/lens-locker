---
ticket: workplan/tickets/015-research-network-enforcement.md
status: findings-delivered
---

# Network enforcement — layered defense research

"100% offline" for LumenVault needs proof, not a promise. No single layer below is
sufficient alone; each catches a different failure mode. Ordered load-bearing → belt-and-braces.

## Recommended checklist

**Load-bearing (the spec should treat these as required):**

1. **Omit network-capable crates entirely.** Don't depend on `reqwest`, `hyper`, `ureq`,
   `tokio` with net features, `tauri-plugin-http`, or `tauri-plugin-updater`. Absence of
   the code path beats any permission system layered on top of it.
2. **`cargo-deny` `[bans]` + `[sources]` in CI, blocking merge.** Explicitly `deny` the
   known network crates (including as transitive deps) and restrict `[sources]` to
   crates.io only, so a dependency can't quietly switch to a git fork that adds
   networking. Runs on every PR.
3. **Runtime WFP connection auditing in CI/QA**, exercised against the actual built
   binary driving every app flow (import, dedupe, tag, browse) — the only layer that
   catches things static analysis structurally cannot see: raw `windows-sys` socket
   calls, shelling out to `curl.exe`, or WebView2's own background traffic. See
   Verification below.
4. **If the shell is Tauri/WebView2:** disable SmartScreen and crash upload via the
   supported `ICoreWebView2Settings8`/`IsCustomCrashReportingEnabled` COM APIs at
   `CoreWebView2Environment` creation — WebView2 makes these calls **on its own**,
   independent of any Tauri capability config.
5. **Windows Firewall outbound-block rule for the app's exe**, created by the installer.
   Catches regressions on the *installed* system even after CI passes, though it's
   user-removable so it isn't sufficient by itself.

**Belt-and-braces (defense in depth; don't let these substitute for #1–#5):**

- `cargo-vet` — audits *provenance/trust* of dependency source diffs, not runtime
  network behavior. Valuable against supply-chain tampering, irrelevant to a network
  call in code that was already audited.
- Tauri v2 capabilities/permissions/CSP — governs the IPC bridge and content loading
  exposed *to the JS frontend*; doesn't reach WebView2's own background behavior and is
  moot if no network-capable plugin is ever added (layer 1 already prevents that).
- AppContainer/LPAC — the strongest theoretical OS-level control (network default-deny
  inside a container), but for a non-MSIX Win32 exe means hand-rolling
  `CreateAppContainerProfile`/`CreateProcess` plumbing yourself; MSIX + "Win32 App
  Isolation" simplifies this but is still Microsoft-labeled preview. v2 stretch goal.
- ETW/Procmon manual monitoring — great for developer spot-checks and incident
  investigation; too heavyweight as an automated CI gate vs. WFP auditing (#3).
- **Choosing a pure-Rust shell (egui/iced/Slint) instead of Tauri removes the entire
  WebView2 problem class** (SmartScreen, telemetry, updater, font/CDN fetches) — the
  single biggest lever available, and belongs in the ticket 007 shell decision.

## 1. Build-time: cargo-deny / cargo-vet

**cargo-deny** lints the resolved Cargo dependency graph — it is a static, declared-graph
tool, not a runtime behavior analyzer: "cargo-deny is a cargo plugin that lets you lint
your project's dependency graph to ensure all your dependencies conform to your
expectations and requirements." ([EmbarkStudios/cargo-deny](https://github.com/EmbarkStudios/cargo-deny))

- `[bans]` → `deny` list bans crates by name/version, with `wrappers` to allow one
  direct dependent while still blocking the crate transitively, and `use-instead` /
  `reason` for diagnostics. Applies across the whole resolved tree, including
  transitive deps. ([cargo-deny bans config](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html))
- `[sources]` → `unknown-registry` / `unknown-git` set to `deny`, with `allow-registry`
  defaulting to crates.io and `allow-git` requiring an exact URL match, closes the door
  on a dependency silently switching to an unvetted git source.
  ([cargo-deny sources config](https://embarkstudios.github.io/cargo-deny/checks/sources/cfg.html))
- **Limits:** it only sees what's declared in `Cargo.toml`/`Cargo.lock`. It cannot catch
  a permitted crate's plain Rust code making a syscall directly (e.g. via `windows-sys`
  raw `WSAConnect`), a build script (`build.rs`) doing arbitrary I/O at compile time, or
  a crate that starts an external process (`curl.exe`, `powershell.exe -c iwr ...`).
  Those require the runtime verification layer (§4).

**cargo-vet** matches dependency updates against audits performed by trusted
organizations/individuals via differential review of what changed between vetted
versions: "analyzes the updated build graph to verify that the new code has been
audited by a trusted organization" ([cargo-vet: How it Works](https://mozilla.github.io/cargo-vet/how-it-works.html));
config lives in `supply-chain/config.toml`/`audits.toml`
([cargo-vet: Configuration](https://mozilla.github.io/cargo-vet/config.html)).
It's a human-trust/provenance control, not a network-behavior scanner — it answers "did
a human we trust read this diff," not "does this code call the network." Overhead for a
solo/small-team project is real: every bump needs an audit or an imported trust set.

## 2. Shell-level: Tauri v2 capabilities vs. WebView2's own behavior

**What Tauri v2 capabilities control:** a default-deny IPC boundary. "Capabilities
define which permissions are granted or denied for which windows or webviews,"
declared as JSON/TOML files under `src-tauri/capabilities/`, and only capabilities
explicitly enabled in `tauri.conf.json` ship in the build.
([Tauri v2: Capabilities](https://v2.tauri.app/security/capabilities/),
[Tauri v2: Permissions](https://v2.tauri.app/security/permissions/))
Network access via Tauri itself is opt-in: the HTTP client and the updater are separate
plugins (`tauri-plugin-http`, `tauri-plugin-updater`) that must be added as Cargo
dependencies *and* granted a capability/scope before any network call is reachable
([Tauri v2 HTTP Client plugin](https://v2.tauri.app/plugin/http-client/),
[Tauri v2 Updater plugin](https://v2.tauri.app/plugin/updater/) — the updater's
`endpoints` config literally posts `{{current_version}}/{{target}}/{{arch}}` to a
remote URL, so it must never be added to this project).
CSP (`tauri.conf.json` → `csp`) restricts what the loaded HTML may itself fetch/load,
but "the CSP protection is only enabled if set on the Tauri configuration file" — it is
off by default and must be turned on explicitly.
([Tauri v2: CSP](https://v2.tauri.app/security/csp/))

**What none of the above touches: WebView2's own background traffic.** WebView2
"collects a set of optional and required diagnostic data," and: **"Regardless of the
Windows Diagnostic data setting, WebView2 collects required data that's necessary to
maintain performance and reliability."** Two features generate outbound calls on their
own: **SmartScreen** — "enabled by default," sends URL/file reputation data to
Microsoft on every navigation, controlled via `IsReputationCheckingRequired`; and
**crash reporting** — minidumps "sent to Microsoft for diagnosis" unless
`IsCustomCrashReportingEnabled` is set `true` at environment creation.
([WebView2: Data and privacy](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/data-privacy))
An alternate disable path is the Chromium flag `--disable-features=msSmartScreenProtection`
via `AdditionalBrowserArguments`/`additionalBrowserArgs` (Tauri exposes this in
`tauri.conf.json`) — but Microsoft warns **"Apps in production shouldn't use WebView2
browser flags, because these flags might be removed or altered at any time"**
([WebView2 browser flags](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/webview-features-flags)):
use the documented COM settings properties as the load-bearing mechanism, the flag only
as a backstop.

**Runtime distribution** matters too: **Evergreen** mode (the default) has the runtime
"automatically updated on client machines" — its own periodic network contact; **Fixed
Version**, bundled with the app, "will not be automatically updated."
([Evergreen vs. fixed version of the WebView2 Runtime](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/evergreen-vs-fixed-version))
If Tauri is chosen, ship Fixed Version.

**Pure-Rust shells (egui/iced/Slint) sidestep this whole layer** — none embeds a
browser engine: egui is "a simple, fast, and highly portable immediate mode GUI library
for Rust" with no networking/auto-update code of its own ([egui](https://github.com/emilk/egui));
iced renders via `wgpu` with no bundled browser ([iced](https://iced.rs/)); Slint is a
native declarative GUI toolkit, likewise no browser engine ([Slint](https://slint.dev/)).
For these, "shell-level enforcement" collapses into the same build-time crate-ban
problem as the rest of the core — no second, semi-autonomous network-capable process to
separately lock down. Material input to ticket 007.

## 3. OS-level: Windows Firewall, WFP, AppContainer/LPAC

**Windows Firewall / WFP.** WFP is explicitly a developer platform, not the firewall
itself: "Windows Filtering Platform is a development platform and not a firewall
itself. The firewall application that is built into Windows... is implemented using
WFP." ([About Windows Filtering Platform](https://learn.microsoft.com/en-us/windows/win32/fwp/windows-filtering-platform-start-page))
Practically, this means the enforcement surface a spec can actually configure is
Windows Defender Firewall rules (which sit on top of WFP), created at install time:

```powershell
New-NetFirewallRule -DisplayName "LumenVault - block outbound" `
  -Direction Outbound -Program "C:\Program Files\LumenVault\lumenvault.exe" `
  -Action Block -Profile Any
```

([Manage Windows Firewall with the command line](https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/configure-with-command-line),
[`New-NetFirewallRule`](https://learn.microsoft.com/en-us/powershell/module/netsecurity/new-netfirewallrule?view=windowsserver2025-ps))
Limits: requires elevation to create, is scoped to the installed machine (not CI, not a
build-time guarantee), and a local admin (including the user) can delete or disable it
— it protects against *regression*, not against a determined bypass.

**AppContainer/LPAC.** For any process placed in an AppContainer, network access is
default-deny and only opened per-capability: "AppContainer prevents the application
from 'escaping' its environment... Granular access can be granted for Internet access,
Intranet access, and acting as a server."
([AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation))
Strongest OS primitive available, but cost differs sharply by packaging: MSIX-packaged
apps get it "easily" via the deployment stack calling `CreateAppContainerProfile` for
you; an **unpackaged (plain .exe) app has to call the Win32 AppContainer APIs itself**
— "If you're not packaging your app, then you have to do the same things by calling
those Win32 APIs yourself; but it can be complicated."
([AppContainer for legacy apps](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-for-legacy-applications-))
Microsoft's newer **Win32 App Isolation** (MSIX + low-integrity AppContainer for
ordinary Win32 apps) targets this gap but shipped as public preview in 2023
([Windows Developer Blog](https://blogs.windows.com/windowsdeveloper/2023/06/14/public-preview-improve-win32-app-security-via-app-isolation/))
— not production-ready for a v1 ship date.

## 4. Verification: proving it in CI and at runtime

The only way to catch what static analysis can't (raw syscalls, shelled-out processes,
WebView2's own traffic) is to observe actual socket activity while exercising the app.

- **Windows Security auditing on WFP (built-in, no extra tooling).** Enable the
  "Filtering Platform Connection" audit subcategory
  (`auditpol /set /subcategory:"Filtering Platform Connection" /success:enable /failure:enable`);
  Windows then logs **event 5156** for every connection WFP *permits* and **5157** for
  every one it *blocks*, each including source PID and full executable path.
  ([Audit Filtering Platform Connection](https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/auditing/audit-filtering-platform-connection),
  [Event 5156](https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/auditing/event-5156))
  This is the load-bearing verification mechanism: drive every app flow (import, dedupe,
  tag, browse, close) with this auditing on, then assert the log contains **zero**
  5156/5157 events for `lumenvault.exe`. It doesn't matter whether the attempt came from
  Rust code, WebView2, or a shelled-out process — only whether a socket was ever created.
- **ETW for finer-grained diagnosis.** `Microsoft-Windows-Winsock-AFD` tracing gives
  per-socket create/connect/send events "with minimal overhead"
  ([Winsock Tracing Events](https://learn.microsoft.com/en-us/windows/win32/winsock/winsock-tracing-events)),
  collectible via `netsh trace`/`logman`
  ([Event Tracing for Windows](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/event-tracing-for-windows)).
  Useful once the WFP audit flags a hit and you need the call site; too heavy as the
  default CI gate.
- **CI environment:** Windows has no direct equivalent to Linux network namespaces.
  Practical approximations: (a) apply the same firewall outbound-block rule to the test
  binary and assert the app still completes every flow (proves no functional network
  dependency) — combine with the WFP audit, since a blocked-but-attempted connection
  wouldn't otherwise be visible; (b) run the suite on a runner/VM with the network
  adapter disabled or on an isolated virtual switch, cruder but zero-setup.
- **Manual/dev-loop tool:** Sysinternals Process Monitor with a network filter is the
  fastest way for a developer to eyeball "did anything just try to phone home,"
  complementary to but not a substitute for the automated WFP-audit CI gate.

## Open questions

- Ticket 007 (shell choice) is still open. If Tauri is chosen over a pure-Rust shell,
  §2's WebView2-hardening steps (SmartScreen/crash-reporting APIs, Fixed Version
  runtime, CSP) become load-bearing requirements in the spec, not optional hardening —
  this research should be weighed directly in that decision.
- Whether LumenVault ships as a plain NSIS/MSI Win32 exe or as MSIX changes the
  AppContainer/LPAC cost-benefit materially (§3) — packaging format isn't yet decided
  in the workplan.
- Exact CI mechanics for the WFP-audit gate (§4) — GitHub Actions Windows runners vs.
  self-hosted, and whether `auditpol`/event-log read access needs elevation in that
  environment — need a spike before this becomes a merge-blocking check.
- Whether `cargo-vet`'s audit overhead is worth adopting for a solo/small-team project
  vs. relying on `cargo-deny` bans plus periodic manual dependency review.
