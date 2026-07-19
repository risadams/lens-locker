# LumenVault

A 100% offline, Windows-first desktop image manager. LumenVault takes custody
of a local photo library — importing files with a **destructive move**,
converting them losslessly where it saves real space (JPEG/PNG/BMP → JPEG XL,
TIFF recompressed in place), de-duplicating exact and near-duplicate images,
tagging, and organizing them — for exactly one user, on one machine.

> **Personal/internal use only, never distributed.** This is a standing scope
> constraint, not just a current-state fact — see [License](#license) below.

Full design rationale and the locked technical spec live in
[`workplan/SPEC.md`](workplan/SPEC.md). This README covers building and
running it.

## Non-negotiable constraints

- **Zero network access.** No telemetry, no update checks, no calling home,
  ever — enforced by four independent layers (WebView2 COM hardening,
  `cargo-deny` dependency bans, a runtime Windows Filtering Platform audit, and
  an install-time firewall rule). See `workplan/SPEC.md` §8.
- **Quality is never sacrificed** by any storage optimization — conversion is
  pixel-exact/byte-verified or it doesn't happen.
- **Destructive actions are always recoverable** — replaced or superseded
  originals sit in quarantine for a retention window (default 30 days,
  configurable) before being permanently deleted.

## Prerequisites

This is a Windows-only project (packaging, codec sourcing, and the installer
all assume it). You'll need:

- **Rust** (stable toolchain) — [rustup.rs](https://rustup.rs)
- **Node.js** — only for the `@tauri-apps/cli` dev dependency (`package.json`);
  the frontend itself is plain HTML/CSS/JS, no bundler
- **Visual Studio 2022 Build Tools**, with two components beyond the base C++
  workload:
  - `Microsoft.VisualStudio.Component.VC.Llvm.Clang`
  - `Microsoft.VisualStudio.Component.VC.Llvm.ClangToolset`

  `jpegxl-sys`'s vendored build invokes CMake with the `-T ClangCL` toolset,
  which needs these specifically — the base C++ workload alone isn't enough,
  and this must be VS **2022** (a `Visual Studio 17 2022` generator is
  hardcoded), not 2019. Install via the [VS Installer](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022) UI, or:

  ```powershell
  winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Llvm.Clang --add Microsoft.VisualStudio.Component.VC.Llvm.ClangToolset --includeRecommended"
  ```
- **WebView2 Runtime** — preinstalled on current Windows 10/11; only needed
  manually on older images.
- **NSIS** — only if you're building the installer (`cargo tauri build`), not
  for `cargo run`/`cargo tauri dev`. `winget install NSIS.NSIS`, or let
  `cargo tauri build` download it automatically.

## Building & running

```powershell
# Install the Tauri CLI (one-time, or via `npm install` + `npx tauri`)
cargo install tauri-cli --version "^2" --locked

# Run the whole workspace's tests
cargo test --workspace

# Launch the app in dev mode
cargo tauri dev
```

If `cargo-tauri` isn't installed, `cargo run -p lumenvault-app` also launches
the compiled app directly (no hot-reload, but avoids the extra install).

On first launch, LumenVault shows a first-run screen to choose where the
library lives — there is no default location (see `workplan/SPEC.md`'s
Milestone 5.5 section for why). Pick an empty folder to create a new vault, or
point it at an existing one to open it.

### Building the installer

```powershell
cargo tauri build --bundles nsis
```

Produces `target/release/bundle/nsis/LumenVault_<version>_x64-setup.exe`. The
installer adds a Windows Firewall outbound-block rule on the app's executable
as a regression catcher (`src-tauri/windows/hooks.nsh`) — real network access
is already impossible per the layers above, this just catches an accidental
dependency creeping back in later.

### Other checks CI runs

```powershell
cargo check --workspace --all-targets
cargo deny check          # dependency license/advisory/source bans
```

## Repo layout

```
crates/
  core/         shared types/utilities
  catalog/      SQLite schema, migrations, queries (rusqlite)
  hash/         BLAKE3 exact hashing + 64-bit perceptual hashing
  decode/       image probing/decoding, thumbnail + preview encoding
  convert/      JPEG XL / TIFF conversion policy (§4), verify-before-commit
  raw-worker/   isolated subprocess for RAW file handling
  xmp/          XMP sidecar read/write, tag sync
  import/       pipeline orchestration: journal, dedupe, quarantine, rebuild
src-tauri/      Tauri v2 app shell — commands binding the crates to the UI
ui/             frontend: plain HTML/CSS/JS, no framework or build step
workplan/       planning artifacts — SPEC.md is the locked technical spec;
                MAP.md/tickets/ hold the full decision trail
docs/           operational runbooks (e.g. the WFP network-audit procedure)
```

## License

**Not licensed for redistribution.** This project bundles `jpegxl-rs`
(GPL-3.0) under the constraint that it is never distributed — see
`workplan/SPEC.md`'s license/legal-review tickets (021, 022). If distribution
scope ever changes (sharing the binary, open-sourcing, anything conveyed to
another person), the GPL-3.0 dependency question and the HEIC/HEVC delegation
strategy both need to be reopened before shipping, and the offline-enforcement
compliance posture (§8) needs to be re-verified from scratch.
