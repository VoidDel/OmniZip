# File-manager context menu (docs/07-shell-integration.md §2). $name is the archive or folder
# name without extension.
menu-root = OmniZip
menu-open = Open with OmniZip
menu-extract-here = Extract here
menu-extract-smart = Extract to "{ $name }"
menu-extract-to = Extract to…
menu-extract-each = Extract each to a separate folder
menu-mount = Mount
menu-test = Test archive
menu-convert = Convert to…
menu-add-zip = Add to "{ $name }.zip"
menu-add-7z = Add to "{ $name }.7z"
menu-add = Add to archive…
menu-unmount = Eject
menu-tooltip = Compress, extract and mount archives with OmniZip

# Notification-area icon (apps/desktop/src-tauri/src/tray.rs).
tray-open = Open OmniZip
tray-no-mounts = No archives are mounted
tray-mount-open = Open
tray-mount-eject = Unmount
tray-eject-all = Unmount all
tray-quit = Quit OmniZip
tray-tooltip-jobs = OmniZip: { $count ->
    [one] 1 task
   *[other] { $count } tasks
} running · { $percent }%
