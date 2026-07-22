---
id: 048
title: "Tune Argon2id parameters for a desktop one-shot unlock"
type: workplan:task
status: closed
assignee: chris (benchmark session 2026-07-22)
blocked-by: [041]
---

## Question

[Ticket 041](041-research-crypto-stack.md) confirmed Argon2id but flagged
concrete parameter tuning (memory/iterations/parallelism) as a real
decision, not a given — this is a once-per-launch desktop unlock defending
against offline brute-force of a stolen disk, not a per-request server
auth path, so it can and should run much heavier than typical
server-interactive guidance.

Benchmark realistic parameter choices (in the spirit of
[ML ticket 025](025-rebenchmark-sqlite-vec.md)'s real-hardware
re-benchmark, not published/assumed figures) on representative Windows
hardware: find a memory/time/parallelism combination that meaningfully
resists offline brute-force while keeping unlock latency acceptable for a
one-time-per-launch action (target: sub-few-seconds, not sub-100ms).
Record actual measured numbers.

## Resolution

Measured with a throwaway benchmark (RustCrypto `argon2` v0.5.3,
`hash_password_into` with a 32-byte output, a Unicode test password
including spaces/emoji per [ticket 043](043-design-setup-onboarding-ux.md)'s
"any Unicode char" requirement, release build, 3 runs averaged) on the
user's actual machine — an 11th Gen Intel Core i9-11900KF @ 3.50GHz, 16
logical CPUs. This is genuinely representative hardware for a personal,
single-user app: it's the real target machine, not a guess.

| Parameters (m / t / p) | Measured (avg) |
|---|---|
| 19 MiB / t=2 / p=1 (OWASP minimum) | 19.6 ms |
| 64 MiB / t=3 / p=1 (server-interactive baseline) | 95.3 ms |
| 256 MiB / t=3 / p=4 | 425.1 ms |
| 512 MiB / t=2 / p=4 | 643.8 ms |
| 1024 MiB / t=2 / p=4 | 1284.0 ms |
| **1024 MiB / t=3 / p=4 — chosen default** | **1806.7 ms** |
| 1024 MiB / t=4 / p=4 | 2373.2 ms |

**Important finding**: the `argon2` crate has no `rayon`/parallel feature
— it does not multi-thread across `p_cost` lanes regardless of available
CPU cores (confirmed 16 logical CPUs sat idle during these runs). `p_cost`
still increases total computational/memory-access work (a real, if
non-wall-clock-visible, resistance factor against certain parallel
attackers), but does not buy wall-clock speed the way it would on a
parallelizing implementation. This means the numbers above are honestly
single-threaded timings, not an artifact of an unused feature — don't
assume a future crate upgrade changes them without re-measuring.

**Chosen default: 1024 MiB (1 GiB) memory, t=3 (iterations), p=4
(parallelism)** — 1.8s measured on real target hardware. This sits
comfortably in the "sub-few-seconds, not sub-100ms" target: a real cost for
offline brute-force (1 GiB per attempt makes GPU/ASIC parallel cracking
attempts expensive in a way the OWASP-minimum or server-interactive
settings don't), while staying well under a "did the app hang?" threshold
for a once-per-launch action. On weaker/older hardware than this
benchmark's, unlock will take longer — `LOCK-SPEC.md` should note this
as an accepted, expected variance (still bounded to single-digit seconds
on any remotely modern machine) rather than something requiring adaptive
tuning for v1.
