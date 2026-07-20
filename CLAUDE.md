# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

LensLocker: a 100% offline, Windows-first desktop image manager (Rust + Tauri
v2). It takes destructive-move custody of a local photo library, losslessly
converts where it saves real space (JPEG/PNG/BMP → JPEG XL, TIFF recompressed
in place), de-duplicates exact and near-duplicate images, tags, and organizes
— for exactly one user, on one machine. **Personal/internal use only, never
distributed** — this is a standing scope constraint that several architecture
decisions (GPL-3.0 encoder dependency, HEIC delegation) depend on; if
distribution scope ever changes, those need reopening (see README's License
section and `workplan/MAP.md`).

The locked technical spec is `workplan/SPEC.md` — read it (and
`workplan/MAP.md` for the decision trail) before making any architectural
change. A second, newer effort — ML auto-tagging/categorization/smart
albums/face recognition — is being spec'd separately in `workplan/ML-MAP.md`
(not yet locked into an ML-SPEC.md).

## Non-negotiable constraints

- **Zero network access, ever.** No telemetry, no update checks, no calling
  home. Enforced by four independent layers: WebView2 COM hardening
  (`src-tauri/src/webview2_hardening.rs`), `cargo-deny` dependency bans
  (`deny.toml`), a runtime Windows Filtering Platform audit
  (`docs/wfp-audit-runbook.md`), and an install-time firewall rule
  (`src-tauri/windows/hooks.nsh`). Never add an HTTP client, `mio`,
  `socket2`, or any networking crate — `deny.toml` bans these and CI enforces
  it via `cargo deny check`.
- **Quality is never sacrificed by storage optimization.** Every conversion
  path (`crates/convert`) is pixel-exact/byte-verified before it's allowed to
  replace an original — verification failure is a hard stop, never a
  degrade.
- **Destructive actions are always recoverable.** Replaced/superseded
  originals sit in quarantine for a retention window (default 30 days,
  user-tunable via `app_settings`) before permanent deletion.
- **The library location is never silently defaulted.** First run always
  requires an explicit choice (see `src-tauri/src/lib.rs`'s bootstrap-config
  doc comment for the history of why this is a hard rule, not a preference).

## Common commands

```powershell
# Run the whole workspace's tests
cargo test --workspace

# Launch the app in dev mode (hot-reload)
cargo tauri dev

# Launch without the Tauri CLI installed (no hot-reload)
cargo run -p lenslocker-app

# Type-check everything, including tests/benches (what CI runs)
cargo check --workspace --all-targets

# Dependency license/advisory/source bans — the automated offline-enforcement backstop
cargo deny check

# Build the NSIS installer
cargo tauri build --bundles nsis
```

Run a single test: `cargo test -p <crate> <test_name>` (e.g.
`cargo test -p lenslocker-catalog list_images_filters_by_tag`). Crate names
are `lenslocker-<dir>` (e.g. the `catalog/` directory is package
`lenslocker-catalog`); the Tauri shell's package is `lenslocker-app`.

Building requires VS 2022 Build Tools with the Clang/ClangCL components
(`jpegxl-sys`'s vendored CMake build needs `-T ClangCL`) — see the README's
Prerequisites section if a build fails on the JXL dependency.

## Architecture

Cargo workspace, one crate per pipeline concern, dependencies flow one
direction (no cycles):

```
crates/
  core/         domain types shared across crates (Image, Library, Tag, …) — no I/O
  catalog/      SQLite schema + migrations (rusqlite/rusqlite_migration) + all queries
  hash/         BLAKE3 exact hashing + 64-bit perceptual hash (dHash) + Hamming distance
  decode/       format probing/decoding (image crate, jxl-oxide, resvg, Windows WIC for HEIC)
  convert/      JPEG XL / TIFF conversion policy (§4) — verify-before-commit, never degrade
  raw-worker/   sandboxed subprocess for camera RAW decode (untrusted-input isolation)
  xmp/          XMP sidecar read/write — catalog is authoritative, sidecars mirror it
  import/       pipeline orchestration: journal, dedupe, quarantine, rebuild-from-sidecars
src-tauri/      Tauri v2 app shell — commands binding the crates to the UI, WebView2 hardening
ui/             frontend: plain HTML/CSS/JS, no framework, no build step
workplan/       SPEC.md is the locked spec; MAP.md/tickets/ hold the decision trail;
                ML-MAP.md is the in-progress ML effort (not yet locked)
docs/           operational runbooks (WFP network-audit procedure)
```

**Layering discipline**: `catalog` is pure SQL with no file I/O — sidecar
mirroring lives in `xmp`, which depends on `catalog` (not the other way
round) to read tags before writing sidecars. `import` orchestrates
`hash`/`decode`/`convert`/`xmp` but owns no format-specific logic itself.
`src-tauri` is the only crate that knows about Tauri; it binds these crates
to commands and never contains business logic of its own beyond DTO mapping.

**SQLite is the single source of truth**; XMP sidecars are a mirror, never
authoritative — a tag edit always goes catalog-first, then syncs the
sidecar. `rebuild-from-sidecars` (`crates/import/src/rebuild.rs`) is the
committed recovery path if the catalog is lost (dedupe history/embeddings
are *not* recoverable this way — only tags/metadata are).

**Import pipeline** (`crates/import`, SPEC.md §3) is crash-safe by
idempotency, not by trusting a journal `step` field as sole truth: every
physical side effect (blob write, `images`/`image_sources` insert,
quarantine copy) checks existing state before acting, so any file not yet
`done` can simply be re-run in full. The original source file is deleted
only after a verified quarantine copy exists — that's the one truly
irreversible step, and it's last.

**Content-addressed managed store**: `<library-root>/blobs/<first-2-hex>/<full-hash-hex>.<ext>`,
keyed by the *pre-conversion* hash. Quarantine
(`<library-root>/quarantine/<journal-id>/<original-filename>`) always lives
inside the managed store, which forces a cross-volume copy for
removable-media imports but keeps retention/cleanup logic simple. Retention
sweeps run at launch only, not on a timer (`import::sweep_expired_quarantine`).

**Tauri app shell** (`src-tauri/src/lib.rs`) centers on `LibraryState`
(`Ready(AppState)` vs `NeedsSetup`) behind a `Mutex`, because a live library
is not guaranteed to exist for the process's whole life (first run, or a
previously-configured library on a now-unreachable drive). Almost every
command goes through the `with_ready` helper to turn "no library yet" into a
clean error instead of a panic. `AppState` holds one shared
`rusqlite::Connection` behind its own `Mutex` — the single-connection-per-
library pattern used since Milestone 1. Long-running, CPU-heavy work
(`import_directory`, `get_full_preview`) runs via
`tauri::async_runtime::spawn_blocking`, never inline on an async command, so
it can't starve the shared worker pool that other commands (`list_images`,
etc.) depend on to keep the IPC bridge responsive. `ImportLock` makes a
second concurrent import impossible to *start* (not just discouraged in the
UI), because the mutex contention from two overlapping imports has caused a
real observed Windows "Application Hang."

**Frontend** (`ui/`) is plain HTML/CSS/JS with no bundler, calling Tauri
commands via `window.__TAURI__.core.invoke`. The image grid is virtualized
(only visible cells + a small buffer are ever in the DOM) — validated by a
real 100k-item benchmark, not estimated (`workplan/research/thumbnail-grid-benchmark.md`).
Thumbnails/full-size previews are served via Tauri's built-in `asset:`
protocol (`convertFileSrc`), not a hand-rolled URI-scheme handler — that
approach was tried and found to fail silently on Windows/WebView2. Because
the library root is runtime-chosen (any drive, decided at first-run) rather
than fixed under `$APPDATA`, `allow_library_in_asset_scope` must widen the
asset-protocol scope at runtime every time a library becomes ready
(`tauri.conf.json`'s static scope entry alone isn't enough).

## Working conventions

- **Every non-obvious decision cites its source** — a SPEC.md section, a
  ticket, or a milestone number — in a doc comment, not just in a commit
  message. Follow this pattern when adding code: if a choice isn't the
  obvious one, say why and point at the ticket/spec section that settled it.
- **Milestones are the unit of work**, referenced directly in doc comments
  (e.g. "Milestone 3 adds RAW+JPEG pairing"). `workplan/SPEC.md`'s Part 2 is
  the milestone build plan; check it before assuming a feature is unbuilt or
  already covered — several crates (`core`, `raw-worker`) are deliberate
  Milestone 0 stubs, not abandoned work.
- **Workplan tracker discipline** (`workplan/TRACKER.md`): tickets live under
  `workplan/tickets/NNN-slug.md` with YAML frontmatter (`status`, `assignee`,
  `blocked-by`). Claim a ticket by setting `assignee` *before* editing it;
  resolve by appending a `## Resolution` section, setting `status: closed`,
  and adding one line to the relevant MAP.md's *Decisions so far*.
- **The workspace lint table denies `unsafe_code`** (`Cargo.toml`). The rare,
  justified exceptions (`webview2_hardening.rs`, the `GetDiskFreeSpaceExW`
  FFI call in `src-tauri/src/lib.rs`) use a module/function-level
  `#[allow(unsafe_code)]` with a comment explaining why no safe alternative
  exists — don't add unsafe code without that same justification-and-allow
  pattern.
- CI (`.github/workflows/ci.yml`) is currently dormant (no remote to run
  against yet) but runs `cargo check --workspace --all-targets`,
  `cargo test --workspace`, and `cargo deny check` the moment one exists —
  treat these three as the real gate regardless.
