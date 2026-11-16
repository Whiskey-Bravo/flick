; Remove "start at login" when Flick is uninstalled. tauri-plugin-autostart
; writes a value named after the product ("Flick") under HKCU ...\Run. Skipped
; on an update, which must keep start-at-login (same guard as Tauri's own).
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Flick"
  ${EndIf}
!macroend
