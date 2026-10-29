//! The parts of the WinFsp 2.x API this backend uses (`inc/winfsp/fsctl.h`, `winfsp.h`), and
//! the loader for the WinFsp library. Field names follow the headers.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::{ffi::c_void, io, os::windows::ffi::OsStrExt, sync::OnceLock};

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

pub type NTSTATUS = i32;

pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_BUFFER_OVERFLOW: NTSTATUS = 0x8000_0005_u32 as i32;
pub const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010_u32 as i32;
pub const STATUS_END_OF_FILE: NTSTATUS = 0xC000_0011_u32 as i32;
pub const STATUS_OBJECT_NAME_NOT_FOUND: NTSTATUS = 0xC000_0034_u32 as i32;
pub const STATUS_OBJECT_NAME_COLLISION: NTSTATUS = 0xC000_0035_u32 as i32;
pub const STATUS_OBJECT_PATH_NOT_FOUND: NTSTATUS = 0xC000_003A_u32 as i32;
pub const STATUS_MEDIA_WRITE_PROTECTED: NTSTATUS = 0xC000_00A2_u32 as i32;
pub const STATUS_FILE_IS_A_DIRECTORY: NTSTATUS = 0xC000_00BA_u32 as i32;
pub const STATUS_UNEXPECTED_IO_ERROR: NTSTATUS = 0xC000_00E9_u32 as i32;
pub const STATUS_NOT_A_DIRECTORY: NTSTATUS = 0xC000_0103_u32 as i32;

pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
pub const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;

/// `CreateOptions` bits.
pub const FILE_DIRECTORY_FILE: u32 = 0x1;
pub const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
pub const FILE_DELETE_ON_CLOSE: u32 = 0x1000;

/// Access rights that change a file or a directory (`winnt.h`): write data or append (add a
/// file or a subdirectory), extended attributes, delete a child, write attributes, delete,
/// change the security, and the generic rights that include writing.
pub const WRITE_ACCESS: u32 = 0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000
    | 0x4000_0000 // GENERIC_WRITE
    | 0x1000_0000; // GENERIC_ALL
pub const FILE_ALL_ACCESS: u32 = 0x1F_01FF;
pub const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

/// `FSP_FSCTL_DISK_DEVICE_NAME`.
pub const DISK_DEVICE_NAME: &str = "WinFsp.Disk";

/// Bits of the first flags word of [`VolumeParams`] (C bit-fields, lowest bit first).
pub mod volume_flags {
    pub const CASE_PRESERVED_NAMES: u32 = 1 << 1;
    pub const UNICODE_ON_DISK: u32 = 1 << 2;
    pub const PERSISTENT_ACLS: u32 = 1 << 3;
    pub const READ_ONLY_VOLUME: u32 = 1 << 9;
    pub const POST_CLEANUP_WHEN_MODIFIED_ONLY: u32 = 1 << 10;
    /// The `FileContext` of callbacks is the pointer `Open` stored.
    pub const UM_FILE_CONTEXT_IS_USER_CONTEXT2: u32 = 1 << 16;
}

/// `FSP_FSCTL_VOLUME_PARAMS`.
#[repr(C)]
pub struct VolumeParams {
    pub Version: u16,
    pub SectorSize: u16,
    pub SectorsPerAllocationUnit: u16,
    pub MaxComponentLength: u16,
    pub VolumeCreationTime: u64,
    pub VolumeSerialNumber: u32,
    pub TransactTimeout: u32,
    pub IrpTimeout: u32,
    pub IrpCapacity: u32,
    pub FileInfoTimeout: u32,
    /// [`volume_flags`].
    pub Flags: u32,
    pub Prefix: [u16; 192],
    pub FileSystemName: [u16; 16],
    pub AdditionalFlags: u32,
    pub VolumeInfoTimeout: u32,
    pub DirInfoTimeout: u32,
    pub SecurityTimeout: u32,
    pub StreamInfoTimeout: u32,
    pub EaTimeout: u32,
    pub FsextControlCode: u32,
    pub Reserved32: [u32; 1],
    pub Reserved64: [u64; 2],
}

/// `FSP_FSCTL_VOLUME_INFO`.
#[repr(C)]
pub struct VolumeInfo {
    pub TotalSize: u64,
    pub FreeSize: u64,
    /// In bytes.
    pub VolumeLabelLength: u16,
    pub VolumeLabel: [u16; 32],
}

/// `FSP_FSCTL_FILE_INFO`. Times are `FILETIME`s.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FileInfo {
    pub FileAttributes: u32,
    pub ReparseTag: u32,
    pub AllocationSize: u64,
    pub FileSize: u64,
    pub CreationTime: u64,
    pub LastAccessTime: u64,
    pub LastWriteTime: u64,
    pub ChangeTime: u64,
    pub IndexNumber: u64,
    pub HardLinks: u32,
    pub EaSize: u32,
}

/// `FSP_FSCTL_OPEN_FILE_INFO`: what the `FileInfo` argument of `Open` really points to. A
/// case-insensitive file system may write the normalized name (the path from the root as the
/// volume spells it) into `NormalizedName`, `NormalizedNameSize` bytes at most, and set the
/// size it wrote; otherwise WinFsp reports the opened name in upper case.
#[repr(C)]
pub struct OpenFileInfo {
    pub FileInfo: FileInfo,
    pub NormalizedName: *mut u16,
    pub NormalizedNameSize: u16,
}

/// `FSP_FILE_SYSTEM_OPERATION_CONTEXT`: the request a dispatcher thread is serving.
#[repr(C)]
pub struct OperationContext {
    pub Request: *const u8,
    pub Response: *mut c_void,
}

/// Fields of `FSP_FSCTL_TRANSACT_REQ` read from a create request: byte offsets of `Kind`,
/// `Req.Create.CreateOptions` and `Req.Create.DesiredAccess`, and the create kind.
pub mod request {
    pub const KIND: usize = 4;
    pub const CREATE_OPTIONS: usize = 16;
    pub const DESIRED_ACCESS: usize = 48;
    pub const CREATE_KIND: u32 = 1;
}

/// `FSP_FSCTL_DIR_INFO` without its trailing name; `Size` covers the header and the name.
#[repr(C)]
pub struct DirInfo {
    pub Size: u16,
    pub FileInfo: FileInfo,
    pub NextOffset: u64,
    pub Padding: [u8; 16],
}

/// `FSP_FILE_SYSTEM`; only its leading fields are declared, and it is only ever handled through
/// pointers that WinFsp allocated.
#[repr(C)]
pub struct FileSystem {
    pub Version: u16,
    pub UserContext: *mut c_void,
}

const _: () = {
    assert!(size_of::<VolumeParams>() == 504);
    assert!(size_of::<VolumeInfo>() == 88);
    assert!(size_of::<FileInfo>() == 72);
    assert!(size_of::<DirInfo>() == 104);
    assert!(size_of::<OpenFileInfo>() == 88);
    assert!(size_of::<Interface>() == 64 * size_of::<usize>());
};

type Fs = *mut FileSystem;

/// `FSP_FILE_SYSTEM_INTERFACE`. Operations a read-only volume does not need stay null (`0`),
/// which WinFsp answers with `STATUS_INVALID_DEVICE_REQUEST`. `Create`, `Open` and `Overwrite`
/// must all be set, or WinFsp refuses every open.
#[repr(C)]
pub struct Interface {
    pub GetVolumeInfo: Option<unsafe extern "C" fn(Fs, *mut VolumeInfo) -> NTSTATUS>,
    pub SetVolumeLabel: usize,
    pub GetSecurityByName:
        Option<unsafe extern "C" fn(Fs, *const u16, *mut u32, *mut c_void, *mut usize) -> NTSTATUS>,
    pub Create: Option<
        unsafe extern "C" fn(
            Fs,
            *const u16,
            u32,
            u32,
            u32,
            *mut c_void,
            u64,
            *mut *mut c_void,
            *mut FileInfo,
        ) -> NTSTATUS,
    >,
    pub Open: Option<
        unsafe extern "C" fn(Fs, *const u16, u32, u32, *mut *mut c_void, *mut FileInfo) -> NTSTATUS,
    >,
    pub Overwrite:
        Option<unsafe extern "C" fn(Fs, *mut c_void, u32, u8, u64, *mut FileInfo) -> NTSTATUS>,
    pub Cleanup: usize,
    pub Close: Option<unsafe extern "C" fn(Fs, *mut c_void)>,
    pub Read:
        Option<unsafe extern "C" fn(Fs, *mut c_void, *mut c_void, u64, u32, *mut u32) -> NTSTATUS>,
    pub Write: Option<
        unsafe extern "C" fn(
            Fs,
            *mut c_void,
            *mut c_void,
            u64,
            u32,
            u8,
            u8,
            *mut u32,
            *mut FileInfo,
        ) -> NTSTATUS,
    >,
    pub Flush: usize,
    pub GetFileInfo: Option<unsafe extern "C" fn(Fs, *mut c_void, *mut FileInfo) -> NTSTATUS>,
    pub SetBasicInfo: Option<
        unsafe extern "C" fn(Fs, *mut c_void, u32, u64, u64, u64, u64, *mut FileInfo) -> NTSTATUS,
    >,
    pub SetFileSize:
        Option<unsafe extern "C" fn(Fs, *mut c_void, u64, u8, *mut FileInfo) -> NTSTATUS>,
    pub CanDelete: Option<unsafe extern "C" fn(Fs, *mut c_void, *const u16) -> NTSTATUS>,
    pub Rename:
        Option<unsafe extern "C" fn(Fs, *mut c_void, *const u16, *const u16, u8) -> NTSTATUS>,
    pub GetSecurity:
        Option<unsafe extern "C" fn(Fs, *mut c_void, *mut c_void, *mut usize) -> NTSTATUS>,
    pub SetSecurity: Option<unsafe extern "C" fn(Fs, *mut c_void, u32, *mut c_void) -> NTSTATUS>,
    pub ReadDirectory: Option<
        unsafe extern "C" fn(
            Fs,
            *mut c_void,
            *const u16,
            *const u16,
            *mut c_void,
            u32,
            *mut u32,
        ) -> NTSTATUS,
    >,
    pub ResolveReparsePoints: usize,
    pub GetReparsePoint: usize,
    pub SetReparsePoint: usize,
    pub DeleteReparsePoint: usize,
    pub GetStreamInfo: usize,
    pub GetDirInfoByName: usize,
    pub Control: usize,
    pub SetDelete: usize,
    pub CreateEx: usize,
    pub OverwriteEx: usize,
    pub GetEa: usize,
    pub SetEa: usize,
    pub Obsolete0: usize,
    pub DispatcherStopped: Option<unsafe extern "C" fn(Fs, u8)>,
    pub Reserved: [usize; 31],
}

type CreateFn =
    unsafe extern "C" fn(*const u16, *const VolumeParams, *const Interface, *mut Fs) -> NTSTATUS;
type FsFn = unsafe extern "C" fn(Fs);
type SetMountPointFn = unsafe extern "C" fn(Fs, *const u16) -> NTSTATUS;
type StartDispatcherFn = unsafe extern "C" fn(Fs, u32) -> NTSTATUS;
type MountPointFn = unsafe extern "C" fn(Fs) -> *const u16;
type AddDirInfoFn = unsafe extern "C" fn(*mut DirInfo, *mut c_void, u32, *mut u32) -> u8;
type OperationContextFn = unsafe extern "C" fn() -> *mut OperationContext;

/// Exported WinFsp functions.
pub struct Api {
    pub create: CreateFn,
    pub delete: FsFn,
    pub set_mount_point: SetMountPointFn,
    pub remove_mount_point: FsFn,
    pub start_dispatcher: StartDispatcherFn,
    pub stop_dispatcher: FsFn,
    pub mount_point: MountPointFn,
    pub add_dir_info: AddDirInfoFn,
    /// The request of the calling dispatcher thread (only valid inside a callback).
    pub operation_context: OperationContextFn,
}

/// The WinFsp library, loaded on first use and kept for the life of the process.
pub fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, String> {
    let path = omnizip_vfs::backend::winfsp_dll().ok_or("WinFsp is not installed")?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: a NUL-terminated path.
    let module = unsafe { LoadLibraryW(wide.as_ptr()) };
    if module.is_null() {
        return Err(format!(
            "{}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    macro_rules! sym {
        ($name:literal: $ty:ty) => {{
            // SAFETY: a NUL-terminated name, looked up in a module that stays loaded.
            let f = unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) }
                .ok_or_else(|| format!("{} has no {}", path.display(), $name))?;
            // SAFETY: the declared signature is the one in winfsp.h.
            unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(f) }
        }};
    }
    Ok(Api {
        create: sym!("FspFileSystemCreate": CreateFn),
        delete: sym!("FspFileSystemDelete": FsFn),
        set_mount_point: sym!("FspFileSystemSetMountPoint": SetMountPointFn),
        remove_mount_point: sym!("FspFileSystemRemoveMountPoint": FsFn),
        start_dispatcher: sym!("FspFileSystemStartDispatcher": StartDispatcherFn),
        stop_dispatcher: sym!("FspFileSystemStopDispatcher": FsFn),
        mount_point: sym!("FspFileSystemMountPointF": MountPointFn),
        add_dir_info: sym!("FspFileSystemAddDirInfo": AddDirInfoFn),
        operation_context: sym!("FspFileSystemGetOperationContext": OperationContextFn),
    })
}
