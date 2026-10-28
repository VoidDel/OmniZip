; OmniZip installer hooks for the Tauri NSIS bundle (docs/07-shell-integration.md §3.3).
; Installation is per user, so everything here writes to HKCU only.

!include "StrFunc.nsh"
${StrStr}
${UnStrStr}
${StrRep}
${UnStrRep}

; The icon of a file type registered by the bundler (tauri.conf.json, ProgID OmniZip.<ext>).
!macro OMNIZIP_TYPE_ICON EXT
  WriteRegStr SHCTX "Software\Classes\OmniZip.${EXT}\DefaultIcon" "" "$INSTDIR\filetypes\${EXT}.ico"
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Explorer keeps the old context-menu DLL loaded. A loaded DLL cannot be overwritten but it
  ; can be renamed, so the new copy installs cleanly and the old one goes on the next reboot.
  IfFileExists "$INSTDIR\omnizip_shell.dll" 0 +4
    Delete "$INSTDIR\omnizip_shell.dll.old"
    Rename "$INSTDIR\omnizip_shell.dll" "$INSTDIR\omnizip_shell.dll.old"
    Delete /REBOOTOK "$INSTDIR\omnizip_shell.dll.old"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Context menu: the classic menu, and the Windows 11 top-level menu through the context-menu
  ; package (omnizip_menu.msix, registered with this folder as its location). A package signed
  ; with the test certificate first has the computer trust that certificate, which shows a UAC
  ; prompt; declined, the classic menu stays the only one.
  ExecWait '"$INSTDIR\omnizip-desktop.exe" --register-shell'

  ; File-type icons (icons/filetypes): the bundler gives every type the app icon.
  !insertmacro OMNIZIP_TYPE_ICON "zip"
  !insertmacro OMNIZIP_TYPE_ICON "zipx"
  !insertmacro OMNIZIP_TYPE_ICON "7z"
  !insertmacro OMNIZIP_TYPE_ICON "rar"
  !insertmacro OMNIZIP_TYPE_ICON "tar"
  !insertmacro OMNIZIP_TYPE_ICON "tgz"
  !insertmacro OMNIZIP_TYPE_ICON "tbz2"
  !insertmacro OMNIZIP_TYPE_ICON "txz"
  !insertmacro OMNIZIP_TYPE_ICON "tzst"
  !insertmacro OMNIZIP_TYPE_ICON "gz"
  !insertmacro OMNIZIP_TYPE_ICON "bz2"
  !insertmacro OMNIZIP_TYPE_ICON "xz"
  !insertmacro OMNIZIP_TYPE_ICON "zst"
  !insertmacro OMNIZIP_TYPE_ICON "lz4"
  !insertmacro OMNIZIP_TYPE_ICON "br"
  !insertmacro UPDATEFILEASSOC

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
  ExecWait '"$INSTDIR\omnizip-desktop.exe" --unregister-shell'

  ; Remove our entry from the user PATH.
  ReadRegStr $0 HKCU "Environment" "Path"
  ${UnStrRep} $1 "$0" ";$INSTDIR" ""
  ${UnStrRep} $1 "$1" "$INSTDIR;" ""
  ${UnStrRep} $1 "$1" "$INSTDIR" ""
  StrCmp $1 $0 +3
    WriteRegExpandStr HKCU "Environment" "Path" "$1"
    SendMessage ${HWND_BROADCAST} ${WM_SETTINGCHANGE} 0 "STR:Environment" /TIMEOUT=500
!macroend
