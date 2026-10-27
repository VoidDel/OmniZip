//! OmniZip shell integration for Windows (`docs/07-shell-integration.md` §3).
//!
//! * [`menu`] — verbs, selection rules, localized titles, settings (platform independent);
//! * `com` — the `IExplorerCommand` COM server exported by `omnizip_shell.dll`;
//! * `register` — per-user registration used by the installer and the settings page.
//!
//! The DLL never opens archives: it only classifies the selection by name (and, for a handful
//! of extension-less files, by their first bytes) and hands the work to `omnizip-desktop.exe`.

pub mod menu;

#[cfg(windows)]
pub mod com;
#[cfg(windows)]
pub mod register;
