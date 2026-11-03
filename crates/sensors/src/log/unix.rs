//! Unix file access for the `log` sensor: inodes identify files (ADR-0024).

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::FileId;

/// What the error message names when the agent may not read a file.
pub const ACCOUNT: &str = "the agent's user";

/// Opens a resolved path for reading. `O_NONBLOCK` in case a FIFO replaced
/// the file since the check; `O_NOFOLLOW` in case a symlink did.
pub fn open(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::NOFOLLOW).bits() as i32)
        .open(path)
}

pub fn id(file: &File) -> io::Result<FileId> {
    let meta = file.metadata()?;
    Ok(FileId {
        volume: meta.dev(),
        index: meta.ino().into(),
    })
}

/// The file `path` names now.
pub fn path_id(path: &Path) -> io::Result<FileId> {
    let meta = std::fs::metadata(path)?;
    Ok(FileId {
        volume: meta.dev(),
        index: meta.ino().into(),
    })
}

/// Where the open file is; `None` as Unix cannot tell reliably, and the
/// path was resolved just before the open.
pub fn final_path(_file: &File) -> io::Result<Option<PathBuf>> {
    Ok(None)
}

pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    file.read_at(buf, offset)
}

/// Whether the file was deleted: holding it would keep its space used.
pub fn deleted(file: &File) -> io::Result<bool> {
    Ok(file.metadata()?.nlink() == 0)
}
