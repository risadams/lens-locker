; LensLocker NSIS installer hooks — layer 4 of workplan/SPEC.md §8's
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

!define LENSLOCKER_FW_RULE_NAME "LensLocker - block outbound"

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Adding Windows Firewall outbound-block rule for $INSTDIR\${MAINBINARYNAME}.exe"
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="${LENSLOCKER_FW_RULE_NAME}" dir=out action=block program="$INSTDIR\${MAINBINARYNAME}.exe" enable=yes profile=any'
  Pop $0
  ${If} $0 != 0
    DetailPrint "warning: could not add the LensLocker outbound-block firewall rule (netsh exit code $0) — install continues; the app itself has no network-capable code path regardless (workplan/SPEC.md §8.1-3)"
  ${EndIf}

  ; model.onnx_data (~3.3GB) can't ship in this installer at all — a real
  ; makensis.exe limitation (confirmed 32-bit, no File-embed of a single
  ; file this large), not a judgment call. See MODELS.md §4 /
  ; src-tauri/models/README.md for the full explanation. Tagging analysis
  ; degrades gracefully (backs off, auto-retries) until this is copied in
  ; by hand — nothing else in the app depends on it — but a silent install
  ; with no mention of a missing multi-gigabyte file would be a real trap.
  DetailPrint "NOTE: tagging analysis needs model.onnx_data (~3.3GB) copied by hand into $INSTDIR\models\siglip-so400m-onnx\ — too large for this installer to include (see MODELS.md)"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Removing Windows Firewall outbound-block rule for LensLocker"
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="${LENSLOCKER_FW_RULE_NAME}" program="$INSTDIR\${MAINBINARYNAME}.exe"'
  Pop $0
!macroend
