---
id: 025
title: "Re-benchmark sqlite-vec at 100k+ scale on real hardware"
type: workplan:task
status: open
assignee:
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
