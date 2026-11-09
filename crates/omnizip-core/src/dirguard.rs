//! Directories below an extraction's destination, entered one level at a time without
//! following links (`docs/09-security.md` §3, step 8).
//!
//! Checking a path and then using it leaves a window in which another program that can write
//! to the destination swaps a checked directory for a link, and the write lands wherever the
//! link points, with the rights of whoever extracts. So each level of the current directory is
//! opened as it is entered and kept open:
//!
//! * Unix: `openat` with `O_NOFOLLOW` from the level above, and every operation on names in the
//!   current directory relative to its descriptor (`openat`, `mkdirat`, `fstatat`, `renameat`,
//!   `unlinkat`, `symlinkat`, `linkat`). Names are never looked up through the path again.
//! * Windows: each level is opened without following reparse points, refused when it is one,
//!   and held without `FILE_SHARE_DELETE`: neither it nor anything above it can then be renamed,
//!   moved or deleted until it is let go. A junction can still be set on a held directory while
//!   it is empty, so whatever is created is checked by its final path
//!   (`GetFinalPathNameByHandleW`) to be below the destination, and removed when it is not.
//!
//! The destination itself may be a link (the user chose it); only what is below it is guarded.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Why a directory could not be entered.
#[derive(Debug)]
pub enum EnterError {
    /// A link (or junction) is where a directory should be.
    Link(PathBuf),
    /// Something other than a directory is there.
    NotDir(PathBuf),
    Io(io::Error, PathBuf),
}

/// What is at a name, without following a link there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    Other,
}

/// A name's metadata, without following a link there.
#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub kind: Kind,
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub readonly: bool,
}

/// The current directory below a destination, held open from the destination down.
pub struct DirGuard {
    root: PathBuf,
    /// Names of the levels below the root, and the open levels, the root first.
    names: Vec<String>,
    dirs: Vec<File>,
    /// Where the root is (volume serial number, path within the volume), for checking where
    /// things were created.
    #[cfg(windows)]
    root_final: (u32, String),
}

impl DirGuard {
    /// Opens the destination `root` (which must exist).
    pub fn open(root: &Path) -> io::Result<Self> {
        let dir = imp::open_root(root)?;
        Ok(Self {
            #[cfg(windows)]
            root_final: imp::locate(&dir)?,
            root: root.to_owned(),
            names: Vec::new(),
            dirs: vec![dir],
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of the current directory.
    pub fn path(&self) -> PathBuf {
        let mut p = self.root.clone();
        p.extend(&self.names);
        p
    }

    /// The open current directory.
    pub fn dir(&self) -> &File {
        self.dirs.last().expect("the root is always open")
    }

    /// Makes `names` (below the root) the current directory, entering each level without
    /// following links. Missing levels are created when `create` (each one passed to
    /// `created`), otherwise they fail with `NotFound`.
    pub fn enter(
        &mut self,
        names: &[String],
        create: bool,
        created: &mut dyn FnMut(&Path),
    ) -> Result<(), EnterError> {
        let same = self
            .names
            .iter()
            .zip(names)
            .take_while(|(a, b)| a == b)
            .count();
        self.names.truncate(same);
        self.dirs.truncate(same + 1);
        for name in &names[same..] {
            let path = self.path().join(name);
            let made = if create {
                match imp::mkdir(self, name) {
                    Ok(()) => true,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
                    Err(e) => return Err(self.classify(name, e, path)),
                }
            } else {
                false
            };
            let dir = match imp::open_dir(self, name) {
                Ok(d) => d,
                Err(e) => return Err(self.classify(name, e, path)),
            };
            #[cfg(windows)]
            if let Err(e) = self.check_inside(&dir, &path, made.then_some(true)) {
                return Err(EnterError::Io(e, path));
            }
            if made {
                created(&path);
            }
            self.names.push(name.clone());
            self.dirs.push(dir);
        }
        Ok(())
    }

    /// Why entering `name` failed: a link or a file in the way, or an I/O error.
    fn classify(&self, name: &str, e: io::Error, path: PathBuf) -> EnterError {
        match self.stat(name).map(|s| s.kind) {
            Ok(Kind::Link) => EnterError::Link(path),
            Ok(Kind::File | Kind::Other) => EnterError::NotDir(path),
            _ => EnterError::Io(e, path),
        }
    }

    /// Creates the file `name` in the current directory for writing, failing when anything is
    /// already there (a link included). On Unix it is private (`0o600`) until its mode is set.
    pub fn create_new(&self, name: &str) -> io::Result<File> {
        let f = imp::create_new(self, name)?;
        #[cfg(windows)]
        if let Err(e) = self.check_inside(&f, &self.path().join(name), Some(false)) {
            drop(f);
            return Err(e);
        }
        Ok(f)
    }

    /// Creates a new temporary file in the current directory; its name.
    pub fn create_temp(&self) -> io::Result<(File, String)> {
        let pid = std::process::id();
        let seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        for n in 0..1000u32 {
            let name = format!(".omnizip-{pid}-{seed:08x}-{n}.part");
            match self.create_new(&name) {
                Ok(f) => return Ok((f, name)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::ErrorKind::AlreadyExists.into())
    }

    /// Opens the file `name` of the current directory for reading, not through a link.
    pub fn open_file(&self, name: &str) -> io::Result<File> {
        imp::open_file(self, name)
    }

    /// What is at `name` in the current directory, without following a link.
    pub fn stat(&self, name: &str) -> io::Result<Stat> {
        imp::stat(self, name)
    }

    /// Removes the file (or link) `name` from the current directory.
    pub fn remove_file(&self, name: &str) -> io::Result<()> {
        imp::remove_file(self, name)
    }

    /// Renames `from` to `to` within the current directory, replacing `to`.
    pub fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        imp::rename(self, from, to)
    }

    /// Creates the symbolic link `name` → `target` in the current directory.
    pub fn symlink(&self, target: &str, name: &str) -> io::Result<()> {
        imp::symlink(self, target, name)
    }

    /// Creates `name` in the current directory as a hard link to `source`, a file opened through
    /// a guard (`source_path` is where it is). Fails, leaving nothing, when what the path leads
    /// to is not that file.
    pub fn hard_link(&self, source: &File, source_path: &Path, name: &str) -> io::Result<()> {
        imp::hard_link(self, source, source_path, name)
    }

    /// A path to `name` in the current directory for engines that can only write to a path
    /// (RAR). Where the system allows it the path leads through the open directory, not
    /// through the names above it (Linux `/proc/self/fd`; on Windows the held levels cannot be
    /// moved).
    pub fn path_for_engine(&self, name: &str) -> PathBuf {
        imp::path_for_engine(self, name)
    }

    /// Checks that `f`, just opened at `path`, is below the root; when it is not (a junction
    /// was set on an empty directory on the way), removes what was created there
    /// (`created`: `Some(true)` a directory, `Some(false)` a file).
    #[cfg(windows)]
    fn check_inside(&self, f: &File, path: &Path, created: Option<bool>) -> io::Result<()> {
        let (volume, p) = imp::locate(f)?;
        let root = self.root_final.1.trim_end_matches('\\');
        let inside = volume == self.root_final.0
            && p.len() > root.len()
            && p.as_bytes()[root.len()] == b'\\'
            && p[..root.len()].eq_ignore_ascii_case(root);
        if inside {
            return Ok(());
        }
        match created {
            Some(true) => {
                let _ = std::fs::remove_dir(path);
            }
            Some(false) => {
                let _ = std::fs::remove_file(path);
            }
            None => {}
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a link on the way leads out of the destination",
        ))
    }
}

#[cfg(unix)]
mod imp {
    use std::{
        ffi::CString,
        fs::File,
        io,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    use super::{DirGuard, Kind, Stat};

    fn c(name: &str) -> io::Result<CString> {
        CString::new(name).map_err(|_| io::ErrorKind::InvalidInput.into())
    }

    fn check(r: libc::c_int) -> io::Result<()> {
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn openat(dir: &File, name: &str, flags: libc::c_int, mode: libc::c_uint) -> io::Result<File> {
        let n = c(name)?;
        // SAFETY: a valid descriptor and a NUL-terminated name; the new descriptor is owned by
        // the returned file.
        let fd =
            unsafe { libc::openat(dir.as_raw_fd(), n.as_ptr(), flags | libc::O_CLOEXEC, mode) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was just opened and is owned by nobody else.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub fn open_root(root: &Path) -> io::Result<File> {
        let p = CString::new(root.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: a NUL-terminated path; the descriptor is owned by the returned file.
        let fd = unsafe {
            libc::open(
                p.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was just opened.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub fn mkdir(g: &DirGuard, name: &str) -> io::Result<()> {
        let n = c(name)?;
        // SAFETY: a valid descriptor and a NUL-terminated name.
        check(unsafe { libc::mkdirat(g.dir().as_raw_fd(), n.as_ptr(), 0o777) })
    }

    pub fn open_dir(g: &DirGuard, name: &str) -> io::Result<File> {
        openat(
            g.dir(),
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            0,
        )
    }

    pub fn create_new(g: &DirGuard, name: &str) -> io::Result<File> {
        openat(
            g.dir(),
            name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600,
        )
    }

    pub fn open_file(g: &DirGuard, name: &str) -> io::Result<File> {
        openat(g.dir(), name, libc::O_RDONLY | libc::O_NOFOLLOW, 0)
    }

    fn fstatat(dir: &File, name: &str) -> io::Result<libc::stat> {
        let n = c(name)?;
        // SAFETY: all-zero is a valid `stat`; a valid descriptor and a NUL-terminated name.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        check(unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                n.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        Ok(st)
    }

    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    pub fn stat(g: &DirGuard, name: &str) -> io::Result<Stat> {
        let st = fstatat(g.dir(), name)?;
        let kind = match st.st_mode as u32 & libc::S_IFMT as u32 {
            m if m == libc::S_IFREG as u32 => Kind::File,
            m if m == libc::S_IFDIR as u32 => Kind::Dir,
            m if m == libc::S_IFLNK as u32 => Kind::Link,
            _ => Kind::Other,
        };
        let modified = (st.st_mtime >= 0).then(|| {
            SystemTime::UNIX_EPOCH
                + Duration::new(
                    st.st_mtime as u64,
                    st.st_mtime_nsec.clamp(0, 999_999_999) as u32,
                )
        });
        Ok(Stat {
            kind,
            len: st.st_size as u64,
            modified,
            readonly: st.st_mode as u32 & 0o222 == 0,
        })
    }

    pub fn remove_file(g: &DirGuard, name: &str) -> io::Result<()> {
        let n = c(name)?;
        // SAFETY: a valid descriptor and a NUL-terminated name.
        check(unsafe { libc::unlinkat(g.dir().as_raw_fd(), n.as_ptr(), 0) })
    }

    pub fn rename(g: &DirGuard, from: &str, to: &str) -> io::Result<()> {
        let (f, t) = (c(from)?, c(to)?);
        let d = g.dir().as_raw_fd();
        // SAFETY: valid descriptors and NUL-terminated names.
        check(unsafe { libc::renameat(d, f.as_ptr(), d, t.as_ptr()) })
    }

    pub fn symlink(g: &DirGuard, target: &str, name: &str) -> io::Result<()> {
        let (t, n) = (c(target)?, c(name)?);
        // SAFETY: a valid descriptor and NUL-terminated strings.
        check(unsafe { libc::symlinkat(t.as_ptr(), g.dir().as_raw_fd(), n.as_ptr()) })
    }

    pub fn hard_link(
        g: &DirGuard,
        source: &File,
        source_path: &Path,
        name: &str,
    ) -> io::Result<()> {
        let s = CString::new(source_path.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let n = c(name)?;
        // SAFETY: a NUL-terminated path, a valid descriptor and a NUL-terminated name.
        check(unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                s.as_ptr(),
                g.dir().as_raw_fd(),
                n.as_ptr(),
                0,
            )
        })?;
        // The source path is looked up again: keep the link only when it is that file.
        use std::os::unix::fs::MetadataExt;
        let want = source.metadata()?;
        let got = fstatat(g.dir(), name)?;
        #[allow(clippy::unnecessary_cast)]
        if got.st_dev as u64 != want.dev() || got.st_ino as u64 != want.ino() {
            let _ = remove_file(g, name);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the link source changed",
            ));
        }
        Ok(())
    }

    pub fn path_for_engine(g: &DirGuard, name: &str) -> PathBuf {
        let through_fd = PathBuf::from(format!("/proc/self/fd/{}", g.dir().as_raw_fd()));
        if cfg!(target_os = "linux") && through_fd.is_dir() {
            return through_fd.join(name);
        }
        g.path().join(name)
    }
}

#[cfg(windows)]
mod imp {
    use std::{
        fs::{self, File},
        io,
        os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        },
        path::{Path, PathBuf},
    };

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_NAME_OPENED, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES,
        GetFileInformationByHandle, GetFinalPathNameByHandleW, VOLUME_NAME_NONE,
    };

    use super::{DirGuard, Kind, Stat};

    const GENERIC_READ: u32 = 0x8000_0000;

    /// A directory held so that it cannot be renamed, moved or deleted (no `FILE_SHARE_DELETE`);
    /// writing into it stays possible. Times can be set through it.
    fn hold(path: &Path, follow: bool) -> io::Result<File> {
        let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
        if !follow {
            flags |= FILE_FLAG_OPEN_REPARSE_POINT;
        }
        fs::OpenOptions::new()
            .access_mode(GENERIC_READ | FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(flags)
            .open(path)
    }

    /// Where an open file is: its volume's serial number and its path within the volume. (The
    /// drive-letter form of the path asks the mount manager on every call, several times slower,
    /// and extraction checks every file it creates.)
    pub fn locate(f: &File) -> io::Result<(u32, String)> {
        // SAFETY: all-zero is a valid value of this plain structure; a valid handle.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((info.dwVolumeSerialNumber, final_path(f)?))
    }

    fn final_path(f: &File) -> io::Result<String> {
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: a valid handle and a buffer of the length given.
            let n = unsafe {
                GetFinalPathNameByHandleW(
                    f.as_raw_handle(),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    FILE_NAME_OPENED | VOLUME_NAME_NONE,
                )
            } as usize;
            if n == 0 {
                return Err(io::Error::last_os_error());
            }
            if n < buf.len() {
                return Ok(String::from_utf16_lossy(&buf[..n]));
            }
            buf.resize(n + 1, 0);
        }
    }

    pub fn open_root(root: &Path) -> io::Result<File> {
        hold(root, true)
    }

    pub fn mkdir(g: &DirGuard, name: &str) -> io::Result<()> {
        fs::create_dir(g.path().join(name))
    }

    pub fn open_dir(g: &DirGuard, name: &str) -> io::Result<File> {
        let f = hold(&g.path().join(name), false)?;
        let attrs = f.metadata()?.file_attributes();
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 || attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        Ok(f)
    }

    pub fn create_new(g: &DirGuard, name: &str) -> io::Result<File> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(g.path().join(name))
    }

    pub fn open_file(g: &DirGuard, name: &str) -> io::Result<File> {
        let f = fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(g.path().join(name))?;
        if f.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(f)
    }

    pub fn stat(g: &DirGuard, name: &str) -> io::Result<Stat> {
        let m = fs::symlink_metadata(g.path().join(name))?;
        let attrs = m.file_attributes();
        let kind = if m.file_type().is_symlink() || attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            Kind::Link
        } else if m.is_dir() {
            Kind::Dir
        } else if m.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        Ok(Stat {
            kind,
            len: m.len(),
            modified: m.modified().ok(),
            readonly: attrs & FILE_ATTRIBUTE_READONLY != 0,
        })
    }

    pub fn remove_file(g: &DirGuard, name: &str) -> io::Result<()> {
        fs::remove_file(g.path().join(name))
    }

    pub fn rename(g: &DirGuard, from: &str, to: &str) -> io::Result<()> {
        let dir = g.path();
        fs::rename(dir.join(from), dir.join(to))
    }

    pub fn symlink(g: &DirGuard, target: &str, name: &str) -> io::Result<()> {
        let t = target.replace('/', "\\");
        let dir = g.path();
        if dir.join(&t).is_dir() {
            std::os::windows::fs::symlink_dir(&t, dir.join(name))
        } else {
            std::os::windows::fs::symlink_file(&t, dir.join(name))
        }
    }

    pub fn hard_link(
        g: &DirGuard,
        _source: &File,
        source_path: &Path,
        name: &str,
    ) -> io::Result<()> {
        // The levels to the source were held when it was written, and the link is checked below.
        let link = g.path().join(name);
        fs::hard_link(source_path, &link)?;
        let f = fs::File::open(&link)?;
        if let Err(e) = g.check_inside(&f, &link, None) {
            drop(f);
            let _ = fs::remove_file(&link);
            return Err(e);
        }
        Ok(())
    }

    pub fn path_for_engine(g: &DirGuard, name: &str) -> PathBuf {
        g.path().join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn enters_creates_and_reports_new_levels() {
        let d = tempfile::tempdir().unwrap();
        let mut g = DirGuard::open(d.path()).unwrap();
        let mut made = Vec::new();
        g.enter(&names(&["a", "b"]), true, &mut |p| made.push(p.to_owned()))
            .unwrap();
        assert_eq!(made, [d.path().join("a"), d.path().join("a/b")]);
        assert_eq!(g.path(), d.path().join("a").join("b"));
        let mut f = g.create_new("f.txt").unwrap();
        std::io::Write::write_all(&mut f, b"hi").unwrap();
        drop(f);
        assert_eq!(std::fs::read(d.path().join("a/b/f.txt")).unwrap(), b"hi");
        assert_eq!(
            g.create_new("f.txt").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let s = g.stat("f.txt").unwrap();
        assert_eq!((s.kind, s.len), (Kind::File, 2));

        // Back up and down again; existing levels are not reported.
        made.clear();
        g.enter(&names(&["a", "c"]), true, &mut |p| made.push(p.to_owned()))
            .unwrap();
        assert_eq!(made, [d.path().join("a/c")]);
        g.enter(&names(&["a", "b"]), false, &mut |_| {}).unwrap();
        let (_, tmp) = g.create_temp().unwrap();
        g.rename(&tmp, "f.txt").unwrap();
        assert_eq!(std::fs::read(d.path().join("a/b/f.txt")).unwrap(), b"");
        assert!(matches!(
            g.enter(&names(&["missing"]), false, &mut |_| {}),
            Err(EnterError::Io(..))
        ));
        std::fs::write(d.path().join("file"), b"").unwrap();
        assert!(matches!(
            g.enter(&names(&["file", "x"]), true, &mut |_| {}),
            Err(EnterError::NotDir(_))
        ));
    }

    /// A directory swapped for a link after it was checked is not entered.
    #[test]
    #[cfg(unix)]
    fn a_directory_swapped_for_a_link_is_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dest = d.path().join("dest");
        std::fs::create_dir_all(dest.join("sub")).unwrap();
        let mut g = DirGuard::open(&dest).unwrap();
        // Swapped before entering: refused.
        std::fs::remove_dir(dest.join("sub")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dest.join("sub")).unwrap();
        assert!(matches!(
            g.enter(&names(&["sub"]), true, &mut |_| {}),
            Err(EnterError::Link(_))
        ));
        // Swapped after entering: writes still go to the directory that was entered.
        std::fs::remove_file(dest.join("sub")).unwrap();
        std::fs::create_dir(dest.join("sub")).unwrap();
        g.enter(&names(&["sub"]), true, &mut |_| {}).unwrap();
        std::fs::rename(dest.join("sub"), d.path().join("moved")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dest.join("sub")).unwrap();
        drop(g.create_new("f.txt").unwrap());
        assert!(d.path().join("moved/f.txt").is_file());
        assert!(!outside.path().join("f.txt").exists());
        // A link planted at a file's name is never written through.
        std::os::unix::fs::symlink(outside.path().join("x"), d.path().join("moved/l")).unwrap();
        assert!(g.create_new("l").is_err());
        assert!(!outside.path().join("x").exists());
    }

    /// What is reached through a junction is recognised as outside, even when the path to it
    /// reads as inside (a junction set on a held directory while it was empty).
    #[test]
    #[cfg(windows)]
    fn things_reached_through_a_junction_are_outside() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("f.txt"), b"x").unwrap();
        let ok = std::process::Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(d.path().join("j"))
            .arg(outside.path())
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J");
        let g = DirGuard::open(d.path()).unwrap();
        let through = File::open(d.path().join("j").join("f.txt")).unwrap();
        assert!(g.check_inside(&through, Path::new("unused"), None).is_err());
        std::fs::write(d.path().join("in.txt"), b"x").unwrap();
        let inside = File::open(d.path().join("in.txt")).unwrap();
        g.check_inside(&inside, Path::new("unused"), None).unwrap();
        // A destination reached through a junction itself is fine: only what is below counts.
        let g = DirGuard::open(&d.path().join("j")).unwrap();
        g.check_inside(&through, Path::new("unused"), None).unwrap();
        // Nor are entering a junction or creating through one possible.
        let mut g = DirGuard::open(d.path()).unwrap();
        assert!(matches!(
            g.enter(&names(&["j"]), true, &mut |_| {}),
            Err(EnterError::Link(_))
        ));
    }

    /// While a level is held it cannot be renamed, so it cannot be swapped for a junction.
    #[test]
    #[cfg(windows)]
    fn held_levels_cannot_be_moved() {
        let d = tempfile::tempdir().unwrap();
        let mut g = DirGuard::open(d.path()).unwrap();
        g.enter(&names(&["a", "b"]), true, &mut |_| {}).unwrap();
        assert!(std::fs::rename(d.path().join("a"), d.path().join("x")).is_err());
        assert!(std::fs::rename(d.path().join("a/b"), d.path().join("a/x")).is_err());
        // Back at the root, "a" is let go.
        g.enter(&[], false, &mut |_| {}).unwrap();
        std::fs::rename(d.path().join("a"), d.path().join("x")).unwrap();
    }
}
