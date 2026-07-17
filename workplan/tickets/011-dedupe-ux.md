---
id: 011
title: "Decide the duplicate-handling experience"
type: workplan:grilling
status: closed
assignee: chris
blocked-by: []
---

## Question

Detection is settled (BLAKE3 exact + perceptual hash); what happens next is not.
Are **exact** duplicates auto-collapsed silently at import (one stored copy, many
logical references) or surfaced for review? For **perceptual** near-duplicates:
review-queue UX, Hamming threshold and whether it's user-tunable, how the "winner"
is chosen (resolution, format, metadata richness?), and what "merging" does to
tags/metadata of the losers. How are deliberate variants (edits, crops, RAW+JPEG
pairs from cameras) distinguished from junk dupes?

## Resolution

**Exact duplicates (BLAKE3 match) auto-collapse silently at import**: one stored
blob, referenced by every logical image row that matched it. No review step —
byte-identical files carry no ambiguity to review. The import log records every
collapse so it stays auditable without being interruptive.

**Perceptual near-duplicates always route to a human review queue; the app never
auto-merges one.** A perceptual match is a strong hint, not proof — automation
stops exactly where ambiguity starts, mirroring the exact-dupe line. Threshold is
**user-tunable, default ≤5 Hamming bits** (the conventional "same image" cutoff),
exposed as a setting since library contents vary (e.g. burst-mode sports shooters
need a tighter threshold than scanned-document libraries).

**RAW+JPEG camera pairs are auto-detected and excluded from the review queue
entirely** — paired by filename stem + capture-timestamp proximity, treated as a
bonded pair rather than a duplicate candidate before perceptual matching runs.
This keeps the review queue focused on genuine junk dupes instead of training the
user to dismiss camera-pair noise on every import. No other variant class (edits,
crops) gets this special-casing — those lack a reliable structural heuristic like
RAW+JPEG's filename/timestamp pairing, so they flow through the standard review
queue and the human distinguishes them case by case.

**Merge semantics, on human confirmation in the review queue**: the queue presents
the pair side by side with resolution, format, file size, and date surfaced; the
higher-resolution, richer-metadata copy is pre-selected as the suggested keeper,
but the human can override. On confirm, the kept image's tags become the **union**
of both images' tags (no tagging work is silently dropped), and the discarded file
is routed through the same quarantine/retention-window mechanism as any other
removed original — decided in
[Adopt verify-plus-retention import safety](005-verify-plus-retention.md) — never
deleted outright.

**Feeds forward into**
[Design the import pipeline and managed store layout](010-import-pipeline-store-layout.md)
(where in the pipeline exact-collapse and RAW+JPEG pairing run relative to
hashing/conversion/move) and
[Draft the catalog schema](017-draft-catalog-schema.md) (blob/reference model for
collapsed exact dupes, review-queue state, RAW+JPEG pair links, tag-union merge
history).
