//! Win32 file access for the `log` sensor: a file is its volume serial
//! number and file index, and it is opened so its writer can still write,
//! rename and delete it (ADR-0085).

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Storage::FileSystem::{
    FileIdInfo, FileStandardInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FILE_NAME_NORMALIZED,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO,
    VOLUME_NAME_DOS,
};

use super::FileId;

/// What the error message names when the agent may not read a file.
pub const ACCOUNT: &str = r"NT SERVICE\MonitorFlowAgent";
/// `ERROR_SHARING_VIOLATION`: the writer opened the file without letting
/// others read it.
pub const SHARING_VIOLATION: i32 = 32;
/// Everything others may do while the sensor holds the file: write,
/// rename, delete.
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

pub fn open(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .open(path)
}

/// The volume serial number and file index. The 128-bit `FILE_ID_INFO`
/// holds ReFS IDs; FAT has none, so the 64-bit index is the fallback.
pub fn id(file: &File) -> io::Result<FileId> {
    let handle = file.as_raw_handle();
    let mut info = FILE_ID_INFO::default();
    // SAFETY: an open handle, and a buffer of the class's type and size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok != 0 {
        return Ok(FileId {
            volume: info.VolumeSerialNumber,
            index: u128::from_le_bytes(info.FileId.Identifier),
        });
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: an open handle and an out pointer to the structure.
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileId {
        volume: info.dwVolumeSerialNumber.into(),
        index: (u128::from(info.nFileIndexHigh) << 32) | u128::from(info.nFileIndexLow),
    })
}

/// The file `path` names now, opened for its attributes only, which no
/// writer's sharing mode can refuse.
pub fn path_id(path: &Path) -> io::Result<FileId> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .open(path)?;
    id(&file)
}

/// Where the open file really is, as `canonicalize` writes it
/// (`\\?\C:\…`), so a swap after the path was checked is caught.
pub fn final_path(file: &File) -> io::Result<Option<PathBuf>> {
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: an open handle, and the buffer and its length match.
        let len = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        if len < buf.len() {
            return Ok(Some(OsString::from_wide(&buf[..len]).into()));
        }
        // Too small: `len` is the size needed, with the NUL.
        buf.resize(len, 0);
    }
}

pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    file.seek_read(buf, offset)
}

/// Whether the file was deleted. Before POSIX delete semantics, the name
/// stays taken while a handle is open, so the writer could not create its
/// file again until the sensor lets go.
pub fn deleted(file: &File) -> io::Result<bool> {
    let mut info = FILE_STANDARD_INFO::default();
    // SAFETY: an open handle, and a buffer of the class's type and size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&mut info as *mut FILE_STANDARD_INFO).cast(),
            size_of::<FILE_STANDARD_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.DeletePending || info.NumberOfLinks == 0)
}
