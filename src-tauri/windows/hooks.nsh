; LumenVault NSIS installer hooks — layer 4 of workplan/SPEC.md §8's
; offline-enforcement package (Milestone 6): an install-time Windows
; Firewall outbound-block rule on the app's own executable.
;
; This is explicitly a *regression catcher*, not the primary offline
; guarantee — layers 1-3 (WebView2 COM settings, cargo-deny bans, the WFP
; connection audit; see src-tauri/src/webview2_hardening.rs, deny.toml,
; docs/verify-wfp-audit.ps1) are. A local admin — including the installing
; user themselves — can delete or disable this rule the same as any
; firewall rule; it catches an accidental network dependency creeping back
; in on an already-installed system, after CI/dev-loop checks passed.
;
; Requires the installer to run elevated for `netsh advfirewall` to
; succeed, which tauri.conf.json's `bundle.windows.nsis.installMode` is set
; to "perMachine" for. `netsh` failing must never abort setup — a broken
; firewall rule is not a reason to refuse to install the app — so neither
; hook checks nsExec's return code beyond logging it.
;
; Hook mechanism: https://v2.tauri.app/distribute/windows-installer/
; (bundle.windows.nsis.installerHooks -> NSIS_HOOK_* macros).
; `${MAINBINARYNAME}` / `$INSTDIR` are supplied by Tauri's own NSIS
; template at the point these macros are inserted.

!define LUMENVAULT_FW_RULE_NAME "LumenVault - block outbound"

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Adding Windows Firewall outbound-block rule for $INSTDIR\${MAINBINARYNAME}.exe"
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="${LUMENVAULT_FW_RULE_NAME}" dir=out action=block program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes profile=any'
  Pop $0
  ${If} $0 != 0
    DetailPrint "warning: could not add the LumenVault outbound-block firewall rule (netsh exit code $0) — install continues; the app itself has no network-capable code path regardless (workplan/SPEC.md §8.1-3)"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Removing Windows Firewall outbound-block rule for LumenVault"
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="${LUMENVAULT_FW_RULE_NAME}" program="$INSTDIR\${MAINBINARYNAME}.exe"'
  Pop $0
!macroend
