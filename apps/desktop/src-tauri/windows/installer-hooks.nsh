; Tauri's Restart Manager handling closes only processes using the installed executable.
; WSL keepalive children are detached and native worker/controller services and data are retained.
!define GRIDSTATOR_RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

!macro NSIS_HOOK_POSTINSTALL
  ; Reattach the existing background host after an in-place upgrade, even if the GUI's
  ; Finish-page launch checkbox is off. No --startup: do not revive manually stopped workers.
  ReadRegStr $0 HKCU "${GRIDSTATOR_RUN_KEY}" "GridstatorWorkerHost"
  ${If} $0 == '$\"$INSTDIR\${MAINBINARYNAME}.exe$\" --worker-host --startup'
    nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" "--worker-host"
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; An updater replacement preserves login preferences. A real uninstall removes only
  ; the host entry belonging to this installation, not another install or native service.
  ${If} $UpdateMode <> 1
    ReadRegStr $0 HKCU "${GRIDSTATOR_RUN_KEY}" "GridstatorWorkerHost"
    ${If} $0 == '$\"$INSTDIR\${MAINBINARYNAME}.exe$\" --worker-host --startup'
      DeleteRegValue HKCU "${GRIDSTATOR_RUN_KEY}" "GridstatorWorkerHost"
    ${EndIf}
  ${EndIf}
!macroend
