# Benchmark: sqlite-vec brute-force KNN at 100k x 768-dim (real hardware)

Ticket: [../tickets/025-rebenchmark-sqlite-vec.md](../tickets/025-rebenchmark-sqlite-vec.md)
Status: real measurements from a running standalone benchmark binary, not estimates.

## Summary — verdict

**Fails the ~200ms interactive-latency bar under the realistic configuration.**
A standalone Rust binary (`rusqlite` 0.40.1 + `sqlite-vec` 0.1.9, real on-disk
WAL-mode SQLite file, not in-memory) built a `vec0` virtual table holding
100,000 synthetic 768-dim float vectors and ran 1,000 brute-force KNN queries
(k=20) against it, each with a fresh random query vector. Measured per-query
wall-clock latency:

| Metric | On-disk (realistic) | In-memory (diagnostic) |
|---|---|---|
| min | 334.950 ms | 90.510 ms |
| median | **352.786 ms** | 94.500 ms |
| mean | 356.112 ms | 95.872 ms |
| p95 | **376.656 ms** | 101.550 ms |
| p99 | 401.660 ms | — |
| max | 875.808 ms | 211.015 ms |

The on-disk number is the one that matters: LensLocker's catalog is a
persistent `.sqlite` file, not an in-memory database, so this is the
configuration a real query from the app would actually run against. At a
**352.8ms median / 376.7ms p95**, this is roughly **1.8-4.7x over** the
ticket's "comfortably under ~200ms" bar — not a marginal miss, a clear one,
and the *max* of 875.8ms would read as a stall in an interactive UI.

The in-memory diagnostic run (same workload, `Connection::open_in_memory()`
instead of a WAL-mode file) lands at a 94.5ms median / 101.6ms p95 — in the
same ballpark as sqlite-vec's own published ≤75ms figure (see [ticket 016's
research](offline-autotagging.md), §3) once accounting for different
hardware/build. That comparison localizes the gap: **the bulk of the shortfall
is not the brute-force distance math itself, it's something about querying a
real on-disk file** (see Caveats for the leading hypothesis, not fully
root-caused within this ticket's time-box).

**This is the "surprisingly bad" result the ticket asked to flag rather than
hide, and it does need to feed back into [ticket
024](../tickets/024-decide-tagging-model-runtime.md)'s runtime decision.**
`sqlite-vec` brute-force, at least in this default configuration against a
real on-disk catalog file, is not adequate as-is for an interactive
similarity-search UI at 100k images on this hardware. Per [ticket 016's
survey](offline-autotagging.md), the concrete next things to evaluate are
`simsimd` (hand-rolled SIMD distance kernels) as a drop-in accelerator, or
`hnsw_rs` as an approximate-search fallback — both flagged there for exactly
this contingency.

## Methodology

Built at `sqlite-vec-bench` (scratchpad, not committed — throwaway, and
deliberately **not** added to `lens-locker`'s workspace `Cargo.toml`, per this
ticket's instructions and the precedent set by
[ticket 019](../tickets/019-validate-thumbnail-grid-performance.md)'s
`tauri-grid-bench` prototype):

- Standalone `cargo new`, two binaries in one crate:
  - `src/main.rs` — the headline benchmark, on-disk.
  - `src/bin/inmem.rs` — an identical workload against `:memory:`, added
    mid-benchmark as a diagnostic once the on-disk number came back far worse
    than expected, to separate "is this the scan itself" from "is this
    file-backed I/O."
- Dependencies: `sqlite-vec = "0.1.9"`, `rusqlite = "0.40.1"` (`bundled`
  feature — vendors and compiles SQLite itself, resolved to SQLite 3.53.2 at
  build time), `rand = "0.10.2"`. `rusqlite` resolved to 0.40.1, not the
  `^0.31` the research doc mentioned — that pin is stale; `sqlite-vec` 0.1.9
  builds against it fine.
- Extension registration: `rusqlite::ffi::sqlite3_auto_extension` +
  `sqlite_vec::sqlite3_vec_init`, the pattern from the crate's own
  `bindings/rust/src/lib.rs` test and `examples/simple-rust/demo.rs`.
- Table: `CREATE VIRTUAL TABLE vec_items USING vec0(embedding float[768])`,
  100,000 rows inserted in a single transaction, each a 768-dim `Vec<f32>` of
  uniform random values in `[-1.0, 1.0)` reinterpreted as raw bytes (no
  external byte-casting crate — a small `unsafe` slice reinterpret, the same
  trick the crate's own demo performs via `zerocopy::AsBytes`).
- On-disk run pragmas (chosen to match how a real desktop app would configure
  its catalog, not tuned for a favorable number): `journal_mode=WAL`,
  `synchronous=NORMAL`, `cache_size=-512000` (~512MB page cache),
  `mmap_size=1073741824` (1GB — comfortably covers the whole DB file).
- Query: `SELECT rowid, distance FROM vec_items WHERE embedding MATCH ?1 ORDER
  BY distance LIMIT ?2` with a fresh random 768-dim query vector and `k=20`
  per call — `vec0`'s default distance metric (L2/Euclidean), matching what
  sqlite-vec's own published benchmark used.
- 10 unmeasured warm-up queries before the timed run, so the 1,000 measured
  queries reflect steady-state interactive use (OS page cache / SQLite pager
  already warm), not first-query cold-start cost.
- Each of the 1,000 (on-disk) / 1,000 (in-memory) queries timed individually
  with `std::time::Instant`, asserting `rows.len() == k` every time (all
  2,000 queries across both runs returned exactly 20 neighbors — no
  correctness failures).
- `cargo run --release` for both binaries; real execution, not estimated.

## Hardware / environment

- CPU: Intel Core i9-11900KF, 8 cores / 16 threads, 3.5GHz base.
- RAM: 128 GB.
- OS: Windows 11 Pro (build 10.0.26200).
- Toolchain: `rustc 1.97.1`, `cargo 1.97.1`, MSVC target
  (`x86_64-pc-windows-msvc`, the rustup default on this machine).
- This is a high-end dev workstation, not LensLocker's target floor hardware
  — see Caveats.

## Results detail

100,000 rows inserted in 4.15s (24,085 rows/s) for the on-disk run, 1.64s for
the in-memory run — insert throughput isn't the concern here (bulk-loading
100k embeddings is a one-time or incremental background cost, not on the
interactive path), but it's recorded for completeness. On-disk DB file size:
296.5 MB — consistent with 100,000 x 768 x 4 bytes (~293 MiB) of raw float
data plus `vec0`'s shadow-table/rowid overhead.

The on-disk run's own spread is worth noting: p99 (401.7ms) and max (875.8ms)
pull well above the median (352.8ms), i.e. this isn't a flat 350ms — there's
real tail latency on top of an already-failing median, which matters more for
an interactive UI than the median alone.

## Caveats — what this does and doesn't establish

- **Root cause of the disk-vs-memory gap (94ms → 353ms, ~3.7x) was not fully
  isolated within this ticket's time-box.** The leading hypothesis: the
  `rusqlite` `bundled` build's compiled-in `SQLITE_MAX_MMAP_SIZE` may cap
  memory-mapped I/O below what the runtime `PRAGMA mmap_size=1073741824` call
  requested (or below the whole 296.5MB file), in which case every brute-force
  scan — which by definition touches all 100k rows every query — falls back
  to SQLite's normal paged `read()`-syscall path instead of true mmap, even
  though the file's pages are fully resident in the OS page cache after
  warm-up. This wasn't confirmed by inspecting the actual runtime mmap
  extent, so it's a plausible explanation, not a proven one. Regardless of
  the exact mechanism, the on-disk number is the real, reproducible number a
  file-backed LensLocker catalog would see.
- **This is a synthetic-vector benchmark**, per the ticket's own framing:
  uniform-random 768-dim floats, not real CLIP/SigLIP embeddings. Real
  embeddings are not uniformly distributed in the unit hypercube (they
  cluster along a lower-dimensional manifold), which could change L2-distance
  computation's *branch/cache* behavior slightly, but the dominant cost here
  (scanning every row's raw bytes) is a function of row count and
  dimensionality, not embedding semantics — so this shouldn't meaningfully
  change the outcome, but it's not the same data the schema will eventually
  hold.
- **k=20 only.** Brute-force cost is dominated by the full-table distance
  scan, not the top-k heap maintenance, so latency should be close to flat
  across reasonable k values (5-100) — not verified directly here.
- **Single query at a time, single connection, no concurrent load.** A real
  session might have a background import/tagging worker writing to the same
  catalog file while a similarity-search query runs interactively — this
  benchmark doesn't test contention, only isolated query latency.
- **High-end dev hardware, not a floor-spec target machine.** No lower-end
  Windows hardware was available to test against in this pass; if
  LensLocker's floor spec is meaningfully below an 11th-gen Core i9, real
  latency there would be worse than what's reported here, not better.
- **Default MSVC-toolchain C build of the `sqlite-vec.c` extension** (via the
  crate's own `cc::Build::new().compile(...)`, no explicit `-march=`/`/arch:`
  flags) — no attempt was made to hand-tune the C extension's compiler flags
  for SIMD (e.g. AVX2/AVX-512, both of which this CPU supports per its
  `cpuid` flags) beyond whatever `cc`'s release-profile defaults apply. Doing
  so was out of scope for reproducing the *default* experience a LensLocker
  build would get from `cargo add sqlite-vec`, but it's a real lever a future
  ticket chasing this further could pull.
- Only L2 (Euclidean) distance was tested — `vec0`'s default and what the
  maintainer's own published benchmark used; cosine distance (more typical for
  CLIP/SigLIP similarity search) was not separately measured and could have
  different cost characteristics.

## What this means for the open questions it feeds

- **[Ticket 024 — decide tagging model runtime](../tickets/024-decide-tagging-model-runtime.md)**:
  this result should be treated as a real input, not background noise.
  `sqlite-vec` brute-force against a real on-disk catalog file, as configured
  here, does not clear the interactive-latency bar on this hardware. Before
  locking a runtime decision that assumes `sqlite-vec` brute-force is
  sufficient, that ticket should account for needing `simsimd` and/or
  `hnsw_rs` (both already surveyed in [ticket 016's
  research](offline-autotagging.md), §3) as a real architectural
  requirement, not merely a documented fallback.
- The mmap/file-I/O hypothesis above is a concrete, cheap thing to test next
  if someone wants to chase a fix within `sqlite-vec` itself before reaching
  for `simsimd`/`hnsw_rs` — e.g. checking `PRAGMA mmap_size` against the
  actual runtime-negotiated value (`sqlite3_db_config` /
  `SQLITE_DBCONFIG_MMAP_SIZE`, or just diffing against a build with a larger
  compiled-in `SQLITE_MAX_MMAP_SIZE`).

The benchmark binaries and their `Cargo.lock` were left in place (not
deleted) at the session's scratchpad path
(`sqlite-vec-bench/`, includes a ~297MB `bench.sqlite3` test artifact and
Rust build output), in case someone wants to re-run it, tweak pragmas, or
chase the mmap hypothesis directly, rather than rebuild from scratch.
