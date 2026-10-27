; OmniZip installer hooks for the Tauri NSIS bundle (docs/07-shell-integration.md §3.3).
; Installation is per user, so everything here writes to HKCU only.

!include "StrFunc.nsh"
${StrStr}
${UnStrStr}
${StrRep}
${UnStrRep}

!macro NSIS_HOOK_PREINSTALL
  ; Explorer keeps the old context-menu DLL loaded. A loaded DLL cannot be overwritten but it
  ; can be renamed, so the new copy installs cleanly and the old one goes on the next reboot.
  IfFileExists "$INSTDIR\omnizip_shell.dll" 0 +4
    Delete "$INSTDIR\omnizip_shell.dll.old"
    Rename "$INSTDIR\omnizip_shell.dll" "$INSTDIR\omnizip_shell.dll.old"
    Delete /REBOOTOK "$INSTDIR\omnizip_shell.dll.old"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Context menu (classic menu; the Windows 11 top-level menu comes with the MSIX package).
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\omnizip_shell.dll"'

  ; Put `omnizip` on the user PATH (once).
  ReadRegStr $0 HKCU "Environment" "Path"
  ${StrStr} $1 "$0" "$INSTDIR"
  StrCmp $1 "" 0 omnizip_path_done
    StrCmp $0 "" 0 +3
      WriteRegExpandStr HKCU "Environment" "Path" "$INSTDIR"
      Goto omnizip_path_notify
    WriteRegExpandStr HKCU "Environment" "Path" "$0;$INSTDIR"
  omnizip_path_notify:
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=500
  omnizip_path_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$SYSDIR\regsvr32.exe" /u /s "$INSTDIR\omnizip_shell.dll"'

  ; Remove our entry from the user PATH.
  ReadRegStr $0 HKCU "Environment" "Path"
  ${UnStrRep} $1 "$0" ";$INSTDIR" ""
  ${UnStrRep} $1 "$1" "$INSTDIR;" ""
  ${UnStrRep} $1 "$1" "$INSTDIR" ""
  StrCmp $1 $0 +3
    WriteRegExpandStr HKCU "Environment" "Path" "$1"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=500
!macroend
