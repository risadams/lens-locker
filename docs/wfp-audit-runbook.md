# Runtime WFP connection-audit runbook

Layer 3 of `workplan/SPEC.md` §8's offline-enforcement package (Milestone 6):
empirical proof, not just static analysis, that LumenVault never opens an
outbound network connection. cargo-deny (§8.2) can only see the declared
dependency graph — it cannot catch a raw `windows-sys` socket call, a
shelled-out `curl.exe`, or WebView2's own background traffic. Windows
Filtering Platform (WFP) connection auditing can, because it observes every
socket the OS actually creates, regardless of what code path created it.

## Why this is a runbook and not a finished CI job

Both steps this needs — setting/reading Windows audit policy (`auditpol`)
and reading the `Security` event log (`Get-WinEvent -LogName Security`) —
require an **elevated** (Administrator) session. The environment this
milestone was built in does not have one:

```
> auditpol /get /subcategory:"Filtering Platform Connection"
Error 0x00000522 occurred:
A required privilege is not held by the client.

> Get-WinEvent -LogName Security -MaxEvents 1
Get-WinEvent : Attempted to perform an unauthorized operation.
```

(`Get-WinEvent -FilterHashtable @{LogName='Security'; Id=5156}` fails too —
it just reports a *different*, misleading error, "No events were found that
match the specified selection criteria," instead of the real
`UnauthorizedAccessException`. Confirmed by running the identical filtered
query against event ID 4624, which fires on every interactive logon and
therefore certainly exists in the log, and getting the same "no events
found" response — i.e. it's the same permission wall, not really an empty
result.)

This is also why the check can't be silently skipped: the milestone's own
build session was never in a position to run it. That gap is intentional —
this file plus `verify-wfp-audit.ps1` are the ready-to-run artifact for
whoever finishes the milestone from an elevated session (the owner, or a
follow-up build session that has been given elevation — the same pattern
Milestone 2 used for its Clang-compiler gap).

## Running it

From an **elevated** PowerShell prompt, at the repo root:

```powershell
cd A:\LumenVault
.\docs\verify-wfp-audit.ps1
```

This will:

1. Record the current "Filtering Platform Connection" audit setting (so it
   can be restored afterwards — this is a system-wide setting, not scoped to
   this script).
2. Enable success+failure auditing for that subcategory
   (`auditpol /set /subcategory:"Filtering Platform Connection"
   /success:enable /failure:enable`).
3. Wait `-DurationSeconds` (default 300s) for you to drive the **real
   compiled app** — launch `target\release\lumenvault-app.exe` (or the
   installed copy) separately — through every flow: import a folder, browse
   the grid with filters/search, tag an image, resolve a dedupe-review
   merge, export an image, close the app.
4. Query the `Security` log for event IDs 5156 (WFP permitted a connection)
   and 5157 (WFP blocked one) since the script started, filtered to
   `lumenvault-app.exe` by matching the event's rendered message text (the
   `Application:` field WFP always logs — event IDs are the fast/indexed
   filter, the process name match happens client-side since it isn't a
   stable XPath-queryable field across OS builds).
5. Print a **PASS**/**FAIL** verdict and, on failure, the offending events.
6. Restore the audit subcategory to its original setting (pass
   `-LeavePolicyChanged` to skip this and leave auditing on).

## What "pass" means

**Zero** 5156 and **zero** 5157 events attributed to `lumenvault-app.exe`
across the driven window. A *blocked* connection attempt (5157) is still a
failure, not a partial success — "100% offline" (`workplan/SPEC.md` §1) means
the app never tries, not that a firewall caught it when it did. (The
firewall rule in `src-tauri/windows/hooks.nsh` — §8's layer 4 — is a
separate, independent regression catcher; it is not what makes this check
pass.)

## If it fails

A 5156/5157 hit means something in the running process opened a socket.
Narrow it down with the `Process ID` and `Layer Name` fields in the event's
`Message` text (also visible via `Format-Table` in the script's failure
output), then correlate against what was happening in the app at that
`TimeCreated` timestamp. `workplan/research/network-enforcement.md` §4
suggests ETW (`Microsoft-Windows-Winsock-AFD` tracing via `netsh trace`) as
the next-level diagnostic once a hit is confirmed, to get the actual call
site.
