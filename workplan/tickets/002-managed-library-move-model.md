---
id: 002
title: "Adopt the managed-library move model"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: []
---

## Question

Does the app index files where they live (referenced/read-only model) or take
custody of them (managed library)?

## Resolution

**Managed library, and import is a MOVE, not a copy** — destructive to the original
location. The managed store must be space-efficient: exact duplicates should not be
stored twice, and **file-type conversions are allowed to save disk space, provided
they never sacrifice quality**. What "never sacrifice quality" means precisely — and
which conversions qualify — is its own decision:
[Define the conversion policy](009-conversion-policy.md). The safety rails for the
destructive move are decided in
[Adopt verify-plus-retention import safety](005-verify-plus-retention.md).
