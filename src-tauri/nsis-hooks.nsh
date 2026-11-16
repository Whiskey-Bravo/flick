; Remove "start at login" when Flick is uninstalled. tauri-plugin-autostart
; writes a value named after the product ("Flick") under HKCU ...\Run.
!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Flick"
!macroend
