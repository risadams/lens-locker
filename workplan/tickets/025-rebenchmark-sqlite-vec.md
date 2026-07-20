---
id: 025
title: "Re-benchmark sqlite-vec at 100k+ scale on real hardware"
type: workplan:task
status: closed
assignee: benchmark-subagent (fired 2026-07-20)
blocked-by: []
---

## Question

[Ticket 016's research](016-research-offline-autotagging.md) cited
`sqlite-vec`'s own published brute-force benchmark (≤75ms/query at
100k×768-dim vectors) as adequate for LensLocker's 100k+ scale target, but
flagged explicitly that this number is on the maintainer's hardware/data, not
LensLocker's, and should be reproduced before the schema is locked.

This ticket is the reproduction: build a real `sqlite-vec` table with ~100k
synthetic (or real, if a large-enough test library exists) 768-dim vectors on
representative Windows hardware, run realistic KNN queries, and record actual
latency. No decision is made here beyond "is this fast enough for an
interactive similarity-search UI" (target: comfortably under ~200ms/query,
matching the interactive-latency budget ticket 016 used) — if it isn't, that
blocks the runtime decision in [ticket 024](024-decide-tagging-model-runtime.md)
and would need `simsimd` or an HNSW fallback (`hnsw_rs`, per ticket 016's
survey) reconsidered.

## Resolution

Built and ran a real standalone Rust benchmark (`rusqlite` 0.40.1 +
`sqlite-vec` 0.1.9, not added to the lens-locker workspace) at
`sqlite-vec-bench/` (session scratchpad, left in place, not committed — same
pattern as [ticket 019](019-validate-thumbnail-grid-performance.md)'s
`tauri-grid-bench` prototype). 100,000 synthetic 768-dim vectors in a real
`vec0` virtual table, 1,000 brute-force KNN queries (k=20) against a real
on-disk WAL-mode `.sqlite` file on this machine (i9-11900KF, 128GB RAM,
Windows 11). Full methodology, numbers, and caveats:
[../research/sqlite-vec-100k-benchmark.md](../research/sqlite-vec-100k-benchmark.md).

**Verdict: fails the ~200ms interactive-latency bar.** Measured **352.8ms
median / 376.7ms p95** (max 875.8ms) against the realistic on-disk
configuration — roughly 1.8-4.7x over target, not a marginal miss. A
diagnostic in-memory-only run of the identical workload came back at 94.5ms
median / 101.6ms p95, close to sqlite-vec's own published ≤75ms figure — which
localizes the shortfall to something about querying a real on-disk file
(leading hypothesis: mmap not actually engaging at the compiled-in cap; not
fully root-caused here) rather than the brute-force distance math itself. But
LensLocker's catalog is a persistent file, not an in-memory DB, so the on-disk
number is the one that governs this ticket's question, and it does not clear
the bar.

**This is the "surprisingly bad" outcome flagged as a live possibility in this
ticket's own text, and it does feed back into [ticket
024](024-decide-tagging-model-runtime.md)'s runtime decision**: that ticket
should treat `sqlite-vec` brute-force-against-a-real-file as inadequate as
measured, and account for needing `simsimd` and/or an `hnsw_rs` ANN fallback
(both already surveyed in [ticket 016's
research](../research/offline-autotagging.md), §3) as a real requirement
rather than a documented-but-unlikely contingency.
