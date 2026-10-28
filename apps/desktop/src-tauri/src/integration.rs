//! The Windows context menu (`docs/07-shell-integration.md` §3): the classic menu, registered
//! under HKCU by `omnizip_shell`, and the Windows 11 top-level menu, which needs package
//! identity. The full MSIX brings its own; the EXE install ships a context-menu package next to
//! the app (`omnizip_menu.msix`, a sparse package whose files are those of the install folder),
//! registered here by the installer (`--register-shell`) or from the settings page.
//!
//! Windows registers a package only when it trusts the certificate it is signed with. Built on a
//! developer machine, the package is signed with the project's self-signed test certificate,
//! which this program carries (`build.rs`): when the computer does not trust it yet, registering
//! a signed package asks for
//! administrator rights (UAC) and an elevated copy of the program (`--trust-cert`) imports it
//! into the machine's Trusted People store. The package itself is then registered by the user's
//! process, since registration is per user.

use serde::Serialize;

/// The Windows 11 top-level menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Win11Menu {
    /// No context-menu package next to the app (development build, full MSIX).
    None,
    /// The package ships with the app but is not registered for it.
    Off,
    /// Registered for this install.
    On,
}

/// Context-menu registration state (settings page, `docs/07-shell-integration.md` §7).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellStatus {
    /// The platform has a context-menu integration in this build.
    pub supported: bool,
    /// The classic menu is registered.
    pub registered: bool,
    /// The extension next to this executable.
    pub dll: Option<String>,
    /// The registry points at another copy (for example an older install).
    pub stale: bool,
    pub win11: Win11Menu,
}

#[cfg(not(windows))]
pub fn status() -> ShellStatus {
    ShellStatus {
        supported: false,
        registered: false,
        dll: None,
        stale: false,
        win11: Win11Menu::None,
    }
}

#[cfg(not(windows))]
pub fn register() -> omnizip_core::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn unregister() -> omnizip_core::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn refresh_classic() {}

#[cfg(not(windows))]
pub fn trust_cert() -> omnizip_core::Result<()> {
    Ok(())
}

#[cfg(windows)]
pub use win::{refresh_classic, register, status, trust_cert, unregister};

#[cfg(windows)]
mod win {
    use std::path::{Path, PathBuf};

    use ::windows::{
        Foundation::Uri,
        Management::Deployment::{AddPackageOptions, PackageManager},
        Win32::{
            Foundation::{CloseHandle, ERROR_CANCELLED},
            Security::Cryptography::{
                CERT_FIND_EXISTING, CERT_OPEN_STORE_FLAGS, CERT_QUERY_ENCODING_TYPE,
                CERT_STORE_ADD_REPLACE_EXISTING, CERT_STORE_OPEN_EXISTING_FLAG,
                CERT_STORE_PROV_SYSTEM_W, CERT_STORE_READONLY_FLAG,
                CERT_SYSTEM_STORE_LOCAL_MACHINE, CertAddEncodedCertificateToStore, CertCloseStore,
                CertCreateCertificateContext, CertFindCertificateInStore,
                CertFreeCertificateContext, CertOpenStore, HCERTSTORE, PKCS_7_ASN_ENCODING,
                X509_ASN_ENCODING,
            },
            System::{
                Com::{
                    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
                    CoUninitialize,
                },
                Threading::{GetExitCodeProcess, WaitForSingleObject},
            },
            UI::{
                Shell::{
                    SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
                },
                WindowsAndMessaging::SW_HIDE,
            },
        },
        core::{HSTRING, PCWSTR, w},
    };
    use omnizip_core::{Error, ErrorCode};

    use super::{ShellStatus, Win11Menu};

    /// Identity of the context-menu package (`platform/windows/msix/AppxManifest.sparse.xml`).
    const PACKAGE: &str = "OmniZip.ContextMenu";
    /// Publisher it is signed as (`pack.ps1 -Publisher`).
    const PUBLISHER: &str = match option_env!("OMNIZIP_PUBLISHER") {
        Some(p) => p,
        None => "CN=OmniZip Contributors",
    };
    const PACKAGE_FILE: &str = "omnizip_menu.msix";
    /// The test certificate the package is signed with when built on a developer machine
    /// (`build.rs`); empty in builds signed with a real certificate.
    const TEST_CERT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/menu-test-cert.cer"));
    const ENCODING: CERT_QUERY_ENCODING_TYPE =
        CERT_QUERY_ENCODING_TYPE(X509_ASN_ENCODING.0 | PKCS_7_ASN_ENCODING.0);

    /// The folder of this executable, where the installer puts the extension and the package.
    fn app_dir() -> Option<PathBuf> {
        std::env::current_exe().ok()?.parent().map(Path::to_owned)
    }

    fn dll() -> Option<PathBuf> {
        let p = app_dir()?.join("omnizip_shell.dll");
        p.is_file().then_some(p)
    }

    fn package_file() -> Option<PathBuf> {
        let p = app_dir()?.join(PACKAGE_FILE);
        p.is_file().then_some(p)
    }

    fn same_dir(a: &Path, b: &Path) -> bool {
        let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
        norm(a) == norm(b)
    }

    fn fail(what: &str, e: impl std::fmt::Display) -> Error {
        Error::internal(format!("{what}: {e}"))
    }

    /// The context-menu package registered for this user: full name and external location.
    fn registered() -> Vec<(HSTRING, PathBuf)> {
        let found = PackageManager::new().and_then(|pm| {
            pm.FindPackagesByUserSecurityIdNamePublisher(
                &HSTRING::new(),
                &HSTRING::from(PACKAGE),
                &HSTRING::from(PUBLISHER),
            )
        });
        let Ok(found) = found else {
            return Vec::new();
        };
        found
            .into_iter()
            .filter_map(|p| {
                let name = p.Id().and_then(|id| id.FullName()).ok()?;
                let at = p.EffectiveExternalPath().unwrap_or_default();
                Some((name, PathBuf::from(at.to_os_string())))
            })
            .collect()
    }

    /// The machine's Trusted People store, where Windows looks for self-signed package
    /// certificates.
    fn trusted_people(flags: CERT_OPEN_STORE_FLAGS) -> ::windows::core::Result<HCERTSTORE> {
        let name = HSTRING::from("TrustedPeople");
        unsafe {
            CertOpenStore(
                CERT_STORE_PROV_SYSTEM_W,
                CERT_QUERY_ENCODING_TYPE(0),
                None,
                CERT_OPEN_STORE_FLAGS(CERT_SYSTEM_STORE_LOCAL_MACHINE | flags.0),
                Some(name.as_ptr().cast()),
            )
        }
    }

    /// Whether this computer trusts the test certificate.
    fn test_cert_trusted() -> bool {
        let Ok(store) = trusted_people(CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG)
        else {
            return false;
        };
        unsafe {
            let cert = CertCreateCertificateContext(ENCODING, TEST_CERT);
            let mut found = false;
            if !cert.is_null() {
                let hit = CertFindCertificateInStore(
                    store,
                    ENCODING,
                    0,
                    CERT_FIND_EXISTING,
                    Some(cert.cast_const().cast()),
                    None,
                );
                found = !hit.is_null();
                if found {
                    let _ = CertFreeCertificateContext(Some(hit.cast_const()));
                }
                let _ = CertFreeCertificateContext(Some(cert.cast_const()));
            }
            let _ = CertCloseStore(Some(store), 0);
            found
        }
    }

    /// Makes this computer trust the test certificate; needs administrator rights (run by the
    /// elevated copy, `--trust-cert`).
    pub fn trust_cert() -> Result<(), Error> {
        if TEST_CERT.is_empty() {
            return Ok(());
        }
        let store =
            trusted_people(CERT_OPEN_STORE_FLAGS(0)).map_err(|e| fail("open Trusted People", e))?;
        let added = unsafe {
            CertAddEncodedCertificateToStore(
                Some(store),
                ENCODING,
                TEST_CERT,
                CERT_STORE_ADD_REPLACE_EXISTING,
                None,
            )
        };
        unsafe {
            let _ = CertCloseStore(Some(store), 0);
        }
        added.map_err(|e| fail("import the test certificate", e))
    }

    /// Whether the package carries a signature (packages built without a certificate do not:
    /// importing the test certificate would not help them).
    fn signed(file: &Path) -> bool {
        let ctx = omnizip_core::JobCtx::default();
        omnizip_formats::shared()
            .open(file, &Default::default(), &ctx)
            .is_ok_and(|r| r.entries().iter().any(|e| e.path == "AppxSignature.p7x"))
    }

    /// Runs this program as administrator with `arg` (after the UAC prompt) and waits for it.
    fn run_elevated(arg: &str) -> Result<(), Error> {
        let exe = std::env::current_exe().map_err(|e| fail("program path", e))?;
        let exe = HSTRING::from(exe.as_os_str());
        let arg = HSTRING::from(arg);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(exe.as_ptr()),
            lpParameters: PCWSTR(arg.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // ShellExecuteEx wants COM on the calling thread.
        let com =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let started = unsafe { ShellExecuteExW(&mut info) };
        if com.is_ok() {
            unsafe { CoUninitialize() };
        }
        if let Err(e) = started {
            // Declined at the UAC prompt.
            return Err(if e.code() == ERROR_CANCELLED.to_hresult() {
                Error::new(ErrorCode::Cancelled)
            } else {
                fail("run as administrator", e)
            });
        }
        let mut code = 1;
        unsafe {
            WaitForSingleObject(info.hProcess, u32::MAX);
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
        }
        if code == 0 {
            Ok(())
        } else {
            Err(fail(
                "import the test certificate",
                format!("exit code {code}"),
            ))
        }
    }

    fn remove_package(pm: &PackageManager, full_name: &HSTRING) -> Result<(), Error> {
        let result = pm
            .RemovePackageAsync(full_name)
            .and_then(|op| op.join())
            .map_err(|e| fail("remove the context-menu package", e))?;
        match result.ExtendedErrorCode() {
            Ok(code) if code.is_err() => Err(fail(
                "remove the context-menu package",
                result.ErrorText().unwrap_or_default(),
            )),
            _ => Ok(()),
        }
    }

    pub fn status() -> ShellStatus {
        let s = omnizip_shell::register::status();
        let dll = dll();
        let stale = match (&s.dll, &dll) {
            (Some(a), Some(b)) => !Path::new(a).eq(b.as_path()),
            _ => false,
        };
        let win11 = match (package_file(), app_dir()) {
            (Some(_), Some(dir)) if registered().iter().any(|(_, at)| same_dir(at, &dir)) => {
                Win11Menu::On
            }
            (Some(_), _) => Win11Menu::Off,
            _ => Win11Menu::None,
        };
        ShellStatus {
            supported: true,
            registered: s.registered,
            dll: dll.map(|p| p.to_string_lossy().into_owned()),
            stale,
            win11,
        }
    }

    /// Registers the classic menu with the current menu settings and, where the app ships it,
    /// the context-menu package for this install folder (replacing one registered for another
    /// copy).
    pub fn register() -> Result<(), Error> {
        let dll = dll().ok_or_else(|| {
            Error::new(ErrorCode::MissingDependency).with("path", "omnizip_shell.dll")
        })?;
        let cfg = omnizip_shell::menu::MenuConfig::load();
        omnizip_shell::register::register(&dll, &cfg).map_err(Error::from)?;

        let (Some(file), Some(dir)) = (package_file(), app_dir()) else {
            return Ok(());
        };
        if !TEST_CERT.is_empty() && signed(&file) && !test_cert_trusted() {
            run_elevated("--trust-cert")?;
        }
        let pm = PackageManager::new().map_err(|e| fail("package manager", e))?;
        for (name, _) in registered() {
            remove_package(&pm, &name)?;
        }
        let uri = |p: &Path| Uri::CreateUri(&HSTRING::from(p.as_os_str()));
        let options = AddPackageOptions::new().map_err(|e| fail("package options", e))?;
        options
            .SetExternalLocationUri(&uri(&dir).map_err(|e| fail("install folder", e))?)
            .map_err(|e| fail("install folder", e))?;
        options
            .SetForceUpdateFromAnyVersion(true)
            .map_err(|e| fail("package options", e))?;
        // The menu's COM surrogate (dllhost) of an earlier registration may still be running.
        options
            .SetForceAppShutdown(true)
            .map_err(|e| fail("package options", e))?;
        let package = uri(&file).map_err(|e| fail("context-menu package", e))?;
        let result = pm
            .AddPackageByUriAsync(&package, &options)
            .and_then(|op| op.join())
            .map_err(|e| fail("register the context-menu package", e))?;
        match result.ExtendedErrorCode() {
            Ok(code) if code.is_err() => Err(fail(
                "register the context-menu package",
                result.ErrorText().unwrap_or_default(),
            )),
            _ => Ok(()),
        }
    }

    /// Re-registers a registered classic menu with the current menu settings (flat or submenu
    /// lives in the registry).
    pub fn refresh_classic() {
        if omnizip_shell::register::status().registered
            && let Some(dll) = dll()
        {
            let cfg = omnizip_shell::menu::MenuConfig::load();
            let _ = omnizip_shell::register::register(&dll, &cfg);
        }
    }

    /// Removes the classic menu and the context-menu package.
    pub fn unregister() -> Result<(), Error> {
        omnizip_shell::register::unregister().map_err(Error::from)?;
        let pm = PackageManager::new().map_err(|e| fail("package manager", e))?;
        for (name, _) in registered() {
            remove_package(&pm, &name)?;
        }
        Ok(())
    }
}
