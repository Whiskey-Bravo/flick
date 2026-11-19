; Register Flick's app id (the bundle identifier) so its Windows notifications
; show as Flick even on a /NS install, which skips the Start-menu shortcut that
; also carries it.
!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Classes\AppUserModelId\${BUNDLEID}" "DisplayName" "${PRODUCTNAME}"
!macroend

; Remove Flick's sign-in task before its files go: flick.exe deletes the task
; and the old Run value, and when Start at login was on leaves a marker next
; to the config so a manual upgrade ("Uninstall before installing" runs this
; uninstaller) gets it back on the new version's first start. Skipped on an
; in-app update, which keeps Start at login.
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    ExecWait '"$INSTDIR\flick.exe" --remove-login-task'
  ${EndIf}
!macroend

; Remove the old "start at login" and the app id when Flick is uninstalled.
; The old way (a plugin) wrote a value named after the product ("Flick")
; under HKCU ...\Run. Skipped on an update, which must keep it (same guard as
; Tauri's own).
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Flick"
    DeleteRegKey HKCU "Software\Classes\AppUserModelId\${BUNDLEID}"
  ${EndIf}
!macroend
