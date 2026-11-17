; Register Flick's app id (the bundle identifier) so its Windows notifications
; show as Flick even on a /NS install, which skips the Start-menu shortcut that
; also carries it.
!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Classes\AppUserModelId\${BUNDLEID}" "DisplayName" "${PRODUCTNAME}"
!macroend

; Remove "start at login" and the app id when Flick is uninstalled.
; tauri-plugin-autostart writes a value named after the product ("Flick")
; under HKCU ...\Run. Skipped on an update, which must keep both (same guard as
; Tauri's own).
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Flick"
    DeleteRegKey HKCU "Software\Classes\AppUserModelId\${BUNDLEID}"
  ${EndIf}
!macroend
