#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Layer 3 of workplan/SPEC.md §8's offline-enforcement package (Milestone 6):
    runtime WFP connection-audit, empirical proof (not just static analysis)
    that LumenVault never opens an outbound connection.

.DESCRIPTION
    Enables the "Filtering Platform Connection" Windows Security audit
    subcategory, which makes Windows log event 5156 for every connection WFP
    *permits* and 5157 for every one it *blocks* — regardless of whether the
    attempt came from LumenVault's own Rust code, a shelled-out process, or
    WebView2's own background traffic (see
    workplan/research/network-enforcement.md §4). Static analysis (cargo-deny,
    §8.2) cannot see any of those; this is the only layer that can.

    This script cannot be run by the build session that wrote it — querying
    or setting audit policy, and reading the Security event log, both require
    an elevated (Administrator) PowerShell session, which the dev/CI
    environment that produced this script did not have (verified: `auditpol
    /get ...` and `Get-WinEvent -LogName Security` both failed with
    "a required privilege is not held by the client" /
    "unauthorized operation" as an ordinary member of BUILTIN\Administrators
    without UAC elevation). Run this from an elevated session instead.

.PARAMETER ProcessName
    The executable name to filter Security log events on. Defaults to
    lumenvault-app.exe (src-tauri's [[bin]] name).

.PARAMETER DurationSeconds
    How long to wait, after auditing is enabled, before checking the log.
    Drive the app manually through every flow during this window: import,
    browse/filter/search, tag, dedupe-review merge, export, close. Default
    300s (5 minutes) — adjust for how long a full manual pass actually takes.

.PARAMETER LeavePolicyChanged
    By default this script restores the "Filtering Platform Connection"
    audit subcategory to whatever it was set to before the script ran (audit
    policy is a system-wide setting; changing it and not reverting it would be
    a side effect a release-gate script shouldn't leave behind). Pass this
    switch to leave the auditing enabled after the script exits instead.

.EXAMPLE
    # From an elevated PowerShell prompt, at the repo root:
    .\docs\verify-wfp-audit.ps1

    # Drive the app through import -> browse -> tag -> merge -> export within
    # the DurationSeconds window, then read the verdict printed at the end.

.NOTES
    "Pass" = zero 5156 (permitted) and zero 5157 (blocked) events attributed
    to $ProcessName in the Security log across the driven window. Either
    event type existing is a failure — a *blocked* attempt (5157) still means
    the app tried to open a socket, which "100% offline" (workplan/SPEC.md §1)
    does not allow, firewall or no firewall.
#>
[CmdletBinding()]
param(
    [string]$ProcessName = 'lumenvault-app.exe',
    [int]$DurationSeconds = 300,
    [switch]$LeavePolicyChanged
)

$ErrorActionPreference = 'Stop'
$subcategory = 'Filtering Platform Connection'

function Get-CurrentAuditSetting {
    # `/r` gives CSV output with a parseable "Inclusion Setting" column,
    # unlike the human-formatted default `auditpol /get` output.
    $csv = auditpol /get /subcategory:"$subcategory" /r | ConvertFrom-Csv
    if (-not $csv) {
        throw "could not read the current '$subcategory' audit setting (auditpol /get /r returned nothing)"
    }
    return $csv[0].'Inclusion Setting'
}

function Set-AuditSetting {
    param([string]$SuccessFlag, [string]$FailureFlag)
    auditpol /set /subcategory:"$subcategory" /success:$SuccessFlag /failure:$FailureFlag | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "auditpol /set failed with exit code $LASTEXITCODE"
    }
}

function Restore-AuditSetting {
    param([string]$OriginalSetting)
    # Map auditpol's /r text back onto the /set flags that would reproduce it.
    switch -Wildcard ($OriginalSetting) {
        '*Success and Failure*' { Set-AuditSetting -SuccessFlag enable  -FailureFlag enable }
        '*Success*'             { Set-AuditSetting -SuccessFlag enable  -FailureFlag disable }
        '*Failure*'             { Set-AuditSetting -SuccessFlag disable -FailureFlag enable }
        default                 { Set-AuditSetting -SuccessFlag disable -FailureFlag disable }
    }
}

Write-Host "=== LumenVault WFP connection audit (workplan/SPEC.md §8.3) ===" -ForegroundColor Cyan

$originalSetting = Get-CurrentAuditSetting
Write-Host "Current '$subcategory' audit setting: $originalSetting"

Write-Host "Enabling success+failure auditing for '$subcategory'..."
Set-AuditSetting -SuccessFlag enable -FailureFlag enable

try {
    $startTime = Get-Date
    Write-Host ""
    Write-Host "Auditing is now ON. You have $DurationSeconds seconds to drive the" -ForegroundColor Yellow
    Write-Host "running app (launch $ProcessName separately) through every flow:" -ForegroundColor Yellow
    Write-Host "  import -> browse/filter/search -> tag -> dedupe-review merge -> export -> close" -ForegroundColor Yellow
    Write-Host ""

    $elapsed = 0
    while ($elapsed -lt $DurationSeconds) {
        $remaining = $DurationSeconds - $elapsed
        Write-Progress -Activity "Driving LumenVault" -Status "$remaining s remaining" `
            -PercentComplete (100 * $elapsed / $DurationSeconds)
        Start-Sleep -Seconds 5
        $elapsed += 5
    }
    Write-Progress -Activity "Driving LumenVault" -Completed

    Write-Host "Window closed. Querying the Security log for 5156/5157 events since $startTime..."

    # Filter by event ID + time range first (fast, indexed); the process
    # name isn't a queryable XPath field on these events in a stable way
    # across OS builds, so match it against the rendered message text
    # instead, which always includes the full "Application:" path.
    $events = Get-WinEvent -FilterHashtable @{
        LogName   = 'Security'
        Id        = 5156, 5157
        StartTime = $startTime
    } -ErrorAction SilentlyContinue |
        Where-Object { $_.Message -match [regex]::Escape($ProcessName) }

    $permitted = @($events | Where-Object Id -eq 5156)
    $blocked   = @($events | Where-Object Id -eq 5157)

    Write-Host ""
    Write-Host "=== Result ===" -ForegroundColor Cyan
    Write-Host "5156 (permitted connections) attributed to ${ProcessName}: $($permitted.Count)"
    Write-Host "5157 (blocked connections) attributed to ${ProcessName}:   $($blocked.Count)"

    if ($permitted.Count -eq 0 -and $blocked.Count -eq 0) {
        Write-Host ""
        Write-Host "PASS: zero connection attempts observed for $ProcessName." -ForegroundColor Green
        exit 0
    }
    else {
        Write-Host ""
        Write-Host "FAIL: $ProcessName attempted at least one network connection." -ForegroundColor Red
        Write-Host "Offending events:"
        $events | Format-Table TimeCreated, Id, Message -Wrap -AutoSize
        exit 1
    }
}
finally {
    if (-not $LeavePolicyChanged) {
        Write-Host ""
        Write-Host "Restoring '$subcategory' audit setting to its original value ($originalSetting)..."
        Restore-AuditSetting -OriginalSetting $originalSetting
    }
    else {
        Write-Host ""
        Write-Host "Leaving '$subcategory' auditing enabled (-LeavePolicyChanged was passed)."
    }
}
