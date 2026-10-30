//! Drives the COM server the way Explorer does, in-process (no registration needed).
#![cfg(windows)]

use std::{ffi::c_void, path::Path};

use omnizip_shell::{
    com::{CLSID_ROOT, DllCanUnloadNow, DllGetClassObject, clsid_for, live_objects},
    menu::{MenuConfig, Verb},
    register,
};
use windows::{
    Win32::{
        Foundation::S_OK,
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, IClassFactory},
        UI::Shell::{
            ECF_HASSUBCOMMANDS, ECS_ENABLED, ECS_HIDDEN, IEnumExplorerCommand, IExplorerCommand,
            IShellItem, IShellItemArray, SHCreateItemFromParsingName,
            SHCreateShellItemArrayFromShellItem,
        },
    },
    core::{GUID, HSTRING, IUnknown, Interface, PWSTR},
};

fn factory(clsid: GUID) -> IClassFactory {
    let mut raw: *mut c_void = std::ptr::null_mut();
    unsafe { DllGetClassObject(&clsid, &IClassFactory::IID, &mut raw) }
        .ok()
        .expect("class object");
    unsafe { IClassFactory::from_raw(raw) }
}

fn command(clsid: GUID) -> IExplorerCommand {
    unsafe { factory(clsid).CreateInstance::<Option<&IUnknown>, IExplorerCommand>(None) }
        .expect("instance")
}

fn items(path: &Path) -> IShellItemArray {
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None) }
            .expect("item");
    unsafe { SHCreateShellItemArrayFromShellItem(&item) }.expect("array")
}

fn text(p: PWSTR) -> String {
    let s = unsafe { p.to_string() }.unwrap();
    unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
    s
}

fn state(cmd: &IExplorerCommand, arr: &IShellItemArray) -> i32 {
    (unsafe { cmd.GetState(arr, true) }.unwrap()) as i32
}

#[test]
fn explorer_command_behaves_like_explorer_expects() {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .ok();
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("photos-2026.zip");
    std::fs::write(&zip, b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").unwrap();
    let txt = dir.path().join("notes.txt");
    std::fs::write(&txt, b"hello").unwrap();
    // Isolated settings: default menu, English.
    unsafe { std::env::set_var("OMNIZIP_HOME", dir.path()) };
    std::fs::write(
        dir.path().join("config.toml"),
        "[language]\nui = \"en-US\"\n",
    )
    .unwrap();

    {
        let root = command(CLSID_ROOT);
        assert_eq!(text(unsafe { root.GetTitle(None) }.unwrap()), "OmniZip");
        assert_eq!(
            unsafe { root.GetFlags() }.unwrap() as i32,
            ECF_HASSUBCOMMANDS.0
        );
        assert_eq!(unsafe { root.GetCanonicalName() }.unwrap(), CLSID_ROOT);

        // Submenu: every verb except the default-off "Convert to…" and "Eject" (an item of its
        // own on drives).
        let en: IEnumExplorerCommand = unsafe { root.EnumSubCommands() }.unwrap();
        let mut subs = Vec::new();
        loop {
            let mut slot: [Option<IExplorerCommand>; 1] = [None];
            let mut fetched = 0u32;
            let hr = unsafe { en.Next(&mut slot, Some(&mut fetched)) };
            if fetched == 0 {
                break;
            }
            subs.push(slot[0].take().unwrap());
            if hr != S_OK {
                break;
            }
        }
        assert_eq!(subs.len(), Verb::ALL.len() - 2);
        let names: Vec<GUID> = subs
            .iter()
            .map(|c| unsafe { c.GetCanonicalName() }.unwrap())
            .collect();
        assert!(names.contains(&clsid_for(Some(Verb::ExtractHere))));
        assert!(!names.contains(&clsid_for(Some(Verb::Convert))));
        assert!(!names.contains(&clsid_for(Some(Verb::Unmount))));

        // An archive shows extraction verbs and hides "Add to".
        let a = items(&zip);
        assert_eq!(state(&root, &a), ECS_ENABLED.0);
        let here = command(clsid_for(Some(Verb::ExtractHere)));
        let smart = command(clsid_for(Some(Verb::ExtractSmart)));
        let add = command(clsid_for(Some(Verb::AddZip)));
        assert_eq!(state(&here, &a), ECS_ENABLED.0);
        assert_eq!(state(&add, &a), ECS_HIDDEN.0);
        assert_eq!(
            text(unsafe { smart.GetTitle(&a) }.unwrap()),
            "Extract to \"photos-2026\\\""
        );

        // A plain file is the other way round.
        let t = items(&txt);
        assert_eq!(state(&here, &t), ECS_HIDDEN.0);
        assert_eq!(state(&add, &t), ECS_ENABLED.0);

        // "Eject" only on a drive OmniZip mounted: hidden on files and on the system drive.
        let eject = command(clsid_for(Some(Verb::Unmount)));
        assert_eq!(state(&eject, &a), ECS_HIDDEN.0);
        assert_eq!(state(&eject, &t), ECS_HIDDEN.0);
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        assert_eq!(
            state(&eject, &items(Path::new(&format!("{system}\\")))),
            ECS_HIDDEN.0
        );
        assert_eq!(text(unsafe { eject.GetTitle(&t) }.unwrap()), "Eject");
        assert_eq!(
            text(unsafe { add.GetTitle(&t) }.unwrap()),
            "Add to \"notes.zip\""
        );

        // Invoke launches the desktop app; point it at a harmless executable for the test.
        unsafe { std::env::set_var("OMNIZIP_DESKTOP_EXE", r"C:\Windows\System32\where.exe") };
        // The selection is listed in a file in the temporary folder, which the app would read and
        // remove: here it goes to the test's own folder.
        unsafe { std::env::set_var("TMP", dir.path()) };
        unsafe { here.Invoke(&a, None) }.expect("invoke");

        // Unknown class ids are refused.
        let mut raw: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { DllGetClassObject(&GUID::from_u128(42), &IClassFactory::IID, &mut raw) };
        assert!(hr.is_err());
        assert!(live_objects() > 0);
    }
    // Every object released: the DLL may be unloaded.
    assert_eq!(live_objects(), 0);
    assert_eq!(DllCanUnloadNow(), S_OK);
}

#[test]
fn registration_round_trip_in_a_scratch_key() {
    const BASE: &str = r"Software\OmniZipTest\Classes";
    let dll = Path::new(r"C:\Program Files\OmniZip\omnizip_shell.dll");
    register::register_at(BASE, dll, &MenuConfig::default()).unwrap();
    let s = register::status_at(BASE);
    assert!(s.registered);
    assert_eq!(
        s.dll.as_deref(),
        Some(r"C:\Program Files\OmniZip\omnizip_shell.dll")
    );
    let handler = windows_registry::CURRENT_USER
        .open(format!(r"{BASE}\AllFilesystemObjects\shell\OmniZip"))
        .and_then(|k| k.get_string("ExplorerCommandHandler"))
        .unwrap();
    assert_eq!(handler, register::guid_string(CLSID_ROOT));
    assert_eq!(handler, "{6F1E7A52-3C4B-4D1E-9A7F-0B2C3D4E5F00}");
    // "Eject" on drives: an item of its own, with the class id after the other verbs'.
    let eject = || {
        windows_registry::CURRENT_USER
            .open(format!(r"{BASE}\Drive\shell\OmniZip.unmount"))
            .and_then(|k| k.get_string("ExplorerCommandHandler"))
    };
    assert_eq!(eject().unwrap(), "{6F1E7A52-3C4B-4D1E-9A7F-0B2C3D4E5F0C}");

    // Flat layout: one top-level verb per enabled item, no submenu.
    let flat: MenuConfig = toml_flat();
    register::register_at(BASE, dll, &flat).unwrap();
    assert!(
        windows_registry::CURRENT_USER
            .open(format!(
                r"{BASE}\AllFilesystemObjects\shell\OmniZip.extract_here"
            ))
            .is_ok()
    );
    assert!(
        windows_registry::CURRENT_USER
            .open(format!(r"{BASE}\AllFilesystemObjects\shell\OmniZip"))
            .is_err()
    );

    assert!(
        windows_registry::CURRENT_USER
            .open(format!(
                r"{BASE}\AllFilesystemObjects\shell\OmniZip.unmount"
            ))
            .is_err(),
        "no flat \"Eject\" on files and folders"
    );
    assert!(eject().is_ok());

    register::unregister_at(BASE).unwrap();
    assert!(!register::status_at(BASE).registered);
    assert!(eject().is_err());

    // With the context-menu package only "Eject" on drives is registered here; it follows
    // its setting.
    register::register_drive_at(BASE, dll, &MenuConfig::default()).unwrap();
    assert!(eject().is_ok());
    assert!(!register::status_at(BASE).registered, "no classic menu");
    let mut off = MenuConfig::default();
    off.shell.items.insert("unmount".into(), false);
    register::register_drive_at(BASE, dll, &off).unwrap();
    assert!(eject().is_err());
    let _ = windows_registry::CURRENT_USER.remove_tree(r"Software\OmniZipTest");
}

fn toml_flat() -> MenuConfig {
    let mut c = MenuConfig::default();
    c.shell.style = "flat".into();
    c
}
