//! File system callbacks and the lifetime of a mounted volume.
//!
//! WinFsp calls the callbacks from its dispatcher threads, by path for `GetSecurityByName` and
//! `Open`, by file context afterwards. Paths are resolved component by component through
//! [`VirtualFs::lookup`]; the file context is a boxed [`FileCtx`] holding the node, the open
//! handle and, for directories, the listing that `ReadDirectory` pages through by name.

use std::{
    collections::HashSet,
    ffi::{OsStr, c_void},
    io,
    os::windows::ffi::OsStrExt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    ptr, slice,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use omnizip_vfs::{
    Attr, BackendCaps, BackendHealth, BackendKind, FileHandle, FsError, Mount, MountBackend,
    MountError, MountOptions, NodeId, OpenFlags, ROOT, VirtualFs,
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, PSECURITY_DESCRIPTOR,
    },
    Storage::FileSystem::GetLogicalDrives,
};

use crate::ffi::{self, *};

/// Everyone may open anything; the volume itself is read-only, so writes fail with "write
/// protected" as on a CD.
const SDDL: &str = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;WD)";

/// Longest name handed to WinFsp, in UTF-16 units.
const MAX_NAME: usize = 255;

/// The WinFsp backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct WinFsp;

/// Drive letters not in use in this session, from `Z:` downwards, skipping `A:` to `C:`.
pub fn free_drive_letters() -> Vec<char> {
    // SAFETY: no arguments.
    let used = unsafe { GetLogicalDrives() };
    ('D'..='Z')
        .rev()
        .filter(|c| used & (1 << (*c as u32 - 'A' as u32)) == 0)
        .collect()
}

impl MountBackend for WinFsp {
    fn kind(&self) -> BackendKind {
        BackendKind::WinFsp
    }

    fn capabilities(&self) -> BackendCaps {
        BackendCaps::DRIVE_LETTER | BackendCaps::DIR_MOUNTPOINT
    }

    fn health(&self) -> BackendHealth {
        match ffi::api() {
            Ok(_) => BackendHealth::Ready,
            Err(_) => match omnizip_vfs::probe(true).health {
                // Installed, but the library does not load or lacks a function.
                BackendHealth::Ready => BackendHealth::VersionMismatch,
                h => h,
            },
        }
    }

    fn mount(
        &self,
        fs: Arc<dyn VirtualFs>,
        opts: &MountOptions,
    ) -> Result<Box<dyn Mount>, MountError> {
        let api = ffi::api().map_err(MountError::NotInstalled)?;
        let stats = fs.statfs().map_err(|e| MountError::System(e.to_string()))?;
        let label: Vec<u16> = opts
            .label
            .as_deref()
            .unwrap_or(&stats.label)
            .encode_utf16()
            .take(32)
            .collect();
        let volume = Box::new(Volume {
            fs,
            sd: security_descriptor()?,
            label,
            total: stats.total_bytes.div_ceil(4096) * 4096,
            offline: opts.offline_attribute,
            files: Mutex::new(HashSet::new()),
            alive: AtomicBool::new(true),
        });

        // SAFETY: an all-zero VolumeParams is valid (it is plain integers).
        let mut params: VolumeParams = unsafe { std::mem::zeroed() };
        params.Version = size_of::<VolumeParams>() as u16;
        params.SectorSize = 4096;
        params.SectorsPerAllocationUnit = 1;
        params.MaxComponentLength = MAX_NAME as u16;
        params.VolumeCreationTime = filetime(SystemTime::now());
        params.VolumeSerialNumber = stats.serial;
        // Nothing changes under a read-only volume, but sizes that a format does not record
        // become known once decoded: let the kernel ask again after a second.
        params.FileInfoTimeout = 1000;
        params.Flags = volume_flags::CASE_PRESERVED_NAMES
            | volume_flags::UNICODE_ON_DISK
            | volume_flags::PERSISTENT_ACLS
            | volume_flags::READ_ONLY_VOLUME
            | volume_flags::POST_CLEANUP_WHEN_MODIFIED_ONLY
            | volume_flags::UM_FILE_CONTEXT_IS_USER_CONTEXT2;
        for (d, s) in params
            .FileSystemName
            .iter_mut()
            .zip(stats.fs_name.encode_utf16().take(15))
        {
            *d = s;
        }

        // From here on, dropping `mount` undoes whatever was done.
        let mut mount = WinFspMount {
            raw: ptr::null_mut(),
            volume: Box::into_raw(volume),
            point: PathBuf::new(),
            global: false,
            restore_dir: None,
        };
        let requested = opts.mount_point.as_deref().map(normalize_point);
        if let Some(p) = &requested
            && !is_drive(p)
        {
            mount.restore_dir = prepare_dir(p)?;
        }
        // A drive goes through the Mount Manager (`\\.\X:`) when WinFsp may register it there:
        // as administrator, or with WinFsp's MountUseMountmgrFromFSD setting. Only then does
        // Windows know the volume's DOS path, which `GetFinalPathNameByHandle` reports and
        // which programs use to find their own files; such a drive is global. Otherwise the
        // drive belongs to this logon session, WinFsp's default. A refused attempt leaves its
        // volume half-registered, so the fallback starts from a new one.
        let global_letter = match &requested {
            _ if !mount_manager_allowed() => None,
            Some(p) if is_drive(p) => Some(p.clone()),
            None => free_drive_letters()
                .first()
                .map(|c| PathBuf::from(format!("{c}:"))),
            Some(_) => None,
        };
        if let Some(l) = global_letter {
            let point = PathBuf::from(format!(r"\\.\{}", l.display()));
            if let Err(st) = mount.attach(api, &params, Some(&point))? {
                tracing::info!(status = %ntstatus_text(st), "no Mount Manager drive");
            }
        }
        if mount.raw.is_null()
            && let Err(st) = mount.attach(api, &params, requested.as_deref())?
        {
            return Err(match requested {
                None => MountError::NoDriveLetter,
                Some(p) if st == STATUS_OBJECT_NAME_COLLISION || is_drive(&p) => {
                    MountError::InUse(p)
                }
                Some(p) => MountError::System(format!("{}: {}", p.display(), ntstatus_text(st))),
            });
        }
        // SAFETY: the mount point was just set; WinFsp owns the string until the file system
        // is deleted.
        let point = unsafe { String::from_utf16_lossy(wstr((api.mount_point)(mount.raw))) };
        mount.global = point.starts_with(r"\\.\");
        mount.point = PathBuf::from(point.trim_start_matches(r"\\.\"));
        // SAFETY: the file system is fully set up.
        let st = unsafe { (api.start_dispatcher)(mount.raw, 0) };
        if st != STATUS_SUCCESS {
            return Err(MountError::System(format!(
                "FspFileSystemStartDispatcher: {}",
                ntstatus_text(st)
            )));
        }
        tracing::info!(point = %mount.point.display(), global = mount.global, "mounted");
        Ok(Box::new(mount))
    }
}

/// `Z`, `z:` or `Z:\` → `Z:`; directories unchanged.
fn normalize_point(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    let t = s.trim_end_matches(['\\', '/']);
    let b = t.as_bytes();
    if (b.len() == 1 || b.len() == 2 && b[1] == b':') && b[0].is_ascii_alphabetic() {
        PathBuf::from(format!("{}:", (b[0] as char).to_ascii_uppercase()))
    } else {
        p.to_owned()
    }
}

fn is_drive(p: &Path) -> bool {
    let b = p.as_os_str().as_encoded_bytes();
    b.len() == 2 && b[1] == b':'
}

/// WinFsp creates a directory mount point itself and needs the directory not to exist: an
/// empty one is removed now and recreated after unmounting.
fn prepare_dir(p: &Path) -> Result<Option<PathBuf>, MountError> {
    match std::fs::read_dir(p) {
        Ok(mut it) => {
            if it.next().is_some() {
                return Err(MountError::InUse(p.to_owned()));
            }
            std::fs::remove_dir(p).map_err(|_| MountError::InUse(p.to_owned()))?;
            Ok(Some(p.to_owned()))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(MountError::InUse(p.to_owned())),
    }
}

fn security_descriptor() -> Result<Vec<u8>, MountError> {
    let sddl = wide(OsStr::new(SDDL));
    let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let mut size = 0u32;
    // SAFETY: valid SDDL; the result is freed with LocalFree below.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, &mut size)
    };
    if ok == 0 {
        return Err(MountError::System(io::Error::last_os_error().to_string()));
    }
    // SAFETY: `size` bytes at `sd`, a self-relative descriptor.
    let bytes = unsafe { slice::from_raw_parts(sd.cast::<u8>(), size as usize) }.to_vec();
    // SAFETY: allocated by the conversion above.
    unsafe { LocalFree(sd) };
    Ok(bytes)
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// The UTF-16 string at `p`, without its terminator.
///
/// # Safety
/// `p` points to a NUL-terminated UTF-16 string that outlives the result.
unsafe fn wstr<'a>(p: *const u16) -> &'a [u16] {
    let mut n = 0;
    // SAFETY: per the contract, every unit up to the terminator is readable.
    unsafe {
        while *p.add(n) != 0 {
            n += 1;
        }
        slice::from_raw_parts(p, n)
    }
}

fn ntstatus_text(st: NTSTATUS) -> String {
    format!("NTSTATUS 0x{:08X}", st as u32)
}

/// 100 ns intervals since 1601.
fn filetime(t: SystemTime) -> u64 {
    const UNIX: u64 = 116_444_736_000_000_000;
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => UNIX.saturating_add((d.as_nanos() / 100).min(u128::from(u64::MAX)) as u64),
        Err(e) => UNIX.saturating_sub((e.duration().as_nanos() / 100).min(u128::from(UNIX)) as u64),
    }
}

fn status(e: FsError) -> NTSTATUS {
    e.ntstatus() as NTSTATUS
}

/// Runs a callback body, turning panics (a decoder choking on corrupt data) into I/O errors:
/// unwinding into WinFsp would abort the process.
fn guard(f: impl FnOnce() -> Result<(), NTSTATUS>) -> NTSTATUS {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => STATUS_SUCCESS,
        Ok(Err(st)) => st,
        Err(_) => STATUS_UNEXPECTED_IO_ERROR,
    }
}

/// What a volume's callbacks share (`FSP_FILE_SYSTEM::UserContext`).
struct Volume {
    fs: Arc<dyn VirtualFs>,
    /// Self-relative security descriptor of every file.
    sd: Vec<u8>,
    label: Vec<u16>,
    total: u64,
    offline: bool,
    /// Open file contexts, freed at unmount if WinFsp never closed them.
    files: Mutex<HashSet<usize>>,
    alive: AtomicBool,
}

/// An open file or directory (the `FileContext` of the callbacks).
struct FileCtx {
    node: NodeId,
    fh: Option<FileHandle>,
    /// Directory listing, built on the first `ReadDirectory` of this handle.
    listing: Mutex<Option<Arc<Vec<DirItem>>>>,
}

struct DirItem {
    name: Vec<u16>,
    info: FileInfo,
}

/// A [`DirInfo`] followed by room for the longest name.
#[repr(C)]
struct DirInfoBuf {
    info: DirInfo,
    name: [u16; MAX_NAME],
}

impl Volume {
    /// # Safety
    /// `fs` is a file system whose `UserContext` is a live `Volume`.
    unsafe fn of<'a>(fs: *mut FileSystem) -> &'a Volume {
        // SAFETY: per the contract.
        unsafe { &*(*fs).UserContext.cast::<Volume>() }
    }

    /// Files are not marked read-only: the volume is, and copies taken out of it should be
    /// ordinary files. A read-only attribute would also make WinFsp answer a write with
    /// "access denied" before [`open`] can answer "write protected".
    fn attributes(&self, a: &Attr) -> u32 {
        if a.is_dir() {
            FILE_ATTRIBUTE_DIRECTORY
        } else if self.offline {
            FILE_ATTRIBUTE_OFFLINE
        } else {
            0
        }
    }

    fn file_info(&self, a: &Attr) -> FileInfo {
        let t = filetime(a.mtime);
        let size = if a.is_dir() { 0 } else { a.size };
        FileInfo {
            FileAttributes: self.attributes(a),
            AllocationSize: size.div_ceil(4096) * 4096,
            FileSize: size,
            CreationTime: t,
            LastAccessTime: t,
            LastWriteTime: t,
            ChangeTime: t,
            IndexNumber: u64::from(a.node),
            ..FileInfo::default()
        }
    }

    /// Resolves a volume path (`\dir\file`).
    ///
    /// # Safety
    /// `name` is a NUL-terminated UTF-16 string.
    unsafe fn lookup(&self, name: *const u16) -> Result<Attr, NTSTATUS> {
        // SAFETY: per the contract.
        unsafe { self.walk(name, false) }.map(|(attr, _)| attr)
    }

    /// [`lookup`](Self::lookup), with the path as the volume spells it (`\Dir\File.txt` for
    /// `\dir\FILE.TXT`): the normalized name of a case-insensitive volume.
    ///
    /// # Safety
    /// `name` is a NUL-terminated UTF-16 string.
    unsafe fn lookup_normalized(&self, name: *const u16) -> Result<(Attr, String), NTSTATUS> {
        // SAFETY: per the contract.
        unsafe { self.walk(name, true) }
    }

    /// # Safety
    /// `name` is a NUL-terminated UTF-16 string.
    unsafe fn walk(&self, name: *const u16, spell: bool) -> Result<(Attr, String), NTSTATUS> {
        // SAFETY: per the contract.
        let path =
            String::from_utf16(unsafe { wstr(name) }).map_err(|_| STATUS_OBJECT_NAME_NOT_FOUND)?;
        let mut attr = self.fs.getattr(ROOT).map_err(status)?;
        let mut spelled = String::new();
        let mut parts = path.split('\\').filter(|c| !c.is_empty()).peekable();
        while let Some(part) = parts.next() {
            let last = parts.peek().is_none();
            if !attr.is_dir() {
                return Err(STATUS_OBJECT_PATH_NOT_FOUND);
            }
            let found = if spell {
                self.fs.lookup_named(attr.node, OsStr::new(part))
            } else {
                self.fs
                    .lookup(attr.node, OsStr::new(part))
                    .map(|a| (a, String::new()))
            };
            let name;
            (attr, name) = match found {
                Ok(found) => found,
                Err(FsError::NotFound) if last => return Err(STATUS_OBJECT_NAME_NOT_FOUND),
                Err(FsError::NotFound) => return Err(STATUS_OBJECT_PATH_NOT_FOUND),
                Err(e) => return Err(status(e)),
            };
            if spell {
                spelled.push('\\');
                spelled.push_str(&name);
            }
        }
        if spell && spelled.is_empty() {
            spelled.push('\\');
        }
        Ok((attr, spelled))
    }

    /// Copies the security descriptor, or reports the size it needs.
    ///
    /// # Safety
    /// `size` is null or valid; `sd` is null or has `*size` writable bytes.
    unsafe fn copy_sd(&self, sd: *mut c_void, size: *mut usize) -> Result<(), NTSTATUS> {
        if size.is_null() {
            return Ok(());
        }
        // SAFETY: per the contract.
        unsafe {
            let room = *size;
            *size = self.sd.len();
            if room < self.sd.len() {
                return Err(STATUS_BUFFER_OVERFLOW);
            }
            if !sd.is_null() {
                ptr::copy_nonoverlapping(self.sd.as_ptr(), sd.cast::<u8>(), self.sd.len());
            }
        }
        Ok(())
    }

    fn listing(&self, ctx: &FileCtx) -> Result<Arc<Vec<DirItem>>, NTSTATUS> {
        let mut slot = ctx.listing.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(l) = &*slot {
            return Ok(l.clone());
        }
        let mut items = Vec::new();
        if ctx.node != ROOT {
            // Like NTFS, every directory but the root lists `.` and `..`.
            let me = self.file_info(&self.fs.getattr(ctx.node).map_err(status)?);
            for dots in [".", ".."] {
                items.push(DirItem {
                    name: dots.encode_utf16().collect(),
                    info: me,
                });
            }
        }
        for e in self.fs.readdir(ctx.node, 0, usize::MAX).map_err(status)? {
            let mut name: Vec<u16> = e.name.encode_utf16().collect();
            name.truncate(MAX_NAME);
            items.push(DirItem {
                name,
                info: self.file_info(&e.attr),
            });
        }
        let l = Arc::new(items);
        *slot = Some(l.clone());
        Ok(l)
    }
}

/// # Safety
/// `ctx` is a context stored by [`open`] and not yet closed.
unsafe fn file<'a>(ctx: *mut c_void) -> &'a FileCtx {
    // SAFETY: per the contract.
    unsafe { &*ctx.cast::<FileCtx>() }
}

static INTERFACE: Interface = Interface {
    GetVolumeInfo: Some(get_volume_info),
    SetVolumeLabel: 0,
    GetSecurityByName: Some(get_security_by_name),
    Create: Some(create),
    Open: Some(open),
    Overwrite: Some(overwrite),
    Cleanup: 0,
    Close: Some(close),
    Read: Some(read),
    Write: Some(write),
    Flush: 0,
    GetFileInfo: Some(get_file_info),
    SetBasicInfo: Some(set_basic_info),
    SetFileSize: Some(set_file_size),
    CanDelete: Some(can_delete),
    Rename: Some(rename),
    GetSecurity: Some(get_security),
    SetSecurity: Some(set_security),
    ReadDirectory: Some(read_directory),
    ResolveReparsePoints: 0,
    GetReparsePoint: 0,
    SetReparsePoint: 0,
    DeleteReparsePoint: 0,
    GetStreamInfo: 0,
    GetDirInfoByName: 0,
    Control: 0,
    SetDelete: 0,
    CreateEx: 0,
    OverwriteEx: 0,
    GetEa: 0,
    SetEa: 0,
    Obsolete0: 0,
    DispatcherStopped: Some(dispatcher_stopped),
    Reserved: [0; 31],
};

// The callbacks below are only called by WinFsp, with the pointers its documentation
// describes: `fs` carries our `Volume`, `ctx` a `FileCtx` from `open`.

unsafe extern "C" fn get_volume_info(fs: *mut FileSystem, info: *mut VolumeInfo) -> NTSTATUS {
    guard(|| {
        // SAFETY: see above.
        let (v, info) = unsafe { (Volume::of(fs), &mut *info) };
        info.TotalSize = v.total;
        info.FreeSize = 0;
        info.VolumeLabelLength = (v.label.len() * 2) as u16;
        info.VolumeLabel[..v.label.len()].copy_from_slice(&v.label);
        Ok(())
    })
}

unsafe extern "C" fn get_security_by_name(
    fs: *mut FileSystem,
    name: *const u16,
    attributes: *mut u32,
    sd: *mut c_void,
    sd_size: *mut usize,
) -> NTSTATUS {
    guard(|| {
        // SAFETY: see above.
        unsafe {
            let v = Volume::of(fs);
            let attr = v.lookup(name)?;
            if !attributes.is_null() {
                *attributes = v.attributes(&attr);
            }
            v.copy_sd(sd, sd_size)
        }
    })
}

unsafe extern "C" fn create(
    _fs: *mut FileSystem,
    _name: *const u16,
    _options: u32,
    _access: u32,
    _attributes: u32,
    _sd: *mut c_void,
    _allocation: u64,
    _ctx: *mut *mut c_void,
    _info: *mut FileInfo,
) -> NTSTATUS {
    STATUS_MEDIA_WRITE_PROTECTED
}

unsafe extern "C" fn overwrite(
    _fs: *mut FileSystem,
    _ctx: *mut c_void,
    _attributes: u32,
    _replace: u8,
    _allocation: u64,
    _info: *mut FileInfo,
) -> NTSTATUS {
    STATUS_MEDIA_WRITE_PROTECTED
}

unsafe extern "C" fn open(
    fs: *mut FileSystem,
    name: *const u16,
    options: u32,
    access: u32,
    ctx: *mut *mut c_void,
    info: *mut FileInfo,
) -> NTSTATUS {
    guard(|| {
        // As on write-protected media: asking to change, delete or re-secure anything fails
        // here, so programs learn it when they open, not when they save.
        if requested_access(options, access) & WRITE_ACCESS != 0
            || options & FILE_DELETE_ON_CLOSE != 0
        {
            return Err(STATUS_MEDIA_WRITE_PROTECTED);
        }
        // SAFETY: see above.
        let v = unsafe { Volume::of(fs) };
        let (attr, spelled) = unsafe { v.lookup_normalized(name) }?;
        if options & FILE_DIRECTORY_FILE != 0 && !attr.is_dir() {
            return Err(STATUS_NOT_A_DIRECTORY);
        }
        if options & FILE_NON_DIRECTORY_FILE != 0 && attr.is_dir() {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        let fh = if attr.is_dir() {
            None
        } else {
            Some(v.fs.open(attr.node, OpenFlags::default()).map_err(status)?)
        };
        let file = Box::into_raw(Box::new(FileCtx {
            node: attr.node,
            fh,
            listing: Mutex::new(None),
        }));
        v.files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file as usize);
        // SAFETY: see above; `info` is an `FSP_FSCTL_OPEN_FILE_INFO`.
        unsafe {
            let open_info = &mut *info.cast::<OpenFileInfo>();
            open_info.FileInfo = v.file_info(&attr);
            set_normalized_name(open_info, &spelled);
            *ctx = file.cast();
        }
        Ok(())
    })
}

/// The access the caller asked for. `granted` is what the security descriptor allows, which
/// is everything when the caller asked for `MAXIMUM_ALLOWED`: such an open must succeed, read
/// only. The request being served says what was asked, once its layout checks out (it carries
/// the `options` WinFsp passed to `open`). Failing that, a full grant counts as
/// `MAXIMUM_ALLOWED`.
fn requested_access(options: u32, granted: u32) -> u32 {
    if let Ok(api) = ffi::api() {
        // SAFETY: called inside a callback, on the dispatcher thread serving the request;
        // a create request is longer than the fields read.
        unsafe {
            let op = (api.operation_context)();
            if !op.is_null() && !(*op).Request.is_null() {
                let req = (*op).Request;
                let field = |at: usize| req.add(at).cast::<u32>().read_unaligned();
                if field(request::KIND) == request::CREATE_KIND
                    && field(request::CREATE_OPTIONS) == options
                {
                    return field(request::DESIRED_ACCESS);
                }
            }
        }
    }
    if granted == FILE_ALL_ACCESS {
        MAXIMUM_ALLOWED
    } else {
        granted
    }
}

/// Reports how the volume spells an opened path, when WinFsp left room for it (see
/// [`OpenFileInfo`]).
///
/// # Safety
/// `NormalizedName` is null or has `NormalizedNameSize` writable bytes.
unsafe fn set_normalized_name(info: &mut OpenFileInfo, path: &str) {
    let w: Vec<u16> = path.encode_utf16().collect();
    let bytes = w.len() * 2;
    if info.NormalizedName.is_null() || bytes > usize::from(info.NormalizedNameSize) {
        return;
    }
    // SAFETY: per the contract.
    unsafe { ptr::copy_nonoverlapping(w.as_ptr(), info.NormalizedName, w.len()) };
    info.NormalizedNameSize = bytes as u16;
}

// Changes through a handle opened without asking for write access (`MAXIMUM_ALLOWED`).

unsafe extern "C" fn write(
    _fs: *mut FileSystem,
    _ctx: *mut c_void,
    _buf: *mut c_void,
    _offset: u64,
    _len: u32,
    _to_end: u8,
    _constrained: u8,
    _done: *mut u32,
    _info: *mut FileInfo,
) -> NTSTATUS {
    STATUS_MEDIA_WRITE_PROTECTED
}

unsafe extern "C" fn set_basic_info(
    _fs: *mut FileSystem,
    _ctx: *mut c_void,
    _attributes: u32,
    _creation: u64,
    _access: u64,
    _write: u64,
    _change: u64,
    _info: *mut FileInfo,
) -> NTSTATUS {
    STATUS_MEDIA_WRITE_PROTECTED
}

unsafe extern "C" fn set_file_size(
    _fs: *mut FileSystem,
    _ctx: *mut c_void,
    _size: u64,
    _allocation: u8,
    _info: *mut FileInfo,
) -> NTSTATUS {
    STATUS_MEDIA_WRITE_PROTECTED
}

unsafe extern "C" fn can_delete(
    _fs: *mut FileSystem,
    _ctx: *mut c_void,
    _name: *const u16,
) -> NTSTATUS {
    STATUS_MEDIA_WRITE_PROTECTED
}

unsafe extern "C" fn rename(
    _fs: *mut FileSystem,
    _ctx: *mut c_void,
    _name: *const u16,
    _new_name: *const u16,
    _replace: u8,
) -> NTSTATUS {
    STATUS_MEDIA_WRITE_PROTECTED
}

unsafe extern "C" fn set_security(
    _fs: *mut FileSystem,
    _ctx: *mut c_void,
    _what: u32,
    _sd: *mut c_void,
) -> NTSTATUS {
    STATUS_MEDIA_WRITE_PROTECTED
}

unsafe extern "C" fn close(fs: *mut FileSystem, ctx: *mut c_void) {
    guard(|| {
        // SAFETY: see above.
        let v = unsafe { Volume::of(fs) };
        if v.files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(ctx as usize))
        {
            // SAFETY: allocated by `open` and removed from the set just now, so freed once.
            let file = unsafe { Box::from_raw(ctx.cast::<FileCtx>()) };
            if let Some(fh) = file.fh {
                v.fs.release(fh);
            }
        }
        Ok(())
    });
}

unsafe extern "C" fn read(
    fs: *mut FileSystem,
    ctx: *mut c_void,
    buf: *mut c_void,
    offset: u64,
    len: u32,
    done: *mut u32,
) -> NTSTATUS {
    guard(|| {
        // SAFETY: see above.
        let (v, file) = unsafe { (Volume::of(fs), file(ctx)) };
        let fh = file.fh.ok_or(STATUS_INVALID_DEVICE_REQUEST)?;
        let data = v.fs.read(fh, offset, len).map_err(status)?;
        if data.is_empty() && len > 0 {
            return Err(STATUS_END_OF_FILE);
        }
        let n = data.len().min(len as usize);
        // SAFETY: WinFsp's buffer holds `len` bytes.
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), buf.cast::<u8>(), n);
            *done = n as u32;
        }
        Ok(())
    })
}

unsafe extern "C" fn get_file_info(
    fs: *mut FileSystem,
    ctx: *mut c_void,
    info: *mut FileInfo,
) -> NTSTATUS {
    guard(|| {
        // SAFETY: see above.
        let (v, file) = unsafe { (Volume::of(fs), file(ctx)) };
        let attr = v.fs.getattr(file.node).map_err(status)?;
        // SAFETY: see above.
        unsafe { *info = v.file_info(&attr) };
        Ok(())
    })
}

unsafe extern "C" fn get_security(
    fs: *mut FileSystem,
    _ctx: *mut c_void,
    sd: *mut c_void,
    sd_size: *mut usize,
) -> NTSTATUS {
    // SAFETY: see above.
    guard(|| unsafe { Volume::of(fs).copy_sd(sd, sd_size) })
}

unsafe extern "C" fn read_directory(
    fs: *mut FileSystem,
    ctx: *mut c_void,
    _pattern: *const u16,
    marker: *const u16,
    buf: *mut c_void,
    len: u32,
    done: *mut u32,
) -> NTSTATUS {
    guard(|| {
        let api = ffi::api().map_err(|_| STATUS_INVALID_DEVICE_REQUEST)?;
        // SAFETY: see above.
        let (v, file) = unsafe { (Volume::of(fs), file(ctx)) };
        let items = v.listing(file)?;
        // The marker is the last name returned by the previous call on this handle.
        let start = if marker.is_null() {
            0
        } else {
            // SAFETY: see above.
            let m = unsafe { wstr(marker) };
            items
                .iter()
                .position(|i| i.name == m)
                .map_or(items.len(), |i| i + 1)
        };
        // SAFETY: plain integers.
        let mut entry: DirInfoBuf = unsafe { std::mem::zeroed() };
        for item in &items[start..] {
            entry.info.Size = (size_of::<DirInfo>() + item.name.len() * 2) as u16;
            entry.info.FileInfo = item.info;
            entry.name[..item.name.len()].copy_from_slice(&item.name);
            // SAFETY: `entry` is a complete DirInfo of `Size` bytes; buf/len/done come from
            // WinFsp.
            if unsafe { (api.add_dir_info)(&mut entry.info, buf, len, done) } == 0 {
                // Full: the next call continues after the last name that fitted.
                return Ok(());
            }
        }
        // SAFETY: a null entry marks the end of the directory.
        unsafe { (api.add_dir_info)(ptr::null_mut(), buf, len, done) };
        Ok(())
    })
}

unsafe extern "C" fn dispatcher_stopped(fs: *mut FileSystem, normally: u8) {
    if normally == 0 {
        // SAFETY: see above.
        unsafe { Volume::of(fs) }
            .alive
            .store(false, Ordering::SeqCst);
        tracing::warn!("WinFsp stopped a volume on its own");
    }
}

/// A mounted volume; dropping it unmounts.
struct WinFspMount {
    raw: *mut FileSystem,
    volume: *mut Volume,
    point: PathBuf,
    /// A drive registered with the Mount Manager (visible to every session).
    global: bool,
    /// An empty directory removed for the mount point, recreated afterwards.
    restore_dir: Option<PathBuf>,
}

// SAFETY: the pointers are only used in `drop`, and WinFsp's functions may be called from any
// thread; the volume itself is Sync.
unsafe impl Send for WinFspMount {}
unsafe impl Sync for WinFspMount {}

impl Mount for WinFspMount {
    fn mount_point(&self) -> &Path {
        &self.point
    }

    fn is_alive(&self) -> bool {
        // SAFETY: the volume lives as long as `self`.
        unsafe { &*self.volume }.alive.load(Ordering::SeqCst)
    }
}

impl WinFspMount {
    /// Creates the WinFsp file system of the volume and sets its mount point (`None`: the
    /// next free drive letter). A refused mount point is the inner error, after which the file
    /// system is deleted again.
    fn attach(
        &mut self,
        api: &ffi::Api,
        params: &VolumeParams,
        point: Option<&Path>,
    ) -> Result<Result<(), NTSTATUS>, MountError> {
        let device = wide(OsStr::new(DISK_DEVICE_NAME));
        let mut raw: *mut FileSystem = ptr::null_mut();
        // SAFETY: valid parameters; INTERFACE is static.
        let st = unsafe { (api.create)(device.as_ptr(), params, &INTERFACE, &mut raw) };
        if st != STATUS_SUCCESS {
            return Err(MountError::System(format!(
                "FspFileSystemCreate: {}",
                ntstatus_text(st)
            )));
        }
        // SAFETY: `raw` is the file system WinFsp just created; no callback runs before the
        // dispatcher starts.
        unsafe { (*raw).UserContext = self.volume.cast() };
        let point_w = point.map(|p| wide(p.as_os_str()));
        // SAFETY: a NUL-terminated mount point, or null for the next free drive letter.
        let st = unsafe {
            (api.set_mount_point)(raw, point_w.as_ref().map_or(ptr::null(), |w| w.as_ptr()))
        };
        if st != STATUS_SUCCESS {
            // SAFETY: created above, never started.
            unsafe { (api.delete)(raw) };
            return Ok(Err(st));
        }
        self.raw = raw;
        Ok(Ok(()))
    }
}

/// Whether WinFsp may register drives with the Mount Manager: from this process when it is
/// elevated (the Mount Manager lets only administrators in), or from WinFsp's driver when
/// WinFsp is set to (`MountUseMountmgrFromFSD`, machine-wide).
fn mount_manager_allowed() -> bool {
    omnizip_vfs::backend::winfsp_setting("MountUseMountmgrFromFSD") == Some(1) || elevated()
}

fn elevated() -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token = ptr::null_mut();
    // SAFETY: the pseudo-handle of this process; the token is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0;
    // SAFETY: a buffer the size of TOKEN_ELEVATION.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut e).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    // SAFETY: opened above.
    unsafe { CloseHandle(token) };
    ok != 0 && e.TokenIsElevated != 0
}

impl Drop for WinFspMount {
    fn drop(&mut self) {
        if !self.raw.is_null()
            && let Ok(api) = ffi::api()
        {
            // SAFETY: `raw` was created by FspFileSystemCreate and is deleted once, here.
            // Removing the mount point and stopping the dispatcher are no-ops when they never
            // happened (a failed mount).
            unsafe {
                (api.remove_mount_point)(self.raw);
                (api.stop_dispatcher)(self.raw);
                (api.delete)(self.raw);
            }
        }
        // SAFETY: no callback runs any more; the volume was boxed in `mount`.
        let volume = unsafe { Box::from_raw(self.volume) };
        let files = std::mem::take(&mut *volume.files.lock().unwrap_or_else(|e| e.into_inner()));
        for f in files {
            // SAFETY: contexts WinFsp never closed, freed once.
            let file = unsafe { Box::from_raw(f as *mut FileCtx) };
            if let Some(fh) = file.fh {
                volume.fs.release(fh);
            }
        }
        if let Some(d) = &self.restore_dir {
            let _ = std::fs::create_dir(d);
        }
        tracing::info!(point = %self.point.display(), "unmounted");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_are_normalized() {
        assert_eq!(normalize_point(Path::new("z")), PathBuf::from("Z:"));
        assert_eq!(normalize_point(Path::new("q:\\")), PathBuf::from("Q:"));
        assert_eq!(
            normalize_point(Path::new("D:\\mnt\\a")),
            PathBuf::from("D:\\mnt\\a")
        );
        assert!(is_drive(Path::new("Z:")));
        assert!(!is_drive(Path::new("Z:\\x")));
    }

    #[test]
    fn filetimes_count_from_1601() {
        assert_eq!(filetime(UNIX_EPOCH), 116_444_736_000_000_000);
        assert_eq!(
            filetime(UNIX_EPOCH + std::time::Duration::from_secs(1)),
            116_444_736_010_000_000
        );
        assert_eq!(
            filetime(SystemTime::UNIX_EPOCH - std::time::Duration::from_secs(1)),
            116_444_735_990_000_000
        );
    }

    #[test]
    fn free_letters_exclude_the_system_drive() {
        assert!(!free_drive_letters().contains(&'C'));
    }
}
