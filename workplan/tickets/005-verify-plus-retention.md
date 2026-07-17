---
id: 005
title: "Adopt verify-plus-retention import safety"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: []
---

## Question

Import MOVEs files out of their original location, possibly converting format on the
way in. What recovery guarantee does the spec commit to when something goes wrong
(bad conversion, interrupted import, user regret)?

## Resolution

**Verify, then retain.** Import verifies the managed copy (hash and/or decode check)
before touching the original; the original is then moved to a quarantine/recycle
area retained for a configurable window (default on the order of 30 days) before
permanent deletion. Bounded disk cost, real undo. The mechanics (quarantine
location, crash recovery, cross-volume behavior) are specified in
[Design the import pipeline and managed store layout](010-import-pipeline-store-layout.md).
